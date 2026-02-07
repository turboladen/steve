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
