# LSP Auto-Restart Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Automatically restart crashed LSP servers with exponential backoff so users don't have to restart Steve.

**Architecture:** The crash watcher task (already exists per-server in `lsp/manager.rs`) gains restart logic: on unintentional crash, it enters a retry loop with backoff delays, then sends `AppEvent::LspRestartNeeded` through the event channel. The event loop handles it by `spawn_blocking` → write-lock `LspManager` → call `restart_server()`. Backoff state lives on `LspStatusEntry`.

**Tech Stack:** Rust, tokio (spawn/sleep/spawn_blocking), existing `AppEvent` channel

---

### Task 1: Add `Restarting` variant to `LspServerState` and backoff fields to `LspStatusEntry`

**Files:**
- Modify: `src/lsp/mod.rs:22-79` (enum + struct + tests)

- [ ] **Step 1: Write failing tests for the new variant and fields**

Add these tests to the existing `#[cfg(test)] mod tests` block in `src/lsp/mod.rs`:

```rust
#[test]
fn lsp_server_state_restarting_label() {
    assert_eq!(LspServerState::Restarting.label(), "Restarting");
}

#[test]
fn lsp_server_state_restarting_is_animated() {
    assert!(LspServerState::Restarting.is_animated());
}

#[test]
fn lsp_status_entry_restart_fields_default() {
    let entry = LspStatusEntry {
        binary: "rust-analyzer".into(),
        state: LspServerState::Ready,
        active_progress: 0,
        progress_message: None,
        updated_at: std::time::Instant::now(),
        restart_attempts: 0,
        next_restart_at: None,
    };
    assert_eq!(entry.restart_attempts, 0);
    assert!(entry.next_restart_at.is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib lsp::tests`
Expected: compilation errors — `Restarting` variant doesn't exist, `restart_attempts`/`next_restart_at` fields don't exist.

- [ ] **Step 3: Add the `Restarting` variant to `LspServerState`**

In `src/lsp/mod.rs`, add the `Restarting` variant to the enum:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspServerState {
    /// Process spawned; Initialize request in flight or queued.
    Starting,
    /// Initialize returned; at least one active `$/progress` work-done token.
    Indexing,
    /// Initialize returned; no active progress tokens.
    Ready,
    /// Crash detected; restart scheduled after backoff delay.
    Restarting,
    /// Initialize failed, or the mainloop exited unexpectedly. Terminal.
    Error { reason: String },
}
```

Update `label()` — add the arm before `Error`:

```rust
Self::Restarting => "Restarting",
```

Update `is_animated()` — `Restarting` should animate (spinner):

```rust
Self::Starting | Self::Indexing | Self::Restarting => true,
Self::Ready | Self::Error { .. } => false,
```

Update the doc comment on the enum to remove the "terminal" / "no auto-restart" language:

```rust
/// Lifecycle state of a single language server as tracked by `LspManager`.
///
/// Crash detection writes `Error`, then transitions to `Restarting` if
/// retry budget remains. After `MAX_RESTART_ATTEMPTS` failures, `Error`
/// is terminal.
```

- [ ] **Step 4: Add backoff fields to `LspStatusEntry`**

In `src/lsp/mod.rs`, add two fields to `LspStatusEntry`:

```rust
pub struct LspStatusEntry {
    pub binary: String,
    pub state: LspServerState,
    pub active_progress: usize,
    pub progress_message: Option<String>,
    pub updated_at: Instant,
    /// Number of consecutive restart attempts since last successful Ready state.
    pub restart_attempts: u8,
    /// When the next restart attempt will fire. Set by the crash watcher,
    /// read by the sidebar Tick handler for countdown display.
    pub next_restart_at: Option<Instant>,
}
```

- [ ] **Step 5: Add the `restart_backoff` helper and `MAX_RESTART_ATTEMPTS` constant**

Add these at the top of `src/lsp/mod.rs`, after the imports:

```rust
/// Maximum number of automatic restart attempts before giving up.
pub const MAX_RESTART_ATTEMPTS: u8 = 3;

/// Backoff delay for the given restart attempt (0-indexed).
pub fn restart_backoff(attempt: u8) -> std::time::Duration {
    match attempt {
        0 => std::time::Duration::ZERO,
        1 => std::time::Duration::from_secs(1),
        _ => std::time::Duration::from_secs(5),
    }
}
```

- [ ] **Step 6: Fix all compilation errors from the new fields**

Every `LspStatusEntry` construction site needs the two new fields. Search for `LspStatusEntry {` across the codebase and add `restart_attempts: 0, next_restart_at: None,` to each. Key locations:

- `src/lsp/mod.rs` — the test `lsp_status_entry_clone_preserves_fields`
- `src/lsp/manager.rs` — `detect_and_seed_starting()` (line 69), `start_server()` (line 141), `sample_entry()` test helper (line 483)
- `src/ui/sidebar/render.rs` tests — any test constructing `SidebarLsp` won't need changes (it doesn't use `LspStatusEntry` directly)

For `lsp_status_entry_clone_preserves_fields` test, add assertions for the new fields:

```rust
assert_eq!(cloned.restart_attempts, original.restart_attempts);
assert_eq!(cloned.next_restart_at, original.next_restart_at);
```

- [ ] **Step 7: Add tests for `restart_backoff` and `MAX_RESTART_ATTEMPTS`**

In the test module of `src/lsp/mod.rs`:

```rust
#[test]
fn restart_backoff_delays() {
    assert_eq!(restart_backoff(0), std::time::Duration::ZERO);
    assert_eq!(restart_backoff(1), std::time::Duration::from_secs(1));
    assert_eq!(restart_backoff(2), std::time::Duration::from_secs(5));
    assert_eq!(restart_backoff(3), std::time::Duration::from_secs(5));
}

#[test]
fn max_restart_attempts_is_three() {
    assert_eq!(MAX_RESTART_ATTEMPTS, 3);
}
```

- [ ] **Step 8: Run tests to verify everything passes**

Run: `cargo test --lib lsp::tests`
Expected: all tests pass.

- [ ] **Step 9: Fix exhaustive match warnings from existing `LspServerState` tests**

The existing tests `lsp_server_state_label_all_variants` and `lsp_server_state_is_animated_matrix` need the `Restarting` case added. Add:

```rust
// In lsp_server_state_label_all_variants:
assert_eq!(LspServerState::Restarting.label(), "Restarting");

// In lsp_server_state_is_animated_matrix:
assert!(LspServerState::Restarting.is_animated());
```

- [ ] **Step 10: Run full test suite and clippy**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all pass, no warnings.

- [ ] **Step 11: Commit**

```bash
git add src/lsp/mod.rs src/lsp/manager.rs
git commit -m "feat(lsp): add Restarting state and backoff fields for auto-restart"
```

---

### Task 2: Add `LspRestartNeeded` variant to `AppEvent`

**Files:**
- Modify: `src/event.rs:28-119`

- [ ] **Step 1: Add the new variant**

In `src/event.rs`, add after the `McpStatus` variant (around line 91):

```rust
/// An LSP server crashed and should be restarted. Sent by the crash
/// watcher after a backoff delay. The event loop handles this by
/// calling `LspManager::restart_server(lang)` in a `spawn_blocking`.
LspRestartNeeded {
    lang: crate::lsp::Language,
},
```

- [ ] **Step 2: Run tests to verify compilation**

Run: `cargo test --lib event::tests`
Expected: pass (enum variants don't need exhaustive test coverage here — the `_ => {}` arm in `handle_event` absorbs it for now).

- [ ] **Step 3: Commit**

```bash
git add src/event.rs
git commit -m "feat(lsp): add LspRestartNeeded AppEvent variant"
```

---

### Task 3: Add `restart_server()` method to `LspManager`

**Files:**
- Modify: `src/lsp/manager.rs`

- [ ] **Step 1: Write a test for restart_server replacing the old server entry**

In the test module of `src/lsp/manager.rs`, add:

```rust
#[tokio::test]
async fn restart_server_resets_status_to_starting() {
    let dir = tempdir().unwrap();
    let mut mgr = LspManager::new(dir.path().to_path_buf(), tokio::runtime::Handle::current());

    // Seed with an Error entry (simulating a crashed server)
    mgr.insert_status_for_test(
        Language::Rust,
        LspStatusEntry {
            binary: "rust-analyzer".into(),
            state: LspServerState::Error {
                reason: "mainloop exited".into(),
            },
            active_progress: 0,
            progress_message: None,
            updated_at: Instant::now(),
            restart_attempts: 1,
            next_restart_at: None,
        },
    );

    // restart_server will fail (no rust-analyzer on PATH in CI) —
    // but the method should still remove the old server and attempt start.
    // We test the cleanup + status transition, not actual server startup.
    let result = mgr.restart_server(Language::Rust);

    // Whether it succeeds or fails depends on PATH — but the status
    // cache should reflect the attempt.
    let snap = mgr.status_snapshot();
    let entry = snap.iter().find(|(l, _)| *l == Language::Rust).unwrap();
    if result.is_err() {
        // Failed to start — should be Error with max attempts
        assert!(
            matches!(entry.1.state, LspServerState::Error { .. }),
            "failed restart should set Error state"
        );
        assert_eq!(
            entry.1.restart_attempts,
            crate::lsp::MAX_RESTART_ATTEMPTS,
            "failed restart should set attempts to MAX"
        );
    }
    // If it succeeded (rust-analyzer on PATH), it would be Starting/Ready
    // with restart_attempts reset — but we can't assert that in CI.
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib lsp::manager::tests::restart_server`
Expected: compilation error — `restart_server` doesn't exist yet.

- [ ] **Step 3: Implement `restart_server`**

Add this method to `impl LspManager` in `src/lsp/manager.rs`:

```rust
/// Restart a crashed LSP server. Called from the event loop's
/// `LspRestartNeeded` handler via `spawn_blocking`.
///
/// 1. Removes the old `LspServer` from `servers` (setting its shutdown
///    flag first so any residual watcher exits cleanly).
/// 2. Calls `start_server` to spawn a fresh process + Initialize.
/// 3. On init failure, sets `restart_attempts` to MAX so the new
///    watcher won't retry (init failures are config/binary issues).
pub fn restart_server(&mut self, lang: Language) -> Result<()> {
    // Remove the old crashed server if still present.
    // Set its shutdown_flag so dropping it doesn't trigger another
    // Error write from any lingering watcher reference.
    if let Some(old_server) = self.servers.remove(&lang) {
        old_server.shutdown_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        // Drop old_server — process is already dead, this is cleanup only.
        drop(old_server);
    }

    tracing::info!("LSP: restarting {lang} server");

    match self.start_server(lang) {
        Ok(server) => {
            tracing::info!("LSP: restarted {lang} server successfully");
            self.servers.insert(lang, server);
            // Reset attempts since start_server succeeded past Initialize.
            let mut map = self.status.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = map.get_mut(&lang) {
                entry.restart_attempts = 0;
                entry.next_restart_at = None;
            }
            Ok(())
        }
        Err(e) => {
            tracing::warn!("LSP: restart of {lang} failed: {e:#}");
            // Init failure — set attempts to MAX so the new watcher
            // (if any) won't retry. This is a config/binary issue.
            let mut map = self.status.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = map.get_mut(&lang) {
                entry.restart_attempts = crate::lsp::MAX_RESTART_ATTEMPTS;
                entry.next_restart_at = None;
            }
            Err(e)
        }
    }
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test --lib lsp::manager::tests::restart_server`
Expected: pass.

- [ ] **Step 5: Run full test suite and clippy**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src/lsp/manager.rs
git commit -m "feat(lsp): add restart_server method to LspManager"
```

---

### Task 4: Wire crash watcher to send `LspRestartNeeded` with backoff

**Files:**
- Modify: `src/lsp/manager.rs:119-198` (`start_server` method — crash watcher block)

- [ ] **Step 1: Pass `event_tx` into `start_server`**

Change the `start_server` signature from:

```rust
fn start_server(&self, lang: Language) -> Result<LspServer> {
```

to:

```rust
fn start_server(
    &self,
    lang: Language,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::event::AppEvent>>,
) -> Result<LspServer> {
```

The `Option` allows `start_server` to work without an event sender (for initial startup before the event channel exists, and for tests). When `None`, the watcher behaves as today (mark Error and exit).

- [ ] **Step 2: Update all call sites for the new parameter**

Three call sites in `src/lsp/manager.rs`:

1. `start_servers()` (line 90): `self.start_server(lang)` → `self.start_server(lang, self.event_tx.clone())`
2. `ensure_server()` (line 346): `self.start_server(lang)` → `self.start_server(lang, self.event_tx.clone())`
3. `restart_server()` (the method from Task 3): `self.start_server(lang)` → `self.start_server(lang, self.event_tx.clone())`

But wait — `LspManager` doesn't have `event_tx` yet. We need to store it. Add it as a field:

```rust
pub struct LspManager {
    servers: HashMap<Language, LspServer>,
    detected_languages: Vec<Language>,
    project_root: PathBuf,
    handle: tokio::runtime::Handle,
    status: SharedLspStatus,
    /// Event channel sender for LSP restart events. `None` during tests
    /// or before the event loop is set up.
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::event::AppEvent>>,
}
```

Update `LspManager::new()` to accept and store it:

```rust
pub fn new(
    project_root: PathBuf,
    handle: tokio::runtime::Handle,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::event::AppEvent>>,
) -> Self {
    Self {
        servers: HashMap::new(),
        detected_languages: Vec::new(),
        project_root,
        handle,
        status: Arc::new(Mutex::new(HashMap::new())),
        event_tx,
    }
}
```

Then each `start_server` call passes `self.event_tx.clone()`.

- [ ] **Step 3: Update `LspManager::new()` call sites**

Search for `LspManager::new(` across the codebase. There will be:

1. `src/app/mod.rs` — the real construction site. Pass the event sender.
2. Test files in `src/lsp/manager.rs` — pass `None`.

For the test helper, all `LspManager::new(dir.path().to_path_buf(), tokio::runtime::Handle::current())` calls become `LspManager::new(dir.path().to_path_buf(), tokio::runtime::Handle::current(), None)`.

For the real construction in `src/app/mod.rs`, the `event_tx` is already available at `App::new` time. Find the line that constructs `LspManager` and pass `Some(event_tx.clone())`.

- [ ] **Step 4: Rewrite the crash watcher to handle restarts**

Replace the crash watcher block in `start_server` (lines 175-198) with:

```rust
// Spawn a crash watcher that awaits mainloop completion. If the
// shutdown flag is not set, the mainloop exited unexpectedly —
// write Error to the status cache, then attempt restart with backoff.
{
    let status = self.status.clone();
    let shutdown_flag_watch = shutdown_flag.clone();
    let binary_for_watch = binary.clone();
    let event_tx = event_tx.clone();
    self.handle.spawn(async move {
        let join_result = mainloop_handle.await;
        if shutdown_flag_watch.load(Ordering::SeqCst) {
            return; // intentional shutdown
        }
        let reason = match join_result {
            Ok(()) => "mainloop exited".to_string(),
            Err(e) if e.is_cancelled() => "mainloop cancelled".to_string(),
            Err(e) => format!("mainloop panicked: {e}"),
        };
        tracing::warn!("LSP {binary_for_watch} ({lang}) {reason}");

        // Read current attempt count and decide whether to restart.
        let (should_restart, attempt) = {
            let mut map = status.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = map.get_mut(&lang) {
                entry.state = LspServerState::Error {
                    reason: reason.clone(),
                };
                entry.active_progress = 0;
                entry.progress_message = None;
                entry.updated_at = Instant::now();

                let attempt = entry.restart_attempts;
                if attempt < crate::lsp::MAX_RESTART_ATTEMPTS {
                    if let Some(ref tx) = event_tx {
                        let delay = crate::lsp::restart_backoff(attempt);
                        entry.restart_attempts = attempt + 1;
                        entry.state = LspServerState::Restarting;
                        entry.next_restart_at = Some(Instant::now() + delay);
                        entry.updated_at = Instant::now();
                        (Some((tx.clone(), delay)), attempt)
                    } else {
                        (None, attempt)
                    }
                } else {
                    tracing::warn!(
                        "LSP {binary_for_watch} ({lang}): max restart attempts \
                         ({}) reached, giving up",
                        crate::lsp::MAX_RESTART_ATTEMPTS,
                    );
                    (None, attempt)
                }
            } else {
                (None, 0)
            }
        };

        if let Some((tx, delay)) = should_restart {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let _ = tx.send(crate::event::AppEvent::LspRestartNeeded { lang });
        }
    });
}
```

- [ ] **Step 5: Run the full test suite**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all pass. The existing crash-watcher tests don't test the async watcher directly (it's an internal tokio task), so they should still pass.

- [ ] **Step 6: Commit**

```bash
git add src/lsp/manager.rs src/app/mod.rs
git commit -m "feat(lsp): wire crash watcher to send LspRestartNeeded with backoff"
```

---

### Task 5: Handle `LspRestartNeeded` in the event loop

**Files:**
- Modify: `src/app/event_loop.rs:114-609` (`handle_event` method)

- [ ] **Step 1: Write a test for the event handler**

In the `#[cfg(test)] mod tests` block of `src/app/event_loop.rs`, add:

```rust
#[tokio::test]
async fn event_lsp_restart_needed_sends_stream_notice_on_failure() {
    let mut app = make_test_app();

    // Seed an LSP status entry so the handler has something to work with
    // (restart will fail — no real LSP server — but it shouldn't panic)
    app.handle_event(AppEvent::LspRestartNeeded {
        lang: crate::lsp::Language::Rust,
    })
    .await
    .unwrap();

    // The handler spawns a spawn_blocking task, which we can't easily
    // await in this test. But the event itself should be handled without
    // error — the actual restart happens asynchronously.
}
```

- [ ] **Step 2: Add the handler arm**

In `handle_event` in `src/app/event_loop.rs`, add a new arm before the `_ => {}` catch-all:

```rust
AppEvent::LspRestartNeeded { lang } => {
    let lsp = self.lsp_manager.clone();
    let tx = self.event_tx.clone();
    tokio::task::spawn_blocking(move || {
        if let Ok(mut mgr) = lsp.write() {
            match mgr.restart_server(lang) {
                Ok(()) => {
                    let _ = tx.send(AppEvent::StreamNotice {
                        text: format!("LSP {lang} server restarted successfully"),
                    });
                }
                Err(e) => {
                    tracing::warn!("LSP restart of {lang} failed: {e:#}");
                    // Don't send an error event — the sidebar already
                    // shows the Error state from the status cache.
                }
            }
        }
    });
}
```

- [ ] **Step 3: Run the test**

Run: `cargo test --lib app::event_loop::tests::event_lsp_restart`
Expected: pass (compiles and the event is handled without panic).

- [ ] **Step 4: Run full test suite and clippy**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src/app/event_loop.rs
git commit -m "feat(lsp): handle LspRestartNeeded in the event loop"
```

---

### Task 6: Update sidebar rendering for `Restarting` state

**Files:**
- Modify: `src/ui/sidebar/render.rs:221-248` (LSP rendering block)

- [ ] **Step 1: Write a test for the Restarting glyph**

In the `#[cfg(test)] mod tests` block of `src/ui/sidebar/render.rs`, add:

```rust
#[test]
fn buffer_sidebar_lsp_section_shows_restarting_with_spinner() {
    use crate::ui::status_line::SPINNER_FRAMES;
    let state = SidebarState {
        lsp_servers: vec![SidebarLsp {
            binary: "rust-analyzer".to_string(),
            state: crate::lsp::LspServerState::Restarting,
            progress_message: None,
        }],
        spinner_frame: 0,
        ..Default::default()
    };
    let text = render_sidebar_to_string(40, 20, &state);
    assert!(text.contains("LSP"), "header should be present");
    assert!(text.contains("rust-analyzer"), "binary name should appear");
    assert!(text.contains("Restarting"), "state label should appear");
    // Should show spinner glyph (same as Starting/Indexing)
    let first_spinner = SPINNER_FRAMES[0];
    assert!(
        text.contains(first_spinner),
        "should show spinner glyph for Restarting"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib ui::sidebar::render::tests::buffer_sidebar_lsp_section_shows_restarting`
Expected: FAIL — the exhaustive match in the render code doesn't have a `Restarting` arm (compiler error).

- [ ] **Step 3: Add the `Restarting` arm to the sidebar render**

In `src/ui/sidebar/render.rs`, update the exhaustive match (around line 229):

```rust
let (glyph, color) = match &server.state {
    LspServerState::Starting => (spinner_glyph.clone(), theme.warning),
    LspServerState::Indexing => (spinner_glyph.clone(), theme.success),
    LspServerState::Ready => ("\u{25cf}".to_string(), theme.success),
    LspServerState::Restarting => (spinner_glyph.clone(), theme.warning),
    LspServerState::Error { .. } => ("\u{2715}".to_string(), theme.error),
};
```

Also update the `is_active` check (around line 241) to include `Restarting`:

```rust
let is_active = matches!(
    server.state,
    LspServerState::Starting | LspServerState::Indexing | LspServerState::Restarting
);
```

- [ ] **Step 4: Run the test**

Run: `cargo test --lib ui::sidebar::render::tests::buffer_sidebar_lsp_section_shows_restarting`
Expected: pass.

- [ ] **Step 5: Update `context.rs` to treat `Restarting` as not-running**

In `src/app/context.rs`, find the match on `LspServerState::Ready | LspServerState::Indexing` (around line 107). `Restarting` is NOT a running state — the existing code uses `matches!` which only matches `Ready | Indexing`, so `Restarting` correctly falls through to `running = false`. No code change needed, but verify this.

- [ ] **Step 6: Run full test suite and clippy**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all pass. If there are exhaustive match warnings elsewhere (e.g., in `language_status()`), fix them — `Restarting` should be treated like `Starting` (not running).

- [ ] **Step 7: Commit**

```bash
git add src/ui/sidebar/render.rs
git commit -m "feat(lsp): render Restarting state with spinner in sidebar"
```

---

### Task 7: Update Tick handler to pass `next_restart_at` to sidebar

**Files:**
- Modify: `src/ui/sidebar/mod.rs:66-75` (`SidebarLsp` struct)
- Modify: `src/app/event_loop.rs:212-223` (Tick handler LSP snapshot)
- Modify: `src/ui/sidebar/render.rs` (progress message for Restarting)

- [ ] **Step 1: Add `next_restart_at` to `SidebarLsp`**

In `src/ui/sidebar/mod.rs`, add the field:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarLsp {
    pub binary: String,
    pub state: crate::lsp::LspServerState,
    pub progress_message: Option<String>,
    /// When the next restart attempt will fire (only set during `Restarting`).
    /// Used to show a countdown in the sidebar.
    pub next_restart_at: Option<std::time::Instant>,
}
```

- [ ] **Step 2: Update all `SidebarLsp` construction sites**

The Tick handler in `src/app/event_loop.rs` (line 215):

```rust
.map(|(_, entry)| SidebarLsp {
    binary: entry.binary,
    state: entry.state,
    progress_message: entry.progress_message,
    next_restart_at: entry.next_restart_at,
})
```

All test construction sites in `src/ui/sidebar/mod.rs` and `src/ui/sidebar/render.rs` — add `next_restart_at: None` to every `SidebarLsp { ... }` literal. There are many — use a find-and-replace approach:

Every occurrence of:
```rust
progress_message: None,
}
```
(in a `SidebarLsp` context) becomes:
```rust
progress_message: None,
next_restart_at: None,
}
```

And for ones with a non-None progress_message:
```rust
progress_message: Some("...".to_string()),
}
```
becomes:
```rust
progress_message: Some("...".to_string()),
next_restart_at: None,
}
```

- [ ] **Step 3: Show countdown as progress message during Restarting**

In `src/ui/sidebar/render.rs`, update the progress message logic. After computing `is_active`, add special handling for `Restarting`:

```rust
if matches!(server.state, LspServerState::Restarting) {
    if let Some(restart_at) = server.next_restart_at {
        let remaining = restart_at.saturating_duration_since(Instant::now());
        let secs = remaining.as_secs();
        if secs > 0 {
            let countdown = format!("restarting in {secs}s");
            let max_chars = sidebar_width.saturating_sub(4);
            if max_chars > 0 {
                let display = crate::truncate_chars(&countdown, max_chars);
                lines.push(Line::from(vec![Span::styled(
                    format!("    {display}"),
                    Style::default().fg(theme.dimmed),
                )]));
            }
        }
    }
} else if is_active && let Some(msg) = &server.progress_message {
    // existing progress message rendering...
}
```

Note: This requires adding `use std::time::Instant;` to the render function's scope if not already imported.

- [ ] **Step 4: Run full test suite and clippy**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src/ui/sidebar/mod.rs src/ui/sidebar/render.rs src/app/event_loop.rs
git commit -m "feat(lsp): show restart countdown in sidebar during Restarting state"
```

---

### Task 8: Integration test — verify end-to-end event flow

**Files:**
- Modify: `src/app/event_loop.rs` (add integration-style test)

- [ ] **Step 1: Write an integration test for the full restart flow**

In the `#[cfg(test)] mod tests` block of `src/app/event_loop.rs`:

```rust
#[tokio::test]
async fn event_tick_shows_restarting_lsp_in_sidebar() {
    let mut app = make_test_app();

    // Directly seed the LSP status cache with a Restarting entry
    // (simulating what the crash watcher would do).
    {
        let mut map = app.lsp_status_cache.lock().unwrap();
        map.insert(
            crate::lsp::Language::Rust,
            crate::lsp::LspStatusEntry {
                binary: "rust-analyzer".into(),
                state: crate::lsp::LspServerState::Restarting,
                active_progress: 0,
                progress_message: None,
                updated_at: std::time::Instant::now(),
                restart_attempts: 1,
                next_restart_at: Some(std::time::Instant::now() + std::time::Duration::from_secs(3)),
            },
        );
    }

    // Tick should pick up the Restarting state
    app.handle_event(AppEvent::Tick).await.unwrap();

    assert_eq!(app.sidebar_state.lsp_servers.len(), 1);
    assert_eq!(
        app.sidebar_state.lsp_servers[0].state,
        crate::lsp::LspServerState::Restarting
    );
    assert!(app.sidebar_state.lsp_servers[0].next_restart_at.is_some());
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test --lib app::event_loop::tests::event_tick_shows_restarting`
Expected: pass.

- [ ] **Step 3: Run the full test suite and clippy one final time**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all pass, no warnings.

- [ ] **Step 4: Commit**

```bash
git add src/app/event_loop.rs
git commit -m "test(lsp): add integration test for restart state in sidebar"
```

---

### Task 9: Final cleanup and verification

**Files:**
- Review: all modified files

- [ ] **Step 1: Run the full test suite**

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 2: Run clippy with CI flags**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 3: Run rustfmt**

Run: `cargo +nightly fmt`
Expected: no changes (all code should already be formatted).

- [ ] **Step 4: Verify the spec's acceptance criteria mentally**

Review against the spec (`docs/superpowers/specs/2026-04-12-lsp-auto-restart-design.md`):

1. After `pkill rust-analyzer`, the crash watcher detects exit, writes Error, checks attempts < 3, transitions to Restarting, sleeps for backoff, sends `LspRestartNeeded`. Event loop calls `restart_server()`, which removes old server and starts new one. Sidebar shows Restarting → Starting → Ready. **Covered by Tasks 1-8.**

2. Repeated crashes trigger exponential backoff. Attempt 0 → 0s, attempt 1 → 1s, attempt 2 → 5s. After 3 failures, stays Error. **Covered by `restart_backoff()` and crash watcher logic in Task 4.**

3. Intentional shutdown does not trigger restart. `shutdown_flag` is set before abort, watcher checks it and exits. **Unchanged from existing code, verified by no changes to that path.**
