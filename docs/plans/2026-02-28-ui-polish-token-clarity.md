# Milestone 2: UI Polish & Token Clarity — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix scroll direction, separate cost tracking from context pressure display, add incremental token updates during tool loops, and add visual borders between UI areas.

**Architecture:** Four independent fixes targeting the event pipeline (new `LlmUsageUpdate` event), status line state (new `last_prompt_tokens` field), input/sidebar rendering (split token display semantics), and scroll direction (swap mapping). All changes are backward-compatible with no data migrations.

**Tech Stack:** Rust, ratatui 0.29, crossterm 0.28, async-openai 0.32

---

## Task 1: Fix Inverted Scroll Direction

**Why:** Terminal emulators on macOS already apply natural scrolling — our code double-inverts by swapping ScrollDown/ScrollUp. Users report scrolling feels backwards.

**Files:**
- Modify: `src/app.rs:352-354`
- Modify: `src/app.rs` tests (if scroll tests exist)

### Step 1: Write failing test

In `src/app.rs`, in the `#[cfg(test)]` module, add:

```rust
#[test]
fn scroll_down_event_scrolls_down() {
    // ScrollDown event (from terminal) should call scroll_down (show newer/lower content)
    let mut state = crate::ui::message_area::MessageAreaState::default();
    state.update_dimensions(100, 500); // content taller than viewport
    state.scroll_to_bottom();
    let initial = state.scroll_offset;

    // Scroll up first to have room to scroll down
    state.scroll_up(10);
    let after_up = state.scroll_offset;
    assert!(after_up < initial, "scroll_up should decrease offset");

    // Now scroll down should increase offset (toward bottom)
    state.scroll_down(3);
    assert!(state.scroll_offset > after_up, "scroll_down should increase offset");
}
```

### Step 2: Run test — verify it passes (validates scroll_up/scroll_down semantics)

Run: `cargo test scroll_down_event`
Expected: PASS (this tests the state methods, not the event mapping — confirms our understanding)

### Step 3: Fix the scroll direction mapping

In `src/app.rs`, find lines 352-354:

```rust
// macOS natural scrolling: ScrollDown = swipe up = see older content
MouseEventKind::ScrollDown => self.message_area_state.scroll_up(3),
MouseEventKind::ScrollUp => self.message_area_state.scroll_down(3),
```

Replace with:

```rust
MouseEventKind::ScrollDown => self.message_area_state.scroll_down(3),
MouseEventKind::ScrollUp => self.message_area_state.scroll_up(3),
```

Remove the comment about macOS natural scrolling — the terminal emulator handles that.

### Step 4: Run tests

Run: `cargo test`
Expected: All pass

### Step 5: Commit

```bash
git add src/app.rs
git commit -m "fix: correct scroll direction — terminal handles natural scrolling"
```

---

## Task 2: Rework Token Display (Context Pressure vs Cost Tracking)

**Why:** Both input bar and sidebar show the same cumulative `total_tokens`. The user needs two different signals: (1) input bar = "how close am I to blowing the context window" (last prompt_tokens / context_window), (2) sidebar = "how much have I spent" (cumulative in/out/total). Currently the input bar shows cumulative total which exceeds the context window and is confusing.

**Files:**
- Modify: `src/ui/status_line.rs:27-38` (add `last_prompt_tokens` field)
- Modify: `src/ui/status_line.rs:88-94` (`context_usage_pct` uses new field)
- Modify: `src/ui/input.rs:39-43` (InputContext uses `last_prompt_tokens`)
- Modify: `src/ui/input.rs:136-158` (context line rendering)
- Modify: `src/app.rs:941-969` (`update_sidebar` syncs new field)
- Modify: `src/event.rs:56-62` (StreamUsage — no change needed, already has prompt_tokens)

### Step 1: Write failing tests for StatusLineState

In `src/ui/status_line.rs` tests, add:

```rust
#[test]
fn context_usage_pct_uses_last_prompt_tokens() {
    let state = StatusLineState {
        activity: Activity::Idle,
        spinner_frame: 0,
        model_name: "test".to_string(),
        total_tokens: 500_000, // cumulative — should NOT be used for pct
        context_window: 128_000,
        last_prompt_tokens: 80_000, // this is what matters — 62.5%
    };
    assert_eq!(state.context_usage_pct(), 63); // ceil of 62.5
}

#[test]
fn context_usage_pct_zero_last_prompt() {
    let state = StatusLineState {
        activity: Activity::Idle,
        spinner_frame: 0,
        model_name: "test".to_string(),
        total_tokens: 100_000,
        context_window: 128_000,
        last_prompt_tokens: 0,
    };
    assert_eq!(state.context_usage_pct(), 0);
}
```

### Step 2: Run tests — expect failure

Run: `cargo test context_usage_pct_uses`
Expected: FAIL — `last_prompt_tokens` field doesn't exist

### Step 3: Add `last_prompt_tokens` to StatusLineState

In `src/ui/status_line.rs`, add to the struct (line ~37):

```rust
pub struct StatusLineState {
    pub activity: Activity,
    pub spinner_frame: usize,
    pub model_name: String,
    pub total_tokens: u64,
    pub context_window: u64,
    /// Last-reported prompt tokens from the API (current context window usage).
    pub last_prompt_tokens: u64,
}
```

Update the `Default` impl to include `last_prompt_tokens: 0`.

Update `context_usage_pct()` (line ~88):

```rust
pub fn context_usage_pct(&self) -> u8 {
    if self.context_window == 0 {
        0
    } else {
        ((self.last_prompt_tokens as f64 / self.context_window as f64) * 100.0)
            .min(100.0) as u8
    }
}
```

Fix any existing tests that construct `StatusLineState` directly — add `last_prompt_tokens: 0` (or appropriate values).

### Step 4: Run tests — expect pass

Run: `cargo test status_line`

### Step 5: Update InputContext to use last_prompt_tokens

In `src/ui/input.rs`, change `InputContext` (line 39):

```rust
pub struct InputContext {
    pub working_dir: String,
    pub last_prompt_tokens: u64,
    pub context_window: u64,
}
```

Update the context line rendering (around line 136) — change `context.total_tokens` to `context.last_prompt_tokens`:

```rust
let pct = if context.context_window == 0 {
    0u8
} else {
    ((context.last_prompt_tokens as f64 / context.context_window as f64) * 100.0)
        .min(100.0) as u8
};
// ...
right_spans.push(Span::styled(
    format!(
        "{}/{} ({}%)",
        format_tokens(context.last_prompt_tokens),
        format_tokens(context.context_window),
        pct,
    ),
    Style::default().fg(token_color),
));
```

Also update the fallback (around line 161):
```rust
} else if context.last_prompt_tokens > 0 {
    right_spans.push(Span::styled(
        format_tokens(context.last_prompt_tokens),
        Style::default().fg(theme.dim),
    ));
}
```

### Step 6: Update InputContext construction in ui/mod.rs

Find where `InputContext` is built (in `src/ui/mod.rs`, around line 86):

```rust
let context = InputContext {
    working_dir: abbreviate_path(&app.project.root),
    last_prompt_tokens: app.status_line_state.last_prompt_tokens,
    context_window: app.status_line_state.context_window,
};
```

### Step 7: Sync last_prompt_tokens in update_sidebar

In `src/app.rs` `update_sidebar()` method (line ~964), add after the existing status_line_state sync:

```rust
if let Some(session) = &self.current_session {
    self.status_line_state.total_tokens = session.token_usage.total_tokens;
}
```

This already exists. Now also add — but where does `last_prompt_tokens` come from? It comes from `StreamUsage.prompt_tokens` on the most recent API response. We need to store it on `App`.

Add a field to `App`:

```rust
/// Last prompt_tokens reported by the API (current context window usage).
last_prompt_tokens: u64,
```

Initialize to `0`, reset on `/new`.

In the `LlmFinish` handler (line ~464), after the `add_usage` call, store it:

```rust
if let (Some(u), Some(session)) = (usage, &mut self.current_session) {
    self.last_prompt_tokens = u.prompt_tokens as u64;
    let _ = mgr.add_usage(session, u.prompt_tokens, u.completion_tokens);
}
```

In `update_sidebar()`, sync it:

```rust
self.status_line_state.last_prompt_tokens = self.last_prompt_tokens;
```

### Step 8: Update check_context_warning to use last_prompt_tokens

In `src/app.rs` `check_context_warning()`, change from `total_tokens` to `last_prompt_tokens`:

```rust
fn check_context_warning(&mut self) {
    if self.context_warned { return; }
    let context_window = self.status_line_state.context_window;
    let prompt_tokens = self.status_line_state.last_prompt_tokens;
    if context_window == 0 { return; }
    let threshold = (context_window as f64 * 0.60) as u64;
    if prompt_tokens >= threshold {
        self.context_warned = true;
        let pct = self.status_line_state.context_usage_pct();
        // ... rest of warning message
    }
}
```

Update the context warning tests accordingly.

### Step 9: Run all tests

Run: `cargo test`

### Step 10: Commit

```bash
git add src/ui/status_line.rs src/ui/input.rs src/ui/mod.rs src/app.rs
git commit -m "feat: input bar shows context pressure (prompt_tokens), sidebar shows cumulative cost"
```

---

## Task 3: Incremental Token Updates During Tool Loops

**Why:** During a multi-tool loop (e.g., 40 tool calls), the token counter freezes until the entire loop finishes. Users see stale numbers for minutes. We should send usage updates after each intermediate API call.

**Files:**
- Modify: `src/event.rs:7-54` (add `LlmUsageUpdate` variant)
- Modify: `src/stream.rs:284-287` (send usage update after each API response)
- Modify: `src/app.rs:445-494` (handle `LlmUsageUpdate` event)

### Step 1: Add LlmUsageUpdate event variant

In `src/event.rs`, add after `LlmFinish`:

```rust
/// Intermediate token usage update during a tool call loop.
/// Sent after each API response so the UI can show incremental token counts.
LlmUsageUpdate { usage: StreamUsage },
```

### Step 2: Send usage update after each API response in stream.rs

In `src/stream.rs`, after the usage accumulation block (around line 287), add:

```rust
if let Some(u) = &response.usage {
    // ... existing accumulation code ...
    total_usage.prompt_tokens += u.prompt_tokens;
    total_usage.completion_tokens += u.completion_tokens;
    total_usage.total_tokens += u.total_tokens;

    // Send incremental update to UI so token counters refresh mid-loop
    let _ = event_tx.send(AppEvent::LlmUsageUpdate {
        usage: total_usage.clone(),
    });
}
```

Note: `StreamUsage` needs `Clone` — check if it already derives it. If not, add `Clone` to the derive list on `StreamUsage`.

### Step 3: Handle LlmUsageUpdate in app.rs

In `src/app.rs`, in the main event match (near the `LlmFinish` handler), add:

```rust
AppEvent::LlmUsageUpdate { usage } => {
    // Update token counters mid-loop without saving to storage
    if let Some(session) = &mut self.current_session {
        // Overwrite with latest cumulative values from stream
        // (stream accumulates across iterations)
        self.last_prompt_tokens = usage.prompt_tokens as u64;
    }
    self.status_line_state.last_prompt_tokens = usage.prompt_tokens as u64;
    // Note: don't update session.token_usage here — that happens
    // on LlmFinish to avoid intermediate disk writes.
    // Only update the display-side fields.
    self.check_context_warning();
}
```

### Step 4: Run tests

Run: `cargo test`

### Step 5: Commit

```bash
git add src/event.rs src/stream.rs src/app.rs
git commit -m "feat: send incremental token updates during tool call loops"
```

---

## Task 4: TRS-80 Structural Framing (Input Area Border)

**Why:** The message area and input area have no visual separation — text scrolls underneath with no indicator of where the message pane ends and input begins. Adding a top border to the input area creates a "bezel" effect.

**Files:**
- Modify: `src/ui/input.rs:193` (change `Borders::NONE` to `Borders::TOP`)
- Modify: `src/ui/layout.rs` (adjust `INPUT_HEIGHT` from 4 to 5 to account for border)

### Step 1: Write failing test

In `src/ui/layout.rs` tests, if not already present, add a test that checks input height accounts for a border:

```rust
#[test]
fn layout_input_height_accounts_for_border() {
    // Input needs 5 rows: 1 border + 1 context line + 3 textarea rows
    let layout = compute_layout(
        ratatui::layout::Rect::new(0, 0, 100, 30),
        false,
    );
    assert_eq!(layout.input_area.height, 5);
}
```

### Step 2: Run test — expect failure

Run: `cargo test layout_input_height_accounts`
Expected: FAIL — input height is 4, not 5

### Step 3: Update INPUT_HEIGHT

In `src/ui/layout.rs`, change:

```rust
const INPUT_HEIGHT: u16 = 5; // 1 border + 1 context line + 3 textarea rows
```

### Step 4: Add border to input area

In `src/ui/input.rs`, line 193, change:

```rust
let input_block = Block::default()
    .borders(Borders::TOP)
    .border_style(Style::default().fg(theme.border));
```

This adds a single horizontal line (using box-drawing chars `─`) between the message area and input area, styled in `DarkGray` (the theme's border color).

### Step 5: Run tests — expect pass

Run: `cargo test`

### Step 6: Verify visually

Run: `cargo run`
Expected: A visible `─────` separator line between the message pane and the input area. The sidebar's existing left border creates an overall "framed" feel.

### Step 7: Commit

```bash
git add src/ui/input.rs src/ui/layout.rs
git commit -m "feat: add top border to input area for visual separation"
```

---

## Implementation Order

```
Task 1 (Scroll Fix)         — independent, 5 minutes
Task 2 (Token Display)      — independent, touches most files
Task 3 (Incremental Tokens) — depends on Task 2 (uses last_prompt_tokens)
Task 4 (Input Border)       — independent, 5 minutes
```

Recommended: Task 1 → Task 4 → Task 2 → Task 3

## Verification

After all tasks:
1. `cargo test` — all tests pass
2. Run the app and verify:
   - [ ] Scroll direction feels correct on macOS with natural scrolling
   - [ ] Input bar shows `prompt_tokens / context_window (%)` — reflects current context pressure
   - [ ] Sidebar shows cumulative `in: X  out: Y  total: Z`
   - [ ] Token counter updates after each intermediate API call during tool loops
   - [ ] Visible `─────` border between message area and input area
   - [ ] 60% context warning still fires correctly (now based on prompt_tokens)
   - [ ] Auto-compact threshold still works at 80%
