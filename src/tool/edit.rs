//! Edit tool — performs string replacement and line-based operations in files.
//!
//! Supports five operations via the `operation` parameter:
//! - `find_replace` (default): Exact string replacement (original behavior)
//! - `insert_lines`: Insert content at a specific line number
//! - `delete_lines`: Delete a range of lines
//! - `replace_range`: Replace a range of lines with new content
//! - `multi_find_replace`: Apply multiple find-replace pairs atomically

use std::fs;

use anyhow::{Context, Result, bail};
use ropey::Rope;
use serde_json::Value;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

pub fn tool() -> ToolEntry {
    let def_json = definition();
    let func = def_json.get("function").unwrap();
    ToolEntry {
        def: ToolDef {
            name: ToolName::Edit,
            description: func
                .get("description")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
            parameters: func.get("parameters").cloned().unwrap(),
        },
        handler: Box::new(execute),
    }
}

pub fn definition() -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "edit",
            "description": "Edit a file. Supports multiple operations:\n\n\
                - **find_replace** (default): Exact string replacement. Provide old_string and new_string. \
                The old_string must match exactly (including whitespace/indentation). Fails if old_string \
                appears multiple times — provide more surrounding context to make it unique.\n\
                - **multi_find_replace**: Apply multiple find-replace pairs atomically in a single call. \
                Provide an array of {old_string, new_string} objects in the `edits` parameter. \
                All old_strings must appear exactly once. Edits must not overlap. \
                Reduces round-trips when making multiple independent replacements in one file.\n\
                - **insert_lines**: Insert content at a specific line number (1-indexed). Content is inserted \
                before the specified line. Use line=N+1 to append after the last line.\n\
                - **delete_lines**: Delete a range of lines (1-indexed, inclusive).\n\
                - **replace_range**: Replace a range of lines (1-indexed, inclusive) with new content.",
            "parameters": {
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Path to the file to edit (relative to project root or absolute)."
                    },
                    "operation": {
                        "type": "string",
                        "enum": ["find_replace", "insert_lines", "delete_lines", "replace_range", "multi_find_replace"],
                        "description": "The edit operation to perform. Defaults to find_replace if omitted."
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The exact string to find in the file (find_replace only)."
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The string to replace it with (find_replace only)."
                    },
                    "line": {
                        "type": "integer",
                        "description": "Line number to insert before (1-indexed). Use N+1 to append after last line (insert_lines only)."
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to insert or replace with (insert_lines and replace_range)."
                    },
                    "start_line": {
                        "type": "integer",
                        "description": "First line of range, inclusive (1-indexed) (delete_lines and replace_range)."
                    },
                    "end_line": {
                        "type": "integer",
                        "description": "Last line of range, inclusive (1-indexed) (delete_lines and replace_range)."
                    },
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_string": { "type": "string" },
                                "new_string": { "type": "string" }
                            },
                            "required": ["old_string", "new_string"]
                        },
                        "description": "Array of find-replace pairs to apply atomically (multi_find_replace only). Each old_string must appear exactly once in the file."
                    }
                },
                "required": ["file_path"]
            }
        }
    })
}

pub fn execute(args: Value, ctx: ToolContext) -> Result<ToolOutput> {
    let file_path_str = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .context("missing 'file_path' parameter")?;

    let operation: super::EditOperation = args
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("find_replace")
        .parse()
        .map_err(|_| {
            let raw = args.get("operation").and_then(|v| v.as_str()).unwrap_or("?");
            anyhow::anyhow!(
                "unknown edit operation: '{raw}'. Expected one of: find_replace, insert_lines, delete_lines, replace_range, multi_find_replace"
            )
        })?;

    match operation {
        super::EditOperation::FindReplace => execute_find_replace(&args, file_path_str, &ctx),
        super::EditOperation::InsertLines => execute_insert_lines(&args, file_path_str, &ctx),
        super::EditOperation::DeleteLines => execute_delete_lines(&args, file_path_str, &ctx),
        super::EditOperation::ReplaceRange => execute_replace_range(&args, file_path_str, &ctx),
        super::EditOperation::MultiFindReplace => {
            execute_multi_find_replace(&args, file_path_str, &ctx)
        }
    }
}

/// Original find-and-replace logic — String-based, not ropey.
fn execute_find_replace(
    args: &Value,
    file_path_str: &str,
    ctx: &ToolContext,
) -> Result<ToolOutput> {
    let old_string = args
        .get("old_string")
        .and_then(|v| v.as_str())
        .context("missing 'old_string' parameter for find_replace")?;

    let new_string = args
        .get("new_string")
        .and_then(|v| v.as_str())
        .context("missing 'new_string' parameter for find_replace")?;

    let file_path = super::resolve_path(file_path_str, &ctx.project_root);

    let content = fs::read_to_string(&file_path)
        .with_context(|| format!("failed to read file: {}", file_path.display()))?;

    let count = content.matches(old_string).count();

    if count == 0 {
        bail!("old_string not found in {}", file_path.display());
    }

    if count > 1 {
        bail!(
            "old_string found {} times in {}. Provide more context to make the match unique.",
            count,
            file_path.display()
        );
    }

    let new_content = content.replacen(old_string, new_string, 1);

    fs::write(&file_path, &new_content)
        .with_context(|| format!("failed to write file: {}", file_path.display()))?;

    let title = format!("Edit {}", file_path_str);
    Ok(ToolOutput {
        title,
        output: format!(
            "Successfully edited {}. Replaced {} bytes with {} bytes.",
            file_path.display(),
            old_string.len(),
            new_string.len()
        ),
        is_error: false,
    })
}

/// Insert content before a specific line number (1-indexed).
fn execute_insert_lines(
    args: &Value,
    file_path_str: &str,
    ctx: &ToolContext,
) -> Result<ToolOutput> {
    let line = args
        .get("line")
        .and_then(|v| v.as_u64())
        .context("missing 'line' parameter for insert_lines")? as usize;

    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .context("missing 'content' parameter for insert_lines")?;

    if line == 0 {
        bail!("line number must be >= 1 (1-indexed), got 0");
    }

    let file_path = super::resolve_path(file_path_str, &ctx.project_root);

    let file_content = fs::read_to_string(&file_path)
        .with_context(|| format!("failed to read file: {}", file_path.display()))?;

    let mut rope = Rope::from_str(&file_content);
    let total = total_lines(&rope);

    // line is 1-indexed; valid range is [1, total+1] where total+1 means "append after last line"
    if line > total + 1 {
        bail!(
            "line {} is past end of file ({} lines). Valid range: 1-{}",
            line,
            total,
            total + 1
        );
    }

    // Convert 1-indexed to 0-indexed
    let line_idx = line - 1;

    // Ensure content ends with newline (lines are newline-terminated in ropes)
    let insert_content = if content.ends_with('\n') {
        content.to_string()
    } else {
        format!("{content}\n")
    };

    if line_idx == total {
        // Appending after the last line (also handles empty files where total=0, line=1)
        // Ensure the file ends with a newline first
        let len = rope.len_chars();
        if len > 0 && rope.char(len - 1) != '\n' {
            rope.insert_char(len, '\n');
        }
        let insert_pos = rope.len_chars();
        rope.insert(insert_pos, &insert_content);
    } else {
        let char_idx = rope.line_to_char(line_idx);
        rope.insert(char_idx, &insert_content);
    }

    let result = rope.to_string();
    fs::write(&file_path, &result)
        .with_context(|| format!("failed to write file: {}", file_path.display()))?;

    let inserted_lines = content.lines().count();
    let title = format!("Edit {}", file_path_str);
    Ok(ToolOutput {
        title,
        output: format!(
            "Inserted {} line(s) at line {} in {}.",
            inserted_lines,
            line,
            file_path.display()
        ),
        is_error: false,
    })
}

/// Delete a range of lines (1-indexed, inclusive).
fn execute_delete_lines(
    args: &Value,
    file_path_str: &str,
    ctx: &ToolContext,
) -> Result<ToolOutput> {
    let start_line = args
        .get("start_line")
        .and_then(|v| v.as_u64())
        .context("missing 'start_line' parameter for delete_lines")? as usize;

    let end_line = args
        .get("end_line")
        .and_then(|v| v.as_u64())
        .context("missing 'end_line' parameter for delete_lines")? as usize;

    if start_line == 0 {
        bail!("start_line must be >= 1 (1-indexed), got 0");
    }
    if end_line == 0 {
        bail!("end_line must be >= 1 (1-indexed), got 0");
    }
    if start_line > end_line {
        bail!("start_line ({start_line}) must be <= end_line ({end_line})");
    }

    let file_path = super::resolve_path(file_path_str, &ctx.project_root);

    let file_content = fs::read_to_string(&file_path)
        .with_context(|| format!("failed to read file: {}", file_path.display()))?;

    let mut rope = Rope::from_str(&file_content);
    let total = total_lines(&rope);

    if end_line > total {
        bail!(
            "end_line {} is past end of file ({} lines). Valid range: 1-{}",
            end_line,
            total,
            total
        );
    }

    // Convert to 0-indexed
    let start_idx = start_line - 1;
    let end_idx = end_line; // exclusive (end_line is inclusive, so +1 in 0-indexed)

    let start_char = rope.line_to_char(start_idx);
    let end_char = if end_idx < rope.len_lines() {
        rope.line_to_char(end_idx)
    } else {
        rope.len_chars()
    };

    rope.remove(start_char..end_char);

    let result = rope.to_string();
    fs::write(&file_path, &result)
        .with_context(|| format!("failed to write file: {}", file_path.display()))?;

    let deleted_count = end_line - start_line + 1;
    let title = format!("Edit {}", file_path_str);
    Ok(ToolOutput {
        title,
        output: format!(
            "Deleted {} line(s) ({}-{}) from {}.",
            deleted_count,
            start_line,
            end_line,
            file_path.display()
        ),
        is_error: false,
    })
}

/// Replace a range of lines (1-indexed, inclusive) with new content.
fn execute_replace_range(
    args: &Value,
    file_path_str: &str,
    ctx: &ToolContext,
) -> Result<ToolOutput> {
    let start_line = args
        .get("start_line")
        .and_then(|v| v.as_u64())
        .context("missing 'start_line' parameter for replace_range")? as usize;

    let end_line = args
        .get("end_line")
        .and_then(|v| v.as_u64())
        .context("missing 'end_line' parameter for replace_range")? as usize;

    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .context("missing 'content' parameter for replace_range")?;

    if start_line == 0 {
        bail!("start_line must be >= 1 (1-indexed), got 0");
    }
    if end_line == 0 {
        bail!("end_line must be >= 1 (1-indexed), got 0");
    }
    if start_line > end_line {
        bail!("start_line ({start_line}) must be <= end_line ({end_line})");
    }

    let file_path = super::resolve_path(file_path_str, &ctx.project_root);

    let file_content = fs::read_to_string(&file_path)
        .with_context(|| format!("failed to read file: {}", file_path.display()))?;

    let mut rope = Rope::from_str(&file_content);
    let total = total_lines(&rope);

    if end_line > total {
        bail!(
            "end_line {} is past end of file ({} lines). Valid range: 1-{}",
            end_line,
            total,
            total
        );
    }

    // Convert to 0-indexed
    let start_idx = start_line - 1;
    let end_idx = end_line; // exclusive

    let start_char = rope.line_to_char(start_idx);
    let end_char = if end_idx < rope.len_lines() {
        rope.line_to_char(end_idx)
    } else {
        rope.len_chars()
    };

    // Ensure replacement content ends with newline (unless replacing through end of file
    // where original didn't have trailing newline)
    let needs_trailing_newline = end_idx < rope.len_lines() || file_content.ends_with('\n');
    let replace_content = if needs_trailing_newline && !content.ends_with('\n') {
        format!("{content}\n")
    } else {
        content.to_string()
    };

    rope.remove(start_char..end_char);
    rope.insert(start_char, &replace_content);

    let result = rope.to_string();
    fs::write(&file_path, &result)
        .with_context(|| format!("failed to write file: {}", file_path.display()))?;

    let old_count = end_line - start_line + 1;
    let new_count = content.lines().count();
    let title = format!("Edit {}", file_path_str);
    Ok(ToolOutput {
        title,
        output: format!(
            "Replaced {} line(s) ({}-{}) with {} line(s) in {}.",
            old_count,
            start_line,
            end_line,
            new_count,
            file_path.display()
        ),
        is_error: false,
    })
}

/// Apply multiple find-replace pairs atomically to a single file.
///
/// All `old_string` values must appear exactly once; matches must not overlap.
/// Replacements are applied from end-to-front so byte offsets remain stable.
fn execute_multi_find_replace(
    args: &Value,
    file_path_str: &str,
    ctx: &ToolContext,
) -> Result<ToolOutput> {
    let edits = args
        .get("edits")
        .and_then(|v| v.as_array())
        .context("missing 'edits' array parameter for multi_find_replace")?;

    if edits.is_empty() {
        bail!("'edits' array must not be empty");
    }

    // Parse edit pairs
    let mut pairs: Vec<(&str, &str)> = Vec::with_capacity(edits.len());
    for (i, edit) in edits.iter().enumerate() {
        let old = edit
            .get("old_string")
            .and_then(|v| v.as_str())
            .with_context(|| format!("edit[{i}] missing 'old_string'"))?;
        let new = edit
            .get("new_string")
            .and_then(|v| v.as_str())
            .with_context(|| format!("edit[{i}] missing 'new_string'"))?;
        pairs.push((old, new));
    }

    // Check for duplicate old_string values across edit pairs
    for i in 0..pairs.len() {
        for j in (i + 1)..pairs.len() {
            if pairs[i].0 == pairs[j].0 {
                bail!("edit[{i}] and edit[{j}] have the same old_string");
            }
        }
    }

    let file_path = super::resolve_path(file_path_str, &ctx.project_root);
    let content = fs::read_to_string(&file_path)
        .with_context(|| format!("failed to read file: {}", file_path.display()))?;

    // Validate each old_string appears exactly once, collect (start_byte, old_len, new_string)
    let mut matches: Vec<(usize, usize, &str)> = Vec::with_capacity(pairs.len());
    for (i, (old, new)) in pairs.iter().enumerate() {
        let count = content.matches(old).count();
        if count == 0 {
            bail!("edit[{i}]: old_string not found in {}", file_path.display());
        }
        if count > 1 {
            bail!(
                "edit[{i}]: old_string found {count} times in {}. Provide more context to make the match unique.",
                file_path.display()
            );
        }
        let start = content.find(old).expect("verified count == 1 above");
        matches.push((start, old.len(), new));
    }

    // Check for overlapping ranges
    for i in 0..matches.len() {
        let (start_a, len_a, _) = matches[i];
        let end_a = start_a + len_a;
        for (j, &(start_b, len_b, _)) in matches.iter().enumerate().skip(i + 1) {
            let end_b = start_b + len_b;
            // Overlap: ranges [start_a, end_a) and [start_b, end_b) intersect
            if start_a < end_b && start_b < end_a {
                bail!(
                    "edit[{i}] and edit[{j}] have overlapping matches (bytes {start_a}..{end_a} and {start_b}..{end_b})"
                );
            }
        }
    }

    // Sort by start_byte descending for end-to-front application
    matches.sort_by(|a, b| b.0.cmp(&a.0));

    // Apply replacements
    let mut result = content;
    for (start, old_len, new_str) in &matches {
        result.replace_range(*start..(*start + *old_len), new_str);
    }

    fs::write(&file_path, &result)
        .with_context(|| format!("failed to write file: {}", file_path.display()))?;

    let count = pairs.len();
    let title = format!("Edit {}", file_path_str);
    let mut output = format!(
        "Successfully applied {count} edit(s) to {}.",
        file_path.display()
    );
    if count == 1 {
        output.push_str(" Hint: use 'find_replace' for single replacements.");
    }
    Ok(ToolOutput {
        title,
        output,
        is_error: false,
    })
}

/// Get the actual number of lines in a rope, correcting for ropey's trailing-newline behavior.
///
/// Ropey's `len_lines()` counts a trailing `\n` as starting a new empty line,
/// so a 3-line file ending with `\n` reports 4. This returns the count users expect.
fn total_lines(rope: &Rope) -> usize {
    if rope.len_chars() == 0 {
        return 0;
    }
    let len = rope.len_lines();
    if rope.char(rope.len_chars() - 1) == '\n' {
        len - 1
    } else {
        len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx(dir: &tempfile::TempDir) -> ToolContext {
        ToolContext {
            project_root: dir.path().to_path_buf(),
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        }
    }

    // ── find_replace tests (backward compat) ──

    #[test]
    fn successful_edit() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello world").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "old_string": "hello",
            "new_string": "goodbye"
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "goodbye world");
    }

    #[test]
    fn successful_edit_explicit_operation() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello world").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "find_replace",
            "old_string": "hello",
            "new_string": "goodbye"
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "goodbye world");
    }

    #[test]
    fn edit_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello world").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "old_string": "missing",
            "new_string": "new"
        });
        let result = execute(args, test_ctx(&dir));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn edit_multiple_occurrences_fails() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "foo bar foo").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "old_string": "foo",
            "new_string": "baz"
        });
        let result = execute(args, test_ctx(&dir));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("2 times"));
    }

    #[test]
    fn edit_missing_file() {
        let dir = tempfile::tempdir().unwrap();

        let args = serde_json::json!({
            "file_path": dir.path().join("nope.txt").to_str().unwrap(),
            "old_string": "a",
            "new_string": "b"
        });
        let result = execute(args, test_ctx(&dir));
        assert!(result.is_err());
    }

    #[test]
    fn edit_with_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("src").join("main.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "fn main() {}").unwrap();

        let args = serde_json::json!({
            "file_path": "src/main.rs",
            "old_string": "fn main() {}",
            "new_string": "fn main() { println!(\"hi\"); }"
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert!(fs::read_to_string(&file).unwrap().contains("println"));
    }

    // ── insert_lines tests ──

    #[test]
    fn insert_lines_at_beginning() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "line1\nline2\nline3\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "insert_lines",
            "line": 1,
            "content": "inserted"
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "inserted\nline1\nline2\nline3\n"
        );
    }

    #[test]
    fn insert_lines_in_middle() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "line1\nline2\nline3\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "insert_lines",
            "line": 2,
            "content": "inserted"
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "line1\ninserted\nline2\nline3\n"
        );
    }

    #[test]
    fn insert_lines_at_end() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "line1\nline2\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "insert_lines",
            "line": 3,
            "content": "line3"
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "line1\nline2\nline3\n");
    }

    #[test]
    fn insert_lines_multiline_content() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "line1\nline4\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "insert_lines",
            "line": 2,
            "content": "line2\nline3"
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "line1\nline2\nline3\nline4\n"
        );
    }

    #[test]
    fn insert_lines_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "insert_lines",
            "line": 1,
            "content": "first line"
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "first line\n");
    }

    #[test]
    fn insert_lines_line_zero_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "insert_lines",
            "line": 0,
            "content": "nope"
        });
        let result = execute(args, test_ctx(&dir));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains(">= 1"));
    }

    #[test]
    fn insert_lines_past_end_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "one\ntwo\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "insert_lines",
            "line": 4,
            "content": "nope"
        });
        let result = execute(args, test_ctx(&dir));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("past end"));
    }

    // ── delete_lines tests ──

    #[test]
    fn delete_lines_single_line() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "line1\nline2\nline3\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "delete_lines",
            "start_line": 2,
            "end_line": 2
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "line1\nline3\n");
    }

    #[test]
    fn delete_lines_range() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "delete_lines",
            "start_line": 2,
            "end_line": 4
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "a\ne\n");
    }

    #[test]
    fn delete_lines_entire_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "a\nb\nc\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "delete_lines",
            "start_line": 1,
            "end_line": 3
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "");
    }

    #[test]
    fn delete_lines_first_line() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "first\nsecond\nthird\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "delete_lines",
            "start_line": 1,
            "end_line": 1
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "second\nthird\n");
    }

    #[test]
    fn delete_lines_last_line() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "first\nsecond\nthird\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "delete_lines",
            "start_line": 3,
            "end_line": 3
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "first\nsecond\n");
    }

    #[test]
    fn delete_lines_invalid_range_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "a\nb\nc\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "delete_lines",
            "start_line": 3,
            "end_line": 1
        });
        let result = execute(args, test_ctx(&dir));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must be <="));
    }

    #[test]
    fn delete_lines_out_of_bounds_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "a\nb\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "delete_lines",
            "start_line": 1,
            "end_line": 5
        });
        let result = execute(args, test_ctx(&dir));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("past end"));
    }

    #[test]
    fn delete_lines_zero_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "a\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "delete_lines",
            "start_line": 0,
            "end_line": 1
        });
        let result = execute(args, test_ctx(&dir));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains(">= 1"));
    }

    // ── replace_range tests ──

    #[test]
    fn replace_range_single_line() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "line1\nold\nline3\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "replace_range",
            "start_line": 2,
            "end_line": 2,
            "content": "new"
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "line1\nnew\nline3\n");
    }

    #[test]
    fn replace_range_expand() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "before\nold\nafter\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "replace_range",
            "start_line": 2,
            "end_line": 2,
            "content": "new1\nnew2\nnew3"
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "before\nnew1\nnew2\nnew3\nafter\n"
        );
    }

    #[test]
    fn replace_range_shrink() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "replace_range",
            "start_line": 2,
            "end_line": 4,
            "content": "replaced"
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "a\nreplaced\ne\n");
    }

    #[test]
    fn replace_range_preserves_surrounding() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "header\nold1\nold2\nfooter\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "replace_range",
            "start_line": 2,
            "end_line": 3,
            "content": "new1\nnew2\nnew3"
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        let content = fs::read_to_string(&file).unwrap();
        assert_eq!(content, "header\nnew1\nnew2\nnew3\nfooter\n");
    }

    #[test]
    fn replace_range_single_line_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "only line\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "replace_range",
            "start_line": 1,
            "end_line": 1,
            "content": "replaced"
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "replaced\n");
    }

    #[test]
    fn replace_range_out_of_bounds_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "a\nb\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "replace_range",
            "start_line": 1,
            "end_line": 5,
            "content": "x"
        });
        let result = execute(args, test_ctx(&dir));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("past end"));
    }

    // ── edge case tests ──

    #[test]
    fn unknown_operation_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "teleport"
        });
        let result = execute(args, test_ctx(&dir));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unknown edit operation")
        );
    }

    #[test]
    fn find_replace_missing_old_string_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "find_replace",
            "new_string": "bye"
        });
        let result = execute(args, test_ctx(&dir));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("old_string"));
    }

    #[test]
    fn insert_lines_missing_content_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "insert_lines",
            "line": 1
        });
        let result = execute(args, test_ctx(&dir));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("content"));
    }

    #[test]
    fn total_lines_with_trailing_newline() {
        let rope = Rope::from_str("a\nb\nc\n");
        assert_eq!(total_lines(&rope), 3);
    }

    #[test]
    fn total_lines_without_trailing_newline() {
        let rope = Rope::from_str("a\nb\nc");
        assert_eq!(total_lines(&rope), 3);
    }

    #[test]
    fn total_lines_empty() {
        let rope = Rope::from_str("");
        assert_eq!(total_lines(&rope), 0);
    }

    #[test]
    fn total_lines_single_newline() {
        let rope = Rope::from_str("\n");
        assert_eq!(total_lines(&rope), 1);
    }

    #[test]
    fn insert_lines_content_with_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "line1\nline3\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "insert_lines",
            "line": 2,
            "content": "line2\n"
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "line1\nline2\nline3\n");
    }

    #[test]
    fn insert_lines_no_trailing_newline_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "line1\nline2").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "insert_lines",
            "line": 3,
            "content": "line3"
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "line1\nline2\nline3\n");
    }

    #[test]
    fn delete_lines_no_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "a\nb\nc").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "delete_lines",
            "start_line": 3,
            "end_line": 3
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "a\nb\n");
    }

    #[test]
    fn replace_range_last_line_no_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "a\nb\nc").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "replace_range",
            "start_line": 3,
            "end_line": 3,
            "content": "z"
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "a\nb\nz");
    }

    // ── multi_find_replace tests ──

    #[test]
    fn multi_find_replace_basic() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "fn foo() {}\nfn bar() {}\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "multi_find_replace",
            "edits": [
                { "old_string": "fn foo()", "new_string": "fn baz()" },
                { "old_string": "fn bar()", "new_string": "fn qux()" }
            ]
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "fn baz() {}\nfn qux() {}\n"
        );
    }

    #[test]
    fn multi_find_replace_three_edits() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "aaa\nbbb\nccc\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "multi_find_replace",
            "edits": [
                { "old_string": "aaa", "new_string": "AAA" },
                { "old_string": "bbb", "new_string": "BBB" },
                { "old_string": "ccc", "new_string": "CCC" }
            ]
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "AAA\nBBB\nCCC\n");
    }

    #[test]
    fn multi_find_replace_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello world").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "multi_find_replace",
            "edits": [
                { "old_string": "hello", "new_string": "hi" },
                { "old_string": "missing", "new_string": "gone" }
            ]
        });
        let result = execute(args, test_ctx(&dir));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("edit[1]"));
        assert!(err.contains("not found"));
        // File should be unchanged (atomic: validate all before applying any)
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[test]
    fn multi_find_replace_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "foo bar foo baz").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "multi_find_replace",
            "edits": [
                { "old_string": "foo", "new_string": "qux" },
                { "old_string": "baz", "new_string": "quux" }
            ]
        });
        let result = execute(args, test_ctx(&dir));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("edit[0]"));
        assert!(err.contains("2 times"));
        // File unchanged
        assert_eq!(fs::read_to_string(&file).unwrap(), "foo bar foo baz");
    }

    #[test]
    fn multi_find_replace_overlapping() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "abcdef").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "multi_find_replace",
            "edits": [
                { "old_string": "abcd", "new_string": "XXXX" },
                { "old_string": "cdef", "new_string": "YYYY" }
            ]
        });
        let result = execute(args, test_ctx(&dir));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("overlapping"));
        // File unchanged
        assert_eq!(fs::read_to_string(&file).unwrap(), "abcdef");
    }

    #[test]
    fn multi_find_replace_empty_edits() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "multi_find_replace",
            "edits": []
        });
        let result = execute(args, test_ctx(&dir));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must not be empty")
        );
    }

    #[test]
    fn multi_find_replace_single_edit_hint() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello world").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "multi_find_replace",
            "edits": [
                { "old_string": "hello", "new_string": "hi" }
            ]
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("find_replace"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "hi world");
    }

    #[test]
    fn multi_find_replace_duplicate_old_string() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello world").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "multi_find_replace",
            "edits": [
                { "old_string": "hello", "new_string": "hi" },
                { "old_string": "hello", "new_string": "hey" }
            ]
        });
        let result = execute(args, test_ctx(&dir));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("same old_string"),
            "expected 'same old_string' error, got: {err}"
        );
        // File unchanged
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[test]
    fn multi_find_replace_preserves_unrelated() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "prefix AAA middle BBB suffix\n").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "multi_find_replace",
            "edits": [
                { "old_string": "AAA", "new_string": "111" },
                { "old_string": "BBB", "new_string": "222" }
            ]
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "prefix 111 middle 222 suffix\n"
        );
    }

    #[test]
    fn multi_find_replace_adjacent() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "AABB").unwrap();

        let args = serde_json::json!({
            "file_path": file.to_str().unwrap(),
            "operation": "multi_find_replace",
            "edits": [
                { "old_string": "AA", "new_string": "XX" },
                { "old_string": "BB", "new_string": "YY" }
            ]
        });
        let result = execute(args, test_ctx(&dir)).unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&file).unwrap(), "XXYY");
    }
}
