//! Diagnostics / health dashboard — stateless, compute-on-demand checks.

pub mod checks;
pub mod types;

pub use types::{Category, DiagnosticCheck, DiagnosticSummary, Severity};

use crate::config::types::Config;

/// Bundle of inputs for running all diagnostic checks.
/// Borrows from existing App state — no cloning required.
pub struct DiagnosticInput<'a> {
    pub agents_md: Option<&'a str>,
    pub system_prompt_len: usize,
    pub config: &'a Config,
    pub lsp_servers: &'a [(&'a str, bool)],
    pub total_tokens: u64,
    pub exchange_count: usize,
    pub cache_hits: u32,
    pub cache_misses: u32,
    pub compaction_count: u32,
    pub session_cost: Option<f64>,
    pub mcp_configured: &'a [&'a str],
    pub mcp_connected: &'a [(&'a str, usize, usize)],
}

/// Run all diagnostic checks and return the combined results.
pub fn run_diagnostics(input: &DiagnosticInput) -> Vec<DiagnosticCheck> {
    let mut results = Vec::new();

    results.extend(checks::ai_environment_checks(
        input.agents_md,
        input.system_prompt_len,
        input.config,
    ));

    results.extend(checks::lsp_health_checks(input.lsp_servers));

    results.extend(checks::mcp_health_checks(input.mcp_configured, input.mcp_connected));

    results.extend(checks::session_efficiency_checks(
        input.total_tokens,
        input.exchange_count,
        input.cache_hits,
        input.cache_misses,
        input.compaction_count,
        input.session_cost,
    ));

    results
}

/// Count checks by severity for the sidebar indicator.
pub fn summarize(checks: &[DiagnosticCheck]) -> DiagnosticSummary {
    let mut summary = DiagnosticSummary::default();
    for check in checks {
        match check.severity {
            Severity::Error => summary.error_count += 1,
            Severity::Warning => summary.warning_count += 1,
            Severity::Info => summary.info_count += 1,
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::Config;

    #[test]
    fn run_diagnostics_returns_checks_from_all_categories() {
        // Use a config that triggers some checks (default has no costs, no small_model)
        let config = Config::default();
        let lsp = [("rust-analyzer", false)];
        let input = DiagnosticInput {
            agents_md: None, // triggers AI env warning
            system_prompt_len: 1000,
            config: &config,
            lsp_servers: &lsp, // triggers LSP warning
            total_tokens: 100_000,
            exchange_count: 3,
            cache_hits: 0,
            cache_misses: 0,
            compaction_count: 0,
            session_cost: None,
            mcp_configured: &["github"],
            mcp_connected: &[],
        };

        let checks = run_diagnostics(&input);
        let categories: Vec<Category> = checks.iter().map(|c| c.category).collect();
        assert!(categories.contains(&Category::AiEnvironment));
        assert!(categories.contains(&Category::LspHealth));
        assert!(categories.contains(&Category::McpHealth));
        assert!(categories.contains(&Category::SessionEfficiency));
    }

    #[test]
    fn summarize_counts_correctly() {
        let checks = vec![
            DiagnosticCheck {
                severity: Severity::Error,
                category: Category::LspHealth,
                label: "error".into(),
                detail: "".into(),
                recommendation: None,
            },
            DiagnosticCheck {
                severity: Severity::Warning,
                category: Category::AiEnvironment,
                label: "warn1".into(),
                detail: "".into(),
                recommendation: None,
            },
            DiagnosticCheck {
                severity: Severity::Warning,
                category: Category::AiEnvironment,
                label: "warn2".into(),
                detail: "".into(),
                recommendation: None,
            },
            DiagnosticCheck {
                severity: Severity::Info,
                category: Category::SessionEfficiency,
                label: "info".into(),
                detail: "".into(),
                recommendation: None,
            },
        ];

        let summary = summarize(&checks);
        assert_eq!(summary.error_count, 1);
        assert_eq!(summary.warning_count, 2);
        assert_eq!(summary.info_count, 1);
    }

    #[test]
    fn summarize_empty_checks() {
        let summary = summarize(&[]);
        assert_eq!(summary.error_count, 0);
        assert_eq!(summary.warning_count, 0);
        assert_eq!(summary.info_count, 0);
    }

    #[test]
    fn run_diagnostics_minimal_returns_some_checks() {
        // Even with minimal input, default config triggers "no costs" and "no small_model"
        let config = Config::default();
        let input = DiagnosticInput {
            agents_md: Some("# AGENTS.md\n"),
            system_prompt_len: 1000,
            config: &config,
            lsp_servers: &[],
            total_tokens: 10_000,
            exchange_count: 5,
            cache_hits: 5,
            cache_misses: 5,
            compaction_count: 0,
            session_cost: None,
            mcp_configured: &[],
            mcp_connected: &[],
        };
        let checks = run_diagnostics(&input);
        assert!(!checks.is_empty());
    }
}
