//! Bash tool — executes shell commands.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde_json::Value;

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

struct CommandResult {
    output: String,
    success: bool,
}

fn run_command(command: &str, cwd: &Path, timeout_secs: u64) -> Result<CommandResult> {
    let output = Command::new("bash")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .output()
        .context("failed to execute bash command")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

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
        result = format!("(exit code: {})", output.status.code().unwrap_or(-1));
    }

    // Truncate very long output
    let max_len = 50_000;
    if result.len() > max_len {
        result.truncate(max_len);
        result.push_str("\n\n... (output truncated)");
    }

    let _ = timeout_secs; // TODO: implement actual timeout with tokio

    Ok(CommandResult {
        output: result,
        success: output.status.success(),
    })
}
