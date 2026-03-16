pub mod client;

use std::collections::HashMap;
use std::env;

use anyhow::{Context, Result};

use crate::config::types::{Config, ModelConfig, ProviderConfig};
use client::LlmClient;

/// A resolved model with its provider context.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub provider_id: String,
    pub model_id: String,
    pub config: ModelConfig,
    pub provider_config: ProviderConfig,
}

impl ResolvedModel {
    /// The API model ID to send in requests.
    pub fn api_model_id(&self) -> &str {
        &self.config.id
    }

    /// Display string: "provider/model"
    pub fn display_ref(&self) -> String {
        format!("{}/{}", self.provider_id, self.model_id)
    }

    /// Session cost based on token usage and model pricing.
    /// Returns None if model has no pricing configured or no tokens have been used.
    pub fn session_cost(&self, prompt_tokens: u64, completion_tokens: u64) -> Option<f64> {
        let cost = self.config.cost.as_ref()?;
        if prompt_tokens == 0 && completion_tokens == 0 {
            return None; // No usage reported — show N/A, not $0.0000
        }
        let input_cost = prompt_tokens as f64 * cost.input_per_million / 1_000_000.0;
        let output_cost = completion_tokens as f64 * cost.output_per_million / 1_000_000.0;
        Some(input_cost + output_cost)
    }
}

/// Registry of configured providers and their models.
pub struct ProviderRegistry {
    providers: HashMap<String, ProviderEntry>,
}

struct ProviderEntry {
    config: ProviderConfig,
    client: LlmClient,
}

impl ProviderRegistry {
    /// Build the registry from configuration.
    pub fn from_config(config: &Config) -> Result<Self> {
        let mut providers = HashMap::new();

        for (provider_id, provider_config) in &config.providers {
            // Resolve the API key from the environment
            let api_key = env::var(&provider_config.api_key_env).with_context(|| {
                format!(
                    "environment variable '{}' not set for provider '{}'",
                    provider_config.api_key_env, provider_id
                )
            })?;

            let client = LlmClient::new(&provider_config.base_url, &api_key);

            providers.insert(
                provider_id.clone(),
                ProviderEntry {
                    config: provider_config.clone(),
                    client,
                },
            );
        }

        Ok(Self { providers })
    }

    /// Resolve a model reference like "provider/model" to a ResolvedModel.
    pub fn resolve_model(&self, model_ref: &str) -> Result<ResolvedModel> {
        let (provider_id, model_id) = model_ref
            .split_once('/')
            .with_context(|| format!("invalid model ref '{model_ref}', expected 'provider/model'"))?;

        let entry = self
            .providers
            .get(provider_id)
            .with_context(|| format!("provider '{provider_id}' not configured"))?;

        let model_config = entry
            .config
            .models
            .get(model_id)
            .with_context(|| {
                format!("model '{model_id}' not found in provider '{provider_id}'")
            })?;

        Ok(ResolvedModel {
            provider_id: provider_id.to_string(),
            model_id: model_id.to_string(),
            config: model_config.clone(),
            provider_config: entry.config.clone(),
        })
    }

    /// Get the LLM client for a provider.
    pub fn client(&self, provider_id: &str) -> Result<&LlmClient> {
        self.providers
            .get(provider_id)
            .map(|e| &e.client)
            .with_context(|| format!("provider '{provider_id}' not configured"))
    }

    /// List all available models across all providers.
    pub fn list_models(&self) -> Vec<ResolvedModel> {
        let mut models = Vec::new();
        for (provider_id, entry) in &self.providers {
            for (model_id, model_config) in &entry.config.models {
                models.push(ResolvedModel {
                    provider_id: provider_id.clone(),
                    model_id: model_id.clone(),
                    config: model_config.clone(),
                    provider_config: entry.config.clone(),
                });
            }
        }
        models
    }

    /// Check if the registry has any providers configured.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Build a registry from pre-constructed entries (no env var lookups).
    #[cfg(test)]
    pub fn from_entries(
        entries: Vec<(String, ProviderConfig, client::LlmClient)>,
    ) -> Self {
        let mut providers = HashMap::new();
        for (id, config, client) in entries {
            providers.insert(id, ProviderEntry { config, client });
        }
        Self { providers }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{ModelCapabilities, ModelCost, ModelConfig, ProviderConfig};

    fn make_test_resolved_model() -> ResolvedModel {
        ResolvedModel {
            provider_id: "test".to_string(),
            model_id: "test-model".to_string(),
            config: ModelConfig {
                id: "test-model".to_string(),
                name: "Test Model".to_string(),
                context_window: 128_000,
                max_output_tokens: None,
                cost: None,
                capabilities: ModelCapabilities {
                    tool_call: true,
                    reasoning: false,
                },
            },
            provider_config: ProviderConfig {
                base_url: "https://api.test.com/v1".to_string(),
                api_key_env: "TEST_API_KEY".to_string(),
                models: HashMap::new(),
            },
        }
    }

    #[test]
    fn session_cost_calculation() {
        let mut model = make_test_resolved_model();
        model.config.cost = Some(ModelCost {
            input_per_million: 0.50,
            output_per_million: 2.00,
        });
        let cost = model.session_cost(1_000_000, 500_000).unwrap();
        assert!((cost - 1.50).abs() < 0.001); // 0.50 + 1.00
    }

    #[test]
    fn session_cost_none_without_pricing() {
        let model = make_test_resolved_model();
        assert!(model.session_cost(1_000_000, 500_000).is_none());
    }

    #[test]
    fn session_cost_none_with_zero_tokens() {
        let mut model = make_test_resolved_model();
        model.config.cost = Some(ModelCost {
            input_per_million: 0.50,
            output_per_million: 2.00,
        });
        // Zero tokens should return None (N/A), not Some(0.0)
        assert!(model.session_cost(0, 0).is_none());
    }
}
