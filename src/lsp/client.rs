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
};

use async_lsp::{
    MainLoop, ServerSocket,
    lsp_types::{Diagnostic, Url, notification, request},
    router::Router,
};

/// Shared diagnostics cache — written by the MainLoop service, read by LspServer.
pub type SharedDiagnostics = Arc<Mutex<HashMap<Url, Vec<Diagnostic>>>>;

/// State held by the Router service.
pub(crate) struct ClientState {
    pub diagnostics: SharedDiagnostics,
}

/// Create an async-lsp client MainLoop + ServerSocket pair.
///
/// The MainLoop should be spawned as a background tokio task via
/// `mainloop.run_buffered(stdout, stdin)`. The ServerSocket is used
/// to send requests and notifications to the language server.
pub(crate) fn create_client(
    diagnostics: SharedDiagnostics,
) -> (MainLoop<Router<ClientState>>, ServerSocket) {
    MainLoop::new_client(|_server_socket| {
        let mut router = Router::new(ClientState { diagnostics });

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
    use async_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

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
        let (_mainloop, _server_socket) = create_client(diags);
        // Just verify construction doesn't panic
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
