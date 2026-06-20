//! Headroom — inline context compression for tool outputs.
//!
//! Port of the core, deterministic (ML-free) parts of
//! [headroom](https://github.com/chopratejas/headroom): compress what an agent
//! reads (tool outputs, build/test logs, grep results) *before* it reaches the
//! LLM, keeping the salient signal while cutting 60-95% of the tokens.
//!
//! A [`ContentRouter`] detects the content type of a tool output and routes it
//! to the best compressor:
//!
//! - build/test output → [`log_compressor::LogCompressor`]
//! - grep / ripgrep results → [`search_compressor::SearchCompressor`]
//! - everything else → passed through unchanged
//!
//! Compression is reversible in spirit: the compressed output always carries an
//! explicit marker (`[N lines omitted: ...]` / `[... and N more matches ...]`)
//! so the model knows content was elided and can re-run the tool for detail.

pub mod adaptive_sizer;
pub mod auth_mode;
pub mod ccr;
pub mod log_compressor;
pub mod search_compressor;
pub mod signals;
pub mod smart_crusher;
pub mod tokenizer;
pub mod transforms;

use std::sync::LazyLock;

use regex::Regex;

use log_compressor::{LogCompressor, LogCompressorConfig};
use search_compressor::{SearchCompressor, SearchCompressorConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    SearchResults,
    BuildOutput,
    PlainText,
}

#[derive(Debug, Clone)]
pub struct CompressionResult {
    pub compressed: String,
    pub original_chars: usize,
    pub compressed_chars: usize,
    pub content_type: ContentType,
    pub strategy: &'static str,
}

impl CompressionResult {
    fn passthrough(content: &str, content_type: ContentType) -> Self {
        Self {
            compressed: content.to_string(),
            original_chars: content.len(),
            compressed_chars: content.len(),
            content_type,
            strategy: "passthrough",
        }
    }

    pub fn ratio(&self) -> f64 {
        if self.original_chars == 0 {
            1.0
        } else {
            self.compressed_chars as f64 / self.original_chars as f64
        }
    }

    /// Estimated tokens saved (rough: ~4 chars per token).
    pub fn tokens_saved_estimate(&self) -> usize {
        self.original_chars.saturating_sub(self.compressed_chars) / 4
    }

    pub fn did_compress(&self) -> bool {
        self.compressed_chars < self.original_chars
    }
}

/// Routes content to the appropriate compressor based on detected type.
pub struct ContentRouter {
    log: LogCompressor,
    search: SearchCompressor,
    /// `bias` multiplier passed to the adaptive sizer (>1 keeps more).
    bias: f64,
}

impl Default for ContentRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl ContentRouter {
    pub fn new() -> Self {
        Self {
            log: LogCompressor::new(LogCompressorConfig::default()),
            search: SearchCompressor::new(SearchCompressorConfig::default()),
            bias: 1.0,
        }
    }

    pub fn with_bias(mut self, bias: f64) -> Self {
        self.bias = bias;
        self
    }

    /// Compress `content`, optionally biasing scoring toward `context`
    /// (e.g. the tool's arguments / the user's intent) for search results.
    pub fn compress(&self, content: &str, context: &str) -> CompressionResult {
        let original_chars = content.len();
        match detect_content_type(content) {
            ContentType::SearchResults => {
                let compressed = self.search.compress(content, context, self.bias).compressed;
                CompressionResult {
                    compressed_chars: compressed.len(),
                    compressed,
                    original_chars,
                    content_type: ContentType::SearchResults,
                    strategy: "search_compressor",
                }
            }
            ContentType::BuildOutput => {
                let compressed = self.log.compress(content, self.bias).compressed;
                CompressionResult {
                    compressed_chars: compressed.len(),
                    compressed,
                    original_chars,
                    content_type: ContentType::BuildOutput,
                    strategy: "log_compressor",
                }
            }
            ContentType::PlainText => {
                CompressionResult::passthrough(content, ContentType::PlainText)
            }
        }
    }
}

static SEARCH_RESULT_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[^\s:]+:\d+:").unwrap());

static LOG_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)\b(ERROR|FAIL|FAILED|FATAL|CRITICAL)\b").unwrap(),
        Regex::new(r"(?i)\b(WARN|WARNING)\b").unwrap(),
        Regex::new(r"(?i)\b(INFO|DEBUG|TRACE)\b").unwrap(),
        Regex::new(r"^\s*\d{4}-\d{2}-\d{2}").unwrap(),
        Regex::new(r"^\s*\[\d{2}:\d{2}:\d{2}\]").unwrap(),
        Regex::new(r"^={3,}|^-{3,}").unwrap(),
        Regex::new(r"^\s*PASSED|^\s*FAILED|^\s*SKIPPED").unwrap(),
        Regex::new(r"^npm ERR!|^yarn error|^cargo error").unwrap(),
        Regex::new(r"Traceback \(most recent call last\)").unwrap(),
        Regex::new(r"^\s*at\s+[\w.$]+\(").unwrap(),
    ]
});

/// Detect the content type of a tool output. Search results win over logs when
/// both match because the `file:line:` shape is more specific.
pub fn detect_content_type(content: &str) -> ContentType {
    if content.trim().is_empty() {
        return ContentType::PlainText;
    }

    if let Some(t) = try_detect_search(content) {
        return t;
    }
    if let Some(t) = try_detect_log(content) {
        return t;
    }
    ContentType::PlainText
}

fn try_detect_search(content: &str) -> Option<ContentType> {
    let lines: Vec<&str> = content.split('\n').take(100).collect();
    let mut matching = 0u32;
    for line in &lines {
        if !line.trim().is_empty() && SEARCH_RESULT_PATTERN.is_match(line) {
            matching += 1;
        }
    }
    if matching == 0 {
        return None;
    }
    let non_empty = lines.iter().filter(|l| !l.trim().is_empty()).count() as u32;
    if non_empty == 0 {
        return None;
    }
    let ratio = matching as f64 / non_empty as f64;
    if ratio >= 0.3 {
        Some(ContentType::SearchResults)
    } else {
        None
    }
}

fn try_detect_log(content: &str) -> Option<ContentType> {
    let lines: Vec<&str> = content.split('\n').take(200).collect();
    let mut pattern_matches = 0u32;
    for line in &lines {
        if LOG_PATTERNS.iter().any(|p| p.is_match(line)) {
            pattern_matches += 1;
        }
    }
    if pattern_matches == 0 {
        return None;
    }
    let non_empty = lines.iter().filter(|l| !l.trim().is_empty()).count() as u32;
    if non_empty == 0 {
        return None;
    }
    let ratio = pattern_matches as f64 / non_empty as f64;
    if ratio >= 0.1 {
        Some(ContentType::BuildOutput)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_search_results() {
        let content = "src/a.py:1:foo\nsrc/b.py:2:bar\nsrc/c.py:3:baz";
        assert_eq!(detect_content_type(content), ContentType::SearchResults);
    }

    #[test]
    fn detects_build_output() {
        let content = "INFO starting\nERROR boom happened\nWARNING careful\nINFO done";
        assert_eq!(detect_content_type(content), ContentType::BuildOutput);
    }

    #[test]
    fn plain_prose_passes_through() {
        let content = "The quick brown fox jumps over the lazy dog. Nothing to compress here.";
        let r = ContentRouter::new().compress(content, "");
        assert_eq!(r.content_type, ContentType::PlainText);
        assert_eq!(r.strategy, "passthrough");
        assert_eq!(r.compressed, content);
    }

    #[test]
    fn router_compresses_noisy_log() {
        let mut lines: Vec<String> = (0..400).map(|i| format!("INFO step {i} ok")).collect();
        lines.push("ERROR: the build broke on widget".to_string());
        let content = lines.join("\n");
        let r = ContentRouter::new().compress(&content, "");
        assert_eq!(r.content_type, ContentType::BuildOutput);
        assert!(r.did_compress());
        assert!(r.compressed.contains("the build broke on widget"));
        assert!(r.tokens_saved_estimate() > 0);
    }
}
