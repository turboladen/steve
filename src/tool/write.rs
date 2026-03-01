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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn make_ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            project_root: dir.to_path_buf(),
            storage_dir: None,
        }
    }

    #[test]
    fn creates_new_file() {
        let dir = tempdir().unwrap();
        let args = json!({
            "file_path": "hello.txt",
            "content": "hello world"
        });
        let result = execute(args, make_ctx(dir.path())).unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("Created"));
        assert!(dir.path().join("hello.txt").exists());
        assert_eq!(fs::read_to_string(dir.path().join("hello.txt")).unwrap(), "hello world");
    }

    #[test]
    fn overwrites_existing_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("existing.txt"), "old content").unwrap();

        let args = json!({
            "file_path": "existing.txt",
            "content": "new content"
        });
        let result = execute(args, make_ctx(dir.path())).unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("Overwrote"));
        assert_eq!(fs::read_to_string(dir.path().join("existing.txt")).unwrap(), "new content");
    }

    #[test]
    fn creates_parent_directories() {
        let dir = tempdir().unwrap();
        let args = json!({
            "file_path": "sub/dir/file.txt",
            "content": "nested"
        });
        let result = execute(args, make_ctx(dir.path())).unwrap();
        assert!(!result.is_error);
        assert!(dir.path().join("sub/dir/file.txt").exists());
        assert_eq!(fs::read_to_string(dir.path().join("sub/dir/file.txt")).unwrap(), "nested");
    }

    #[test]
    fn missing_file_path_error() {
        let dir = tempdir().unwrap();
        let args = json!({
            "content": "no path"
        });
        let result = execute(args, make_ctx(dir.path()));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("file_path"), "expected error about file_path, got: {err_msg}");
    }

    #[test]
    fn missing_content_error() {
        let dir = tempdir().unwrap();
        let args = json!({
            "file_path": "test.txt"
        });
        let result = execute(args, make_ctx(dir.path()));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("content"), "expected error about content, got: {err_msg}");
    }

    #[test]
    fn relative_path_resolves_to_project_root() {
        let dir = tempdir().unwrap();
        let args = json!({
            "file_path": "relative/path.txt",
            "content": "resolved"
        });
        let result = execute(args, make_ctx(dir.path())).unwrap();
        assert!(!result.is_error);
        let expected = dir.path().join("relative/path.txt");
        assert!(expected.exists(), "file should be created under project_root");
        assert_eq!(fs::read_to_string(expected).unwrap(), "resolved");
    }
}
