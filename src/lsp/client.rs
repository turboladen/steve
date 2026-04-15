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
    MainLoop, ServerSocket,
    lsp_types::{Diagnostic, ProgressParamsValue, Url, WorkDoneProgress, notification, request},
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
}
