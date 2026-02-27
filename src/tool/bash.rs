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
            "description": "Execute a bash command in the project directory. Use this to run builds, tests, git commands, or other shell operations. The command runs with the project root as the working directory.",
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

pub fn execute(args: Value, ctx: ToolContext) -> Result<ToolOutput> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .context("missing 'command' parameter")?;

    let timeout_secs = args
        .get("timeout")
        .and_then(|v| v.as_u64())
        .unwrap_or(30);

    let result = run_command(command, &ctx.project_root, timeout_secs)?;

    let title = if command.len() > 60 {
        format!("$ {}...", &command[..57])
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

    // Truncate very long output
    let max_len = 50_000;
    if result.len() > max_len {
        result.truncate(max_len);
        result.push_str("\n\n... (output truncated)");
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
    fn execute_with_timeout_param() {
        let args = serde_json::json!({
            "command": "sleep 60",
            "timeout": 1
        });
        let ctx = ToolContext { project_root: Path::new("/tmp").to_path_buf() };
        let err = execute(args, ctx).unwrap_err();
        assert!(err.to_string().contains("timed out"));
    }
}
