//! Edit tool — performs string replacement in files.

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
            name: "edit".to_string(),
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
            "name": "edit",
            "description": "Perform an exact string replacement in a file. Provide the old_string to find and the new_string to replace it with. The old_string must match exactly (including whitespace and indentation). If old_string appears multiple times, the replacement will fail — provide more surrounding context to make it unique.",
            "parameters": {
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Path to the file to edit (relative to project root or absolute)."
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The exact string to find in the file."
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The string to replace it with."
                    }
                },
                "required": ["file_path", "old_string", "new_string"]
            }
        }
    })
}

pub fn execute(args: Value, ctx: ToolContext) -> Result<ToolOutput> {
    let file_path_str = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .context("missing 'file_path' parameter")?;

    let old_string = args
        .get("old_string")
        .and_then(|v| v.as_str())
        .context("missing 'old_string' parameter")?;

    let new_string = args
        .get("new_string")
        .and_then(|v| v.as_str())
        .context("missing 'new_string' parameter")?;

    let file_path = resolve_path(file_path_str, &ctx.project_root);

    // Read the file
    let content = fs::read_to_string(&file_path)
        .with_context(|| format!("failed to read file: {}", file_path.display()))?;

    // Count occurrences
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

    // Perform the replacement
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

    fn test_ctx(dir: &tempfile::TempDir) -> ToolContext {
        ToolContext {
            project_root: dir.path().to_path_buf(),
        }
    }

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

    #[test]
    fn resolve_path_absolute_passthrough() {
        let abs = resolve_path("/tmp/file.txt", std::path::Path::new("/project"));
        assert_eq!(abs, PathBuf::from("/tmp/file.txt"));
    }

    #[test]
    fn resolve_path_relative_joins() {
        let rel = resolve_path("src/main.rs", std::path::Path::new("/project"));
        assert_eq!(rel, PathBuf::from("/project/src/main.rs"));
    }
}
