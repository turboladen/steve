use chrono::{DateTime, Utc};

// ── Write-side types (used by UsageWriter) ──────────────────

/// A single API call's usage data, recorded per-call for analytics.
pub struct ApiCallRecord {
    pub timestamp: DateTime<Utc>,
    pub project_id: String,
    pub session_id: String,
    pub model_ref: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    /// NULL if no ModelCost configured for the model.
    pub cost: Option<f64>,
    /// Wall-clock time for the API call in milliseconds.
    pub duration_ms: u64,
    /// 0-based index in the tool call loop.
    pub iteration: u32,
}

/// Project metadata for the usage database.
pub struct ProjectRecord {
    pub project_id: String,
    pub display_name: String,
    pub root_path: String,
}

/// Session metadata for the usage database.
pub struct SessionRecord {
    pub session_id: String,
    pub project_id: String,
    pub title: String,
    pub model_ref: String,
    pub created_at: DateTime<Utc>,
}

// ── Read-side types (used by `steve data` TUI) ─────────────

/// A session row with aggregated API call stats for the session list view.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub session_id: String,
    pub project_id: String,
    pub project_name: String,
    pub title: String,
    pub model_ref: String,
    pub created_at: String,
    pub call_count: u32,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_tokens: u64,
    pub total_cost: Option<f64>,
    pub total_duration_ms: u64,
}

/// A single API call row for the drill-down detail view.
#[derive(Debug, Clone)]
pub struct ApiCallDetail {
    pub id: i64,
    pub timestamp: String,
    pub model_ref: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cost: Option<f64>,
    pub duration_ms: u64,
    pub iteration: u32,
}

/// Aggregate totals across the currently filtered data.
#[derive(Debug, Clone, Default)]
pub struct UsageStats {
    pub session_count: u32,
    pub call_count: u32,
    pub total_tokens: u64,
    pub total_cost: f64,
    pub models_used: Vec<String>,
}

/// Project metadata for the filter dropdown.
#[derive(Debug, Clone)]
pub struct ProjectInfo {
    pub project_id: String,
    pub display_name: String,
    pub session_count: u32,
}

/// Filter criteria for session queries.
#[derive(Debug, Clone, Default)]
pub struct SessionFilter {
    pub project_id: Option<String>,
    pub model_ref: Option<String>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
}

// ── Internal types ──────────────────────────────────────────

/// Commands sent to the usage writer thread via channel.
pub(crate) enum UsageCommand {
    RecordApiCall(ApiCallRecord),
    UpsertProject(ProjectRecord),
    UpsertSession(SessionRecord),
    UpdateSessionTitle { session_id: String, title: String },
    Shutdown,
}
