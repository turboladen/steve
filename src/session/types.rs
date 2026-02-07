use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Metadata about a chat session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// Project this session belongs to (git root commit hash or hashed CWD).
    pub project_id: String,
    /// Human-readable title (auto-generated after first exchange).
    pub title: String,
    /// The model used for this session, in "provider/model" format.
    pub model_ref: String,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
    /// When the session was last updated.
    pub updated_at: DateTime<Utc>,
    /// Accumulated token usage across all steps.
    pub token_usage: TokenUsage,
}

/// Accumulated token usage for a session.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}
