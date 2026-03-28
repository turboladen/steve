use serde_json::Value;

use crate::{
    tool::ToolName,
    ui::message_block::{DiffContent, DiffLine},
};

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
            let op = args
                .get("operation")
                .and_then(|v| v.as_str())
                .unwrap_or("list_symbols");
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
            let op = args
                .get("operation")
                .and_then(|v| v.as_str())
                .unwrap_or("diagnostics");
            match op {
                "diagnostics" => format!("{path} diagnostics"),
                _ => {
                    let line = args.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                    format!("{path} {op}@{line}")
                }
            }
        }
        ToolName::Agent => {
            let agent_type = args
                .get("agent_type")
                .and_then(|v| v.as_str())
                .unwrap_or("explore");
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
                            let old = edit
                                .get("old_string")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let new = edit
                                .get("new_string")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::message_block::{DiffContent, DiffLine};
    use serde_json::json;
    use strum::IntoEnumIterator;

    // -- extract_args_summary tests --

    #[test]
    fn extract_args_summary_read_path() {
        let args = json!({"path": "src/main.rs"});
        assert_eq!(extract_args_summary(ToolName::Read, &args), "src/main.rs");
    }

    #[test]
    fn extract_args_summary_list_path() {
        let args = json!({"path": "/tmp/dir"});
        assert_eq!(extract_args_summary(ToolName::List, &args), "/tmp/dir");
    }

    #[test]
    fn extract_args_summary_grep_pattern() {
        let args = json!({"pattern": "fn main"});
        assert_eq!(extract_args_summary(ToolName::Grep, &args), "fn main");
    }

    #[test]
    fn extract_args_summary_glob_pattern() {
        let args = json!({"pattern": "**/*.rs"});
        assert_eq!(extract_args_summary(ToolName::Glob, &args), "**/*.rs");
    }

    #[test]
    fn extract_args_summary_edit_path() {
        let args = json!({"file_path": "src/lib.rs", "old_string": "x", "new_string": "y"});
        assert_eq!(extract_args_summary(ToolName::Edit, &args), "src/lib.rs");
    }

    #[test]
    fn extract_args_summary_write_path() {
        let args = json!({"file_path": "new_file.txt", "content": "hello"});
        assert_eq!(extract_args_summary(ToolName::Write, &args), "new_file.txt");
    }

    #[test]
    fn extract_args_summary_patch_path() {
        let args = json!({"file_path": "src/app.rs", "diff": "..."});
        assert_eq!(extract_args_summary(ToolName::Patch, &args), "src/app.rs");
    }

    #[test]
    fn extract_args_summary_bash_short_command() {
        let args = json!({"command": "ls -la"});
        assert_eq!(extract_args_summary(ToolName::Bash, &args), "ls -la");
    }

    #[test]
    fn extract_args_summary_bash_long_command_truncates() {
        let long_cmd = "a".repeat(50);
        let args = json!({"command": long_cmd});
        let result = extract_args_summary(ToolName::Bash, &args);
        assert_eq!(result.chars().count(), 40); // 37 + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn extract_args_summary_bash_exactly_40_chars() {
        let cmd = "a".repeat(40);
        let args = json!({"command": cmd});
        let result = extract_args_summary(ToolName::Bash, &args);
        assert_eq!(result.chars().count(), 40);
        assert!(!result.ends_with("..."));
    }

    #[test]
    fn extract_args_summary_question_short() {
        let args = json!({"question": "What is this?"});
        assert_eq!(
            extract_args_summary(ToolName::Question, &args),
            "What is this?"
        );
    }

    #[test]
    fn extract_args_summary_question_long_truncates() {
        let long_text = "a".repeat(40);
        let args = json!({"question": long_text});
        let result = extract_args_summary(ToolName::Question, &args);
        assert_eq!(result.chars().count(), 30); // 27 + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn extract_args_summary_task_returns_action() {
        let args = json!({"action": "create", "title": "something"});
        assert_eq!(extract_args_summary(ToolName::Task, &args), "create");
    }

    #[test]
    fn extract_args_summary_webfetch_url() {
        let args = json!({"url": "https://example.com"});
        assert_eq!(
            extract_args_summary(ToolName::Webfetch, &args),
            "https://example.com"
        );
    }

    #[test]
    fn extract_args_summary_missing_field_returns_empty() {
        let args = json!({});
        assert_eq!(extract_args_summary(ToolName::Read, &args), "");
        assert_eq!(extract_args_summary(ToolName::Grep, &args), "");
        assert_eq!(extract_args_summary(ToolName::Edit, &args), "");
        assert_eq!(extract_args_summary(ToolName::Bash, &args), "");
        assert_eq!(extract_args_summary(ToolName::Question, &args), "");
        assert_eq!(extract_args_summary(ToolName::Webfetch, &args), "");
        assert_eq!(extract_args_summary(ToolName::Memory, &args), "");
    }

    #[test]
    fn extract_args_summary_all_variants_covered() {
        // Ensure every ToolName variant is handled (exhaustive match).
        // This test will fail to compile if a new variant is added without
        // updating extract_args_summary.
        let args = json!({});
        for tool in ToolName::iter() {
            // Just ensure it doesn't panic
            let _ = extract_args_summary(tool, &args);
        }
    }

    // -- extract_diff_content tests --

    #[test]
    fn diff_content_edit_basic() {
        let args = json!({
            "file_path": "src/main.rs",
            "old_string": "use std::collections::HashMap;",
            "new_string": "use std::collections::BTreeMap;"
        });
        let result = extract_diff_content(ToolName::Edit, &args);
        match result {
            Some(DiffContent::EditDiff { lines }) => {
                assert_eq!(lines.len(), 2);
                assert_eq!(
                    lines[0],
                    DiffLine::Removal("use std::collections::HashMap;".into())
                );
                assert_eq!(
                    lines[1],
                    DiffLine::Addition("use std::collections::BTreeMap;".into())
                );
            }
            other => panic!("expected EditDiff, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_edit_multiline() {
        let args = json!({
            "file_path": "f.rs",
            "old_string": "line1\nline2",
            "new_string": "new1\nnew2\nnew3"
        });
        match extract_diff_content(ToolName::Edit, &args) {
            Some(DiffContent::EditDiff { lines }) => {
                assert_eq!(lines.len(), 5);
                assert_eq!(lines[0], DiffLine::Removal("line1".into()));
                assert_eq!(lines[1], DiffLine::Removal("line2".into()));
                assert_eq!(lines[2], DiffLine::Addition("new1".into()));
                assert_eq!(lines[3], DiffLine::Addition("new2".into()));
                assert_eq!(lines[4], DiffLine::Addition("new3".into()));
            }
            other => panic!("expected EditDiff, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_edit_empty_strings_returns_none() {
        let args = json!({"file_path": "f.rs", "old_string": "", "new_string": ""});
        assert!(extract_diff_content(ToolName::Edit, &args).is_none());
    }

    #[test]
    fn diff_content_edit_missing_args_returns_none() {
        let args = json!({"file_path": "f.rs"});
        assert!(extract_diff_content(ToolName::Edit, &args).is_none());
    }

    #[test]
    fn diff_content_edit_insert_lines() {
        let args = json!({
            "file_path": "f.rs",
            "operation": "insert_lines",
            "line": 5,
            "content": "new line 1\nnew line 2"
        });
        match extract_diff_content(ToolName::Edit, &args) {
            Some(DiffContent::EditDiff { lines }) => {
                assert_eq!(lines.len(), 3);
                assert_eq!(lines[0], DiffLine::HunkHeader("@@ +5 @@".into()));
                assert_eq!(lines[1], DiffLine::Addition("new line 1".into()));
                assert_eq!(lines[2], DiffLine::Addition("new line 2".into()));
            }
            other => panic!("expected EditDiff, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_edit_insert_lines_empty_content_returns_none() {
        let args = json!({
            "file_path": "f.rs",
            "operation": "insert_lines",
            "line": 1,
            "content": ""
        });
        assert!(extract_diff_content(ToolName::Edit, &args).is_none());
    }

    #[test]
    fn diff_content_edit_delete_lines() {
        let args = json!({
            "file_path": "f.rs",
            "operation": "delete_lines",
            "start_line": 3,
            "end_line": 7
        });
        match extract_diff_content(ToolName::Edit, &args) {
            Some(DiffContent::EditDiff { lines }) => {
                assert_eq!(lines.len(), 2);
                assert_eq!(lines[0], DiffLine::HunkHeader("@@ -3,5 @@".into()));
                assert_eq!(lines[1], DiffLine::Removal("(5 line(s) deleted)".into()));
            }
            other => panic!("expected EditDiff, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_edit_replace_range() {
        let args = json!({
            "file_path": "f.rs",
            "operation": "replace_range",
            "start_line": 2,
            "end_line": 4,
            "content": "replaced1\nreplaced2"
        });
        match extract_diff_content(ToolName::Edit, &args) {
            Some(DiffContent::EditDiff { lines }) => {
                assert_eq!(lines.len(), 4);
                assert_eq!(lines[0], DiffLine::HunkHeader("@@ -2,3 @@".into()));
                assert_eq!(lines[1], DiffLine::Removal("(3 line(s) replaced)".into()));
                assert_eq!(lines[2], DiffLine::Addition("replaced1".into()));
                assert_eq!(lines[3], DiffLine::Addition("replaced2".into()));
            }
            other => panic!("expected EditDiff, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_edit_unknown_operation_returns_none() {
        let args = json!({
            "file_path": "f.rs",
            "operation": "teleport"
        });
        assert!(extract_diff_content(ToolName::Edit, &args).is_none());
    }

    #[test]
    fn diff_content_write_basic() {
        let args = json!({"file_path": "new.txt", "content": "line1\nline2\nline3"});
        match extract_diff_content(ToolName::Write, &args) {
            Some(DiffContent::WriteSummary { line_count }) => {
                assert_eq!(line_count, 3);
            }
            other => panic!("expected WriteSummary, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_write_empty_content() {
        let args = json!({"file_path": "empty.txt", "content": ""});
        match extract_diff_content(ToolName::Write, &args) {
            Some(DiffContent::WriteSummary { line_count }) => {
                assert_eq!(line_count, 0);
            }
            other => panic!("expected WriteSummary, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_write_missing_content() {
        let args = json!({"file_path": "f.txt"});
        match extract_diff_content(ToolName::Write, &args) {
            Some(DiffContent::WriteSummary { line_count }) => {
                assert_eq!(line_count, 0);
            }
            other => panic!("expected WriteSummary, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_patch_basic() {
        let diff_str = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,3 @@\n context\n-old line\n+new line\n context2";
        let args = json!({"file_path": "src/main.rs", "diff": diff_str});
        match extract_diff_content(ToolName::Patch, &args) {
            Some(DiffContent::PatchDiff { lines }) => {
                assert_eq!(lines.len(), 5);
                assert_eq!(lines[0], DiffLine::HunkHeader("@@ -1,3 +1,3 @@".into()));
                assert_eq!(lines[1], DiffLine::Context("context".into()));
                assert_eq!(lines[2], DiffLine::Removal("old line".into()));
                assert_eq!(lines[3], DiffLine::Addition("new line".into()));
                assert_eq!(lines[4], DiffLine::Context("context2".into()));
            }
            other => panic!("expected PatchDiff, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_patch_empty_returns_none() {
        let args = json!({"file_path": "f.rs", "diff": ""});
        assert!(extract_diff_content(ToolName::Patch, &args).is_none());
    }

    #[test]
    fn diff_content_non_write_tools_return_none() {
        let args = json!({"path": "src/main.rs"});
        for tool in [
            ToolName::Read,
            ToolName::Grep,
            ToolName::Glob,
            ToolName::List,
            ToolName::Bash,
            ToolName::Question,
            ToolName::Task,
            ToolName::Webfetch,
            ToolName::Memory,
        ] {
            assert!(
                extract_diff_content(tool, &args).is_none(),
                "{tool} should return None"
            );
        }
    }

    #[test]
    fn diff_content_all_variants_covered() {
        let args = json!({});
        for tool in ToolName::iter() {
            let result = extract_diff_content(tool, &args);
            // Write tools produce diff content; all others return None.
            // Empty args produce None for write tools too, but the exhaustive
            // match is the point — a new variant without a match arm won't compile.
            if matches!(tool, ToolName::Edit | ToolName::Write | ToolName::Patch) {
                // With empty args, write tools may return None (no old_string etc.)
                // — the key assertion is that this doesn't panic.
                let _ = result;
            } else {
                assert!(
                    result.is_none(),
                    "{tool} should return None for diff content"
                );
            }
        }
    }

    // -- parse_unified_diff_lines tests --

    #[test]
    fn parse_diff_skips_file_headers() {
        let patch = "--- a/file.rs\n+++ b/file.rs\n@@ -1 +1 @@\n-old\n+new";
        let lines = parse_unified_diff_lines(patch);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], DiffLine::HunkHeader("@@ -1 +1 @@".into()));
        assert_eq!(lines[1], DiffLine::Removal("old".into()));
        assert_eq!(lines[2], DiffLine::Addition("new".into()));
    }

    #[test]
    fn parse_diff_context_lines() {
        let patch = "@@ -1,3 +1,3 @@\n unchanged\n-removed\n+added\n still here";
        let lines = parse_unified_diff_lines(patch);
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0], DiffLine::HunkHeader("@@ -1,3 +1,3 @@".into()));
        assert_eq!(lines[1], DiffLine::Context("unchanged".into()));
        assert_eq!(lines[2], DiffLine::Removal("removed".into()));
        assert_eq!(lines[3], DiffLine::Addition("added".into()));
        assert_eq!(lines[4], DiffLine::Context("still here".into()));
    }

    #[test]
    fn parse_diff_empty_string() {
        let lines = parse_unified_diff_lines("");
        assert!(lines.is_empty());
    }
}
