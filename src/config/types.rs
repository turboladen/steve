use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Top-level configuration for steve.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Default model in "provider/model" format.
    #[serde(default)]
    pub model: Option<String>,

    /// Small/fast model for title generation, in "provider/model" format.
    #[serde(default)]
    pub small_model: Option<String>,

    /// Whether to automatically compact when approaching context window limit.
    #[serde(default = "default_auto_compact")]
    pub auto_compact: bool,

    /// Provider definitions keyed by provider ID.
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
}

/// Configuration for a single LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Base URL for the OpenAI-compatible API (e.g., "https://api.openai.com/v1").
    pub base_url: String,

    /// Name of the environment variable containing the API key.
    pub api_key_env: String,

    /// Models available from this provider, keyed by model ID.
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,
}

/// Configuration for a single model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// The model ID sent to the API.
    pub id: String,

    /// Human-readable display name.
    pub name: String,

    /// Maximum context window in tokens.
    #[serde(default = "default_context_window")]
    pub context_window: u32,

    /// Maximum output tokens (if limited).
    #[serde(default)]
    pub max_output_tokens: Option<u32>,

    /// Pricing information.
    #[serde(default)]
    pub cost: Option<ModelCost>,

    /// Model capabilities.
    #[serde(default)]
    pub capabilities: ModelCapabilities,
}

/// Pricing per million tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCost {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

/// Feature capabilities of a model.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelCapabilities {
    /// Whether the model supports tool/function calling.
    #[serde(default)]
    pub tool_call: bool,

    /// Whether the model supports reasoning/thinking tokens.
    #[serde(default)]
    pub reasoning: bool,
}

fn default_context_window() -> u32 {
    128_000
}

fn default_auto_compact() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_auto_compact_true() {
        let config: Config = serde_json::from_str("{}").unwrap();
        assert!(config.auto_compact);
    }

    #[test]
    fn auto_compact_can_be_disabled() {
        let config: Config = serde_json::from_str(r#"{"auto_compact": false}"#).unwrap();
        assert!(!config.auto_compact);
    }

    #[test]
    fn default_context_window_is_128k() {
        let model: ModelConfig = serde_json::from_str(
            r#"{"id": "test", "name": "Test"}"#,
        )
        .unwrap();
        assert_eq!(model.context_window, 128_000);
    }

    #[test]
    fn custom_context_window() {
        let model: ModelConfig = serde_json::from_str(
            r#"{"id": "test", "name": "Test", "context_window": 32000}"#,
        )
        .unwrap();
        assert_eq!(model.context_window, 32_000);
    }

    #[test]
    fn capabilities_default_to_false() {
        let model: ModelConfig = serde_json::from_str(
            r#"{"id": "test", "name": "Test"}"#,
        )
        .unwrap();
        assert!(!model.capabilities.tool_call);
        assert!(!model.capabilities.reasoning);
    }

    #[test]
    fn full_config_parses() {
        let json = r#"{
            "model": "openai/gpt-4o",
            "small_model": "openai/gpt-4o-mini",
            "auto_compact": true,
            "providers": {
                "openai": {
                    "base_url": "https://api.openai.com/v1",
                    "api_key_env": "OPENAI_API_KEY",
                    "models": {
                        "gpt-4o": {
                            "id": "gpt-4o",
                            "name": "GPT-4o",
                            "context_window": 128000,
                            "capabilities": { "tool_call": true, "reasoning": false }
                        }
                    }
                }
            }
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.model, Some("openai/gpt-4o".into()));
        assert_eq!(config.small_model, Some("openai/gpt-4o-mini".into()));
        assert!(config.providers.contains_key("openai"));
        let openai = &config.providers["openai"];
        assert_eq!(openai.models["gpt-4o"].context_window, 128_000);
        assert!(openai.models["gpt-4o"].capabilities.tool_call);
    }

    #[test]
    fn empty_providers_is_valid() {
        let config: Config = serde_json::from_str(r#"{"model": "test/m"}"#).unwrap();
        assert!(config.providers.is_empty());
    }
}
