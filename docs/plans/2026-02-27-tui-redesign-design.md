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

## Resolved Questions

- Sidebar toggle: Ctrl+B (cycles auto → hide → show → auto)
- Expand/collapse state: persisted on MessageBlock structs
- Keyboard navigation between blocks: deferred

---

## Milestone 3: Aesthetic Refresh & UX Overhaul

**Date:** 2026-02-28
**Status:** Draft

### Problem

The TUI is functionally solid after Milestones 1–2, but aesthetically generic. Cyan-on-dark is the default for every TUI app (lazygit, bottom, gitui). Tool calls read like log lines. The sidebar shows bookkeeping instead of situational awareness. There's no visual distinction between read and write operations. The conversation is a scrolling wall with no spatial structure.

### Design Direction: "Warm Terminal"

Move from cold cyan to a **warm amber/gold** palette. Not retro cosplay — just warm, inviting, and instantly recognizable.

| Element | Current | Proposed |
|---------|---------|----------|
| Accent | Cyan | Amber/Gold (`#FFAA33` / `Color::Rgb(255, 170, 51)`) |
| UI chrome/borders | DarkGray | Warm gray (`Color::Rgb(88, 88, 88)`) |
| User messages | Blue | Soft warm white |
| Assistant text | White | Slightly warm off-white |
| Mutations (write/edit/patch) | Magenta | Coral/Orange — visually distinct from reads |
| Reads (grep/glob/read) | Magenta (same as writes!) | Muted/dim — background operations |
| Mode: Build | Green bg | Amber bg |
| Mode: Plan | Cyan bg | Cool blue bg |
| Major dividers | `─` DarkGray | `═` warm gray (double-line for emphasis) |

### A. Inline Diff Rendering for Mutations

When `edit`/`write`/`patch` completes, render a compact inline diff instead of raw tool output:

```
  ✎ edit src/main.rs
  ┌──────────────────────────────
  │ -use std::collections::HashMap;
  │ +use std::collections::BTreeMap;
  └──────────────────────────────
```

Green for additions, red for removals, framed in a subtle box. This is the single highest-impact UX change — it makes Steve feel like a *coding tool* rather than a chatbot that happens to edit files.

### B. Read vs Write Visual Distinction

Trivial but important: use different colors and symbols for read-only vs mutation tools.

- **Read ops** (read, grep, glob, list): dim color, subtle dot marker `·`
- **Write ops** (edit, write, patch): coral/orange, pen marker `✎`
- **Execute** (bash): accent color, terminal marker `$`
- **Interactive** (question, todo): accent color, current markers

This makes it immediately obvious when the agent is observing vs changing.

### C. Changeset Panel (Sidebar Redesign)

Replace the current sidebar content with a **live changeset view** as the primary section:

```
╔═ Changes ═══════════════╗
║                         ║
║  src/main.rs       +3 -1║
║  src/tool/mod.rs   +12  ║
║  Cargo.toml        +1   ║
║                         ║
║─────────────────────────║
║  3 files · +16 -1       ║
║                         ║
╠═ Session ═══════════════╣
║  Model: gpt-4o          ║
║  Ctx: 23.4k/128k (18%)  ║
║  Cost: $0.12             ║
╚═════════════════════════╝
```

Answers the question users actually have: **"what has the agent done to my codebase?"** Token info drops to a compact footer. Changeset tracks files touched by write tools this session. Reset on `/new`.

### D. Code Block Framing

When the assistant outputs fenced code blocks in its response text, detect them and render with a visible frame and language label:

```
  Here's the fix:

  ┌─ rust ──────────────────────────┐
  │ fn main() {                     │
  │     println!("Hello, world!");  │
  │ }                               │
  └─────────────────────────────────┘
```

#### Copy-text constraint (critical)

The frame characters (`│`, `┌`, `└`, `─`) are rendered by ratatui as part of the terminal output — they **cannot** appear in the selectable text region if we want clean copy-paste. Two strategies:

1. **Gutter-only framing**: Place the `│` border in a dedicated left gutter column (1-2 chars) that sits outside the content area. The language label and top/bottom rules render on their own lines. When the user Shift+click+drags to select code lines, the gutter column is outside the selection rectangle if the terminal supports rectangular selection (iTerm2 Alt+drag, Kitty). For standard line selection, the `│` will be included — but it's a single predictable character at column 0, trivially stripped.

2. **Background-color framing (preferred)**: Instead of box-drawing characters around code, use a **different background color** for code block lines (e.g., slightly lighter than the terminal background, `Color::Rgb(30, 30, 30)` on a black terminal). The language label renders as a styled prefix on the first line. No border characters at all — the background shift creates the visual "card" effect. Shift+click+drag copies pure code text with zero cleanup needed.

   ```
     Here's the fix:

      fn main() {                      ← these lines have a tinted background
          println!("Hello, world!");    ← visually distinct without border chars
      }

   ```

   The language label can render as a dim right-aligned tag on the first line of the block, or as a separate line above with the same background. Either way, no characters intrude into the copyable content.

**Decision**: Prefer strategy 2 (background-color framing). It gives a clean visual card effect and preserves perfect copy-paste. Falls back gracefully on terminals that don't support RGB colors (the code still renders, just without the background tint).

**Remaining limitation**: Background-color framing solves intra-block copy cleanliness, but terminal text selection is row-based — when the sidebar is visible, shift-drag copies sidebar content (token counters, changeset) on the same rows as code. Ctrl+B hides the sidebar as a workaround. See section I (OSC 52 Clipboard Copy) for a proper solution that bypasses terminal selection entirely.

### E. Activity Rail (Left Gutter)

Add a narrow left gutter (3-4 cols) to the message area that shows a vertical activity timeline:

```
 │   Assistant: I'll fix the import...
 ├── · read src/main.rs
 ├── · grep "use std"
 ├── ✎ edit src/main.rs        ← mutation: highlighted in coral
 │
 │   Done. I updated the import to use...
```

- Reads are dim markers (background operations)
- Writes are bold/colored marks (mutations are what matter)
- The conversation text stays clean — tool output doesn't interleave with prose
- Select a rail entry to expand and see full output

This separates "what the agent said" from "what the agent did" spatially.

### F. Intent Indicators

Before each assistant turn, show a contextual label derived from which tools the agent calls:

```
  ── exploring ──────────────────
  I'll look at the current implementation...

  ── editing ────────────────────
  Let me fix that import and update the tests.
```

Heuristic: if the agent only calls read/grep/glob → "exploring." If it calls edit/write/patch → "editing." If it calls bash with test-like commands → "testing." Inferred after the turn completes, or updated live as tools execute.

### G. Permission Prompts with Diff Preview

Render permission prompts as contextual cards showing *what will change*:

```
  ┌─ Allow? ──────────────────────────┐
  │  ✎ edit  src/main.rs:42           │
  │                                    │
  │  -use std::collections::HashMap;   │
  │  +use std::collections::BTreeMap;  │
  │                                    │
  │  [y]es  [n]o  [a]lways    Esc=deny│
  └────────────────────────────────────┘
```

For `bash`, show the command. For `write`, show a file-size summary. The user sees the actual impact, not just the tool name and args.

### H. Ambient Context Pressure

Shift border color as context pressure increases:

- <40%: Normal warm gray borders
- 40–60%: Slightly warmer tint
- 60–80%: Yellow borders (warning zone)
- 80%+: Red/orange borders (auto-compact imminent)

The entire UI "feels" different as you approach limits — visceral, not just a number.

### I. OSC 52 Clipboard Copy

Terminal text selection is fundamentally row-based — shift-drag always grabs the full terminal width, including sidebar content on the same rows. No amount of character tricks or background-color framing can prevent this. The proper solution is to bypass terminal selection entirely.

**OSC 52** (`\x1b]52;c;<base64-data>\x07`) is a terminal escape sequence that writes directly to the system clipboard. Most modern terminals support it: iTerm2, WezTerm, Alacritty, Kitty, Ghostty, foot, Windows Terminal. It works over SSH and tmux (with `set -g set-clipboard on`).

**Feature: keyboard-driven code block copy**

Add a keybinding (e.g., `Ctrl+Y`) that copies the most recently rendered code block to the clipboard via OSC 52:

1. Track the "last code block" content in `App` state — updated whenever `render_text_with_code_blocks` processes a fenced block, storing the raw text content (no UI decoration, no language label, no background)
2. On `Ctrl+Y`, base64-encode the content and emit `\x1b]52;c;{base64}\x07` to stdout
3. Show a brief system message: "Copied N lines to clipboard"

```
> Show me the main function

  rust
  fn main() {
      println!("Hello, world!");
  }

  Here's the main function. It prints a greeting.

[Ctrl+Y]
⟡ Copied 3 lines to clipboard
```

**Advantages over terminal selection**:
- Copies clean code without sidebar content, line numbers, or UI decoration
- Works even when sidebar is visible
- No shift-drag coordination needed
- Content is always the raw code, not what's visually rendered

**Edge cases**:
- Multiple code blocks: copy the last one by default, or add `Ctrl+Shift+Y` to enter a "pick which block" mode
- No code blocks visible: show "No code block to copy" system message
- Terminal doesn't support OSC 52: content isn't written to clipboard, but the operation is silent (no crash). Could detect support via OSC 52 query mode and fall back to a "copy not supported" message
- tmux: requires `set -g set-clipboard on` in tmux config

**Implementation sketch**:
- New `App` field: `last_code_block: Option<String>` — raw content of most recent code block
- Updated in `render_text_with_code_blocks` (or a parallel extraction pass) to capture content between opening and closing fences
- Keybinding handler: base64-encode → write OSC 52 → show confirmation
- No new dependencies needed (`base64` encoding is trivial, or use the `base64` crate)

### Implementation Priority

Ordered by impact-to-effort ratio:

| # | Item | Effort | Impact |
|---|------|--------|--------|
| 1 | Color palette swap (Warm Terminal) | Low — theme.rs only | Instant identity |
| 2 | Read vs write distinction (B) | Low — theme.rs + message_area.rs | Clarity |
| 3 | Inline diff rendering (A) | Medium — tool output parsing | "This is a coding tool" moment |
| 4 | Code block framing with bg color (D) | Medium — markdown detection in render | Visual polish + clean copy |
| 5 | Changeset panel (C) | Medium — sidebar rewrite + file tracking | Situational awareness |
| 6 | Ambient context pressure (H) | Low — border color logic | Polish |
| 7 | Intent indicators (F) | Low — heuristic + render label | Contextual clarity |
| 8 | OSC 52 clipboard copy (I) | Low — keybinding + escape sequence | Clean copy without sidebar |
| 9 | Activity rail (E) | High — message_area layout refactor | Spatial separation |
| 10 | Permission diff preview (G) | High — data through permission channel | Informed decisions |
