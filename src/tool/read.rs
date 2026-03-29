//! Read tool — reads file contents with optional line range, tail, count, or multi-file support.

use std::{
    io::Read as _,
    path::{Path, PathBuf},
};

use serde_json::Value;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

/// Maximum number of files allowed in a single multi-file read.
const MAX_MULTI_FILES: usize = 20;

/// Check if a file is binary by looking for null bytes in the first 8KB.
fn is_binary(path: &Path) -> anyhow::Result<bool> {
    let mut file = std::fs::File::open(path)?;
    let mut buf = [0u8; 8192];
    let n = file.read(&mut buf)?;
    Ok(buf[..n].contains(&0))
}

/// How to read the file contents.
enum ReadMode {
    /// Standard offset/limit reading (existing behavior).
    Normal { offset: usize, limit: Option<usize> },
    /// Read the last N lines of the file.
    Tail { n: usize },
    /// Return only line count and file size.
    Count,
}

pub fn tool() -> ToolEntry {
    ToolEntry {
        def: ToolDef {
            name: ToolName::Read,
            description:
                "Read file contents, line counts, or tail. Supports single or multiple paths."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to read (relative to project root or absolute)"
                    },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Read multiple files. Use instead of path for multiple files."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Starting line number (1-indexed). Defaults to 1."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Number of lines to read. Defaults to all."
                    },
                    "tail": {
                        "type": "integer",
                        "description": "Read the last N lines of the file. Mutually exclusive with offset."
                    },
                    "count": {
                        "type": "boolean",
                        "description": "Return only the line count and file size, without reading content."
                    },
                    "max_lines": {
                        "type": "integer",
                        "description": "Maximum number of lines to return (default: 2000). Use with offset for large files."
                    }
                },
                "required": []
            }),
        },
        handler: Box::new(execute),
    }
}

fn execute(args: Value, ctx: ToolContext) -> anyhow::Result<ToolOutput> {
    let has_path = args.get("path").and_then(|v| v.as_str()).is_some();
    let has_paths = args
        .get("paths")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty());

    // Validate: must have exactly one of path or paths
    if has_path && has_paths {
        return Ok(ToolOutput {
            title: "read".to_string(),
            output: "Error: provide either `path` or `paths`, not both.".to_string(),
            is_error: true,
        });
    }
    if !has_path && !has_paths {
        return Ok(ToolOutput {
            title: "read".to_string(),
            output: "Error: missing `path` or `paths` argument.".to_string(),
            is_error: true,
        });
    }

    // Determine mode (precedence: count > tail > normal)
    let is_count = args.get("count").and_then(|v| v.as_bool()).unwrap_or(false);

    let tail_n = args
        .get("tail")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);

    let mode = if is_count {
        ReadMode::Count
    } else if let Some(n) = tail_n {
        // Validate: tail and offset are mutually exclusive
        if args.get("offset").and_then(|v| v.as_u64()).is_some() {
            return Ok(ToolOutput {
                title: "read".to_string(),
                output: "Error: `tail` and `offset` are mutually exclusive.".to_string(),
                is_error: true,
            });
        }
        ReadMode::Tail { n }
    } else {
        let offset = args
            .get("offset")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(1);
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        ReadMode::Normal { offset, limit }
    };

    let max_lines = args
        .get("max_lines")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(2000)
        .max(1);

    if has_paths {
        // Multi-file mode
        let paths: Vec<String> = args
            .get("paths")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("missing 'paths' argument"))?
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();

        if paths.len() > MAX_MULTI_FILES {
            return Ok(ToolOutput {
                title: "read (multi)".to_string(),
                output: format!(
                    "Error: too many files ({}) — maximum is {MAX_MULTI_FILES}.",
                    paths.len()
                ),
                is_error: true,
            });
        }

        let mut output = String::new();
        let mut error_count = 0;
        for path_str in &paths {
            let resolved = resolve_path(path_str, &ctx.project_root);
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&format!("==> {path_str} <==\n"));
            match read_single_file(path_str, &resolved, &mode, max_lines) {
                Ok(content) => output.push_str(&content),
                Err(e) => {
                    output.push_str(&format!("Error: {e}"));
                    error_count += 1;
                }
            }
            output.push('\n');
        }

        // Only mark as error if ALL files failed
        Ok(ToolOutput {
            title: format!("read {} files", paths.len()),
            output,
            is_error: error_count == paths.len(),
        })
    } else {
        // Single file mode
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'path' argument"))?;
        let resolved = resolve_path(path_str, &ctx.project_root);

        match read_single_file(path_str, &resolved, &mode, max_lines) {
            Ok(content) => Ok(ToolOutput {
                title: format!("read {path_str}"),
                output: content,
                is_error: false,
            }),
            Err(e) => Ok(ToolOutput {
                title: format!("read {path_str}"),
                output: format!("Error: {e}"),
                is_error: true,
            }),
        }
    }
}

/// Resolve a path string relative to the project root.
fn resolve_path(path_str: &str, project_root: &Path) -> PathBuf {
    let p = Path::new(path_str);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        project_root.join(p)
    }
}

/// Read a single file according to the given mode.
fn read_single_file(
    path_str: &str,
    resolved: &Path,
    mode: &ReadMode,
    max_lines: usize,
) -> Result<String, String> {
    if !resolved.exists() {
        return Err(format!("file not found: {}", resolved.display()));
    }

    // Count mode — just return metadata
    if matches!(mode, ReadMode::Count) {
        let size = std::fs::metadata(resolved).map(|m| m.len()).unwrap_or(0);

        if is_binary(resolved).unwrap_or(false) {
            return Ok(format!("{path_str}: binary file ({size} bytes)"));
        }

        let content = std::fs::read_to_string(resolved)
            .map_err(|e| format!("failed to read {}: {e}", resolved.display()))?;
        let line_count = if content.is_empty() {
            0
        } else {
            content.lines().count()
        };
        return Ok(format!("{path_str}: {line_count} lines ({size} bytes)"));
    }

    // Detect binary files
    if is_binary(resolved).unwrap_or(false) {
        let size = std::fs::metadata(resolved).map(|m| m.len()).unwrap_or(0);
        return Ok(format!("Binary file ({size} bytes), not displayed."));
    }

    let content = std::fs::read_to_string(resolved)
        .map_err(|e| format!("failed to read {}: {e}", resolved.display()))?;

    let lines: Vec<&str> = content.lines().collect();

    if lines.is_empty() {
        return Ok(String::new());
    }

    match mode {
        ReadMode::Count => Err("internal error: count mode should be handled earlier".into()),
        ReadMode::Tail { n } => {
            let capped = (*n).min(max_lines);
            let start = lines.len().saturating_sub(capped);
            let mut output = String::new();
            for (i, line) in lines[start..].iter().enumerate() {
                let line_num = start + i + 1;
                output.push_str(&format!("{:>4} | {}\n", line_num, line));
            }
            if *n > max_lines {
                output.push_str(&format!(
                    "\n... (showing last {} of requested {} lines — max_lines cap)",
                    capped, n
                ));
            }
            Ok(output)
        }
        ReadMode::Normal { offset, limit } => {
            let start = offset.saturating_sub(1); // Convert 1-indexed to 0-indexed
            let end = match limit {
                Some(n) => std::cmp::min(start + n, lines.len()),
                None => lines.len(),
            };

            if start >= lines.len() {
                return Err(format!(
                    "offset {} exceeds file length ({} lines)",
                    offset,
                    lines.len()
                ));
            }

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

            Ok(output)
        }
    }
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

    // -- Tail mode tests --

    #[test]
    fn read_tail_basic() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lines.txt");
        let content: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&file, &content).unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap(), "tail": 5});
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("  96 | line 96"));
        assert!(result.output.contains(" 100 | line 100"));
        assert!(!result.output.contains("  95 |"));
    }

    #[test]
    fn read_tail_exceeds_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("short.txt");
        std::fs::write(&file, "a\nb\nc\n").unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap(), "tail": 100});
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        // Should show all 3 lines
        assert!(result.output.contains("   1 | a"));
        assert!(result.output.contains("   3 | c"));
    }

    #[test]
    fn read_tail_with_max_lines() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lines.txt");
        let content: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&file, &content).unwrap();

        // Request tail 50 but cap at max_lines=10
        let args = serde_json::json!({"path": file.to_str().unwrap(), "tail": 50, "max_lines": 10});
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        // Should show only last 10 lines (91-100)
        assert!(result.output.contains("  91 | line 91"));
        assert!(result.output.contains(" 100 | line 100"));
        assert!(!result.output.contains("  90 |"));
        assert!(result.output.contains("showing last 10 of requested 50"));
    }

    #[test]
    fn read_tail_and_offset_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lines.txt");
        std::fs::write(&file, "a\nb\nc\n").unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap(), "tail": 5, "offset": 2});
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("mutually exclusive"));
    }

    // -- Count mode tests --

    #[test]
    fn read_count_basic() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lines.txt");
        let content: String = (1..=42).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&file, &content).unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap(), "count": true});
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("42 lines"));
        assert!(result.output.contains("bytes"));
        // Should NOT contain numbered lines
        assert!(!result.output.contains(" | "));
    }

    #[test]
    fn read_count_binary() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data.bin");
        std::fs::write(&file, b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR").unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap(), "count": true});
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("binary file"));
        assert!(result.output.contains("bytes"));
    }

    #[test]
    fn read_count_ignores_range_params() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lines.txt");
        let content: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&file, &content).unwrap();

        // count should ignore offset, limit, tail, max_lines
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "count": true,
            "offset": 5,
            "limit": 3,
            "max_lines": 2
        });
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("10 lines"));
    }

    // -- Multi-file tests --

    #[test]
    fn read_multi_file_basic() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        std::fs::write(&f1, "alpha\n").unwrap();
        std::fs::write(&f2, "beta\n").unwrap();

        let args = serde_json::json!({
            "paths": [f1.to_str().unwrap(), f2.to_str().unwrap()]
        });
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("==>"));
        assert!(result.output.contains("alpha"));
        assert!(result.output.contains("beta"));
        assert!(result.title.contains("2 files"));
    }

    #[test]
    fn read_multi_file_with_binary() {
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("a.txt");
        let bin = dir.path().join("b.bin");
        std::fs::write(&txt, "hello\n").unwrap();
        std::fs::write(&bin, b"\x00binary\x00").unwrap();

        let args = serde_json::json!({
            "paths": [txt.to_str().unwrap(), bin.to_str().unwrap()]
        });
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("hello"));
        assert!(result.output.contains("Binary file"));
    }

    #[test]
    fn read_multi_file_count() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        std::fs::write(&f1, "one\ntwo\n").unwrap();
        std::fs::write(&f2, "alpha\nbeta\ngamma\n").unwrap();

        let args = serde_json::json!({
            "paths": [f1.to_str().unwrap(), f2.to_str().unwrap()],
            "count": true
        });
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("2 lines"));
        assert!(result.output.contains("3 lines"));
    }

    #[test]
    fn read_path_and_paths_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "content\n").unwrap();

        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "paths": [file.to_str().unwrap()]
        });
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("not both"));
    }

    #[test]
    fn read_neither_path_nor_paths_error() {
        let dir = tempfile::tempdir().unwrap();
        let args = serde_json::json!({});
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("missing"));
    }

    #[test]
    fn read_multi_file_max_cap() {
        let dir = tempfile::tempdir().unwrap();
        // Create 21 files (exceeds MAX_MULTI_FILES=20)
        let paths: Vec<String> = (0..21)
            .map(|i| {
                let f = dir.path().join(format!("f{i}.txt"));
                std::fs::write(&f, format!("file {i}\n")).unwrap();
                f.to_str().unwrap().to_string()
            })
            .collect();

        let args = serde_json::json!({"paths": paths});
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("too many files"));
    }

    #[test]
    fn read_multi_file_partial_failure_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let exists = dir.path().join("exists.txt");
        std::fs::write(&exists, "content\n").unwrap();

        let args = serde_json::json!({
            "paths": [exists.to_str().unwrap(), "/nonexistent/missing.txt"]
        });
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        // Partial failure: one file exists, one doesn't — should NOT be is_error
        assert!(
            !result.is_error,
            "partial failure should not mark entire result as error"
        );
        assert!(result.output.contains("content"));
        assert!(result.output.contains("Error:"));
    }

    #[test]
    fn read_multi_file_all_fail_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let args = serde_json::json!({
            "paths": ["/nonexistent/a.txt", "/nonexistent/b.txt"]
        });
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(result.is_error, "all-fail should mark result as error");
    }
}
