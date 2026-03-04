# Implementation Plan

Remaining TUI redesign items. Completed items archived in `docs/plans/2026-02-27-tui-redesign-design.md`.

## Priority Order

| # | Item | Effort | Impact |
|---|------|--------|--------|
| 8 | OSC 52 clipboard copy | Low | Clean copy without sidebar |
| 9 | Activity rail | High | Spatial separation of prose vs tool activity |
| 10 | Permission diff preview | High | Informed permission decisions |

---

## 8. OSC 52 Clipboard Copy

Bypass terminal text selection entirely. Terminal selection is row-based — shift-drag always grabs sidebar content on the same rows. OSC 52 (`\x1b]52;c;<base64-data>\x07`) writes directly to the system clipboard.

**Keybinding:** `Ctrl+Y` copies the most recently rendered code block.

**Flow:**
1. Track `last_code_block: Option<String>` in `App` — updated when `render_text_with_code_blocks` processes a fenced block (raw text, no UI decoration)
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

**Effort note:** Requires message_area layout refactor — the gutter is a new column alongside existing content.

---

## 10. Permission Prompts with Diff Preview

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

**Effort note:** Requires threading diff data through the permission channel (`PermissionRequest`). Currently the stream task has the tool call arguments when it sends the permission request, but that data doesn't flow to the UI's permission rendering.
