use serde_json::Value;

use crate::tool::ToolName;
use crate::ui::message_block::{DiffContent, DiffLine};

/// Extract a compact argument summary for display in tool call lines.
/// Build a compact argument summary for a tool call (e.g., path for read, pattern for grep).
/// Public so `stream.rs` can use it for sub-agent progress updates.
pub fn extract_args_summary(tool_name: ToolName, args: &Value) -> String {
    match tool_name {
        ToolName::Read => {
            if let Some(paths) = args.get("paths").and_then(|v| v.as_array()) {
                format!("{} files", paths.len())
            } else {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let is_count = args.get("count").and_then(|v| v.as_bool()).unwrap_or(false);
                let tail_n = args.get("tail").and_then(|v| v.as_u64());
                if is_count {
                    format!("{path} (count)")
                } else if let Some(n) = tail_n {
                    format!("{path} (tail {n})")
                } else {
                    path.to_string()
                }
            }
        }
        ToolName::List => args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Symbols => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let op = args.get("operation").and_then(|v| v.as_str()).unwrap_or("list_symbols");
            match op {
                "find_scope" => {
                    let line = args.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                    format!("{path} scope@{line}")
                }
                "find_definition" => {
                    let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    format!("{path} def:{name}")
                }
                _ => path.to_string(),
            }
        }
        ToolName::Grep | ToolName::Glob => args
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Edit | ToolName::Write | ToolName::Patch => args
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Move | ToolName::Copy => {
            let from = args.get("from_path").and_then(|v| v.as_str()).unwrap_or("");
            let to = args.get("to_path").and_then(|v| v.as_str()).unwrap_or("");
            format!("{from} \u{2192} {to}")
        }
        ToolName::Delete | ToolName::Mkdir => args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Bash => {
            let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            if cmd.chars().count() > 40 {
                let truncated: String = cmd.chars().take(37).collect();
                format!("{truncated}...")
            } else {
                cmd.to_string()
            }
        }
        ToolName::Question => args
            .get("question")
            .and_then(|v| v.as_str())
            .map(|s| {
                if s.chars().count() > 30 {
                    let truncated: String = s.chars().take(27).collect();
                    format!("{truncated}...")
                } else {
                    s.to_string()
                }
            })
            .unwrap_or_default(),
        ToolName::Task => args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Webfetch => args
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Memory => args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Lsp => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let op = args.get("operation").and_then(|v| v.as_str()).unwrap_or("diagnostics");
            match op {
                "diagnostics" => format!("{path} diagnostics"),
                _ => {
                    let line = args.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                    format!("{path} {op}@{line}")
                }
            }
        }
        ToolName::Agent => {
            let agent_type = args.get("agent_type").and_then(|v| v.as_str()).unwrap_or("explore");
            let task = args.get("task").and_then(|v| v.as_str()).unwrap_or("");
            let truncated = if task.chars().count() > 30 {
                let t: String = task.chars().take(27).collect();
                format!("{t}...")
            } else {
                task.to_string()
            };
            format!("{agent_type}: {truncated}")
        }
    }
}

/// Build a compact result summary for a tool output (truncated to 80 chars).
/// Public so `stream.rs` can use it for sub-agent progress updates.
pub fn extract_result_summary(tool_name: ToolName, output: &crate::tool::ToolOutput) -> String {
    let _ = tool_name; // All tools use the same truncation logic for now
    if output.output.chars().count() > 80 {
        let truncated: String = output.output.chars().take(77).collect();
        format!("{truncated}...")
    } else {
        output.output.clone()
    }
}

/// Extract inline diff content from tool call arguments for UI rendering.
/// Returns `None` for tools that don't produce diffs (read, grep, bash, etc.).
pub(super) fn extract_diff_content(tool_name: ToolName, args: &Value) -> Option<DiffContent> {
    match tool_name {
        ToolName::Edit => {
            let operation = args
                .get("operation")
                .and_then(|v| v.as_str())
                .unwrap_or("find_replace");
            match operation {
                "find_replace" => {
                    let old = args
                        .get("old_string")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let new = args
                        .get("new_string")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if old.is_empty() && new.is_empty() {
                        return None;
                    }
                    let mut lines = Vec::new();
                    for line in old.lines() {
                        lines.push(DiffLine::Removal(line.to_string()));
                    }
                    for line in new.lines() {
                        lines.push(DiffLine::Addition(line.to_string()));
                    }
                    Some(DiffContent::EditDiff { lines })
                }
                "insert_lines" => {
                    let line_num = args.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    if content.is_empty() {
                        return None;
                    }
                    let mut lines = vec![DiffLine::HunkHeader(format!("@@ +{line_num} @@"))];
                    for line in content.lines() {
                        lines.push(DiffLine::Addition(line.to_string()));
                    }
                    Some(DiffContent::EditDiff { lines })
                }
                "delete_lines" => {
                    let start = args.get("start_line").and_then(|v| v.as_u64()).unwrap_or(0);
                    let end = args.get("end_line").and_then(|v| v.as_u64()).unwrap_or(0);
                    let count = end.saturating_sub(start) + 1;
                    let lines = vec![
                        DiffLine::HunkHeader(format!("@@ -{start},{count} @@")),
                        DiffLine::Removal(format!("({count} line(s) deleted)")),
                    ];
                    Some(DiffContent::EditDiff { lines })
                }
                "replace_range" => {
                    let start = args.get("start_line").and_then(|v| v.as_u64()).unwrap_or(0);
                    let end = args.get("end_line").and_then(|v| v.as_u64()).unwrap_or(0);
                    let old_count = end.saturating_sub(start) + 1;
                    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    let mut lines = vec![
                        DiffLine::HunkHeader(format!("@@ -{start},{old_count} @@")),
                        DiffLine::Removal(format!("({old_count} line(s) replaced)")),
                    ];
                    for line in content.lines() {
                        lines.push(DiffLine::Addition(line.to_string()));
                    }
                    Some(DiffContent::EditDiff { lines })
                }
                "multi_find_replace" => {
                    let edits = args.get("edits").and_then(|v| v.as_array());
                    let mut lines = Vec::new();
                    if let Some(edits) = edits {
                        for edit in edits {
                            let old =
                                edit.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
                            let new =
                                edit.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
                            for line in old.lines() {
                                lines.push(DiffLine::Removal(line.to_string()));
                            }
                            for line in new.lines() {
                                lines.push(DiffLine::Addition(line.to_string()));
                            }
                        }
                    }
                    if lines.is_empty() {
                        None
                    } else {
                        Some(DiffContent::EditDiff { lines })
                    }
                }
                other => {
                    tracing::warn!("unhandled edit operation for diff extraction: {other}");
                    None
                }
            }
        }
        ToolName::Write => {
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let line_count = if content.is_empty() {
                0
            } else {
                content.lines().count()
            };
            Some(DiffContent::WriteSummary { line_count })
        }
        ToolName::Patch => {
            let diff = args.get("diff").and_then(|v| v.as_str()).unwrap_or("");
            if diff.is_empty() {
                return None;
            }
            Some(DiffContent::PatchDiff {
                lines: parse_unified_diff_lines(diff),
            })
        }
        ToolName::Read
        | ToolName::Grep
        | ToolName::Glob
        | ToolName::List
        | ToolName::Bash
        | ToolName::Question
        | ToolName::Task
        | ToolName::Webfetch
        | ToolName::Memory
        | ToolName::Move
        | ToolName::Copy
        | ToolName::Delete
        | ToolName::Mkdir
        | ToolName::Symbols
        | ToolName::Lsp
        | ToolName::Agent => None,
    }
}

/// Parse a unified diff string into structured `DiffLine` entries.
/// Skips `---`/`+++` file headers, keeps `@@` hunk headers.
pub(super) fn parse_unified_diff_lines(patch: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    for line in patch.lines() {
        if line.starts_with("---") || line.starts_with("+++") {
            // Skip file headers
            continue;
        } else if line.starts_with("@@") {
            lines.push(DiffLine::HunkHeader(line.to_string()));
        } else if let Some(rest) = line.strip_prefix('-') {
            lines.push(DiffLine::Removal(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix('+') {
            lines.push(DiffLine::Addition(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix(' ') {
            lines.push(DiffLine::Context(rest.to_string()));
        } else {
            // Lines without a prefix (e.g., "No newline at end of file") → context
            lines.push(DiffLine::Context(line.to_string()));
        }
    }
    lines
}
