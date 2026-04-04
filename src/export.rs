//! Debug export — writes the current session as a structured markdown file
//! for external debugging (e.g., pasting into another AI for analysis).

use std::{
    fmt::Write as FmtWrite,
    path::{Path, PathBuf},
};

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::DateTimeExt;

use crate::{
    session::message::{Message, MessagePart, Role, ToolCallState},
    tool::ToolName,
};

/// Parameters for a debug export.
pub struct ExportParams<'a> {
    pub session_id: &'a str,
    pub session_title: &'a str,
    pub session_created_at: DateTime<Utc>,
    pub token_usage: &'a crate::session::types::TokenUsage,
    pub messages: &'a [Message],
    pub system_prompt: Option<String>,
    pub model_ref: Option<&'a str>,
    pub project_root: &'a Path,
    pub include_logs: bool,
}

/// Export the current session to a markdown debug file.
///
/// Returns the path of the written file.
pub fn export_debug(params: &ExportParams) -> Result<PathBuf> {
    let mut out = String::with_capacity(8192);

    write_header(&mut out, params);
    write_system_prompt(&mut out, params.system_prompt.as_deref());
    write_messages(&mut out, params.messages);

    if params.include_logs {
        write_logs(&mut out, params.session_created_at);
    }

    let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
    let filename = format!("steve-debug-{timestamp}.md");
    let path = params.project_root.join(&filename);
    std::fs::write(&path, &out)?;

    Ok(path)
}

// ---------------------------------------------------------------------------
// Markdown sections
// ---------------------------------------------------------------------------

fn write_header(out: &mut String, params: &ExportParams) {
    let _ = writeln!(out, "# Steve Debug Export\n");
    let _ = writeln!(out, "| Field | Value |");
    let _ = writeln!(out, "|-------|-------|");
    let _ = writeln!(out, "| Session ID | `{}` |", params.session_id);
    let _ = writeln!(out, "| Title | {} |", params.session_title);
    let _ = writeln!(
        out,
        "| Model | {} |",
        params.model_ref.unwrap_or("(unknown)")
    );
    let _ = writeln!(
        out,
        "| Created | {} |",
        params.session_created_at.display_full_utc()
    );
    let _ = writeln!(out, "| Exported | {} |", Utc::now().display_full_utc());
    let _ = writeln!(out, "| Messages | {} |", params.messages.len());
    let _ = writeln!(
        out,
        "| Tokens | prompt: {}, completion: {}, total: {} |",
        params.token_usage.prompt_tokens,
        params.token_usage.completion_tokens,
        params.token_usage.total_tokens,
    );
    let _ = writeln!(out);
}

fn write_system_prompt(out: &mut String, system_prompt: Option<&str>) {
    let Some(prompt) = system_prompt else {
        return;
    };
    let _ = writeln!(out, "## System Prompt\n");
    let _ = writeln!(out, "```");
    let _ = writeln!(out, "{prompt}");
    let _ = writeln!(out, "```\n");
}

fn write_messages(out: &mut String, messages: &[Message]) {
    let _ = writeln!(out, "## Conversation\n");

    for msg in messages {
        let role_label = match msg.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::System => "System",
        };
        let ts = msg.created_at.format("%H:%M:%S");
        let _ = writeln!(out, "### {role_label} [{ts}]\n");

        for part in &msg.parts {
            write_message_part(out, part);
        }
    }
}

fn write_message_part(out: &mut String, part: &MessagePart) {
    match part {
        MessagePart::Text { text } => {
            if !text.is_empty() {
                let _ = writeln!(out, "{text}\n");
            }
        }
        MessagePart::Reasoning { text } => {
            let _ = writeln!(out, "> **Thinking:**");
            for line in text.lines() {
                let _ = writeln!(out, "> {line}");
            }
            let _ = writeln!(out);
        }
        MessagePart::ToolCall {
            call_id,
            tool_name,
            input,
            state,
        } => {
            let summary = extract_tool_summary(*tool_name, input);
            let state_label = format_tool_state(state);
            let _ = writeln!(
                out,
                "#### Tool Call: `{tool_name}` — {summary} [{state_label}]\n"
            );
            let _ = writeln!(out, "Call ID: `{call_id}`\n");
            let _ = writeln!(out, "```json");
            let pretty = serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
            let _ = writeln!(out, "{pretty}");
            let _ = writeln!(out, "```\n");
        }
        MessagePart::ToolResult {
            call_id,
            tool_name,
            output,
            title,
            is_error,
        } => {
            let error_label = if *is_error { " [ERROR]" } else { "" };
            let _ = writeln!(
                out,
                "#### Tool Result: `{tool_name}` — {title}{error_label}\n"
            );
            let _ = writeln!(out, "Call ID: `{call_id}`\n");
            let truncated = truncate_tool_output(output, 200);
            let _ = writeln!(out, "```");
            let _ = writeln!(out, "{truncated}");
            let _ = writeln!(out, "```\n");
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a short summary from tool call arguments (similar to `extract_args_summary` in app.rs).
/// Delegate to the shared args summary, but use "(no ...)" fallbacks for export readability.
fn extract_tool_summary(tool_name: ToolName, input: &serde_json::Value) -> String {
    let summary = crate::app::extract_args_summary(tool_name, input);
    if summary.is_empty() {
        // Provide a readable fallback for export context
        match tool_name {
            ToolName::Read
            | ToolName::List
            | ToolName::Delete
            | ToolName::Mkdir
            | ToolName::Edit
            | ToolName::Write
            | ToolName::Patch
            | ToolName::Symbols
            | ToolName::Lsp => "(no path)".to_string(),
            ToolName::Grep | ToolName::Glob => "(no pattern)".to_string(),
            ToolName::Bash => "(no command)".to_string(),
            ToolName::Webfetch => "(no url)".to_string(),
            ToolName::Move | ToolName::Copy => "(no path) \u{2192} (no path)".to_string(),
            ToolName::Agent => "(no task)".to_string(),
            ToolName::Question | ToolName::Task | ToolName::Memory => String::new(),
        }
    } else {
        summary
    }
}

/// Format a `ToolCallState` as a human-readable label.
fn format_tool_state(state: &ToolCallState) -> &'static str {
    match state {
        ToolCallState::Pending => "pending",
        ToolCallState::Running => "running",
        ToolCallState::Completed => "completed",
        ToolCallState::Error { .. } => "error",
        ToolCallState::Denied => "denied",
    }
}

/// Truncate tool output to `max_lines`, keeping first half and last half if over limit.
fn truncate_tool_output(output: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= max_lines {
        return output.to_string();
    }
    let half = max_lines / 2;
    let head: Vec<&str> = lines[..half].to_vec();
    let tail: Vec<&str> = lines[lines.len() - half..].to_vec();
    let omitted = lines.len() - max_lines;
    format!(
        "{}\n\n... ({omitted} lines omitted) ...\n\n{}",
        head.join("\n"),
        tail.join("\n")
    )
}

// ---------------------------------------------------------------------------
// Log filtering
// ---------------------------------------------------------------------------

fn write_logs(out: &mut String, session_start: DateTime<Utc>) {
    let _ = writeln!(out, "## Logs\n");

    let log_dir = match directories::ProjectDirs::from("", "", "steve") {
        Some(dirs) => dirs.data_dir().join("logs"),
        None => {
            let _ = writeln!(out, "*Could not determine log directory.*\n");
            return;
        }
    };

    if !log_dir.exists() {
        let _ = writeln!(out, "*Log directory not found.*\n");
        return;
    }

    let now = Utc::now();
    let session_date = session_start.display_date();
    let now_date = now.display_date();

    // Collect matching log files
    let mut log_files: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&log_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(date) = date_from_log_filename(&path)
                && date >= session_date
                && date <= now_date
            {
                log_files.push(path);
            }
        }
    }

    log_files.sort();

    if log_files.is_empty() {
        let _ = writeln!(out, "*No log files found for session timespan.*\n");
        return;
    }

    let _ = writeln!(out, "```");

    let mut count = 0;
    for log_file in &log_files {
        let mut emitting = false;
        if let Ok(content) = std::fs::read_to_string(log_file) {
            for line in content.lines() {
                if let Some(ts) = parse_log_timestamp(line) {
                    emitting = ts >= session_start && ts <= now;
                    if emitting {
                        let _ = writeln!(out, "{line}");
                        count += 1;
                    }
                } else if emitting {
                    // Continuation line (e.g., multi-line log entry) — include if
                    // the most recent timestamped line in this file was in range
                    let _ = writeln!(out, "{line}");
                }
            }
        }
    }

    let _ = writeln!(out, "```\n");

    if count == 0 {
        let _ = writeln!(out, "*No log entries matched the session timespan.*\n");
    }
}

/// Parse an RFC 3339 timestamp from the start of a log line.
///
/// tracing-appender writes lines like: `2026-03-04T10:23:45.123456Z  INFO steve::app: ...`
fn parse_log_timestamp(line: &str) -> Option<DateTime<Utc>> {
    // The timestamp is the first whitespace-delimited token
    let token = line.split_whitespace().next()?;
    token.parse::<DateTime<Utc>>().ok()
}

/// Extract the date string from a log filename like `steve.log.2026-03-04`.
fn date_from_log_filename(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let suffix = name.strip_prefix("steve.log.")?;
    // Validate it looks like a date (YYYY-MM-DD = 10 chars)
    if suffix.len() >= 10 && suffix[..10].chars().filter(|c| *c == '-').count() == 2 {
        Some(suffix[..10].to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::types::TokenUsage;
    use chrono::TimeZone;
    use serde_json::json;
    use strum::IntoEnumIterator;

    #[test]
    fn parse_log_timestamp_valid() {
        let line = "2026-03-04T10:23:45.123456Z  INFO steve::app: something";
        let ts = parse_log_timestamp(line).unwrap();
        assert_eq!(ts.year(), 2026);
        assert_eq!(ts.month(), 3);
        assert_eq!(ts.day(), 4);
    }

    #[test]
    fn parse_log_timestamp_invalid() {
        assert!(parse_log_timestamp("not a timestamp line").is_none());
        assert!(parse_log_timestamp("").is_none());
        assert!(parse_log_timestamp("  INFO something").is_none());
    }

    #[test]
    fn date_from_log_filename_valid() {
        let path = PathBuf::from("/some/dir/steve.log.2026-03-04");
        assert_eq!(
            date_from_log_filename(&path),
            Some("2026-03-04".to_string())
        );
    }

    #[test]
    fn date_from_log_filename_invalid() {
        assert!(date_from_log_filename(&PathBuf::from("other.log")).is_none());
        assert!(date_from_log_filename(&PathBuf::from("steve.log")).is_none());
        assert!(date_from_log_filename(&PathBuf::from("steve.log.notadate")).is_none());
    }

    #[test]
    fn truncate_tool_output_short() {
        let output = "line1\nline2\nline3";
        assert_eq!(truncate_tool_output(output, 10), output);
    }

    #[test]
    fn truncate_tool_output_long() {
        let lines: Vec<String> = (0..300).map(|i| format!("line {i}")).collect();
        let output = lines.join("\n");
        let result = truncate_tool_output(&output, 200);
        assert!(result.contains("line 0"));
        assert!(result.contains("line 99"));
        assert!(result.contains("100 lines omitted"));
        assert!(result.contains("line 299"));
    }

    #[test]
    fn format_tool_state_variants() {
        // Exhaustive coverage of all ToolCallState variants
        assert_eq!(format_tool_state(&ToolCallState::Pending), "pending");
        assert_eq!(format_tool_state(&ToolCallState::Running), "running");
        assert_eq!(format_tool_state(&ToolCallState::Completed), "completed");
        assert_eq!(
            format_tool_state(&ToolCallState::Error {
                message: "fail".into()
            }),
            "error"
        );
        assert_eq!(format_tool_state(&ToolCallState::Denied), "denied");
    }

    #[test]
    fn extract_tool_summary_all_tools() {
        // Every ToolName variant with empty args: tools with required string fields
        // return a fallback like "(no path)"; tools keyed on optional fields return "".
        let args = json!({});
        for tool in ToolName::iter() {
            let result = extract_tool_summary(tool, &args);
            if matches!(tool, ToolName::Question | ToolName::Task | ToolName::Memory) {
                assert_eq!(
                    result, "",
                    "{tool} with empty args should return empty string"
                );
            } else {
                assert!(
                    !result.is_empty(),
                    "{tool} with empty args should return a non-empty fallback"
                );
            }
        }
    }

    #[test]
    fn extract_tool_summary_specific_values() {
        assert_eq!(
            extract_tool_summary(ToolName::Read, &json!({"path": "src/main.rs"})),
            "src/main.rs"
        );
        assert_eq!(
            extract_tool_summary(ToolName::Grep, &json!({"pattern": "fn main"})),
            "fn main"
        );
        assert_eq!(
            extract_tool_summary(ToolName::Edit, &json!({"file_path": "lib.rs"})),
            "lib.rs"
        );
        assert_eq!(
            extract_tool_summary(ToolName::Bash, &json!({"command": "ls -la"})),
            "ls -la"
        );
        assert_eq!(
            extract_tool_summary(ToolName::Webfetch, &json!({"url": "https://example.com"})),
            "https://example.com"
        );
    }

    #[test]
    fn format_message_part_text() {
        let mut out = String::new();
        write_message_part(
            &mut out,
            &MessagePart::Text {
                text: "hello world".into(),
            },
        );
        assert!(out.contains("hello world"));
    }

    #[test]
    fn format_message_part_reasoning() {
        let mut out = String::new();
        write_message_part(
            &mut out,
            &MessagePart::Reasoning {
                text: "thinking about it".into(),
            },
        );
        assert!(out.contains("> **Thinking:**"));
        assert!(out.contains("> thinking about it"));
    }

    #[test]
    fn format_message_part_tool_call() {
        let mut out = String::new();
        write_message_part(
            &mut out,
            &MessagePart::ToolCall {
                call_id: "call-1".into(),
                tool_name: ToolName::Read,
                input: json!({"path": "src/main.rs"}),
                state: ToolCallState::Completed,
            },
        );
        assert!(out.contains("Tool Call: `read`"));
        assert!(out.contains("src/main.rs"));
        assert!(out.contains("completed"));
        assert!(out.contains("call-1"));
    }

    #[test]
    fn format_message_part_tool_result() {
        let mut out = String::new();
        write_message_part(
            &mut out,
            &MessagePart::ToolResult {
                call_id: "call-1".into(),
                tool_name: ToolName::Read,
                output: "file contents here".into(),
                title: "src/main.rs".into(),
                is_error: false,
            },
        );
        assert!(out.contains("Tool Result: `read`"));
        assert!(out.contains("file contents here"));
        assert!(!out.contains("[ERROR]"));
    }

    #[test]
    fn format_message_part_tool_result_error() {
        let mut out = String::new();
        write_message_part(
            &mut out,
            &MessagePart::ToolResult {
                call_id: "call-1".into(),
                tool_name: ToolName::Bash,
                output: "command failed".into(),
                title: "bash".into(),
                is_error: true,
            },
        );
        assert!(out.contains("[ERROR]"));
    }

    #[test]
    fn export_debug_writes_file() {
        let dir = tempfile::tempdir().unwrap();
        let created = Utc.with_ymd_and_hms(2026, 3, 4, 10, 0, 0).unwrap();
        let usage = TokenUsage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
        };
        let messages = vec![
            Message::user("sess-1", "What is this project?"),
            Message::assistant("sess-1", "It's a Rust TUI coding agent."),
        ];

        let params = ExportParams {
            session_id: "sess-1",
            session_title: "Test Session",
            session_created_at: created,
            token_usage: &usage,
            messages: &messages,
            system_prompt: Some("You are a helpful assistant.".into()),
            model_ref: Some("openai/gpt-4o"),
            project_root: dir.path(),
            include_logs: false,
        };

        let path = export_debug(&params).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();

        // Header fields
        assert!(content.contains("# Steve Debug Export"));
        assert!(content.contains("sess-1"));
        assert!(content.contains("Test Session"));
        assert!(content.contains("openai/gpt-4o"));
        assert!(content.contains("prompt: 1000"));

        // System prompt
        assert!(content.contains("## System Prompt"));
        assert!(content.contains("You are a helpful assistant."));

        // Messages
        assert!(content.contains("### User"));
        assert!(content.contains("What is this project?"));
        assert!(content.contains("### Assistant"));
        assert!(content.contains("It's a Rust TUI coding agent."));
    }

    #[test]
    fn export_debug_without_logs() {
        let dir = tempfile::tempdir().unwrap();
        let params = ExportParams {
            session_id: "s",
            session_title: "t",
            session_created_at: Utc::now(),
            token_usage: &TokenUsage::default(),
            messages: &[],
            system_prompt: None,
            model_ref: None,
            project_root: dir.path(),
            include_logs: false,
        };
        let path = export_debug(&params).unwrap();
        let content = std::fs::read_to_string(path).unwrap();
        assert!(!content.contains("## Logs"));
    }

    #[test]
    fn export_debug_with_logs_section() {
        let dir = tempfile::tempdir().unwrap();
        let params = ExportParams {
            session_id: "s",
            session_title: "t",
            session_created_at: Utc::now(),
            token_usage: &TokenUsage::default(),
            messages: &[],
            system_prompt: None,
            model_ref: None,
            project_root: dir.path(),
            include_logs: true,
        };
        let path = export_debug(&params).unwrap();
        let content = std::fs::read_to_string(path).unwrap();
        // The "## Logs" section should always be present when include_logs is true,
        // even if no log files are found
        assert!(content.contains("## Logs"));
    }

    #[test]
    fn format_message_part_empty_text_skipped() {
        let mut out = String::new();
        write_message_part(
            &mut out,
            &MessagePart::Text {
                text: String::new(),
            },
        );
        assert!(out.is_empty());
    }

    #[test]
    fn extract_tool_summary_bash_long_truncates() {
        let long_cmd = "a".repeat(80);
        let result = extract_tool_summary(ToolName::Bash, &json!({"command": long_cmd}));
        assert!(result.ends_with("..."));
        assert!(result.chars().count() <= 40); // Truncated via extract_args_summary
    }

    #[test]
    fn extract_tool_summary_read_count() {
        assert_eq!(
            extract_tool_summary(
                ToolName::Read,
                &json!({"path": "src/main.rs", "count": true})
            ),
            "src/main.rs (count)"
        );
    }

    #[test]
    fn extract_tool_summary_read_tail() {
        assert_eq!(
            extract_tool_summary(ToolName::Read, &json!({"path": "src/main.rs", "tail": 20})),
            "src/main.rs (tail 20)"
        );
    }

    #[test]
    fn extract_tool_summary_read_multi_file() {
        let result =
            extract_tool_summary(ToolName::Read, &json!({"paths": ["a.rs", "b.rs", "c.rs"]}));
        assert_eq!(result, "3 files");
    }

    use chrono::Datelike;
}
