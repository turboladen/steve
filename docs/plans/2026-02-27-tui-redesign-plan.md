# TUI Redesign Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Rearchitect Steve's TUI around a structured message model, 4-region layout, and proper scroll/status/input systems.

**Architecture:** Replace `Vec<DisplayMessage>` with `Vec<MessageBlock>` — a structured enum that groups tool calls, thinking tokens, and response text into collapsible assistant turns. Add a status line footer, fix scroll direction/clamping, and improve the input area with Shift+Enter newlines and command autocomplete.

**Tech Stack:** Rust, ratatui 0.29, crossterm 0.28, tui-textarea 0.7, strum 0.28

---

## Phase 1: Foundation Types

New types with full test coverage. No behavioral changes — existing code continues to work.

### Task 1: Create `MessageBlock` type module

**Files:**
- Create: `src/ui/message_block.rs`
- Modify: `src/ui/mod.rs:1` (add `pub mod message_block;`)

**Step 1: Write the failing test**

Create `src/ui/message_block.rs` with types and test module:

```rust
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
```

**Step 2: Register the module**

In `src/ui/mod.rs`, add after line 1:
```rust
pub mod message_block;
```

**Step 3: Run tests to verify they pass**

Run: `cargo test ui::message_block`
Expected: All 6 tests pass.

**Step 4: Commit**

```bash
git add src/ui/message_block.rs src/ui/mod.rs
git commit -m "feat(ui): add MessageBlock structured types for message area"
```

---

### Task 2: Add `MessageBlock` helper methods

**Files:**
- Modify: `src/ui/message_block.rs`

These helpers will be used by event handlers in `app.rs` to mutate the current assistant block.

**Step 1: Write the failing tests**

Add to the `tests` module in `src/ui/message_block.rs`:

```rust
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
        let mut block = MessageBlock::User { text: "hello".into() };
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
                assert_eq!(tool_groups.len(), 1); // reused, not duplicated
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
                status: ToolGroupStatus::Running { current_tool: ToolName::Read },
            }],
        };
        block.complete_tool_call(ToolName::Read, "150 lines".into(), "fn main() {}".into(), false);
        match &block {
            MessageBlock::Assistant { tool_groups, .. } => {
                let call = &tool_groups.last().unwrap().calls[0];
                assert_eq!(call.result_summary.as_deref(), Some("150 lines"));
                assert_eq!(call.full_output.as_deref(), Some("fn main() {}"));
                assert!(!call.is_error);
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
        let u = MessageBlock::User { text: "hi".into() };
        assert!(a.is_assistant());
        assert!(!u.is_assistant());
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
```

**Step 2: Run tests to verify they fail**

Run: `cargo test ui::message_block`
Expected: FAIL — methods don't exist yet.

**Step 3: Implement the helper methods**

Add to `MessageBlock` in `src/ui/message_block.rs`, before the `tests` module:

```rust
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
            let needs_new = tool_groups.last()
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
                group.status = ToolGroupStatus::Running { current_tool: tool_name };
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
                if let Some(call) = group.calls.iter_mut().rev()
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
```

**Step 4: Run tests to verify they pass**

Run: `cargo test ui::message_block`
Expected: All tests pass.

**Step 5: Commit**

```bash
git add src/ui/message_block.rs
git commit -m "feat(ui): add MessageBlock helper methods for event-driven mutation"
```

---

### Task 3: Create `StatusLineState` type

**Files:**
- Create: `src/ui/status_line.rs`
- Modify: `src/ui/mod.rs` (add `pub mod status_line;`)

**Step 1: Write the type and tests**

Create `src/ui/status_line.rs`:

```rust
//! Status line state and rendering for the TUI footer.

use crate::tool::ToolName;

/// Braille spinner frames, cycled on each 100ms tick.
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];

/// Current activity shown in the status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    /// No activity — agent is idle.
    Idle,
    /// LLM is generating text (streaming, no tool calls yet).
    Thinking,
    /// A tool is currently executing.
    RunningTool { tool_name: ToolName, args_summary: String },
    /// Waiting for the user to approve a permission prompt.
    WaitingForPermission,
    /// Compaction is in progress.
    Compacting,
}

/// State for the status line footer.
pub struct StatusLineState {
    /// Current activity.
    pub activity: Activity,
    /// Spinner frame index (0..SPINNER_FRAMES.len()), advanced on tick.
    pub spinner_frame: usize,
    /// Model reference string (e.g., "gpt-4o").
    pub model_name: String,
    /// Total tokens used in this session.
    pub total_tokens: u64,
    /// Context window size for the current model.
    pub context_window: u64,
}

impl Default for StatusLineState {
    fn default() -> Self {
        Self {
            activity: Activity::Idle,
            spinner_frame: 0,
            model_name: String::new(),
            total_tokens: 0,
            context_window: 0,
        }
    }
}

impl StatusLineState {
    /// Advance the spinner to the next frame. Called on each tick.
    pub fn tick(&mut self) {
        self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
    }

    /// Get the current spinner character, or None if idle.
    pub fn spinner_char(&self) -> Option<char> {
        if self.activity == Activity::Idle {
            None
        } else {
            Some(SPINNER_FRAMES[self.spinner_frame])
        }
    }

    /// Format the activity as a display string.
    pub fn activity_text(&self) -> String {
        match &self.activity {
            Activity::Idle => String::new(),
            Activity::Thinking => "Thinking...".to_string(),
            Activity::RunningTool { tool_name, args_summary } => {
                if args_summary.is_empty() {
                    format!("Running {}...", tool_name)
                } else {
                    format!("Running {}({})...", tool_name, args_summary)
                }
            }
            Activity::WaitingForPermission => "Waiting for permission...".to_string(),
            Activity::Compacting => "Compacting...".to_string(),
        }
    }

    /// Context window usage as a percentage (0–100).
    pub fn context_usage_pct(&self) -> u8 {
        if self.context_window == 0 {
            0
        } else {
            ((self.total_tokens as f64 / self.context_window as f64) * 100.0).min(100.0) as u8
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_idle() {
        let state = StatusLineState::default();
        assert_eq!(state.activity, Activity::Idle);
        assert_eq!(state.spinner_frame, 0);
        assert!(state.model_name.is_empty());
    }

    #[test]
    fn tick_advances_spinner() {
        let mut state = StatusLineState::default();
        state.activity = Activity::Thinking;
        assert_eq!(state.spinner_frame, 0);
        state.tick();
        assert_eq!(state.spinner_frame, 1);
        // Wraps around
        for _ in 0..7 {
            state.tick();
        }
        assert_eq!(state.spinner_frame, 0);
    }

    #[test]
    fn spinner_char_none_when_idle() {
        let state = StatusLineState::default();
        assert_eq!(state.spinner_char(), None);
    }

    #[test]
    fn spinner_char_some_when_active() {
        let mut state = StatusLineState::default();
        state.activity = Activity::Thinking;
        assert_eq!(state.spinner_char(), Some('⠋'));
        state.tick();
        assert_eq!(state.spinner_char(), Some('⠙'));
    }

    #[test]
    fn activity_text_variants() {
        assert_eq!(
            StatusLineState { activity: Activity::Idle, ..Default::default() }.activity_text(),
            ""
        );
        assert_eq!(
            StatusLineState { activity: Activity::Thinking, ..Default::default() }.activity_text(),
            "Thinking..."
        );
        assert_eq!(
            StatusLineState {
                activity: Activity::RunningTool {
                    tool_name: ToolName::Read,
                    args_summary: "src/main.rs".into(),
                },
                ..Default::default()
            }.activity_text(),
            "Running read(src/main.rs)..."
        );
        assert_eq!(
            StatusLineState {
                activity: Activity::RunningTool {
                    tool_name: ToolName::Bash,
                    args_summary: String::new(),
                },
                ..Default::default()
            }.activity_text(),
            "Running bash..."
        );
        assert_eq!(
            StatusLineState { activity: Activity::WaitingForPermission, ..Default::default() }.activity_text(),
            "Waiting for permission..."
        );
        assert_eq!(
            StatusLineState { activity: Activity::Compacting, ..Default::default() }.activity_text(),
            "Compacting..."
        );
    }

    #[test]
    fn context_usage_pct_calculation() {
        let state = StatusLineState {
            total_tokens: 12800,
            context_window: 128000,
            ..Default::default()
        };
        assert_eq!(state.context_usage_pct(), 10);
    }

    #[test]
    fn context_usage_pct_zero_window() {
        let state = StatusLineState::default();
        assert_eq!(state.context_usage_pct(), 0);
    }

    #[test]
    fn context_usage_pct_capped_at_100() {
        let state = StatusLineState {
            total_tokens: 200000,
            context_window: 128000,
            ..Default::default()
        };
        assert_eq!(state.context_usage_pct(), 100);
    }
}
```

**Step 2: Register the module**

In `src/ui/mod.rs`, add: `pub mod status_line;`

**Step 3: Run tests**

Run: `cargo test ui::status_line`
Expected: All tests pass.

**Step 4: Commit**

```bash
git add src/ui/status_line.rs src/ui/mod.rs
git commit -m "feat(ui): add StatusLineState with spinner and activity tracking"
```

---

### Task 4: Add command metadata for autocomplete

**Files:**
- Modify: `src/command.rs`

**Step 1: Write the failing tests**

Add to the `tests` module in `src/command.rs`:

```rust
    #[test]
    fn all_commands_returns_all_entries() {
        let cmds = Command::all_commands();
        // Must contain every known command prefix
        let names: Vec<&str> = cmds.iter().map(|c| c.name).collect();
        assert!(names.contains(&"/exit"));
        assert!(names.contains(&"/new"));
        assert!(names.contains(&"/rename"));
        assert!(names.contains(&"/models"));
        assert!(names.contains(&"/model"));
        assert!(names.contains(&"/init"));
        assert!(names.contains(&"/compact"));
        assert!(names.contains(&"/help"));
        assert_eq!(cmds.len(), 8);
    }

    #[test]
    fn filter_commands_by_prefix() {
        let matches = Command::matching_commands("/m");
        let names: Vec<&str> = matches.iter().map(|c| c.name).collect();
        assert!(names.contains(&"/models"));
        assert!(names.contains(&"/model"));
        assert!(!names.contains(&"/exit"));
    }

    #[test]
    fn filter_commands_slash_only() {
        // "/" matches everything
        let matches = Command::matching_commands("/");
        assert_eq!(matches.len(), 8);
    }

    #[test]
    fn filter_commands_no_match() {
        let matches = Command::matching_commands("/zzz");
        assert!(matches.is_empty());
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test command`
Expected: FAIL — `all_commands` and `matching_commands` don't exist yet.

**Step 3: Implement command metadata**

Add to `src/command.rs`, before the `tests` module:

```rust
/// Metadata for a known slash command, used for autocomplete.
#[derive(Debug, Clone)]
pub struct CommandInfo {
    /// The command string (e.g., "/exit").
    pub name: &'static str,
    /// Short description shown in autocomplete popup.
    pub description: &'static str,
}

impl Command {
    /// Returns metadata for all known commands, in display order.
    pub fn all_commands() -> Vec<CommandInfo> {
        vec![
            CommandInfo { name: "/new", description: "Start a new session" },
            CommandInfo { name: "/rename", description: "Rename current session" },
            CommandInfo { name: "/model", description: "Switch model" },
            CommandInfo { name: "/models", description: "List available models" },
            CommandInfo { name: "/compact", description: "Compact conversation" },
            CommandInfo { name: "/init", description: "Create AGENTS.md" },
            CommandInfo { name: "/help", description: "Show help" },
            CommandInfo { name: "/exit", description: "Quit" },
        ]
    }

    /// Returns commands matching the given prefix (case-sensitive).
    pub fn matching_commands(prefix: &str) -> Vec<CommandInfo> {
        Self::all_commands()
            .into_iter()
            .filter(|c| c.name.starts_with(prefix))
            .collect()
    }
}
```

**Step 4: Run tests**

Run: `cargo test command`
Expected: All tests pass (old and new).

**Step 5: Commit**

```bash
git add src/command.rs
git commit -m "feat(command): add command metadata and prefix matching for autocomplete"
```

---

## Phase 2: Layout and Rendering

### Task 5: Update layout for 4 regions

**Files:**
- Modify: `src/ui/layout.rs`

**Step 1: Write the new layout struct and tests**

Replace the entire contents of `src/ui/layout.rs`:

```rust
use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Computed layout regions for the app.
pub struct AppLayout {
    pub message_area: Rect,
    pub input_area: Rect,
    pub status_line: Rect,
    pub sidebar: Option<Rect>,
}

const SIDEBAR_WIDTH: u16 = 40;
const SIDEBAR_MIN_TERMINAL_WIDTH: u16 = 120;
const INPUT_HEIGHT: u16 = 3;
const STATUS_HEIGHT: u16 = 1;

/// Compute the layout given the full terminal area.
///
/// Layout order (top to bottom):
/// - Message area (fills remaining space)
/// - Input area (3 rows, adjacent to messages)
/// - Status line (1 row footer, spans full width including sidebar)
///
/// Sidebar (if shown) sits to the right of messages + input, but status line
/// spans below everything.
pub fn compute_layout(area: Rect, show_sidebar: bool) -> AppLayout {
    let sidebar_visible = show_sidebar && area.width >= SIDEBAR_MIN_TERMINAL_WIDTH;

    // First, split off the status line at the bottom (full width)
    let vertical_outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),              // main content
            Constraint::Length(STATUS_HEIGHT), // status line
        ])
        .split(area);

    let main_area = vertical_outer[0];
    let status_line = vertical_outer[1];

    if sidebar_visible {
        // Split main area horizontally: content | sidebar
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(40),
                Constraint::Length(SIDEBAR_WIDTH),
            ])
            .split(main_area);

        let content_area = horizontal[0];
        let sidebar = horizontal[1];

        // Split content vertically: messages | input
        let vertical_inner = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(INPUT_HEIGHT),
            ])
            .split(content_area);

        AppLayout {
            message_area: vertical_inner[0],
            input_area: vertical_inner[1],
            status_line,
            sidebar: Some(sidebar),
        }
    } else {
        // No sidebar: just messages | input above status line
        let vertical_inner = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(INPUT_HEIGHT),
            ])
            .split(main_area);

        AppLayout {
            message_area: vertical_inner[0],
            input_area: vertical_inner[1],
            status_line,
            sidebar: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(width: u16, height: u16) -> Rect {
        Rect::new(0, 0, width, height)
    }

    #[test]
    fn layout_without_sidebar() {
        let layout = compute_layout(rect(80, 24), false);
        assert!(layout.sidebar.is_none());
        // Status line at bottom, 1 row
        assert_eq!(layout.status_line.height, STATUS_HEIGHT);
        assert_eq!(layout.status_line.y, 23); // last row of 24
        // Input above status
        assert_eq!(layout.input_area.height, INPUT_HEIGHT);
        assert_eq!(layout.input_area.y, 20); // 24 - 1(status) - 3(input)
        // Messages fill the rest
        assert_eq!(layout.message_area.y, 0);
        assert_eq!(layout.message_area.height, 20);
    }

    #[test]
    fn layout_with_sidebar() {
        let layout = compute_layout(rect(120, 24), true);
        assert!(layout.sidebar.is_some());
        let sidebar = layout.sidebar.unwrap();
        assert_eq!(sidebar.width, SIDEBAR_WIDTH);
        // Message area width = 120 - 40 = 80
        assert_eq!(layout.message_area.width, 80);
        // Status line spans full width
        assert_eq!(layout.status_line.width, 120);
    }

    #[test]
    fn layout_sidebar_not_shown_below_threshold() {
        let layout = compute_layout(rect(119, 24), true);
        assert!(layout.sidebar.is_none());
    }

    #[test]
    fn layout_status_line_always_full_width() {
        let layout = compute_layout(rect(150, 30), true);
        assert_eq!(layout.status_line.width, 150);
    }
}
```

**Step 2: Run tests**

Run: `cargo test ui::layout`
Expected: All tests pass.

**Step 3: Run `cargo check` to find compile errors from the layout change**

Run: `cargo check 2>&1`

The `AppLayout` struct changed (added `status_line` field). `src/ui/mod.rs:45-74` uses it in `render()` — this will need updating. However, we'll fix the compile error in the next task when we wire up the status line renderer.

If `cargo check` fails on `render()`, add a temporary `let _ = layout.status_line;` line in `render()` to suppress the unused field warning and keep compiling.

**Step 4: Commit**

```bash
git add src/ui/layout.rs src/ui/mod.rs
git commit -m "feat(ui): update layout for 4 regions with status line footer"
```

---

### Task 6: Create status line renderer

**Files:**
- Modify: `src/ui/status_line.rs` (add render function)
- Modify: `src/ui/mod.rs` (import and call `render_status_line`)

**Step 1: Add the render function**

Add to `src/ui/status_line.rs`, after the `impl StatusLineState` block and before `#[cfg(test)]`:

```rust
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use super::theme::Theme;
use super::input::AgentMode;

/// Format a token count with K/M suffixes.
fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Render the status line into the given 1-row area.
pub fn render_status_line(
    frame: &mut Frame,
    area: Rect,
    state: &StatusLineState,
    theme: &Theme,
    mode: AgentMode,
) {
    let mut left_spans: Vec<Span> = Vec::new();

    // Spinner + activity text
    if let Some(spinner) = state.spinner_char() {
        left_spans.push(Span::styled(
            format!("{spinner} "),
            Style::default().fg(theme.accent),
        ));
    }
    let activity = state.activity_text();
    if !activity.is_empty() {
        left_spans.push(Span::styled(
            activity,
            Style::default().fg(theme.accent),
        ));
    }

    // Right side: model | tokens/context (pct%) | mode
    let mut right_parts: Vec<String> = Vec::new();

    if !state.model_name.is_empty() {
        right_parts.push(state.model_name.clone());
    }

    if state.context_window > 0 {
        let pct = state.context_usage_pct();
        right_parts.push(format!(
            "{}/{}  ({}%)",
            format_tokens(state.total_tokens),
            format_tokens(state.context_window),
            pct,
        ));
    } else if state.total_tokens > 0 {
        right_parts.push(format_tokens(state.total_tokens));
    }

    right_parts.push(mode.display_name().to_string());

    let right_text = right_parts.join(" │ ");
    let pct = state.context_usage_pct();
    let right_color = if pct >= 80 {
        theme.error
    } else if pct >= 50 {
        theme.warning
    } else {
        theme.dim
    };

    // Calculate padding
    let left_width: usize = left_spans.iter().map(|s| s.width()).sum();
    let right_width = right_text.len();
    let available = area.width as usize;
    let padding = available.saturating_sub(left_width + right_width);

    left_spans.push(Span::raw(" ".repeat(padding)));
    left_spans.push(Span::styled(right_text, Style::default().fg(right_color)));

    let line = Line::from(left_spans);
    let block = Block::default().borders(Borders::NONE);
    let paragraph = Paragraph::new(line).block(block);
    frame.render_widget(paragraph, area);
}
```

**Step 2: Update `render()` in `src/ui/mod.rs`**

Replace the `render` function to include the status line:

```rust
use status_line::render_status_line;

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let show_sidebar = area.width >= 120;
    let layout = compute_layout(area, show_sidebar);

    render_messages(
        frame,
        layout.message_area,
        &app.messages,
        &mut app.message_area_state,
        &app.theme,
        app.is_loading,
    );

    if let Some(sidebar_area) = layout.sidebar {
        render_sidebar(
            frame,
            sidebar_area,
            &app.sidebar_state,
            &app.theme,
        );
    }

    render_input(
        frame,
        layout.input_area,
        &mut app.input,
        &app.theme,
    );

    render_status_line(
        frame,
        layout.status_line,
        &app.status_line_state,
        &app.theme,
        app.input.mode,
    );
}
```

**Note:** This will not compile until Task 8 adds `status_line_state` to `App`. For now, you can either:
- Add a temporary `pub status_line_state: StatusLineState` field to `App` with `Default::default()` in the constructor
- Or keep this as a planned change and commit the renderer module separately

**Step 3: Run `cargo check`**

If compile errors exist from `app.status_line_state`, add the field to `App` temporarily.

**Step 4: Commit**

```bash
git add src/ui/status_line.rs src/ui/mod.rs
git commit -m "feat(ui): add status line renderer with spinner and context usage"
```

---

### Task 7: Overhaul scroll system

**Files:**
- Modify: `src/ui/message_area.rs` (rewrite `MessageAreaState`)

**Step 1: Rewrite `MessageAreaState` with conventional coordinates**

Replace the `MessageAreaState` struct and its impl (lines 29–61 in current file) with:

```rust
/// State for the scrollable message area.
///
/// Coordinate system: `scroll_offset = 0` means top of content.
/// Auto-scroll sets `scroll_offset = max_scroll` (bottom of content).
/// This aligns with ratatui's `Paragraph::scroll((row, 0))` API.
pub struct MessageAreaState {
    /// Current scroll position (0 = top of content).
    pub scroll_offset: u16,
    /// Whether to automatically scroll to follow new content.
    pub auto_scroll: bool,
    /// Total content height from last render (used for clamping).
    content_height: u16,
    /// Visible area height from last render.
    visible_height: u16,
}

impl Default for MessageAreaState {
    fn default() -> Self {
        Self {
            scroll_offset: 0,
            auto_scroll: true,
            content_height: 0,
            visible_height: 0,
        }
    }
}

impl MessageAreaState {
    /// Maximum scroll offset (0 if content fits in view).
    pub fn max_scroll(&self) -> u16 {
        self.content_height.saturating_sub(self.visible_height)
    }

    /// Scroll toward older content (up).
    pub fn scroll_up(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
        self.auto_scroll = false;
    }

    /// Scroll toward newer content (down).
    pub fn scroll_down(&mut self, amount: u16) {
        let max = self.max_scroll();
        self.scroll_offset = self.scroll_offset.saturating_add(amount).min(max);
        if self.scroll_offset >= max {
            self.auto_scroll = true;
        }
    }

    /// Jump to the bottom (newest content). Re-enables auto-scroll.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.max_scroll();
        self.auto_scroll = true;
    }

    /// Update dimensions from render. If auto-scroll, jump to bottom.
    /// Clamp offset to valid range.
    pub fn update_dimensions(&mut self, content_height: u16, visible_height: u16) {
        self.content_height = content_height;
        self.visible_height = visible_height;
        let max = self.max_scroll();
        if self.auto_scroll {
            self.scroll_offset = max;
        } else {
            self.scroll_offset = self.scroll_offset.min(max);
        }
    }
}
```

**Step 2: Update `render_messages` auto-scroll logic**

In the `render_messages` function, replace the auto-scroll section (the lines that compute `content_height` and set `scroll_offset`) with a call to `update_dimensions`:

```rust
    // After computing content_height and visible_height:
    state.update_dimensions(content_height, visible_height);
```

Remove the old `if state.auto_scroll && content_height > visible_height` block.

**Step 3: Update mouse event handling in `app.rs`**

Swap the scroll direction mapping in `handle_event` (around line 261-263):

```rust
AppEvent::Input(Event::Mouse(mouse)) => match mouse.kind {
    // macOS natural scrolling: ScrollDown = swipe up = see older content
    MouseEventKind::ScrollDown => self.message_area_state.scroll_up(3),
    MouseEventKind::ScrollUp => self.message_area_state.scroll_down(3),
    _ => {}
},
```

**Step 4: Add tests for the new scroll system**

Add a test module at the bottom of `message_area.rs` (or in a new section):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_starts_at_zero_with_auto_scroll() {
        let state = MessageAreaState::default();
        assert_eq!(state.scroll_offset, 0);
        assert!(state.auto_scroll);
    }

    #[test]
    fn update_dimensions_auto_scrolls_to_bottom() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        assert_eq!(state.scroll_offset, 80); // max_scroll = 100 - 20
        assert!(state.auto_scroll);
    }

    #[test]
    fn scroll_up_disables_auto_scroll() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        assert_eq!(state.scroll_offset, 80);
        state.scroll_up(5);
        assert_eq!(state.scroll_offset, 75);
        assert!(!state.auto_scroll);
    }

    #[test]
    fn scroll_up_clamps_at_zero() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(200);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn scroll_down_to_bottom_re_enables_auto_scroll() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(30); // at 50 now
        assert!(!state.auto_scroll);
        state.scroll_down(30); // at 80 = max_scroll
        assert_eq!(state.scroll_offset, 80);
        assert!(state.auto_scroll);
    }

    #[test]
    fn scroll_down_clamps_at_max() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(10); // at 70
        state.scroll_down(200); // should clamp to 80
        assert_eq!(state.scroll_offset, 80);
    }

    #[test]
    fn update_dimensions_clamps_when_not_auto_scrolling() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(10); // at 70, auto_scroll = false
        // Content shrinks (e.g., after compact)
        state.update_dimensions(50, 20);
        // max_scroll = 30, so offset should clamp from 70 to 30
        assert_eq!(state.scroll_offset, 30);
        assert!(!state.auto_scroll);
    }

    #[test]
    fn max_scroll_zero_when_content_fits() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(10, 20);
        assert_eq!(state.max_scroll(), 0);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn scroll_to_bottom_works() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(50); // at 30
        state.scroll_to_bottom();
        assert_eq!(state.scroll_offset, 80);
        assert!(state.auto_scroll);
    }
}
```

**Step 5: Run tests**

Run: `cargo test ui::message_area`
Expected: All tests pass.

**Step 6: Run `cargo check`**

Verify the full project compiles with the new scroll system.

**Step 7: Commit**

```bash
git add src/ui/message_area.rs src/app.rs
git commit -m "fix(ui): overhaul scroll system with conventional coordinates, clamping, and direction fix"
```

---

### Task 8: Render `MessageBlock`s in the message area

This is the big rendering change. We add a **new** render function that works with `Vec<MessageBlock>` alongside the existing one, then swap in Phase 3.

**Files:**
- Modify: `src/ui/message_area.rs` (add `render_message_blocks` function)

**Step 1: Add the new render function**

This function takes `&[MessageBlock]` and renders them as styled `Line`s. Add this after the existing `render_messages` function:

```rust
use super::message_block::{MessageBlock, ToolGroupStatus};

/// Render structured message blocks into the given area.
pub fn render_message_blocks(
    frame: &mut Frame,
    area: Rect,
    messages: &[MessageBlock],
    state: &mut MessageAreaState,
    theme: &Theme,
    is_loading: bool,
) {
    let mut lines: Vec<Line> = Vec::new();

    for msg in messages {
        match msg {
            MessageBlock::User { text } => {
                for text_line in text.lines() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "> ",
                            Style::default()
                                .fg(theme.user_msg)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            text_line.to_string(),
                            Style::default().fg(theme.user_msg),
                        ),
                    ]));
                }
            }

            MessageBlock::Assistant { thinking, text, tool_groups } => {
                // Thinking block (collapsed by default)
                if let Some(t) = thinking {
                    if t.expanded {
                        lines.push(Line::from(Span::styled(
                            format!("▼ Thinking ({} tokens)", t.token_count),
                            Style::default().fg(theme.reasoning).add_modifier(Modifier::ITALIC),
                        )));
                        for content_line in t.content.lines() {
                            lines.push(Line::from(Span::styled(
                                format!("  {content_line}"),
                                Style::default().fg(theme.reasoning),
                            )));
                        }
                    } else {
                        lines.push(Line::from(Span::styled(
                            format!("▶ Thinking ({} tokens)", t.token_count),
                            Style::default().fg(theme.reasoning).add_modifier(Modifier::ITALIC),
                        )));
                    }
                }

                // Tool groups
                for group in tool_groups {
                    for call in &group.calls {
                        let status_indicator = match (&group.status, &call.result_summary) {
                            (_, Some(_)) if call.expanded => "▼",
                            (_, Some(_)) => "▶",
                            (ToolGroupStatus::Running { .. }, None) => "⠋",
                            (ToolGroupStatus::Preparing, None) => "⠋",
                            _ => "▶",
                        };

                        let result_part = match &call.result_summary {
                            Some(summary) => format!(" → {summary}"),
                            None => match &group.status {
                                ToolGroupStatus::Preparing => " preparing...".to_string(),
                                ToolGroupStatus::Running { .. } => " running...".to_string(),
                                ToolGroupStatus::Complete => String::new(),
                            },
                        };

                        let color = if call.is_error { theme.error } else { theme.tool_call };

                        lines.push(Line::from(Span::styled(
                            format!(
                                "{status_indicator} ⚡ {}({}){}",
                                call.tool_name, call.args_summary, result_part
                            ),
                            Style::default().fg(color),
                        )));

                        // Expanded output
                        if call.expanded {
                            if let Some(output) = &call.full_output {
                                for output_line in output.lines() {
                                    lines.push(Line::from(Span::styled(
                                        format!("  {output_line}"),
                                        Style::default().fg(theme.dim),
                                    )));
                                }
                            }
                        }
                    }
                }

                // Response text
                if text.is_empty() && is_loading {
                    lines.push(Line::from(Span::styled(
                        "...",
                        Style::default()
                            .fg(theme.dim)
                            .add_modifier(Modifier::DIM),
                    )));
                } else {
                    for text_line in text.lines() {
                        lines.push(Line::from(Span::styled(
                            text_line.to_string(),
                            Style::default().fg(theme.assistant_msg),
                        )));
                    }
                }
            }

            MessageBlock::System { text } => {
                for text_line in text.lines() {
                    lines.push(Line::from(Span::styled(
                        text_line.to_string(),
                        Style::default()
                            .fg(theme.dim)
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
            }

            MessageBlock::Error { text } => {
                for text_line in text.lines() {
                    lines.push(Line::from(Span::styled(
                        text_line.to_string(),
                        Style::default().fg(theme.error),
                    )));
                }
            }
        }

        // Blank line between messages
        lines.push(Line::from(""));
    }

    // Compute content height with wrapping
    let available_width = area.width.max(1) as usize;
    let content_height_u32: u32 = lines
        .iter()
        .map(|line| {
            let line_width: usize = line.width();
            if line_width == 0 {
                1u32
            } else {
                ((line_width + available_width - 1) / available_width) as u32
            }
        })
        .sum();
    let content_height = content_height_u32.min(u16::MAX as u32) as u16;
    let visible_height = area.height.saturating_sub(2);

    state.update_dimensions(content_height, visible_height);

    let block = Block::default()
        .borders(Borders::NONE)
        .style(Style::default().fg(theme.fg));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((state.scroll_offset, 0));

    frame.render_widget(paragraph, area);
}
```

**Step 2: Run `cargo check`**

Expected: Compiles (the new function exists alongside the old one).

**Step 3: Commit**

```bash
git add src/ui/message_area.rs
git commit -m "feat(ui): add render_message_blocks for structured MessageBlock rendering"
```

---

## Phase 3: State Migration

### Task 9: Migrate `App` state to `MessageBlock`

This is the big switch. Change `App.messages` from `Vec<DisplayMessage>` to `Vec<MessageBlock>`, add `status_line_state`, update `render()` to call the new renderer, and update all event handlers.

**Files:**
- Modify: `src/app.rs` — change `messages` field type, update all event handlers
- Modify: `src/ui/mod.rs` — switch to `render_message_blocks`
- Modify: `src/event.rs` — add `LlmReasoning` variant (for future use, wire now)

**This is a large task. Break it into sub-steps:**

**Step 1: Add `LlmReasoning` event variant**

In `src/event.rs`, add after `LlmDelta`:

```rust
    /// Reasoning/thinking tokens from the LLM.
    LlmReasoning { text: String },
```

**Step 2: Add `status_line_state` to `App`**

In `src/app.rs`, add to the `App` struct (in the UI state section):

```rust
    pub status_line_state: StatusLineState,
```

Import at the top:
```rust
use crate::ui::status_line::{Activity, StatusLineState};
```

In `App::new()` (or wherever `App` is constructed), add:
```rust
    status_line_state: StatusLineState::default(),
```

**Step 3: Change `messages` field type**

In `src/app.rs`, change:
```rust
    pub messages: Vec<DisplayMessage>,
```
to:
```rust
    pub messages: Vec<MessageBlock>,
```

Import at the top:
```rust
use crate::ui::message_block::MessageBlock;
```

Remove (or keep alongside for now) the `DisplayMessage` import.

**Step 4: Update event handlers one by one**

This is the critical migration. Each event handler that pushed `DisplayMessage` now mutates `MessageBlock`:

**`handle_input()` — user sends a message:**

Replace the display message pushes with:
```rust
    // Add user message to display
    self.messages.push(MessageBlock::User { text: text.clone() });

    // Add empty assistant block for streaming
    self.messages.push(MessageBlock::Assistant {
        thinking: None,
        text: String::new(),
        tool_groups: vec![],
    });
```

Set status line:
```rust
    self.status_line_state.activity = Activity::Thinking;
```

**`LlmDelta` handler:**

Replace the last-message text append with:
```rust
    if self.streaming_active {
        if let Some(last) = self.messages.last_mut() {
            last.append_text(&text);
        }
        // ... keep streaming_message append ...
        self.message_area_state.scroll_to_bottom();
    }
```

**`LlmReasoning` handler (new):**

```rust
    AppEvent::LlmReasoning { text } => {
        if self.streaming_active {
            if let Some(last) = self.messages.last_mut() {
                last.append_thinking(&text);
            }
            self.message_area_state.scroll_to_bottom();
        }
    }
```

**`LlmToolCallStreaming` handler:**

Replace the System "Preparing..." message logic with:
```rust
    AppEvent::LlmToolCallStreaming { count: _, tool_name } => {
        if let Some(last) = self.messages.last_mut() {
            last.ensure_preparing_tool_group();
        }
        self.status_line_state.activity = Activity::RunningTool {
            tool_name,
            args_summary: String::new(),
        };
        self.message_area_state.scroll_to_bottom();
    }
```

**`LlmToolCall` handler:**

Replace the Tool message push with:
```rust
    AppEvent::LlmToolCall { call_id: _, tool_name, arguments } => {
        // Extract a compact args summary for display
        let args_summary = extract_args_summary(tool_name, &arguments);
        if let Some(last) = self.messages.last_mut() {
            last.add_tool_call(tool_name, args_summary.clone());
        }
        self.status_line_state.activity = Activity::RunningTool {
            tool_name,
            args_summary,
        };
        self.message_area_state.scroll_to_bottom();
    }
```

You'll need a helper function `extract_args_summary`:
```rust
/// Extract a compact argument summary for display in tool call lines.
fn extract_args_summary(tool_name: ToolName, args: &Value) -> String {
    match tool_name {
        ToolName::Read => args.get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Grep => args.get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Glob => args.get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Edit | ToolName::Write | ToolName::Patch => args.get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Bash => {
            let cmd = args.get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // Truncate long bash commands
            if cmd.len() > 40 {
                format!("{}...", &cmd[..37])
            } else {
                cmd.to_string()
            }
        }
        ToolName::List => args.get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string(),
        ToolName::Question => args.get("text")
            .and_then(|v| v.as_str())
            .map(|s| if s.len() > 30 { format!("{}...", &s[..27]) } else { s.to_string() })
            .unwrap_or_default(),
        ToolName::Todo => "".to_string(),
        ToolName::Webfetch => args.get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    }
}
```

**`ToolResult` handler:**

Replace the ToolResult/Error message push + empty Assistant push with:
```rust
    AppEvent::ToolResult { call_id: _, tool_name, output } => {
        // UTF-8 safe truncation for summary
        let summary = if output.output.chars().count() > 80 {
            let truncated: String = output.output.chars().take(77).collect();
            format!("{truncated}...")
        } else {
            output.output.clone()
        };

        if let Some(last) = self.messages.last_mut() {
            last.complete_tool_call(
                tool_name,
                summary,
                output.output.clone(),
                output.is_error,
            );
        }

        self.update_sidebar();
        self.message_area_state.scroll_to_bottom();
    }
```

**Important:** Remove the old code that pushes a new empty `DisplayMessage::Assistant` after every `ToolResult`. The `MessageBlock::Assistant` block persists across the tool loop — the stream task will continue sending `LlmDelta` events that append to the same block's `text` field.

**`LlmFinish` handler:**

Update to clear status line:
```rust
    self.status_line_state.activity = Activity::Idle;
```

Also update the trailing-empty-assistant cleanup:
```rust
    if let Some(last) = self.messages.last() {
        if last.is_empty_assistant() {
            self.messages.pop();
        }
    }
```

**`LlmError` handler:**

```rust
    self.status_line_state.activity = Activity::Idle;
    self.messages.push(MessageBlock::Error { text: error });
```

**`PermissionRequest` handler:**

The permission prompt is now shown in the status line rather than as a message. But we still need the visual prompt in the message area for context:
```rust
    AppEvent::PermissionRequest(req) => {
        let summary = format!(
            "⚠ {}: {} — Allow? (y)es / (n)o / (a)lways",
            req.tool_name, req.arguments_summary
        );
        self.messages.push(MessageBlock::System { text: summary });
        self.pending_permission = Some(PendingPermission {
            tool_name: req.tool_name,
            summary: req.arguments_summary.clone(),
            response_tx: req.response_tx,
        });
        self.status_line_state.activity = Activity::WaitingForPermission;
        self.message_area_state.scroll_to_bottom();
    }
```

**Permission response handlers (y/n/a):**

After sending the response, reset status:
```rust
    self.status_line_state.activity = Activity::Thinking; // or RunningTool if we track it
```

And push system messages as `MessageBlock::System` instead of `DisplayMessage`:
```rust
    self.messages.push(MessageBlock::System {
        text: format!("✓ allowed: {}", tool_name),
    });
```

**`CompactFinish` handler:**

Replace `DisplayMessage` pushes with `MessageBlock::System`:
```rust
    self.messages.clear();
    self.messages.push(MessageBlock::System {
        text: "Conversation compacted.".to_string(),
    });
```

**`CompactError` handler:**

```rust
    self.messages.push(MessageBlock::Error { text: error });
    self.status_line_state.activity = Activity::Idle;
```

**`Tick` handler:**

Update to drive the spinner:
```rust
    AppEvent::Tick => {
        self.status_line_state.tick();
    }
```

**Step 5: Update `render()` in `src/ui/mod.rs`**

Switch from `render_messages` to `render_message_blocks`:

```rust
use message_area::render_message_blocks;

// In render():
    render_message_blocks(
        frame,
        layout.message_area,
        &app.messages,
        &mut app.message_area_state,
        &app.theme,
        app.is_loading,
    );
```

**Step 6: Update `update_sidebar`**

Update status line state alongside sidebar:
```rust
    if let Some(model) = &self.current_model {
        self.sidebar_state.model_name = model.clone();
        self.status_line_state.model_name = model.clone();
    }
    if let Some(session) = &self.current_session {
        self.status_line_state.total_tokens = session.token_usage.total_tokens;
    }
```

Also set `context_window` when a model is resolved (in the model-switching code or at stream start). Look for where `config.providers` resolves the model's `context_window` and set:
```rust
    self.status_line_state.context_window = model_config.context_window as u64;
```

**Step 7: Run `cargo check`**

Fix all remaining compile errors. The most likely issues:
- Old `DisplayMessage` references that weren't caught
- `render_messages` still being called (replace with `render_message_blocks`)
- Missing imports

**Step 8: Run `cargo test`**

Run: `cargo test`
Expected: All existing tests pass. The message_area tests may need updating if they referenced `DisplayMessage`.

**Step 9: Commit**

```bash
git add src/app.rs src/event.rs src/ui/mod.rs src/ui/message_area.rs
git commit -m "refactor(app): migrate from DisplayMessage to structured MessageBlock"
```

---

## Phase 4: Input and Interaction Improvements

### Task 10: Multi-line input with Shift+Enter

**Files:**
- Modify: `src/app.rs` — update `handle_key` for Shift+Enter

**Step 1: Update key handling**

In `handle_key`, before the `KeyCode::Enter` handler (around line 556), add a Shift+Enter check:

```rust
    // Shift+Enter: insert newline
    if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::SHIFT) {
        self.input.textarea.input(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ));
        return Ok(());
    }
```

This intercepts Shift+Enter before the plain Enter handler and forwards a bare Enter to tui-textarea, which inserts a newline.

**Note:** Some terminals send Shift+Enter as `KeyCode::Enter` with `SHIFT` modifier, others may send it differently. Test with your terminal. If `tui-textarea` doesn't handle plain Enter as a newline insert, you may need to call `textarea.insert_newline()` directly.

**Step 2: Test manually**

Run: `cargo run`
- Type text, press Shift+Enter — should insert a newline
- Press Enter — should submit

**Step 3: Commit**

```bash
git add src/app.rs
git commit -m "feat(input): support Shift+Enter for multi-line input"
```

---

### Task 11: Command autocomplete

**Files:**
- Create: `src/ui/autocomplete.rs`
- Modify: `src/ui/mod.rs` (add module, render overlay)
- Modify: `src/app.rs` (handle Tab key for autocomplete, render popup)

**Step 1: Create autocomplete state**

Create `src/ui/autocomplete.rs`:

```rust
//! Command autocomplete popup state and rendering.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
};

use crate::command::CommandInfo;
use super::theme::Theme;

/// State for the command autocomplete popup.
pub struct AutocompleteState {
    /// Whether the popup is currently visible.
    pub visible: bool,
    /// Matching commands.
    pub matches: Vec<CommandInfo>,
    /// Currently selected index.
    pub selected: usize,
}

impl Default for AutocompleteState {
    fn default() -> Self {
        Self {
            visible: false,
            matches: vec![],
            selected: 0,
        }
    }
}

impl AutocompleteState {
    /// Update matches based on current input prefix.
    /// Shows popup if there are matches, hides if none.
    pub fn update(&mut self, input: &str) {
        use crate::command::Command;
        if input.starts_with('/') && !input.contains(' ') {
            self.matches = Command::matching_commands(input);
            self.visible = !self.matches.is_empty();
            // Clamp selection
            if self.selected >= self.matches.len() {
                self.selected = 0;
            }
        } else {
            self.hide();
        }
    }

    /// Hide the popup.
    pub fn hide(&mut self) {
        self.visible = false;
        self.matches.clear();
        self.selected = 0;
    }

    /// Move selection down (wraps).
    pub fn next(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + 1) % self.matches.len();
        }
    }

    /// Move selection up (wraps).
    pub fn prev(&mut self) {
        if !self.matches.is_empty() {
            self.selected = if self.selected == 0 {
                self.matches.len() - 1
            } else {
                self.selected - 1
            };
        }
    }

    /// Get the selected command name.
    pub fn selected_command(&self) -> Option<&str> {
        self.matches.get(self.selected).map(|c| c.name)
    }
}

/// Render the autocomplete popup as an overlay above the input area.
pub fn render_autocomplete(
    frame: &mut Frame,
    input_area: Rect,
    state: &AutocompleteState,
    theme: &Theme,
) {
    if !state.visible || state.matches.is_empty() {
        return;
    }

    let item_count = state.matches.len().min(8) as u16; // max 8 visible
    let popup_height = item_count + 2; // +2 for borders
    let popup_width = 40u16.min(input_area.width);

    // Position above the input area
    let popup_area = Rect {
        x: input_area.x + 9, // offset past mode indicator
        y: input_area.y.saturating_sub(popup_height),
        width: popup_width,
        height: popup_height,
    };

    // Clear the area behind the popup
    frame.render_widget(Clear, popup_area);

    let items: Vec<ListItem> = state.matches.iter().enumerate().map(|(i, cmd)| {
        let style = if i == state.selected {
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        ListItem::new(Line::from(vec![
            Span::styled(format!("{:<12}", cmd.name), style),
            Span::styled(cmd.description, Style::default().fg(theme.dim)),
        ]))
    }).collect();

    let list = List::new(items)
        .block(Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border))
        );

    frame.render_widget(list, popup_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_shows_matches() {
        let mut state = AutocompleteState::default();
        state.update("/m");
        assert!(state.visible);
        assert!(state.matches.len() >= 2); // /model, /models
    }

    #[test]
    fn update_hides_on_no_match() {
        let mut state = AutocompleteState::default();
        state.update("/zzz");
        assert!(!state.visible);
    }

    #[test]
    fn update_hides_on_space() {
        let mut state = AutocompleteState::default();
        state.update("/model something");
        assert!(!state.visible);
    }

    #[test]
    fn next_wraps_around() {
        let mut state = AutocompleteState::default();
        state.update("/");
        let count = state.matches.len();
        for _ in 0..count {
            state.next();
        }
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn prev_wraps_around() {
        let mut state = AutocompleteState::default();
        state.update("/");
        state.prev();
        assert_eq!(state.selected, state.matches.len() - 1);
    }

    #[test]
    fn selected_command_returns_name() {
        let mut state = AutocompleteState::default();
        state.update("/e");
        assert_eq!(state.selected_command(), Some("/exit"));
    }
}
```

**Step 2: Register module and wire into rendering**

In `src/ui/mod.rs`, add `pub mod autocomplete;` and update `render()`:

```rust
use autocomplete::render_autocomplete;

// In render(), after render_input:
    render_autocomplete(
        frame,
        layout.input_area,
        &app.autocomplete_state,
        &app.theme,
    );
```

**Step 3: Wire into `App`**

Add to `App` struct:
```rust
    pub autocomplete_state: AutocompleteState,
```

In `App::new()`:
```rust
    autocomplete_state: AutocompleteState::default(),
```

**Step 4: Handle Tab key for autocomplete**

In `handle_key`, update the Tab handler:

```rust
    KeyCode::Tab if key.modifiers.is_empty() => {
        let current_text = self.input.textarea.lines().join("\n");
        if self.autocomplete_state.visible {
            // Tab cycles to next match
            self.autocomplete_state.next();
        } else if current_text.starts_with('/') {
            // First Tab: show autocomplete
            self.autocomplete_state.update(&current_text);
        } else {
            // No slash prefix: toggle mode (existing behavior)
            self.input.mode = self.input.mode.toggle();
            self.sync_permission_mode().await;
        }
    }
```

Handle Enter when autocomplete is visible (insert before the existing Enter handler):
```rust
    KeyCode::Enter if self.autocomplete_state.visible => {
        if let Some(cmd_name) = self.autocomplete_state.selected_command() {
            let cmd_name = cmd_name.to_string();
            // Replace input with selected command
            let mut textarea = TextArea::default();
            textarea.set_cursor_line_style(Style::default());
            textarea.set_placeholder_text("Type a message...");
            textarea.insert_str(&cmd_name);
            self.input.textarea = textarea;
            self.autocomplete_state.hide();
        }
    }
```

Handle Esc to dismiss:
```rust
    KeyCode::Esc if self.autocomplete_state.visible => {
        self.autocomplete_state.hide();
    }
```

After every keystroke that modifies the input (the `_ =>` catch-all), update autocomplete:
```rust
    _ => {
        self.input.textarea.input(key);
        // Update autocomplete if input starts with /
        let current_text = self.input.textarea.lines().join("\n");
        self.autocomplete_state.update(&current_text);
    }
```

**Step 5: Run tests**

Run: `cargo test ui::autocomplete`
Expected: All tests pass.

Run: `cargo check`
Expected: Compiles.

**Step 6: Commit**

```bash
git add src/ui/autocomplete.rs src/ui/mod.rs src/app.rs
git commit -m "feat(ui): add command autocomplete popup with Tab cycling"
```

---

### Task 12: Enable bracketed paste

**Files:**
- Modify: `src/ui/mod.rs`

**Step 1: Enable bracketed paste in terminal setup**

In `setup_terminal()`, add `EnableBracketedPaste`:

```rust
use crossterm::event::{EnableBracketedPaste, DisableBracketedPaste};

pub fn setup_terminal() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub fn restore_terminal(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;
    Ok(())
}
```

**Step 2: Run `cargo check`**

Expected: Compiles.

**Step 3: Commit**

```bash
git add src/ui/mod.rs
git commit -m "feat(ui): enable bracketed paste for better paste handling"
```

---

## Phase 5: Stream Integration

### Task 13: Capture reasoning tokens in stream task

**Files:**
- Modify: `src/stream.rs` — emit `LlmReasoning` events for reasoning content
- Modify: `src/event.rs` — already added `LlmReasoning` in Task 9

**Step 1: Research how the LLM API sends reasoning tokens**

Different providers send reasoning/thinking content in different fields:
- OpenAI o1/o3: `reasoning_content` field on the delta
- Some providers: `content` with a specific role or flag
- async-openai 0.32: Check `ChatChoiceDelta` for available fields

In the stream chunk processing loop (around lines 232–334 of `stream.rs`), look for where `choice.delta.content` is processed. Add handling for reasoning content.

The exact field depends on what async-openai 0.32 exposes. Common patterns:

```rust
// If async-openai has a reasoning_content field:
if let Some(reasoning) = &choice.delta.reasoning_content {
    event_tx.send(AppEvent::LlmReasoning { text: reasoning.clone() }).ok();
}
```

If the field doesn't exist in the library's types, we may need to handle it via a raw JSON approach or skip this for now and revisit when async-openai adds support.

**Step 2: Update event handler**

The `LlmReasoning` handler was already added in Task 9.

**Step 3: Run `cargo check`**

Expected: Compiles (the event variant exists, the handler exists, just need to emit from stream).

**Step 4: Commit**

```bash
git add src/stream.rs
git commit -m "feat(stream): emit LlmReasoning events for thinking token display"
```

---

## Phase 6: Cleanup

### Task 14: Remove old `DisplayMessage` and `DisplayRole` types

**Files:**
- Modify: `src/ui/message_area.rs` — remove `DisplayMessage`, `DisplayRole`, and `render_messages`

**Step 1: Remove the old types**

Delete the `DisplayMessage` struct, `DisplayRole` enum, and the `render_messages` function from `src/ui/message_area.rs`. Keep `MessageAreaState`, `render_message_blocks`, and the tests.

**Step 2: Search for any remaining references**

Run: `cargo check 2>&1`

Fix any remaining references to `DisplayMessage` or `DisplayRole` in other files.

**Step 3: Run all tests**

Run: `cargo test`
Expected: All tests pass.

**Step 4: Commit**

```bash
git add src/ui/message_area.rs
git commit -m "refactor(ui): remove old DisplayMessage/DisplayRole types"
```

---

### Task 15: Fix UTF-8 safe truncation in remaining places

**Files:**
- Modify: `src/app.rs` — check for any remaining byte-slice truncation

**Step 1: Search for byte-slice truncation**

Look for patterns like `&output[..N]` or `&text[..N]` that could panic on multi-byte chars.

**Step 2: Replace with char-based truncation**

Use the pattern:
```rust
fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() > max_chars {
        let truncated: String = s.chars().take(max_chars.saturating_sub(3)).collect();
        format!("{truncated}...")
    } else {
        s.to_string()
    }
}
```

**Step 3: Run `cargo test`**

**Step 4: Commit**

```bash
git add src/app.rs
git commit -m "fix: use UTF-8 safe truncation for tool result previews"
```

---

### Task 16: Final integration test

**Step 1: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

**Step 2: Run the application**

Run: `cargo run`

Manually verify:
- [ ] Status line shows at bottom with model name and token count
- [ ] Scroll direction feels natural (swipe up = older content)
- [ ] Scrolling doesn't go past content (clamped)
- [ ] Shift+click allows text selection in terminal
- [ ] Tool calls render as compact collapsed lines
- [ ] Typing `/` shows autocomplete popup
- [ ] Tab cycles through autocomplete matches
- [ ] Enter selects autocomplete match
- [ ] Shift+Enter inserts newline in input
- [ ] Enter submits input
- [ ] Spinner animates in status line during LLM streaming
- [ ] Permission prompts still work (y/n/a)

**Step 3: Final commit**

If any fixes were needed during manual testing, commit them.

```bash
git commit -m "feat: TUI redesign — structured messages, status line, scroll fixes, autocomplete"
```
