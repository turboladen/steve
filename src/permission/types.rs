use serde::{Deserialize, Serialize};

/// What the permission system decides for a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionAction {
    /// Tool call is allowed automatically (no prompt).
    Allow,
    /// Tool call is denied outright.
    Deny,
    /// User must be asked for permission.
    Ask,
}

/// A rule that maps a tool + path pattern to an action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    /// Tool name (e.g., "bash", "edit", "write").
    pub tool: String,
    /// Glob pattern for matching the first argument / path (e.g., "src/**").
    /// Use "*" to match everything.
    pub pattern: String,
    /// The action to take when this rule matches.
    pub action: PermissionActionSerde,
}

/// Serializable version of PermissionAction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionActionSerde {
    Allow,
    Deny,
    Ask,
}

impl From<PermissionActionSerde> for PermissionAction {
    fn from(s: PermissionActionSerde) -> Self {
        match s {
            PermissionActionSerde::Allow => PermissionAction::Allow,
            PermissionActionSerde::Deny => PermissionAction::Deny,
            PermissionActionSerde::Ask => PermissionAction::Ask,
        }
    }
}

/// A permission request sent to the UI for user approval.
#[derive(Debug)]
pub struct PermissionRequest {
    pub call_id: String,
    pub tool_name: String,
    pub arguments_summary: String,
    pub response_tx: tokio::sync::oneshot::Sender<PermissionReply>,
}

/// The user's reply to a permission prompt.
#[derive(Debug, Clone)]
pub enum PermissionReply {
    /// Allow this specific tool call.
    AllowOnce,
    /// Allow this tool for the rest of the session.
    AllowAlways,
    /// Deny this tool call.
    Deny,
}
