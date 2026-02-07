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

            while i < patch_lines.len() && hunk_old_consumed < old_count {
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
