//! Tool call helpers: accumulation, validation, permission summaries, and cache invalidation.

use std::collections::HashMap;

use async_openai::types::chat::{
    ChatCompletionMessageToolCalls, ChatCompletionRequestAssistantMessageContent,
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageContent,
    ChatCompletionRequestToolMessageContent, ChatCompletionRequestUserMessageContent,
};
use serde_json::Value;

use crate::{
    context::cache::ToolResultCache,
    tool::{EditOperation, ToolName},
};

/// Accumulated tool call from streaming fragments.
#[derive(Default)]
pub(super) struct PendingToolCall {
    pub id: String,
    pub function_name: String,
    pub arguments: String,
}

/// Extract the primary file path from tool arguments for path-based permission checks.
///
/// Returns `None` for tools that don't operate on file paths (bash, question, task).
pub(super) fn extract_tool_path(tool_name: ToolName, args: &Value) -> Option<String> {
    // For permission checks, prefer the write destination (last key).
    // move/copy: last = "to_path"; single-key tools: last = only key.
    tool_name
        .path_arg_keys()
        .last()
        .and_then(|k| args.get(*k).and_then(|v| v.as_str()).map(|s| s.to_string()))
}

/// Build a human-readable summary of what a tool call wants to do.
pub(super) fn build_permission_summary(tool_name: ToolName, args: &Value) -> String {
    match tool_name {
        ToolName::Bash => {
            let cmd = args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown command)");
            format!("Run command: {cmd}")
        }
        ToolName::Edit => {
            let file = args
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown file)");
            let operation: EditOperation = args
                .get("operation")
                .and_then(|v| v.as_str())
                .unwrap_or("find_replace")
                .parse()
                .unwrap_or(EditOperation::FindReplace);
            match operation {
                EditOperation::InsertLines => {
                    let line = args.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                    format!("Insert lines at line {line} in {file}")
                }
                EditOperation::DeleteLines => {
                    let start = args.get("start_line").and_then(|v| v.as_u64()).unwrap_or(0);
                    let end = args.get("end_line").and_then(|v| v.as_u64()).unwrap_or(0);
                    format!("Delete lines {start}-{end} from {file}")
                }
                EditOperation::ReplaceRange => {
                    let start = args.get("start_line").and_then(|v| v.as_u64()).unwrap_or(0);
                    let end = args.get("end_line").and_then(|v| v.as_u64()).unwrap_or(0);
                    format!("Replace lines {start}-{end} in {file}")
                }
                EditOperation::FindReplace => format!("Edit file: {file}"),
                EditOperation::MultiFindReplace => {
                    let count = args
                        .get("edits")
                        .and_then(|v| v.as_array())
                        .map_or(0, |a| a.len());
                    format!("Multi-edit ({count} replacements) in {file}")
                }
            }
        }
        ToolName::Write => {
            let file = args
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown file)");
            format!("Write file: {file}")
        }
        ToolName::Patch => {
            let file = args
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown file)");
            format!("Patch file: {file}")
        }
        ToolName::Move | ToolName::Copy => {
            let from = args
                .get("from_path")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            let to = args
                .get("to_path")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            format!("{tool_name}: {from} \u{2192} {to}")
        }
        ToolName::Delete => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            format!("Delete: {path}")
        }
        ToolName::Mkdir => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            format!("Create directory: {path}")
        }
        ToolName::Read
        | ToolName::Grep
        | ToolName::Glob
        | ToolName::List
        | ToolName::Question
        | ToolName::Task
        | ToolName::Webfetch
        | ToolName::Memory
        | ToolName::Symbols
        | ToolName::Lsp
        | ToolName::FindSymbol => {
            format!(
                "{tool_name}: {}",
                serde_json::to_string(args).unwrap_or_default()
            )
        }
        ToolName::Agent => {
            let agent_type = args
                .get("agent_type")
                .and_then(|v| v.as_str())
                .unwrap_or("explore");
            let task = args
                .get("task")
                .and_then(|v| v.as_str())
                .unwrap_or("(no task)");
            format!("Spawn {agent_type} agent: {task}")
        }
    }
}

/// Accumulate a tool call fragment from a stream chunk delta.
/// Updates the pending_tool_calls map with the fragment data.
/// Returns true if this fragment introduces a new tool call (has a function name).
pub(super) fn accumulate_tool_call(
    pending: &mut HashMap<u32, PendingToolCall>,
    index: u32,
    id: Option<&str>,
    name: Option<&str>,
    arguments: Option<&str>,
) -> bool {
    let entry = pending.entry(index).or_default();

    if let Some(id) = id {
        entry.id = id.to_string();
    }
    if let Some(name) = name {
        entry.function_name = name.to_string();
    }
    if let Some(args) = arguments {
        entry.arguments.push_str(args);
    }

    // A new tool call is signaled when a function name is provided
    name.is_some()
}

/// Check if a pending tool call is valid: non-empty id, function_name,
/// and parseable JSON arguments.
pub(super) fn is_valid_tool_call(tc: &PendingToolCall) -> bool {
    !tc.arguments.is_empty()
        && !tc.id.is_empty()
        && !tc.function_name.is_empty()
        && serde_json::from_str::<Value>(&tc.arguments).is_ok()
}

/// Invalidate cache entries for paths affected by a write tool.
///
/// Different write tools use different argument keys for their paths:
/// - `edit`/`write`/`patch` use `"file_path"`
/// - `move`/`copy` use `"from_path"` and `"to_path"`
/// - `delete`/`mkdir` use `"path"`
pub(super) fn invalidate_write_tool_cache(
    tool_name: ToolName,
    args: &Value,
    cache: &mut ToolResultCache,
) {
    // Guard: only write tools should invalidate cache entries. Read tools also
    // have non-empty path_arg_keys() but must not trigger invalidation.
    if !tool_name.is_write_tool() {
        return;
    }
    for key in tool_name.path_arg_keys() {
        if let Some(path) = args.get(*key).and_then(|v| v.as_str()) {
            cache.invalidate_path(path);
        }
    }
}

/// Path arg keys to notify LSP about for a write tool.
/// For move, only the destination matters (source no longer exists).
/// For copy, the source is unchanged so only the destination needs notification.
fn write_tool_notify_keys(tool_name: ToolName) -> &'static [&'static str] {
    let keys = tool_name.path_arg_keys();
    if matches!(tool_name, ToolName::Move | ToolName::Copy) {
        keys.last().map(std::slice::from_ref).unwrap_or(&[])
    } else {
        keys
    }
}

/// Send LSP notifications and collect diagnostics after a write tool executes.
/// Returns `(error_count, summary_string)` or `None` if no new errors.
pub(super) fn notify_and_diagnose_write_tool(
    tool_name: ToolName,
    args: &Value,
    ctx: &crate::tool::ToolContext,
) -> Option<(usize, String)> {
    use async_lsp::lsp_types::DiagnosticSeverity;

    if !tool_name.is_write_tool() || matches!(tool_name, ToolName::Delete | ToolName::Mkdir) {
        return None;
    }

    let lsp_manager = ctx.lsp_manager.as_ref()?;
    let notify_keys = write_tool_notify_keys(tool_name);

    let mgr = match lsp_manager.read() {
        Ok(mgr) => mgr,
        Err(_) => return None,
    };

    let mut all_errors: Vec<String> = Vec::new();

    for key in notify_keys {
        let Some(path_str) = args.get(*key).and_then(|v| v.as_str()) else {
            continue;
        };
        let path = crate::tool::resolve_path(path_str, &ctx.project_root);

        // Snapshot errors before sending didChange so we can detect NEW errors.
        // The file is already written to disk but the server hasn't been notified
        // yet. Stale diagnostics from in-flight analysis can arrive after didChange,
        // so we only report errors that weren't already in the cache.
        let pre_notify_errors: std::collections::HashSet<String> =
            if let Ok(server) = mgr.server_for_file(&path) {
                server
                    .cached_diagnostics(&path)
                    .unwrap_or_default()
                    .iter()
                    .filter(|d| d.severity == Some(DiagnosticSeverity::ERROR))
                    .map(|d| {
                        format!(
                            "{}:{}:{}",
                            d.range.start.line, d.range.start.character, d.message
                        )
                    })
                    .collect()
            } else {
                std::collections::HashSet::new()
            };

        match mgr.notify_file_changed(&path) {
            Ok(()) => {
                tracing::debug!(path = %path.display(), "LSP didChange/didSave sent");
            }
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "LSP notify_file_changed failed");
                continue;
            }
        }

        // Poll cached diagnostics — the server pushes publishDiagnostics
        // asynchronously after processing didChange. We poll a few times
        // with short delays to catch errors without excessive blocking.
        // block_in_place tells tokio to move other tasks off this thread
        // while we sleep.
        match mgr.server_for_file(&path) {
            Ok(server) => {
                let mut diags = Vec::new();
                let mut found_at: Option<usize> = None;
                for attempt in 0..4 {
                    tokio::task::block_in_place(|| {
                        std::thread::sleep(std::time::Duration::from_millis(150));
                    });
                    match server.cached_diagnostics(&path) {
                        Ok(d) => {
                            if !d.is_empty() {
                                diags = d;
                                if found_at.is_none() {
                                    found_at = Some(attempt);
                                } else {
                                    tracing::debug!(
                                        path = %path.display(),
                                        count = diags.len(),
                                        attempt,
                                        "LSP diagnostics settled after edit"
                                    );
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::debug!(
                                path = %path.display(),
                                error = %e,
                                "LSP cached_diagnostics failed"
                            );
                            break;
                        }
                    }
                }
                let file_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown");
                for diag in &diags {
                    if diag.severity == Some(DiagnosticSeverity::ERROR) {
                        let key = format!("{}:{}", diag.range.start.line, diag.message);
                        if !pre_notify_errors.contains(&key) {
                            let line = diag.range.start.line + 1;
                            all_errors.push(format!("  {}:{}: {}", file_name, line, diag.message));
                        }
                    }
                }
                tracing::debug!(
                    path = %path.display(),
                    total_diags = diags.len(),
                    pre_edit = pre_notify_errors.len(),
                    new_errors = all_errors.len(),
                    "LSP post-edit diagnostic comparison"
                );
            }
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "no LSP server for file");
            }
        }
    }

    if all_errors.is_empty() {
        None
    } else {
        const MAX_SHOWN: usize = 10;
        let total = all_errors.len();
        let mut summary = String::from("\n\n[LSP Errors]\n");
        for error in all_errors.iter().take(MAX_SHOWN) {
            summary.push_str(error);
            summary.push('\n');
        }
        if total > MAX_SHOWN {
            summary.push_str(&format!("  ... and {} more errors\n", total - MAX_SHOWN));
        }
        Some((total, summary))
    }
}

/// Estimate the character count of a message for token approximation.
pub(super) fn estimate_message_chars(msg: &ChatCompletionRequestMessage) -> usize {
    match msg {
        ChatCompletionRequestMessage::System(s) => match &s.content {
            ChatCompletionRequestSystemMessageContent::Text(t) => t.len(),
            _ => 0,
        },
        ChatCompletionRequestMessage::User(u) => match &u.content {
            ChatCompletionRequestUserMessageContent::Text(t) => t.len(),
            _ => 0,
        },
        ChatCompletionRequestMessage::Assistant(a) => {
            let content_len = match &a.content {
                Some(ChatCompletionRequestAssistantMessageContent::Text(t)) => t.len(),
                _ => 0,
            };
            let tool_calls_len = a
                .tool_calls
                .as_ref()
                .map(|tcs| {
                    tcs.iter()
                        .map(|tc| {
                            if let ChatCompletionMessageToolCalls::Function(f) = tc {
                                f.function.name.len() + f.function.arguments.len()
                            } else {
                                0
                            }
                        })
                        .sum::<usize>()
                })
                .unwrap_or(0);
            content_len + tool_calls_len
        }
        ChatCompletionRequestMessage::Tool(t) => match &t.content {
            ChatCompletionRequestToolMessageContent::Text(t) => t.len(),
            _ => 0,
        },
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;

    // -- accumulate_tool_call tests --

    #[test]
    fn accumulate_new_tool_call() {
        let mut pending = HashMap::new();
        let is_new = accumulate_tool_call(
            &mut pending,
            0,
            Some("call_123"),
            Some("read"),
            Some("{\"path\":"),
        );
        assert!(is_new);
        assert_eq!(pending.len(), 1);
        let tc = &pending[&0];
        assert_eq!(tc.id, "call_123");
        assert_eq!(tc.function_name, "read");
        assert_eq!(tc.arguments, "{\"path\":");
    }

    #[test]
    fn accumulate_appends_arguments() {
        let mut pending = HashMap::new();
        accumulate_tool_call(
            &mut pending,
            0,
            Some("call_123"),
            Some("read"),
            Some("{\"path\":"),
        );
        let is_new = accumulate_tool_call(&mut pending, 0, None, None, Some("\"src/main.rs\"}"));
        assert!(!is_new); // No new name, just appending
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[&0].arguments, "{\"path\":\"src/main.rs\"}");
    }

    #[test]
    fn accumulate_multiple_indices() {
        let mut pending = HashMap::new();
        accumulate_tool_call(&mut pending, 0, Some("call_1"), Some("read"), Some("{}"));
        accumulate_tool_call(&mut pending, 1, Some("call_2"), Some("grep"), Some("{}"));
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[&0].function_name, "read");
        assert_eq!(pending[&1].function_name, "grep");
    }

    // -- is_valid_tool_call tests --

    #[test]
    fn valid_tool_call_complete() {
        let tc = PendingToolCall {
            id: "call_123".to_string(),
            function_name: "read".to_string(),
            arguments: r#"{"path":"src/main.rs"}"#.to_string(),
        };
        assert!(is_valid_tool_call(&tc));
    }

    #[test]
    fn invalid_tool_call_truncated_arguments() {
        let tc = PendingToolCall {
            id: "call_123".to_string(),
            function_name: "read".to_string(),
            arguments: r#"{"path":"src/main"#.to_string(), // truncated
        };
        assert!(!is_valid_tool_call(&tc));
    }

    #[test]
    fn invalid_tool_call_empty_id() {
        let tc = PendingToolCall {
            id: String::new(),
            function_name: "read".to_string(),
            arguments: "{}".to_string(),
        };
        assert!(!is_valid_tool_call(&tc));
    }

    #[test]
    fn invalid_tool_call_empty_function_name() {
        let tc = PendingToolCall {
            id: "call_123".to_string(),
            function_name: String::new(),
            arguments: "{}".to_string(),
        };
        assert!(!is_valid_tool_call(&tc));
    }

    #[test]
    fn invalid_tool_call_empty_arguments() {
        let tc = PendingToolCall {
            id: "call_123".to_string(),
            function_name: "read".to_string(),
            arguments: String::new(),
        };
        assert!(!is_valid_tool_call(&tc));
    }

    // -- notify_and_diagnose_write_tool tests --

    fn test_ctx() -> crate::tool::ToolContext {
        crate::tool::test_tool_context(std::path::PathBuf::from("/tmp/test-project"))
    }

    #[test]
    fn notify_diagnose_skips_non_write_tools() {
        let ctx = test_ctx();
        let args = serde_json::json!({"file_path": "src/main.rs"});
        for tool in ToolName::iter() {
            if !tool.is_write_tool() {
                assert!(
                    notify_and_diagnose_write_tool(tool, &args, &ctx).is_none(),
                    "{tool} is not a write tool, should return None"
                );
            }
        }
    }

    #[test]
    fn notify_diagnose_skips_delete_and_mkdir() {
        let ctx = test_ctx();
        let args = serde_json::json!({"path": "/tmp/test-project/foo"});
        assert!(notify_and_diagnose_write_tool(ToolName::Delete, &args, &ctx).is_none());
        assert!(notify_and_diagnose_write_tool(ToolName::Mkdir, &args, &ctx).is_none());
    }

    #[test]
    fn write_tool_notify_keys_uses_destination_for_move_copy() {
        // Move/Copy have ["from_path", "to_path"] — only destination should be notified
        let move_keys = write_tool_notify_keys(ToolName::Move);
        assert_eq!(
            move_keys,
            &["to_path"],
            "Move should only notify destination"
        );

        let copy_keys = write_tool_notify_keys(ToolName::Copy);
        assert_eq!(
            copy_keys,
            &["to_path"],
            "Copy should only notify destination"
        );

        // Edit/Write/Patch should use all their keys
        assert_eq!(write_tool_notify_keys(ToolName::Edit), &["file_path"]);
        assert_eq!(write_tool_notify_keys(ToolName::Write), &["file_path"]);
        assert_eq!(write_tool_notify_keys(ToolName::Patch), &["file_path"]);
    }

    #[test]
    fn notify_diagnose_returns_none_without_lsp_manager() {
        let ctx = test_ctx();
        assert!(ctx.lsp_manager.is_none());
        let args = serde_json::json!({"file_path": "src/main.rs"});
        assert!(notify_and_diagnose_write_tool(ToolName::Edit, &args, &ctx).is_none());
        assert!(notify_and_diagnose_write_tool(ToolName::Write, &args, &ctx).is_none());
        assert!(notify_and_diagnose_write_tool(ToolName::Patch, &args, &ctx).is_none());
    }
}
