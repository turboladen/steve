//! Pure diagnostic check functions — each takes typed inputs and returns findings.

use crate::config::types::Config;

use super::types::{Category, DiagnosticCheck, Severity};

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
    let has_costs = config.providers.values().any(|p| {
        p.models.values().any(|m| m.cost.is_some())
    });
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
pub fn lsp_health_checks(servers: &[(&str, bool)]) -> Vec<DiagnosticCheck> {
    let mut checks = Vec::new();

    if servers.is_empty() {
        // No languages detected — nothing to report
        return checks;
    }

    let running_count = servers.iter().filter(|(_, running)| *running).count();

    if running_count == 0 {
        checks.push(DiagnosticCheck {
            severity: Severity::Error,
            category: Category::LspHealth,
            label: "No LSP servers running".into(),
            detail: "Code intelligence unavailable".into(),
            recommendation: Some("Install language servers or check PATH".into()),
        });
    }

    for (name, running) in servers {
        if !running {
            checks.push(DiagnosticCheck {
                severity: Severity::Warning,
                category: Category::LspHealth,
                label: format!("{name} server not running"),
                detail: format!("{name} detected in project but server not available"),
                recommendation: Some(format!("Install {name} language server or check PATH")),
            });
        }
    }

    checks
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
    if let Some(cost) = session_cost {
        if cost > 0.0 {
            let cost_per = cost / exchange_count as f64;
            checks.push(DiagnosticCheck {
                severity: Severity::Info,
                category: Category::SessionEfficiency,
                label: format!("${cost_per:.4}/exchange ({exchange_count} exchanges)"),
                detail: format!("Session total: ${cost:.4}"),
                recommendation: None,
            });
        }
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
    use crate::config::types::Config;

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
        assert!(checks
            .iter()
            .any(|c| c.label.contains("System prompt large")));
    }

    #[test]
    fn small_system_prompt_no_check() {
        let config = Config::default();
        let checks = ai_environment_checks(Some("short"), 5000, &config);
        assert!(!checks
            .iter()
            .any(|c| c.label.contains("System prompt")));
    }

    #[test]
    fn no_costs_configured_info() {
        let config = Config::default();
        let checks = ai_environment_checks(Some("ok"), 1000, &config);
        assert!(checks
            .iter()
            .any(|c| c.label.contains("No model costs")));
    }

    #[test]
    fn no_small_model_info() {
        let config = Config::default();
        let checks = ai_environment_checks(Some("ok"), 1000, &config);
        assert!(checks
            .iter()
            .any(|c| c.label.contains("No small_model")));
    }

    #[test]
    fn small_model_configured_no_check() {
        let mut config = Config::default();
        config.small_model = Some("openai/gpt-4o-mini".into());
        let checks = ai_environment_checks(Some("ok"), 1000, &config);
        assert!(!checks
            .iter()
            .any(|c| c.label.contains("No small_model")));
    }

    // -- lsp_health_checks tests --

    #[test]
    fn lsp_all_running_no_warnings() {
        let servers = vec![
            ("rust-analyzer", true),
            ("ty", true),
        ];
        let checks = lsp_health_checks(&servers);
        assert!(checks.is_empty());
    }

    #[test]
    fn lsp_detected_not_running_warns() {
        let servers = vec![
            ("rust-analyzer", true),
            ("ty", false),
        ];
        let checks = lsp_health_checks(&servers);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].label.contains("ty"));
        assert_eq!(checks[0].severity, Severity::Warning);
    }

    #[test]
    fn lsp_zero_running_error() {
        let servers = vec![
            ("rust-analyzer", false),
            ("ty", false),
        ];
        let checks = lsp_health_checks(&servers);
        // Should have 1 error (no servers) + 2 warnings (per-server)
        assert!(checks.iter().any(|c| c.severity == Severity::Error));
        assert!(checks
            .iter()
            .any(|c| c.label.contains("No LSP servers running")));
    }

    #[test]
    fn lsp_no_languages_detected_no_checks() {
        let checks = lsp_health_checks(&[]);
        assert!(checks.is_empty());
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
}
