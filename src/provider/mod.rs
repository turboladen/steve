pub mod client;

use std::{collections::HashMap, env};

use anyhow::{Context, Result};

use crate::config::{Config, ModelConfig, ProviderConfig};
use client::LlmClient;

/// Why a provider could not be initialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderInitReason {
    /// `api_key_env` is unset in the environment.
    MissingEnvVar,
    /// `api_key_env` is set but its value is not valid UTF-8. Distinct from
    /// `MissingEnvVar` so the user isn't told to "set" a variable that's
    /// already set — the remediation is to re-export it with a valid value.
    NonUtf8EnvVar,
}

/// A provider that could not be initialized. Surfaced as a diagnostic so users
/// see *which* provider is disabled, *which* env var is involved, and *why*,
/// instead of an opaque "provider setup failed" chat message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInitWarning {
    pub provider_id: String,
    pub env_var: String,
    pub reason: ProviderInitReason,
}

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
    ///
    /// Providers whose `api_key_env` variable is not set are **skipped** and
    /// reported in the returned warnings list. Previously a single missing env
    /// var aborted registry construction entirely, making all providers
    /// unavailable (steve-itzf).
    pub fn from_config(config: &Config) -> (Self, Vec<ProviderInitWarning>) {
        let mut providers = HashMap::new();
        let mut warnings = Vec::new();

        for (provider_id, provider_config) in &config.providers {
            match env::var(&provider_config.api_key_env) {
                Ok(api_key) => {
                    let client = LlmClient::new(&provider_config.base_url, &api_key);
                    providers.insert(
                        provider_id.clone(),
                        ProviderEntry {
                            config: provider_config.clone(),
                            client,
                        },
                    );
                }
                Err(e) => {
                    let reason = match e {
                        env::VarError::NotPresent => ProviderInitReason::MissingEnvVar,
                        env::VarError::NotUnicode(_) => ProviderInitReason::NonUtf8EnvVar,
                    };
                    warnings.push(ProviderInitWarning {
                        provider_id: provider_id.clone(),
                        env_var: provider_config.api_key_env.clone(),
                        reason,
                    });
                }
            }
        }

        // HashMap iteration is non-deterministic; sort so startup messages
        // and overlay entries appear in stable order across runs.
        warnings.sort_by(|a, b| a.provider_id.cmp(&b.provider_id));

        (Self { providers }, warnings)
    }

    /// Resolve a model reference like "provider/model" to a ResolvedModel.
    pub fn resolve_model(&self, model_ref: &str) -> Result<ResolvedModel> {
        let (provider_id, model_id) = model_ref.split_once('/').with_context(|| {
            format!("invalid model ref '{model_ref}', expected 'provider/model'")
        })?;

        let entry = self
            .providers
            .get(provider_id)
            .with_context(|| format!("provider '{provider_id}' not configured"))?;

        let model_config =
            entry.config.models.get(model_id).with_context(|| {
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

    /// Iterator over the provider IDs that successfully initialized.
    pub fn provider_ids(&self) -> impl Iterator<Item = &str> {
        self.providers.keys().map(String::as_str)
    }

    /// Build a registry from pre-constructed entries (no env var lookups).
    #[cfg(test)]
    pub fn from_entries(entries: Vec<(String, ProviderConfig, client::LlmClient)>) -> Self {
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
    use crate::config::{ModelCapabilities, ModelConfig, ModelCost, ProviderConfig};

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

    /// Build a `ProviderConfig` wired to the given `api_key_env` name.
    fn make_provider(api_key_env: &str) -> ProviderConfig {
        ProviderConfig {
            base_url: "https://api.test.com/v1".to_string(),
            api_key_env: api_key_env.to_string(),
            models: HashMap::new(),
        }
    }

    #[test]
    fn from_config_keeps_providers_with_set_env_vars_and_warns_for_unset() {
        // Use test-specific env var names so parallel tests don't clash, and
        // so we never depend on the developer's actual API-key env vars.
        const SET_VAR: &str = "STEVE_TEST_ITZF_SET";
        const UNSET_VAR: &str = "STEVE_TEST_ITZF_UNSET";

        // SAFETY: single-threaded per-test and these env var names are
        // namespaced to this test — no cross-test pollution.
        // (`set_var` is unsafe in Rust 2024 edition due to process-global
        // state; acceptable here because the name is unique.)
        unsafe {
            env::set_var(SET_VAR, "fake-key-value");
            env::remove_var(UNSET_VAR);
        }

        let mut providers = HashMap::new();
        providers.insert("good".to_string(), make_provider(SET_VAR));
        providers.insert("bad".to_string(), make_provider(UNSET_VAR));

        let config = Config {
            providers,
            ..Config::default()
        };

        let (registry, warnings) = ProviderRegistry::from_config(&config);

        assert!(
            registry.providers.contains_key("good"),
            "provider 'good' with set env var should be registered",
        );
        assert!(
            !registry.providers.contains_key("bad"),
            "provider 'bad' with unset env var must be skipped, not registered with an empty key",
        );
        assert_eq!(
            warnings.len(),
            1,
            "exactly one missing-env-var warning expected"
        );
        assert_eq!(warnings[0].provider_id, "bad");
        assert_eq!(warnings[0].env_var, UNSET_VAR);
        assert_eq!(warnings[0].reason, ProviderInitReason::MissingEnvVar);

        // Cleanup — avoid leaking the env var into sibling tests.
        unsafe {
            env::remove_var(SET_VAR);
        }
    }

    /// `env::var` can return `VarError::NotUnicode` when the env var *is* set
    /// but contains bytes that aren't valid UTF-8 — a distinct failure mode
    /// from "not set" that the user needs to fix differently. Only runs on
    /// Unix where we can construct an `OsString` from raw bytes.
    #[cfg(unix)]
    #[test]
    fn from_config_distinguishes_non_utf8_env_var_from_missing() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        const NON_UTF8_VAR: &str = "STEVE_TEST_ITZF_BADUTF8";

        // SAFETY: test-unique name, same rationale as the other env tests.
        unsafe {
            // 0xFF is not valid UTF-8.
            env::set_var(
                NON_UTF8_VAR,
                OsString::from_vec(vec![0xFF, b'k', b'e', b'y']),
            );
        }

        let mut providers = HashMap::new();
        providers.insert("broken".to_string(), make_provider(NON_UTF8_VAR));
        let config = Config {
            providers,
            ..Config::default()
        };

        let (registry, warnings) = ProviderRegistry::from_config(&config);

        assert!(
            registry.is_empty(),
            "provider with bad UTF-8 must be skipped"
        );
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].reason, ProviderInitReason::NonUtf8EnvVar);

        unsafe {
            env::remove_var(NON_UTF8_VAR);
        }
    }

    #[test]
    fn from_config_returns_empty_registry_when_all_env_vars_unset_and_sorts_warnings() {
        const UNSET_A: &str = "STEVE_TEST_ITZF_UNSET_A";
        const UNSET_B: &str = "STEVE_TEST_ITZF_UNSET_B";

        unsafe {
            env::remove_var(UNSET_A);
            env::remove_var(UNSET_B);
        }

        let mut providers = HashMap::new();
        // Deliberately insert in reverse alpha order — from_config should
        // still emit warnings sorted by provider_id for stable startup output.
        providers.insert("zeta".to_string(), make_provider(UNSET_B));
        providers.insert("alpha".to_string(), make_provider(UNSET_A));

        let config = Config {
            providers,
            ..Config::default()
        };

        let (registry, warnings) = ProviderRegistry::from_config(&config);

        assert!(registry.is_empty());
        assert_eq!(warnings.len(), 2);
        assert_eq!(
            warnings[0].provider_id, "alpha",
            "warnings must be sorted by provider_id for deterministic startup output",
        );
        assert_eq!(warnings[1].provider_id, "zeta");
    }
}
