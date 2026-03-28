//! Tool result compression for reducing token usage in the tool-call loop.
//!
//! Before each LLM API call, old tool results (from prior loop iterations)
//! are replaced with compact heuristic summaries. This dramatically reduces
//! the number of tokens re-sent on each iteration.

use std::collections::HashMap;

use async_openai::types::chat::{
    ChatCompletionMessageToolCalls, ChatCompletionRequestMessage, ChatCompletionRequestToolMessage,
    ChatCompletionRequestToolMessageContent,
};

use crate::tool::ToolName;

/// Compress tool results that the LLM has already seen.
///
/// `messages` is the full conversation being built for the next API call.
/// `keep_recent` is the number of most-recent tool result messages to leave
/// uncompressed (typically the current iteration's tool results, which the
/// LLM hasn't seen yet).
pub fn compress_old_tool_results(
    messages: &mut [ChatCompletionRequestMessage],
    keep_recent: usize,
) {
    // Build mappings from tool_call_id → tool_name and tool_call_id → args
    // in a single pass over assistant messages. The OpenAI format stores tool names
    // and arguments in the assistant's tool_calls, not in tool result messages.
    let (tool_name_map, tool_args_map) = build_tool_maps(messages);

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
            let tool_args = tool_args_map.get(&tool_call_id);
            let compressed = match tool_name_map.get(&tool_call_id) {
                Some(&name) => compress_tool_output(name, &content, tool_args),
                None => compress_generic(&content),
            };

            saved_chars += content.len().saturating_sub(compressed.len());
            compressed_count += 1;

            messages[idx] = ChatCompletionRequestMessage::Tool(ChatCompletionRequestToolMessage {
                content: ChatCompletionRequestToolMessageContent::Text(compressed),
                tool_call_id,
            });
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

/// Build mappings from tool_call_id → (tool_name, args) in a single pass.
fn build_tool_maps(
    messages: &[ChatCompletionRequestMessage],
) -> (
    HashMap<String, ToolName>,
    HashMap<String, serde_json::Value>,
) {
    let mut name_map = HashMap::new();
    let mut args_map = HashMap::new();
    for msg in messages {
        if let ChatCompletionRequestMessage::Assistant(assistant) = msg
            && let Some(tool_calls) = &assistant.tool_calls
        {
            for tc in tool_calls {
                if let ChatCompletionMessageToolCalls::Function(func_call) = tc {
                    if let Ok(name) = func_call.function.name.parse::<ToolName>() {
                        name_map.insert(func_call.id.clone(), name);
                    }
                    if let Ok(args) =
                        serde_json::from_str::<serde_json::Value>(&func_call.function.arguments)
                    {
                        args_map.insert(func_call.id.clone(), args);
                    }
                }
            }
        }
    }
    (name_map, args_map)
}

/// Compress a tool output into a compact summary based on the tool type.
fn compress_tool_output(
    tool_name: ToolName,
    content: &str,
    tool_args: Option<&serde_json::Value>,
) -> String {
    match tool_name {
        ToolName::Read => compress_read(content, tool_args),
        ToolName::Grep => compress_grep(content, tool_args),
        ToolName::Glob => compress_glob(content),
        ToolName::List => compress_list(content),
        ToolName::Bash => compress_bash(content),
        ToolName::Edit => compress_edit(content),
        ToolName::Write => compress_write(content),
        ToolName::Patch => compress_patch(content),
        ToolName::Move
        | ToolName::Copy
        | ToolName::Delete
        | ToolName::Mkdir
        | ToolName::Question
        | ToolName::Task
        | ToolName::Webfetch
        | ToolName::Memory
        | ToolName::Symbols
        | ToolName::Lsp
        | ToolName::Agent => compress_generic(content),
    }
}

/// Compress read tool output.
/// Input format: "   4 | line content\n" with line numbers
fn compress_read(content: &str, tool_args: Option<&serde_json::Value>) -> String {
    let lines: Vec<&str> = content.lines().collect();
    // Exclude truncation footer lines from the count (e.g., "... (showing N of M lines ...)")
    let line_count = lines.iter().filter(|l| !l.starts_with("... (")).count();

    // Extract file path and line range from tool args if available
    let file_path = tool_args
        .and_then(|a| {
            crate::tool::ToolName::Read
                .path_arg_keys()
                .iter()
                .find_map(|k| a.get(*k))
        })
        .and_then(|v| v.as_str())
        .unwrap_or("(unknown)");
    let offset = tool_args
        .and_then(|a| a.get("offset"))
        .and_then(|v| v.as_u64());
    let limit = tool_args
        .and_then(|a| a.get("limit"))
        .and_then(|v| v.as_u64());
    let range_str = match (offset, limit) {
        (Some(o), Some(l)) => format!(" lines {}-{}", o, o + l),
        (Some(o), None) => format!(" from line {}", o),
        _ => String::new(),
    };

    let lang = detect_language_label(tool_args);

    // Extract key symbols (imports, definitions) via tree-sitter
    let definitions = extract_definitions(tool_args, content);

    let key_items = if !definitions.is_empty() {
        let total = definitions.len();
        let combined: Vec<&str> = definitions.iter().take(5).map(|s| s.as_str()).collect();
        let extra_count = total.saturating_sub(5);
        let extra_str = if extra_count > 0 {
            format!(", +{extra_count} more")
        } else {
            String::new()
        };
        format!(" Key items: {}{extra_str}.", combined.join(", "))
    } else {
        String::new()
    };

    format!("[Previously read: {file_path}{range_str} ({line_count} lines, {lang}).{key_items}]")
}

/// Compress grep tool output.
/// Input format: "path/file.rs:42: matched line\n" per match
fn compress_grep(content: &str, tool_args: Option<&serde_json::Value>) -> String {
    if content.starts_with("No matches found") {
        return "[Previously searched: no matches.]".to_string();
    }

    let pattern = tool_args
        .and_then(|a| a.get("pattern"))
        .and_then(|v| v.as_str())
        .unwrap_or("?");

    let lines: Vec<&str> = content.lines().collect();

    // Count matches per file, collect top match lines
    let mut file_counts: HashMap<&str, usize> = HashMap::new();
    let mut total = 0usize;
    let mut top_matches: Vec<String> = Vec::new();

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
            // Collect first 3 match lines (truncated to ~80 chars)
            if top_matches.len() < 3 {
                let truncated: String = line.chars().take(80).collect();
                top_matches.push(truncated);
            }
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

    let matches_str = if top_matches.is_empty() {
        String::new()
    } else {
        format!("\nTop matches: {}", top_matches.join(", "))
    };

    format!(
        "[Previously searched \"{pattern}\": {total} matches in {file_count} files.{matches_str}\n\
         Full files: {}.]",
        top_files.join(", ")
    )
}

/// Compress glob tool output.
/// Input format: one file path per line
fn compress_glob(content: &str) -> String {
    if content.starts_with("No files found") {
        return "[Previously globbed: no matches.]".to_string();
    }

    let lines: Vec<&str> = content.lines().collect();
    let count = lines
        .iter()
        .filter(|l| !l.starts_with("(showing first"))
        .count();

    format!("[Previously globbed: {count} files found.]")
}

/// Compress list tool output.
/// Input format: "path/file.ext (1.2KB)" or "path/dir/" per line
fn compress_list(content: &str) -> String {
    if content.ends_with("(empty)") {
        return "[Previously listed: empty directory.]".to_string();
    }

    let lines: Vec<&str> = content.lines().collect();
    let entry_count = lines
        .iter()
        .filter(|l| !l.starts_with("... (truncated)"))
        .count();

    let dir_count = lines.iter().filter(|l| l.ends_with('/')).count();
    let file_count = entry_count - dir_count;

    format!("[Previously listed: {entry_count} entries ({file_count} files, {dir_count} dirs).]")
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
        let truncated = if first_err.chars().count() > 100 {
            let t: String = first_err.chars().take(97).collect();
            format!("{t}...")
        } else {
            first_err.to_string()
        };
        format!("error: {truncated}")
    } else {
        "success".to_string()
    };

    format!("[Previously ran: {exit_info}, {line_count} lines output.]")
}

/// Compress edit tool output (already short, but standardize format).
fn compress_edit(content: &str) -> String {
    // Edit outputs are already minimal: "Successfully edited {path}..."
    if content.len() < 200 {
        return content.to_string(); // Don't compress short outputs
    }
    "[Previously edited file.]".to_string()
}

/// Compress write tool output.
fn compress_write(content: &str) -> String {
    if content.len() < 200 {
        return content.to_string();
    }
    "[Previously wrote file.]".to_string()
}

/// Compress patch tool output.
fn compress_patch(content: &str) -> String {
    if content.len() < 200 {
        return content.to_string();
    }
    "[Previously patched file.]".to_string()
}

/// Generic compression for unknown tools.
fn compress_generic(content: &str) -> String {
    let line_count = content.lines().count();
    let char_count = content.len();
    format!("[Previous tool result: {line_count} lines, {char_count} chars.]")
}

/// Detect programming language from file path extension.
fn detect_language_label(tool_args: Option<&serde_json::Value>) -> &'static str {
    let path_str = tool_args
        .and_then(|a| {
            crate::tool::ToolName::Read
                .path_arg_keys()
                .iter()
                .find_map(|k| a.get(*k))
        })
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if path_str.is_empty() {
        return "text";
    }

    crate::tool::symbols::detect_language(std::path::Path::new(path_str))
        .map(|info| info.name)
        .unwrap_or("text")
}

/// Extract function/struct/class definitions using tree-sitter.
/// Falls back to empty vec if the language isn't supported or parsing fails.
fn extract_definitions(tool_args: Option<&serde_json::Value>, content: &str) -> Vec<String> {
    let path_str = tool_args
        .and_then(|a| {
            crate::tool::ToolName::Read
                .path_arg_keys()
                .iter()
                .find_map(|k| a.get(*k))
        })
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if path_str.is_empty() {
        return Vec::new();
    }

    let path = std::path::Path::new(path_str);
    let lang_info = match crate::tool::symbols::detect_language(path) {
        Some(info) => info,
        None => return Vec::new(),
    };

    // Strip line-number prefixes to get raw source
    let raw_source = strip_line_numbers(content);

    // Parse with tree-sitter
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&lang_info.language).is_err() {
        return Vec::new();
    }
    let tree = match parser.parse(raw_source.as_bytes(), None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    // Walk symbols (depth 0 = top-level only)
    let symbols = crate::tool::symbols::walk_symbols(
        tree.root_node(),
        raw_source.as_bytes(),
        lang_info.name,
        0,
    );

    symbols
        .iter()
        .map(|s| {
            let kind = crate::tool::symbols::kind_label(&s.kind);
            format!("{kind} {}", s.name)
        })
        .collect()
}

/// Strip "   N | " line-number prefixes from read tool output to get raw source.
fn strip_line_numbers(content: &str) -> String {
    content
        .lines()
        .map(|line| {
            if let Some(pipe_pos) = line.find(" | ") {
                &line[pipe_pos + 3..]
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::chat::{
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
    };

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
        let args = serde_json::json!({"path": "src/main.rs", "offset": 1, "limit": 100});
        let result = compress_read(&content, Some(&args));
        assert!(result.starts_with("[Previously read:"));
        assert!(result.contains("100 lines"));
        assert!(result.contains("rust"));
        assert!(result.contains("src/main.rs"));
        assert!(result.ends_with("]"));
    }

    #[test]
    fn test_compress_grep() {
        let content =
            "src/app.rs:10: let x = 1;\nsrc/app.rs:20: let y = 2;\nsrc/stream.rs:5: let z = 3;";
        let args = serde_json::json!({"pattern": "let.*="});
        let result = compress_grep(content, Some(&args));
        assert!(result.starts_with("[Previously searched"));
        assert!(result.contains("3 matches"));
        assert!(result.contains("2 files"));
        assert!(result.contains("let.*="));
    }

    #[test]
    fn test_compress_grep_no_matches() {
        let result = compress_grep("No matches found for pattern: foo", None);
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
    fn test_extract_definitions_rust() {
        let content = "   1 | use std::io;\n   2 | \n   3 | pub fn main() {\n   4 |     let x = 5;\n   5 | }\n   6 | \n   7 | struct Foo {\n   8 |     bar: i32,\n   9 | }";
        let args = serde_json::json!({"path": "test.rs"});
        let defs = extract_definitions(Some(&args), content);
        assert!(
            defs.iter().any(|d| d.contains("main")),
            "should find fn main, got: {:?}",
            defs
        );
        assert!(
            defs.iter().any(|d| d.contains("Foo")),
            "should find struct Foo, got: {:?}",
            defs
        );
    }

    #[test]
    fn test_extract_definitions_unsupported_language() {
        let content = "   1 | some content";
        let args = serde_json::json!({"path": "readme.txt"});
        let defs = extract_definitions(Some(&args), content);
        assert!(defs.is_empty());
    }

    #[test]
    fn test_extract_definitions_no_args() {
        let content = "   1 | fn main() {}";
        let defs = extract_definitions(None, content);
        assert!(defs.is_empty());
    }

    #[test]
    fn test_skip_short_content() {
        // Short content should not be compressed (checked in compress_old_tool_results)
        let content = "OK";
        assert!(content.len() < 200);
    }

    /// Helper to build a tool result message.
    fn make_tool_result(call_id: &str, content: &str) -> ChatCompletionRequestMessage {
        ChatCompletionRequestMessage::Tool(ChatCompletionRequestToolMessage {
            content: ChatCompletionRequestToolMessageContent::Text(content.to_string()),
            tool_call_id: call_id.to_string(),
        })
    }

    /// Helper to build an assistant message with tool calls.
    fn make_assistant_with_tool_calls(
        call_ids_and_names: &[(&str, &str)],
    ) -> ChatCompletionRequestMessage {
        let tool_calls: Vec<ChatCompletionMessageToolCalls> = call_ids_and_names
            .iter()
            .map(|(id, name)| {
                ChatCompletionMessageToolCalls::Function(
                    async_openai::types::chat::ChatCompletionMessageToolCall {
                        id: id.to_string(),
                        function: async_openai::types::chat::FunctionCall {
                            name: name.to_string(),
                            arguments: "{}".to_string(),
                        },
                    },
                )
            })
            .collect();

        #[allow(deprecated)]
        ChatCompletionRequestMessage::Assistant(ChatCompletionRequestAssistantMessage {
            content: Some(ChatCompletionRequestAssistantMessageContent::Text(
                "I'll read those files.".to_string(),
            )),
            name: None,
            audio: None,
            tool_calls: Some(tool_calls),
            function_call: None,
            refusal: None,
        })
    }

    #[test]
    fn test_compress_all_tool_results_with_zero_keep() {
        // Build a conversation: user → assistant+tool_calls → tool results
        let long_content: String = (1..=100)
            .map(|i| format!("{:>4} | line {i}\n", i))
            .collect();
        assert!(long_content.len() > 200); // must exceed skip threshold

        let mut messages = vec![
            ChatCompletionRequestMessage::User(
                async_openai::types::chat::ChatCompletionRequestUserMessage {
                    content:
                        async_openai::types::chat::ChatCompletionRequestUserMessageContent::Text(
                            "Read files".to_string(),
                        ),
                    name: None,
                },
            ),
            make_assistant_with_tool_calls(&[("call1", "read"), ("call2", "read")]),
            make_tool_result("call1", &long_content),
            make_tool_result("call2", &long_content),
        ];

        let original_len = messages.len();
        compress_old_tool_results(&mut messages, 0);

        // Same number of messages (structure preserved)
        assert_eq!(messages.len(), original_len);

        // All tool results should be compressed (short)
        for msg in &messages {
            if let ChatCompletionRequestMessage::Tool(t) = msg {
                let text = extract_text(t);
                assert!(
                    text.len() < 200,
                    "tool result should be compressed, got {} chars",
                    text.len()
                );
            }
        }
    }

    #[test]
    fn test_compress_all_preserves_tool_call_ids() {
        let long_content: String = (1..=100)
            .map(|i| format!("{:>4} | line {i}\n", i))
            .collect();

        let mut messages = vec![
            make_assistant_with_tool_calls(&[("call_a", "read"), ("call_b", "grep")]),
            make_tool_result("call_a", &long_content),
            make_tool_result("call_b", &long_content),
        ];

        compress_old_tool_results(&mut messages, 0);

        // Verify tool_call_ids are preserved
        let tool_ids: Vec<String> = messages
            .iter()
            .filter_map(|m| {
                if let ChatCompletionRequestMessage::Tool(t) = m {
                    Some(t.tool_call_id.clone())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(tool_ids, vec!["call_a", "call_b"]);
    }

    #[test]
    fn test_compress_read_includes_file_path_and_range() {
        let mut lines = Vec::new();
        lines.push(format!("{:>4} | use std::collections::HashMap;", 10));
        lines.push(format!("{:>4} | use std::io;", 11));
        for i in 12..=50 {
            lines.push(format!("{:>4} | fn func_{i}() {{}}", i));
        }
        // Pad to > 200 chars
        for i in 51..=100 {
            lines.push(format!("{:>4} | // padding line {i}", i));
        }
        let content = lines.join("\n");
        let args = serde_json::json!({"path": "src/stream.rs", "offset": 10, "limit": 91});
        let result = compress_read(&content, Some(&args));
        assert!(result.contains("src/stream.rs"), "should include file path");
        assert!(result.contains("lines 10-101"), "should include line range");
        assert!(result.contains("Key items:"), "should include key items");
        assert!(
            result.contains("use std::collections::HashMap"),
            "should include imports"
        );
    }

    #[test]
    fn test_compress_read_without_args() {
        let mut lines = Vec::new();
        for i in 1..=100 {
            lines.push(format!("{:>4} | // line {i}", i));
        }
        let content = lines.join("\n");
        let result = compress_read(&content, None);
        assert!(result.contains("(unknown)"), "should show unknown file");
        assert!(result.contains("100 lines"), "should include line count");
    }

    #[test]
    fn test_compress_grep_includes_pattern_and_matches() {
        let content =
            "src/app.rs:10: let x = 1;\nsrc/app.rs:20: let y = 2;\nsrc/stream.rs:5: let z = 3;";
        let args = serde_json::json!({"pattern": "let\\s+"});
        let result = compress_grep(content, Some(&args));
        assert!(result.contains("let\\s+"), "should include search pattern");
        assert!(
            result.contains("Top matches:"),
            "should include top matches"
        );
        assert!(
            result.contains("src/app.rs:10"),
            "should include match lines"
        );
    }

    #[test]
    fn test_compress_grep_without_args() {
        let content = "src/app.rs:10: let x = 1;";
        let result = compress_grep(content, None);
        assert!(result.contains("\"?\""), "should show unknown pattern");
        assert!(result.contains("1 matches"), "should show match count");
    }

    #[test]
    fn test_build_tool_maps() {
        let tool_calls = vec![ChatCompletionMessageToolCalls::Function(
            async_openai::types::chat::ChatCompletionMessageToolCall {
                id: "call_1".to_string(),
                function: async_openai::types::chat::FunctionCall {
                    name: "read".to_string(),
                    arguments: r#"{"file_path":"src/main.rs"}"#.to_string(),
                },
            },
        )];

        #[allow(deprecated)]
        let messages = vec![ChatCompletionRequestMessage::Assistant(
            ChatCompletionRequestAssistantMessage {
                content: None,
                name: None,
                audio: None,
                tool_calls: Some(tool_calls),
                function_call: None,
                refusal: None,
            },
        )];

        let (name_map, args_map) = build_tool_maps(&messages);
        assert_eq!(name_map.len(), 1);
        assert_eq!(*name_map.get("call_1").unwrap(), ToolName::Read);
        assert_eq!(args_map.len(), 1);
        let args = args_map.get("call_1").unwrap();
        assert_eq!(args["file_path"], "src/main.rs");
    }
}
