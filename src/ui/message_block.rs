//! Structured message blocks for the TUI message area.
//!
//! Replaces flat `DisplayMessage { role, text }` with rich types that support
//! grouped tool calls, collapsible thinking sections, and expandable results.

use crate::tool::ToolName;

/// A structured message block in the conversation.
#[derive(Debug, Clone)]
pub enum MessageBlock {
    /// User's input message.
    User { text: String },

    /// Assistant response with optional structured sub-parts.
    Assistant {
        /// Collapsed thinking indicator, if model emitted reasoning tokens.
        thinking: Option<ThinkingBlock>,
        /// The actual response text (streamed token by token).
        text: String,
        /// Tool activity that occurred during this response turn.
        tool_groups: Vec<ToolGroup>,
    },

    /// System notification (session started, model switched, etc.).
    System { text: String },

    /// Error message.
    Error { text: String },
}

/// Reasoning/thinking content from the model, collapsed by default.
#[derive(Debug, Clone)]
pub struct ThinkingBlock {
    /// Number of reasoning tokens received.
    pub token_count: usize,
    /// Full thinking text (kept for expand-on-demand).
    pub content: String,
    /// Whether this block is currently expanded in the UI.
    pub expanded: bool,
}

/// A batch of tool calls executed together.
#[derive(Debug, Clone)]
pub struct ToolGroup {
    /// Individual tool calls in this batch.
    pub calls: Vec<ToolCall>,
    /// Overall status of this group.
    pub status: ToolGroupStatus,
}

/// A single tool call with its result.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Which tool was called.
    pub tool_name: ToolName,
    /// Compact argument summary (e.g., "src/main.rs" for read).
    pub args_summary: String,
    /// Full output for expand-on-demand.
    pub full_output: Option<String>,
    /// Compact result summary shown when collapsed (e.g., "150 lines").
    pub result_summary: Option<String>,
    /// Whether the tool call resulted in an error.
    pub is_error: bool,
    /// Whether this call's output is currently expanded in the UI.
    pub expanded: bool,
}

/// Status of a tool group's execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolGroupStatus {
    /// Tool calls are being assembled from the stream.
    Preparing,
    /// Tool calls are executing.
    Running { current_tool: ToolName },
    /// All tool calls in this group have completed.
    Complete,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_block_construction() {
        let block = MessageBlock::User { text: "hello".into() };
        match &block {
            MessageBlock::User { text } => assert_eq!(text, "hello"),
            _ => panic!("expected User block"),
        }
    }

    #[test]
    fn assistant_block_default_state() {
        let block = MessageBlock::Assistant {
            thinking: None,
            text: String::new(),
            tool_groups: vec![],
        };
        match &block {
            MessageBlock::Assistant { thinking, text, tool_groups } => {
                assert!(thinking.is_none());
                assert!(text.is_empty());
                assert!(tool_groups.is_empty());
            }
            _ => panic!("expected Assistant block"),
        }
    }

    #[test]
    fn thinking_block_defaults_collapsed() {
        let thinking = ThinkingBlock {
            token_count: 42,
            content: "Let me think...".into(),
            expanded: false,
        };
        assert!(!thinking.expanded);
        assert_eq!(thinking.token_count, 42);
    }

    #[test]
    fn tool_call_defaults_collapsed() {
        let call = ToolCall {
            tool_name: ToolName::Read,
            args_summary: "src/main.rs".into(),
            full_output: Some("fn main() {}".into()),
            result_summary: Some("1 line".into()),
            is_error: false,
            expanded: false,
        };
        assert!(!call.expanded);
        assert!(!call.is_error);
        assert_eq!(call.result_summary.as_deref(), Some("1 line"));
    }

    #[test]
    fn tool_group_status_transitions() {
        let mut group = ToolGroup {
            calls: vec![],
            status: ToolGroupStatus::Preparing,
        };
        assert_eq!(group.status, ToolGroupStatus::Preparing);

        group.status = ToolGroupStatus::Running { current_tool: ToolName::Read };
        assert_eq!(group.status, ToolGroupStatus::Running { current_tool: ToolName::Read });

        group.status = ToolGroupStatus::Complete;
        assert_eq!(group.status, ToolGroupStatus::Complete);
    }

    #[test]
    fn system_and_error_blocks() {
        let sys = MessageBlock::System { text: "Session started.".into() };
        let err = MessageBlock::Error { text: "Connection failed.".into() };
        match &sys {
            MessageBlock::System { text } => assert_eq!(text, "Session started."),
            _ => panic!("expected System block"),
        }
        match &err {
            MessageBlock::Error { text } => assert_eq!(text, "Connection failed."),
            _ => panic!("expected Error block"),
        }
    }
}
