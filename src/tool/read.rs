//! Read tool — reads file contents with optional line range.

use std::io::Read as _;
use std::path::Path;

use serde_json::Value;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

/// Check if a file is binary by looking for null bytes in the first 8KB.
fn is_binary(path: &Path) -> anyhow::Result<bool> {
    let mut file = std::fs::File::open(path)?;
    let mut buf = [0u8; 8192];
    let n = file.read(&mut buf)?;
    Ok(buf[..n].contains(&0))
}

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

    // Detect binary files by checking for null bytes in the first 8KB
    if is_binary(&path)? {
        let size = std::fs::metadata(&path)
            .map(|m| m.len())
            .unwrap_or(0);
        return Ok(ToolOutput {
            title: format!("read {path_str}"),
            output: format!("Binary file ({} bytes), not displayed.", size),
            is_error: false,
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

    if lines.is_empty() {
        return Ok(ToolOutput {
            title: format!("read {path_str}"),
            output: String::new(),
            is_error: false,
        });
    }

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
        .unwrap_or(default_max_lines)
        .max(1);

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
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
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
    fn read_offset_limit_max_lines_interaction() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.rs");
        let content: String = (1..=1000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&file, &content).unwrap();

        // offset=100, limit=200 means read lines 100-299
        // max_lines=50 caps to lines 100-149
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "offset": 100,
            "limit": 200,
            "max_lines": 50
        });
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains(" 100 |"));
        assert!(result.output.contains(" 149 |"));
        assert!(!result.output.contains(" 150 |"));
        assert!(result.output.contains("(showing 50 of 200 lines"));
    }

    #[test]
    fn read_max_lines_zero_clamps_to_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("small.rs");
        std::fs::write(&file, "line one\nline two\n").unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap(), "max_lines": 0});
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        // Should show at least 1 line due to clamping
        assert!(result.output.contains("   1 |"));
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

    #[test]
    fn read_binary_file_detected() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("image.png");
        // PNG header + null bytes
        std::fs::write(&file, b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR").unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap()});
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("Binary file"));
        assert!(result.output.contains("bytes"));
    }

    #[test]
    fn read_text_file_not_detected_as_binary() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, "Hello, world!\nLine two.\n").unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap()});
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        assert!(!result.output.contains("Binary file"));
        assert!(result.output.contains("Hello, world!"));
    }

    #[test]
    fn read_empty_file_not_binary() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("empty.txt");
        std::fs::write(&file, "").unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap()});
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        assert!(!result.output.contains("Binary file"));
    }

    #[test]
    fn is_binary_detects_null_bytes() {
        let dir = tempfile::tempdir().unwrap();

        let bin = dir.path().join("data.bin");
        std::fs::write(&bin, b"hello\x00world").unwrap();
        assert!(is_binary(&bin).unwrap());

        let txt = dir.path().join("data.txt");
        std::fs::write(&txt, b"hello world").unwrap();
        assert!(!is_binary(&txt).unwrap());
    }
}
