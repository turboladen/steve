# Implementation Plan

Remaining work items. TUI redesign items (8–13) archived from
`docs/plans/2026-02-27-tui-redesign-design.md`. Items 14–15 from `docs/vision.md` Milestone 3 audit.
Milestones 4–5 (agent intelligence, ecosystem integration) remain in `docs/vision.md` as longer-term
roadmap items.

## Priority Order

| #  | Item                                    | Effort | Impact                                       |
| -- | --------------------------------------- | ------ | -------------------------------------------- |
| 8  | OSC 52 clipboard copy                   | Low    | Clean copy without sidebar                   |
| 9  | Activity rail                           | High   | Spatial separation of prose vs tool activity |
| 10 | Permission diff preview                 | High   | Informed permission decisions                |
| 11 | Keyboard scrolling (Up/Down, PgUp/PgDn) | Low    | Navigate messages without mouse              |
| 12 | `/models` interactive picker overlay    | Medium | Discoverable model switching                 |
| 13 | OSC 10/11 terminal-adaptive theme       | Medium | Correct colors in light terminals            |
| 14 | Binary file detection in `read` tool    | Low    | Prevents garbled output / wasted tokens      |
| 15 | `ropey`-based text editing              | Medium | Reliable multi-edit, UTF-8 safety            |

---

## 8. OSC 52 Clipboard Copy

Bypass terminal text selection entirely. Terminal selection is row-based — shift-drag always grabs
sidebar content on the same rows. OSC 52 (`\x1b]52;c;<base64-data>\x07`) writes directly to the
system clipboard.

**Keybinding:** `Ctrl+Y` copies the most recently rendered code block.

**Flow:**

1. Track `last_code_block: Option<String>` in `App` — updated when `render_text_with_code_blocks`
   processes a fenced block (raw text, no UI decoration)
2. On `Ctrl+Y`, base64-encode content and emit `\x1b]52;c;{base64}\x07` to stdout
3. Show system message: "Copied N lines to clipboard"

**Edge cases:**

- Multiple code blocks: copy the last one by default; consider `Ctrl+Shift+Y` to pick
- No code blocks visible: "No code block to copy" system message
- Terminal doesn't support OSC 52: silent no-op (no crash)
- tmux: requires `set -g set-clipboard on`

**Supported terminals:** iTerm2, WezTerm, Alacritty, Kitty, Ghostty, foot, Windows Terminal.

**Dependencies:** None (`base64` encoding is trivial or use `base64` crate).

---

## 9. Activity Rail (Left Gutter)

Add a narrow left gutter (3-4 cols) to the message area showing a vertical activity timeline:

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
- Conversation text stays clean — tool output doesn't interleave with prose
- Select a rail entry to expand and see full output

Separates "what the agent said" from "what the agent did" spatially.

**Effort note:** Requires message_area layout refactor — the gutter is a new column alongside
existing content.

---

## 10. Permission Prompts with Diff Preview

Render permission prompts as contextual cards showing _what will change_:

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

For `bash`, show the command. For `write`, show a file-size summary. The user sees the actual
impact, not just the tool name and args.

**Effort note:** Requires threading diff data through the permission channel (`PermissionRequest`).
Currently the stream task has the tool call arguments when it sends the permission request, but that
data doesn't flow to the UI's permission rendering.

---

## 11. Keyboard Scrolling (Up/Down, PageUp/PageDown)

_Originally promised in PLAN.md Phase 1 key bindings but never implemented._

Currently only mouse wheel scrolling works. Add keyboard-based message area scrolling:

| Key      | Action               |
| -------- | -------------------- |
| Up       | Scroll up one line   |
| Down     | Scroll down one line |
| PageUp   | Scroll up one page   |
| PageDown | Scroll down one page |

**Implementation:** Handle `KeyCode::Up`, `KeyCode::Down`, `KeyCode::PageUp`, `KeyCode::PageDown` in
`app.rs` `handle_key()`. Route to existing `scroll_up()` / `scroll_down()` methods. PageUp/PageDown
should scroll by the visible message area height.

**Consideration:** Up/Down arrows conflict with tui-textarea cursor movement when the input is
focused. Only scroll when the input area doesn't need the keys (e.g., input is empty or a modifier
indicates scroll intent). Alternatively, always forward to scroll since tui-textarea is
single-line-send (Enter submits).

**Effort:** Low — the scroll infrastructure already exists.

---

## 12. `/models` Interactive Picker Overlay

_PLAN.md Phase 10 promised a "picker overlay" but the current implementation just prints a text
list._

Replace the text-list `/models` output with a floating overlay widget (similar to the autocomplete
popup) that lets the user arrow-key through available models and press Enter to select.

**Sketch:**

```
┌─ Models ──────────────────────┐
│  ● openai/gpt-4o        ◄    │
│    openai/gpt-4o-mini         │
│    anthropic/claude-sonnet    │
│                               │
│  Enter=select  Esc=cancel     │
└───────────────────────────────┘
```

**Implementation:**

- Add `ModelPicker` state to `App` (visible flag, selected index, filtered model list)
- Render as a centered floating `Paragraph` or `List` widget with `Clear` background
- Arrow keys navigate, Enter selects and calls the existing model-switch logic, Esc dismisses

**Effort:** Medium — similar pattern to autocomplete popup but needs its own state and key handling
mode.

---

## 13. OSC 10/11 Terminal-Adaptive Theme

_PLAN.md Phase 10 promised "Terminal-adaptive theme via ANSI OSC 10/11" but the theme is hardcoded
RGB._

Query the terminal's background color at startup using OSC 10/11 escape sequences to detect light vs
dark terminals, then select the appropriate color palette.

**How OSC 10/11 works:**

1. Emit `\x1b]11;?\x07` (query background color)
2. Terminal responds with `\x1b]11;rgb:RRRR/GGGG/BBBB\x07`
3. Parse the RGB values; compute luminance to determine light vs dark

**Implementation:**

- Add `Theme::light()` palette (dark text on light background)
- At startup (before entering raw mode or immediately after), send the OSC query and read the
  response with a short timeout
- If response received: pick `dark()` or `light()` based on luminance
- If no response (terminal doesn't support it): default to `dark()`

**Edge cases:**

- tmux/screen may not forward OSC responses — timeout gracefully
- Some terminals return `rgba:` format — handle both
- Query must happen before or immediately after entering raw mode

**Effort:** Medium — the palette work is the bulk; the OSC query itself is ~20 lines.

**Dependencies:** None (raw escape sequence I/O).

---

## 14. Binary File Detection in `read` Tool

_Vision doc Milestone 3 item: "Language-aware file read — detect binary files."_

`read.rs` currently calls `std::fs::read_to_string()` directly. If the LLM asks to read a binary
file (compiled output, images, `.wasm`, etc.), the result is garbled text that wastes context tokens
and confuses the model.

**Implementation:**

1. Read the first 8KB of the file as raw bytes
2. Check for null bytes (`\0`) — presence indicates binary
3. If binary, return early: `"Binary file (N bytes), not displayed"` with `is_error: false`
4. Otherwise, convert to string and proceed as normal

**Edge cases:**

- Empty files: not binary (current behavior is fine)
- UTF-8 with BOM: still valid text
- Files with embedded nulls that are technically text (rare, acceptable false positive)

**Optional enhancement:** Use file extension heuristics (`.png`, `.o`, `.wasm` → skip entirely
without reading) as a fast path before byte inspection.

**Effort:** Low — ~10 lines added to `read.rs`.

**Dependencies:** None.

---

## 15. `ropey`-Based Text Editing

_Vision doc Milestone 3 item: "Text editing via ropey — rope-based in-memory buffer for
edit/write/patch operations."_

Currently `edit.rs` uses `String::replacen()` for find-and-replace. This works for single edits but
has limitations:

- No efficient line indexing (line-range operations require scanning from the start)
- Multi-edit within the same file requires careful offset tracking
- Large files are fully loaded into a single `String`

**Implementation:**

- Replace `String`-based file manipulation in `edit.rs`, `write.rs`, and `patch.rs` with
  `ropey::Rope`
- Use `Rope::from_reader()` for efficient loading
- Line-indexed access for range operations (read tool could also benefit)
- Multi-edit batching: apply edits in reverse-offset order to avoid position shifts

**When to do this:** When string-based editing hits real problems. The current approach works for
the MVP use case (single find-and-replace per tool call). This is an optimization for robustness,
not a blocker.

**Effort:** Medium — touches three tool files and needs careful testing.

**Dependencies:** Add `ropey` crate.
