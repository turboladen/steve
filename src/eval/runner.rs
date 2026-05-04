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

use std::{
    path::Path,
    time::{Duration, Instant},
};

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

/// Hard cap per user turn. A wedged stream (LLM stuck in a loop, network
/// stall, etc.) without this would hang the eval indefinitely. 5 minutes
/// is conservative for a single Sonnet/Haiku turn with tool calls.
const PER_TURN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

pub struct Runner {
    workspace: ScenarioWorkspace,
    app: App,
    /// Held to keep the writer thread alive for the lifetime of the App.
    /// Dropped together with `Runner`; the writer thread exits cleanly
    /// once all sender clones (held by App) drop.
    _usage_handle: UsageWriterHandle,
    /// Holds the eval-infrastructure tempdir (storage + usage db) OUTSIDE
    /// the scenario workspace so those files don't pollute the workspace's
    /// baseline snapshot or show up in agent `grep`/`list` traces.
    /// `Drop` cleans up the tempdir.
    _infra_tmp: tempfile::TempDir,
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

        // Storage and usage DB live in a SEPARATE tempdir, NOT inside the
        // scenario workspace. Putting them in the workspace would (a)
        // contaminate the baseline snapshot if added before snapshot, (b)
        // make agent `grep`/`list` traces surface eval-infra files as if
        // they were workspace content, and (c) silently fail any future
        // file_unchanged assertion against `.eval-storage/**`.
        let infra_tmp = tempfile::tempdir().context("creating eval-infra tempdir")?;
        let storage = Storage::with_base(infra_tmp.path().join("storage"))
            .context("creating eval-infra storage")?;
        let usage_handle = spawn_usage_writer(&infra_tmp.path().join("usage.db"))
            .context("spawning eval-infra usage writer")?;

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
            _infra_tmp: infra_tmp,
        })
    }

    /// Drive the conversation: send each `user_turns[i]` and wait for the
    /// stream to go idle before sending the next. Records every event into
    /// the returned `CapturedRun`. A wedged turn (no progress within
    /// `PER_TURN_TIMEOUT`) sets `captured.timed_out = true` and stops the
    /// loop — subsequent turns are skipped because the stream task is in
    /// an unknown state.
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
            let drain = self.app.run_until_idle(|event| captured.observe(event));
            match tokio::time::timeout(PER_TURN_TIMEOUT, drain).await {
                Ok(result) => {
                    result.with_context(|| format!("draining stream for user_turn #{}", idx + 1))?
                }
                Err(_elapsed) => {
                    captured.timed_out = true;
                    // Don't try further turns — the stream task is wedged
                    // and the next handle_input would race with whatever
                    // it's stuck doing. The CLI verdict treats timed_out
                    // as a fail signal via completed_normally().
                    break;
                }
            }
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

    // End-to-end Runner coverage requires lifting MockChatStream out of
    // `#[cfg(test)]` in src/stream/mod.rs — deferred until Phase 4 (judge)
    // forces it. The smoke test (`cargo run -- eval`) is the ecologically-
    // valid gate for v1.

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
