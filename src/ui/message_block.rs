//! Structured message blocks for the TUI message area.
//!
//! Rich structured message types that support grouped tool calls,
//! collapsible thinking sections, and expandable results.

use crate::tool::ToolName;

/// Diff content extracted from tool call arguments for inline rendering.
#[derive(Debug, Clone)]
pub enum DiffContent {
    /// Edit tool: old_string lines as removals, new_string lines as additions.
    EditDiff { lines: Vec<DiffLine> },
    /// Write tool: just the line count of the written content.
    WriteSummary { line_count: usize },
    /// Patch tool: parsed unified diff with context lines.
    PatchDiff { lines: Vec<DiffLine> },
}

/// A single line in a diff display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    /// Removed line (red, "-" prefix).
    Removal(String),
    /// Added line (green, "+" prefix).
    Addition(String),
    /// Unchanged context line (dim, " " prefix).
    Context(String),
    /// Hunk header (dim, @@ line).
    HunkHeader(String),
}

/// An ordered part within an assistant response block.
///
/// Text and tool groups are interleaved in the order they arrive from the
/// LLM stream, preserving the chronological reading order.
#[derive(Debug, Clone)]
pub enum AssistantPart {
    /// A segment of response text (may be streamed token-by-token).
    Text(String),
    /// A batch of tool calls executed together.
    ToolGroup(ToolGroup),
}

/// A structured message block in the conversation.
#[derive(Debug, Clone)]
pub enum MessageBlock {
    /// User's input message.
    User { text: String },

    /// Assistant response with optional structured sub-parts.
    Assistant {
        /// Collapsed thinking indicator, if model emitted reasoning tokens.
        thinking: Option<ThinkingBlock>,
        /// Ordered sequence of text and tool groups, preserving stream order.
        parts: Vec<AssistantPart>,
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
    /// Diff content extracted from tool arguments for inline rendering.
    pub diff_content: Option<DiffContent>,
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

    /// Returns true if this is an `Assistant` block with no parts and no thinking.
    pub fn is_empty_assistant(&self) -> bool {
        matches!(self, MessageBlock::Assistant { parts, thinking, .. }
            if parts.is_empty() && thinking.is_none())
    }

    /// Append text to an `Assistant` block. If the last part is `Text`, appends
    /// to it; otherwise pushes a new `Text` part. No-op on other variants.
    pub fn append_text(&mut self, delta: &str) {
        if let MessageBlock::Assistant { parts, .. } = self {
            if let Some(AssistantPart::Text(text)) = parts.last_mut() {
                text.push_str(delta);
            } else {
                parts.push(AssistantPart::Text(delta.to_string()));
            }
        }
    }

    /// Ensure the last part is a `ToolGroup` in `Preparing` status. Creates one if needed.
    /// No-op on non-Assistant blocks.
    pub fn ensure_preparing_tool_group(&mut self) {
        if let MessageBlock::Assistant { parts, .. } = self {
            let needs_new = match parts.last() {
                Some(AssistantPart::ToolGroup(g)) => g.status != ToolGroupStatus::Preparing,
                _ => true,
            };
            if needs_new {
                parts.push(AssistantPart::ToolGroup(ToolGroup {
                    calls: vec![],
                    status: ToolGroupStatus::Preparing,
                }));
            }
        }
    }

    /// Add a tool call to the last tool group part, setting its status to `Running`.
    /// Write tools auto-expand to show inline diffs. No-op on non-Assistant blocks.
    pub fn add_tool_call(
        &mut self,
        tool_name: ToolName,
        args_summary: String,
        diff_content: Option<DiffContent>,
    ) {
        if let MessageBlock::Assistant { parts, .. } = self {
            if let Some(AssistantPart::ToolGroup(group)) = parts.last_mut() {
                group.calls.push(ToolCall {
                    tool_name,
                    args_summary,
                    full_output: None,
                    result_summary: None,
                    diff_content,
                    is_error: false,
                    expanded: tool_name.is_write_tool(),
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
        if let MessageBlock::Assistant { parts, .. } = self {
            // Find the last ToolGroup part
            if let Some(AssistantPart::ToolGroup(group)) =
                parts.iter_mut().rev().find_map(|p| match p {
                    AssistantPart::ToolGroup(_) => Some(p),
                    _ => None,
                })
            {
                // Find the matching call (first pending one with this tool name).
                // Forward search: results arrive in the same order as calls were added
                // (stream.rs emits events iterating auto_allowed in original order).
                if let Some(call) = group
                    .calls
                    .iter_mut()
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

/// Classification of a text line as a CommonMark fenced code block delimiter.
///
/// A fence is triple backticks (`` ``` ``) with ≤3 leading ASCII spaces per the CommonMark spec.
/// Used by `render_text_with_code_blocks()` and `extract_last_code_block()` as the single
/// source of truth for fence detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeFence {
    /// Not a fence — regular text line.
    NotFence,
    /// Opening fence, with optional language label (e.g., "rust", "").
    Open { lang: String },
    /// Closing fence.
    Close,
}

impl CodeFence {
    /// Classify a text line as a fence or not.
    ///
    /// `in_code_block` is needed because the same line (e.g. `` ``` ``)
    /// is an opening fence or a closing fence depending on context.
    pub fn classify(line: &str, in_code_block: bool) -> Self {
        let trimmed = line.trim_start_matches(' ');
        let leading_spaces = line.len() - trimmed.len();
        if leading_spaces <= 3 && trimmed.starts_with("```") {
            if in_code_block {
                CodeFence::Close
            } else {
                CodeFence::Open {
                    lang: trimmed[3..].trim().to_string(),
                }
            }
        } else {
            CodeFence::NotFence
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
            parts: vec![],
        };
        match &block {
            MessageBlock::Assistant { thinking, parts } => {
                assert!(thinking.is_none());
                assert!(parts.is_empty());
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
            diff_content: None,
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
            parts: vec![AssistantPart::Text("Hello".into())],
        };
        block.append_text(" world");
        match &block {
            MessageBlock::Assistant { parts, .. } => {
                assert_eq!(parts.len(), 1);
                match &parts[0] {
                    AssistantPart::Text(t) => assert_eq!(t, "Hello world"),
                    _ => panic!("expected Text part"),
                }
            }
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
            parts: vec![],
        };
        block.ensure_preparing_tool_group();
        match &block {
            MessageBlock::Assistant { parts, .. } => {
                assert_eq!(parts.len(), 1);
                match &parts[0] {
                    AssistantPart::ToolGroup(g) => {
                        assert_eq!(g.status, ToolGroupStatus::Preparing);
                    }
                    _ => panic!("expected ToolGroup part"),
                }
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn assistant_ensure_tool_group_reuses_preparing() {
        let mut block = MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![],
                status: ToolGroupStatus::Preparing,
            })],
        };
        block.ensure_preparing_tool_group();
        match &block {
            MessageBlock::Assistant { parts, .. } => {
                assert_eq!(parts.len(), 1);
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn assistant_add_tool_call() {
        let mut block = MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![],
                status: ToolGroupStatus::Preparing,
            })],
        };
        block.add_tool_call(ToolName::Read, "src/main.rs".into(), None);
        match &block {
            MessageBlock::Assistant { parts, .. } => {
                let group = match &parts[0] {
                    AssistantPart::ToolGroup(g) => g,
                    _ => panic!("expected ToolGroup part"),
                };
                assert_eq!(group.calls.len(), 1);
                let call = &group.calls[0];
                assert_eq!(call.tool_name, ToolName::Read);
                assert_eq!(call.args_summary, "src/main.rs");
                assert!(call.result_summary.is_none());
                assert!(call.full_output.is_none());
                assert!(!call.expanded, "read tool should not auto-expand");
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn assistant_complete_tool_call() {
        let mut block = MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![ToolCall {
                    tool_name: ToolName::Read,
                    args_summary: "src/main.rs".into(),
                    full_output: None,
                    result_summary: None,
                    diff_content: None,
                    is_error: false,
                    expanded: false,
                }],
                status: ToolGroupStatus::Running {
                    current_tool: ToolName::Read,
                },
            })],
        };
        block.complete_tool_call(
            ToolName::Read,
            "150 lines".into(),
            "fn main() {}".into(),
            false,
        );
        match &block {
            MessageBlock::Assistant { parts, .. } => {
                let group = match &parts[0] {
                    AssistantPart::ToolGroup(g) => g,
                    _ => panic!("expected ToolGroup part"),
                };
                let call = &group.calls[0];
                assert_eq!(call.result_summary.as_deref(), Some("150 lines"));
                assert_eq!(call.full_output.as_deref(), Some("fn main() {}"));
                assert!(!call.is_error);
                // Group should be marked Complete since all calls have results
                assert_eq!(group.status, ToolGroupStatus::Complete);
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn assistant_append_thinking() {
        let mut block = MessageBlock::Assistant {
            thinking: None,
            parts: vec![],
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
            parts: vec![],
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
            parts: vec![],
        };
        let non_empty = MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::Text("hello".into())],
        };
        assert!(empty.is_empty_assistant());
        assert!(!non_empty.is_empty_assistant());
    }

    #[test]
    fn write_tools_auto_expand() {
        for tool in [ToolName::Edit, ToolName::Write, ToolName::Patch] {
            let mut block = MessageBlock::Assistant {
                thinking: None,
                parts: vec![AssistantPart::ToolGroup(ToolGroup {
                    calls: vec![],
                    status: ToolGroupStatus::Preparing,
                })],
            };
            block.add_tool_call(tool, "file.rs".into(), None);
            match &block {
                MessageBlock::Assistant { parts, .. } => {
                    let group = match &parts[0] {
                        AssistantPart::ToolGroup(g) => g,
                        _ => panic!("expected ToolGroup part"),
                    };
                    assert!(group.calls[0].expanded, "{tool} should auto-expand");
                }
                _ => panic!("expected Assistant"),
            }
        }
    }

    #[test]
    fn non_write_tools_stay_collapsed() {
        for tool in [
            ToolName::Read,
            ToolName::Grep,
            ToolName::Glob,
            ToolName::List,
            ToolName::Bash,
            ToolName::Question,
            ToolName::Todo,
            ToolName::Webfetch,
            ToolName::Memory,
        ] {
            let mut block = MessageBlock::Assistant {
                thinking: None,
                parts: vec![AssistantPart::ToolGroup(ToolGroup {
                    calls: vec![],
                    status: ToolGroupStatus::Preparing,
                })],
            };
            block.add_tool_call(tool, "pattern".into(), None);
            match &block {
                MessageBlock::Assistant { parts, .. } => {
                    let group = match &parts[0] {
                        AssistantPart::ToolGroup(g) => g,
                        _ => panic!("expected ToolGroup part"),
                    };
                    assert!(!group.calls[0].expanded, "{tool} should not auto-expand");
                }
                _ => panic!("expected Assistant"),
            }
        }
    }

    #[test]
    fn diff_content_stored_in_tool_call() {
        let mut block = MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![],
                status: ToolGroupStatus::Preparing,
            })],
        };
        let diff = DiffContent::EditDiff {
            lines: vec![
                DiffLine::Removal("old line".into()),
                DiffLine::Addition("new line".into()),
            ],
        };
        block.add_tool_call(ToolName::Edit, "src/main.rs".into(), Some(diff));
        match &block {
            MessageBlock::Assistant { parts, .. } => {
                let group = match &parts[0] {
                    AssistantPart::ToolGroup(g) => g,
                    _ => panic!("expected ToolGroup part"),
                };
                assert!(group.calls[0].diff_content.is_some());
                assert!(group.calls[0].expanded);
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn append_text_creates_new_part_after_tool_group() {
        let mut block = MessageBlock::Assistant {
            thinking: None,
            parts: vec![
                AssistantPart::Text("intro".into()),
                AssistantPart::ToolGroup(ToolGroup {
                    calls: vec![],
                    status: ToolGroupStatus::Complete,
                }),
            ],
        };
        block.append_text("summary");
        match &block {
            MessageBlock::Assistant { parts, .. } => {
                assert_eq!(parts.len(), 3);
                match &parts[2] {
                    AssistantPart::Text(t) => assert_eq!(t, "summary"),
                    _ => panic!("expected Text part"),
                }
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn append_text_to_empty_parts() {
        let mut block = MessageBlock::Assistant {
            thinking: None,
            parts: vec![],
        };
        block.append_text("hello");
        match &block {
            MessageBlock::Assistant { parts, .. } => {
                assert_eq!(parts.len(), 1);
                match &parts[0] {
                    AssistantPart::Text(t) => assert_eq!(t, "hello"),
                    _ => panic!("expected Text part"),
                }
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn complete_tool_call_finds_group_after_text() {
        // Simulate: tool group → text appended → complete arrives
        let mut block = MessageBlock::Assistant {
            thinking: None,
            parts: vec![
                AssistantPart::ToolGroup(ToolGroup {
                    calls: vec![ToolCall {
                        tool_name: ToolName::Read,
                        args_summary: "f.rs".into(),
                        full_output: None,
                        result_summary: None,
                        diff_content: None,
                        is_error: false,
                        expanded: false,
                    }],
                    status: ToolGroupStatus::Running {
                        current_tool: ToolName::Read,
                    },
                }),
                AssistantPart::Text("some text".into()),
            ],
        };
        block.complete_tool_call(ToolName::Read, "ok".into(), "content".into(), false);
        match &block {
            MessageBlock::Assistant { parts, .. } => {
                let group = match &parts[0] {
                    AssistantPart::ToolGroup(g) => g,
                    _ => panic!("expected ToolGroup"),
                };
                assert_eq!(group.calls[0].result_summary.as_deref(), Some("ok"));
                assert_eq!(group.status, ToolGroupStatus::Complete);
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn complete_tool_call_same_tool_parallel_preserves_order() {
        // Two parallel read calls — results arrive in forward order (stream.rs
        // iterates auto_allowed in original order). Each result must match the
        // corresponding call, not get swapped by a reverse search.
        let mut block = MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![
                    ToolCall {
                        tool_name: ToolName::Read,
                        args_summary: "a.rs".into(),
                        full_output: None,
                        result_summary: None,
                        diff_content: None,
                        is_error: false,
                        expanded: false,
                    },
                    ToolCall {
                        tool_name: ToolName::Read,
                        args_summary: "b.rs".into(),
                        full_output: None,
                        result_summary: None,
                        diff_content: None,
                        is_error: false,
                        expanded: false,
                    },
                ],
                status: ToolGroupStatus::Running {
                    current_tool: ToolName::Read,
                },
            })],
        };
        block.complete_tool_call(ToolName::Read, "10 lines".into(), "content_a".into(), false);
        block.complete_tool_call(ToolName::Read, "20 lines".into(), "content_b".into(), false);
        match &block {
            MessageBlock::Assistant { parts, .. } => {
                let group = match &parts[0] {
                    AssistantPart::ToolGroup(g) => g,
                    _ => panic!("expected ToolGroup"),
                };
                assert_eq!(
                    group.calls[0].result_summary.as_deref(),
                    Some("10 lines"),
                    "first result should go to first call (a.rs)"
                );
                assert_eq!(
                    group.calls[0].full_output.as_deref(),
                    Some("content_a"),
                );
                assert_eq!(
                    group.calls[1].result_summary.as_deref(),
                    Some("20 lines"),
                    "second result should go to second call (b.rs)"
                );
                assert_eq!(
                    group.calls[1].full_output.as_deref(),
                    Some("content_b"),
                );
                assert_eq!(group.status, ToolGroupStatus::Complete);
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn diff_line_equality() {
        assert_eq!(
            DiffLine::Removal("foo".into()),
            DiffLine::Removal("foo".into())
        );
        assert_ne!(
            DiffLine::Removal("foo".into()),
            DiffLine::Addition("foo".into())
        );
    }

    // --- CodeFence tests ---

    #[test]
    fn code_fence_plain_text() {
        assert_eq!(CodeFence::classify("hello", false), CodeFence::NotFence);
        assert_eq!(CodeFence::classify("hello", true), CodeFence::NotFence);
    }

    #[test]
    fn code_fence_open_bare() {
        assert_eq!(
            CodeFence::classify("```", false),
            CodeFence::Open { lang: String::new() }
        );
    }

    #[test]
    fn code_fence_open_with_lang() {
        assert_eq!(
            CodeFence::classify("```rust", false),
            CodeFence::Open { lang: "rust".to_string() }
        );
        assert_eq!(
            CodeFence::classify("```  python  ", false),
            CodeFence::Open { lang: "python".to_string() }
        );
    }

    #[test]
    fn code_fence_close() {
        assert_eq!(CodeFence::classify("```", true), CodeFence::Close);
        // Language label on closing fence still counts as close
        assert_eq!(CodeFence::classify("```rust", true), CodeFence::Close);
    }

    #[test]
    fn code_fence_four_spaces_not_fence() {
        assert_eq!(CodeFence::classify("    ```", false), CodeFence::NotFence);
        assert_eq!(CodeFence::classify("    ```", true), CodeFence::NotFence);
    }

    #[test]
    fn code_fence_three_spaces_is_fence() {
        assert_eq!(
            CodeFence::classify("   ```", false),
            CodeFence::Open { lang: String::new() }
        );
        assert_eq!(CodeFence::classify("   ```", true), CodeFence::Close);
    }

    #[test]
    fn code_fence_tab_not_fence() {
        assert_eq!(CodeFence::classify("\t```", false), CodeFence::NotFence);
        assert_eq!(CodeFence::classify("\t```", true), CodeFence::NotFence);
    }
}
