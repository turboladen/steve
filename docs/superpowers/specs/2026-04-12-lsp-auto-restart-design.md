# LSP Auto-Restart on Crash

**Date:** 2026-04-12
**Bead:** steve-wvhy
**Status:** Approved

## Problem

When an LSP server crashes mid-session, Steve marks it as Error in the sidebar
and the user must restart Steve entirely to recover LSP for that language.

## Approach: Event-Channel Restart

The crash watcher task sends `AppEvent::LspRestartNeeded { lang }` through the
event channel after a backoff delay. The main event loop handles it by spawning
a `spawn_blocking` that acquires the `LspManager` write lock and calls a new
`restart_server(lang)` method.

This avoids threading `Arc<RwLock<LspManager>>` into the watcher (which only
needs the status Mutex and event sender) and follows the existing pattern where
`spawn_blocking` tasks communicate results via `AppEvent`.

## Backoff Strategy

- Max retries: 3
- Delays: `[0s, 1s, 5s]` (immediate first retry, then escalating)
- After 3 failed attempts, server stays in permanent `Error`

## State Changes

### `LspStatusEntry` (lsp/mod.rs)

New fields:

```rust
pub restart_attempts: u8,
pub next_restart_at: Option<Instant>,
```

### `LspServerState` (lsp/mod.rs)

New variant:

```rust
Restarting,
```

### State Transitions (crash path)

```
Ready/Indexing
  -> Error           (crash detected by watcher)
  -> Restarting      (attempts < 3: watcher sets state, begins backoff sleep)
  -> [backoff delay]
  -> AppEvent::LspRestartNeeded sent
  -> Starting        (restart_server() removes old server, calls start_server())
  -> Ready/Indexing  (initialize success, restart_attempts reset to 0)
     OR
  -> Error           (initialize/start failure; retry budget is preserved,
                      not clamped to max — a subsequent successful restart
                      still resets it)

Ready/Indexing
  -> Error           (crash detected, attempts >= 3, no further auto-restart
                      this session)
```

### New `AppEvent` Variant (event.rs)

```rust
LspRestartNeeded { lang: Language },
```

## Crash Watcher Changes (lsp/manager.rs)

The watcher task receives a clone of `event_tx: mpsc::UnboundedSender<AppEvent>`
in addition to the existing `status: SharedLspStatus` and `shutdown_flag`.

New flow after detecting unintentional exit:

1. Write `Error` to status cache (as today)
2. Read `restart_attempts` from the status entry
3. If `attempts < 3`:
   - Increment `restart_attempts`
   - Set state to `Restarting`
   - Set `next_restart_at` to `Instant::now() + backoff_delay(attempts)`
   - Sleep for the backoff delay
   - Send `AppEvent::LspRestartNeeded { lang }`
4. If `attempts >= 3`:
   - Stay `Error`, exit watcher task

Backoff delay function:

```rust
fn restart_backoff(attempt: u8) -> Duration {
    match attempt {
        0 => Duration::ZERO,
        1 => Duration::from_secs(1),
        _ => Duration::from_secs(5),
    }
}
```

## Event Loop Handler (app/event_loop.rs)

On `AppEvent::LspRestartNeeded { lang }`:

1. Clone `self.lsp_manager` Arc
2. Clone `self.event_tx`
3. `spawn_blocking`:
   - Acquire write lock on `LspManager`
   - Call `restart_server(lang)`

## `restart_server()` (lsp/manager.rs)

New method on `LspManager`:

1. Remove old `LspServer` from `servers` HashMap and call `transport_shutdown()`
   on it. In the normal post-crash case the mainloop is already dead so this
   is mostly a cheap no-op, but it reliably reaps any still-live child process
   (useful if `restart_server` is ever invoked on a healthy server in the
   future, e.g. from a manual restart command)
2. Call `start_server(lang)` (reuses existing startup path)
3. Insert new server into `servers`
4. On success: `restart_attempts` reset to 0 happens inside `start_server`'s
   post-Initialize critical section, atomically with the Ready transition —
   so a rapid re-crash cannot race between "Ready" and "budget reset"
5. On init/start failure: preserve the existing `restart_attempts` value.
   The crash watcher already incremented it before `LspRestartNeeded` was
   sent, and clamping to MAX here would permanently disable auto-restart
   after a transient spawn failure (binary momentarily unavailable, etc.).
   Set `next_restart_at` to None and flip to `Error` if `start_server` did
   not already record a specific Error reason.

### Shutting Down the Old Server

`transport_shutdown()` sets `shutdown_flag = true` before aborting the
mainloop and best-effort killing the child. Calling it during restart
handles both the normal case (mainloop already dead — cheap no-op path
through the shutdown sequence) and the edge case where `restart_server`
is invoked on a server whose mainloop is still running. The old
implementation manually set `shutdown_flag` and dropped the server; this
was replaced with `transport_shutdown()` so the child process is reliably
reaped instead of orphaned.

## Sidebar Display

The Tick handler already maps `LspStatusEntry` to `SidebarLsp`. New rendering:

- `Restarting` with `next_restart_at`: "Restarting in Ns" (countdown from
  `next_restart_at - Instant::now()`)
- `Error` with `restart_attempts >= MAX`: "Error (restart failed)"

## Success Reset

When `start_server()` transitions state to `Ready`, reset:
- `restart_attempts = 0`
- `next_restart_at = None`

This gives the server a fresh retry budget for future crashes.

## Non-Goals

- No restart on intentional shutdown (`shutdown_flag=true`)
- No restart on Initialize failure from `start_server()` (config/binary issue)
- No user-triggered manual restart (future work)

## Exhaustive Match Updates

Adding `LspServerState::Restarting` requires updating all exhaustive matches.
Key locations (from CLAUDE.md and codebase):

- Sidebar rendering (`ui/` — wherever `LspServerState` is matched for display)
- Any `Display` impl for `LspServerState`
- Tests that match on `LspServerState` variants

## Testing

- Unit test: `restart_backoff()` returns correct delays for each attempt
- Unit test: state transitions — crash with attempts < max sets `Restarting`,
  crash with attempts >= max stays `Error`
- Unit test: init failure during restart sets permanent `Error`
- Unit test: successful restart resets `restart_attempts` to 0
- Existing `LspServerState` tests updated for new `Restarting` variant
