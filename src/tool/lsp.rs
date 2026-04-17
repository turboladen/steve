//! LSP tool — Language Server Protocol integration for code intelligence.
//!
//! Provides four operations:
//! - `diagnostics`: Get compiler errors/warnings for a file
//! - `definition`: Go to the definition of a symbol
//! - `references`: Find all references to a symbol
//! - `rename`: Get a rename plan (read-only, LLM applies edits separately)

use std::{
    path::Path,
    sync::{Arc, RwLock},
};

use anyhow::Result;
use serde_json::Value;

use crate::lsp::{LspManager, uri_to_path};

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

pub fn tool() -> ToolEntry {
    ToolEntry {
        def: ToolDef {
            name: ToolName::Lsp,
            description: "Primary tool for code navigation and verification. Get compiler \
                diagnostics, jump to definitions, find all references, or plan safe renames \
                via language servers. Prefer this over `grep` for any semantic code query. \
                Use `definition` instead of `grep` to find where a symbol is actually defined. \
                Use `references` to find all usages of a function, type, or variable across \
                the entire project. The `rename` operation returns a read-only plan — apply \
                the listed edits using the `edit` tool. For position-based operations, you \
                can pass `symbol_name` instead of `line`/`character` — the tool resolves \
                the position automatically."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path to query (relative to project root or absolute)"
                    },
                    "operation": {
                        "type": "string",
                        "enum": ["diagnostics", "definition", "references", "rename"],
                        "description": "Operation to perform. Defaults to 'diagnostics'."
                    },
                    "line": {
                        "type": "integer",
                        "description": "1-indexed line number (required with character for definition/references/rename, unless symbol_name is used)"
                    },
                    "character": {
                        "type": "integer",
                        "description": "0-indexed column (required with line for definition/references/rename, unless symbol_name is used)"
                    },
                    "new_name": {
                        "type": "string",
                        "description": "New name for rename operation (required for rename)"
                    },
                    "symbol_name": {
                        "type": "string",
                        "description": "Symbol name to look up (alternative to line/character \
                            for definition/references/rename). The tool resolves the symbol's \
                            position automatically via tree-sitter. If both symbol_name and \
                            line/character are provided, line/character takes precedence."
                    }
                },
                "required": ["path"]
            }),
        },
        handler: Box::new(execute),
    }
}

fn execute(args: Value, ctx: ToolContext) -> Result<ToolOutput> {
    let path_str = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing 'path' argument"))?;

    let raw_operation = args
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("diagnostics");
    let operation: super::LspOperation = match raw_operation.parse() {
        Ok(op) => op,
        Err(_) => {
            return Ok(ToolOutput {
                title: format!("lsp {path_str}"),
                output: format!(
                    "Error: unknown operation: '{raw_operation}'. Expected one of: diagnostics, definition, references, rename"
                ),
                is_error: true,
            });
        }
    };

    // Resolve path relative to project root
    let path = super::resolve_path(path_str, &ctx.project_root);

    if !path.exists() {
        return Ok(ToolOutput {
            title: format!("lsp {path_str}"),
            output: format!("Error: file not found: {}", path.display()),
            is_error: true,
        });
    }

    if !path.is_file() {
        return Ok(ToolOutput {
            title: format!("lsp {path_str}"),
            output: format!(
                "Error: '{}' is a directory, not a file. The `lsp` tool requires a specific file \
                path (e.g. src/main.rs). Use `grep` to search across directories.",
                path.display()
            ),
            is_error: true,
        });
    }

    let lsp_manager = ctx
        .lsp_manager
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("LSP not available"))?;

    match operation {
        super::LspOperation::Diagnostics => execute_diagnostics(lsp_manager, &path, path_str),
        super::LspOperation::Definition => {
            let (line, character) = resolve_position(&args, &path)?;
            execute_definition(lsp_manager, &path, path_str, line, character)
        }
        super::LspOperation::References => {
            let (line, character) = resolve_position(&args, &path)?;
            execute_references(lsp_manager, &path, path_str, line, character)
        }
        super::LspOperation::Rename => {
            let (line, character) = resolve_position(&args, &path)?;
            let new_name = args
                .get("new_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("rename requires 'new_name' argument"))?;
            execute_rename(lsp_manager, &path, path_str, line, character, new_name)
        }
    }
}

/// Resolve position from args: prefer explicit line/character, fall back to symbol_name.
/// Errors if only one of line/character is provided (incomplete position).
fn resolve_position(args: &Value, path: &Path) -> Result<(u32, u32)> {
    let has_line = args.get("line").and_then(|v| v.as_u64()).is_some();
    let has_character = args.get("character").and_then(|v| v.as_u64()).is_some();

    // If both line and character are present, use them (existing behavior)
    if has_line && has_character {
        return extract_position(args);
    }

    // Reject partial position — one without the other is an error
    if has_line && !has_character {
        return Err(anyhow::anyhow!(
            "line provided without character — provide both line and character, or use symbol_name"
        ));
    }
    if has_character && !has_line {
        return Err(anyhow::anyhow!(
            "character provided without line — provide both line and character, or use symbol_name"
        ));
    }

    // Try symbol_name resolution via tree-sitter
    if let Some(symbol_name) = args.get("symbol_name").and_then(|v| v.as_str()) {
        return super::symbols::resolve_symbol_position(path, symbol_name)
            .map_err(|e| anyhow::anyhow!("{e}"));
    }

    // Nothing provided
    Err(anyhow::anyhow!(
        "this operation requires either line/character or symbol_name"
    ))
}

/// Extract line and character from tool arguments.
/// Line is 1-indexed in args, converted to 0-indexed for LSP.
fn extract_position(args: &Value) -> Result<(u32, u32)> {
    let line = args
        .get("line")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("this operation requires 'line' argument (1-indexed)"))?;

    if line == 0 {
        return Err(anyhow::anyhow!("line must be >= 1 (1-indexed)"));
    }

    let character = args
        .get("character")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            anyhow::anyhow!("this operation requires 'character' argument (0-indexed)")
        })?;

    // Convert 1-indexed line to 0-indexed for LSP
    Ok((line as u32 - 1, character as u32))
}

fn execute_diagnostics(
    lsp_manager: &Arc<RwLock<LspManager>>,
    path: &Path,
    path_str: &str,
) -> Result<ToolOutput> {
    // Try read lock (common path — server already running)
    let diagnostics = match lsp_manager.read() {
        Ok(mgr) => match mgr.server_for_file(path) {
            Ok(server) => server.diagnostics(path)?,
            Err(_) => {
                // Server not running, need write lock to start it
                drop(mgr);
                let mut mgr = lsp_manager
                    .write()
                    .map_err(|_| anyhow::anyhow!("LSP manager lock poisoned"))?;
                let server = mgr.server_for_file_or_start(path)?;
                server.diagnostics(path)?
            }
        },
        Err(_) => return Err(anyhow::anyhow!("LSP manager lock poisoned")),
    };

    if diagnostics.is_empty() {
        return Ok(ToolOutput {
            title: format!("lsp {path_str} diagnostics"),
            output: "No diagnostics (clean).".to_string(),
            is_error: false,
        });
    }

    let mut output = format!("{} diagnostic(s) in {path_str}:\n\n", diagnostics.len());
    for diag in &diagnostics {
        let severity = match diag.severity {
            Some(async_lsp::lsp_types::DiagnosticSeverity::ERROR) => "error",
            Some(async_lsp::lsp_types::DiagnosticSeverity::WARNING) => "warning",
            Some(async_lsp::lsp_types::DiagnosticSeverity::INFORMATION) => "info",
            Some(async_lsp::lsp_types::DiagnosticSeverity::HINT) => "hint",
            _ => "note",
        };
        let line = diag.range.start.line + 1; // back to 1-indexed
        let col = diag.range.start.character;
        output.push_str(&format!(
            "{path_str}:{line}:{col} {severity}: {}\n",
            diag.message
        ));
    }

    Ok(ToolOutput {
        title: format!("lsp {path_str} diagnostics"),
        output,
        is_error: false,
    })
}

fn execute_definition(
    lsp_manager: &Arc<RwLock<LspManager>>,
    path: &Path,
    path_str: &str,
    line: u32,
    character: u32,
) -> Result<ToolOutput> {
    // Try read lock (common path — server already running)
    let locations = match lsp_manager.read() {
        Ok(mgr) => match mgr.server_for_file(path) {
            Ok(server) => server.definition(path, line, character)?,
            Err(_) => {
                drop(mgr);
                let mut mgr = lsp_manager
                    .write()
                    .map_err(|_| anyhow::anyhow!("LSP manager lock poisoned"))?;
                let server = mgr.server_for_file_or_start(path)?;
                server.definition(path, line, character)?
            }
        },
        Err(_) => return Err(anyhow::anyhow!("LSP manager lock poisoned")),
    };

    if locations.is_empty() {
        return Ok(ToolOutput {
            title: format!("lsp {path_str} definition@{}", line + 1),
            output: format!(
                "No definition found at {path_str}:{}:{character}.",
                line + 1
            ),
            is_error: false,
        });
    }

    let mut output = format!(
        "Definition(s) for symbol at {path_str}:{}:{character}:\n\n",
        line + 1
    );
    for loc in &locations {
        let file = uri_to_display(loc.uri.as_str());
        let def_line = loc.range.start.line + 1;
        let def_col = loc.range.start.character;

        // Try to read a few lines of context from the target file
        let preview = uri_to_path(loc.uri.as_str())
            .map(|path| read_context(&path, loc.range.start.line as usize, 5))
            .unwrap_or_default();

        output.push_str(&format!("{file}:{def_line}:{def_col}\n"));
        if !preview.is_empty() {
            output.push_str(&preview);
            output.push('\n');
        }
    }

    Ok(ToolOutput {
        title: format!("lsp {path_str} definition@{}", line + 1),
        output,
        is_error: false,
    })
}

fn execute_references(
    lsp_manager: &Arc<RwLock<LspManager>>,
    path: &Path,
    path_str: &str,
    line: u32,
    character: u32,
) -> Result<ToolOutput> {
    // Try read lock (common path — server already running)
    let locations = match lsp_manager.read() {
        Ok(mgr) => match mgr.server_for_file(path) {
            Ok(server) => server.references(path, line, character)?,
            Err(_) => {
                drop(mgr);
                let mut mgr = lsp_manager
                    .write()
                    .map_err(|_| anyhow::anyhow!("LSP manager lock poisoned"))?;
                let server = mgr.server_for_file_or_start(path)?;
                server.references(path, line, character)?
            }
        },
        Err(_) => return Err(anyhow::anyhow!("LSP manager lock poisoned")),
    };

    if locations.is_empty() {
        return Ok(ToolOutput {
            title: format!("lsp {path_str} references@{}", line + 1),
            output: format!(
                "No references found at {path_str}:{}:{character}.",
                line + 1
            ),
            is_error: false,
        });
    }

    // Group by file
    let mut by_file: std::collections::BTreeMap<String, Vec<u32>> =
        std::collections::BTreeMap::new();
    for loc in &locations {
        let file = uri_to_display(loc.uri.as_str());
        by_file
            .entry(file)
            .or_default()
            .push(loc.range.start.line + 1);
    }

    let mut output = format!(
        "{} reference(s) for symbol at {path_str}:{}:{character}:\n\n",
        locations.len(),
        line + 1
    );
    for (file, lines) in &by_file {
        let line_nums: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        output.push_str(&format!("{file}: line {}\n", line_nums.join(", ")));
    }

    Ok(ToolOutput {
        title: format!("lsp {path_str} references@{}", line + 1),
        output,
        is_error: false,
    })
}

fn execute_rename(
    lsp_manager: &Arc<RwLock<LspManager>>,
    path: &Path,
    path_str: &str,
    line: u32,
    character: u32,
    new_name: &str,
) -> Result<ToolOutput> {
    // Try read lock (common path — server already running)
    let edit = match lsp_manager.read() {
        Ok(mgr) => match mgr.server_for_file(path) {
            Ok(server) => server.rename(path, line, character, new_name)?,
            Err(_) => {
                drop(mgr);
                let mut mgr = lsp_manager
                    .write()
                    .map_err(|_| anyhow::anyhow!("LSP manager lock poisoned"))?;
                let server = mgr.server_for_file_or_start(path)?;
                server.rename(path, line, character, new_name)?
            }
        },
        Err(_) => return Err(anyhow::anyhow!("LSP manager lock poisoned")),
    };

    // Format the workspace edit as a readable plan.
    // Servers may return changes in `changes` (simple) or `document_changes` (rich).
    // Normalize to a common (uri, edits) list.
    let file_edits: Vec<(String, Vec<async_lsp::lsp_types::TextEdit>)> =
        if let Some(changes) = edit.changes {
            changes
                .into_iter()
                .map(|(uri, edits)| (uri.as_str().to_string(), edits))
                .collect()
        } else if let Some(doc_changes) = edit.document_changes {
            match doc_changes {
                async_lsp::lsp_types::DocumentChanges::Edits(edits) => edits
                    .into_iter()
                    .map(|e| {
                        (
                            e.text_document.uri.as_str().to_string(),
                            e.edits
                                .into_iter()
                                .map(|edit| match edit {
                                    async_lsp::lsp_types::OneOf::Left(te) => te,
                                    async_lsp::lsp_types::OneOf::Right(ate) => ate.text_edit,
                                })
                                .collect(),
                        )
                    })
                    .collect(),
                async_lsp::lsp_types::DocumentChanges::Operations(ops) => ops
                    .into_iter()
                    .filter_map(|op| match op {
                        async_lsp::lsp_types::DocumentChangeOperation::Edit(e) => Some((
                            e.text_document.uri.as_str().to_string(),
                            e.edits
                                .into_iter()
                                .map(|edit| match edit {
                                    async_lsp::lsp_types::OneOf::Left(te) => te,
                                    async_lsp::lsp_types::OneOf::Right(ate) => ate.text_edit,
                                })
                                .collect(),
                        )),
                        async_lsp::lsp_types::DocumentChangeOperation::Op(_) => None, // file create/rename/delete
                    })
                    .collect(),
            }
        } else {
            Vec::new()
        };

    if file_edits.is_empty() {
        return Ok(ToolOutput {
            title: format!("lsp {path_str} rename@{}", line + 1),
            output: format!(
                "No changes needed for rename at {path_str}:{}:{character}.",
                line + 1
            ),
            is_error: false,
        });
    }

    let total_edits: usize = file_edits.iter().map(|(_, v)| v.len()).sum();
    let mut output = format!(
        "Rename plan: → `{new_name}` ({total_edits} edit(s) across {} file(s))\n\n",
        file_edits.len()
    );

    for (uri, edits) in &file_edits {
        let file = uri_to_display(uri);
        output.push_str(&format!("{file}:\n"));
        for edit in edits {
            let edit_line = edit.range.start.line + 1;
            output.push_str(&format!("  Line {edit_line}: → `{new_name}`\n"));
        }
    }

    output.push_str("\nThis is a read-only plan. Use the edit tool to apply changes.");

    Ok(ToolOutput {
        title: format!("lsp {path_str} rename@{}", line + 1),
        output,
        is_error: false,
    })
}

/// Convert a file URI to a display-friendly path.
fn uri_to_display(uri: &str) -> String {
    // Reuse uri_to_path for proper percent-decoding, fall back to raw URI
    crate::lsp::uri_to_path(uri)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| uri.to_string())
}

/// Read a few lines of context from a file around a given 0-indexed line.
pub(crate) fn read_context(path: &Path, center_line: usize, radius: usize) -> String {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    let lines: Vec<&str> = content.lines().collect();
    let start = center_line.saturating_sub(radius / 2);
    let end = std::cmp::min(start + radius, lines.len());

    let mut result = String::new();
    for (i, line) in lines[start..end].iter().enumerate() {
        let line_num = start + i + 1;
        let marker = if start + i == center_line { "→" } else { " " };
        result.push_str(&format!("{marker} {line_num:>4} | {line}\n"));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_ctx(dir: &Path) -> ToolContext {
        ToolContext {
            project_root: dir.to_path_buf(),
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        }
    }

    fn test_runtime_handle() -> tokio::runtime::Handle {
        use std::sync::OnceLock;
        static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        RT.get_or_init(|| tokio::runtime::Runtime::new().expect("test tokio runtime"))
            .handle()
            .clone()
    }

    fn test_ctx_with_lsp(dir: &Path) -> ToolContext {
        ToolContext {
            project_root: dir.to_path_buf(),
            storage_dir: None,
            task_store: None,
            lsp_manager: Some(Arc::new(RwLock::new(LspManager::new(
                dir.to_path_buf(),
                test_runtime_handle(),
                None,
            )))),
        }
    }

    #[test]
    fn missing_path_arg() {
        let dir = tempdir().unwrap();
        let args = serde_json::json!({});
        let result = execute(args, test_ctx(dir.path()));
        assert!(result.is_err());
    }

    #[test]
    fn file_not_found() {
        let dir = tempdir().unwrap();
        let args = serde_json::json!({"path": "nonexistent.rs"});
        let result = execute(args, test_ctx_with_lsp(dir.path())).unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("not found"));
    }

    #[test]
    fn no_lsp_manager() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        let args = serde_json::json!({"path": file.to_str().unwrap()});
        let result = execute(args, test_ctx(dir.path()));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("LSP not available")
        );
    }

    #[test]
    fn unknown_operation() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "operation": "unknown_op"
        });
        let output = execute(args, test_ctx_with_lsp(dir.path())).unwrap();
        assert!(output.is_error);
        assert!(
            output.output.contains("unknown operation"),
            "expected parse error, got: {}",
            output.output
        );
    }

    #[test]
    fn definition_missing_position() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "operation": "definition"
        });
        let result = execute(args, test_ctx_with_lsp(dir.path()));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("line/character or symbol_name")
        );
    }

    #[test]
    fn definition_line_without_character() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "operation": "definition",
            "line": 1
        });
        let result = execute(args, test_ctx_with_lsp(dir.path()));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("line provided without character")
        );
    }

    #[test]
    fn definition_line_zero_error() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "operation": "definition",
            "line": 0,
            "character": 0
        });
        let result = execute(args, test_ctx_with_lsp(dir.path()));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("line must be >= 1")
        );
    }

    #[test]
    fn rename_missing_new_name() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "operation": "rename",
            "line": 1,
            "character": 3
        });
        let result = execute(args, test_ctx_with_lsp(dir.path()));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("new_name"));
    }

    #[test]
    fn default_operation_is_diagnostics() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.xyz");
        std::fs::write(&file, "content").unwrap();
        // No operation specified, should default to diagnostics
        // This will fail because .xyz has no LSP server, which is fine —
        // we're testing that it picks the right operation
        let args = serde_json::json!({"path": file.to_str().unwrap()});
        let result = execute(args, test_ctx_with_lsp(dir.path()));
        // Should fail with "unsupported language", not "unknown operation"
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("unsupported") || err_msg.contains("no extension"),
            "unexpected error: {err_msg}"
        );
    }

    #[test]
    fn uri_to_display_strips_file_prefix() {
        assert_eq!(
            uri_to_display("file:///home/user/test.rs"),
            "/home/user/test.rs"
        );
        assert_eq!(
            uri_to_display("file:///path%20with%20spaces/test.rs"),
            "/path with spaces/test.rs"
        );
        assert_eq!(uri_to_display("https://example.com"), "https://example.com");
    }

    #[test]
    fn read_context_from_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(
            &file,
            "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\n",
        )
        .unwrap();

        let result = read_context(&file, 3, 5); // center on line 4 (0-indexed 3)
        assert!(result.contains("line 2"));
        assert!(result.contains("line 4"));
        assert!(result.contains("→")); // marker on center line
    }

    #[test]
    fn extract_position_valid() {
        let args = serde_json::json!({"line": 10, "character": 5});
        let (line, char) = extract_position(&args).unwrap();
        assert_eq!(line, 9); // 1-indexed → 0-indexed
        assert_eq!(char, 5); // 0-indexed stays
    }

    // ── resolve_position ────────────────────────────────────────────────

    #[test]
    fn resolve_position_prefers_line_character() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn hello() {}\nfn world() {}\n").unwrap();

        // Both line/character and symbol_name present — line/character wins
        let args = serde_json::json!({
            "line": 2,
            "character": 3,
            "symbol_name": "hello"
        });
        let (line, char) = resolve_position(&args, &file).unwrap();
        assert_eq!(line, 1); // line 2 → 0-indexed 1
        assert_eq!(char, 3);
    }

    #[test]
    fn resolve_position_uses_symbol_name() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn hello() {}\n").unwrap();

        let args = serde_json::json!({"symbol_name": "hello"});
        let (line, char) = resolve_position(&args, &file).unwrap();
        assert_eq!(line, 0);
        assert_eq!(char, 3); // "hello" starts at column 3
    }

    #[test]
    fn resolve_position_neither_provided() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn hello() {}\n").unwrap();

        let args = serde_json::json!({});
        let result = resolve_position(&args, &file);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("line/character or symbol_name")
        );
    }

    #[test]
    fn resolve_position_symbol_not_found() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn hello() {}\n").unwrap();

        let args = serde_json::json!({"symbol_name": "nonexistent"});
        let result = resolve_position(&args, &file);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn resolve_position_line_without_character_errors() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn hello() {}\n").unwrap();

        // line without character, even with symbol_name — should error about incomplete position
        let args = serde_json::json!({"line": 1, "symbol_name": "hello"});
        let result = resolve_position(&args, &file);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("line provided without character")
        );
    }

    #[test]
    fn resolve_position_character_without_line_errors() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn hello() {}\n").unwrap();

        let args = serde_json::json!({"character": 3, "symbol_name": "hello"});
        let result = resolve_position(&args, &file);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("character provided without line")
        );
    }
}
