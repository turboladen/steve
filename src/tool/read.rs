//! Read tool — reads file contents with optional line range.

use serde_json::Value;

use super::{ToolContext, ToolDef, ToolEntry, ToolOutput};

pub fn tool() -> ToolEntry {
    ToolEntry {
        def: ToolDef {
            name: "read".to_string(),
            description: "Read the contents of a file. Optionally specify a line range.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to read (relative to project root or absolute)"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Starting line number (1-indexed). Defaults to 1."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Number of lines to read. Defaults to all."
                    }
                },
                "required": ["path"]
            }),
        },
        handler: Box::new(|args, ctx| execute(args, ctx)),
    }
}

fn execute(args: Value, ctx: ToolContext) -> anyhow::Result<ToolOutput> {
    let path_str = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing 'path' argument"))?;

    let offset = args
        .get("offset")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(1);

    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);

    // Resolve path relative to project root
    let path = if std::path::Path::new(path_str).is_absolute() {
        std::path::PathBuf::from(path_str)
    } else {
        ctx.project_root.join(path_str)
    };

    if !path.exists() {
        return Ok(ToolOutput {
            title: format!("read {path_str}"),
            output: format!("Error: file not found: {}", path.display()),
            is_error: true,
        });
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;

    let lines: Vec<&str> = content.lines().collect();
    let start = offset.saturating_sub(1); // Convert 1-indexed to 0-indexed
    let end = match limit {
        Some(n) => std::cmp::min(start + n, lines.len()),
        None => lines.len(),
    };

    if start >= lines.len() {
        return Ok(ToolOutput {
            title: format!("read {path_str}"),
            output: format!(
                "Error: offset {} exceeds file length ({} lines)",
                offset,
                lines.len()
            ),
            is_error: true,
        });
    }

    let mut output = String::new();
    for (i, line) in lines[start..end].iter().enumerate() {
        let line_num = start + i + 1;
        output.push_str(&format!("{:>4} | {}\n", line_num, line));
    }

    // Truncate very long outputs
    if output.len() > 50_000 {
        output.truncate(50_000);
        output.push_str("\n... (truncated)");
    }

    Ok(ToolOutput {
        title: format!("read {path_str}"),
        output,
        is_error: false,
    })
}
