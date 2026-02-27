# TUI Redesign Design

**Date:** 2026-02-27
**Status:** Draft

## Problem

Steve's TUI has several UX pain points that compound into a rough experience:

1. **Scroll direction is inverted** on macOS (natural scrolling swaps the events)
2. **Can't copy text** — mouse capture suppresses native terminal selection, with no internal alternative
3. **Paste and multi-line input** — no way to enter multi-line messages (Enter always submits)
4. **Tool call noise** — each tool call produces 3+ display messages (Preparing, call, result), overwhelming the conversation
5. **No status feedback** — only a "..." placeholder indicates activity; no spinner, no progress, no context window usage
6. **Thinking tokens discarded** — models that emit reasoning content produce no visible output
7. **Flat message model** — `DisplayMessage { role, text }` can't represent grouped tool calls, collapsible sections, or structured turns

## Approach

Rearchitect the message model and layout rather than patching the existing flat system. Since no users depend on the current behavior, this is the right time to get the foundation right.

## Design

### 1. Layout Architecture

4-region layout with toggle-able sidebar:

```
+---------------------------------------------------+-----------+
|                                                    |           |
|                  Message Area                      | Sidebar   |
|  (scrollable, structured message blocks)           | (toggle-  |
|                                                    |  able)    |
+---------------------------------------------------+           |
| [Build] | input area (Shift+Enter for newlines)    |           |
|          +-------------------+                     |           |
|          | /compact          | <- autocomplete     |           |
|          | /clear            |    popup (overlay)   |           |
|          +-------------------+                     |           |
+---------------------------------------------------+-----------+
| spinner Activity text               model | tokens | ctx% | mode |
+---------------------------------------------------------------+
```

- **Message area**: scrollable, renders `MessageBlock`s
- **Input area**: adjacent to messages (above status line), `[Mode]` indicator on left, textarea on right
- **Command autocomplete**: overlay popup when input starts with `/`, Tab cycles, Enter selects
- **Status line**: 1-row footer spanning full width (below sidebar too)
- **Sidebar**: toggle-able with keybinding, shows session info, todos; hidden by default when terminal < 120 wide

Layout constants:
- `STATUS_HEIGHT: u16 = 1`
- `INPUT_HEIGHT: u16 = 3` (grows to max ~8 for multi-line content)
- `SIDEBAR_WIDTH: u16 = 40`
- `SIDEBAR_MIN_TERMINAL_WIDTH: u16 = 120`

### 2. Message Model

Replace flat `Vec<DisplayMessage>` with structured `Vec<MessageBlock>`:

```rust
enum MessageBlock {
    /// User's input message
    User { text: String },

    /// Assistant response with optional structured sub-parts
    Assistant {
        /// Collapsed thinking indicator
        thinking: Option<ThinkingBlock>,
        /// The actual response text
        text: String,
        /// Tool activity that occurred during this response turn
        tool_groups: Vec<ToolGroup>,
    },

    /// System notification
    System { text: String },

    /// Error message
    Error { text: String },
}

struct ThinkingBlock {
    token_count: usize,
    content: String,
    expanded: bool,  // UI state
}

struct ToolGroup {
    calls: Vec<ToolCall>,
    status: ToolGroupStatus,
}

struct ToolCall {
    tool_name: ToolName,
    args_summary: String,
    full_output: Option<String>,
    result_summary: Option<String>,
    is_error: bool,
    expanded: bool,  // UI state
}

enum ToolGroupStatus {
    Preparing,
    Running { current_tool: ToolName },
    Complete,
}
```

#### Rendering

Collapsed (default):
```
> User message here

▶ Thinking (42 tokens)
▶ ⚡ read(src/main.rs) → 150 lines
▶ ⚡ glob(**/*.rs) → 12 files
Here's what I found in the code...
```

Expanded (on click or keyboard toggle):
```
▼ ⚡ read(src/main.rs) → 150 lines
  use std::io::{self, Stdout};
  use anyhow::Result;
  ...
```

#### Event Mapping

Events update the current `MessageBlock::Assistant` in-place:
- `LlmToolCallStreaming` → upsert into `tool_groups` with `Preparing` status
- `LlmToolCall` → update group to `Running`
- `ToolResult` → update call's `result_summary` and `full_output`, mark group `Complete`
- `LlmDelta` → append to `text`
- `LlmReasoning` (new event) → accumulate in `thinking`

Permission prompts render inline within the tool activity section:
```
▶ ⚡ read(src/main.rs) → 150 lines
⚠ bash: cargo test — Allow? (y/n/a)
```

### 3. Scrolling and Mouse

**Mouse capture: keep enabled** for click and scroll support. Native text selection works via Shift+click/drag (standard terminal convention — iTerm2, Ghostty, Kitty, WezTerm, Alacritty all support this).

**Scroll direction fix:**
Swap `ScrollUp`/`ScrollDown` handler mapping. macOS natural scrolling inverts the events at the OS level; crossterm passes them through. Swapping aligns with user expectation: physical "swipe up" → see older content.

**Scroll coordinate system:**
Switch from inverted (0 = bottom) to conventional (0 = top). Auto-scroll sets `scroll_offset = max_scroll`. Aligns with ratatui's `Paragraph::scroll((row, 0))` API.

**Scroll clamping:**
After computing content height, enforce `scroll_offset = min(scroll_offset, max_scroll)`. No more scrolling into empty space.

**Auto-scroll:**
- Re-enabled when user explicitly scrolls to the bottom (offset reaches `max_scroll`)
- Disabled when user scrolls up at all
- During streaming with auto-scroll on, offset is set to `max_scroll` each render

**Expand/collapse interaction:**
- Phase 1: Mouse click on `▶`/`▼` markers toggles `expanded` on the corresponding `ToolCall` or `ThinkingBlock`
- Phase 2 (follow-up): Keyboard navigation — j/k moves a cursor between expandable blocks, Enter/Space toggles

### 4. Status Line

1-row footer spanning full terminal width.

**Left side:** Spinner + activity
- Braille spinner (`⠋⠙⠹⠸⠼⠴⠦⠧`), advances on 100ms tick
- Activity labels: "Thinking..." | "Running {tool}({args})" | "Waiting for permission..." | empty when idle
- Styled in accent color

**Right side:** Static info, pipe-separated
- Model ref (e.g., `gpt-4o`)
- Token usage with context window: `12.4k/128k (10%)`
  - Dim when < 50%, yellow at 50-79%, red at >= 80%
- Agent mode (`Build`/`Plan`, colored)

### 5. Input Improvements

**Multi-line input:**
- `Shift+Enter` inserts a newline
- `Enter` submits (unchanged)
- Input area height grows from 3 to max ~8 lines, then textarea scrolls internally

**Command autocomplete:**
- Triggered when input starts with `/`
- Overlay popup rendered above input area (ratatui `Clear` + bordered list)
- Tab cycles through filtered matches, Enter selects, Esc dismisses
- Each command shows name + short description

**Paste:**
- Enable `crossterm::event::EnableBracketedPaste` explicitly
- tui-textarea handles bracket paste events natively

### 6. Fixes and Polish

1. **UTF-8 safe truncation** — `.chars().take(n).collect::<String>()` for tool result previews
2. **Scroll clamping** — enforce offset <= max_scroll on every render
3. **Remove "Preparing" system messages** — handled by `ToolGroupStatus::Preparing` within the message block
4. **Spinner on tick** — 100ms tick drives braille spinner rotation in status line
5. **Reasoning token capture** — new `AppEvent::LlmReasoning { text }` event from stream task, accumulated into `ThinkingBlock`
6. **Auto-scroll correctness** — only re-enable at true bottom, not just when offset math reaches 0

### 7. New Dependencies

- None strictly required for Phase 1
- If Shift+Enter detection requires explicit bracketed paste, `crossterm::event::EnableBracketedPaste` is already available in crossterm 0.28
- Spinner implementation is hand-rolled (static array of braille chars)

## Open Questions

- Exact keybinding for sidebar toggle (Ctrl+B? F2? backslash?)
- Keyboard navigation between message blocks (j/k) — deferred to Phase 2
- Whether to persist expand/collapse state across re-renders (yes — state lives on the MessageBlock)
