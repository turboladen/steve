use chrono::{DateTime, Utc};

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

/// Commands sent to the usage writer thread via channel.
pub(crate) enum UsageCommand {
    RecordApiCall(ApiCallRecord),
    UpsertProject(ProjectRecord),
    UpsertSession(SessionRecord),
    UpdateSessionTitle { session_id: String, title: String },
    Shutdown,
}
