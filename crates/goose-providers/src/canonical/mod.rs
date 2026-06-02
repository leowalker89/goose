mod model;
mod name_builder;
mod registry;

pub use model::{CanonicalModel, Limit, Modalities, Modality, Pricing};
pub use name_builder::{
    canonical_name, map_provider_name, map_to_canonical_model, strip_version_suffix,
};
pub use registry::CanonicalModelRegistry;

pub const PROVIDER_METADATA_JSON: &str = include_str!("data/provider_metadata.json");

/// Providers that run models locally — their cost is always zero regardless
/// of what the canonical registry says for the underlying model architecture.
fn is_local_provider(provider: &str) -> bool {
    matches!(provider, "ollama" | "local")
}

pub fn maybe_get_canonical_model(provider: &str, model: &str) -> Option<CanonicalModel> {
    let registry = CanonicalModelRegistry::bundled().ok()?;

    let canonical_id = map_to_canonical_model(provider, model, registry)?;
    let mut canonical = if let Some((canon_provider, canon_model)) = canonical_id.split_once('/') {
        registry.get(canon_provider, canon_model).cloned()?
    } else {
        return None;
    };

    // Local providers run models on the user's own hardware — zero out cloud
    // pricing so every consumer (CLI, server, etc.) sees the correct cost.
    if is_local_provider(provider) {
        canonical.cost = Pricing::default();
    }

    Some(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_models_have_zero_cost() {
        // "mistral-nemo" resolves to mistralai/mistral-nemo which has non-zero cloud pricing.
        // When accessed via ollama, cost must be zeroed out.
        let canonical = maybe_get_canonical_model("ollama", "mistral-nemo")
            .expect("mistral-nemo should resolve via ollama");
        assert_eq!(canonical.cost.input, None);
        assert_eq!(canonical.cost.output, None);
        assert!(
            canonical.limit.context > 0,
            "context limit should be preserved"
        );
    }

    #[test]
    fn cloud_provider_retains_cost() {
        let canonical = maybe_get_canonical_model("anthropic", "claude-3-5-sonnet-20241022")
            .expect("claude-3.5-sonnet should resolve");
        assert!(canonical.cost.input.is_some());
        assert!(canonical.cost.output.is_some());
    }
}
