//! Pure diagnostic check functions — each takes typed inputs and returns findings.

use crate::{
    config::Config,
    provider::{ProviderInitReason, ProviderInitWarning},
};

use super::types::{Category, DiagnosticCheck, Severity};

/// Surface per-provider `api_key_env` failures as AI-environment errors.
/// One check per affected provider so the overlay shows each env var by name,
/// with wording that reflects the specific failure mode ("not set" vs
/// "set but not valid UTF-8") — the remediation differs.
pub fn api_key_env_checks(missing: &[ProviderInitWarning]) -> Vec<DiagnosticCheck> {
    missing
        .iter()
        .map(|w| {
            let (label, detail, recommendation) = match w.reason {
                ProviderInitReason::MissingEnvVar => (
                    format!("Missing API key for provider '{}'", w.provider_id),
                    format!(
                        "${} is not set — provider '{}' is disabled",
                        w.env_var, w.provider_id,
                    ),
                    format!(
                        "Set ${} in your shell environment and restart steve",
                        w.env_var
                    ),
                ),
                ProviderInitReason::NonUtf8EnvVar => (
                    format!("Corrupt API key for provider '{}'", w.provider_id),
                    format!(
                        "${} is set but contains non-UTF-8 bytes — provider '{}' is disabled",
                        w.env_var, w.provider_id,
                    ),
                    format!(
                        "Re-export ${} with a valid UTF-8 value and restart steve",
                        w.env_var
                    ),
                ),
            };
            DiagnosticCheck {
                severity: Severity::Error,
                category: Category::AiEnvironment,
                label,
                detail,
                recommendation: Some(recommendation),
            }
        })
        .collect()
}

/// Static project-health checks for the AI environment.
pub fn ai_environment_checks(
    agents_md: Option<&str>,
    system_prompt_len: usize,
    config: &Config,
) -> Vec<DiagnosticCheck> {
    let mut checks = Vec::new();

    // AGENTS.md presence and size
    match agents_md {
        None => {
            checks.push(DiagnosticCheck {
                severity: Severity::Warning,
                category: Category::AiEnvironment,
                label: "No AGENTS.md".into(),
                detail: "Project has no AGENTS.md file for agent instructions".into(),
                recommendation: Some("Run /init to create one".into()),
            });
        }
        Some(content) => {
            let line_count = content.lines().count();
            if line_count > 100 {
                checks.push(DiagnosticCheck {
                    severity: Severity::Warning,
                    category: Category::AiEnvironment,
                    label: format!("AGENTS.md large ({line_count} lines)"),
                    detail: "Large AGENTS.md files reduce agent focus".into(),
                    recommendation: Some("Trim to <100 lines for better agent focus".into()),
                });
            }
        }
    }

    // System prompt size
    if system_prompt_len > 8000 {
        checks.push(DiagnosticCheck {
            severity: Severity::Info,
            category: Category::AiEnvironment,
            label: format!("System prompt large ({system_prompt_len} chars)"),
            detail: "Large system prompt reduces effective context".into(),
            recommendation: Some("Consider trimming AGENTS.md or config".into()),
        });
    }

    // No model costs configured
    let has_costs = config
        .providers
        .values()
        .any(|p| p.models.values().any(|m| m.cost.is_some()));
    if !has_costs {
        checks.push(DiagnosticCheck {
            severity: Severity::Info,
            category: Category::AiEnvironment,
            label: "No model costs configured".into(),
            detail: "Cost tracking unavailable without pricing config".into(),
            recommendation: Some("Add cost config to track spending".into()),
        });
    }

    // No small_model
    if config.small_model.is_none() {
        checks.push(DiagnosticCheck {
            severity: Severity::Info,
            category: Category::AiEnvironment,
            label: "No small_model configured".into(),
            detail: "Title generation and compaction use the main model".into(),
            recommendation: Some("Set small_model for faster title gen + compaction".into()),
        });
    }

    checks
}

/// LSP server health checks.
///
/// `servers` carries `(binary, running, error_reason)` per detected
/// language. `error_reason` is the `LspServerState::Error { reason }`
/// reason verbatim when present, otherwise `None`. The recommendation
/// branches on the reason so a server that crashed post-spawn (e.g.
/// `"initialize failed: ..."` or `"mainloop exited"`) is not blamed on
/// missing PATH — that has been a misleading recommendation source for
/// users with the binary correctly installed.
pub fn lsp_health_checks(servers: &[(&str, bool, Option<&str>)]) -> Vec<DiagnosticCheck> {
    let mut checks = Vec::new();

    if servers.is_empty() {
        // No languages detected — nothing to report
        return checks;
    }

    let running_count = servers.iter().filter(|(_, running, _)| *running).count();

    if running_count == 0 {
        checks.push(DiagnosticCheck {
            severity: Severity::Error,
            category: Category::LspHealth,
            label: "No LSP servers running".into(),
            detail: "Code intelligence unavailable".into(),
            recommendation: Some(
                "Check per-server warnings below for the specific failure mode".into(),
            ),
        });
    }

    for (name, running, reason) in servers {
        if !running {
            checks.push(DiagnosticCheck {
                severity: Severity::Warning,
                category: Category::LspHealth,
                label: format!("{name} server not running"),
                detail: lsp_unavailable_detail(name, *reason),
                recommendation: Some(lsp_recommendation_for_reason(name, *reason)),
            });
        }
    }

    checks
}

/// Heuristic: does this `LspServerState::Error` reason indicate the
/// binary itself wasn't found / failed to launch (in which case the
/// user genuinely needs to install it or fix `PATH`)?
fn reason_is_install_path_issue(reason: &str) -> bool {
    let r = reason.to_ascii_lowercase();
    // Phrases produced by start_server when resolve_server returns None
    // ("no … language server found on PATH") or when Command::spawn
    // itself fails with a "No such file or directory" or permission error.
    r.contains("not found on path")
        || r.contains("found on path")
        || r.contains("no such file")
        || r.contains("failed to spawn")
}

fn lsp_unavailable_detail(name: &str, reason: Option<&str>) -> String {
    match reason {
        None => format!("{name} detected in project but server not available"),
        Some(r) => format!("{name} unavailable: {r}"),
    }
}

fn lsp_recommendation_for_reason(name: &str, reason: Option<&str>) -> String {
    match reason {
        None => format!(
            "Install {name} language server or check PATH (status pending — \
             rerun if the server is still starting up)"
        ),
        Some(r) if reason_is_install_path_issue(r) => {
            format!("Install {name} language server or check PATH")
        }
        Some(r) => format!(
            "{name} crashed or failed to handshake — not a PATH issue. \
             Reason: {r}. Try `RUST_LOG=steve=debug` for transport-level \
             detail; consider re-installing or version-pinning {name}."
        ),
    }
}

/// Live session efficiency checks.
pub fn session_efficiency_checks(
    total_tokens: u64,
    exchange_count: usize,
    cache_hits: u32,
    cache_misses: u32,
    compaction_count: u32,
    session_cost: Option<f64>,
) -> Vec<DiagnosticCheck> {
    let mut checks = Vec::new();

    // Avoid division by zero for sessions that haven't had exchanges yet
    if exchange_count == 0 {
        return checks;
    }

    // Context burn rate
    let tokens_per_exchange = total_tokens / exchange_count as u64;
    if tokens_per_exchange > 20_000 {
        checks.push(DiagnosticCheck {
            severity: Severity::Warning,
            category: Category::SessionEfficiency,
            label: format!("High context burn ({tokens_per_exchange} tok/exchange)"),
            detail: "Consuming context quickly — may need compaction soon".into(),
            recommendation: Some("Consider more targeted prompts or /compact".into()),
        });
    }

    // Cache hit ratio
    let total_lookups = cache_hits + cache_misses;
    if total_lookups >= 10 {
        let hit_ratio = cache_hits as f64 / total_lookups as f64;
        if hit_ratio < 0.30 {
            let pct = (hit_ratio * 100.0) as u32;
            checks.push(DiagnosticCheck {
                severity: Severity::Info,
                category: Category::SessionEfficiency,
                label: format!("Low cache hit ratio ({pct}%)"),
                detail: "Agent re-reading files frequently".into(),
                recommendation: Some(
                    "May indicate unclear instructions or heavy exploration".into(),
                ),
            });
        }
    }

    // Frequent compaction
    if compaction_count > 3 {
        checks.push(DiagnosticCheck {
            severity: Severity::Warning,
            category: Category::SessionEfficiency,
            label: format!("Frequent compaction ({compaction_count} times)"),
            detail: "High compaction churn in this session".into(),
            recommendation: Some("Consider starting a /new session".into()),
        });
    }

    // Cost per exchange (informational)
    if let Some(cost) = session_cost
        && cost > 0.0
    {
        let cost_per = cost / exchange_count as f64;
        checks.push(DiagnosticCheck {
            severity: Severity::Info,
            category: Category::SessionEfficiency,
            label: format!("${cost_per:.4}/exchange ({exchange_count} exchanges)"),
            detail: format!("Session total: ${cost:.4}"),
            recommendation: None,
        });
    }

    checks
}

/// MCP server health checks.
/// `configured` = server IDs from config.
/// `connected` = (server_id, tool_count, resource_count, prompt_count) for running servers.
pub fn mcp_health_checks(
    configured: &[&str],
    connected: &[(&str, usize, usize, usize)],
) -> Vec<DiagnosticCheck> {
    let mut checks = Vec::new();

    if configured.is_empty() {
        return checks;
    }

    for &server_id in configured {
        match connected.iter().find(|(id, _, _, _)| *id == server_id) {
            None => {
                checks.push(DiagnosticCheck {
                    severity: Severity::Error,
                    category: Category::McpHealth,
                    label: format!("{server_id} not connected"),
                    detail: format!("MCP server '{server_id}' is configured but not running"),
                    recommendation: Some(log_path_recommendation(
                        "Check server command/URL, authentication, and logs",
                    )),
                });
            }
            Some((_, tool_count, _, _)) if *tool_count == 0 => {
                checks.push(DiagnosticCheck {
                    severity: Severity::Warning,
                    category: Category::McpHealth,
                    label: format!("{server_id} has no tools"),
                    detail: format!("MCP server '{server_id}' is connected but exposes 0 tools"),
                    recommendation: Some("Verify server configuration and capabilities".into()),
                });
            }
            Some(_) => {
                // Connected with tools — healthy, nothing to report
            }
        }
    }

    checks
}

/// Append the log directory path to a recommendation string for user guidance.
fn log_path_recommendation(base_msg: &str) -> String {
    match directories::ProjectDirs::from("", "", "steve") {
        Some(dirs) => format!("{base_msg} ({})", dirs.data_dir().join("logs").display()),
        None => base_msg.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    // -- ai_environment_checks tests --

    #[test]
    fn agents_md_absent_warns() {
        let config = Config::default();
        let checks = ai_environment_checks(None, 1000, &config);
        assert!(checks.iter().any(|c| c.label.contains("No AGENTS.md")));
        assert!(checks.iter().any(|c| c.severity == Severity::Warning));
    }

    #[test]
    fn agents_md_present_short_no_warning() {
        let config = Config::default();
        let content = "# AGENTS.md\n\nShort instructions.\n";
        let checks = ai_environment_checks(Some(content), 1000, &config);
        assert!(!checks.iter().any(|c| c.label.contains("AGENTS.md")));
    }

    #[test]
    fn agents_md_over_100_lines_warns() {
        let config = Config::default();
        let content = "line\n".repeat(150);
        let checks = ai_environment_checks(Some(&content), 1000, &config);
        assert!(checks.iter().any(|c| c.label.contains("150 lines")));
    }

    #[test]
    fn large_system_prompt_info() {
        let config = Config::default();
        let checks = ai_environment_checks(Some("short"), 10000, &config);
        assert!(
            checks
                .iter()
                .any(|c| c.label.contains("System prompt large"))
        );
    }

    #[test]
    fn small_system_prompt_no_check() {
        let config = Config::default();
        let checks = ai_environment_checks(Some("short"), 5000, &config);
        assert!(!checks.iter().any(|c| c.label.contains("System prompt")));
    }

    #[test]
    fn no_costs_configured_info() {
        let config = Config::default();
        let checks = ai_environment_checks(Some("ok"), 1000, &config);
        assert!(checks.iter().any(|c| c.label.contains("No model costs")));
    }

    #[test]
    fn no_small_model_info() {
        let config = Config::default();
        let checks = ai_environment_checks(Some("ok"), 1000, &config);
        assert!(checks.iter().any(|c| c.label.contains("No small_model")));
    }

    #[test]
    fn small_model_configured_no_check() {
        let config = Config {
            small_model: Some("openai/gpt-4o-mini".into()),
            ..Default::default()
        };
        let checks = ai_environment_checks(Some("ok"), 1000, &config);
        assert!(!checks.iter().any(|c| c.label.contains("No small_model")));
    }

    // -- lsp_health_checks tests --

    #[test]
    fn lsp_all_running_no_warnings() {
        let servers = vec![("rust-analyzer", true, None), ("ty", true, None)];
        let checks = lsp_health_checks(&servers);
        assert!(checks.is_empty());
    }

    #[test]
    fn lsp_detected_not_running_warns() {
        let servers = vec![("rust-analyzer", true, None), ("ty", false, None)];
        let checks = lsp_health_checks(&servers);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].label.contains("ty"));
        assert_eq!(checks[0].severity, Severity::Warning);
    }

    #[test]
    fn lsp_zero_running_error() {
        let servers = vec![("rust-analyzer", false, None), ("ty", false, None)];
        let checks = lsp_health_checks(&servers);
        // Should have 1 error (no servers) + 2 warnings (per-server)
        assert!(checks.iter().any(|c| c.severity == Severity::Error));
        assert!(
            checks
                .iter()
                .any(|c| c.label.contains("No LSP servers running"))
        );
    }

    #[test]
    fn lsp_no_languages_detected_no_checks() {
        let checks = lsp_health_checks(&[]);
        assert!(checks.is_empty());
    }

    #[test]
    fn lsp_recommendation_path_reason_suggests_install() {
        // Reason from start_server line 187: "no yaml language server
        // found on PATH" — install/PATH advice is correct here.
        let servers = vec![(
            "yaml-language-server",
            false,
            Some("no yaml language server found on PATH"),
        )];
        let checks = lsp_health_checks(&servers);
        let warning = checks
            .iter()
            .find(|c| c.label.contains("yaml-language-server"))
            .expect("warning for yaml-language-server");
        let rec = warning.recommendation.as_deref().unwrap_or("");
        assert!(
            rec.starts_with("Install yaml-language-server"),
            "PATH-failure should recommend Install/check PATH, got: {rec}"
        );
    }

    #[test]
    fn lsp_recommendation_initialize_failure_does_not_blame_path() {
        // Regression for steve-dbzv: the user has yaml-language-server
        // installed and on PATH; it's being spawned but crashing during
        // Initialize. The recommendation must NOT tell them to install
        // it or check PATH — that's the misleading message we're fixing.
        let servers = vec![(
            "yaml-language-server",
            false,
            Some("initialize failed: ResponseError { code: -32001 }"),
        )];
        let checks = lsp_health_checks(&servers);
        let warning = checks
            .iter()
            .find(|c| c.label.contains("yaml-language-server"))
            .expect("warning for yaml-language-server");
        let rec = warning.recommendation.as_deref().unwrap_or("");
        assert!(
            !rec.starts_with("Install"),
            "post-Initialize failure must NOT recommend Install, got: {rec}"
        );
        assert!(
            rec.contains("crashed or failed to handshake"),
            "should describe the actual failure mode, got: {rec}"
        );
        assert!(
            rec.contains("initialize failed"),
            "should surface the underlying reason verbatim, got: {rec}"
        );
        assert!(
            rec.contains("RUST_LOG=steve=debug"),
            "should point at the debug log for transport-level detail, got: {rec}"
        );
    }

    #[test]
    fn lsp_recommendation_mainloop_exit_does_not_blame_path() {
        // Same pattern, different reason variant — crash watcher writes
        // "mainloop exited" / "mainloop panicked: ..." for post-Initialize
        // crashes.
        let servers = vec![("typescript-language-server", false, Some("mainloop exited"))];
        let checks = lsp_health_checks(&servers);
        let warning = checks
            .iter()
            .find(|c| c.label.contains("typescript-language-server"))
            .expect("warning for typescript-language-server");
        let rec = warning.recommendation.as_deref().unwrap_or("");
        assert!(!rec.starts_with("Install"), "got: {rec}");
        assert!(rec.contains("mainloop exited"), "got: {rec}");
    }

    #[test]
    fn lsp_recommendation_failed_to_spawn_blames_path() {
        // start_server line 196: Command::spawn failure (binary on PATH
        // but not executable, or transient FS error). PATH advice IS
        // appropriate here — the user needs to fix the binary.
        let servers = vec![(
            "yaml-language-server",
            false,
            Some("failed to spawn yaml-language-server: permission denied"),
        )];
        let checks = lsp_health_checks(&servers);
        let warning = checks
            .iter()
            .find(|c| c.label.contains("yaml-language-server"))
            .expect("warning for yaml-language-server");
        let rec = warning.recommendation.as_deref().unwrap_or("");
        assert!(
            rec.starts_with("Install yaml-language-server"),
            "spawn-failure should recommend Install/check PATH, got: {rec}"
        );
    }

    #[test]
    fn lsp_detail_surfaces_reason_when_present() {
        // The `detail` field should also carry the reason so the
        // diagnostic panel displays it without the user having to expand
        // recommendations.
        let servers = vec![(
            "basedpyright-langserver",
            false,
            Some("mainloop panicked: JoinError"),
        )];
        let checks = lsp_health_checks(&servers);
        let warning = checks
            .iter()
            .find(|c| c.label.contains("basedpyright-langserver"))
            .expect("warning for basedpyright-langserver");
        assert!(
            warning.detail.contains("mainloop panicked"),
            "detail should surface the reason verbatim, got: {}",
            warning.detail
        );
    }

    // -- session_efficiency_checks tests --

    #[test]
    fn zero_exchanges_no_checks() {
        let checks = session_efficiency_checks(50000, 0, 10, 5, 0, Some(0.05));
        assert!(checks.is_empty());
    }

    #[test]
    fn high_burn_rate_warns() {
        let checks = session_efficiency_checks(100_000, 3, 0, 0, 0, None);
        assert!(checks.iter().any(|c| c.label.contains("High context burn")));
    }

    #[test]
    fn normal_burn_rate_no_warning() {
        let checks = session_efficiency_checks(30_000, 5, 0, 0, 0, None);
        assert!(!checks.iter().any(|c| c.label.contains("burn")));
    }

    #[test]
    fn low_cache_hit_ratio_info() {
        // 2 hits, 10 misses = 16.7% < 30%
        let checks = session_efficiency_checks(10_000, 5, 2, 10, 0, None);
        assert!(checks.iter().any(|c| c.label.contains("cache hit ratio")));
    }

    #[test]
    fn adequate_cache_ratio_no_check() {
        // 8 hits, 5 misses = 61.5% > 30%
        let checks = session_efficiency_checks(10_000, 5, 8, 5, 0, None);
        assert!(!checks.iter().any(|c| c.label.contains("cache")));
    }

    #[test]
    fn few_lookups_skips_cache_check() {
        // Under 10 total lookups — too early to judge
        let checks = session_efficiency_checks(10_000, 5, 1, 5, 0, None);
        assert!(!checks.iter().any(|c| c.label.contains("cache")));
    }

    #[test]
    fn frequent_compaction_warns() {
        let checks = session_efficiency_checks(10_000, 5, 0, 0, 4, None);
        assert!(checks.iter().any(|c| c.label.contains("compaction")));
    }

    #[test]
    fn normal_compaction_no_warning() {
        let checks = session_efficiency_checks(10_000, 5, 0, 0, 2, None);
        assert!(!checks.iter().any(|c| c.label.contains("compaction")));
    }

    #[test]
    fn cost_per_exchange_shown() {
        let checks = session_efficiency_checks(10_000, 4, 0, 0, 0, Some(0.08));
        assert!(checks.iter().any(|c| c.label.contains("$0.0200/exchange")));
    }

    #[test]
    fn no_cost_no_check() {
        let checks = session_efficiency_checks(10_000, 4, 0, 0, 0, None);
        assert!(!checks.iter().any(|c| c.label.contains("exchange")));
    }

    #[test]
    fn zero_cost_no_check() {
        let checks = session_efficiency_checks(10_000, 4, 0, 0, 0, Some(0.0));
        assert!(!checks.iter().any(|c| c.label.contains("exchange")));
    }

    // -- mcp_health_checks tests --

    #[test]
    fn mcp_no_servers_configured_no_checks() {
        let checks = mcp_health_checks(&[], &[]);
        assert!(checks.is_empty());
    }

    #[test]
    fn mcp_configured_but_not_connected() {
        let checks = mcp_health_checks(&["github"], &[]);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].severity, Severity::Error);
        assert!(checks[0].label.contains("github"));
        assert!(checks[0].label.contains("not connected"));
        assert_eq!(checks[0].category, Category::McpHealth);
    }

    #[test]
    fn mcp_all_connected_no_errors() {
        let configured = &["github", "slack"];
        let connected = &[("github", 5_usize, 2_usize, 0_usize), ("slack", 3, 0, 0)];
        let checks = mcp_health_checks(configured, connected);
        assert!(checks.is_empty());
    }

    #[test]
    fn mcp_connected_but_no_tools_warns() {
        let configured = &["github"];
        let connected = &[("github", 0_usize, 1_usize, 0_usize)];
        let checks = mcp_health_checks(configured, connected);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].severity, Severity::Warning);
        assert!(checks[0].label.contains("no tools"));
        assert_eq!(checks[0].category, Category::McpHealth);
    }

    // -- api_key_env_checks tests --

    #[test]
    fn api_key_env_checks_empty_input_produces_no_checks() {
        assert!(api_key_env_checks(&[]).is_empty());
    }

    #[test]
    fn api_key_env_checks_one_per_missing_provider() {
        let missing = vec![
            ProviderInitWarning {
                provider_id: "fireworks".to_string(),
                env_var: "FIREWORKS_API_KEY".to_string(),
                reason: ProviderInitReason::MissingEnvVar,
            },
            ProviderInitWarning {
                provider_id: "anthropic".to_string(),
                env_var: "ANTHROPIC_API_KEY".to_string(),
                reason: ProviderInitReason::MissingEnvVar,
            },
        ];

        let checks = api_key_env_checks(&missing);

        assert_eq!(checks.len(), 2);
        for check in &checks {
            assert_eq!(check.severity, Severity::Error);
            assert_eq!(check.category, Category::AiEnvironment);
        }

        let fireworks = checks
            .iter()
            .find(|c| c.label.contains("fireworks"))
            .expect("fireworks check present");
        assert!(
            fireworks.detail.contains("$FIREWORKS_API_KEY"),
            "detail should name the env var so user knows what to set: {}",
            fireworks.detail,
        );
        assert!(
            fireworks
                .recommendation
                .as_ref()
                .is_some_and(|r| r.contains("FIREWORKS_API_KEY")),
            "recommendation must reference the specific env var",
        );
    }

    #[test]
    fn api_key_env_checks_distinguishes_non_utf8_from_missing() {
        let missing = vec![ProviderInitWarning {
            provider_id: "fireworks".to_string(),
            env_var: "FIREWORKS_API_KEY".to_string(),
            reason: ProviderInitReason::NonUtf8EnvVar,
        }];

        let check = api_key_env_checks(&missing).pop().expect("one check");

        assert_eq!(check.severity, Severity::Error);
        assert!(
            check.label.contains("Corrupt"),
            "label must distinguish from plain 'missing': {}",
            check.label,
        );
        assert!(
            check.detail.contains("non-UTF-8"),
            "detail must tell the user the var IS set, just unreadable: {}",
            check.detail,
        );
        assert!(
            check
                .recommendation
                .as_ref()
                .is_some_and(|r| r.contains("Re-export")),
            "recommendation must not just say 'set the var' — it's already set",
        );
    }
}
