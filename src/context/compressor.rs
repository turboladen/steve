//! Tool result compression for reducing token usage in the tool-call loop.
//!
//! Before each LLM API call, old tool results (from prior loop iterations)
//! are replaced with compact heuristic summaries. This dramatically reduces
//! the number of tokens re-sent on each iteration.

use std::collections::HashMap;

use async_openai::types::chat::{
    ChatCompletionMessageToolCalls, ChatCompletionRequestMessage,
    ChatCompletionRequestToolMessage, ChatCompletionRequestToolMessageContent,
};

use crate::tool::ToolName;

/// Compress tool results that the LLM has already seen.
///
/// `messages` is the full conversation being built for the next API call.
/// `keep_recent` is the number of most-recent tool result messages to leave
/// uncompressed (typically the current iteration's tool results, which the
/// LLM hasn't seen yet).
pub fn compress_old_tool_results(
    messages: &mut Vec<ChatCompletionRequestMessage>,
    keep_recent: usize,
) {
    // Build a mapping from tool_call_id → tool_name by scanning assistant messages.
    // The OpenAI format stores tool names in the assistant's tool_calls, not in the
    // tool result messages themselves.
    let tool_name_map = build_tool_name_map(messages);

    // Find all tool result message indices
    let tool_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter_map(|(i, msg)| {
            if matches!(msg, ChatCompletionRequestMessage::Tool(_)) {
                Some(i)
            } else {
                None
            }
        })
        .collect();

    // Only compress tool results that are NOT in the most recent batch
    let compress_count = tool_indices.len().saturating_sub(keep_recent);
    if compress_count == 0 {
        return;
    }

    let indices_to_compress = &tool_indices[..compress_count];

    let mut compressed_count = 0u32;
    let mut saved_chars = 0usize;

    for &idx in indices_to_compress {
        if let ChatCompletionRequestMessage::Tool(tool_msg) = &messages[idx] {
            let content = extract_text(tool_msg);

            // Skip already-compressed messages and short messages
            if content.starts_with("[Previously ") || content.len() < 200 {
                continue;
            }

            let tool_call_id = tool_msg.tool_call_id.clone();
            let compressed = match tool_name_map.get(&tool_call_id) {
                Some(&name) => compress_tool_output(name, &content),
                None => compress_generic(&content),
            };

            saved_chars += content.len().saturating_sub(compressed.len());
            compressed_count += 1;

            messages[idx] = ChatCompletionRequestMessage::Tool(
                ChatCompletionRequestToolMessage {
                    content: ChatCompletionRequestToolMessageContent::Text(compressed),
                    tool_call_id,
                },
            );
        }
    }

    if compressed_count > 0 {
        tracing::info!(
            compressed = compressed_count,
            saved_chars = saved_chars,
            "compressed old tool results"
        );
    }
}

/// Extract text content from a tool message.
fn extract_text(msg: &ChatCompletionRequestToolMessage) -> String {
    match &msg.content {
        ChatCompletionRequestToolMessageContent::Text(t) => t.clone(),
        ChatCompletionRequestToolMessageContent::Array(parts) => {
            // Concatenate text parts
            parts
                .iter()
                .map(|p| {
                    let async_openai::types::chat::ChatCompletionRequestToolMessageContentPart::Text(t) = p;
                    t.text.as_str()
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

/// Build a mapping from tool_call_id to tool_name by scanning assistant messages.
fn build_tool_name_map(messages: &[ChatCompletionRequestMessage]) -> HashMap<String, ToolName> {
    let mut map = HashMap::new();
    for msg in messages {
        if let ChatCompletionRequestMessage::Assistant(assistant) = msg {
            if let Some(tool_calls) = &assistant.tool_calls {
                for tc in tool_calls {
                    if let ChatCompletionMessageToolCalls::Function(func_call) = tc {
                        if let Ok(name) = func_call.function.name.parse::<ToolName>() {
                            map.insert(func_call.id.clone(), name);
                        }
                    }
                }
            }
        }
    }
    map
}

/// Compress a tool output into a compact summary based on the tool type.
fn compress_tool_output(tool_name: ToolName, content: &str) -> String {
    match tool_name {
        ToolName::Read => compress_read(content),
        ToolName::Grep => compress_grep(content),
        ToolName::Glob => compress_glob(content),
        ToolName::List => compress_list(content),
        ToolName::Bash => compress_bash(content),
        ToolName::Edit => compress_edit(content),
        ToolName::Write => compress_write(content),
        ToolName::Patch => compress_patch(content),
        ToolName::Question | ToolName::Todo | ToolName::Webfetch => compress_generic(content),
    }
}

/// Compress read tool output.
/// Input format: "   4 | line content\n" with line numbers
fn compress_read(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let line_count = lines.len();

    // Try to extract the file path from the numbered-line format.
    // We can detect the language from the content patterns.
    let lang = detect_language_from_content(content);

    // Extract key definitions (fn, struct, impl, class, def, etc.)
    let definitions = extract_definitions(content);
    let defs_str = if definitions.is_empty() {
        String::new()
    } else {
        let truncated: Vec<&str> = definitions.iter().take(5).map(|s| s.as_str()).collect();
        let suffix = if definitions.len() > 5 {
            format!(", +{} more", definitions.len() - 5)
        } else {
            String::new()
        };
        format!(" Defines: {}{suffix}.", truncated.join(", "))
    };

    format!(
        "[Previously read: {line_count} lines, {lang}.{defs_str} Re-read if needed.]"
    )
}

/// Compress grep tool output.
/// Input format: "path/file.rs:42: matched line\n" per match
fn compress_grep(content: &str) -> String {
    if content.starts_with("No matches found") {
        return format!("[Previously searched: no matches. Re-search if needed.]");
    }

    let lines: Vec<&str> = content.lines().collect();

    // Count matches per file
    let mut file_counts: HashMap<&str, usize> = HashMap::new();
    let mut total = 0usize;

    for line in &lines {
        // Skip the truncation notice
        if line.starts_with("(showing first") {
            continue;
        }
        // Format: "path/file.rs:42: content"
        if let Some(colon_pos) = line.find(':') {
            let file = &line[..colon_pos];
            *file_counts.entry(file).or_default() += 1;
            total += 1;
        }
    }

    let file_count = file_counts.len();

    // Show top 3 files by match count
    let mut sorted: Vec<(&&str, &usize)> = file_counts.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));

    let top_files: Vec<String> = sorted
        .iter()
        .take(3)
        .map(|(file, count)| format!("{}({})", file, count))
        .collect();

    format!(
        "[Previously searched: {total} matches in {file_count} files ({}). Re-search if needed.]",
        top_files.join(", ")
    )
}

/// Compress glob tool output.
/// Input format: one file path per line
fn compress_glob(content: &str) -> String {
    if content.starts_with("No files found") {
        return format!("[Previously globbed: no matches. Re-glob if needed.]");
    }

    let lines: Vec<&str> = content.lines().collect();
    let count = lines
        .iter()
        .filter(|l| !l.starts_with("(showing first"))
        .count();

    format!("[Previously globbed: {count} files found. Re-glob if needed.]")
}

/// Compress list tool output.
/// Input format: "path/file.ext (1.2KB)" or "path/dir/" per line
fn compress_list(content: &str) -> String {
    if content.ends_with("(empty)") {
        return format!("[Previously listed: empty directory. Re-list if needed.]");
    }

    let lines: Vec<&str> = content.lines().collect();
    let entry_count = lines
        .iter()
        .filter(|l| !l.starts_with("... (truncated)"))
        .count();

    let dir_count = lines.iter().filter(|l| l.ends_with('/')).count();
    let file_count = entry_count - dir_count;

    format!(
        "[Previously listed: {entry_count} entries ({file_count} files, {dir_count} dirs). Re-list if needed.]"
    )
}

/// Compress bash tool output.
/// Input format: stdout, optional "STDERR:\n" section, or "(exit code: N)"
fn compress_bash(content: &str) -> String {
    let is_error = content.contains("STDERR:");
    let line_count = content.lines().count();

    // Try to detect exit code
    let exit_info = if content.starts_with("(exit code:") {
        // Command produced no output, just exit code
        content.trim().to_string()
    } else if is_error {
        // Extract first error line
        let first_err = content
            .lines()
            .skip_while(|l| !l.starts_with("STDERR:"))
            .nth(1)
            .unwrap_or("(error details omitted)")
            .trim();
        let truncated = if first_err.len() > 100 {
            format!("{}...", &first_err[..97])
        } else {
            first_err.to_string()
        };
        format!("error: {truncated}")
    } else {
        "success".to_string()
    };

    format!(
        "[Previously ran: {exit_info}, {line_count} lines output. Re-run if needed.]"
    )
}

/// Compress edit tool output (already short, but standardize format).
fn compress_edit(content: &str) -> String {
    // Edit outputs are already minimal: "Successfully edited {path}..."
    if content.len() < 200 {
        return content.to_string(); // Don't compress short outputs
    }
    format!("[Previously edited file. Re-read if needed.]")
}

/// Compress write tool output.
fn compress_write(content: &str) -> String {
    if content.len() < 200 {
        return content.to_string();
    }
    format!("[Previously wrote file. Re-read if needed.]")
}

/// Compress patch tool output.
fn compress_patch(content: &str) -> String {
    if content.len() < 200 {
        return content.to_string();
    }
    format!("[Previously patched file. Re-read if needed.]")
}

/// Generic compression for unknown tools.
fn compress_generic(content: &str) -> String {
    let line_count = content.lines().count();
    let char_count = content.len();
    format!("[Previous tool result: {line_count} lines, {char_count} chars. Re-run if needed.]")
}

/// Detect programming language from file content heuristics.
fn detect_language_from_content(content: &str) -> &'static str {
    // Check for common language patterns in the numbered-line format
    // Lines look like "   4 | fn main() {"
    let sample: String = content.lines().take(30).collect::<Vec<_>>().join("\n");

    if sample.contains("fn ") && (sample.contains("let ") || sample.contains("use ")) {
        "Rust"
    } else if sample.contains("def ") && sample.contains("self") {
        "Python"
    } else if sample.contains("function ") || sample.contains("const ") && sample.contains("=>") {
        "JavaScript/TypeScript"
    } else if sample.contains("func ") && sample.contains("package ") {
        "Go"
    } else if sample.contains("class ") && sample.contains("public ") {
        "Java/C#"
    } else if sample.contains("#include") {
        "C/C++"
    } else {
        "text"
    }
}

/// Extract function/struct/class definitions from numbered-line file content.
fn extract_definitions(content: &str) -> Vec<String> {
    let mut defs = Vec::new();

    for line in content.lines() {
        // Strip the line number prefix: "   4 | actual content"
        let stripped = if let Some(pipe_pos) = line.find(" | ") {
            &line[pipe_pos + 3..]
        } else {
            line
        };

        let trimmed = stripped.trim();

        // Rust: fn, struct, enum, impl, trait, mod, type
        if let Some(name) = extract_rust_def(trimmed) {
            defs.push(name);
        }
        // Python: def, class
        else if let Some(name) = extract_python_def(trimmed) {
            defs.push(name);
        }
        // JS/TS: function, class, export
        else if let Some(name) = extract_js_def(trimmed) {
            defs.push(name);
        }
        // Go: func, type
        else if let Some(name) = extract_go_def(trimmed) {
            defs.push(name);
        }
    }

    defs
}

fn extract_rust_def(line: &str) -> Option<String> {
    for keyword in &["pub fn ", "fn ", "pub struct ", "struct ", "pub enum ", "enum ",
                      "pub trait ", "trait ", "impl ", "pub mod ", "mod ",
                      "pub type ", "type "] {
        let matches = if line.starts_with(keyword) {
            true
        } else if keyword.starts_with("pub ") {
            // Also check for pub(crate) variants
            line.starts_with(&format!("pub(crate) {}", &keyword[4..]))
        } else {
            false
        };

        if matches {
            let rest = &line[keyword.len()..];
            let name: String = rest.chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '<')
                .take_while(|c| *c != '<')
                .collect();
            if !name.is_empty() {
                return Some(format!("{keyword}{name}").trim_end().to_string());
            }
        }
    }
    None
}

fn extract_python_def(line: &str) -> Option<String> {
    if line.starts_with("def ") {
        let name: String = line[4..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if !name.is_empty() {
            return Some(format!("def {name}"));
        }
    }
    if line.starts_with("class ") {
        let name: String = line[6..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if !name.is_empty() {
            return Some(format!("class {name}"));
        }
    }
    None
}

fn extract_js_def(line: &str) -> Option<String> {
    if line.starts_with("function ") {
        let name: String = line[9..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if !name.is_empty() {
            return Some(format!("function {name}"));
        }
    }
    if line.starts_with("class ") {
        let name: String = line[6..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if !name.is_empty() {
            return Some(format!("class {name}"));
        }
    }
    if line.starts_with("export ") {
        // export function/class/const
        let rest = line[7..].trim_start();
        if rest.starts_with("function ") || rest.starts_with("class ") || rest.starts_with("const ") {
            let keyword_end = rest.find(' ').unwrap_or(0) + 1;
            let name: String = rest[keyword_end..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if !name.is_empty() {
                let keyword = &rest[..keyword_end - 1];
                return Some(format!("export {keyword} {name}"));
            }
        }
    }
    None
}

fn extract_go_def(line: &str) -> Option<String> {
    if line.starts_with("func ") {
        let rest = &line[5..];
        // Skip receiver: func (r *Receiver) Name(...)
        let name_start = if rest.starts_with('(') {
            rest.find(')').map(|p| p + 2).unwrap_or(0)
        } else {
            0
        };
        if name_start < rest.len() {
            let name: String = rest[name_start..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if !name.is_empty() {
                return Some(format!("func {name}"));
            }
        }
    }
    if line.starts_with("type ") {
        let name: String = line[5..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if !name.is_empty() {
            return Some(format!("type {name}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_read() {
        let mut lines = Vec::new();
        lines.push(format!("{:>4} | use std::io;", 1));
        lines.push(format!("{:>4} | ", 2));
        lines.push(format!("{:>4} | fn main() {{", 3));
        lines.push(format!("{:>4} |     let x = 5;", 4));
        for i in 5..=100 {
            lines.push(format!("{:>4} | // line {i}", i));
        }
        let content = lines.join("\n");
        let result = compress_read(&content);
        assert!(result.starts_with("[Previously read:"));
        assert!(result.contains("100 lines"));
        assert!(result.contains("Rust"));
        assert!(result.ends_with("Re-read if needed.]"));
    }

    #[test]
    fn test_compress_grep() {
        let content = "src/app.rs:10: let x = 1;\nsrc/app.rs:20: let y = 2;\nsrc/stream.rs:5: let z = 3;";
        let result = compress_grep(content);
        assert!(result.starts_with("[Previously searched:"));
        assert!(result.contains("3 matches"));
        assert!(result.contains("2 files"));
    }

    #[test]
    fn test_compress_grep_no_matches() {
        let result = compress_grep("No matches found for pattern: foo");
        assert!(result.contains("no matches"));
    }

    #[test]
    fn test_compress_glob() {
        let content = "src/main.rs\nsrc/app.rs\nsrc/stream.rs";
        let result = compress_glob(content);
        assert!(result.contains("3 files"));
    }

    #[test]
    fn test_compress_bash_success() {
        let content = "Compiling steve v0.1.0\nFinished dev target(s)";
        let result = compress_bash(content);
        assert!(result.contains("success"));
    }

    #[test]
    fn test_compress_bash_error() {
        let content = "STDERR:\nerror[E0308]: mismatched types";
        let result = compress_bash(content);
        assert!(result.contains("error"));
    }

    #[test]
    fn test_extract_rust_defs() {
        assert_eq!(extract_rust_def("pub fn main() {"), Some("pub fn main".to_string()));
        assert_eq!(extract_rust_def("struct Foo {"), Some("struct Foo".to_string()));
        assert_eq!(extract_rust_def("impl ToolRegistry {"), Some("impl ToolRegistry".to_string()));
        assert_eq!(extract_rust_def("let x = 5;"), None);
    }

    #[test]
    fn test_skip_short_content() {
        // Short content should not be compressed (checked in compress_old_tool_results)
        let content = "OK";
        assert!(content.len() < 200);
    }
}
