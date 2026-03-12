//! Core types for the diagnostics / health dashboard system.

/// Severity level for a diagnostic check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// Category grouping for diagnostic checks (used for overlay section headers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    AiEnvironment,
    LspHealth,
    SessionEfficiency,
}

impl Category {
    /// Human-readable label for display in the overlay.
    pub fn label(self) -> &'static str {
        match self {
            Category::AiEnvironment => "AI Environment",
            Category::LspHealth => "LSP Health",
            Category::SessionEfficiency => "Session Efficiency",
        }
    }
}

/// A single diagnostic finding.
#[derive(Debug, Clone)]
pub struct DiagnosticCheck {
    pub severity: Severity,
    pub category: Category,
    pub label: String,
    pub detail: String,
    pub recommendation: Option<String>,
}

/// Aggregate counts by severity (for sidebar indicator).
#[derive(Debug, Clone, Default)]
pub struct DiagnosticSummary {
    pub error_count: usize,
    pub warning_count: usize,
    pub info_count: usize,
}

impl DiagnosticSummary {
    /// The highest severity present in the summary.
    pub fn max_severity(&self) -> Severity {
        if self.error_count > 0 {
            Severity::Error
        } else if self.warning_count > 0 {
            Severity::Warning
        } else {
            Severity::Info
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_ordering() {
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
    }

    #[test]
    fn category_labels() {
        assert_eq!(Category::AiEnvironment.label(), "AI Environment");
        assert_eq!(Category::LspHealth.label(), "LSP Health");
        assert_eq!(Category::SessionEfficiency.label(), "Session Efficiency");
    }

    #[test]
    fn summary_default_is_zero() {
        let s = DiagnosticSummary::default();
        assert_eq!(s.error_count, 0);
        assert_eq!(s.warning_count, 0);
        assert_eq!(s.info_count, 0);
    }

    #[test]
    fn summary_max_severity_empty() {
        let s = DiagnosticSummary::default();
        assert_eq!(s.max_severity(), Severity::Info);
    }

    #[test]
    fn summary_max_severity_warning() {
        let s = DiagnosticSummary {
            warning_count: 2,
            ..Default::default()
        };
        assert_eq!(s.max_severity(), Severity::Warning);
    }

    #[test]
    fn summary_max_severity_error_takes_precedence() {
        let s = DiagnosticSummary {
            error_count: 1,
            warning_count: 3,
            info_count: 5,
        };
        assert_eq!(s.max_severity(), Severity::Error);
    }
}
