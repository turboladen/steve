pub mod agents;
pub mod persist;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub use agents::{AgentsFile, load_agents_md, load_agents_md_chain};
pub use persist::persist_allow_tool;

use crate::ui::terminal_detect::ThemePreference;

// ---------------------------------------------------------------------------
// Types (inlined from former types.rs)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

/// Load configuration with global + project merge.
/// Global config at `~/.config/steve/config.jsonc` (XDG-style on all platforms) provides defaults;
/// project-level `.steve.jsonc` overlays on top.
///
/// Returns `(Config, Vec<String>)` — the merged config and any non-fatal warnings
/// (e.g., parse errors from config files that exist but couldn't be loaded).
pub fn load(project_root: &Path) -> Result<(Config, Vec<String>)> {
    let mut warnings = Vec::new();
    let global = load_global(&mut warnings);
    let project = load_project(project_root, &mut warnings)?;

    Ok((global.merge(project), warnings))
}

/// Load global config from `~/.config/steve/config.jsonc`.
/// Returns `Config::default()` if no global config exists.
/// Pushes a warning if the file exists but can't be parsed.
fn load_global(warnings: &mut Vec<String>) -> Config {
    let Some(path) = global_config_path() else {
        tracing::debug!(
            dir = ?global_config_dir(),
            "no global config.jsonc found"
        );
        return Config::default();
    };
    tracing::info!(path = %path.display(), "loading global config");
    match load_jsonc_file(&path) {
        Ok(config) => config,
        Err(e) => {
            let msg = format_config_error(&path, &e);
            tracing::warn!(path = %path.display(), error = ?e, "failed to load global config, using defaults");
            warnings.push(msg);
            Config::default()
        }
    }
}

/// Load project-level config from `.steve.jsonc` in the project root.
/// Pushes a warning if the file exists but can't be parsed.
fn load_project(project_root: &Path, warnings: &mut Vec<String>) -> Result<Config> {
    let path = project_root.join(".steve.jsonc");
    if path.exists() {
        match load_jsonc_file(&path) {
            Ok(config) => Ok(config),
            Err(e) => {
                let msg = format_config_error(&path, &e);
                warnings.push(msg);
                Ok(Config::default())
            }
        }
    } else {
        Ok(Config::default())
    }
}

/// Format a config error into a user-friendly message showing the file path
/// and the root cause (e.g., the specific serde field error).
fn format_config_error(path: &Path, error: &anyhow::Error) -> String {
    // Walk the error chain to find the most specific cause
    let root_cause = error.chain().last().unwrap_or(error.as_ref());
    format!("Config error in {}: {root_cause}", path.display())
}

/// Parse a JSONC file into a Config. Works for both `.json` and `.jsonc`.
fn load_jsonc_file(path: &Path) -> Result<Config> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;

    let json_value = jsonc_parser::parse_to_serde_value(&content, &Default::default())
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;

    match json_value {
        Some(value) => serde_json::from_value(value)
            .with_context(|| format!("failed to deserialize config from {}", path.display())),
        None => Ok(Config::default()),
    }
}

/// Returns the path to the global `config.jsonc`, if it exists.
fn global_config_path() -> Option<PathBuf> {
    find_global_config_in(None)
}

/// Find the global `config.jsonc` file. Accepts an optional override directory
/// for testing; when `None`, uses `~/.config/steve/`.
fn find_global_config_in(dir_override: Option<&Path>) -> Option<PathBuf> {
    let config_dir = match dir_override {
        Some(d) => d.to_path_buf(),
        None => global_config_dir()?,
    };
    let path = config_dir.join("config.jsonc");
    if path.exists() { Some(path) } else { None }
}

/// Returns `~/.config/steve/` — the global config directory.
/// Uses `$HOME/.config/steve/` directly (XDG-style) on all platforms.
pub fn global_config_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".config").join("steve"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Type tests (from former types.rs) --

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
        let model: ModelConfig = serde_json::from_str(r#"{"id": "test", "name": "Test"}"#).unwrap();
        assert_eq!(model.context_window, 128_000);
    }

    #[test]
    fn custom_context_window() {
        let model: ModelConfig =
            serde_json::from_str(r#"{"id": "test", "name": "Test", "context_window": 32000}"#)
                .unwrap();
        assert_eq!(model.context_window, 32_000);
    }

    #[test]
    fn capabilities_default_to_false() {
        let model: ModelConfig = serde_json::from_str(r#"{"id": "test", "name": "Test"}"#).unwrap();
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
        assert!(
            openai.models.contains_key("gpt-4o"),
            "global model preserved"
        );
        assert!(
            openai.models.contains_key("gpt-4o-mini"),
            "project model added"
        );
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
        assert_eq!(
            config.theme,
            crate::ui::terminal_detect::ThemePreference::Auto
        );
    }

    #[test]
    fn theme_dark_light_parse() {
        let config: Config = serde_json::from_str(r#"{"theme": "dark"}"#).unwrap();
        assert_eq!(
            config.theme,
            crate::ui::terminal_detect::ThemePreference::Dark
        );

        let config: Config = serde_json::from_str(r#"{"theme": "light"}"#).unwrap();
        assert_eq!(
            config.theme,
            crate::ui::terminal_detect::ThemePreference::Light
        );
    }

    #[test]
    fn merge_preserves_global_theme_when_project_empty() {
        let mut global = Config::default();
        global.theme = crate::ui::terminal_detect::ThemePreference::Dark;
        let project = Config::default();
        let merged = global.merge(project);
        // Project has Auto (default) theme, so global Dark should be preserved
        assert_eq!(
            merged.theme,
            crate::ui::terminal_detect::ThemePreference::Dark
        );
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
        assert_eq!(
            merged.theme,
            crate::ui::terminal_detect::ThemePreference::Dark
        );
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

    // -- Config loading tests --

    #[test]
    fn load_dotfile_jsonc() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".steve.jsonc"),
            r#"{"model": "openai/gpt-4o", "providers": {}}"#,
        )
        .unwrap();
        let (config, _warnings) = load(dir.path()).unwrap();
        assert_eq!(config.model, Some("openai/gpt-4o".into()));
    }

    #[test]
    fn load_dotfile_jsonc_with_comments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".steve.jsonc"),
            "{\n  // this is a comment\n  \"model\": \"openai/gpt-4o\",\n  \"providers\": {}\n}",
        )
        .unwrap();
        let (config, _warnings) = load(dir.path()).unwrap();
        assert_eq!(config.model, Some("openai/gpt-4o".into()));
    }

    #[test]
    fn no_config_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let (config, _warnings) = load(dir.path()).unwrap();
        let default = Config::default();
        assert_eq!(config.model, default.model);
        assert_eq!(config.auto_compact, default.auto_compact);
    }

    #[test]
    fn invalid_json_returns_warning() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".steve.jsonc"), "{{invalid").unwrap();
        let (config, warnings) = load(dir.path()).unwrap();
        assert!(!warnings.is_empty(), "should have a config warning");
        assert!(
            warnings[0].contains(".steve.jsonc"),
            "warning mentions the file"
        );
        assert_eq!(config.model, None, "falls back to default config");
    }

    #[test]
    fn partial_config_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".steve.jsonc"), r#"{"model": "test/m"}"#).unwrap();
        let (config, _warnings) = load(dir.path()).unwrap();
        assert_eq!(config.model, Some("test/m".into()));
        assert!(config.auto_compact);
    }

    #[test]
    fn load_jsonc_file_parses_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.json");
        std::fs::write(&path, r#"{"model": "test/m"}"#).unwrap();
        let config = load_jsonc_file(&path).unwrap();
        assert_eq!(config.model, Some("test/m".into()));
    }

    #[test]
    fn load_jsonc_file_parses_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonc");
        std::fs::write(&path, "{\n  // comment\n  \"model\": \"test/m\"\n}").unwrap();
        let config = load_jsonc_file(&path).unwrap();
        assert_eq!(config.model, Some("test/m".into()));
    }

    #[test]
    fn load_jsonc_file_missing_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        assert!(load_jsonc_file(&path).is_err());
    }

    #[test]
    fn load_project_no_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let mut warnings = Vec::new();
        let config = load_project(dir.path(), &mut warnings).unwrap();
        assert_eq!(config.model, None);
        assert!(config.providers.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn global_config_dir_returns_some() {
        // On any platform with a home directory, this should return Some
        let dir = global_config_dir();
        assert!(dir.is_some(), "should resolve a config directory");
    }

    #[test]
    fn global_config_finds_config_jsonc() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.jsonc"),
            r#"{"model": "openai/gpt-4o", "providers": {}}"#,
        )
        .unwrap();

        let found = find_global_config_in(Some(dir.path()));
        assert!(found.is_some(), "should find config.jsonc");
        assert!(found.unwrap().ends_with("config.jsonc"));
    }

    #[test]
    fn global_config_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let found = find_global_config_in(Some(dir.path()));
        assert!(
            found.is_none(),
            "should return None when no config file exists"
        );
    }

    #[test]
    fn global_config_ignores_old_config_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"model": "openai/gpt-4o"}"#,
        )
        .unwrap();
        let found = find_global_config_in(Some(dir.path()));
        assert!(found.is_none(), "config.json (old name) should be ignored");
    }

    #[test]
    fn old_project_filenames_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("steve.json"),
            r#"{"model": "openai/gpt-4o"}"#,
        )
        .unwrap();
        let (config, _warnings) = load(dir.path()).unwrap();
        assert_eq!(config.model, None, "steve.json should not be loaded");

        std::fs::write(
            dir.path().join("steve.jsonc"),
            r#"{"model": "openai/gpt-4o"}"#,
        )
        .unwrap();
        let (config, _warnings) = load(dir.path()).unwrap();
        assert_eq!(config.model, None, "steve.jsonc should not be loaded");
    }

    #[test]
    fn global_merge_with_empty_project() {
        // Simulate: global config has providers, empty project config
        let dir = tempfile::tempdir().unwrap();
        let global_dir = tempfile::tempdir().unwrap();

        // Write global config
        std::fs::write(
            global_dir.path().join("config.jsonc"),
            r#"{"model": "openai/gpt-4o", "providers": {"openai": {"base_url": "https://api.openai.com/v1", "api_key_env": "OPENAI_API_KEY", "models": {}}}}"#,
        ).unwrap();

        // Write empty project config
        std::fs::write(dir.path().join(".steve.jsonc"), "{}").unwrap();

        // Load and verify global providers are preserved
        let global = load_jsonc_file(&global_dir.path().join("config.jsonc")).unwrap();
        let mut warnings = Vec::new();
        let project = load_project(dir.path(), &mut warnings).unwrap();
        let merged = global.merge(project);

        assert_eq!(merged.model, Some("openai/gpt-4o".into()));
        assert!(
            merged.providers.contains_key("openai"),
            "global providers preserved"
        );
    }
}
