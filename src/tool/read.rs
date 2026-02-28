//! Read tool — reads file contents with optional line range.

use serde_json::Value;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

pub fn tool() -> ToolEntry {
    ToolEntry {
        def: ToolDef {
            name: ToolName::Read,
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
                    },
                    "max_lines": {
                        "type": "integer",
                        "description": "Maximum number of lines to return (default: 2000). Use with offset for large files."
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

    // Apply max_lines cap to prevent oversized tool results
    let default_max_lines: usize = 2000;
    let max_lines = args
        .get("max_lines")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(default_max_lines);

    let total_available = end - start;
    let actual_end = std::cmp::min(end, start + max_lines);
    let was_line_truncated = actual_end < end;

    let mut output = String::new();
    for (i, line) in lines[start..actual_end].iter().enumerate() {
        let line_num = start + i + 1;
        output.push_str(&format!("{:>4} | {}\n", line_num, line));
    }

    if was_line_truncated {
        output.push_str(&format!(
            "\n... (showing {} of {} lines — use offset/limit to read specific ranges)",
            actual_end - start,
            total_available
        ));
    }

    Ok(ToolOutput {
        title: format!("read {path_str}"),
        output,
        is_error: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            project_root: dir.to_path_buf(),
        }
    }

    #[test]
    fn read_truncates_at_max_lines() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.rs");
        let content: String = (1..=3000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&file, &content).unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap()});
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        // Default max is 2000 lines — output should contain line 2000 but not 2001
        assert!(result.output.contains("2000 |"));
        assert!(!result.output.contains("2001 |"));
        assert!(result.output.contains("(showing 2000 of 3000 lines"));
    }

    #[test]
    fn read_respects_max_lines_param() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.rs");
        let content: String = (1..=500).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&file, &content).unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap(), "max_lines": 100});
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(result.output.contains(" 100 |"));
        assert!(!result.output.contains(" 101 |"));
        assert!(result.output.contains("(showing 100 of 500 lines"));
    }

    #[test]
    fn read_small_file_not_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("small.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap()});
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.output.contains("showing"));
        assert!(!result.output.contains("truncated"));
    }
}
