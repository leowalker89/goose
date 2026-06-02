# Canonical Model System

Provides a unified view of model metadata (pricing, capabilities, context limits) across different LLM providers.
Normalizes provider-specific model names (e.g., `claude-3-5-sonnet-20241022`) to canonical IDs (e.g., `anthropic/claude-3.5-sonnet`).

## Build Canonical Models

Fetches latest model metadata from models.dev and updates the bundled registry:

```bash
cargo run -p goose-providers --features rustls-tls --bin build_canonical_models
```

This writes to:

- `src/canonical/data/canonical_models.json`
- `src/canonical/data/provider_metadata.json`

The script is located at `src/bin/build_canonical_models.rs`.
