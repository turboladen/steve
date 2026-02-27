//! Structured message blocks for the TUI message area.
//!
//! Rich structured message types that support grouped tool calls,
//! collapsible thinking sections, and expandable results.

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

    /// Permission prompt requiring user input.
    Permission {
        tool_name: String,
        args_summary: String,
    },
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

impl MessageBlock {
    /// Returns true if this is an `Assistant` block.
    pub fn is_assistant(&self) -> bool {
        matches!(self, MessageBlock::Assistant { .. })
    }

    /// Returns true if this is an `Assistant` block with empty text and no tool groups.
    pub fn is_empty_assistant(&self) -> bool {
        matches!(self, MessageBlock::Assistant { text, tool_groups, thinking, .. }
            if text.is_empty() && tool_groups.is_empty() && thinking.is_none())
    }

    /// Append text to an `Assistant` block. No-op on other variants.
    pub fn append_text(&mut self, delta: &str) {
        if let MessageBlock::Assistant { text, .. } = self {
            text.push_str(delta);
        }
    }

    /// Ensure the last tool group is in `Preparing` status. Creates one if needed.
    /// No-op on non-Assistant blocks.
    pub fn ensure_preparing_tool_group(&mut self) {
        if let MessageBlock::Assistant { tool_groups, .. } = self {
            let needs_new = tool_groups
                .last()
                .map(|g| g.status != ToolGroupStatus::Preparing)
                .unwrap_or(true);
            if needs_new {
                tool_groups.push(ToolGroup {
                    calls: vec![],
                    status: ToolGroupStatus::Preparing,
                });
            }
        }
    }

    /// Add a tool call to the last tool group, setting its status to `Running`.
    /// No-op on non-Assistant blocks.
    pub fn add_tool_call(&mut self, tool_name: ToolName, args_summary: String) {
        if let MessageBlock::Assistant { tool_groups, .. } = self {
            if let Some(group) = tool_groups.last_mut() {
                group.calls.push(ToolCall {
                    tool_name,
                    args_summary,
                    full_output: None,
                    result_summary: None,
                    is_error: false,
                    expanded: false,
                });
                group.status = ToolGroupStatus::Running {
                    current_tool: tool_name,
                };
            }
        }
    }

    /// Complete a tool call by filling in its result. Marks the group `Complete`
    /// if all calls have results.
    /// No-op on non-Assistant blocks.
    pub fn complete_tool_call(
        &mut self,
        tool_name: ToolName,
        result_summary: String,
        full_output: String,
        is_error: bool,
    ) {
        if let MessageBlock::Assistant { tool_groups, .. } = self {
            if let Some(group) = tool_groups.last_mut() {
                // Find the matching call (last one with this tool name and no result)
                if let Some(call) = group
                    .calls
                    .iter_mut()
                    .rev()
                    .find(|c| c.tool_name == tool_name && c.result_summary.is_none())
                {
                    call.result_summary = Some(result_summary);
                    call.full_output = Some(full_output);
                    call.is_error = is_error;
                }

                // Check if all calls are complete
                if group.calls.iter().all(|c| c.result_summary.is_some()) {
                    group.status = ToolGroupStatus::Complete;
                }
            }
        }
    }

    /// Append reasoning/thinking content. Creates the ThinkingBlock on first call.
    /// Increments token_count for each call (approximation: one call per delta).
    /// No-op on non-Assistant blocks.
    pub fn append_thinking(&mut self, delta: &str) {
        if let MessageBlock::Assistant { thinking, .. } = self {
            match thinking {
                Some(t) => {
                    t.content.push_str(delta);
                    t.token_count += 1;
                }
                None => {
                    *thinking = Some(ThinkingBlock {
                        token_count: 1,
                        content: delta.to_string(),
                        expanded: false,
                    });
                }
            }
        }
    }
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

    #[test]
    fn assistant_append_text() {
        let mut block = MessageBlock::Assistant {
            thinking: None,
            text: "Hello".into(),
            tool_groups: vec![],
        };
        block.append_text(" world");
        match &block {
            MessageBlock::Assistant { text, .. } => assert_eq!(text, "Hello world"),
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn assistant_append_text_noop_on_non_assistant() {
        let mut block = MessageBlock::User {
            text: "hello".into(),
        };
        block.append_text(" world");
        match &block {
            MessageBlock::User { text } => assert_eq!(text, "hello"),
            _ => panic!("expected User"),
        }
    }

    #[test]
    fn assistant_ensure_tool_group_creates_new() {
        let mut block = MessageBlock::Assistant {
            thinking: None,
            text: String::new(),
            tool_groups: vec![],
        };
        block.ensure_preparing_tool_group();
        match &block {
            MessageBlock::Assistant { tool_groups, .. } => {
                assert_eq!(tool_groups.len(), 1);
                assert_eq!(tool_groups[0].status, ToolGroupStatus::Preparing);
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn assistant_ensure_tool_group_reuses_preparing() {
        let mut block = MessageBlock::Assistant {
            thinking: None,
            text: String::new(),
            tool_groups: vec![ToolGroup {
                calls: vec![],
                status: ToolGroupStatus::Preparing,
            }],
        };
        block.ensure_preparing_tool_group();
        match &block {
            MessageBlock::Assistant { tool_groups, .. } => {
                assert_eq!(tool_groups.len(), 1);
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn assistant_add_tool_call() {
        let mut block = MessageBlock::Assistant {
            thinking: None,
            text: String::new(),
            tool_groups: vec![ToolGroup {
                calls: vec![],
                status: ToolGroupStatus::Preparing,
            }],
        };
        block.add_tool_call(ToolName::Read, "src/main.rs".into());
        match &block {
            MessageBlock::Assistant { tool_groups, .. } => {
                assert_eq!(tool_groups.last().unwrap().calls.len(), 1);
                let call = &tool_groups.last().unwrap().calls[0];
                assert_eq!(call.tool_name, ToolName::Read);
                assert_eq!(call.args_summary, "src/main.rs");
                assert!(call.result_summary.is_none());
                assert!(call.full_output.is_none());
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn assistant_complete_tool_call() {
        let mut block = MessageBlock::Assistant {
            thinking: None,
            text: String::new(),
            tool_groups: vec![ToolGroup {
                calls: vec![ToolCall {
                    tool_name: ToolName::Read,
                    args_summary: "src/main.rs".into(),
                    full_output: None,
                    result_summary: None,
                    is_error: false,
                    expanded: false,
                }],
                status: ToolGroupStatus::Running {
                    current_tool: ToolName::Read,
                },
            }],
        };
        block.complete_tool_call(
            ToolName::Read,
            "150 lines".into(),
            "fn main() {}".into(),
            false,
        );
        match &block {
            MessageBlock::Assistant { tool_groups, .. } => {
                let call = &tool_groups.last().unwrap().calls[0];
                assert_eq!(call.result_summary.as_deref(), Some("150 lines"));
                assert_eq!(call.full_output.as_deref(), Some("fn main() {}"));
                assert!(!call.is_error);
                // Group should be marked Complete since all calls have results
                assert_eq!(
                    tool_groups.last().unwrap().status,
                    ToolGroupStatus::Complete
                );
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn assistant_append_thinking() {
        let mut block = MessageBlock::Assistant {
            thinking: None,
            text: String::new(),
            tool_groups: vec![],
        };
        block.append_thinking("Let me ");
        block.append_thinking("think...");
        match &block {
            MessageBlock::Assistant { thinking, .. } => {
                let t = thinking.as_ref().unwrap();
                assert_eq!(t.content, "Let me think...");
                assert_eq!(t.token_count, 2);
                assert!(!t.expanded);
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn is_assistant_check() {
        let a = MessageBlock::Assistant {
            thinking: None,
            text: String::new(),
            tool_groups: vec![],
        };
        let u = MessageBlock::User {
            text: "hi".into(),
        };
        assert!(a.is_assistant());
        assert!(!u.is_assistant());
    }

    #[test]
    fn permission_block_construction() {
        let block = MessageBlock::Permission {
            tool_name: "bash".into(),
            args_summary: "ls -la".into(),
        };
        match &block {
            MessageBlock::Permission { tool_name, args_summary } => {
                assert_eq!(tool_name, "bash");
                assert_eq!(args_summary, "ls -la");
            }
            _ => panic!("expected Permission block"),
        }
    }

    #[test]
    fn permission_is_not_assistant() {
        let block = MessageBlock::Permission {
            tool_name: "bash".into(),
            args_summary: "ls".into(),
        };
        assert!(!block.is_assistant());
        assert!(!block.is_empty_assistant());
    }

    #[test]
    fn is_empty_assistant() {
        let empty = MessageBlock::Assistant {
            thinking: None,
            text: String::new(),
            tool_groups: vec![],
        };
        let non_empty = MessageBlock::Assistant {
            thinking: None,
            text: "hello".into(),
            tool_groups: vec![],
        };
        assert!(empty.is_empty_assistant());
        assert!(!non_empty.is_empty_assistant());
    }
}
