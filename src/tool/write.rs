//! Write tool — creates or overwrites files.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

pub fn tool() -> ToolEntry {
    let def_json = definition();
    let func = def_json.get("function").unwrap();
    ToolEntry {
        def: ToolDef {
            name: ToolName::Write,
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
            "name": "write",
            "description": "Write content to a file, creating it if it doesn't exist or overwriting if it does. Parent directories are created automatically.",
            "parameters": {
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Path to the file to write (relative to project root or absolute)."
                    },
                    "content": {
                        "type": "string",
                        "description": "The content to write to the file."
                    }
                },
                "required": ["file_path", "content"]
            }
        }
    })
}

pub fn execute(args: Value, ctx: ToolContext) -> Result<ToolOutput> {
    let file_path_str = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .context("missing 'file_path' parameter")?;

    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .context("missing 'content' parameter")?;

    let file_path = resolve_path(file_path_str, &ctx.project_root);

    // Create parent directories if needed
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directories for: {}", file_path.display()))?;
    }

    let existed = file_path.exists();

    fs::write(&file_path, content)
        .with_context(|| format!("failed to write file: {}", file_path.display()))?;

    let action = if existed { "Overwrote" } else { "Created" };
    let title = format!("Write {}", file_path_str);

    Ok(ToolOutput {
        title,
        output: format!(
            "{} {} ({} bytes)",
            action,
            file_path.display(),
            content.len()
        ),
        is_error: false,
    })
}

fn resolve_path(path_str: &str, project_root: &std::path::Path) -> PathBuf {
    let path = PathBuf::from(path_str);
    if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    }
}
