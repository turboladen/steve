//! LSP client service for async-lsp integration.
//!
//! Provides `SteveLspService` — a Router-based handler for server-initiated
//! messages (notifications like `publishDiagnostics`, requests like
//! `workspace/configuration`). The `MainLoop` drives the service automatically;
//! `ServerSocket` is used by `LspServer` to send requests/notifications.

use std::{
    collections::HashMap,
    ops::ControlFlow,
    sync::{Arc, Mutex},
    time::Instant,
};

use async_lsp::{
    ErrorCode, MainLoop, ResponseError, ServerSocket,
    lsp_types::{
        Diagnostic, MessageType, ProgressParamsValue, Url, WorkDoneProgress, notification, request,
    },
    router::Router,
};

use super::{Language, LspServerState, LspStatusEntry};

/// Shared diagnostics cache — written by the MainLoop service, read by LspServer.
///
/// Uses `std::sync::Mutex` (not `tokio::sync::Mutex`) because the critical section
/// is trivial (a single HashMap insert/lookup) and holding it never blocks the async
/// runtime. This also allows access from both async (MainLoop) and sync (LspServer) contexts.
pub type SharedDiagnostics = Arc<Mutex<HashMap<Url, Vec<Diagnostic>>>>;

/// Shared per-language LSP status cache — written by `LspManager::start_server`,
/// the `$/progress` notification handler, and the crash watcher task; read by
/// `LspManager::snapshot_cache` (sidebar, via a direct Arc clone that bypasses
/// the `RwLock<LspManager>`) and `LspManager::language_status` (system prompt,
/// via the manager). Mirrors the `SharedDiagnostics` pattern.
pub type SharedLspStatus = Arc<Mutex<HashMap<Language, LspStatusEntry>>>;

/// State held by the Router service.
pub(crate) struct ClientState {
    pub diagnostics: SharedDiagnostics,
    pub status: SharedLspStatus,
    pub language: Language,
}

/// Apply a `$/progress` notification to a status entry.
///
/// Extracted as a free function so the transition rules can be unit-tested
/// without standing up a full async-lsp Router. The rules:
///
/// - **Begin**: increment `active_progress`; flip `Ready → Indexing`
///   (but not `Starting → Indexing` — Initialize return handles that flip
///   to avoid clobbering the startup state).
/// - **Report**: update `progress_message` only.
/// - **End**: decrement `active_progress` (saturating — tolerates stray End
///   without matching Begin); flip `Indexing → Ready` only when the counter
///   reaches zero.
///
/// Leaked End notifications keep the server in `Indexing` until the next Begin
/// cycle completes — annoying but not lethal. Counter-based tracking tolerates
/// this gracefully.
pub(crate) fn apply_progress_update(entry: &mut LspStatusEntry, value: ProgressParamsValue) {
    let ProgressParamsValue::WorkDone(wd) = value;
    match wd {
        WorkDoneProgress::Begin(begin) => {
            entry.active_progress = entry.active_progress.saturating_add(1);
            entry.progress_message = Some(begin.title.clone());
            if matches!(entry.state, LspServerState::Ready) {
                entry.state = LspServerState::Indexing;
            }
            entry.updated_at = Instant::now();
        }
        WorkDoneProgress::Report(report) => {
            if let Some(msg) = report.message {
                entry.progress_message = Some(msg);
                entry.updated_at = Instant::now();
            }
        }
        WorkDoneProgress::End(end) => {
            entry.active_progress = entry.active_progress.saturating_sub(1);
            entry.progress_message = end.message;
            if entry.active_progress == 0 && matches!(entry.state, LspServerState::Indexing) {
                entry.state = LspServerState::Ready;
            }
            entry.updated_at = Instant::now();
        }
    }
}

/// Surface an LSP `window/logMessage` / `window/showMessage` /
/// `window/showMessageRequest` payload via `tracing` at a level matching
/// the LSP message type. The macro form is required because `tracing`'s
/// `target:` clause needs a literal at the macro call site — a runtime
/// `&'static str` parameter would not work.
///
/// `MessageType` is a transparent newtype around `i32` (not a Rust
/// enum), so the spec-defined variants are constants and the `_` arm
/// is required for forward-compatibility with any unknown values a
/// non-conforming server might send.
macro_rules! log_lsp_at_typ_level {
    ($target:literal, $lang:expr, $typ:expr, $message:expr) => {{
        let lang = $lang;
        let message = $message;
        match $typ {
            MessageType::ERROR => {
                tracing::error!(target: $target, lang = %lang, "{}", message)
            }
            MessageType::WARNING => {
                tracing::warn!(target: $target, lang = %lang, "{}", message)
            }
            MessageType::INFO => {
                tracing::info!(target: $target, lang = %lang, "{}", message)
            }
            MessageType::LOG => {
                tracing::debug!(target: $target, lang = %lang, "{}", message)
            }
            other => {
                tracing::info!(target: $target, lang = %lang, type_ = ?other, "{}", message)
            }
        }
    }};
}

/// Create an async-lsp client MainLoop + ServerSocket pair.
///
/// The MainLoop should be spawned as a background tokio task via
/// `mainloop.run_buffered(stdout, stdin)`. The ServerSocket is used
/// to send requests and notifications to the language server.
pub(crate) fn create_client(
    diagnostics: SharedDiagnostics,
    status: SharedLspStatus,
    language: Language,
) -> (MainLoop<Router<ClientState>>, ServerSocket) {
    MainLoop::new_client(move |_server_socket| {
        let mut router = Router::new(ClientState {
            diagnostics,
            status,
            language,
        });

        // Handle textDocument/publishDiagnostics — buffer into shared cache
        router.notification::<notification::PublishDiagnostics>(|state, params| {
            let mut diags = match state.diagnostics.lock() {
                Ok(guard) => guard,
                Err(poisoned) => {
                    tracing::warn!("diagnostics mutex poisoned, recovering");
                    poisoned.into_inner()
                }
            };
            diags.insert(params.uri, params.diagnostics);
            ControlFlow::Continue(())
        });

        // Handle $/progress — track active workDone tokens for Indexing state
        router.notification::<notification::Progress>(|state, params| {
            tracing::debug!(
                language = ?state.language,
                token = ?params.token,
                value = ?params.value,
                "$/progress notification received"
            );
            let mut map = match state.status.lock() {
                Ok(guard) => guard,
                Err(poisoned) => {
                    tracing::warn!("lsp status mutex poisoned in progress handler, recovering");
                    poisoned.into_inner()
                }
            };
            let Some(entry) = map.get_mut(&state.language) else {
                tracing::debug!(
                    language = ?state.language,
                    "progress notification for language not in status cache"
                );
                return ControlFlow::Continue(());
            };
            apply_progress_update(entry, params.value);
            tracing::debug!(
                language = ?state.language,
                state = ?entry.state,
                active_progress = entry.active_progress,
                "after apply_progress_update"
            );
            ControlFlow::Continue(())
        });

        // Handle workspace/configuration — respond with empty config per item
        router.request::<request::WorkspaceConfiguration, _>(|_state, params| {
            let items: Vec<serde_json::Value> =
                params.items.iter().map(|_| serde_json::json!({})).collect();
            futures_util::future::ready(Ok(items))
        });

        // Handle client/registerCapability — acknowledge
        router.request::<request::RegisterCapability, _>(|_state, _params| {
            futures_util::future::ready(Ok(()))
        });

        // Handle window/workDoneProgress/create — acknowledge
        router.request::<request::WorkDoneProgressCreate, _>(|_state, _params| {
            futures_util::future::ready(Ok(()))
        });

        // Handle window/logMessage — server log lines surfaced via tracing.
        // Without this, async-lsp's Router terminates the mainloop with
        // "Unhandled notification: window/logMessage" (steve-385x). Most
        // Node-based servers (yaml-language-server, typescript-language-
        // server, basedpyright-langserver) emit logMessage during or
        // immediately after Initialize, so the absence of this handler
        // crashed every such server within ~ms of reaching Ready.
        //
        // Tracing level mirrors `params.typ`: an ERROR-level logMessage
        // (e.g. yaml-language-server's "schema fetch failed") shows up at
        // tracing::error, so users running with `RUST_LOG=steve=warn`
        // still see it. INFO and LOG variants use proportionally lower
        // levels.
        router.notification::<notification::LogMessage>(|state, params| {
            log_lsp_at_typ_level!(
                "steve::lsp_log",
                state.language,
                params.typ,
                &params.message
            );
            ControlFlow::Continue(())
        });

        // Handle window/showMessage — like logMessage but conventionally
        // user-visible. We don't have UX to surface a popup, so log it at
        // a level matching the message's typ so users can grep for both.
        router.notification::<notification::ShowMessage>(|state, params| {
            log_lsp_at_typ_level!(
                "steve::lsp_show",
                state.language,
                params.typ,
                &params.message
            );
            ControlFlow::Continue(())
        });

        // Handle window/showMessageRequest — server asking the user to
        // pick from a list. We don't have an interactive prompt here, so
        // respond with None (no action selected). The message itself is
        // logged so it's still visible.
        router.request::<request::ShowMessageRequest, _>(|state, params| {
            let augmented = format!(
                "{} (no UX to prompt user — responding with None; actions={:?})",
                params.message, params.actions,
            );
            log_lsp_at_typ_level!("steve::lsp_show", state.language, params.typ, &augmented);
            futures_util::future::ready(Ok(None))
        });

        // Catch-all fallback for any other server-to-client notifications
        // we don't have an explicit handler for. async-lsp's default does
        // nothing for `$/`-prefixed methods and terminates the mainloop
        // with `Error::Routing` for everything else — including
        // `telemetry/event`, vendor extensions like
        // `rust-analyzer/serverStatus`, and any future LSP additions our
        // code hasn't been updated for. The official docs warn the
        // catch-all is unsafe for *client-to-server* notifications where
        // missing `didChange` handling causes state desync — but
        // server-to-client notifications are advisory by design (logs,
        // telemetry, status pings). Dropping them silently is the right
        // default; the explicit handlers above cover the ones we want to
        // act on.
        router.unhandled_notification(|_state, notification| {
            tracing::debug!(
                target: "steve::lsp_unhandled",
                method = %notification.method,
                "dropping unhandled server-to-client notification",
            );
            ControlFlow::Continue(())
        });

        // Catch-all for unhandled server-to-client *requests*. async-lsp's
        // default already returns METHOD_NOT_FOUND (so it doesn't crash
        // the mainloop), but it does so without logging — and some
        // servers complain noisily on receiving the error. Replace it
        // with a logging variant so the rejection is visible at debug
        // level and the user can see which extension methods their
        // server is asking about.
        router.unhandled_request(|_state, req| {
            tracing::debug!(
                target: "steve::lsp_unhandled",
                method = %req.method,
                "rejecting unhandled server-to-client request with METHOD_NOT_FOUND",
            );
            let method = req.method.clone();
            futures_util::future::ready(Err(ResponseError::new(
                ErrorCode::METHOD_NOT_FOUND,
                format!("No such method {method}"),
            )))
        });

        router
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_lsp::lsp_types::{
        Diagnostic, DiagnosticSeverity, Position, Range, WorkDoneProgressBegin,
        WorkDoneProgressEnd, WorkDoneProgressReport,
    };

    fn sample_entry(state: LspServerState, active: usize) -> LspStatusEntry {
        LspStatusEntry {
            binary: "test-ls".into(),
            state,
            active_progress: active,
            progress_message: None,
            updated_at: Instant::now(),
            restart_attempts: 0,
            next_restart_at: None,
            ready_since: None,
        }
    }

    fn begin(title: &str) -> ProgressParamsValue {
        ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(WorkDoneProgressBegin {
            title: title.into(),
            cancellable: None,
            message: None,
            percentage: None,
        }))
    }

    fn report(msg: Option<&str>) -> ProgressParamsValue {
        ProgressParamsValue::WorkDone(WorkDoneProgress::Report(WorkDoneProgressReport {
            cancellable: None,
            message: msg.map(String::from),
            percentage: None,
        }))
    }

    fn end(msg: Option<&str>) -> ProgressParamsValue {
        ProgressParamsValue::WorkDone(WorkDoneProgress::End(WorkDoneProgressEnd {
            message: msg.map(String::from),
        }))
    }

    #[test]
    fn shared_diagnostics_insert_and_read() {
        let diags: SharedDiagnostics = Arc::new(Mutex::new(HashMap::new()));
        let uri = Url::parse("file:///test.rs").unwrap();
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 5)),
            severity: Some(DiagnosticSeverity::ERROR),
            message: "test error".to_string(),
            ..Default::default()
        };

        diags.lock().unwrap().insert(uri.clone(), vec![diagnostic]);

        let locked = diags.lock().unwrap();
        let result = locked.get(&uri).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].message, "test error");
    }

    #[test]
    fn create_client_returns_socket_and_mainloop() {
        let diags: SharedDiagnostics = Arc::new(Mutex::new(HashMap::new()));
        let status: SharedLspStatus = Arc::new(Mutex::new(HashMap::new()));
        let (_mainloop, _server_socket) = create_client(diags, status, Language::Rust);
        // Just verify construction doesn't panic
    }

    #[test]
    fn progress_begin_on_ready_transitions_to_indexing() {
        let mut entry = sample_entry(LspServerState::Ready, 0);
        apply_progress_update(&mut entry, begin("rustAnalyzer/Indexing"));
        assert_eq!(entry.state, LspServerState::Indexing);
        assert_eq!(entry.active_progress, 1);
        assert_eq!(
            entry.progress_message.as_deref(),
            Some("rustAnalyzer/Indexing")
        );
    }

    #[test]
    fn progress_begin_on_starting_leaves_state_unchanged() {
        let mut entry = sample_entry(LspServerState::Starting, 0);
        apply_progress_update(&mut entry, begin("rustAnalyzer/Fetching"));
        // Begin during Starting increments the counter only — Initialize return
        // is responsible for the flip to Ready or Indexing.
        assert_eq!(entry.state, LspServerState::Starting);
        assert_eq!(entry.active_progress, 1);
        assert_eq!(
            entry.progress_message.as_deref(),
            Some("rustAnalyzer/Fetching")
        );
    }

    #[test]
    fn progress_begin_on_indexing_increments_counter_only() {
        let mut entry = sample_entry(LspServerState::Indexing, 2);
        apply_progress_update(&mut entry, begin("second-op"));
        assert_eq!(entry.state, LspServerState::Indexing);
        assert_eq!(entry.active_progress, 3);
    }

    #[test]
    fn progress_report_updates_message_only() {
        let mut entry = sample_entry(LspServerState::Indexing, 1);
        entry.progress_message = Some("old".into());
        apply_progress_update(&mut entry, report(Some("Building crate graph")));
        assert_eq!(entry.state, LspServerState::Indexing);
        assert_eq!(entry.active_progress, 1);
        assert_eq!(
            entry.progress_message.as_deref(),
            Some("Building crate graph")
        );
    }

    #[test]
    fn progress_report_without_message_is_noop() {
        let mut entry = sample_entry(LspServerState::Indexing, 1);
        entry.progress_message = Some("stable".into());
        apply_progress_update(&mut entry, report(None));
        assert_eq!(entry.progress_message.as_deref(), Some("stable"));
    }

    #[test]
    fn progress_end_transitions_to_ready_when_counter_zero() {
        let mut entry = sample_entry(LspServerState::Indexing, 1);
        apply_progress_update(&mut entry, end(None));
        assert_eq!(entry.state, LspServerState::Ready);
        assert_eq!(entry.active_progress, 0);
    }

    #[test]
    fn progress_end_stays_indexing_when_other_tokens_active() {
        let mut entry = sample_entry(LspServerState::Indexing, 3);
        apply_progress_update(&mut entry, end(None));
        assert_eq!(entry.state, LspServerState::Indexing);
        assert_eq!(entry.active_progress, 2);
    }

    #[test]
    fn progress_end_underflow_saturates_at_zero() {
        let mut entry = sample_entry(LspServerState::Ready, 0);
        apply_progress_update(&mut entry, end(Some("stray end")));
        assert_eq!(entry.state, LspServerState::Ready);
        assert_eq!(entry.active_progress, 0);
        assert_eq!(entry.progress_message.as_deref(), Some("stray end"));
    }

    #[test]
    fn progress_end_does_not_flip_error_to_ready() {
        // A crash watcher could have flipped the entry to Error while End
        // notifications were in flight. End must not resurrect a dead server.
        let mut entry = sample_entry(
            LspServerState::Error {
                reason: "boom".into(),
            },
            1,
        );
        apply_progress_update(&mut entry, end(None));
        assert_eq!(
            entry.state,
            LspServerState::Error {
                reason: "boom".into()
            }
        );
        assert_eq!(entry.active_progress, 0);
    }

    #[test]
    fn shared_diagnostics_overwrite() {
        let diags: SharedDiagnostics = Arc::new(Mutex::new(HashMap::new()));
        let uri = Url::parse("file:///test.rs").unwrap();

        let d1 = Diagnostic {
            message: "first".to_string(),
            ..Default::default()
        };
        let d2 = Diagnostic {
            message: "second".to_string(),
            ..Default::default()
        };

        diags.lock().unwrap().insert(uri.clone(), vec![d1]);
        diags.lock().unwrap().insert(uri.clone(), vec![d2]);

        let locked = diags.lock().unwrap();
        let result = locked.get(&uri).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].message, "second");
    }

    // -- end-to-end Router behavior ------------------------------------------
    //
    // These tests drive the real `create_client` MainLoop through a duplex
    // pipe pair, simulating an LSP server that sends notifications down
    // stdout. Pre-steve-385x, async-lsp's Router would terminate the
    // mainloop on receipt of any notification without an explicit handler
    // (e.g. `window/logMessage`). Each test below sends one such
    // notification and verifies the mainloop does NOT exit with an error —
    // it should consume the message, continue running, and only exit
    // cleanly when stdout is closed by the test.

    /// Drive the real `create_client` mainloop with a single server-to-
    /// client notification, then close the pipe. Returns the mainloop's
    /// terminal `Result`, which the caller is expected to check is NOT
    /// `Err(Routing(...))` — that's the failure mode we're guarding
    /// against. `Err(Eof)` is the *expected* terminal state once we drop
    /// the pipe (async-lsp surfaces EOF as an error, not as `Ok`).
    ///
    /// `tokio::io::duplex` is an in-memory pipe, so the mainloop's
    /// read+dispatch is synchronous w.r.t. the test's `write_all`+`flush`
    /// — there's no race window between the framed write and the
    /// subsequent EOF signal that would require a sleep to bridge.
    async fn drive_mainloop_with_notification(
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), async_lsp::Error> {
        use tokio::io::AsyncWriteExt;
        use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

        let (mut stdout_server, stdout_client) = tokio::io::duplex(8192);
        let (stdin_client, _stdin_server) = tokio::io::duplex(8192);

        let diagnostics: SharedDiagnostics = Arc::new(Mutex::new(HashMap::new()));
        let status: SharedLspStatus = Arc::new(Mutex::new(HashMap::new()));
        let (mainloop, _socket) = create_client(diagnostics, status, Language::Rust);

        let mainloop_handle = tokio::spawn(async move {
            mainloop
                .run_buffered(stdout_client.compat(), stdin_client.compat_write())
                .await
        });

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        })
        .to_string();
        let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        stdout_server.write_all(frame.as_bytes()).await.unwrap();
        stdout_server.flush().await.unwrap();

        // Close the server side so the mainloop reaches stdout EOF and
        // returns Err(Eof). The flushed frame is already in the pipe
        // buffer; the mainloop reads it before observing EOF.
        drop(stdout_server);

        tokio::time::timeout(std::time::Duration::from_secs(2), mainloop_handle)
            .await
            .expect("mainloop hang")
            .expect("mainloop task panicked")
    }

    /// Assert that a mainloop result is NOT a routing failure. `Ok(())` and
    /// `Err(Eof)` both pass — the former means the loop ran clean, the
    /// latter is the expected terminal state when we drop the pipe in the
    /// driver above. `Err(Routing(_))` is the failure mode under test:
    /// async-lsp's default for unhandled notifications.
    fn assert_not_routing_error(method: &str, result: Result<(), async_lsp::Error>) {
        match result {
            Ok(()) => {}
            Err(async_lsp::Error::Eof) => {}
            Err(other) => panic!(
                "{method} terminated the mainloop with a non-EOF error \
                 (this is the steve-385x failure mode): {other:?}"
            ),
        }
    }

    #[tokio::test]
    async fn mainloop_does_not_terminate_on_window_log_message() {
        // Regression for steve-385x: yaml-language-server, typescript-
        // language-server, and basedpyright-langserver all emit
        // window/logMessage during or shortly after Initialize. Pre-fix,
        // async-lsp's default Router terminated the mainloop with
        // "Unhandled notification: window/logMessage", which manifested
        // as an opaque ServiceStopped to the user.
        let result = drive_mainloop_with_notification(
            "window/logMessage",
            serde_json::json!({"type": 3, "message": "test log line from server"}),
        )
        .await;
        assert_not_routing_error("window/logMessage", result);
    }

    #[tokio::test]
    async fn mainloop_does_not_terminate_on_window_show_message() {
        let result = drive_mainloop_with_notification(
            "window/showMessage",
            serde_json::json!({"type": 1, "message": "important user message"}),
        )
        .await;
        assert_not_routing_error("window/showMessage", result);
    }

    #[tokio::test]
    async fn mainloop_does_not_terminate_on_telemetry_event() {
        let result =
            drive_mainloop_with_notification("telemetry/event", serde_json::json!({"foo": "bar"}))
                .await;
        assert_not_routing_error("telemetry/event", result);
    }

    #[tokio::test]
    async fn mainloop_does_not_terminate_on_unknown_vendor_notification() {
        // Tests the unhandled_notification fallback. Servers (especially
        // rust-analyzer with its many `rust-analyzer/*` extensions) send
        // notifications we don't have explicit handlers for; without the
        // catch-all, every such notification would crash us as soon as
        // the server's vocabulary changes.
        let result = drive_mainloop_with_notification(
            "rust-analyzer/serverStatus",
            serde_json::json!({"health": "ok", "quiescent": true}),
        )
        .await;
        assert_not_routing_error("rust-analyzer/serverStatus", result);
    }

    #[tokio::test]
    async fn typed_show_message_request_handler_responds_with_null_result() {
        // Closes the L5 coverage gap: the four "does_not_terminate" tests
        // above prove the mainloop survives, but they CANNOT distinguish
        // the typed `ShowMessageRequest` handler from the catch-all
        // `unhandled_request` (both keep the loop alive). This test
        // observes the *response side*: the typed handler returns
        // `Ok(None)` which serializes as `"result": null`, whereas the
        // catch-all returns `Err(METHOD_NOT_FOUND)` which serializes as
        // `"error": {"code": -32601, ...}`. Reading the response off
        // the server-side stdin pipe lets us assert which path ran,
        // catching a regression where someone drops the typed handler
        // and accidentally relies on the fallback.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

        let (mut stdout_server, stdout_client) = tokio::io::duplex(8192);
        let (stdin_client, mut stdin_server) = tokio::io::duplex(8192);

        let diagnostics: SharedDiagnostics = Arc::new(Mutex::new(HashMap::new()));
        let status: SharedLspStatus = Arc::new(Mutex::new(HashMap::new()));
        let (mainloop, _socket) = create_client(diagnostics, status, Language::Rust);

        let mainloop_handle = tokio::spawn(async move {
            mainloop
                .run_buffered(stdout_client.compat(), stdin_client.compat_write())
                .await
        });

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "window/showMessageRequest",
            "params": {
                "type": 3,
                "message": "Pick one",
                "actions": [{"title": "Yes"}, {"title": "No"}],
            },
        })
        .to_string();
        let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        stdout_server.write_all(frame.as_bytes()).await.unwrap();
        stdout_server.flush().await.unwrap();

        // Read the response off the client-to-server pipe (which is what
        // we'd see on stdin if this were a real LSP child). 4 KB is
        // ample headroom for a JSON-RPC response with a `null` result.
        let mut buf = vec![0u8; 4096];
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stdin_server.read(&mut buf),
        )
        .await
        .expect("response timeout — handler likely never ran")
        .expect("stdin read error");
        let response = std::str::from_utf8(&buf[..n]).expect("response is UTF-8");

        // Cleanup: close stdout to let the mainloop exit; ignore the
        // terminal Result (we already verified what we cared about).
        drop(stdout_server);
        let _ = mainloop_handle.await;

        assert!(
            response.contains("\"id\":42"),
            "response should reference id=42; got: {response}"
        );
        assert!(
            response.contains("\"result\":null"),
            "typed showMessageRequest handler should respond with null; got: {response}"
        );
        assert!(
            !response.contains("-32601"),
            "should NOT be METHOD_NOT_FOUND (which would mean unhandled_request fallback ran instead of the typed handler); got: {response}"
        );
    }
}
