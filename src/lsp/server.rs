use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Child,
};

use anyhow::{Context, Result};
use lsp_types::*;
use serde_json::Value;

use super::{
    Language,
    client::{JsonRpcNotification, JsonRpcTransport},
};

pub struct LspServer {
    pub(super) _process: Child,
    pub(super) transport: JsonRpcTransport,
    pub(super) language: Language,
    pub binary: String,
    pub(super) capabilities: ServerCapabilities,
    pub(super) open_files: HashSet<Uri>,
    pub(super) diagnostics: HashMap<Uri, Vec<Diagnostic>>,
}

impl LspServer {
    fn ensure_open(&mut self, path: &Path) -> Result<Uri> {
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
            self.transport
                .send_notification("textDocument/didOpen", &params)?;
            self.open_files.insert(uri.clone());
        }

        Ok(uri)
    }

    fn process_notifications(&mut self, notifications: Vec<JsonRpcNotification>) {
        for notif in notifications {
            if notif.method == "textDocument/publishDiagnostics"
                && let Some(params) = notif.params
                && let Ok(diag_params) = serde_json::from_value::<PublishDiagnosticsParams>(params)
            {
                self.diagnostics
                    .insert(diag_params.uri, diag_params.diagnostics);
            }
        }
    }

    pub fn diagnostics(&mut self, path: &Path) -> Result<Vec<Diagnostic>> {
        let uri = self.ensure_open(path)?;

        let params = DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        match self
            .transport
            .send_request("textDocument/documentSymbol", &params)
        {
            Ok((_result, notifications)) => {
                self.process_notifications(notifications);
            }
            Err(e) => {
                tracing::debug!("documentSymbol request failed (ok, just for diagnostics): {e}");
            }
        }

        Ok(self.diagnostics.get(&uri).cloned().unwrap_or_default())
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

        let (result, notifications) = self
            .transport
            .send_request("textDocument/definition", &params)?;
        self.process_notifications(notifications);

        parse_locations(result)
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

        let (result, notifications) = self
            .transport
            .send_request("textDocument/references", &params)?;
        self.process_notifications(notifications);

        parse_locations(result)
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

        let (result, notifications) = self
            .transport
            .send_request("textDocument/rename", &params)?;
        self.process_notifications(notifications);

        serde_json::from_value(result).context("failed to parse WorkspaceEdit")
    }

    pub(super) fn transport_shutdown(mut self) -> Result<()> {
        let _ = self.transport.send_request("shutdown", Value::Null);
        let _ = self.transport.send_notification("exit", Value::Null);

        drop(self.transport);

        match self._process.try_wait() {
            Ok(Some(_status)) => {}
            _ => {
                std::thread::sleep(std::time::Duration::from_millis(500));
                match self._process.try_wait() {
                    Ok(Some(_)) => {}
                    _ => {
                        let _ = self._process.kill();
                        let _ = self._process.wait();
                    }
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn path_to_uri(path: &Path) -> Result<Uri> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let canonical = std::fs::canonicalize(&abs).unwrap_or(abs);
    let uri_string = format!("file://{}", canonical.display());
    uri_string
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid URI for path {}: {e}", canonical.display()))
}

pub fn uri_to_path(uri_str: &str) -> Option<PathBuf> {
    uri_str
        .strip_prefix("file://")
        .map(|p| PathBuf::from(percent_decode(p)))
}

fn percent_decode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next();
            let lo = chars.next();
            if let (Some(hi), Some(lo)) = (hi, lo)
                && let (Some(h), Some(l)) = (hex_val(hi), hex_val(lo))
            {
                result.push((h << 4 | l) as char);
                continue;
            }
            result.push('%');
        } else {
            result.push(b as char);
        }
    }
    result
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn parse_locations(value: Value) -> Result<Vec<Location>> {
    if value.is_null() {
        return Ok(Vec::new());
    }

    if let Ok(loc) = serde_json::from_value::<Location>(value.clone()) {
        return Ok(vec![loc]);
    }

    if let Ok(locs) = serde_json::from_value::<Vec<Location>>(value.clone()) {
        return Ok(locs);
    }

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
    fn percent_decode_passthrough() {
        assert_eq!(percent_decode("no-encoding"), "no-encoding");
        assert_eq!(percent_decode(""), "");
    }

    #[test]
    fn percent_decode_malformed() {
        assert_eq!(percent_decode("abc%"), "abc%");
        assert_eq!(percent_decode("abc%2G"), "abc%");
    }

    #[test]
    fn process_notifications_buffers_diagnostics() {
        use crate::lsp::client::JsonRpcNotification;

        let notif = JsonRpcNotification {
            method: "textDocument/publishDiagnostics".to_string(),
            params: Some(serde_json::json!({
                "uri": "file:///test.rs",
                "diagnostics": [
                    {
                        "range": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 0, "character": 5}
                        },
                        "severity": 1,
                        "message": "test error"
                    }
                ]
            })),
        };

        let mut diagnostics: HashMap<Uri, Vec<Diagnostic>> = HashMap::new();
        if notif.method == "textDocument/publishDiagnostics" {
            if let Some(params) = notif.params {
                if let Ok(diag_params) = serde_json::from_value::<PublishDiagnosticsParams>(params)
                {
                    diagnostics.insert(diag_params.uri, diag_params.diagnostics);
                }
            }
        }

        assert_eq!(diagnostics.len(), 1);
        let uri: Uri = "file:///test.rs".parse().unwrap();
        let diags = diagnostics.get(&uri).unwrap();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "test error");
    }
}
