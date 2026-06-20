//! Tokenizer stub for headroom integration.
//!
//! The full tokenizer system estimates token counts for different models.
//! For headroom integration in goose, we provide a simple character-to-token estimator.

use std::sync::OnceLock;

/// Simple tokenizer trait for duck-typing in live_zone
pub trait Tokenizer: Send + Sync {
    fn estimate_tokens(&self, text: &str) -> usize;
}

/// Simple tokenizer that estimates tokens based on character count.
/// Different models have different chars-per-token ratios.
pub struct SimpleTokenizer {
    chars_per_token: f64,
}

impl SimpleTokenizer {
    pub fn new(model: &str) -> Self {
        // Estimate chars-per-token based on model name
        let chars_per_token = match model {
            m if m.contains("claude") => 3.5,
            m if m.contains("gpt") => 4.0,
            _ => 4.0,
        };
        Self { chars_per_token }
    }
}

impl Tokenizer for SimpleTokenizer {
    fn estimate_tokens(&self, text: &str) -> usize {
        ((text.len() as f64) / self.chars_per_token).ceil() as usize
    }
}

/// Get or create a tokenizer for the given model
pub fn get_tokenizer(_model: &str) -> &'static dyn Tokenizer {
    static DEFAULT: OnceLock<SimpleTokenizer> = OnceLock::new();
    DEFAULT.get_or_init(|| SimpleTokenizer::new("claude-3-5-sonnet-20241022"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_estimates_claude() {
        let t = SimpleTokenizer::new("claude-3-5-sonnet-20241022");
        // "hello world" is 11 characters → ~3 tokens at 3.5 cpt
        assert!(t.estimate_tokens("hello world") >= 3);
    }

    #[test]
    fn tokenizer_estimates_gpt() {
        let t = SimpleTokenizer::new("gpt-4");
        // "hello world" is 11 characters → ~3 tokens at 4 cpt
        assert!(t.estimate_tokens("hello world") >= 2);
    }
}
