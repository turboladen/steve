//! Patch tool — applies unified diffs to files.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::{ToolContext, ToolDef, ToolEntry, ToolOutput};

pub fn tool() -> ToolEntry {
    let def_json = definition();
    let func = def_json.get("function").unwrap();
    ToolEntry {
        def: ToolDef {
            name: "patch".to_string(),
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
            "name": "patch",
            "description": "Apply a unified diff patch to a file. The patch should be in standard unified diff format (with --- and +++ headers, @@ hunks, and +/- lines).",
            "parameters": {
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Path to the file to patch (relative to project root or absolute)."
                    },
                    "patch": {
                        "type": "string",
                        "description": "The unified diff to apply."
                    }
                },
                "required": ["file_path", "patch"]
            }
        }
    })
}

pub fn execute(args: Value, ctx: ToolContext) -> Result<ToolOutput> {
    let file_path_str = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .context("missing 'file_path' parameter")?;

    let patch_str = args
        .get("patch")
        .and_then(|v| v.as_str())
        .context("missing 'patch' parameter")?;

    let file_path = resolve_path(file_path_str, &ctx.project_root);

    // Read the original file
    let original = fs::read_to_string(&file_path)
        .with_context(|| format!("failed to read file: {}", file_path.display()))?;

    // Apply the patch
    let patched = apply_unified_diff(&original, patch_str)?;

    fs::write(&file_path, &patched)
        .with_context(|| format!("failed to write file: {}", file_path.display()))?;

    let title = format!("Patch {}", file_path_str);
    Ok(ToolOutput {
        title,
        output: format!("Successfully patched {}", file_path.display()),
        is_error: false,
    })
}

/// Simple unified diff applier.
/// Handles basic @@ -start,count +start,count @@ hunks.
fn apply_unified_diff(original: &str, patch: &str) -> Result<String> {
    let original_lines: Vec<&str> = original.lines().collect();
    let mut result_lines: Vec<String> = original_lines.iter().map(|s| s.to_string()).collect();

    // Parse hunks from the patch
    let mut offset: i64 = 0;

    let patch_lines: Vec<&str> = patch.lines().collect();
    let mut i = 0;

    while i < patch_lines.len() {
        let line = patch_lines[i];

        // Skip --- and +++ headers
        if line.starts_with("---") || line.starts_with("+++") {
            i += 1;
            continue;
        }

        // Parse hunk header: @@ -old_start,old_count +new_start,new_count @@
        if line.starts_with("@@") {
            let (old_start, old_count) = parse_hunk_header(line)?;

            // Collect hunk lines
            let mut removals = Vec::new();
            let mut additions = Vec::new();
            let mut context_before = 0;

            i += 1;
            let mut hunk_old_consumed = 0;

            while i < patch_lines.len() {
                let hunk_line = patch_lines[i];

                if hunk_line.starts_with("@@") {
                    break; // Next hunk
                }

                if hunk_line.starts_with('-') {
                    removals.push(&hunk_line[1..]);
                    hunk_old_consumed += 1;
                } else if hunk_line.starts_with('+') {
                    additions.push(&hunk_line[1..]);
                } else if hunk_line.starts_with(' ') || hunk_line.is_empty() {
                    // Context line
                    if removals.is_empty() && additions.is_empty() {
                        context_before += 1;
                    }
                    hunk_old_consumed += 1;
                } else {
                    // Treat as context
                    hunk_old_consumed += 1;
                }

                i += 1;

                // Once all old lines are consumed, continue only for
                // trailing '+' lines that belong to this hunk.
                if hunk_old_consumed >= old_count {
                    while i < patch_lines.len() && patch_lines[i].starts_with('+') {
                        additions.push(&patch_lines[i][1..]);
                        i += 1;
                    }
                    break;
                }
            }

            // Apply this hunk
            let actual_start = ((old_start as i64 - 1 + offset) + context_before as i64) as usize;

            if !removals.is_empty() {
                // Remove lines
                let end = actual_start + removals.len();
                if end > result_lines.len() {
                    bail!("patch hunk extends beyond end of file");
                }
                result_lines.splice(
                    actual_start..end,
                    additions.iter().map(|s| s.to_string()),
                );
                offset += additions.len() as i64 - removals.len() as i64;
            } else if !additions.is_empty() {
                // Pure addition
                for (j, line) in additions.iter().enumerate() {
                    result_lines.insert(actual_start + j, line.to_string());
                }
                offset += additions.len() as i64;
            }
        } else {
            i += 1;
        }
    }

    // Preserve original trailing newline behavior
    let mut result = result_lines.join("\n");
    if original.ends_with('\n') {
        result.push('\n');
    }

    Ok(result)
}

/// Parse a hunk header like "@@ -1,5 +1,7 @@" and return (old_start, old_count).
fn parse_hunk_header(line: &str) -> Result<(usize, usize)> {
    // Find the range between @@ markers
    let stripped = line
        .trim_start_matches("@@")
        .trim_end_matches("@@")
        .trim();

    // Parse -old_start,old_count
    let parts: Vec<&str> = stripped.split_whitespace().collect();
    let old_part = parts
        .first()
        .context("invalid hunk header")?
        .trim_start_matches('-');

    let (start, count) = if let Some((s, c)) = old_part.split_once(',') {
        (s.parse::<usize>()?, c.parse::<usize>()?)
    } else {
        (old_part.parse::<usize>()?, 1)
    };

    Ok((start, count))
}

fn resolve_path(path_str: &str, project_root: &std::path::Path) -> PathBuf {
    let path = PathBuf::from(path_str);
    if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hunk_header_with_counts() {
        let (start, count) = parse_hunk_header("@@ -5,3 +5,4 @@").unwrap();
        assert_eq!(start, 5);
        assert_eq!(count, 3);
    }

    #[test]
    fn parse_hunk_header_single_line() {
        let (start, count) = parse_hunk_header("@@ -1 +1 @@").unwrap();
        assert_eq!(start, 1);
        assert_eq!(count, 1); // default count when no comma
    }

    #[test]
    fn parse_hunk_header_with_context_label() {
        let (start, count) = parse_hunk_header("@@ -10,2 +10,3 @@ fn main()").unwrap();
        assert_eq!(start, 10);
        assert_eq!(count, 2);
    }

    #[test]
    fn apply_simple_replacement() {
        // Replacements need context lines so old_count encompasses the + lines
        let original = "line1\nline2\nline3\n";
        let patch = "--- a/file\n+++ b/file\n@@ -1,3 +1,3 @@\n line1\n-line2\n+replaced\n line3\n";
        let result = apply_unified_diff(original, patch).unwrap();
        assert_eq!(result, "line1\nreplaced\nline3\n");
    }

    #[test]
    fn apply_pure_addition() {
        let original = "line1\nline2\n";
        let patch = "--- a/file\n+++ b/file\n@@ -1,2 +1,3 @@\n line1\n+inserted\n line2\n";
        let result = apply_unified_diff(original, patch).unwrap();
        assert_eq!(result, "line1\ninserted\nline2\n");
    }

    #[test]
    fn apply_pure_deletion() {
        let original = "line1\nline2\nline3\n";
        let patch = "--- a/file\n+++ b/file\n@@ -1,3 +1,2 @@\n line1\n-line2\n line3\n";
        let result = apply_unified_diff(original, patch).unwrap();
        assert_eq!(result, "line1\nline3\n");
    }

    #[test]
    fn apply_multi_line_replacement() {
        let original = "aaa\nbbb\nccc\nddd\n";
        let patch =
            "--- a/f\n+++ b/f\n@@ -1,4 +1,5 @@\n aaa\n-bbb\n-ccc\n+xxx\n+yyy\n+zzz\n ddd\n";
        let result = apply_unified_diff(original, patch).unwrap();
        assert_eq!(result, "aaa\nxxx\nyyy\nzzz\nddd\n");
    }

    #[test]
    fn preserves_trailing_newline() {
        let original = "hello\nworld\n";
        let patch = "--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n-hello\n+goodbye\n world\n";
        let result = apply_unified_diff(original, patch).unwrap();
        assert_eq!(result, "goodbye\nworld\n");
    }

    #[test]
    fn preserves_no_trailing_newline() {
        let original = "hello\nworld";
        let patch = "--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n-hello\n+goodbye\n world\n";
        let result = apply_unified_diff(original, patch).unwrap();
        assert_eq!(result, "goodbye\nworld");
    }

    #[test]
    fn apply_replacement_without_trailing_context() {
        // Regression: +lines after all old lines consumed must not be dropped
        let original = "aaa\nbbb\nccc\n";
        let patch = "--- a/f\n+++ b/f\n@@ -2,1 +2,1 @@\n-bbb\n+xxx\n";
        let result = apply_unified_diff(original, patch).unwrap();
        assert_eq!(result, "aaa\nxxx\nccc\n");
    }

    #[test]
    fn apply_replacement_growing_without_context() {
        // Regression: replacing 1 line with 2 lines, no surrounding context
        let original = "aaa\nbbb\nccc\n";
        let patch = "--- a/f\n+++ b/f\n@@ -2,1 +2,2 @@\n-bbb\n+xxx\n+yyy\n";
        let result = apply_unified_diff(original, patch).unwrap();
        assert_eq!(result, "aaa\nxxx\nyyy\nccc\n");
    }

    #[test]
    fn apply_append_at_end() {
        // Pure addition at the end of file (old_count covers only the context line)
        let original = "aaa\nbbb\n";
        let patch = "--- a/f\n+++ b/f\n@@ -2,1 +2,2 @@\n bbb\n+ccc\n";
        let result = apply_unified_diff(original, patch).unwrap();
        assert_eq!(result, "aaa\nbbb\nccc\n");
    }

    #[test]
    fn hunk_beyond_eof_fails() {
        let original = "one\n";
        let patch = "--- a/f\n+++ b/f\n@@ -5,1 +5,1 @@\n-missing\n+new\n";
        assert!(apply_unified_diff(original, patch).is_err());
    }

    #[test]
    fn execute_patches_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello\nworld\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "patch": "--- a/test.txt\n+++ b/test.txt\n@@ -1,2 +1,2 @@\n-hello\n+goodbye\n world\n"
        });
        let ctx = ToolContext { project_root: dir.path().to_path_buf() };
        let result = execute(args, ctx).unwrap();
        assert!(!result.is_error);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "goodbye\nworld\n");
    }
}
