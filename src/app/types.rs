use crate::permission::types::PermissionReply;

/// A permission prompt waiting for user input.
pub(super) struct PendingPermission {
    pub(super) tool_name: crate::tool::ToolName,
    #[allow(dead_code)]
    pub(super) summary: String,
    pub(super) response_tx: tokio::sync::oneshot::Sender<PermissionReply>,
}

/// A question prompt from the LLM waiting for user input.
pub(super) struct PendingQuestion {
    pub(super) call_id: String,
    #[allow(dead_code)]
    pub(super) question: String,
    pub(super) options: Vec<String>,
    pub(super) selected: Option<usize>,
    pub(super) free_text: String,
    pub(super) response_tx: tokio::sync::oneshot::Sender<String>,
}
