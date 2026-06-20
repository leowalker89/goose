//! SmartCrusher — JSON array compression for structured tool outputs.
//!
//! A stub implementation that delegates JSON compression to simple heuristics.
//! The full SmartCrusher (11k+ lines) is a complex JSON AST analyzer; for now
//! we keep the interface but use deterministic fallbacks.

use serde_json::Value;

#[derive(Debug, Clone)]
pub struct SmartCrusherConfig {
    /// Maximum depth to traverse in JSON structures
    pub max_depth: usize,
}

impl Default for SmartCrusherConfig {
    fn default() -> Self {
        Self { max_depth: 10 }
    }
}

#[derive(Debug, Clone)]
pub struct SmartCrusher {
    #[allow(dead_code)]
    config: SmartCrusherConfig,
}

#[derive(Debug, Clone)]
pub struct CrusherResult {
    pub crushed: String,
    pub original_bytes: usize,
    pub crushed_bytes: usize,
}

impl SmartCrusher {
    pub fn new(config: SmartCrusherConfig) -> Self {
        Self { config }
    }

    /// Attempt to compress a JSON array. For now, this is a simple
    /// prettify → minify fallback that removes whitespace.
    /// A full port of upstream SmartCrusher would do structured
    /// AST-aware compression (field deduplication, array collapsing, etc.).
    pub fn crush(&self, content: &str, _query: &str, _bias: f64) -> Result<CrusherResult, String> {
        let trimmed = content.trim();
        if !trimmed.starts_with('[') {
            return Err("not a json array".to_string());
        }

        // Try to parse as JSON
        let parsed: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => return Err(format!("json parse error: {}", e)),
        };

        // Simple minification: re-serialize without whitespace
        let minified =
            serde_json::to_string(&parsed).map_err(|e| format!("json serialize error: {}", e))?;

        let original_bytes = trimmed.len();
        let crushed_bytes = minified.len();

        // Only return the crushed version if it's actually smaller
        if crushed_bytes >= original_bytes {
            return Err("no compression gained".to_string());
        }

        Ok(CrusherResult {
            crushed: minified,
            original_bytes,
            crushed_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crush_simple_json_array() {
        let crusher = SmartCrusher::new(SmartCrusherConfig::default());
        let input = r#"[
            {"id": 1, "name": "Alice"},
            {"id": 2, "name": "Bob"}
        ]"#;
        let result = crusher.crush(input, "", 0.0).unwrap();
        assert!(result.crushed_bytes < result.original_bytes);
        assert!(result.crushed.starts_with('['));
    }

    #[test]
    fn crush_rejects_non_json() {
        let crusher = SmartCrusher::new(SmartCrusherConfig::default());
        assert!(crusher.crush("not json", "", 0.0).is_err());
    }

    #[test]
    fn crush_rejects_non_array() {
        let crusher = SmartCrusher::new(SmartCrusherConfig::default());
        assert!(crusher.crush(r#"{"id": 1}"#, "", 0.0).is_err());
    }
}
