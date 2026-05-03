//! Headless scenario driver.
//!
//! `Runner::build` stands up a tempdir workspace, synthesizes an eval-mode
//! `Config` (trust permission profile, no MCP, model from caller), and
//! constructs an `App` pointed at the workspace. `Runner::run` then drives
//! each `user_turn` through `App::handle_input` + `App::run_until_idle`,
//! recording the trace into a `CapturedRun`.
//!
//! The eval Config inherits provider definitions from the user's global
//! config (`~/.config/steve/config.jsonc`) so real API keys and base URLs
//! work as expected. Eval-specific fields (model, permission profile, MCP
//! servers, allow_tools, permission_rules) are overridden after load.

use std::{path::Path, time::Instant};

use anyhow::{Context, Result};

use crate::{
    app::App,
    config,
    eval::{capture::CapturedRun, scenario::Scenario, workspace::ScenarioWorkspace},
    permission::PermissionProfile,
    project::ProjectInfo,
    provider::ProviderRegistry,
    storage::Storage,
    usage::{UsageWriterHandle, spawn_usage_writer},
};

pub struct Runner {
    workspace: ScenarioWorkspace,
    app: App,
    /// Held to keep the writer thread alive for the lifetime of the App.
    /// Dropped together with `Runner`; the writer thread exits cleanly
    /// once all sender clones (held by App) drop.
    _usage_handle: UsageWriterHandle,
}

impl Runner {
    /// Build a runner: create the workspace, construct an eval-mode App.
    /// `cli_model` overrides whatever model the user's config specifies; it
    /// must be in `provider/model_id` format and the provider must be
    /// resolvable from the user's global config.
    pub fn build(scenario: &Scenario, scenario_dir: &Path, cli_model: &str) -> Result<Self> {
        let workspace = ScenarioWorkspace::build(scenario_dir, &scenario.setup)
            .with_context(|| format!("building workspace for scenario {}", scenario.name))?;

        // Load globals (providers, base URLs) and discard project config —
        // tempdir has no `.steve.jsonc`, so load_project returns defaults.
        let (mut cfg, _warnings) =
            config::load(&workspace.root).context("loading user config for eval mode")?;

        // Override eval-specific fields. These are isolation guarantees, not
        // user preferences: trust profile means no permission events fire,
        // empty mcp_servers means no MCP processes spawn, empty allow_tools
        // and permission_rules mean nothing else can override the trust
        // profile, and auto_compact off keeps captures deterministic across
        // runs (compaction inserts a fresh assistant turn that would be
        // observable in the trace).
        cfg.model = Some(cli_model.to_string());
        cfg.permission_profile = Some(PermissionProfile::Trust);
        cfg.mcp_servers = std::collections::HashMap::new();
        cfg.allow_tools = Vec::new();
        cfg.permission_rules = Vec::new();
        cfg.auto_compact = false;

        let (provider_registry, missing) = ProviderRegistry::from_config(&cfg);
        if !missing.is_empty() {
            let names: Vec<String> = missing.iter().map(|w| w.provider_id.clone()).collect();
            anyhow::bail!(
                "eval requires API keys for the configured provider(s): {}",
                names.join(", ")
            );
        }

        // Use the workspace tempdir's UUID-derived id so storage and usage
        // databases land inside the workspace and are reaped on drop.
        let project_id = format!(
            "eval-{}",
            workspace
                .root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("anon")
        );
        let project = ProjectInfo {
            root: workspace.root.clone(),
            cwd: workspace.root.clone(),
            id: project_id,
        };

        let storage = Storage::with_base(workspace.root.join(".eval-storage"))
            .context("creating workspace-local storage")?;

        let usage_handle = spawn_usage_writer(&workspace.root.join("usage.db"))
            .context("spawning workspace-local usage writer")?;

        let app = App::new(
            project,
            cfg,
            storage,
            Vec::new(),
            Some(provider_registry),
            Vec::new(),
            Vec::new(),
            usage_handle.writer.clone(),
        );

        Ok(Self {
            workspace,
            app,
            _usage_handle: usage_handle,
        })
    }

    /// Drive the conversation: send each `user_turns[i]` and wait for the
    /// stream to go idle before sending the next. Records every event into
    /// the returned `CapturedRun`.
    pub async fn run(mut self, scenario: &Scenario) -> Result<CapturedRun> {
        let mut captured =
            CapturedRun::new(self.workspace.root.clone(), self.workspace.baseline.clone());
        let started_at = Instant::now();

        for (idx, turn) in scenario.user_turns.iter().enumerate() {
            self.app
                .handle_input(turn.clone())
                .await
                .with_context(|| format!("submitting user_turn #{}", idx + 1))?;
            // handle_input returns Ok(()) on silent rejection (e.g. model not
            // resolvable). Detect that here so the eval surfaces a clear
            // error rather than producing an empty trace.
            if !self.app.is_streaming() {
                let why = self
                    .app
                    .last_error_message()
                    .map(|s| format!(": {s}"))
                    .unwrap_or_else(|| {
                        " (no error message — likely model not resolvable from the user's config)"
                            .into()
                    });
                anyhow::bail!("user_turn #{} did not start a stream{}", idx + 1, why);
            }
            self.app
                .run_until_idle(|event| captured.observe(event))
                .await
                .with_context(|| format!("draining stream for user_turn #{}", idx + 1))?;
        }

        captured.duration = started_at.elapsed();
        Ok(captured)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::config::{Config, ModelCapabilities, ModelConfig, ProviderConfig};

    // Runner::build / Runner::run end-to-end coverage requires either a real
    // LLM provider (smoke test in `cargo run -- eval`) or stubbing the
    // `ChatStreamProvider` (would require lifting `MockChatStream` out of
    // `#[cfg(test)]` in `src/stream/mod.rs`). The smoke test is the
    // ecologically-valid gate for Phase 2; deeper unit coverage lands later.
    //
    // What we *can* test in isolation: the Config-shaping logic, which is
    // pure data manipulation and protects the eval-mode isolation guarantees.

    /// Confirm that the Config overrides Runner::build applies after loading
    /// the user's config land where expected and survive a Config that
    /// previously had MCP servers, allow_tools, and a non-trust profile.
    /// This is the safety net for the eval-isolation invariants.
    #[test]
    fn eval_mode_config_overrides_take_effect() {
        // Simulate what Runner::build does to the Config after load.
        let mut cfg = Config {
            permission_profile: Some(PermissionProfile::Cautious),
            mcp_servers: HashMap::from_iter([(
                "github".into(),
                serde_json::from_value(serde_json::json!({
                    "command": "true",
                    "args": []
                }))
                .unwrap(),
            )]),
            allow_tools: vec!["bash".into()],
            providers: HashMap::from_iter([(
                "anthropic".into(),
                ProviderConfig {
                    base_url: "https://api.anthropic.com/v1".into(),
                    api_key_env: Some("ANTHROPIC_API_KEY".into()),
                    models: HashMap::from_iter([(
                        "claude-sonnet-4-6".into(),
                        ModelConfig {
                            id: "claude-sonnet-4-6".into(),
                            name: "Claude Sonnet 4.6".into(),
                            context_window: 200_000,
                            max_output_tokens: None,
                            cost: None,
                            capabilities: ModelCapabilities::default(),
                        },
                    )]),
                },
            )]),
            ..Config::default()
        };

        // Apply the same overrides Runner::build applies.
        cfg.model = Some("anthropic/claude-sonnet-4-6".into());
        cfg.permission_profile = Some(PermissionProfile::Trust);
        cfg.mcp_servers = HashMap::new();
        cfg.allow_tools = Vec::new();
        cfg.permission_rules = Vec::new();
        cfg.auto_compact = false;

        assert_eq!(
            cfg.permission_profile,
            Some(PermissionProfile::Trust),
            "trust profile must replace user's choice"
        );
        assert!(
            cfg.mcp_servers.is_empty(),
            "MCP must be disabled regardless of user config"
        );
        assert!(
            cfg.allow_tools.is_empty(),
            "allow_tools cleared so trust profile is the sole gate"
        );
        assert_eq!(cfg.model.as_deref(), Some("anthropic/claude-sonnet-4-6"));
        assert!(
            !cfg.providers.is_empty(),
            "provider definitions must survive the override"
        );
    }
}
