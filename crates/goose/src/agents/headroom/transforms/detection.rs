//! Stage-3d ContentRouter detection chain.
//!
//! Wires together content detection for multi-format compression routing.
//! Currently uses regex-based content_detector for simplicity (no ML/ONNX
//! loading required). Can be extended later with magika integration.

use crate::agents::headroom::transforms::content_detector::ContentType;
use crate::agents::headroom::transforms::unidiff_detector::is_diff;

/// Detect the content type of `content` for routing to appropriate compressor.
///
/// Detection order:
/// 1. Try regex-based content detector (matches multiple types at once)
/// 2. If regex is uncertain, use unidiff parser as a fallback
/// 3. Return the detected type or PlainText as default
pub fn detect(content: &str) -> ContentType {
    if content.is_empty() {
        return ContentType::PlainText;
    }

    // Use the regex-based content detector which handles all types
    let result =
        crate::agents::headroom::transforms::content_detector::detect_content_type(content);

    // If we got a high-confidence result, use it
    if result.confidence >= 0.6 {
        return result.content_type;
    }

    // Low confidence: use unidiff parser as fallback
    if is_diff(content) {
        return ContentType::GitDiff;
    }

    // Fallback to what the regex detector suggested, or PlainText
    if result.confidence > 0.0 {
        result.content_type
    } else {
        ContentType::PlainText
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_plain_text() {
        assert_eq!(detect(""), ContentType::PlainText);
    }

    #[test]
    fn json_array_detected() {
        let payload = r#"[{"id": 1}, {"id": 2}]"#;
        assert_eq!(detect(payload), ContentType::JsonArray);
    }

    #[test]
    fn git_diff_detected() {
        let diff = "diff --git a/foo.py b/foo.py\n\
                    --- a/foo.py\n\
                    +++ b/foo.py\n\
                    @@ -1,1 +1,2 @@\n \
                    def hello():\n\
                    +    print(\"new\")\n";
        assert_eq!(detect(diff), ContentType::GitDiff);
    }

    #[test]
    fn build_output_detected() {
        let log = "[INFO] Starting build\n[ERROR] Compilation failed\nFAILED test_one\n";
        assert_eq!(detect(log), ContentType::BuildOutput);
    }

    #[test]
    fn search_results_detected() {
        let grep = "src/foo.py:42:def process():\nsrc/bar.py:10:    return True\n";
        assert_eq!(detect(grep), ContentType::SearchResults);
    }

    #[test]
    fn plain_text_is_fallback() {
        let prose = "Just some random text without any special structure.";
        assert_eq!(detect(prose), ContentType::PlainText);
    }
}
