pub mod client;

use std::collections::HashMap;
use std::env;

use anyhow::{Context, Result, bail};

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
}
