use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::ui::terminal_detect::ThemePreference;

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

    /// Permission profile: "trust", "standard" (default), or "cautious".
    #[serde(default)]
    pub permission_profile: Option<crate::permission::PermissionProfile>,

    /// Tools to auto-allow regardless of permission profile.
    /// e.g., `["edit", "bash"]` to skip permission prompts for those tools.
    #[serde(default)]
    pub allow_tools: Vec<String>,

    /// Path-based permission rules.
    /// e.g., `[{"tool": "edit", "pattern": "src/**", "action": "allow"}]`
    /// More specific path rules should come before general rules (first-match wins).
    #[serde(default)]
    pub permission_rules: Vec<crate::permission::types::PermissionRule>,

    /// Theme preference: "auto" (default), "dark", or "light".
    #[serde(default)]
    pub theme: ThemePreference,

    /// Provider definitions keyed by provider ID.
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,

    /// MCP server definitions keyed by server ID.
    #[serde(default)]
    pub mcp_servers: HashMap<String, crate::mcp::types::McpServerConfig>,
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
    #[serde(alias = "input")]
    pub input_per_million: f64,
    #[serde(alias = "output")]
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

impl Config {
    /// Merge a project config on top of this (global) config.
    /// Project values take precedence over global values.
    /// Providers merge by ID; models merge within providers.
    pub fn merge(mut self, project: Config) -> Config {
        // Detect whether the project config had meaningful content before moving fields.
        // This prevents a default project Config from clobbering global auto_compact.
        let project_has_content = !project.providers.is_empty()
            || !project.mcp_servers.is_empty()
            || project.model.is_some()
            || project.small_model.is_some();

        // Scalar fields: project overrides global
        if project.model.is_some() {
            self.model = project.model;
        }
        if project.small_model.is_some() {
            self.small_model = project.small_model;
        }
        if project.permission_profile.is_some() {
            self.permission_profile = project.permission_profile;
        }
        if !project.allow_tools.is_empty() {
            self.allow_tools = project.allow_tools;
        }
        if !project.permission_rules.is_empty() {
            self.permission_rules = project.permission_rules;
        }
        if project_has_content {
            self.auto_compact = project.auto_compact;
        }
        if project.theme != ThemePreference::Auto {
            self.theme = project.theme;
        }

        // MCP servers: project overrides global by server ID
        for (server_id, project_server) in project.mcp_servers {
            self.mcp_servers.insert(server_id, project_server);
        }

        // Providers: deep merge by provider ID, then by model ID
        for (provider_id, project_provider) in project.providers {
            match self.providers.get_mut(&provider_id) {
                Some(global_provider) => {
                    global_provider.merge(project_provider);
                }
                None => {
                    self.providers.insert(provider_id, project_provider);
                }
            }
        }

        self
    }
}

impl ProviderConfig {
    /// Merge a project provider on top of this (global) provider.
    /// Project values override; models merge by model ID.
    fn merge(&mut self, project: ProviderConfig) {
        // Provider-level fields: project overrides
        self.base_url = project.base_url;
        self.api_key_env = project.api_key_env;

        // Models: project overrides per model ID
        for (model_id, project_model) in project.models {
            self.models.insert(model_id, project_model);
        }
    }
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

    // -- Config::merge tests --

    #[test]
    fn merge_project_model_overrides_global() {
        let global = Config {
            model: Some("global/model".into()),
            ..Default::default()
        };
        let project = Config {
            model: Some("project/model".into()),
            ..Default::default()
        };
        let merged = global.merge(project);
        assert_eq!(merged.model, Some("project/model".into()));
    }

    #[test]
    fn merge_project_none_keeps_global() {
        let global = Config {
            model: Some("global/model".into()),
            small_model: Some("global/small".into()),
            ..Default::default()
        };
        let project = Config::default();
        let merged = global.merge(project);
        assert_eq!(merged.model, Some("global/model".into()));
        assert_eq!(merged.small_model, Some("global/small".into()));
    }

    #[test]
    fn merge_providers_new_provider_added() {
        let global = Config::default();
        let mut project = Config::default();
        project.providers.insert(
            "openai".into(),
            ProviderConfig {
                base_url: "https://api.openai.com/v1".into(),
                api_key_env: "OPENAI_API_KEY".into(),
                models: HashMap::new(),
            },
        );
        let merged = global.merge(project);
        assert!(merged.providers.contains_key("openai"));
    }

    #[test]
    fn merge_providers_deep_merge_models() {
        let mut global = Config::default();
        let mut global_models = HashMap::new();
        global_models.insert(
            "gpt-4o".into(),
            ModelConfig {
                id: "gpt-4o".into(),
                name: "GPT-4o".into(),
                context_window: 128_000,
                max_output_tokens: None,
                cost: None,
                capabilities: ModelCapabilities::default(),
            },
        );
        global.providers.insert(
            "openai".into(),
            ProviderConfig {
                base_url: "https://api.openai.com/v1".into(),
                api_key_env: "OPENAI_API_KEY".into(),
                models: global_models,
            },
        );

        let mut project = Config::default();
        let mut project_models = HashMap::new();
        project_models.insert(
            "gpt-4o-mini".into(),
            ModelConfig {
                id: "gpt-4o-mini".into(),
                name: "GPT-4o Mini".into(),
                context_window: 128_000,
                max_output_tokens: None,
                cost: None,
                capabilities: ModelCapabilities::default(),
            },
        );
        project.providers.insert(
            "openai".into(),
            ProviderConfig {
                base_url: "https://custom.proxy/v1".into(),
                api_key_env: "CUSTOM_KEY".into(),
                models: project_models,
            },
        );

        let merged = global.merge(project);
        let openai = &merged.providers["openai"];
        // Provider-level fields come from project
        assert_eq!(openai.base_url, "https://custom.proxy/v1");
        assert_eq!(openai.api_key_env, "CUSTOM_KEY");
        // Both models present
        assert!(openai.models.contains_key("gpt-4o"), "global model preserved");
        assert!(openai.models.contains_key("gpt-4o-mini"), "project model added");
    }

    #[test]
    fn merge_project_model_overrides_global_model_same_id() {
        let mut global = Config::default();
        let mut global_models = HashMap::new();
        global_models.insert(
            "gpt-4o".into(),
            ModelConfig {
                id: "gpt-4o".into(),
                name: "Global Name".into(),
                context_window: 128_000,
                max_output_tokens: None,
                cost: None,
                capabilities: ModelCapabilities::default(),
            },
        );
        global.providers.insert(
            "openai".into(),
            ProviderConfig {
                base_url: "https://api.openai.com/v1".into(),
                api_key_env: "OPENAI_API_KEY".into(),
                models: global_models,
            },
        );

        let mut project = Config::default();
        let mut project_models = HashMap::new();
        project_models.insert(
            "gpt-4o".into(),
            ModelConfig {
                id: "gpt-4o".into(),
                name: "Project Override".into(),
                context_window: 64_000,
                max_output_tokens: Some(4096),
                cost: None,
                capabilities: ModelCapabilities::default(),
            },
        );
        project.providers.insert(
            "openai".into(),
            ProviderConfig {
                base_url: "https://api.openai.com/v1".into(),
                api_key_env: "OPENAI_API_KEY".into(),
                models: project_models,
            },
        );

        let merged = global.merge(project);
        let model = &merged.providers["openai"].models["gpt-4o"];
        assert_eq!(model.name, "Project Override");
        assert_eq!(model.context_window, 64_000);
        assert_eq!(model.max_output_tokens, Some(4096));
    }

    #[test]
    fn default_theme_is_auto() {
        let config: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(config.theme, crate::ui::terminal_detect::ThemePreference::Auto);
    }

    #[test]
    fn theme_dark_light_parse() {
        let config: Config = serde_json::from_str(r#"{"theme": "dark"}"#).unwrap();
        assert_eq!(config.theme, crate::ui::terminal_detect::ThemePreference::Dark);

        let config: Config = serde_json::from_str(r#"{"theme": "light"}"#).unwrap();
        assert_eq!(config.theme, crate::ui::terminal_detect::ThemePreference::Light);
    }

    #[test]
    fn merge_preserves_global_theme_when_project_empty() {
        let mut global = Config::default();
        global.theme = crate::ui::terminal_detect::ThemePreference::Dark;
        let project = Config::default();
        let merged = global.merge(project);
        // Project has Auto (default) theme, so global Dark should be preserved
        assert_eq!(merged.theme, crate::ui::terminal_detect::ThemePreference::Dark);
    }

    #[test]
    fn merge_theme_only_project_overrides_global() {
        // A project config with only "theme": "dark" and no providers/model
        // should still override the global theme
        let global = Config {
            theme: crate::ui::terminal_detect::ThemePreference::Auto,
            ..Default::default()
        };
        let mut project = Config::default();
        project.theme = crate::ui::terminal_detect::ThemePreference::Dark;
        let merged = global.merge(project);
        assert_eq!(merged.theme, crate::ui::terminal_detect::ThemePreference::Dark);
    }

    #[test]
    fn merge_both_empty_returns_default() {
        let merged = Config::default().merge(Config::default());
        assert_eq!(merged.model, None);
        assert!(merged.providers.is_empty());
        // Note: Config::default() gives auto_compact=false (bool default),
        // while serde deserialization gives auto_compact=true via default_auto_compact().
        // Merging two defaults preserves the global's value (false).
        assert!(!merged.auto_compact);
    }

    #[test]
    fn model_cost_accepts_short_field_names() {
        let cost: ModelCost = serde_json::from_str(r#"{"input": 3.0, "output": 15.0}"#).unwrap();
        assert!((cost.input_per_million - 3.0).abs() < f64::EPSILON);
        assert!((cost.output_per_million - 15.0).abs() < f64::EPSILON);
    }

    #[test]
    fn model_cost_accepts_full_field_names() {
        let cost: ModelCost =
            serde_json::from_str(r#"{"input_per_million": 3.0, "output_per_million": 15.0}"#)
                .unwrap();
        assert!((cost.input_per_million - 3.0).abs() < f64::EPSILON);
        assert!((cost.output_per_million - 15.0).abs() < f64::EPSILON);
    }
}
