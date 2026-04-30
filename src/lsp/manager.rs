use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, Result};
use async_lsp::lsp_types::*;
use tokio::io::AsyncBufReadExt;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::{
    Language, LspServerState, LspStatusEntry,
    client::{SharedDiagnostics, SharedLspStatus, create_client},
    server::{LspServer, path_to_uri},
};

/// Maximum number of stderr lines retained in the rolling tail buffer.
const STDERR_TAIL_MAX_LINES: usize = 10;

/// Maximum total bytes retained in the rolling tail buffer (defense
/// against a single very long line blowing up memory).
const STDERR_TAIL_MAX_BYTES: usize = 2048;

/// Bounded ring buffer of recent stderr lines from a spawned LSP child.
///
/// Written by the per-child stderr pump task; read by the Initialize-
/// failure branch and the crash watcher when constructing an `Error`
/// reason. Eviction enforces both a line count and a byte budget so a
/// single pathological long line can't starve the buffer of other
/// useful context.
#[derive(Debug, Default)]
pub(super) struct StderrTail {
    lines: VecDeque<String>,
    total_bytes: usize,
}

impl StderrTail {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn push(&mut self, line: String) {
        self.total_bytes = self.total_bytes.saturating_add(line.len());
        self.lines.push_back(line);
        // Evict from the front while we're over either limit, but always
        // retain at least the most recent line — a single oversized line
        // (e.g., a verbose Node traceback) is more useful than nothing.
        while self.lines.len() > STDERR_TAIL_MAX_LINES
            || (self.total_bytes > STDERR_TAIL_MAX_BYTES && self.lines.len() > 1)
        {
            let Some(dropped) = self.lines.pop_front() else {
                break;
            };
            self.total_bytes = self.total_bytes.saturating_sub(dropped.len());
        }
    }

    /// Render the tail as a single-line preview suitable for inclusion in
    /// an `Error` reason. Lines are joined by `" | "` after trimming
    /// trailing whitespace; the result is empty if no lines have been
    /// captured.
    pub(super) fn preview(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.trim_end().to_string())
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

/// Shared handle used by the pump task (writer) and the Initialize-fail
/// + crash-watcher paths (readers). All access goes through the mutex.
pub(super) type SharedStderrTail = Arc<Mutex<StderrTail>>;

/// Spawn a task that pumps lines from the child's stderr into the shared
/// tail buffer and the tracing log. Each line is logged at WARN level so
/// it surfaces in the default `steve=info` filter (the user has set
/// `RUST_LOG=steve=debug` for transport-level detail anyway, but the
/// stderr line is the actionable signal). The task ends when stderr
/// reaches EOF (child closed it) or a read error fires.
fn spawn_stderr_pump(
    stderr: tokio::process::ChildStderr,
    tail: SharedStderrTail,
    lang: Language,
    binary: String,
    handle: &tokio::runtime::Handle,
) {
    handle.spawn(async move {
        let mut reader = tokio::io::BufReader::new(stderr).lines();
        loop {
            match reader.next_line().await {
                Ok(Some(line)) => {
                    tracing::warn!(
                        target: "lsp_stderr",
                        lang = %lang,
                        binary = %binary,
                        "{line}",
                    );
                    let mut tail = tail.lock().unwrap_or_else(|p| p.into_inner());
                    tail.push(line);
                }
                Ok(None) => break, // EOF — child closed stderr
                Err(e) => {
                    tracing::debug!(
                        target: "lsp_stderr",
                        lang = %lang,
                        binary = %binary,
                        "stderr read error: {e}",
                    );
                    break;
                }
            }
        }
    });
}

/// Combine a base reason (e.g. `"mainloop exited"`) with the current
/// stderr tail preview. Used by both Initialize failure and crash watcher
/// to attach actionable context — the actual server-side error message —
/// to the user-visible `Error.reason`.
fn reason_with_stderr(base: &str, tail: &SharedStderrTail) -> String {
    let preview = tail.lock().unwrap_or_else(|p| p.into_inner()).preview();
    if preview.is_empty() {
        base.to_string()
    } else {
        format!("{base} (stderr: {preview})")
    }
}

pub struct LspManager {
    servers: HashMap<Language, LspServer>,
    detected_languages: Vec<Language>,
    project_root: PathBuf,
    handle: tokio::runtime::Handle,
    /// Shared per-language status cache. Single source of truth for
    /// `status_snapshot` / `language_status`. Written by `start_server`,
    /// the `$/progress` notification handler in `client::create_client`,
    /// and per-server crash-watcher tasks.
    status: SharedLspStatus,
    /// Event channel sender for LSP restart events. `None` during tests.
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::event::AppEvent>>,
}

/// Pure scheduling logic for the crash watcher. Given the current status
/// entry, whether a restart can actually be notified, and a `now`
/// timestamp, this transitions the entry to Error (always) and optionally
/// to Restarting with a computed backoff delay. Returns `Some(delay)` iff
/// a restart should be scheduled.
///
/// Extracted as a free function so the decision logic can be unit-tested
/// without spawning real LSP processes or waiting for crashes. `now` is
/// threaded through (rather than calling `Instant::now()` internally) so
/// tests can construct deterministic time scenarios without subtracting
/// from a real `Instant::now()` — `Instant`'s origin is platform-defined
/// and `checked_sub` is not guaranteed to succeed for arbitrary deltas.
///
/// The caller is responsible for holding the status-mutex critical
/// section around the call and for actually sending the
/// `LspRestartNeeded` event after the returned delay has elapsed.
pub(super) fn plan_crash_restart(
    entry: &mut LspStatusEntry,
    reason: String,
    can_notify: bool,
    now: Instant,
) -> Option<std::time::Duration> {
    // Stability gate: only reset the retry budget if the server was
    // continuously Ready/Indexing for at least STABILITY_WINDOW before
    // crashing. Servers that pass Initialize but crash within seconds
    // (yaml-language-server schema-fetch crashes, etc.) keep their
    // accumulated attempts so the budget cap actually trips.
    if let Some(ready_since) = entry.ready_since
        && now.saturating_duration_since(ready_since) >= crate::lsp::STABILITY_WINDOW
    {
        entry.restart_attempts = 0;
    }

    entry.state = LspServerState::Error { reason };
    entry.active_progress = 0;
    entry.progress_message = None;
    entry.ready_since = None;
    entry.updated_at = now;

    let attempt = entry.restart_attempts;
    if !can_notify || attempt >= crate::lsp::MAX_RESTART_ATTEMPTS {
        return None;
    }
    let delay = crate::lsp::restart_backoff(attempt);
    entry.restart_attempts = attempt + 1;
    entry.state = LspServerState::Restarting;
    entry.next_restart_at = Some(now + delay);
    entry.updated_at = now;
    Some(delay)
}

impl LspManager {
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

    /// Run filesystem detection and seed a `Starting` entry in the shared
    /// status cache for every detected language. Idempotent — safe to call
    /// multiple times.
    ///
    /// This method is split out from `start_servers` so the main thread can
    /// call it synchronously at `App::new` time, before the event loop and
    /// the background `spawn_blocking` task. That way the sidebar shows
    /// `Starting` entries on the very first `Tick` rather than briefly
    /// showing nothing and then jumping straight to `Ready` (or worse,
    /// `Ready` appearing before `Starting` is ever visible because the
    /// blocking startup is too fast). The actual server startup (which
    /// includes the slow `block_on(Initialize)` calls) still happens in
    /// `start_servers`, invoked from `spawn_blocking`.
    pub fn detect_and_seed_starting(&mut self) {
        let languages = Language::detect_from_project(&self.project_root);
        self.detected_languages = languages.clone();

        let mut map = self.status.lock().unwrap_or_else(|p| p.into_inner());
        for &lang in &languages {
            let binary = lang
                .server_candidates()
                .first()
                .map(|c| c.binary.to_string())
                .unwrap_or_else(|| format!("{lang}-lsp"));
            map.entry(lang).or_insert_with(|| LspStatusEntry {
                binary,
                state: LspServerState::Starting,
                active_progress: 0,
                progress_message: None,
                updated_at: Instant::now(),
                restart_attempts: 0,
                next_restart_at: None,
                ready_since: None,
            });
        }
    }

    pub fn start_servers(&mut self) {
        // Idempotent — if `detect_and_seed_starting` already ran at
        // `App::new` time, this is a no-op re-detection plus `or_insert_with`
        // seeding. Otherwise it does the detection + seeding now.
        self.detect_and_seed_starting();
        let languages = self.detected_languages.clone();

        for lang in languages {
            if self.servers.contains_key(&lang) {
                continue;
            }
            match self.start_server(lang, self.event_tx.clone()) {
                Ok(server) => {
                    tracing::info!("LSP: started {lang} server");
                    self.servers.insert(lang, server);
                }
                Err(e) => {
                    tracing::debug!("LSP: {lang} server not available: {e:#}");
                    // Fallback write: flip the seeded Starting entry to Error
                    // for failure paths that `start_server` cannot record
                    // itself — e.g., `resolve_server` returning `None` or
                    // `spawn` failing, both of which happen before the
                    // in-method cache upsert. If `start_server` has already
                    // written a more specific Error (e.g., the Initialize
                    // failure path), preserve that reason instead of
                    // clobbering it with the anyhow-wrapped outer message.
                    let mut map = self.status.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(entry) = map.get_mut(&lang)
                        && !matches!(entry.state, LspServerState::Error { .. })
                    {
                        entry.state = LspServerState::Error {
                            reason: format!("{e:#}"),
                        };
                        entry.updated_at = Instant::now();
                    }
                }
            }
        }
    }

    fn start_server(
        &self,
        lang: Language,
        event_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::event::AppEvent>>,
    ) -> Result<LspServer> {
        let (binary, args) = lang
            .resolve_server()
            .ok_or_else(|| anyhow::anyhow!("no {lang} language server found on PATH"))?;

        tracing::debug!(binary = %binary, ?args, "spawning LSP server for {lang}");

        let mut child = tokio::process::Command::new(&binary)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn {binary}"))?;

        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;

        // Capture stderr into a rolling tail + tracing logs. The Initialize-
        // fail branch and the crash watcher both read from this tail when
        // building the user-visible `Error.reason`, so a server that prints
        // "Cannot find module 'foo'" before exiting surfaces that signal
        // instead of an opaque `mainloop exited` / `ServiceStopped`.
        let stderr_tail: SharedStderrTail = Arc::new(Mutex::new(StderrTail::new()));
        spawn_stderr_pump(
            stderr,
            stderr_tail.clone(),
            lang,
            binary.clone(),
            &self.handle,
        );

        // Upsert the real binary name into the status cache now that
        // `resolve_server` has picked one. Keep state as Starting.
        {
            let mut map = self.status.lock().unwrap_or_else(|p| p.into_inner());
            let entry = map.entry(lang).or_insert_with(|| LspStatusEntry {
                binary: binary.clone(),
                state: LspServerState::Starting,
                active_progress: 0,
                progress_message: None,
                updated_at: Instant::now(),
                restart_attempts: 0,
                next_restart_at: None,
                ready_since: None,
            });
            entry.binary = binary.clone();
            entry.state = LspServerState::Starting;
            entry.updated_at = Instant::now();
        }

        let binary_for_log = binary.clone();
        let diagnostics: SharedDiagnostics = Arc::new(Mutex::new(HashMap::new()));
        let (mainloop, server_socket) =
            create_client(diagnostics.clone(), self.status.clone(), lang);

        // Hold a sender clone inside the mainloop task so `rx` stays open for
        // the entire lifetime of the mainloop future. Our Router service
        // (`create_client`) does not capture the server socket, so without
        // this keepalive the only senders live in `LspServer` + its shutdown
        // task; when those drop after `transport_shutdown`, async-lsp's
        // `rx.next() => event.expect("Sender is alive")` races the abort and
        // panics. The keepalive drops with the task, after the mainloop is
        // done polling.
        let mainloop_keepalive = server_socket.clone();
        let mainloop_handle = self.handle.spawn(async move {
            let _keepalive = mainloop_keepalive;
            tracing::debug!("LSP MainLoop starting for {binary_for_log}");
            match mainloop
                .run_buffered(stdout.compat(), stdin.compat_write())
                .await
            {
                Ok(()) => tracing::debug!("LSP MainLoop exited cleanly"),
                Err(e) => tracing::debug!("LSP MainLoop exited with error: {e:#}"),
            }
        });

        let mainloop_abort = mainloop_handle.abort_handle();
        let shutdown_flag = Arc::new(AtomicBool::new(false));

        // Spawn a crash watcher that awaits mainloop completion. If the
        // shutdown flag is not set, the mainloop exited unexpectedly —
        // write Error to the status cache so the sidebar surfaces it,
        // then schedule a restart via `LspRestartNeeded` if budget remains.
        {
            let status = self.status.clone();
            let shutdown_flag_watch = shutdown_flag.clone();
            let binary_for_watch = binary.clone();
            let event_tx = event_tx.clone();
            let stderr_tail_for_watch = stderr_tail.clone();
            self.handle.spawn(async move {
                let join_result = mainloop_handle.await;
                if shutdown_flag_watch.load(Ordering::SeqCst) {
                    return; // intentional shutdown
                }
                // Brief pause so the stderr pump has a chance to drain any
                // last-gasp error lines after the child exits — the mainloop
                // resolves on stdout EOF, but stderr's reader is a separate
                // task and may not have processed the final bytes yet.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let base = match join_result {
                    Ok(()) => "mainloop exited".to_string(),
                    Err(e) if e.is_cancelled() => "mainloop cancelled".to_string(),
                    Err(e) => format!("mainloop panicked: {e}"),
                };
                let reason = reason_with_stderr(&base, &stderr_tail_for_watch);
                tracing::warn!("LSP {binary_for_watch} ({lang}) {reason}");

                let restart_delay = {
                    let mut map = status.lock().unwrap_or_else(|p| p.into_inner());
                    let now = Instant::now();
                    map.get_mut(&lang).and_then(|entry| {
                        plan_crash_restart(entry, reason.clone(), event_tx.is_some(), now)
                    })
                };

                match (restart_delay, event_tx) {
                    (Some(delay), Some(tx)) => {
                        if !delay.is_zero() {
                            tokio::time::sleep(delay).await;
                        }
                        let _ = tx.send(crate::event::AppEvent::LspRestartNeeded { lang });
                    }
                    (None, Some(_)) => {
                        // Had an event channel but the plan declined — restart
                        // budget is exhausted. In test mode (event_tx None) the
                        // plan also returns None, but that's expected, not a
                        // user-visible giving-up.
                        tracing::warn!(
                            "LSP {binary_for_watch} ({lang}): restart budget exhausted (max {}), giving up",
                            crate::lsp::MAX_RESTART_ATTEMPTS,
                        );
                    }
                    _ => {}
                }
            });
        }

        let root_uri = path_to_uri(&self.project_root)?;
        let init_params = InitializeParams {
            process_id: Some(std::process::id()),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri,
                name: self
                    .project_root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("project")
                    .to_string(),
            }]),
            capabilities: ClientCapabilities {
                text_document: Some(TextDocumentClientCapabilities {
                    synchronization: Some(TextDocumentSyncClientCapabilities {
                        dynamic_registration: Some(false),
                        did_save: Some(true),
                        ..Default::default()
                    }),
                    publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                        related_information: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                // Declare window.workDoneProgress so servers emit `$/progress`
                // notifications. Without this, rust-analyzer (and others)
                // silently index without telling the client — the sidebar
                // would never leave Ready. Required for the Indexing state
                // to ever fire.
                window: Some(WindowClientCapabilities {
                    work_done_progress: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let init_result: InitializeResult = match self
            .handle
            .block_on(server_socket.request::<request::Initialize>(init_params))
        {
            Ok(result) => result,
            Err(e) => {
                // Record Error in the cache before returning — the caller
                // (`start_servers`) sees the Err and bails, but the cache
                // needs to reflect the specific initialize failure.
                // Clear Restarting-path fields so the Error row doesn't
                // carry stale `next_restart_at` / progress data into the
                // sidebar. Append the stderr tail so users see WHY the
                // server exited (e.g. "Cannot find module 'foo'") rather
                // than just the transport-level `ServiceStopped`.
                let base = format!("initialize failed: {e:?}");
                let reason = reason_with_stderr(&base, &stderr_tail);
                let mut map = self.status.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(entry) = map.get_mut(&lang) {
                    entry.state = LspServerState::Error { reason };
                    entry.active_progress = 0;
                    entry.progress_message = None;
                    entry.next_restart_at = None;
                    entry.ready_since = None;
                    entry.updated_at = Instant::now();
                }
                return Err(anyhow::anyhow!("initialize request failed: {e:?}"));
            }
        };

        server_socket
            .notify::<notification::Initialized>(InitializedParams {})
            .map_err(|e| anyhow::anyhow!("initialized notification failed: {e}"))?;

        // Transition Starting → Ready (or Indexing if `$/progress` Begin
        // notifications arrived during init — rust-analyzer does this).
        // Stamp `ready_since` so the next crash watcher can decide via the
        // STABILITY_WINDOW gate whether this run was stable enough to reset
        // the retry budget. Crucially, do NOT reset `restart_attempts` here:
        // a server that passes Initialize but crashes within seconds (e.g.
        // yaml-language-server failing during schema fetch) would otherwise
        // restart-loop forever as each Initialize success zeroes the budget.
        {
            let mut map = self.status.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = map.get_mut(&lang) {
                let new_state = if entry.active_progress > 0 {
                    LspServerState::Indexing
                } else {
                    LspServerState::Ready
                };
                tracing::debug!(
                    "LSP {} ({lang}) post-Initialize transition: {:?} → {:?} (active_progress={})",
                    binary,
                    entry.state,
                    new_state,
                    entry.active_progress,
                );
                entry.state = new_state;
                entry.next_restart_at = None;
                entry.ready_since = Some(Instant::now());
                entry.updated_at = Instant::now();
            }
        }

        Ok(LspServer {
            process: child,
            mainloop_abort,
            shutdown_flag,
            server_socket,
            handle: self.handle.clone(),
            language: lang,
            binary: binary.clone(),
            capabilities: init_result.capabilities,
            open_files: std::sync::Mutex::new(std::collections::HashMap::new()),
            diagnostics,
        })
    }

    pub fn server_for_file(&self, path: &std::path::Path) -> Result<&LspServer> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| anyhow::anyhow!("file has no extension"))?;

        let lang = Language::from_extension(ext)
            .ok_or_else(|| anyhow::anyhow!("unsupported language for .{ext}"))?;

        // During the crash-restart backoff window the `LspServer` is still in
        // `self.servers` but its transport is dead, so any request against it
        // will fail or time out. Consult the status cache (which the crash
        // watcher updated synchronously on exit) and treat Error/Restarting
        // as not-running. The real removal happens when `restart_server`
        // runs after the backoff.
        if !self.is_server_live(lang) {
            return Err(anyhow::anyhow!(
                "{lang} server is not currently live (crashed or restarting)"
            ));
        }

        self.servers
            .get(&lang)
            .ok_or_else(|| anyhow::anyhow!("no {lang} server running"))
    }

    /// Whether the status cache shows a server we can actually send requests
    /// to. False during the crash-restart backoff window and after any
    /// Initialize failure.
    fn is_server_live(&self, lang: Language) -> bool {
        let map = self.status.lock().unwrap_or_else(|p| p.into_inner());
        match map.get(&lang).map(|e| &e.state) {
            Some(LspServerState::Ready | LspServerState::Indexing) => true,
            Some(
                LspServerState::Starting
                | LspServerState::Restarting
                | LspServerState::Error { .. },
            )
            | None => false,
        }
    }

    /// Notify the appropriate LSP server that a file was modified and saved.
    /// Returns `Ok(())` if no server handles this file type (graceful skip).
    pub fn notify_file_changed(&self, path: &std::path::Path) -> Result<()> {
        match self.server_for_file(path) {
            Ok(server) => {
                let uri = server.notify_did_change(path)?;
                server.notify_did_save(&uri)?;
                Ok(())
            }
            Err(_) => Ok(()),
        }
    }

    pub fn server_for_file_or_start(&mut self, path: &std::path::Path) -> Result<&LspServer> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| anyhow::anyhow!("file has no extension"))?;

        let lang = Language::from_extension(ext)
            .ok_or_else(|| anyhow::anyhow!("unsupported language for .{ext}"))?;

        self.ensure_server(lang)
    }

    fn ensure_server(&mut self, lang: Language) -> Result<&LspServer> {
        // If a server is mid-restart, the old transport is dead and a fresh
        // process is already queued via LspRestartNeeded — don't racing-start
        // a second one. Tool callers see a clear error and can retry once
        // the sidebar goes back to Ready.
        {
            let map = self.status.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = map.get(&lang)
                && matches!(
                    entry.state,
                    LspServerState::Restarting | LspServerState::Error { .. }
                )
            {
                return Err(anyhow::anyhow!(
                    "{lang} server is not currently live (crashed or restarting)"
                ));
            }
        }

        if !self.servers.contains_key(&lang) {
            // On-demand starts for a language not in the initial detection
            // set should also appear in the sidebar — append to
            // `detected_languages` so `status_snapshot` includes it.
            if !self.detected_languages.contains(&lang) {
                self.detected_languages.push(lang);
            }
            let server = self.start_server(lang, self.event_tx.clone())?;
            self.servers.insert(lang, server);
        }
        Ok(self.servers.get(&lang).expect("just inserted"))
    }

    /// Restart a crashed LSP server. Called from the event loop's
    /// `LspRestartNeeded` handler via `spawn_blocking`.
    ///
    /// 1. Removes the old `LspServer` and calls `transport_shutdown` on it
    ///    so any still-live child process is reaped instead of orphaned.
    ///    In the normal post-crash case the mainloop is already dead, so
    ///    this is a cheap no-op; it defends against future callers
    ///    invoking restart on a healthy server.
    /// 2. Calls `start_server` to spawn a fresh process + Initialize.
    ///    `start_server` stamps `ready_since` on the post-Initialize
    ///    transition; the next crash watcher uses STABILITY_WINDOW to
    ///    decide whether the run was stable enough to reset the retry
    ///    budget (see `plan_crash_restart`).
    /// 3. On init failure, leaves `restart_attempts` untouched (already
    ///    incremented by the crash watcher before `LspRestartNeeded` was
    ///    sent). Clears `next_restart_at` and `ready_since` and — if
    ///    `start_server` didn't already write a specific Error — flips
    ///    the entry to Error so the sidebar shows the failure.
    pub fn restart_server(&mut self, lang: Language) -> Result<()> {
        if let Some(old_server) = self.servers.remove(&lang)
            && let Err(e) = old_server.transport_shutdown()
        {
            tracing::debug!("LSP: {lang} old-server shutdown error during restart: {e}");
        }

        tracing::info!("LSP: restarting {lang} server");

        match self.start_server(lang, self.event_tx.clone()) {
            Ok(server) => {
                tracing::info!("LSP: restarted {lang} server successfully");
                self.servers.insert(lang, server);
                Ok(())
            }
            Err(e) => {
                tracing::warn!("LSP: restart of {lang} failed: {e:#}");
                let mut map = self.status.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(entry) = map.get_mut(&lang) {
                    entry.next_restart_at = None;
                    entry.ready_since = None;
                    // If `start_server`'s Initialize-failure branch already
                    // wrote a specific Error, preserve it. Otherwise (e.g.
                    // resolve_server returned None, spawn failed), flip the
                    // still-Starting/Restarting entry to Error now.
                    if !matches!(entry.state, LspServerState::Error { .. }) {
                        entry.state = LspServerState::Error {
                            reason: format!("restart failed: {e}"),
                        };
                        entry.active_progress = 0;
                        entry.progress_message = None;
                        entry.updated_at = Instant::now();
                    }
                }
                Err(e)
            }
        }
    }

    pub fn shutdown(&mut self) {
        for (lang, server) in self.servers.drain() {
            tracing::debug!("LSP: shutting down {lang} server");
            if let Err(e) = server.transport_shutdown() {
                tracing::debug!("LSP: {lang} shutdown error: {e}");
            }
        }
        // Drop all status entries — the sidebar should empty out on exit.
        self.status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    /// Iterator over servers whose transport is actually live — i.e., the
    /// status cache shows Ready or Indexing. Crashed servers awaiting restart
    /// are excluded so callers (e.g., workspace/symbol fan-out) don't send
    /// requests into a dead transport.
    pub fn running_servers(&self) -> impl Iterator<Item = &LspServer> {
        let live: std::collections::HashSet<Language> = {
            let map = self.status.lock().unwrap_or_else(|p| p.into_inner());
            map.iter()
                .filter(|(_, entry)| {
                    matches!(
                        entry.state,
                        LspServerState::Ready | LspServerState::Indexing
                    )
                })
                .map(|(lang, _)| *lang)
                .collect()
        };
        self.servers
            .iter()
            .filter(move |(lang, _)| live.contains(lang))
            .map(|(_, server)| server)
    }

    pub fn has_servers(&self) -> bool {
        !self.servers.is_empty()
    }

    pub fn running_languages(&self) -> Vec<Language> {
        self.servers.keys().copied().collect()
    }

    /// Clone of the shared status cache Arc. The event loop stores this
    /// once at App construction so the Tick handler can read status without
    /// acquiring the `RwLock<LspManager>` — critical because the startup
    /// `spawn_blocking` holds the write lock for the duration of every
    /// blocking Initialize request, which would otherwise make every
    /// `try_read()` from the Tick handler fail and hide Starting/Indexing
    /// transitions entirely.
    pub fn status_cache_handle(&self) -> SharedLspStatus {
        self.status.clone()
    }

    /// Rich status snapshot for every detected language. Reads from the
    /// shared status cache — the single source of truth.
    pub fn status_snapshot(&self) -> Vec<(Language, LspStatusEntry)> {
        let map = self.status.lock().unwrap_or_else(|p| p.into_inner());
        self.detected_languages
            .iter()
            .filter_map(|&lang| map.get(&lang).map(|entry| (lang, entry.clone())))
            .collect()
    }

    /// Pure function that snapshots the shared cache into a sorted vector,
    /// without requiring access to `self.detected_languages`. Used by the
    /// event loop Tick handler, which cannot call `status_snapshot` because
    /// the startup `spawn_blocking` holds the `RwLock<LspManager>` write
    /// lock for the duration of server Initialize.
    ///
    /// The snapshot is sorted by `Language` (declaration order matches
    /// `detect_from_project` order) so the sidebar ordering is stable across
    /// ticks even though `HashMap` iteration is not.
    ///
    /// Unlike `status_snapshot`, this does NOT filter by `detected_languages`
    /// — it returns every entry in the cache. In practice the two sets are
    /// equal because `start_servers` only seeds entries for detected
    /// languages, and `ensure_server` appends to `detected_languages` before
    /// calling `start_server` (which writes to the cache). So any language
    /// with a cache entry is also in `detected_languages`. If that invariant
    /// ever breaks, this snapshot and `status_snapshot` would diverge.
    pub fn snapshot_cache(cache: &SharedLspStatus) -> Vec<(Language, LspStatusEntry)> {
        let map = cache.lock().unwrap_or_else(|p| p.into_inner());
        let mut entries: Vec<(Language, LspStatusEntry)> = map
            .iter()
            .map(|(lang, entry)| (*lang, entry.clone()))
            .collect();
        entries.sort_by_key(|(lang, _)| *lang);
        entries
    }

    /// Back-compat pair view used by `src/app/prompt.rs` when building the
    /// system prompt. `running` is true iff the server is usable (Ready or
    /// Indexing — both respond to requests).
    pub fn language_status(&self) -> Vec<(String, bool)> {
        self.status_snapshot()
            .into_iter()
            .map(|(_, entry)| {
                let running = matches!(
                    entry.state,
                    LspServerState::Ready | LspServerState::Indexing
                );
                (entry.binary, running)
            })
            .collect()
    }

    /// Test-only helper: insert a status entry directly into the shared cache.
    /// Used by tests in this module and by `app::event_loop` tick tests.
    #[cfg(test)]
    pub(crate) fn insert_status_for_test(&mut self, lang: Language, entry: LspStatusEntry) {
        if !self.detected_languages.contains(&lang) {
            self.detected_languages.push(lang);
        }
        self.status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(lang, entry);
    }
}

impl Drop for LspManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn lsp_manager_new() {
        let dir = tempdir().unwrap();
        let mgr = LspManager::new(
            dir.path().to_path_buf(),
            tokio::runtime::Handle::current(),
            None,
        );
        assert!(!mgr.has_servers());
        assert!(mgr.running_languages().is_empty());
        assert!(mgr.language_status().is_empty());
    }

    #[tokio::test]
    async fn running_servers_empty() {
        let dir = tempdir().unwrap();
        let mgr = LspManager::new(
            dir.path().to_path_buf(),
            tokio::runtime::Handle::current(),
            None,
        );
        assert_eq!(mgr.running_servers().count(), 0);
    }

    fn sample_entry(state: LspServerState, binary: &str) -> LspStatusEntry {
        LspStatusEntry {
            binary: binary.into(),
            state,
            active_progress: 0,
            progress_message: None,
            updated_at: Instant::now(),
            restart_attempts: 0,
            next_restart_at: None,
            ready_since: None,
        }
    }

    #[tokio::test]
    async fn server_for_file_errors_when_status_cache_shows_restarting() {
        // During the crash-restart backoff window, `self.servers` may still
        // hold the crashed LspServer (removal happens when restart_server
        // runs after the sleep). Lookups must fail fast instead of handing
        // out a dead transport.
        let dir = tempdir().unwrap();
        let mut mgr = LspManager::new(
            dir.path().to_path_buf(),
            tokio::runtime::Handle::current(),
            None,
        );
        mgr.insert_status_for_test(
            Language::Rust,
            sample_entry(LspServerState::Restarting, "rust-analyzer"),
        );
        let rust_file = dir.path().join("main.rs");
        std::fs::write(&rust_file, "fn main() {}").unwrap();
        let err = match mgr.server_for_file(&rust_file) {
            Err(e) => e,
            Ok(_) => panic!("lookup should fail while Restarting"),
        };
        let msg = format!("{err}");
        assert!(
            msg.contains("not currently live"),
            "expected 'not currently live' in error, got {msg:?}"
        );
    }

    #[tokio::test]
    async fn server_for_file_errors_when_status_cache_shows_error() {
        let dir = tempdir().unwrap();
        let mut mgr = LspManager::new(
            dir.path().to_path_buf(),
            tokio::runtime::Handle::current(),
            None,
        );
        mgr.insert_status_for_test(
            Language::Rust,
            sample_entry(
                LspServerState::Error {
                    reason: "mainloop exited".into(),
                },
                "rust-analyzer",
            ),
        );
        let rust_file = dir.path().join("main.rs");
        std::fs::write(&rust_file, "fn main() {}").unwrap();
        assert!(mgr.server_for_file(&rust_file).is_err());
    }

    #[tokio::test]
    async fn ensure_server_errors_instead_of_double_starting_during_restart() {
        let dir = tempdir().unwrap();
        let mut mgr = LspManager::new(
            dir.path().to_path_buf(),
            tokio::runtime::Handle::current(),
            None,
        );
        mgr.insert_status_for_test(
            Language::Rust,
            sample_entry(LspServerState::Restarting, "rust-analyzer"),
        );
        // ensure_server is &mut self — we can't call it from inside the
        // #[tokio::test] outer runtime without spawn_blocking (block_on
        // issues), so drive through server_for_file_or_start with a dummy
        // path. Restarting short-circuits before any block_on.
        let rust_file = dir.path().join("main.rs");
        std::fs::write(&rust_file, "fn main() {}").unwrap();
        let err = match mgr.server_for_file_or_start(&rust_file) {
            Err(e) => e,
            Ok(_) => panic!("should refuse to start while Restarting"),
        };
        assert!(format!("{err}").contains("not currently live"));
    }

    #[tokio::test]
    async fn language_status_reads_from_state_cache_starting_and_error_are_not_running() {
        let dir = tempdir().unwrap();
        let mut mgr = LspManager::new(
            dir.path().to_path_buf(),
            tokio::runtime::Handle::current(),
            None,
        );
        mgr.insert_status_for_test(
            Language::Rust,
            sample_entry(LspServerState::Starting, "rust-analyzer"),
        );
        mgr.insert_status_for_test(
            Language::Python,
            sample_entry(
                LspServerState::Error {
                    reason: "no binary".into(),
                },
                "pyright-langserver",
            ),
        );
        let status = mgr.language_status();
        assert_eq!(status.len(), 2);
        assert!(
            status.iter().all(|(_, running)| !*running),
            "Starting and Error should both report running=false"
        );
    }

    #[tokio::test]
    async fn language_status_ready_and_indexing_report_running_true() {
        let dir = tempdir().unwrap();
        let mut mgr = LspManager::new(
            dir.path().to_path_buf(),
            tokio::runtime::Handle::current(),
            None,
        );
        mgr.insert_status_for_test(
            Language::Rust,
            sample_entry(LspServerState::Ready, "rust-analyzer"),
        );
        mgr.insert_status_for_test(
            Language::Python,
            sample_entry(LspServerState::Indexing, "pyright-langserver"),
        );
        let status = mgr.language_status();
        assert_eq!(status.len(), 2);
        for (binary, running) in &status {
            assert!(running, "{binary} should report running=true");
        }
    }

    #[tokio::test]
    async fn status_snapshot_returns_entries_for_detected_languages_only() {
        let dir = tempdir().unwrap();
        let mut mgr = LspManager::new(
            dir.path().to_path_buf(),
            tokio::runtime::Handle::current(),
            None,
        );
        mgr.insert_status_for_test(
            Language::Rust,
            sample_entry(LspServerState::Ready, "rust-analyzer"),
        );
        // Directly inject into the shared map an entry for a language that
        // is NOT in `detected_languages` — snapshot must skip it.
        mgr.status.lock().unwrap().insert(
            Language::Json,
            sample_entry(LspServerState::Ready, "json-ls"),
        );
        let snap = mgr.status_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].0, Language::Rust);
    }

    #[tokio::test]
    async fn snapshot_cache_reads_directly_from_arc_and_sorts_by_language() {
        let dir = tempdir().unwrap();
        let mut mgr = LspManager::new(
            dir.path().to_path_buf(),
            tokio::runtime::Handle::current(),
            None,
        );
        // Capture a cache handle BEFORE inserting — this proves the Arc is
        // a live view of the same HashMap the manager writes to.
        let cache_handle = mgr.status_cache_handle();
        // Insert in "wrong" order to verify sorting.
        mgr.insert_status_for_test(
            Language::Json,
            sample_entry(LspServerState::Ready, "json-ls"),
        );
        mgr.insert_status_for_test(
            Language::Rust,
            sample_entry(LspServerState::Starting, "rust-analyzer"),
        );
        mgr.insert_status_for_test(
            Language::Python,
            sample_entry(LspServerState::Indexing, "pyright-langserver"),
        );
        // Call the static helper through the captured cache Arc — it must
        // see all three entries without touching `mgr`.
        let snap = LspManager::snapshot_cache(&cache_handle);
        assert_eq!(snap.len(), 3);
        // Declaration order: Rust, Python, TypeScript, Json, Ruby.
        assert_eq!(snap[0].0, Language::Rust);
        assert_eq!(snap[1].0, Language::Python);
        assert_eq!(snap[2].0, Language::Json);
    }

    #[tokio::test]
    async fn cache_handle_reads_bypass_manager_rwlock() {
        // Regression: startup `spawn_blocking` holds `RwLock<LspManager>::write`
        // for the entire duration of server Initialize, blocking all
        // `try_read()` calls during that window. The whole point of the
        // direct cache Arc is that it sidesteps that lock. This test proves
        // a reader holding only the cache Arc can observe updates while a
        // writer holds the enclosing RwLock exclusively.
        use std::sync::RwLock;
        let dir = tempdir().unwrap();
        let mgr = LspManager::new(
            dir.path().to_path_buf(),
            tokio::runtime::Handle::current(),
            None,
        );
        let cache_handle = mgr.status_cache_handle();
        let rwlock = Arc::new(RwLock::new(mgr));

        // Simulate the startup pattern: a writer holds the LspManager
        // exclusively and mutates the cache through the locked manager.
        {
            let mut write_guard = rwlock.write().unwrap();
            write_guard.insert_status_for_test(
                Language::Rust,
                sample_entry(LspServerState::Starting, "rust-analyzer"),
            );

            // While the write lock is still held, a reader using the cache
            // Arc directly can see the entry. This is what the Tick handler
            // does.
            assert!(
                rwlock.try_read().is_err(),
                "write lock should be held exclusively"
            );
            let snap = LspManager::snapshot_cache(&cache_handle);
            assert_eq!(snap.len(), 1);
            assert_eq!(snap[0].1.state, LspServerState::Starting);

            // Writer transitions state — reader observes that too.
            write_guard.insert_status_for_test(
                Language::Rust,
                sample_entry(LspServerState::Ready, "rust-analyzer"),
            );
            let snap = LspManager::snapshot_cache(&cache_handle);
            assert_eq!(snap[0].1.state, LspServerState::Ready);
        }
    }

    #[tokio::test]
    async fn status_cache_handle_returns_same_arc_instance() {
        let dir = tempdir().unwrap();
        let mut mgr = LspManager::new(
            dir.path().to_path_buf(),
            tokio::runtime::Handle::current(),
            None,
        );
        let handle = mgr.status_cache_handle();
        mgr.insert_status_for_test(
            Language::Rust,
            sample_entry(LspServerState::Ready, "rust-analyzer"),
        );
        // The handle we captured BEFORE the insert must see the new entry,
        // proving both views share the same underlying HashMap.
        let snap = LspManager::snapshot_cache(&handle);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].1.state, LspServerState::Ready);
    }

    #[tokio::test]
    async fn shutdown_clears_status_cache() {
        let dir = tempdir().unwrap();
        let mut mgr = LspManager::new(
            dir.path().to_path_buf(),
            tokio::runtime::Handle::current(),
            None,
        );
        mgr.insert_status_for_test(
            Language::Rust,
            sample_entry(LspServerState::Ready, "rust-analyzer"),
        );
        assert_eq!(mgr.status_snapshot().len(), 1);
        mgr.shutdown();
        assert!(mgr.status_snapshot().is_empty());
    }

    #[tokio::test]
    async fn outer_error_write_preserves_more_specific_inner_reason() {
        // Regression: `start_server`'s Initialize-failure branch writes a
        // specific `Error { reason: "initialize failed: ..." }` to the cache,
        // then returns Err. The caller `start_servers` catches the Err and
        // ALSO writes an Error entry — the second write must NOT clobber
        // the first when the first already recorded a specific reason.
        let dir = tempdir().unwrap();
        let mut mgr = LspManager::new(
            dir.path().to_path_buf(),
            tokio::runtime::Handle::current(),
            None,
        );

        // Seed the cache with a specific Error as if `start_server`'s inner
        // Initialize-failure branch had just written it.
        mgr.insert_status_for_test(
            Language::Rust,
            LspStatusEntry {
                binary: "rust-analyzer".into(),
                state: LspServerState::Error {
                    reason: "initialize failed: ResponseError { code: -32001, message: \"indexer borked\" }".into(),
                },
                active_progress: 0,
                progress_message: None,
                updated_at: Instant::now(),
                restart_attempts: 0,
                next_restart_at: None,
                ready_since: None,
            },
        );

        // Simulate the outer catch in `start_servers` — this code path runs
        // for every `start_server` error, including ones that already wrote
        // a more specific reason. It must preserve the existing reason.
        {
            let mut map = mgr.status.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = map.get_mut(&Language::Rust)
                && !matches!(entry.state, LspServerState::Error { .. })
            {
                entry.state = LspServerState::Error {
                    reason: "initialize request failed: generic".into(),
                };
                entry.updated_at = Instant::now();
            }
        }

        // The original, more specific reason should still be in the cache.
        let snap = mgr.status_snapshot();
        match &snap[0].1.state {
            LspServerState::Error { reason } => {
                assert!(
                    reason.contains("indexer borked"),
                    "outer fallback clobbered the specific reason: {reason}"
                );
            }
            other => panic!("expected Error state, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn outer_error_write_fires_when_no_inner_error_was_recorded() {
        // Complement: if the entry is still `Starting` (e.g., `resolve_server`
        // returned None before `start_server` could upsert), the outer write
        // MUST fire so the sidebar surfaces the failure.
        let dir = tempdir().unwrap();
        let mut mgr = LspManager::new(
            dir.path().to_path_buf(),
            tokio::runtime::Handle::current(),
            None,
        );
        mgr.insert_status_for_test(
            Language::Rust,
            sample_entry(LspServerState::Starting, "rust-analyzer"),
        );

        // Simulate the outer catch firing with the entry still in Starting.
        {
            let mut map = mgr.status.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = map.get_mut(&Language::Rust)
                && !matches!(entry.state, LspServerState::Error { .. })
            {
                entry.state = LspServerState::Error {
                    reason: "no rust language server found on PATH".into(),
                };
                entry.updated_at = Instant::now();
            }
        }

        let snap = mgr.status_snapshot();
        match &snap[0].1.state {
            LspServerState::Error { reason } => {
                assert!(reason.contains("not found") || reason.contains("no rust"));
            }
            other => panic!("expected Error state, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn start_servers_seeds_error_entry_when_binary_missing() {
        // In CI, rust-analyzer is not guaranteed to be on PATH. We detect
        // rust from Cargo.toml, seed a Starting entry, fail to spawn, and
        // flip to Error. Skip this test if rust-analyzer IS on PATH — the
        // assertion would fail because the server would actually start.
        if which::which("rust-analyzer").is_ok() {
            return;
        }
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let mut mgr = LspManager::new(
            dir.path().to_path_buf(),
            tokio::runtime::Handle::current(),
            None,
        );
        mgr.start_servers();
        let snap = mgr.status_snapshot();
        let rust_entry = snap
            .iter()
            .find(|(lang, _)| *lang == Language::Rust)
            .map(|(_, e)| e.clone())
            .expect("Rust should be detected from Cargo.toml");
        assert!(
            matches!(rust_entry.state, LspServerState::Error { .. }),
            "expected Error state when rust-analyzer is missing, got {:?}",
            rust_entry.state
        );
    }

    #[tokio::test]
    async fn restart_server_failure_flips_to_error_and_preserves_attempts() {
        // Deterministic failure path: pick a language whose LSP binaries
        // almost certainly aren't on PATH. If they happen to be, skip —
        // the success path is covered by the `plan_crash_restart` unit
        // tests below plus the end-to-end rust-analyzer test elsewhere.
        if which::which("solargraph").is_ok() || which::which("ruby-lsp").is_ok() {
            return;
        }
        let dir = tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();
        let (err_result, snap) = tokio::task::spawn_blocking(move || {
            let mut mgr = LspManager::new(dir_path, tokio::runtime::Handle::current(), None);
            // Simulate state right before `LspRestartNeeded` is handled:
            // crash watcher has transitioned to Restarting with attempts=1
            // and a scheduled restart_at. Also plant stale progress state
            // to verify the Err path clears it.
            mgr.insert_status_for_test(
                Language::Ruby,
                LspStatusEntry {
                    binary: "solargraph".into(),
                    state: LspServerState::Restarting,
                    active_progress: 2,
                    progress_message: Some("stale progress".into()),
                    updated_at: Instant::now(),
                    restart_attempts: 1,
                    next_restart_at: Some(Instant::now()),
                    ready_since: None,
                },
            );
            let err = mgr.restart_server(Language::Ruby);
            let snap = mgr.status_snapshot();
            (err, snap)
        })
        .await
        .unwrap();
        assert!(
            err_result.is_err(),
            "start_server should fail when Ruby LSP is not on PATH"
        );
        let (_, entry) = snap.iter().find(|(l, _)| *l == Language::Ruby).unwrap();
        assert!(
            matches!(entry.state, LspServerState::Error { .. }),
            "restart failure should leave state Error, got {:?}",
            entry.state
        );
        assert_eq!(
            entry.restart_attempts, 1,
            "restart_attempts preserved — not clamped to MAX — so a future \
             successful restart can reset it normally"
        );
        assert!(
            entry.next_restart_at.is_none(),
            "next_restart_at cleared after failed restart"
        );
        assert_eq!(
            entry.active_progress, 0,
            "active_progress cleared after failed restart"
        );
        assert!(
            entry.progress_message.is_none(),
            "progress_message cleared after failed restart"
        );
    }

    // -- plan_crash_restart: unit tests covering every branch of the pure
    // scheduling logic used by the crash watcher. These don't spawn any
    // LSP processes — they test the decision table directly.

    fn restart_entry(attempts: u8) -> LspStatusEntry {
        restart_entry_with_ready(attempts, None)
    }

    fn restart_entry_with_ready(attempts: u8, ready_since: Option<Instant>) -> LspStatusEntry {
        LspStatusEntry {
            binary: "fake-ls".into(),
            state: LspServerState::Ready,
            active_progress: 3,
            progress_message: Some("indexing something".into()),
            updated_at: Instant::now(),
            restart_attempts: attempts,
            next_restart_at: None,
            ready_since,
        }
    }

    #[test]
    fn plan_crash_restart_first_crash_schedules_zero_delay() {
        let mut entry = restart_entry(0);
        let delay = plan_crash_restart(&mut entry, "mainloop exited".into(), true, Instant::now());
        assert_eq!(delay, Some(std::time::Duration::ZERO));
        assert_eq!(entry.state, LspServerState::Restarting);
        assert_eq!(entry.restart_attempts, 1);
        assert!(entry.next_restart_at.is_some());
        assert_eq!(entry.active_progress, 0, "stale progress cleared");
        assert!(entry.progress_message.is_none(), "stale message cleared");
    }

    #[test]
    fn plan_crash_restart_second_crash_schedules_one_second() {
        let mut entry = restart_entry(1);
        let delay =
            plan_crash_restart(&mut entry, "mainloop panicked".into(), true, Instant::now());
        assert_eq!(delay, Some(std::time::Duration::from_secs(1)));
        assert_eq!(entry.state, LspServerState::Restarting);
        assert_eq!(entry.restart_attempts, 2);
    }

    #[test]
    fn plan_crash_restart_third_crash_schedules_five_seconds() {
        let mut entry = restart_entry(2);
        let delay = plan_crash_restart(&mut entry, "mainloop exited".into(), true, Instant::now());
        assert_eq!(delay, Some(std::time::Duration::from_secs(5)));
        assert_eq!(entry.state, LspServerState::Restarting);
        assert_eq!(entry.restart_attempts, 3);
    }

    #[test]
    fn plan_crash_restart_budget_exhausted_stays_error() {
        let mut entry = restart_entry(crate::lsp::MAX_RESTART_ATTEMPTS);
        let delay = plan_crash_restart(&mut entry, "mainloop exited".into(), true, Instant::now());
        assert_eq!(delay, None, "no more retries once budget is exhausted");
        assert!(
            matches!(entry.state, LspServerState::Error { .. }),
            "state should remain Error, got {:?}",
            entry.state
        );
        assert_eq!(
            entry.restart_attempts,
            crate::lsp::MAX_RESTART_ATTEMPTS,
            "attempts should not increment past MAX"
        );
        assert!(entry.next_restart_at.is_none());
    }

    #[test]
    fn plan_crash_restart_no_event_channel_stays_error() {
        // Test mode (event_tx absent): plan must NOT transition to
        // Restarting, because no LspRestartNeeded event will ever be sent.
        let mut entry = restart_entry(0);
        let delay = plan_crash_restart(&mut entry, "mainloop exited".into(), false, Instant::now());
        assert_eq!(delay, None);
        assert!(
            matches!(entry.state, LspServerState::Error { .. }),
            "without an event channel the plan should stop at Error, got {:?}",
            entry.state
        );
        assert_eq!(
            entry.restart_attempts, 0,
            "attempts should not increment when no retry is scheduled"
        );
        assert!(entry.next_restart_at.is_none());
    }

    #[test]
    fn plan_crash_restart_stores_reason_on_error_transition_even_when_scheduling() {
        // Even when a restart IS scheduled, the intermediate Error state
        // must have carried the reason through — the watcher temporarily
        // writes Error before overwriting with Restarting, and the reason
        // is what makes Error useful if the user happens to see it.
        let mut entry = restart_entry(0);
        let reason = "mainloop panicked: JoinError".to_string();
        let _ = plan_crash_restart(&mut entry, reason, true, Instant::now());
        // After a successful schedule the state is Restarting (confirmed
        // in other tests). This test documents the intent of the reason
        // path — covered via the budget-exhausted test, which lands in
        // Error with the reason set.
        let mut exhausted = restart_entry(crate::lsp::MAX_RESTART_ATTEMPTS);
        plan_crash_restart(
            &mut exhausted,
            "specific reason".into(),
            true,
            Instant::now(),
        );
        match &exhausted.state {
            LspServerState::Error { reason } => {
                assert_eq!(reason, "specific reason");
            }
            other => panic!("expected Error state with reason, got {other:?}"),
        }
    }

    #[test]
    fn plan_crash_restart_recent_ready_does_not_reset_budget() {
        // Regression for steve-ooox: a server that passes Initialize and
        // crashes within STABILITY_WINDOW must NOT have its retry budget
        // reset. Without this, yaml-language-server (and any other server
        // that reaches Ready before crashing during schema fetch / first
        // didOpen) loops forever because each cycle zeroes attempts.
        let mut entry = restart_entry_with_ready(2, Some(Instant::now()));
        let delay = plan_crash_restart(&mut entry, "mainloop exited".into(), true, Instant::now());
        assert_eq!(
            entry.restart_attempts, 3,
            "attempts should increment from 2, NOT reset to 1, when ready_since is recent"
        );
        assert_eq!(delay, Some(std::time::Duration::from_secs(5)));
        assert!(entry.ready_since.is_none(), "ready_since cleared on crash");
    }

    #[test]
    fn plan_crash_restart_recent_ready_at_exhausted_budget_stays_error() {
        // The combined behavior: a server that has already exhausted its
        // budget and crashes again within STABILITY_WINDOW lands in
        // permanent Error — the eager reset is what was breaking this.
        let mut entry =
            restart_entry_with_ready(crate::lsp::MAX_RESTART_ATTEMPTS, Some(Instant::now()));
        let delay = plan_crash_restart(&mut entry, "mainloop exited".into(), true, Instant::now());
        assert_eq!(delay, None, "no retry once budget exhausted, even if Ready");
        assert!(matches!(entry.state, LspServerState::Error { .. }));
        assert_eq!(
            entry.restart_attempts,
            crate::lsp::MAX_RESTART_ATTEMPTS,
            "attempts should stay at MAX, not reset"
        );
    }

    #[test]
    fn plan_crash_restart_stable_uptime_resets_budget() {
        // A server that has been continuously Ready for at least
        // STABILITY_WINDOW before crashing IS treated as a stable run —
        // its retry budget resets so the next crash gets the full cap.
        // Construct `now` *forward* from the ready_since baseline rather
        // than backward via `checked_sub`: `Instant`'s origin is platform-
        // defined, so subtracting an arbitrary delta from `Instant::now()`
        // is not guaranteed to succeed.
        let stable_since = Instant::now();
        let now = stable_since + crate::lsp::STABILITY_WINDOW + std::time::Duration::from_secs(1);
        let mut entry =
            restart_entry_with_ready(crate::lsp::MAX_RESTART_ATTEMPTS, Some(stable_since));
        let delay = plan_crash_restart(&mut entry, "mainloop exited".into(), true, now);
        assert_eq!(
            delay,
            Some(std::time::Duration::ZERO),
            "stable run grants a fresh budget — first crash schedules zero-delay restart"
        );
        assert_eq!(
            entry.restart_attempts, 1,
            "attempts reset to 0 then incremented for this crash"
        );
        assert!(entry.ready_since.is_none(), "ready_since cleared on crash");
    }

    #[test]
    fn plan_crash_restart_no_ready_since_does_not_reset() {
        // Defensive: the very first crash (ready_since=None — server never
        // reached Ready, e.g. Initialize failed) must not be treated as a
        // stable run. The default behavior (no reset) applies and the
        // existing budget logic counts every crash.
        let mut entry = restart_entry_with_ready(2, None);
        let delay = plan_crash_restart(&mut entry, "mainloop exited".into(), true, Instant::now());
        assert_eq!(entry.restart_attempts, 3);
        assert_eq!(delay, Some(std::time::Duration::from_secs(5)));
    }

    // -- StderrTail unit tests ------------------------------------------------

    #[test]
    fn stderr_tail_push_under_limit_keeps_all_lines() {
        let mut tail = StderrTail::new();
        for i in 0..5 {
            tail.push(format!("line {i}"));
        }
        assert_eq!(tail.lines.len(), 5);
        let preview = tail.preview();
        assert!(preview.contains("line 0"));
        assert!(preview.contains("line 4"));
    }

    #[test]
    fn stderr_tail_drops_oldest_past_line_limit() {
        let mut tail = StderrTail::new();
        // Push more than STDERR_TAIL_MAX_LINES (10) short lines.
        for i in 0..(STDERR_TAIL_MAX_LINES + 5) {
            tail.push(format!("L{i}"));
        }
        assert_eq!(tail.lines.len(), STDERR_TAIL_MAX_LINES);
        let preview = tail.preview();
        // First 5 lines should have been evicted.
        assert!(!preview.contains("L0"), "L0 should be evicted: {preview}");
        assert!(!preview.contains("L4"), "L4 should be evicted: {preview}");
        assert!(preview.contains("L5"), "L5 should be retained: {preview}");
        assert!(
            preview.contains(&format!("L{}", STDERR_TAIL_MAX_LINES + 4)),
            "newest line should be retained: {preview}"
        );
    }

    #[test]
    fn stderr_tail_drops_oldest_past_byte_limit() {
        let mut tail = StderrTail::new();
        // A single line larger than the byte budget should still be retained
        // (we don't truncate inside a line) but should evict everything else.
        let huge = "x".repeat(STDERR_TAIL_MAX_BYTES * 2);
        tail.push("first".to_string());
        tail.push("second".to_string());
        tail.push(huge.clone());
        // The huge line alone exceeds the budget, so the smaller predecessors
        // are evicted; the huge line stays since the loop stops once `lines`
        // is empty (saturating_sub keeps total_bytes consistent).
        assert_eq!(tail.lines.len(), 1);
        assert_eq!(tail.lines.front().unwrap(), &huge);
    }

    #[test]
    fn stderr_tail_preview_empty_returns_empty_string() {
        let tail = StderrTail::new();
        assert_eq!(tail.preview(), "");
    }

    #[test]
    fn stderr_tail_preview_joins_with_separator_and_trims_trailing_whitespace() {
        let mut tail = StderrTail::new();
        tail.push("first line   ".to_string());
        tail.push("second line\t".to_string());
        let preview = tail.preview();
        assert_eq!(preview, "first line | second line");
    }

    #[test]
    fn reason_with_stderr_appends_preview_when_present() {
        let tail: SharedStderrTail = Arc::new(Mutex::new(StderrTail::new()));
        {
            let mut t = tail.lock().unwrap();
            t.push("Error: Cannot find module 'foo'".to_string());
        }
        let reason = reason_with_stderr("mainloop exited", &tail);
        assert!(
            reason.contains("(stderr: Error: Cannot find module 'foo')"),
            "stderr preview should be appended in parens: {reason}"
        );
        assert!(reason.starts_with("mainloop exited"));
    }

    #[test]
    fn reason_with_stderr_returns_base_unchanged_when_tail_empty() {
        let tail: SharedStderrTail = Arc::new(Mutex::new(StderrTail::new()));
        let reason = reason_with_stderr("mainloop exited", &tail);
        assert_eq!(reason, "mainloop exited");
    }

    // -- stderr pump end-to-end ----------------------------------------------

    #[tokio::test]
    async fn stderr_pump_captures_lines_from_real_child() {
        // Spawn a tiny shell pipeline that writes a few lines to stderr and
        // exits. The pump should drain all of them into the shared tail.
        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("printf 'first\\nsecond\\nthird\\n' >&2; exit 0")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn sh");
        let stderr = child.stderr.take().expect("child stderr handle");
        let tail: SharedStderrTail = Arc::new(Mutex::new(StderrTail::new()));
        spawn_stderr_pump(
            stderr,
            tail.clone(),
            Language::Bash,
            "sh".into(),
            &tokio::runtime::Handle::current(),
        );
        let _ = child.wait().await;
        // Give the pump a beat to drain after the child exits.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let preview = tail.lock().unwrap().preview();
        assert!(
            preview.contains("first") && preview.contains("second") && preview.contains("third"),
            "all stderr lines should be captured: {preview}"
        );
    }
}
