//! LSP integration — manages language server processes for code intelligence.
//!
//! Provides diagnostics, go-to-definition, find-references, and rename operations
//! by communicating with language servers over JSON-RPC stdio transport.

pub mod client;
pub mod types;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};

use anyhow::{Context, Result};
use lsp_types::*;
use serde_json::Value;

use client::{JsonRpcNotification, JsonRpcTransport};
use types::Language;

/// A running language server instance.
pub struct LspServer {
    _process: Child,
    transport: JsonRpcTransport,
    capabilities: ServerCapabilities,
    open_files: HashSet<Uri>,
    /// Buffered diagnostics from publishDiagnostics notifications.
    diagnostics: HashMap<Uri, Vec<Diagnostic>>,
}

/// Manages multiple language server processes for a project.
pub struct LspManager {
    servers: HashMap<Language, LspServer>,
    project_root: PathBuf,
}

impl LspManager {
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            servers: HashMap::new(),
            project_root,
        }
    }

    /// Detect languages in the project and start their servers.
    pub fn start_servers(&mut self) {
        let languages = Language::detect_from_project(&self.project_root);
        for lang in languages {
            if self.servers.contains_key(&lang) {
                continue;
            }
            match self.start_server(lang) {
                Ok(server) => {
                    tracing::info!("LSP: started {lang} server");
                    self.servers.insert(lang, server);
                }
                Err(e) => {
                    tracing::debug!("LSP: {lang} server not available: {e}");
                }
            }
        }
    }

    /// Start a single language server.
    fn start_server(&self, lang: Language) -> Result<LspServer> {
        let (binary, args) = lang
            .resolve_server()
            .ok_or_else(|| anyhow::anyhow!("no {lang} language server found on PATH"))?;

        tracing::debug!(binary = %binary, ?args, "spawning LSP server for {lang}");

        let mut child = std::process::Command::new(&binary)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to spawn {binary}"))?;

        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let mut transport = JsonRpcTransport::new(stdin, stdout);

        // Send initialize request
        #[allow(deprecated)] // root_uri is deprecated but widely supported
        let init_params = InitializeParams {
            process_id: Some(std::process::id()),
            root_uri: Some(path_to_uri(&self.project_root)?),
            capabilities: ClientCapabilities {
                text_document: Some(TextDocumentClientCapabilities {
                    publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                        related_information: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let (result, _notifications) = transport
            .send_request("initialize", &init_params)
            .context("initialize request failed")?;

        let init_result: InitializeResult =
            serde_json::from_value(result).context("failed to parse InitializeResult")?;

        // Send initialized notification
        transport.send_notification("initialized", serde_json::json!({}))?;

        Ok(LspServer {
            _process: child,
            transport,
            capabilities: init_result.capabilities,
            open_files: HashSet::new(),
            diagnostics: HashMap::new(),
        })
    }

    /// Get or lazily start a server for a file based on its extension.
    pub fn server_for_file(&mut self, path: &Path) -> Result<&mut LspServer> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| anyhow::anyhow!("file has no extension"))?;

        let lang = Language::from_extension(ext)
            .ok_or_else(|| anyhow::anyhow!("unsupported language for .{ext}"))?;

        self.ensure_server(lang)
    }

    /// Get an existing server or start one for the given language.
    fn ensure_server(&mut self, lang: Language) -> Result<&mut LspServer> {
        if !self.servers.contains_key(&lang) {
            let server = self.start_server(lang)?;
            self.servers.insert(lang, server);
        }
        Ok(self.servers.get_mut(&lang).unwrap())
    }

    /// Shut down all language servers gracefully.
    pub fn shutdown(&mut self) {
        for (lang, server) in self.servers.drain() {
            tracing::debug!("LSP: shutting down {lang} server");
            if let Err(e) = server.transport_shutdown() {
                tracing::debug!("LSP: {lang} shutdown error: {e}");
            }
        }
    }

    /// Check if any servers are running.
    pub fn has_servers(&self) -> bool {
        !self.servers.is_empty()
    }

    /// List running server languages.
    pub fn running_languages(&self) -> Vec<Language> {
        self.servers.keys().copied().collect()
    }
}

impl LspServer {
    /// Ensure a file is open in the language server (send textDocument/didOpen if needed).
    fn ensure_open(&mut self, path: &Path) -> Result<Uri> {
        let uri = path_to_uri(path)?;

        if !self.open_files.contains(&uri) {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?;

            let params = DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: String::new(), // server infers from extension
                    version: 0,
                    text: content,
                },
            };
            self.transport
                .send_notification("textDocument/didOpen", &params)?;
            self.open_files.insert(uri.clone());
        }

        Ok(uri)
    }

    /// Process any notifications received alongside a response.
    fn process_notifications(&mut self, notifications: Vec<JsonRpcNotification>) {
        for notif in notifications {
            if notif.method == "textDocument/publishDiagnostics" {
                if let Some(params) = notif.params {
                    if let Ok(diag_params) =
                        serde_json::from_value::<PublishDiagnosticsParams>(params)
                    {
                        self.diagnostics
                            .insert(diag_params.uri, diag_params.diagnostics);
                    }
                }
            }
            // Ignore other notifications (window/logMessage, etc.)
        }
    }

    /// Get diagnostics for a file. Opens the file if needed, then polls for diagnostics.
    pub fn diagnostics(&mut self, path: &Path) -> Result<Vec<Diagnostic>> {
        let uri = self.ensure_open(path)?;

        // To get diagnostics, we need to give the server time to analyze.
        // Send a dummy request that the server will process, collecting
        // diagnostics notifications along the way.
        //
        // We use textDocument/documentSymbol as a "ping" — most servers support it
        // and it forces analysis of the file.
        let params = DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        match self.transport.send_request("textDocument/documentSymbol", &params) {
            Ok((_result, notifications)) => {
                self.process_notifications(notifications);
            }
            Err(e) => {
                tracing::debug!("documentSymbol request failed (ok, just for diagnostics): {e}");
            }
        }

        Ok(self.diagnostics.get(&uri).cloned().unwrap_or_default())
    }

    /// Go to definition of the symbol at the given position.
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

        let (result, notifications) = self
            .transport
            .send_request("textDocument/definition", &params)?;
        self.process_notifications(notifications);

        parse_locations(result)
    }

    /// Find all references to the symbol at the given position.
    pub fn references(&mut self, path: &Path, line: u32, character: u32) -> Result<Vec<Location>> {
        let uri = self.ensure_open(path)?;

        if self.capabilities.references_provider.is_none() {
            return Err(anyhow::anyhow!(
                "server does not support find-references"
            ));
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

        let (result, notifications) = self
            .transport
            .send_request("textDocument/references", &params)?;
        self.process_notifications(notifications);

        parse_locations(result)
    }

    /// Get a rename plan (read-only) for the symbol at the given position.
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

        let (result, notifications) = self
            .transport
            .send_request("textDocument/rename", &params)?;
        self.process_notifications(notifications);

        serde_json::from_value(result).context("failed to parse WorkspaceEdit")
    }

    /// Gracefully shut down the server.
    fn transport_shutdown(mut self) -> Result<()> {
        let _ = self.transport.send_request("shutdown", Value::Null);
        let _ = self.transport.send_notification("exit", Value::Null);
        let _ = self._process.kill();
        Ok(())
    }
}

/// Convert a filesystem path to a `file://` URI.
fn path_to_uri(path: &Path) -> Result<Uri> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let uri_string = format!("file://{}", abs.display());
    uri_string
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid URI for path {}: {e}", abs.display()))
}

/// Extract a filesystem path from a `file://` URI string.
pub fn uri_to_path(uri_str: &str) -> Option<PathBuf> {
    uri_str
        .strip_prefix("file://")
        .map(|p| PathBuf::from(p.replace("%20", " ")))
}

/// Parse a definition/references response into a list of `Location`s.
///
/// The response can be `Location`, `Location[]`, `LocationLink[]`, or `null`.
fn parse_locations(value: Value) -> Result<Vec<Location>> {
    if value.is_null() {
        return Ok(Vec::new());
    }

    // Try as single Location
    if let Ok(loc) = serde_json::from_value::<Location>(value.clone()) {
        return Ok(vec![loc]);
    }

    // Try as Location[]
    if let Ok(locs) = serde_json::from_value::<Vec<Location>>(value.clone()) {
        return Ok(locs);
    }

    // Try as LocationLink[]
    if let Ok(links) = serde_json::from_value::<Vec<LocationLink>>(value) {
        return Ok(links
            .into_iter()
            .map(|link| Location {
                uri: link.target_uri,
                range: link.target_selection_range,
            })
            .collect());
    }

    Ok(Vec::new())
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

    #[test]
    fn lsp_manager_new() {
        let dir = tempdir().unwrap();
        let mgr = LspManager::new(dir.path().to_path_buf());
        assert!(!mgr.has_servers());
        assert!(mgr.running_languages().is_empty());
    }

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
    fn parse_locations_null() {
        let result = parse_locations(Value::Null).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_locations_single() {
        let json = serde_json::json!({
            "uri": "file:///test.rs",
            "range": {
                "start": {"line": 10, "character": 5},
                "end": {"line": 10, "character": 15}
            }
        });
        let result = parse_locations(json).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].range.start.line, 10);
    }

    #[test]
    fn parse_locations_array() {
        let json = serde_json::json!([
            {
                "uri": "file:///a.rs",
                "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 5}}
            },
            {
                "uri": "file:///b.rs",
                "range": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 5}}
            }
        ]);
        let result = parse_locations(json).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_locations_link_array() {
        let json = serde_json::json!([
            {
                "targetUri": "file:///target.rs",
                "targetRange": {"start": {"line": 5, "character": 0}, "end": {"line": 10, "character": 0}},
                "targetSelectionRange": {"start": {"line": 5, "character": 4}, "end": {"line": 5, "character": 15}}
            }
        ]);
        let result = parse_locations(json).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].range.start.line, 5);
        assert_eq!(result[0].range.start.character, 4);
    }
}
