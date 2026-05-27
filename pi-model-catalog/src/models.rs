//! Model catalog data.
//!
//! Loads model data from the embedded `models.json` at runtime (once, on first
//! access) and provides lookup queries.
//!
//! Mirrors `packages/ai/src/models.generated.ts` and `packages/ai/src/models.ts`.

use std::collections::HashMap;
use std::sync::LazyLock;

use pi_ai_core::types::{KnownProvider, Model};
use serde::Deserialize;

// ── Embedded JSON data ──────────────────────────────────────────────

/// Raw model entry as stored in `models.json`.
///
/// Field names match the JSON exactly (snake_case).
#[derive(Deserialize)]
#[allow(dead_code)]
struct ModelData {
    id: String,
    name: Option<String>,
    api: String,
    provider: String,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    supports_thinking: bool,
    #[serde(default)]
    supports_image_input: bool,
    #[serde(default = "return_true")]
    supports_tools: bool,
    #[serde(default = "return_true")]
    supports_streaming: bool,
    context_window: u64,
    max_tokens: u64,
    #[serde(default)]
    cost_per_input_token: Option<f64>,
    #[serde(default)]
    cost_per_output_token: Option<f64>,
    #[serde(default)]
    cost_per_cache_read_token: Option<f64>,
    #[serde(default)]
    cost_per_cache_write_token: Option<f64>,
}

const fn return_true() -> bool {
    true
}

impl ModelData {
    fn into_model(self) -> Model {
        Model {
            id: self.id,
            provider: parse_provider(&self.provider)
                .expect("Unknown provider in model data"),
            api: self.api,
            name: self.name,
            base_url: None,
            supports_thinking: self.supports_thinking,
            supports_tools: self.supports_tools,
            supports_streaming: self.supports_streaming,
            supports_image_input: self.supports_image_input,
            max_tokens: None,
            max_input_tokens: Some(self.context_window),
            max_output_tokens: Some(self.max_tokens),
            cost_per_input_token: self.cost_per_input_token,
            cost_per_output_token: self.cost_per_output_token,
            cost_per_cache_read_token: self.cost_per_cache_read_token,
            cost_per_cache_write_token: self.cost_per_cache_write_token,
        }
    }
}

/// Parse a JSON provider string into a `KnownProvider`.
fn parse_provider(s: &str) -> Option<KnownProvider> {
    match s {
        "anthropic" => Some(KnownProvider::Anthropic),
        "openai" => Some(KnownProvider::OpenAi),
        "google" => Some(KnownProvider::Google),
        "mistral" => Some(KnownProvider::Mistral),
        "bedrock" => Some(KnownProvider::Bedrock),
        "faux" => Some(KnownProvider::Faux),
        _ => None,
    }
}

// ── Registry ────────────────────────────────────────────────────────

struct Registry {
    all: Vec<Model>,
    by_id: HashMap<String, usize>,
    by_provider: HashMap<KnownProvider, Vec<usize>>,
}

impl Registry {
    fn load() -> Self {
        let json_data: Vec<ModelData> =
            serde_json::from_str(include_str!("../models.json"))
                .expect("Failed to parse embedded models.json");

        let mut all = Vec::with_capacity(json_data.len());
        let mut by_id = HashMap::new();
        let mut by_provider: HashMap<KnownProvider, Vec<usize>> = HashMap::new();

        for md in json_data {
            let idx = all.len();
            let model = md.into_model();
            by_id.insert(model.id.clone(), idx);
            by_provider
                .entry(model.provider)
                .or_default()
                .push(idx);
            all.push(model);
        }

        Registry {
            all,
            by_id,
            by_provider,
        }
    }
}

static REGISTRY: LazyLock<Registry> = LazyLock::new(Registry::load);

// ── Public API ──────────────────────────────────────────────────────

/// Return all known models.
pub fn all_models() -> &'static [Model] {
    &REGISTRY.all
}

/// Return the list of unique providers present in the catalog.
pub fn get_providers() -> Vec<KnownProvider> {
    REGISTRY.by_provider.keys().copied().collect()
}

/// Return all models for a given provider.
pub fn get_models(provider: KnownProvider) -> Vec<&'static Model> {
    REGISTRY
        .by_provider
        .get(&provider)
        .map(|indices| {
            indices
                .iter()
                .map(|&i| &REGISTRY.all[i])
                .collect()
        })
        .unwrap_or_default()
}

/// Find a model by its ID string.
pub fn find_model(id: &str) -> Option<&'static Model> {
    REGISTRY.by_id.get(id).map(|&i| &REGISTRY.all[i])
}

/// Find a model by provider + ID.
pub fn get_model(provider: KnownProvider, id: &str) -> Option<&'static Model> {
    REGISTRY
        .by_id
        .get(id)
        .filter(|&&i| REGISTRY.all[i].provider == provider)
        .map(|&i| &REGISTRY.all[i])
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_models_non_empty() {
        let models = all_models();
        assert!(!models.is_empty(), "should have at least one model");
    }

    #[test]
    fn test_all_models_contains_openai_models() {
        let models = all_models();
        assert!(models.iter().any(|m| m.provider == KnownProvider::OpenAi));
    }

    #[test]
    fn test_all_models_contains_anthropic_models() {
        let models = all_models();
        assert!(models.iter().any(|m| m.provider == KnownProvider::Anthropic));
    }

    #[test]
    fn test_all_models_contains_google_models() {
        let models = all_models();
        assert!(models.iter().any(|m| m.provider == KnownProvider::Google));
    }

    #[test]
    fn test_get_providers() {
        let providers = get_providers();
        assert!(providers.contains(&KnownProvider::OpenAi));
        assert!(providers.contains(&KnownProvider::Anthropic));
        assert!(providers.contains(&KnownProvider::Google));
    }

    #[test]
    fn test_get_models_by_provider() {
        let openai_models = get_models(KnownProvider::OpenAi);
        assert!(!openai_models.is_empty(), "should have openai models");
        for m in &openai_models {
            assert_eq!(m.provider, KnownProvider::OpenAi);
        }
    }

    #[test]
    fn test_get_models_google() {
        let google_models = get_models(KnownProvider::Google);
        assert!(!google_models.is_empty(), "should have google models");
        for m in &google_models {
            assert_eq!(m.provider, KnownProvider::Google);
        }
    }

    #[test]
    fn test_find_model_by_id() {
        let model = find_model("gpt-4o").expect("gpt-4o should exist");
        assert_eq!(model.id, "gpt-4o");
        assert_eq!(model.provider, KnownProvider::OpenAi);
    }

    #[test]
    fn test_find_model_anthropic() {
        let model = find_model("claude-sonnet-4-20250514")
            .expect("claude-sonnet-4-20250514 should exist");
        assert_eq!(model.provider, KnownProvider::Anthropic);
    }

    #[test]
    fn test_find_model_not_found() {
        assert!(find_model("nonexistent-model-xyz").is_none());
    }

    #[test]
    fn test_get_model_with_provider() {
        let model = get_model(KnownProvider::OpenAi, "gpt-4o")
            .expect("gpt-4o should exist for openai");
        assert_eq!(model.id, "gpt-4o");
    }

    #[test]
    fn test_get_model_wrong_provider() {
        // gpt-4o exists for openai, not for anthropic
        let model = get_model(KnownProvider::Anthropic, "gpt-4o");
        assert!(model.is_none());
    }

    #[test]
    fn test_model_has_cost_data() {
        let model = find_model("gpt-4o").expect("gpt-4o should exist");
        assert!(
            model.cost_per_input_token.is_some(),
            "gpt-4o should have input cost"
        );
        assert!(
            model.cost_per_output_token.is_some(),
            "gpt-4o should have output cost"
        );
    }

    #[test]
    fn test_model_has_context_window() {
        let model = find_model("gpt-4o").expect("gpt-4o should exist");
        assert_eq!(model.max_input_tokens, Some(128_000));
    }

    #[test]
    fn test_model_max_tokens() {
        let model = find_model("gpt-4o").expect("gpt-4o should exist");
        assert_eq!(model.max_output_tokens, Some(16_384));
    }

    #[test]
    fn test_supports_thinking() {
        // o3-mini supports thinking
        let o3 = find_model("o3-mini").expect("o3-mini should exist");
        assert!(o3.supports_thinking, "o3-mini should support thinking");

        // gpt-4o does not (by default)
        let gpt4o = find_model("gpt-4o").expect("gpt-4o should exist");
        assert!(!gpt4o.supports_thinking, "gpt-4o should not support thinking");
    }

    #[test]
    fn test_supports_image_input() {
        let model = find_model("gpt-4o").expect("gpt-4o should exist");
        assert!(model.supports_image_input, "gpt-4o should support image input");
    }

    #[test]
    fn test_cost_conversion() {
        // gpt-4o TS cost.input = 2.5 ($/1M tokens)
        // Per-token = 2.5 / 1_000_000 = 0.0000025
        let model = find_model("gpt-4o").expect("gpt-4o should exist");
        let expected = 2.5 / 1_000_000.0;
        let actual = model.cost_per_input_token.unwrap();
        let diff = (actual - expected).abs();
        assert!(
            diff < 1e-12,
            "cost_per_input_token mismatch: expected {expected}, got {actual}, diff {diff}"
        );
    }

    #[test]
    fn test_all_provider_counts() {
        // Smoke test: ensure all known provider keys are accounted for
        let providers = get_providers();
        // At minimum we should have these
        for p in &[
            KnownProvider::OpenAi,
            KnownProvider::Anthropic,
            KnownProvider::Google,
        ] {
            assert!(
                providers.contains(p),
                "missing provider {p:?}"
            );
        }
    }
}
