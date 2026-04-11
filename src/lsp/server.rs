use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result};
use async_lsp::{ServerSocket, lsp_types::*};
use tokio::task::AbortHandle;

use super::Language;
use crate::lsp::client::SharedDiagnostics;

/// Timeout for LSP requests (matches the old JsonRpcTransport timeout).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub struct LspServer {
    pub(super) process: tokio::process::Child,
    /// Abort handle for the MainLoop task. The actual `JoinHandle<()>` is
    /// owned by a per-server crash-watcher task spawned in `LspManager::start_server`.
    pub(super) mainloop_abort: AbortHandle,
    /// Set to `true` by `transport_shutdown` before aborting, so the crash
    /// watcher can distinguish an intentional shutdown from a genuine crash.
    pub(super) shutdown_flag: Arc<AtomicBool>,
    pub(super) server_socket: ServerSocket,
    pub(super) handle: tokio::runtime::Handle,
    pub(super) language: Language,
    pub binary: String,
    pub(super) capabilities: ServerCapabilities,
    pub(super) open_files: Mutex<HashMap<Url, i32>>,
    pub(super) diagnostics: SharedDiagnostics,
}

impl LspServer {
    fn ensure_open(&self, path: &Path) -> Result<Url> {
        let uri = path_to_uri(path)?;

        // Check under lock, then drop before I/O
        let already_open = {
            let open = self
                .open_files
                .lock()
                .map_err(|_| anyhow::anyhow!("open_files lock poisoned"))?;
            open.contains_key(&uri)
        };

        if !already_open {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?;

            let params = DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: self.language.language_id().to_string(),
                    version: 0,
                    text: content,
                },
            };
            self.server_socket
                .notify::<notification::DidOpenTextDocument>(params)
                .map_err(|e| anyhow::anyhow!("failed to send didOpen: {e}"))?;

            // Re-acquire to insert (race-safe: duplicate insert is harmless)
            let mut open = self
                .open_files
                .lock()
                .map_err(|_| anyhow::anyhow!("open_files lock poisoned"))?;
            open.insert(uri.clone(), 0);
        }

        Ok(uri)
    }

    /// Notify the language server that a file's content has changed (full sync).
    /// Reads the file once, opens it if needed, increments the version, and
    /// sends `textDocument/didChange`. Returns the URI for use by `notify_did_save`.
    pub fn notify_did_change(&self, path: &Path) -> Result<Url> {
        let uri = path_to_uri(path)?;
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        // Determine action under lock, then drop before sending notifications.
        // Mutations are deferred until after successful notify to avoid stale
        // version state if the notification fails.
        let next_version = {
            let open = self
                .open_files
                .lock()
                .map_err(|_| anyhow::anyhow!("open_files lock poisoned"))?;
            open.get(&uri).map(|v| v + 1)
        };

        if let Some(version) = next_version {
            // File already open — send didChange with incremented version
            let params = DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: content,
                }],
            };
            self.server_socket
                .notify::<notification::DidChangeTextDocument>(params)
                .map_err(|e| anyhow::anyhow!("failed to send didChange: {e}"))?;

            // Commit version bump only after successful notify
            let mut open = self
                .open_files
                .lock()
                .map_err(|_| anyhow::anyhow!("open_files lock poisoned"))?;
            if let Some(v) = open.get_mut(&uri) {
                *v = version;
            }
        } else {
            // File not yet open — didOpen sets the content
            let params = DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: self.language.language_id().to_string(),
                    version: 0,
                    text: content,
                },
            };
            self.server_socket
                .notify::<notification::DidOpenTextDocument>(params)
                .map_err(|e| anyhow::anyhow!("failed to send didOpen: {e}"))?;

            // Insert only after successful notify
            let mut open = self
                .open_files
                .lock()
                .map_err(|_| anyhow::anyhow!("open_files lock poisoned"))?;
            open.insert(uri.clone(), 0);
        }

        Ok(uri)
    }

    /// Notify the language server that a file has been saved.
    /// Takes a pre-resolved URI to avoid redundant `ensure_open` calls when
    /// used after `notify_did_change`.
    pub fn notify_did_save(&self, uri: &Url) -> Result<()> {
        let params = DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            text: None,
        };
        self.server_socket
            .notify::<notification::DidSaveTextDocument>(params)
            .map_err(|e| anyhow::anyhow!("failed to send didSave: {e}"))?;
        Ok(())
    }

    /// Read cached diagnostics without sending any LSP requests.
    /// Safe to call from within the tokio runtime (no `block_on`).
    /// Returns whatever the server has last pushed via `publishDiagnostics`.
    pub fn cached_diagnostics(&self, path: &Path) -> Result<Vec<Diagnostic>> {
        let uri = path_to_uri(path)?;
        let locked = match self.diagnostics.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!("diagnostics mutex poisoned in reader, recovering");
                poisoned.into_inner()
            }
        };
        Ok(locked.get(&uri).cloned().unwrap_or_default())
    }

    pub fn diagnostics(&self, path: &Path) -> Result<Vec<Diagnostic>> {
        let uri = self.ensure_open(path)?;

        let params = DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        match self.handle.block_on(async {
            tokio::time::timeout(
                REQUEST_TIMEOUT,
                self.server_socket
                    .request::<request::DocumentSymbolRequest>(params),
            )
            .await
        }) {
            Ok(Ok(_result)) => {}
            Ok(Err(e)) => {
                tracing::debug!("documentSymbol request failed (ok, just for diagnostics): {e}");
            }
            Err(_) => {
                tracing::debug!("documentSymbol request timed out (ok, just for diagnostics)");
            }
        }

        let locked = match self.diagnostics.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!("diagnostics mutex poisoned in reader, recovering");
                poisoned.into_inner()
            }
        };
        Ok(locked.get(&uri).cloned().unwrap_or_default())
    }

    pub fn definition(&self, path: &Path, line: u32, character: u32) -> Result<Vec<Location>> {
        let uri = self.ensure_open(path)?;

        if !self
            .capabilities
            .definition_provider
            .as_ref()
            .is_some_and(|p| match p {
                OneOf::Left(b) => *b,
                OneOf::Right(_) => true,
            })
        {
            return Err(anyhow::anyhow!("server does not support go-to-definition"));
        }

        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result = self
            .handle
            .block_on(async {
                tokio::time::timeout(
                    REQUEST_TIMEOUT,
                    self.server_socket
                        .request::<request::GotoDefinition>(params),
                )
                .await
            })
            .map_err(|_| {
                anyhow::anyhow!(
                    "definition request timed out after {}s",
                    REQUEST_TIMEOUT.as_secs()
                )
            })?
            .map_err(|e| anyhow::anyhow!("definition request failed: {e}"))?;

        parse_goto_definition_response(result)
    }

    pub fn references(&self, path: &Path, line: u32, character: u32) -> Result<Vec<Location>> {
        let uri = self.ensure_open(path)?;

        if self.capabilities.references_provider.is_none() {
            return Err(anyhow::anyhow!("server does not support find-references"));
        }

        let params = ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            context: ReferenceContext {
                include_declaration: true,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result = self
            .handle
            .block_on(async {
                tokio::time::timeout(
                    REQUEST_TIMEOUT,
                    self.server_socket.request::<request::References>(params),
                )
                .await
            })
            .map_err(|_| {
                anyhow::anyhow!(
                    "references request timed out after {}s",
                    REQUEST_TIMEOUT.as_secs()
                )
            })?
            .map_err(|e| anyhow::anyhow!("references request failed: {e}"))?;

        Ok(result.unwrap_or_default())
    }

    pub fn rename(
        &self,
        path: &Path,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<WorkspaceEdit> {
        let uri = self.ensure_open(path)?;

        if self.capabilities.rename_provider.is_none() {
            return Err(anyhow::anyhow!("server does not support rename"));
        }

        let params = RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            new_name: new_name.to_string(),
            work_done_progress_params: Default::default(),
        };

        let result = self
            .handle
            .block_on(async {
                tokio::time::timeout(
                    REQUEST_TIMEOUT,
                    self.server_socket.request::<request::Rename>(params),
                )
                .await
            })
            .map_err(|_| {
                anyhow::anyhow!(
                    "rename request timed out after {}s",
                    REQUEST_TIMEOUT.as_secs()
                )
            })?
            .map_err(|e| anyhow::anyhow!("rename request failed: {e}"))?;

        result.ok_or_else(|| anyhow::anyhow!("server could not rename at this position"))
    }

    pub fn workspace_symbols(&self, query: &str) -> Result<Vec<WorkspaceSymbolResult>> {
        if !matches!(
            self.capabilities.workspace_symbol_provider,
            Some(OneOf::Left(true)) | Some(OneOf::Right(_))
        ) {
            return Err(anyhow::anyhow!("server does not support workspace/symbol"));
        }

        let params = WorkspaceSymbolParams {
            query: query.to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result = self
            .handle
            .block_on(async {
                tokio::time::timeout(
                    REQUEST_TIMEOUT,
                    self.server_socket
                        .request::<request::WorkspaceSymbolRequest>(params),
                )
                .await
            })
            .map_err(|_| {
                anyhow::anyhow!(
                    "workspace/symbol request timed out after {}s",
                    REQUEST_TIMEOUT.as_secs()
                )
            })?
            .map_err(|e| anyhow::anyhow!("workspace/symbol request failed: {e}"))?;

        Ok(parse_workspace_symbol_response(result))
    }

    pub(super) fn transport_shutdown(mut self) -> Result<()> {
        // Flag the shutdown as intentional BEFORE aborting the mainloop so
        // the crash-watcher task skips writing an Error entry to the status cache.
        self.shutdown_flag.store(true, Ordering::SeqCst);

        // Send LSP shutdown request. If we're inside the tokio runtime (e.g.,
        // Drop during app exit), we can't block_on — spawn a fire-and-forget
        // task instead. Either way, this is best-effort.
        let socket = self.server_socket.clone();
        if tokio::runtime::Handle::try_current().is_ok() {
            // Inside runtime — spawn the entire shutdown sequence so the
            // mainloop stays alive long enough for Shutdown+Exit to be sent.
            let mainloop_abort = self.mainloop_abort.clone();
            let mut process = self.process;
            tokio::spawn(async move {
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    socket.request::<request::Shutdown>(()),
                )
                .await;
                let _ = socket.notify::<notification::Exit>(());
                mainloop_abort.abort();
                // Brief wait for process exit, then force-kill
                tokio::time::sleep(Duration::from_millis(500)).await;
                if matches!(process.try_wait(), Ok(Some(_))) {
                    return;
                }
                let _ = process.start_kill();
            });
            return Ok(());
        }

        // Outside runtime — safe to block
        let _ = self.handle.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(5),
                socket.request::<request::Shutdown>(()),
            )
            .await
        });
        let _ = self.server_socket.notify::<notification::Exit>(());
        self.mainloop_abort.abort();

        match self.process.try_wait() {
            Ok(Some(_status)) => {}
            _ => {
                std::thread::sleep(Duration::from_millis(500));
                match self.process.try_wait() {
                    Ok(Some(_)) => {}
                    _ => {
                        let _ = self.process.start_kill();
                    }
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn path_to_uri(path: &Path) -> Result<Url> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let canonical = std::fs::canonicalize(&abs).unwrap_or(abs);
    url::Url::from_file_path(&canonical)
        .map_err(|()| anyhow::anyhow!("invalid file path for URI: {}", canonical.display()))
}

pub fn uri_to_path(uri_str: &str) -> Option<PathBuf> {
    url::Url::parse(uri_str)
        .ok()
        .and_then(|u| u.to_file_path().ok())
}

/// A normalized workspace symbol result, usable regardless of whether the server
/// returned the older `SymbolInformation` or the newer `WorkspaceSymbol` format.
#[derive(Debug, Clone)]
pub struct WorkspaceSymbolResult {
    pub name: String,
    pub kind: SymbolKind,
    pub location: Location,
    pub container_name: Option<String>,
}

fn parse_workspace_symbol_response(
    result: Option<WorkspaceSymbolResponse>,
) -> Vec<WorkspaceSymbolResult> {
    let Some(response) = result else {
        return Vec::new();
    };
    match response {
        WorkspaceSymbolResponse::Flat(symbols) => symbols
            .into_iter()
            .map(|si| WorkspaceSymbolResult {
                name: si.name,
                kind: si.kind,
                location: si.location,
                container_name: si.container_name,
            })
            .collect(),
        WorkspaceSymbolResponse::Nested(symbols) => symbols
            .into_iter()
            .filter_map(|ws| {
                // WorkspaceLocation (URI only, no range) would produce a bogus
                // line 1 and unrelated source preview — skip these results rather
                // than fabricating a position. A future workspaceSymbol/resolve
                // call could recover the full location if needed.
                let location = match ws.location {
                    OneOf::Left(loc) => loc,
                    OneOf::Right(_) => return None,
                };
                Some(WorkspaceSymbolResult {
                    name: ws.name,
                    kind: ws.kind,
                    location,
                    container_name: ws.container_name,
                })
            })
            .collect(),
    }
}

fn parse_goto_definition_response(result: Option<GotoDefinitionResponse>) -> Result<Vec<Location>> {
    match result {
        None => Ok(Vec::new()),
        Some(GotoDefinitionResponse::Scalar(loc)) => Ok(vec![loc]),
        Some(GotoDefinitionResponse::Array(locs)) => Ok(locs),
        Some(GotoDefinitionResponse::Link(links)) => Ok(links
            .into_iter()
            .map(|link| Location {
                uri: link.target_uri,
                range: link.target_selection_range,
            })
            .collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn path_to_uri_absolute() {
        let uri = path_to_uri(Path::new("/tmp/test.rs")).unwrap();
        assert_eq!(uri.as_str(), "file:///tmp/test.rs");
    }

    #[test]
    fn uri_to_path_valid() {
        let path = uri_to_path("file:///home/user/test.rs").unwrap();
        assert_eq!(path, PathBuf::from("/home/user/test.rs"));
    }

    #[test]
    fn uri_to_path_with_spaces() {
        let path = uri_to_path("file:///home/my%20project/test.rs").unwrap();
        assert_eq!(path, PathBuf::from("/home/my project/test.rs"));
    }

    #[test]
    fn uri_to_path_non_file() {
        assert!(uri_to_path("https://example.com").is_none());
    }

    #[test]
    fn uri_to_path_percent_encoded_hash() {
        let path = uri_to_path("file:///home/user/C%23/test.cs").unwrap();
        assert_eq!(path, PathBuf::from("/home/user/C#/test.cs"));
    }

    #[test]
    fn uri_to_path_percent_encoded_parens() {
        let path = uri_to_path("file:///home/%28old%29/test.rs").unwrap();
        assert_eq!(path, PathBuf::from("/home/(old)/test.rs"));
    }

    #[test]
    fn uri_to_path_space_in_path() {
        let path = uri_to_path("file:///home/my%20project/test.rs").unwrap();
        assert_eq!(path, PathBuf::from("/home/my project/test.rs"));
    }

    #[test]
    fn uri_to_path_non_file_returns_none() {
        assert!(uri_to_path("https://example.com").is_none());
    }

    #[test]
    fn shared_diagnostics_from_publish_params() {
        use crate::lsp::client::SharedDiagnostics;

        let diags: SharedDiagnostics = std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
        let params = PublishDiagnosticsParams {
            uri: Url::parse("file:///test.rs").unwrap(),
            diagnostics: vec![Diagnostic {
                range: Range::new(Position::new(0, 0), Position::new(0, 5)),
                severity: Some(DiagnosticSeverity::ERROR),
                message: "test error".to_string(),
                ..Default::default()
            }],
            version: None,
        };

        diags
            .lock()
            .unwrap()
            .insert(params.uri.clone(), params.diagnostics);

        let locked = diags.lock().unwrap();
        let result = locked.get(&Url::parse("file:///test.rs").unwrap()).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].message, "test error");
    }

    #[test]
    fn parse_goto_definition_response_none() {
        let result = parse_goto_definition_response(None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_goto_definition_response_scalar() {
        let loc = Location {
            uri: Url::parse("file:///test.rs").unwrap(),
            range: Range::new(Position::new(10, 5), Position::new(10, 15)),
        };
        let result =
            parse_goto_definition_response(Some(GotoDefinitionResponse::Scalar(loc))).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].range.start.line, 10);
    }

    #[test]
    fn parse_goto_definition_response_array() {
        let locs = vec![
            Location {
                uri: Url::parse("file:///a.rs").unwrap(),
                range: Range::new(Position::new(1, 0), Position::new(1, 5)),
            },
            Location {
                uri: Url::parse("file:///b.rs").unwrap(),
                range: Range::new(Position::new(2, 0), Position::new(2, 5)),
            },
        ];
        let result =
            parse_goto_definition_response(Some(GotoDefinitionResponse::Array(locs))).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_goto_definition_response_links() {
        let links = vec![LocationLink {
            origin_selection_range: None,
            target_uri: Url::parse("file:///target.rs").unwrap(),
            target_range: Range::new(Position::new(5, 0), Position::new(10, 0)),
            target_selection_range: Range::new(Position::new(5, 4), Position::new(5, 15)),
        }];
        let result =
            parse_goto_definition_response(Some(GotoDefinitionResponse::Link(links))).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].range.start.line, 5);
        assert_eq!(result[0].range.start.character, 4);
    }

    #[test]
    fn parse_workspace_symbol_response_none() {
        let result = parse_workspace_symbol_response(None);
        assert!(result.is_empty());
    }

    #[test]
    #[allow(deprecated)] // SymbolInformation.deprecated field is deprecated in favor of tags
    fn parse_workspace_symbol_response_flat() {
        let symbols = vec![SymbolInformation {
            name: "MyStruct".to_string(),
            kind: SymbolKind::STRUCT,
            tags: None,
            deprecated: None,
            location: Location {
                uri: Url::parse("file:///src/lib.rs").unwrap(),
                range: Range::new(Position::new(10, 0), Position::new(10, 15)),
            },
            container_name: Some("my_module".to_string()),
        }];
        let result = parse_workspace_symbol_response(Some(WorkspaceSymbolResponse::Flat(symbols)));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "MyStruct");
        assert_eq!(result[0].kind, SymbolKind::STRUCT);
        assert_eq!(result[0].location.range.start.line, 10);
        assert_eq!(result[0].container_name.as_deref(), Some("my_module"));
    }

    #[test]
    fn parse_workspace_symbol_response_nested_with_location() {
        let symbols = vec![async_lsp::lsp_types::WorkspaceSymbol {
            name: "my_fn".to_string(),
            kind: SymbolKind::FUNCTION,
            tags: None,
            container_name: None,
            location: OneOf::Left(Location {
                uri: Url::parse("file:///src/main.rs").unwrap(),
                range: Range::new(Position::new(5, 3), Position::new(5, 8)),
            }),
            data: None,
        }];
        let result =
            parse_workspace_symbol_response(Some(WorkspaceSymbolResponse::Nested(symbols)));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "my_fn");
        assert_eq!(result[0].location.range.start.line, 5);
        assert_eq!(result[0].location.range.start.character, 3);
    }

    #[test]
    fn parse_workspace_symbol_response_nested_workspace_location_filtered() {
        // WorkspaceLocation (URI only, no range) should be filtered out
        let symbols = vec![async_lsp::lsp_types::WorkspaceSymbol {
            name: "Config".to_string(),
            kind: SymbolKind::STRUCT,
            tags: None,
            container_name: Some("config".to_string()),
            location: OneOf::Right(async_lsp::lsp_types::WorkspaceLocation {
                uri: Url::parse("file:///src/config.rs").unwrap(),
            }),
            data: None,
        }];
        let result =
            parse_workspace_symbol_response(Some(WorkspaceSymbolResponse::Nested(symbols)));
        assert!(
            result.is_empty(),
            "WorkspaceLocation results (no range) should be filtered out"
        );
    }
}
