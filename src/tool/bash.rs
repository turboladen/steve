//! Bash tool — executes shell commands.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use wait_timeout::ChildExt;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

pub fn tool() -> ToolEntry {
    let def_json = definition();
    let func = def_json.get("function").unwrap();
    ToolEntry {
        def: ToolDef {
            name: ToolName::Bash,
            description: func.get("description").unwrap().as_str().unwrap().to_string(),
            parameters: func.get("parameters").cloned().unwrap(),
        },
        handler: Box::new(execute),
    }
}

pub fn definition() -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "bash",
            "description": "Execute a bash command in the project directory. Use ONLY for builds, tests, git commands, or operations with no dedicated tool. Commands duplicating native tools (cat, ls, grep, sed, etc.) are REJECTED — use read, list, glob, grep, edit, write, patch instead. The command runs with the project root as the working directory.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The bash command to execute."
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Optional timeout in seconds (default: 30)."
                    }
                },
                "required": ["command"]
            }
        }
    })
}

/// Check if a bash command is a simple substitute for a native tool.
/// Only intercepts standalone commands (no pipes, chains, or subshells).
/// Returns Some(redirect_message) if rejected, None if allowed.
fn check_native_tool_redirect(command: &str) -> Option<String> {
    let trimmed = command.trim();

    // Allow any compound command — pipes, chains, subshells mean the user
    // is doing something beyond what a single native tool provides.
    if trimmed.contains('|')
        || trimmed.contains("&&")
        || trimmed.contains("||")
        || trimmed.contains(';')
        || trimmed.contains('`')
        || trimmed.contains("$(")
    {
        return None;
    }

    // Extract base command, stripping sudo/env/path prefixes
    let mut words = trimmed.split_whitespace();
    let mut base = words.next().unwrap_or("");
    while matches!(base, "sudo" | "env" | "nice" | "nohup" | "time") {
        base = words.next().unwrap_or("");
    }
    let base = base.rsplit('/').next().unwrap_or(base);

    match base {
        "cat" | "head" | "tail" | "less" | "more" => Some(
            "Use the `read` tool instead. It supports `offset`/`limit` for ranges, `tail` for last N lines, `count` for line counts, and `paths` for multiple files.".into(),
        ),
        "wc" => {
            // Only redirect wc -l (line count) or bare wc — not wc -w, wc -c, etc.
            let rest: Vec<&str> = words.collect();
            let has_non_l_flags = rest.iter().any(|w| {
                w.starts_with('-') && *w != "-l"
            });
            if has_non_l_flags {
                None
            } else {
                Some("Use the `read` tool with `count: true` to get file line counts.".into())
            }
        }
        "ls" | "dir" => Some("Use the `list` tool instead.".into()),
        "find" => Some(
            "Use the `glob` tool instead. It supports patterns like `**/*.rs`.".into(),
        ),
        "grep" | "rg" | "ag" | "ack" => {
            Some("Use the `grep` tool instead. It supports regex and is cached.".into())
        }
        "sed" | "awk" => Some("Use the `edit` or `patch` tool instead.".into()),
        _ => None,
    }
}

pub fn execute(args: Value, ctx: ToolContext) -> Result<ToolOutput> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .context("missing 'command' parameter")?;

    // Reject simple commands that duplicate native tools
    if let Some(redirect) = check_native_tool_redirect(command) {
        let title = if command.chars().count() > 60 {
            let truncated: String = command.chars().take(57).collect();
            format!("$ {truncated}...")
        } else {
            format!("$ {command}")
        };
        return Ok(ToolOutput {
            title,
            output: redirect,
            is_error: true,
        });
    }

    let timeout_secs = args
        .get("timeout")
        .and_then(|v| v.as_u64())
        .unwrap_or(30);

    let result = run_command(command, &ctx.project_root, timeout_secs)?;

    let title = if command.chars().count() > 60 {
        let truncated: String = command.chars().take(57).collect();
        format!("$ {truncated}...")
    } else {
        format!("$ {command}")
    };

    Ok(ToolOutput {
        title,
        output: result.output,
        is_error: !result.success,
    })
}

#[derive(Debug)]
struct CommandResult {
    output: String,
    success: bool,
}

fn run_command(command: &str, cwd: &Path, timeout_secs: u64) -> Result<CommandResult> {
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn bash command")?;

    let timeout = Duration::from_secs(timeout_secs);
    let status = match child.wait_timeout(timeout)? {
        Some(status) => status,
        None => {
            // Timed out — kill the child and reap it
            let _ = child.kill();
            let _ = child.wait();
            bail!("command timed out after {timeout_secs}s");
        }
    };

    let stdout = child.stdout.take().map(|mut s| {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut s, &mut buf).unwrap_or(0);
        String::from_utf8_lossy(&buf).into_owned()
    }).unwrap_or_default();

    let stderr = child.stderr.take().map(|mut s| {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut s, &mut buf).unwrap_or(0);
        String::from_utf8_lossy(&buf).into_owned()
    }).unwrap_or_default();

    let mut result = String::new();

    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("STDERR:\n");
        result.push_str(&stderr);
    }

    if result.is_empty() {
        result = format!("(exit code: {})", status.code().unwrap_or(-1));
    }

    // Truncate very long output — keep head + tail for useful context
    let max_len = 20_000;
    if result.len() > max_len {
        let total_len = result.len();
        // Find safe char boundaries first to avoid panicking on multi-byte UTF-8
        let safe_head_boundary = {
            let mut end = 15_000.min(total_len);
            while end > 0 && !result.is_char_boundary(end) {
                end -= 1;
            }
            end
        };
        let safe_tail_boundary = {
            let mut start = total_len.saturating_sub(4_000);
            while start < total_len && !result.is_char_boundary(start) {
                start += 1;
            }
            start
        };
        // Find newline boundaries within the safe ranges
        let head_end = result[..safe_head_boundary]
            .rfind('\n')
            .unwrap_or(safe_head_boundary);
        let tail_start = result[safe_tail_boundary..]
            .find('\n')
            .map(|i| safe_tail_boundary + i + 1)
            .unwrap_or(safe_tail_boundary);
        let head = &result[..head_end];
        let tail = &result[tail_start..];
        result = format!(
            "{head}\n\n... (middle omitted — showing first/last of {total_len} bytes) ...\n\n{tail}"
        );
    }

    Ok(CommandResult {
        output: result,
        success: status.success(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_command_captures_stdout() {
        let result = run_command("echo hello", Path::new("/tmp"), 30).unwrap();
        assert!(result.success);
        assert_eq!(result.output.trim(), "hello");
    }

    #[test]
    fn run_command_captures_stderr() {
        let result = run_command("echo oops >&2", Path::new("/tmp"), 30).unwrap();
        assert!(result.success);
        assert!(result.output.contains("STDERR:"));
        assert!(result.output.contains("oops"));
    }

    #[test]
    fn run_command_reports_failure() {
        let result = run_command("exit 1", Path::new("/tmp"), 30).unwrap();
        assert!(!result.success);
    }

    #[test]
    fn run_command_timeout_kills_long_running() {
        let err = run_command("sleep 60", Path::new("/tmp"), 1).unwrap_err();
        assert!(
            err.to_string().contains("timed out"),
            "expected timeout error, got: {err}"
        );
    }

    #[test]
    fn run_command_long_output_head_tail() {
        // Generate output > 20k bytes
        let result = run_command("seq 1 10000", Path::new("/tmp"), 30).unwrap();
        // Should have head content (first line)
        assert!(result.output.starts_with("1\n"));
        // Should have tail content (last value)
        assert!(result.output.contains("10000"));
        // Should have omission marker
        assert!(result.output.contains("middle omitted"));
        // Middle content should actually be omitted
        assert!(
            !result.output.contains("\n5000\n"),
            "middle should be omitted"
        );
    }

    #[test]
    fn execute_with_timeout_param() {
        let args = serde_json::json!({
            "command": "sleep 60",
            "timeout": 1
        });
        let ctx = ToolContext { project_root: Path::new("/tmp").to_path_buf(), storage_dir: None, task_store: None, lsp_manager: None };
        let err = execute(args, ctx).unwrap_err();
        assert!(err.to_string().contains("timed out"));
    }

    // -- check_native_tool_redirect tests --

    #[test]
    fn redirect_cat_to_read() {
        let msg = check_native_tool_redirect("cat src/main.rs");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("`read`"));
    }

    #[test]
    fn redirect_ls_to_list() {
        let msg = check_native_tool_redirect("ls -la src/");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("`list`"));
    }

    #[test]
    fn redirect_grep_to_grep_tool() {
        let msg = check_native_tool_redirect("grep -r 'pattern' src/");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("`grep`"));
    }

    #[test]
    fn redirect_sudo_cat() {
        let msg = check_native_tool_redirect("sudo cat /etc/hosts");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("`read`"));
    }

    #[test]
    fn redirect_absolute_path_cat() {
        let msg = check_native_tool_redirect("/usr/bin/cat foo.txt");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("`read`"));
    }

    #[test]
    fn redirect_find_to_glob() {
        let msg = check_native_tool_redirect("find . -name '*.rs'");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("`glob`"));
    }

    #[test]
    fn redirect_sed_to_edit() {
        let msg = check_native_tool_redirect("sed -i 's/foo/bar/' file.txt");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("`edit`"));
    }

    #[test]
    fn redirect_rg_to_grep_tool() {
        let msg = check_native_tool_redirect("rg 'pattern' src/");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("`grep`"));
    }

    #[test]
    fn redirect_head_to_read() {
        let msg = check_native_tool_redirect("head -20 src/main.rs");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("`read`"));
    }

    #[test]
    fn redirect_wc_l_to_read_count() {
        let msg = check_native_tool_redirect("wc -l src/main.rs");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("`read`"));
    }

    #[test]
    fn redirect_bare_wc_to_read_count() {
        let msg = check_native_tool_redirect("wc src/main.rs");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("`read`"));
    }

    #[test]
    fn allow_wc_w_and_wc_c() {
        assert!(check_native_tool_redirect("wc -w src/main.rs").is_none());
        assert!(check_native_tool_redirect("wc -c src/main.rs").is_none());
        assert!(check_native_tool_redirect("wc -m src/main.rs").is_none());
    }

    #[test]
    fn allow_pipe_commands() {
        assert!(check_native_tool_redirect("cat foo.rs | wc -l").is_none());
        assert!(check_native_tool_redirect("grep TODO src/ | sort").is_none());
        assert!(check_native_tool_redirect("cargo test 2>&1 | head -20").is_none());
    }

    #[test]
    fn allow_chain_commands() {
        assert!(check_native_tool_redirect("ls && echo done").is_none());
    }

    #[test]
    fn allow_subshell_commands() {
        assert!(check_native_tool_redirect("echo $(cat foo)").is_none());
    }

    #[test]
    fn allow_empty_command() {
        assert!(check_native_tool_redirect("").is_none());
        assert!(check_native_tool_redirect("   ").is_none());
    }

    #[test]
    fn allow_legitimate_bash_commands() {
        assert!(check_native_tool_redirect("cargo build").is_none());
        assert!(check_native_tool_redirect("git status").is_none());
        assert!(check_native_tool_redirect("npm install").is_none());
        assert!(check_native_tool_redirect("python script.py").is_none());
    }

    #[test]
    fn execute_rejects_simple_cat() {
        let args = serde_json::json!({ "command": "cat src/main.rs" });
        let ctx = ToolContext { project_root: Path::new("/tmp").to_path_buf(), storage_dir: None, task_store: None, lsp_manager: None };
        let output = execute(args, ctx).unwrap();
        assert!(output.is_error);
        assert!(output.output.contains("`read`"));
    }
}
