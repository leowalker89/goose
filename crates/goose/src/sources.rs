//! Filesystem-backed CRUD for [`SourceEntry`] values exchanged over ACP custom

use crate::skills::{
    build_skill_md, discover_skills, infer_skill_name, is_global_skill_dir,
    parse_skill_frontmatter, resolve_discoverable_skill_dir, resolve_skill_dir, skill_base_dir,
    validate_skill_name,
};
use fs_err as fs;
use goose_sdk::custom_requests::{SourceEntry, SourceType};
use sacp::Error;
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing::warn;

pub fn parse_frontmatter<T: for<'de> Deserialize<'de>>(
    content: &str,
) -> Result<Option<(T, String)>, serde_yaml::Error> {
    let content = trim_frontmatter_start(content);
    let mut lines = content.lines();
    if !lines.next().is_some_and(is_frontmatter_delimiter) {
        return Ok(None);
    }

    let mut yaml_lines = Vec::new();
    for line in &mut lines {
        if is_frontmatter_delimiter(line) {
            let yaml_content = yaml_lines.join("\n");
            let metadata: T = serde_yaml::from_str(yaml_content.trim())?;
            let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
            return Ok(Some((metadata, body)));
        }
        yaml_lines.push(line);
    }

    Ok(None)
}

fn trim_frontmatter_start(content: &str) -> &str {
    content.trim_start_matches(|ch: char| ch.is_whitespace() || ch == '\u{feff}')
}

fn is_frontmatter_delimiter(line: &str) -> bool {
    line.trim_end_matches('\r') == "---"
}

pub type SourceMetadata = BTreeMap<String, serde_json::Value>;

const RESERVED_AGENT_METADATA_KEYS: [&str; 2] = ["name", "description"];

fn unsupported_source_type(source_type: SourceType, supported: &str) -> Error {
    Error::invalid_params().data(format!(
        "Source type '{}' is not supported. Only '{}' is currently supported.",
        source_type, supported
    ))
}

fn source_already_exists(name: &str) -> Error {
    Error::invalid_params().data(format!("A source named \"{}\" already exists", name))
}

fn source_not_found(path: impl std::fmt::Display) -> Error {
    Error::invalid_params().data(format!("Source \"{}\" not found", path))
}

fn write_new_file(path: &Path, contents: &str, duplicate_error: Error) -> Result<(), Error> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                duplicate_error
            } else {
                Error::internal_error().data(format!("Failed to create source file: {e}"))
            }
        })?;

    file.write_all(contents.as_bytes())
        .map_err(|e| Error::internal_error().data(format!("Failed to write source file: {e}")))
}

fn require_skill_type(source_type: SourceType) -> Result<(), Error> {
    if source_type != SourceType::Skill {
        return Err(unsupported_source_type(source_type, "skill"));
    }
    Ok(())
}

fn require_crud_source_type(source_type: SourceType) -> Result<(), Error> {
    if !matches!(source_type, SourceType::Skill | SourceType::Agent) {
        return Err(Error::invalid_params().data(format!(
            "Source type '{}' is not supported. Only 'skill' and 'agent' are currently supported.",
            source_type
        )));
    }
    Ok(())
}

fn require_listable_type(source_type: Option<SourceType>) -> Result<SourceType, Error> {
    match source_type.unwrap_or(SourceType::Skill) {
        SourceType::Skill => Ok(SourceType::Skill),
        SourceType::BuiltinSkill => Ok(SourceType::BuiltinSkill),
        SourceType::Agent => Ok(SourceType::Agent),
        other => Err(Error::invalid_params().data(format!(
            "Source type '{}' is not supported. Only 'skill', 'builtinSkill', and 'agent' are currently supported for listing.",
            other
        ))),
    }
}

fn source_entry(
    source_type: SourceType,
    name: &str,
    description: &str,
    content: &str,
    metadata: Option<SourceMetadata>,
    dir: &std::path::Path,
    global: bool,
) -> SourceEntry {
    SourceEntry {
        source_type,
        name: name.to_string(),
        description: description.to_string(),
        content: content.to_string(),
        metadata,
        directory: dir.to_string_lossy().to_string(),
        global,
        supporting_files: Vec::new(),
    }
}

fn builtin_skill_entry(mut source: SourceEntry) -> SourceEntry {
    source.source_type = SourceType::BuiltinSkill;
    source.directory = format!("builtin://skills/{}", source.name);
    source.global = true;
    source.supporting_files.clear();
    source
}

#[derive(Debug, Default)]
pub(crate) struct AgentFrontmatter {
    pub name: String,
    pub description: Option<String>,
    pub metadata: SourceMetadata,
}

pub(crate) fn parse_agent_markdown(raw: &str) -> Result<Option<AgentFrontmatterAndBody>, Error> {
    let Some((mut frontmatter, body)) = parse_frontmatter::<Mapping>(raw)
        .map_err(|e| Error::invalid_params().data(format!("Invalid agent frontmatter: {e}")))?
    else {
        return Ok(None);
    };

    let name = remove_string_key(&mut frontmatter, "name").unwrap_or_default();
    if name.trim().is_empty() {
        return Err(Error::invalid_params().data("Agent name must not be empty"));
    }

    let description = remove_string_key(&mut frontmatter, "description");
    let metadata = mapping_to_metadata(frontmatter)?;

    Ok(Some(AgentFrontmatterAndBody {
        frontmatter: AgentFrontmatter {
            name,
            description,
            metadata,
        },
        body,
    }))
}

#[derive(Debug)]
pub(crate) struct AgentFrontmatterAndBody {
    pub frontmatter: AgentFrontmatter,
    pub body: String,
}

pub(crate) fn build_agent_md(
    name: &str,
    description: &str,
    content: &str,
    metadata: Option<&SourceMetadata>,
) -> Result<String, Error> {
    if name.trim().is_empty() {
        return Err(Error::invalid_params().data("Agent name must not be empty"));
    }

    let mut frontmatter = Mapping::new();
    frontmatter.insert(
        Value::String("name".to_string()),
        Value::String(name.to_string()),
    );
    frontmatter.insert(
        Value::String("description".to_string()),
        Value::String(description.to_string()),
    );

    if let Some(metadata) = metadata {
        for (key, value) in metadata {
            if RESERVED_AGENT_METADATA_KEYS.contains(&key.as_str()) {
                continue;
            }
            frontmatter.insert(
                Value::String(key.clone()),
                serde_yaml::to_value(value).map_err(|e| {
                    Error::invalid_params().data(format!("Invalid agent metadata: {e}"))
                })?,
            );
        }
    }

    let yaml = serde_yaml::to_string(&frontmatter)
        .map_err(|e| Error::internal_error().data(format!("Failed to serialize agent: {e}")))?;
    let mut md = format!("---\n{}---\n", yaml);
    if !content.is_empty() {
        md.push('\n');
        md.push_str(content);
        md.push('\n');
    }
    Ok(md)
}

fn remove_string_key(frontmatter: &mut Mapping, key: &str) -> Option<String> {
    frontmatter
        .remove(Value::String(key.to_string()))
        .and_then(|value| value.as_str().map(ToString::to_string))
}

fn mapping_to_metadata(frontmatter: Mapping) -> Result<SourceMetadata, Error> {
    let mut metadata = SourceMetadata::new();
    for (key, value) in frontmatter {
        let Some(key) = key.as_str() else {
            return Err(Error::invalid_params().data("Agent metadata keys must be strings"));
        };
        let value = serde_json::to_value(value).map_err(|e| {
            Error::invalid_params().data(format!("Invalid agent metadata value: {e}"))
        })?;
        metadata.insert(key.to_string(), value);
    }
    Ok(metadata)
}

fn sanitize_agent_metadata(metadata: &SourceMetadata) -> SourceMetadata {
    metadata
        .iter()
        .filter(|(key, _)| !RESERVED_AGENT_METADATA_KEYS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn global_agents_dir() -> Result<PathBuf, Error> {
    dirs::home_dir()
        .map(|h| h.join(".agents").join("agents"))
        .ok_or_else(|| Error::internal_error().data("Could not determine home directory"))
}

fn project_agents_dir(project_dir: &str) -> Result<PathBuf, Error> {
    if project_dir.trim().is_empty() {
        return Err(
            Error::invalid_params().data("projectDir must not be empty when global is false")
        );
    }

    let project = Path::new(project_dir);
    if !project.is_absolute() {
        return Err(Error::invalid_params().data("projectDir must be an absolute path"));
    }

    Ok(project.join(".agents").join("agents"))
}

fn agent_base_dir(global: bool, project_dir: Option<&str>) -> Result<PathBuf, Error> {
    if global {
        global_agents_dir()
    } else {
        let pd = project_dir.ok_or_else(|| {
            Error::invalid_params().data("projectDir is required when global is false")
        })?;
        project_agents_dir(pd)
    }
}

fn slugify_agent_name(name: &str) -> Result<String, Error> {
    if name.trim().is_empty() {
        return Err(Error::invalid_params().data("Agent name must not be empty"));
    }

    let mut slug = String::new();
    let mut previous_was_separator = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            previous_was_separator = false;
        } else if !previous_was_separator {
            slug.push('-');
            previous_was_separator = true;
        }
    }

    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        return Err(Error::invalid_params().data("Agent name must contain ASCII letters or digits"));
    }
    if slug.len() > 64 {
        return Err(Error::invalid_params().data(format!(
            "Invalid agent name \"{}\". Generated filenames must be at most 64 characters.",
            name
        )));
    }

    Ok(slug)
}

fn canonicalize_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn inferred_agent_root(path: &Path) -> Option<PathBuf> {
    let canonical_path = canonicalize_or_original(path);

    if let Ok(global_root) = global_agents_dir() {
        let canonical_root = canonicalize_or_original(&global_root);
        if canonical_path.starts_with(&canonical_root) {
            return Some(canonical_root);
        }
    }

    canonical_path.ancestors().find_map(|ancestor| {
        let parent = ancestor.parent()?;
        let is_project_agents_root = ancestor.file_name().and_then(|name| name.to_str())
            == Some("agents")
            && parent.file_name().and_then(|name| name.to_str()) == Some(".agents");
        is_project_agents_root.then(|| ancestor.to_path_buf())
    })
}

fn resolve_agent_file(path: &str) -> Result<PathBuf, Error> {
    if path.is_empty() {
        return Err(Error::invalid_params().data("Source path must not be empty"));
    }

    let canonical_file = Path::new(path)
        .canonicalize()
        .map_err(|_| source_not_found(path))?;

    let Some(root) = inferred_agent_root(&canonical_file) else {
        return Err(source_not_found(path));
    };
    if canonical_file.parent() != Some(root.as_path())
        || !canonical_file.is_file()
        || canonical_file.extension().and_then(|ext| ext.to_str()) != Some("md")
    {
        return Err(source_not_found(path));
    }

    Ok(canonical_file)
}

fn is_global_agent_file(path: &Path) -> bool {
    global_agents_dir().ok().as_deref().is_some_and(|root| {
        canonicalize_or_original(path).starts_with(canonicalize_or_original(root))
    })
}

fn read_agent_file(path: &Path, global: bool) -> Result<SourceEntry, Error> {
    let raw = fs::read_to_string(path)
        .map_err(|e| Error::internal_error().data(format!("Failed to read agent: {e}")))?;
    let Some(parsed) = parse_agent_markdown(&raw)? else {
        return Err(source_not_found(path.display()));
    };
    let description = parsed
        .frontmatter
        .description
        .unwrap_or_else(|| "Agent".to_string());
    Ok(source_entry(
        SourceType::Agent,
        &parsed.frontmatter.name,
        &description,
        &parsed.body,
        Some(parsed.frontmatter.metadata),
        path,
        global,
    ))
}

fn create_agent_source(
    name: &str,
    description: &str,
    content: &str,
    metadata: Option<&SourceMetadata>,
    global: bool,
    project_dir: Option<&str>,
) -> Result<SourceEntry, Error> {
    let slug = slugify_agent_name(name)?;
    let base = agent_base_dir(global, project_dir)?;
    let path = base.join(format!("{slug}.md"));
    let sanitized_metadata = metadata.map(sanitize_agent_metadata);

    fs::create_dir_all(&base).map_err(|e| {
        Error::internal_error().data(format!("Failed to create source directory: {e}"))
    })?;
    let md = build_agent_md(name, description, content, sanitized_metadata.as_ref())?;
    write_new_file(&path, &md, source_already_exists(name))?;

    Ok(source_entry(
        SourceType::Agent,
        name,
        description,
        content,
        sanitized_metadata,
        &path,
        global,
    ))
}

fn update_agent_source(
    path: &str,
    name: &str,
    description: &str,
    content: &str,
    metadata: Option<&SourceMetadata>,
) -> Result<SourceEntry, Error> {
    let current_path = resolve_agent_file(path)?;
    let existing = read_agent_file(&current_path, is_global_agent_file(&current_path))?;
    let effective_metadata = metadata.map(sanitize_agent_metadata).or(existing.metadata);
    let slug = slugify_agent_name(name)?;
    let target_path = current_path
        .parent()
        .ok_or_else(|| Error::internal_error().data("Failed to resolve source base directory"))?
        .join(format!("{slug}.md"));

    let md = build_agent_md(name, description, content, effective_metadata.as_ref())?;
    if target_path == current_path {
        fs::write(&target_path, md)
            .map_err(|e| Error::internal_error().data(format!("Failed to write agent: {e}")))?;
    } else {
        write_new_file(&target_path, &md, source_already_exists(name))?;
        if let Err(e) = fs::remove_file(&current_path) {
            let _ = fs::remove_file(&target_path);
            return Err(
                Error::internal_error().data(format!("Failed to remove old source file: {e}"))
            );
        }
    }

    Ok(source_entry(
        SourceType::Agent,
        name,
        description,
        content,
        effective_metadata,
        &target_path,
        is_global_agent_file(&target_path),
    ))
}

fn delete_agent_source(path: &str) -> Result<(), Error> {
    let file = resolve_agent_file(path)?;
    fs::remove_file(&file)
        .map_err(|e| Error::internal_error().data(format!("Failed to delete source: {e}")))?;
    Ok(())
}

fn scan_agents_from_dir(
    dir: &Path,
    global: bool,
    seen: &mut std::collections::HashSet<String>,
) -> Vec<SourceEntry> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut sources = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }

        match read_agent_file(&path, global) {
            Ok(source) => {
                if seen.insert(source.name.clone()) {
                    sources.push(source);
                }
            }
            Err(e) => warn!("Failed to parse agent file {}: {}", path.display(), e),
        }
    }

    sources
}

fn discover_agents(project_dir: Option<&str>) -> Result<Vec<SourceEntry>, Error> {
    let mut sources = Vec::new();
    let mut seen = std::collections::HashSet::new();

    if let Some(project_dir) = project_dir.map(str::trim).filter(|p| !p.is_empty()) {
        let dir = project_agents_dir(project_dir)?;
        sources.extend(scan_agents_from_dir(&dir, false, &mut seen));
    }

    let global_dir = global_agents_dir()?;
    sources.extend(scan_agents_from_dir(&global_dir, true, &mut seen));

    Ok(sources)
}

pub fn create_source(
    source_type: SourceType,
    name: &str,
    description: &str,
    content: &str,
    metadata: Option<&SourceMetadata>,
    global: bool,
    project_dir: Option<&str>,
) -> Result<SourceEntry, Error> {
    require_crud_source_type(source_type)?;
    if source_type == SourceType::Agent {
        return create_agent_source(name, description, content, metadata, global, project_dir);
    }

    validate_skill_name(name)?;
    let dir = skill_base_dir(global, project_dir)?.join(name);

    if dir.exists() {
        return Err(
            Error::invalid_params().data(format!("A source named \"{}\" already exists", name))
        );
    }

    fs::create_dir_all(&dir).map_err(|e| {
        Error::internal_error().data(format!("Failed to create source directory: {e}"))
    })?;
    let file_path = dir.join("SKILL.md");
    let md = build_skill_md(name, description, content);
    fs::write(&file_path, md)
        .map_err(|e| Error::internal_error().data(format!("Failed to write SKILL.md: {e}")))?;

    Ok(source_entry(
        source_type,
        name,
        description,
        content,
        None,
        &dir,
        global,
    ))
}

pub fn update_source(
    source_type: SourceType,
    path: &str,
    name: &str,
    description: &str,
    content: &str,
    metadata: Option<&SourceMetadata>,
) -> Result<SourceEntry, Error> {
    require_crud_source_type(source_type)?;
    if source_type == SourceType::Agent {
        return update_agent_source(path, name, description, content, metadata);
    }

    validate_skill_name(name)?;

    let dir = resolve_discoverable_skill_dir(path)?;
    let current_dir_name = dir
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| Error::internal_error().data("Failed to resolve source directory name"))?;

    let target_dir = if name == current_dir_name {
        dir.clone()
    } else {
        let base_dir = dir.parent().ok_or_else(|| {
            Error::internal_error().data("Failed to resolve source base directory")
        })?;
        let target_dir = base_dir.join(name);

        if target_dir.exists() {
            return Err(
                Error::invalid_params().data(format!("A source named \"{}\" already exists", name))
            );
        }

        fs::rename(&dir, &target_dir).map_err(|e| {
            Error::internal_error().data(format!("Failed to rename source directory: {e}"))
        })?;

        target_dir
    };

    let file_path = target_dir.join("SKILL.md");
    let md = build_skill_md(name, description, content);
    fs::write(&file_path, md)
        .map_err(|e| Error::internal_error().data(format!("Failed to write SKILL.md: {e}")))?;

    Ok(source_entry(
        source_type,
        name,
        description,
        content,
        None,
        &target_dir,
        is_global_skill_dir(&target_dir),
    ))
}

pub fn delete_source(source_type: SourceType, path: &str) -> Result<(), Error> {
    require_crud_source_type(source_type)?;
    if source_type == SourceType::Agent {
        return delete_agent_source(path);
    }

    let dir = resolve_skill_dir(path)?;
    fs::remove_dir_all(&dir)
        .map_err(|e| Error::internal_error().data(format!("Failed to delete source: {e}")))?;
    Ok(())
}

pub fn list_sources(
    source_type: Option<SourceType>,
    project_dir: Option<&str>,
) -> Result<Vec<SourceEntry>, Error> {
    let listed_type = require_listable_type(source_type)?;

    if listed_type == SourceType::Agent {
        let mut sources = discover_agents(project_dir)?;
        sources.sort_by(|a, b| a.name.cmp(&b.name));
        return Ok(sources);
    }

    let working_dir = project_dir
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);

    let mut sources: Vec<SourceEntry> = discover_skills(working_dir.as_deref())
        .into_iter()
        .filter(|s| s.source_type == listed_type)
        .map(|s| {
            if listed_type == SourceType::BuiltinSkill {
                builtin_skill_entry(s)
            } else {
                s
            }
        })
        .collect();

    sources.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(sources)
}

pub fn export_source(source_type: SourceType, path: &str) -> Result<(String, String), Error> {
    require_skill_type(source_type)?;
    let dir = resolve_discoverable_skill_dir(path)?;

    let md = dir.join("SKILL.md");
    let raw = fs::read_to_string(&md)
        .map_err(|e| Error::internal_error().data(format!("Failed to read SKILL.md: {e}")))?;
    let (description, content) = parse_skill_frontmatter(&raw);

    let name = infer_skill_name(&dir);

    let export = serde_json::json!({
        "version": 1,
        "type": "skill",
        "name": name,
        "description": description,
        "content": content,
    });
    let json = serde_json::to_string_pretty(&export)
        .map_err(|e| Error::internal_error().data(format!("Failed to serialize source: {e}")))?;
    let filename = format!("{}.skill.json", name);
    Ok((json, filename))
}

pub fn import_sources(
    data: &str,
    global: bool,
    project_dir: Option<&str>,
) -> Result<Vec<SourceEntry>, Error> {
    let value: serde_json::Value = serde_json::from_str(data)
        .map_err(|e| Error::invalid_params().data(format!("Invalid JSON: {e}")))?;

    let version = value
        .get("version")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| Error::invalid_params().data("Missing or invalid \"version\" field"))?;
    if version != 1 {
        return Err(
            Error::invalid_params().data(format!("Unsupported source export version: {}", version))
        );
    }

    match value
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("skill")
    {
        "skill" => {}
        other => {
            return Err(Error::invalid_params().data(format!(
                "Source type '{}' is not supported. Only 'skill' is currently supported.",
                other
            )));
        }
    };

    let name = value
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::invalid_params().data("Missing or invalid \"name\" field"))?
        .to_string();
    if name.is_empty() {
        return Err(Error::invalid_params().data("Source name must not be empty"));
    }

    let description = value
        .get("description")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::invalid_params().data("Missing or invalid \"description\" field"))?
        .to_string();
    if description.is_empty() {
        return Err(Error::invalid_params().data("Source description must not be empty"));
    }

    let content = value
        .get("content")
        .or_else(|| value.get("instructions"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    validate_skill_name(&name)?;

    let base = skill_base_dir(global, project_dir)?;
    let mut final_name = name.clone();
    if base.join(&final_name).exists() {
        final_name = format!("{}-imported", name);
        let mut counter = 2u32;
        while base.join(&final_name).exists() {
            final_name = format!("{}-imported-{}", name, counter);
            counter += 1;
        }
    }

    let dir = base.join(&final_name);
    fs::create_dir_all(&dir).map_err(|e| {
        Error::internal_error().data(format!("Failed to create source directory: {e}"))
    })?;
    let file_path = dir.join("SKILL.md");
    let md = build_skill_md(&final_name, &description, &content);
    fs::write(&file_path, md)
        .map_err(|e| Error::internal_error().data(format!("Failed to write SKILL.md: {e}")))?;

    Ok(vec![source_entry(
        SourceType::Skill,
        &final_name,
        &description,
        &content,
        None,
        &dir,
        global,
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn skill_name_validation() {
        assert!(validate_skill_name("my-skill").is_ok());
        assert!(validate_skill_name("abc123").is_ok());
        assert!(validate_skill_name("double--hyphen").is_ok());
        assert!(validate_skill_name("").is_err());
        assert!(validate_skill_name("-leading").is_err());
        assert!(validate_skill_name("trailing-").is_err());
        assert!(validate_skill_name("CAPS").is_err());
        assert!(validate_skill_name("../escape").is_err());
        assert!(validate_skill_name(&"a".repeat(64)).is_ok());
        assert!(validate_skill_name(&"a".repeat(65)).is_err());
    }

    #[test]
    fn parse_frontmatter_uses_delimiter_lines() {
        let raw = "---\nname: Delimiter Agent\ndescription: |\n  before\n  ---\n  after\n---\nbody\n---\nmore body\n";
        let parsed = parse_agent_markdown(raw).unwrap().unwrap();

        assert_eq!(parsed.frontmatter.name, "Delimiter Agent");
        assert_eq!(
            parsed.frontmatter.description.as_deref(),
            Some("before\n---\nafter")
        );
        assert_eq!(parsed.body, "body\n---\nmore body");
    }

    #[test]
    fn parse_frontmatter_accepts_crlf_delimiters() {
        let raw = "---\r\nname: Windows Agent\r\ndescription: crlf\r\n---\r\nbody\r\n";
        let parsed = parse_agent_markdown(raw).unwrap().unwrap();

        assert_eq!(parsed.frontmatter.name, "Windows Agent");
        assert_eq!(parsed.frontmatter.description.as_deref(), Some("crlf"));
        assert_eq!(parsed.body, "body");
    }

    #[test]
    fn parse_frontmatter_accepts_bom_prefixed_delimiter() {
        let raw = "\u{feff}---\nname: Bom Agent\ndescription: bom\n---\nbody\n";
        let parsed = parse_agent_markdown(raw).unwrap().unwrap();

        assert_eq!(parsed.frontmatter.name, "Bom Agent");
        assert_eq!(parsed.frontmatter.description.as_deref(), Some("bom"));
        assert_eq!(parsed.body, "body");
    }

    #[test]
    fn create_list_update_delete_project_skill() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().to_str().unwrap();

        let created = create_source(
            SourceType::Skill,
            "my-skill",
            "does the thing",
            "step one\nstep two",
            None,
            false,
            Some(project),
        )
        .unwrap();
        assert_eq!(created.name, "my-skill");
        assert!(!created.global);
        let dir = PathBuf::from(&created.directory);
        assert!(dir.join("SKILL.md").exists());

        let listed = list_sources(Some(SourceType::Skill), Some(project)).unwrap();
        assert!(listed.iter().any(|s| s.name == "my-skill" && !s.global));

        let updated = update_source(
            SourceType::Skill,
            created.directory.as_str(),
            "my-skill",
            "now does a different thing",
            "step three",
            None,
        )
        .unwrap();
        assert_eq!(updated.description, "now does a different thing");
        assert_eq!(updated.name, "my-skill");

        delete_source(SourceType::Skill, created.directory.as_str()).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn create_list_update_delete_agent_sources() {
        let home = TempDir::new().unwrap();
        let _env = env_lock::lock_env([("HOME", Some(home.path().to_str().unwrap()))]);
        let project = TempDir::new().unwrap();
        let project_dir = project.path().to_str().unwrap();

        let mut metadata = SourceMetadata::new();
        metadata.insert("model".to_string(), serde_json::json!("gpt-4o"));
        metadata.insert("temperature".to_string(), serde_json::json!(0.2));

        let global = create_source(
            SourceType::Agent,
            "Global Agent",
            "global description",
            "global instructions",
            Some(&metadata),
            true,
            None,
        )
        .unwrap();
        assert_eq!(global.name, "Global Agent");
        assert!(global.global);
        assert_eq!(
            PathBuf::from(&global.directory),
            home.path()
                .join(".agents")
                .join("agents")
                .join("global-agent.md")
        );
        assert!(global.metadata.as_ref().unwrap().contains_key("model"));

        let project_agent = create_source(
            SourceType::Agent,
            "Project Agent",
            "project description",
            "project instructions",
            None,
            false,
            Some(project_dir),
        )
        .unwrap();
        assert!(!project_agent.global);
        assert_eq!(
            PathBuf::from(&project_agent.directory),
            project
                .path()
                .join(".agents")
                .join("agents")
                .join("project-agent.md")
        );

        let global_only = list_sources(Some(SourceType::Agent), None).unwrap();
        assert_eq!(global_only.len(), 1);
        assert_eq!(global_only[0].name, "Global Agent");

        let listed = list_sources(Some(SourceType::Agent), Some(project_dir)).unwrap();
        assert!(listed
            .iter()
            .any(|source| source.name == "Project Agent" && !source.global));
        assert!(listed
            .iter()
            .any(|source| source.name == "Global Agent" && source.global));

        let updated = update_source(
            SourceType::Agent,
            project_agent.directory.as_str(),
            "Renamed Project Agent",
            "new description",
            "new instructions",
            None,
        )
        .unwrap();
        assert_eq!(updated.name, "Renamed Project Agent");
        assert!(updated.directory.ends_with("renamed-project-agent.md"));

        delete_source(SourceType::Agent, updated.directory.as_str()).unwrap();
        let listed = list_sources(Some(SourceType::Agent), Some(project_dir)).unwrap();
        assert!(!listed
            .iter()
            .any(|source| source.name == "Renamed Project Agent"));
    }

    #[test]
    fn agent_metadata_round_trips_and_preserves_on_update() {
        let home = TempDir::new().unwrap();
        let _env = env_lock::lock_env([("HOME", Some(home.path().to_str().unwrap()))]);
        let project = TempDir::new().unwrap();
        let project_dir = project.path().to_str().unwrap();

        let mut metadata = SourceMetadata::new();
        metadata.insert("model".to_string(), serde_json::json!("gpt-4o"));
        metadata.insert("tools".to_string(), serde_json::json!(["shell", "edit"]));
        metadata.insert("name".to_string(), serde_json::json!("ignored"));
        metadata.insert("description".to_string(), serde_json::json!("ignored"));

        let created = create_source(
            SourceType::Agent,
            "Metadata Agent",
            "description",
            "body",
            Some(&metadata),
            false,
            Some(project_dir),
        )
        .unwrap();
        assert!(!created.metadata.as_ref().unwrap().contains_key("name"));
        assert!(!created
            .metadata
            .as_ref()
            .unwrap()
            .contains_key("description"));
        let raw = std::fs::read_to_string(&created.directory).unwrap();
        assert!(raw.contains("model: gpt-4o"));
        assert!(raw.contains("tools:"));
        assert!(raw.contains("name: Metadata Agent"));
        assert!(raw.contains("description: description"));
        assert!(!raw.contains("ignored"));

        let updated = update_source(
            SourceType::Agent,
            created.directory.as_str(),
            "Metadata Agent",
            "updated description",
            "updated body",
            None,
        )
        .unwrap();
        let preserved = updated.metadata.as_ref().unwrap();
        assert_eq!(preserved.get("model"), Some(&serde_json::json!("gpt-4o")));
        assert_eq!(
            preserved.get("tools"),
            Some(&serde_json::json!(["shell", "edit"]))
        );

        let mut replacement = SourceMetadata::new();
        replacement.insert("model".to_string(), serde_json::json!("claude"));
        replacement.insert("name".to_string(), serde_json::json!("ignored again"));
        let updated = update_source(
            SourceType::Agent,
            updated.directory.as_str(),
            "Metadata Agent",
            "updated again",
            "body again",
            Some(&replacement),
        )
        .unwrap();
        assert_eq!(
            updated.metadata.as_ref().unwrap().get("model"),
            Some(&serde_json::json!("claude"))
        );
        assert!(!updated.metadata.as_ref().unwrap().contains_key("name"));
        assert!(!updated.metadata.as_ref().unwrap().contains_key("tools"));
    }

    #[test]
    fn agent_metadata_writes_deterministically() {
        let home = TempDir::new().unwrap();
        let _env = env_lock::lock_env([("HOME", Some(home.path().to_str().unwrap()))]);
        let project = TempDir::new().unwrap();
        let project_dir = project.path().to_str().unwrap();

        let mut metadata = SourceMetadata::new();
        metadata.insert("zeta".to_string(), serde_json::json!("last"));
        metadata.insert("alpha".to_string(), serde_json::json!("first"));
        metadata.insert("model".to_string(), serde_json::json!("gpt-4o"));

        let created = create_source(
            SourceType::Agent,
            "Stable Agent",
            "description",
            "body",
            Some(&metadata),
            false,
            Some(project_dir),
        )
        .unwrap();
        let first = std::fs::read_to_string(&created.directory).unwrap();
        assert!(
            first.find("alpha: first").unwrap() < first.find("model: gpt-4o").unwrap()
                && first.find("model: gpt-4o").unwrap() < first.find("zeta: last").unwrap()
        );

        let updated = update_source(
            SourceType::Agent,
            created.directory.as_str(),
            "Stable Agent",
            "description",
            "body",
            None,
        )
        .unwrap();
        let second = std::fs::read_to_string(&updated.directory).unwrap();

        let updated = update_source(
            SourceType::Agent,
            updated.directory.as_str(),
            "Stable Agent",
            "description",
            "body",
            None,
        )
        .unwrap();
        let third = std::fs::read_to_string(&updated.directory).unwrap();

        assert_eq!(second, third);
    }

    #[test]
    fn project_agent_precedes_global_agent_with_same_name() {
        let home = TempDir::new().unwrap();
        let _env = env_lock::lock_env([("HOME", Some(home.path().to_str().unwrap()))]);
        let project = TempDir::new().unwrap();
        let project_dir = project.path().to_str().unwrap();

        create_source(
            SourceType::Agent,
            "Shared Agent",
            "global",
            "global body",
            None,
            true,
            None,
        )
        .unwrap();
        let project_agent = create_source(
            SourceType::Agent,
            "Shared Agent",
            "project",
            "project body",
            None,
            false,
            Some(project_dir),
        )
        .unwrap();

        let listed = list_sources(Some(SourceType::Agent), Some(project_dir)).unwrap();
        let matching: Vec<_> = listed
            .iter()
            .filter(|source| source.name == "Shared Agent")
            .collect();
        assert_eq!(matching.len(), 1);
        assert!(!matching[0].global);
        assert_eq!(matching[0].description, "project");
        assert_eq!(matching[0].directory, project_agent.directory);
    }

    #[test]
    fn agent_metadata_rejects_non_string_yaml_keys() {
        let raw = "---\nname: Bad Agent\ndescription: bad\n42: nope\n---\nbody\n";
        let err = parse_agent_markdown(raw).unwrap_err();
        assert!(format!("{:?}", err).contains("keys must be strings"));
    }

    #[test]
    fn create_rejects_duplicate_name() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().to_str().unwrap();

        create_source(
            SourceType::Skill,
            "dup",
            "d",
            "c",
            None,
            false,
            Some(project),
        )
        .unwrap();
        let err = create_source(
            SourceType::Skill,
            "dup",
            "d",
            "c",
            None,
            false,
            Some(project),
        )
        .unwrap_err();
        assert!(format!("{:?}", err).contains("already exists"));
    }

    #[test]
    fn project_scope_requires_project_dir() {
        let err = create_source(SourceType::Skill, "x", "d", "c", None, false, None).unwrap_err();
        assert!(format!("{:?}", err).contains("projectDir"));
    }

    #[test]
    fn agent_source_rejects_empty_names_and_unsafe_paths() {
        let home = TempDir::new().unwrap();
        let _env = env_lock::lock_env([("HOME", Some(home.path().to_str().unwrap()))]);
        let project = TempDir::new().unwrap();
        let project_dir = project.path().to_str().unwrap();

        let err = create_source(
            SourceType::Agent,
            "",
            "d",
            "c",
            None,
            false,
            Some(project_dir),
        )
        .unwrap_err();
        assert!(format!("{:?}", err).contains("must not be empty"));

        let err = create_source(
            SourceType::Agent,
            "../",
            "d",
            "c",
            None,
            false,
            Some(project_dir),
        )
        .unwrap_err();
        assert!(format!("{:?}", err).contains("ASCII letters or digits"));

        let escaped = project.path().join(".goose").join("agents");
        std::fs::create_dir_all(&escaped).unwrap();
        let legacy_agent = escaped.join("legacy.md");
        std::fs::write(
            &legacy_agent,
            "---\nname: Legacy Agent\ndescription: legacy\n---\nlegacy body\n",
        )
        .unwrap();
        let err = update_source(
            SourceType::Agent,
            legacy_agent.to_str().unwrap(),
            "Legacy Agent",
            "d",
            "c",
            None,
        )
        .unwrap_err();
        assert!(format!("{:?}", err).contains("not found"));

        let escaped_agent = project.path().join(".agents").join("escape.md");
        std::fs::create_dir_all(escaped_agent.parent().unwrap()).unwrap();
        std::fs::write(
            &escaped_agent,
            "---\nname: Escaped Agent\ndescription: escaped\n---\nescaped body\n",
        )
        .unwrap();
        let attempted_escape = project
            .path()
            .join(".agents")
            .join("agents")
            .join("..")
            .join("escape.md");
        let err = update_source(
            SourceType::Agent,
            attempted_escape.to_str().unwrap(),
            "Escaped Agent",
            "d",
            "c",
            None,
        )
        .unwrap_err();
        assert!(format!("{:?}", err).contains("not found"));

        let nested = project
            .path()
            .join(".agents")
            .join("agents")
            .join("sub")
            .join("nested.md");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(
            &nested,
            "---\nname: Nested Agent\ndescription: nested\n---\nnested body\n",
        )
        .unwrap();
        let err = update_source(
            SourceType::Agent,
            nested.to_str().unwrap(),
            "Nested Agent",
            "d",
            "c",
            None,
        )
        .unwrap_err();
        assert!(format!("{:?}", err).contains("not found"));
        let listed = list_sources(Some(SourceType::Agent), Some(project_dir)).unwrap();
        assert!(!listed.iter().any(|source| source.name == "Nested Agent"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let outside_agent = project.path().join("outside-agent.md");
            std::fs::write(
                &outside_agent,
                "---\nname: Symlink Agent\ndescription: linked\n---\nlinked body\n",
            )
            .unwrap();
            let symlinked_agent = project
                .path()
                .join(".agents")
                .join("agents")
                .join("symlink-agent.md");
            std::fs::create_dir_all(symlinked_agent.parent().unwrap()).unwrap();
            symlink(&outside_agent, &symlinked_agent).unwrap();

            let listed = list_sources(Some(SourceType::Agent), Some(project_dir)).unwrap();
            assert!(!listed.iter().any(|source| source.name == "Symlink Agent"));
        }

        let missing = project
            .path()
            .join(".agents")
            .join("agents")
            .join("missing.md");
        let err = delete_source(SourceType::Agent, missing.to_str().unwrap()).unwrap_err();
        assert!(format!("{:?}", err).contains("not found"));
    }

    #[test]
    fn update_agent_rejects_rename_collision_without_clobbering() {
        let home = TempDir::new().unwrap();
        let _env = env_lock::lock_env([("HOME", Some(home.path().to_str().unwrap()))]);
        let project = TempDir::new().unwrap();
        let project_dir = project.path().to_str().unwrap();

        let source = create_source(
            SourceType::Agent,
            "Source Agent",
            "source",
            "source body",
            None,
            false,
            Some(project_dir),
        )
        .unwrap();
        let target = create_source(
            SourceType::Agent,
            "Target Agent",
            "target",
            "target body",
            None,
            false,
            Some(project_dir),
        )
        .unwrap();
        let target_before = std::fs::read_to_string(&target.directory).unwrap();

        let err = update_source(
            SourceType::Agent,
            source.directory.as_str(),
            "Target Agent",
            "attempted overwrite",
            "attempted body",
            None,
        )
        .unwrap_err();
        assert!(format!("{:?}", err).contains("already exists"));
        assert!(PathBuf::from(&source.directory).exists());
        assert_eq!(
            std::fs::read_to_string(&target.directory).unwrap(),
            target_before
        );
    }

    #[test]
    fn list_agents_ignores_legacy_persona_paths() {
        let home = TempDir::new().unwrap();
        let _env = env_lock::lock_env([("HOME", Some(home.path().to_str().unwrap()))]);
        let project = TempDir::new().unwrap();
        let project_dir = project.path().to_str().unwrap();

        let legacy_project = project.path().join(".goose").join("agents");
        std::fs::create_dir_all(&legacy_project).unwrap();
        std::fs::write(
            legacy_project.join("legacy.md"),
            "---\nname: Legacy Project\ndescription: old\n---\nold\n",
        )
        .unwrap();
        std::fs::write(
            project.path().join(".persona.json"),
            r#"{"name":"Legacy Persona"}"#,
        )
        .unwrap();

        let legacy_global = home.path().join(".goose").join("agents");
        std::fs::create_dir_all(&legacy_global).unwrap();
        std::fs::write(
            legacy_global.join("global.md"),
            "---\nname: Legacy Global\ndescription: old\n---\nold\n",
        )
        .unwrap();
        std::fs::create_dir_all(home.path().join(".goose")).unwrap();
        std::fs::write(
            home.path().join(".goose").join("personas.json"),
            r#"[{"name":"Legacy Persona"}]"#,
        )
        .unwrap();

        let listed = list_sources(Some(SourceType::Agent), Some(project_dir)).unwrap();
        assert!(listed.is_empty());
    }

    #[test]
    fn export_then_import_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let project_a = tmp.path().join("a");
        let project_b = tmp.path().join("b");
        std::fs::create_dir_all(&project_a).unwrap();
        std::fs::create_dir_all(&project_b).unwrap();

        create_source(
            SourceType::Skill,
            "portable",
            "describes itself",
            "body goes here",
            None,
            false,
            Some(project_a.to_str().unwrap()),
        )
        .unwrap();

        let portable_dir = project_a.join(".agents").join("skills").join("portable");
        let (json, filename) =
            export_source(SourceType::Skill, portable_dir.to_str().unwrap()).unwrap();
        assert_eq!(filename, "portable.skill.json");

        let imported = import_sources(&json, false, Some(project_b.to_str().unwrap())).unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].name, "portable");
        assert_eq!(imported[0].description, "describes itself");
        assert_eq!(imported[0].content, "body goes here");
    }

    #[test]
    fn export_allows_discovered_read_only_skill() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        let claude_skill_dir = project.join(".claude").join("skills").join("portable");
        std::fs::create_dir_all(&claude_skill_dir).unwrap();
        std::fs::write(
            claude_skill_dir.join("SKILL.md"),
            build_skill_md("portable", "describes itself", "body goes here"),
        )
        .unwrap();

        let listed =
            list_sources(Some(SourceType::Skill), Some(project.to_str().unwrap())).unwrap();
        let exported_skill = listed
            .iter()
            .find(|skill| skill.name == "portable")
            .expect("expected listed skill");

        let (json, filename) =
            export_source(SourceType::Skill, exported_skill.directory.as_str()).unwrap();
        assert_eq!(filename, "portable.skill.json");
        assert!(json.contains("\"name\": \"portable\""));
    }

    #[test]
    fn update_allows_discovered_read_only_skill() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        let claude_skill_dir = project.join(".claude").join("skills").join("portable");
        std::fs::create_dir_all(&claude_skill_dir).unwrap();
        std::fs::write(
            claude_skill_dir.join("SKILL.md"),
            build_skill_md("portable", "describes itself", "body goes here"),
        )
        .unwrap();

        let updated = update_source(
            SourceType::Skill,
            claude_skill_dir.to_str().unwrap(),
            "portable",
            "updated description",
            "updated body",
            None,
        )
        .unwrap();

        assert_eq!(updated.name, "portable");
        assert_eq!(updated.description, "updated description");
        assert_eq!(updated.content, "updated body");

        let raw = std::fs::read_to_string(claude_skill_dir.join("SKILL.md")).unwrap();
        assert!(raw.contains("description: 'updated description'"));
        assert!(raw.contains("updated body"));
    }

    #[test]
    fn import_collision_appends_suffix() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().to_str().unwrap();

        create_source(
            SourceType::Skill,
            "busy",
            "d",
            "c",
            None,
            false,
            Some(project),
        )
        .unwrap();

        let payload = serde_json::json!({
            "version": 1,
            "type": "skill",
            "name": "busy",
            "description": "d",
            "content": "c",
        })
        .to_string();
        let imported = import_sources(&payload, false, Some(project)).unwrap();
        assert_eq!(imported[0].name, "busy-imported");
    }

    #[test]
    fn update_rejects_nonexistent_source() {
        let tmp = TempDir::new().unwrap();
        let missing_dir = tmp
            .path()
            .join(".goose")
            .join("skills")
            .join("no-such-skill");
        let err = update_source(
            SourceType::Skill,
            missing_dir.to_str().unwrap(),
            "no-such-skill",
            "d",
            "c",
            None,
        )
        .unwrap_err();
        assert!(format!("{:?}", err).contains("not found"));
    }

    #[test]
    fn delete_rejects_nonexistent_source() {
        let tmp = TempDir::new().unwrap();
        let missing_dir = tmp
            .path()
            .join(".goose")
            .join("skills")
            .join("no-such-skill");
        let err = delete_source(SourceType::Skill, missing_dir.to_str().unwrap()).unwrap_err();
        assert!(format!("{:?}", err).contains("not found"));
    }

    #[test]
    fn list_sources_lists_builtin_skills() {
        let listed = list_sources(Some(SourceType::BuiltinSkill), None).unwrap();
        let builtin = listed
            .iter()
            .find(|source| source.name == "goose-doc-guide")
            .expect("expected goose-doc-guide builtin skill");

        assert_eq!(builtin.source_type, SourceType::BuiltinSkill);
        assert!(builtin.global);
        assert_eq!(builtin.directory, "builtin://skills/goose-doc-guide");
        assert!(builtin.supporting_files.is_empty());
        assert!(!builtin.content.is_empty());
    }

    #[test]
    fn list_skill_excludes_builtin_skills() {
        let listed = list_sources(Some(SourceType::Skill), None).unwrap();
        assert!(!listed
            .iter()
            .any(|source| source.source_type == SourceType::BuiltinSkill));
    }

    #[test]
    fn filesystem_skill_suppresses_same_named_builtin() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        let skill_dir = project
            .join(".agents")
            .join("skills")
            .join("goose-doc-guide");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            build_skill_md("goose-doc-guide", "project override", "Use project docs"),
        )
        .unwrap();

        let builtins = list_sources(
            Some(SourceType::BuiltinSkill),
            Some(project.to_str().unwrap()),
        )
        .unwrap();
        assert!(!builtins
            .iter()
            .any(|source| source.name == "goose-doc-guide"));

        let skills =
            list_sources(Some(SourceType::Skill), Some(project.to_str().unwrap())).unwrap();
        let project_skill = skills
            .iter()
            .find(|source| source.name == "goose-doc-guide")
            .expect("expected project skill");
        assert_eq!(project_skill.source_type, SourceType::Skill);
        assert_eq!(project_skill.description, "project override");
    }

    #[test]
    fn mutations_reject_non_writable_source_types() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().to_str().unwrap();

        let err = create_source(
            SourceType::BuiltinSkill,
            "x",
            "d",
            "c",
            None,
            false,
            Some(project),
        )
        .unwrap_err();
        assert!(format!("{:?}", err).contains("not supported"));

        let err = update_source(
            SourceType::BuiltinSkill,
            "builtin://skills/x",
            "x",
            "d",
            "c",
            None,
        )
        .unwrap_err();
        assert!(format!("{:?}", err).contains("not supported"));

        let err = update_source(SourceType::Recipe, "x", "x", "d", "c", None).unwrap_err();
        assert!(format!("{:?}", err).contains("not supported"));

        let err = delete_source(SourceType::BuiltinSkill, "builtin://skills/x").unwrap_err();
        assert!(format!("{:?}", err).contains("not supported"));

        let err = delete_source(SourceType::Subrecipe, "x").unwrap_err();
        assert!(format!("{:?}", err).contains("not supported"));

        let listed = list_sources(Some(SourceType::BuiltinSkill), Some(project)).unwrap();
        assert!(listed
            .iter()
            .any(|source| source.source_type == SourceType::BuiltinSkill));

        let err = list_sources(Some(SourceType::Recipe), Some(project)).unwrap_err();
        assert!(format!("{:?}", err).contains("not supported"));

        let err = export_source(SourceType::BuiltinSkill, "builtin://skills/x").unwrap_err();
        assert!(format!("{:?}", err).contains("not supported"));

        let err = export_source(SourceType::Recipe, "x").unwrap_err();
        assert!(format!("{:?}", err).contains("not supported"));

        let payload = serde_json::json!({
            "version": 1,
            "type": "builtinSkill",
            "name": "x",
            "description": "d",
            "content": "c",
        })
        .to_string();
        let err = import_sources(&payload, false, Some(project)).unwrap_err();
        assert!(format!("{:?}", err).contains("not supported"));
    }

    #[test]
    fn import_export_remain_skill_only() {
        let err = export_source(SourceType::Agent, "x").unwrap_err();
        assert!(format!("{:?}", err).contains("Only 'skill'"));

        let payload = serde_json::json!({
            "version": 1,
            "type": "agent",
            "name": "Agent",
            "description": "d",
            "content": "c",
        })
        .to_string();
        let err = import_sources(&payload, true, None).unwrap_err();
        assert!(format!("{:?}", err).contains("Only 'skill'"));
    }

    #[test]
    fn update_derives_name_from_frontmatter() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().to_str().unwrap();

        create_source(
            SourceType::Skill,
            "my-dir",
            "orig",
            "body",
            None,
            false,
            Some(project),
        )
        .unwrap();

        let skill_dir = tmp.path().join(".agents").join("skills").join("my-dir");
        let updated = update_source(
            SourceType::Skill,
            skill_dir.to_str().unwrap(),
            "my-dir",
            "new description",
            "new body",
            None,
        )
        .unwrap();
        // Name is derived from the frontmatter written by create_source
        assert_eq!(updated.name, "my-dir");
    }

    #[test]
    fn list_sources_reads_project_agents_skills() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join(".agents").join("skills").join("test-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            build_skill_md("test-skill", "from agents", "Body"),
        )
        .unwrap();

        let listed =
            list_sources(Some(SourceType::Skill), Some(tmp.path().to_str().unwrap())).unwrap();
        let skill = listed
            .iter()
            .find(|source| source.name == "test-skill" && !source.global)
            .unwrap();
        assert!(skill.directory.contains(".agents/skills"));
        assert_eq!(skill.description, "from agents");
    }

    #[test]
    fn project_sources_prefer_agents_directory_over_legacy_goose() {
        let tmp = TempDir::new().unwrap();
        let agents_skill_dir = tmp
            .path()
            .join(".agents")
            .join("skills")
            .join("shared-skill");
        let legacy_skill_dir = tmp
            .path()
            .join(".goose")
            .join("skills")
            .join("shared-skill");
        std::fs::create_dir_all(&agents_skill_dir).unwrap();
        std::fs::create_dir_all(&legacy_skill_dir).unwrap();
        std::fs::write(
            agents_skill_dir.join("SKILL.md"),
            build_skill_md("shared-skill", "preferred", "Agents"),
        )
        .unwrap();
        std::fs::write(
            legacy_skill_dir.join("SKILL.md"),
            build_skill_md("shared-skill", "legacy", "Goose"),
        )
        .unwrap();

        let listed =
            list_sources(Some(SourceType::Skill), Some(tmp.path().to_str().unwrap())).unwrap();
        let matching: Vec<_> = listed
            .iter()
            .filter(|source| source.name == "shared-skill" && !source.global)
            .collect();
        assert_eq!(matching.len(), 1);
        assert!(matching[0].directory.contains(".agents/skills"));
        assert_eq!(matching[0].description, "preferred");

        let exported = export_source(SourceType::Skill, matching[0].directory.as_str()).unwrap();
        assert!(exported.0.contains("preferred"));
    }

    #[test]
    fn update_rejects_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        let escaped_dir = project.join(".goose").join("escaped");
        std::fs::create_dir_all(&escaped_dir).unwrap();
        std::fs::write(
            escaped_dir.join("SKILL.md"),
            "---\nname: escaped\ndescription: escaped\n---\ncontent",
        )
        .unwrap();

        let attempted_escape = project.join(".goose").join("escaped");
        let err = update_source(
            SourceType::Skill,
            attempted_escape.to_str().unwrap(),
            "escaped",
            "new description",
            "new content",
            None,
        )
        .unwrap_err();
        assert!(format!("{:?}", err).contains("not found"));
    }
}
