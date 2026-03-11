//! Patch tool — applies unified diffs to files.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use mpatch::ApplyOptions;
use serde_json::Value;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

pub fn tool() -> ToolEntry {
    let def_json = definition();
    let func = def_json.get("function").unwrap();
    ToolEntry {
        def: ToolDef {
            name: ToolName::Patch,
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

/// Apply a unified diff to the original content using mpatch (with fuzzy matching).
fn apply_unified_diff(original: &str, patch: &str) -> Result<String> {
    let options = ApplyOptions::new();
    let mut result = mpatch::patch_content_str(patch, Some(original), &options)
        .map_err(|e| anyhow::anyhow!("failed to apply patch: {e}"))?;

    // Preserve original trailing newline behavior: mpatch always adds a trailing
    // newline, but if the original file didn't have one, strip it.
    if !original.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }

    Ok(result)
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
        let ctx = ToolContext { project_root: dir.path().to_path_buf(), storage_dir: None, task_store: None };
        let result = execute(args, ctx).unwrap();
        assert!(!result.is_error);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "goodbye\nworld\n");
    }
}
