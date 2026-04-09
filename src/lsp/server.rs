use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use async_lsp::{ServerSocket, lsp_types::*};
use tokio::task::JoinHandle;

use super::Language;
use crate::lsp::client::SharedDiagnostics;

pub struct LspServer {
    pub(super) _process: tokio::process::Child,
    pub(super) _mainloop_handle: JoinHandle<()>,
    pub(super) server_socket: ServerSocket,
    pub(super) handle: tokio::runtime::Handle,
    pub(super) language: Language,
    pub binary: String,
    pub(super) capabilities: ServerCapabilities,
    pub(super) open_files: HashSet<Url>,
    pub(super) diagnostics: SharedDiagnostics,
}

impl LspServer {
    fn ensure_open(&mut self, path: &Path) -> Result<Url> {
        let uri = path_to_uri(path)?;

        if !self.open_files.contains(&uri) {
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
            self.open_files.insert(uri.clone());
        }

        Ok(uri)
    }

    pub fn diagnostics(&mut self, path: &Path) -> Result<Vec<Diagnostic>> {
        let uri = self.ensure_open(path)?;

        let params = DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        match self.handle.block_on(
            self.server_socket
                .request::<request::DocumentSymbolRequest>(params),
        ) {
            Ok(_result) => {}
            Err(e) => {
                tracing::debug!("documentSymbol request failed (ok, just for diagnostics): {e}");
            }
        }

        let locked = self
            .diagnostics
            .lock()
            .map_err(|_| anyhow::anyhow!("diagnostics lock poisoned"))?;
        Ok(locked.get(&uri).cloned().unwrap_or_default())
    }

    pub fn definition(&mut self, path: &Path, line: u32, character: u32) -> Result<Vec<Location>> {
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
            .block_on(
                self.server_socket
                    .request::<request::GotoDefinition>(params),
            )
            .map_err(|e| anyhow::anyhow!("definition request failed: {e}"))?;

        parse_goto_definition_response(result)
    }

    pub fn references(&mut self, path: &Path, line: u32, character: u32) -> Result<Vec<Location>> {
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
            .block_on(self.server_socket.request::<request::References>(params))
            .map_err(|e| anyhow::anyhow!("references request failed: {e}"))?;

        Ok(result.unwrap_or_default())
    }

    pub fn rename(
        &mut self,
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
            .block_on(self.server_socket.request::<request::Rename>(params))
            .map_err(|e| anyhow::anyhow!("rename request failed: {e}"))?;

        Ok(result.unwrap_or_default())
    }

    pub(super) fn transport_shutdown(mut self) -> Result<()> {
        // Shutdown is best-effort — don't block_on since this may run from Drop on a tokio worker thread.
        // Send exit notification directly; the server should exit on receiving it.
        let _ = self.server_socket.notify::<notification::Exit>(());
        self._mainloop_handle.abort();

        match self._process.try_wait() {
            Ok(Some(_status)) => {}
            _ => {
                std::thread::sleep(std::time::Duration::from_millis(500));
                match self._process.try_wait() {
                    Ok(Some(_)) => {}
                    _ => {
                        let _ = self._process.start_kill();
                        // Don't await — process is being killed, cleanup happens on drop
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
}
