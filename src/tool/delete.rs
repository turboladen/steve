//! Delete tool — remove files or directories with safety checks.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

pub fn tool() -> ToolEntry {
    let def_json = definition();
    let func = def_json.get("function").unwrap();
    ToolEntry {
        def: ToolDef {
            name: ToolName::Delete,
            description: func.get("description").unwrap().as_str().unwrap().to_string(),
            parameters: func.get("parameters").cloned().unwrap(),
        },
        handler: Box::new(execute),
    }
}

fn definition() -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "delete",
            "description": "Delete a file or directory. For directories, removes recursively. Refuses to delete the project root.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to delete (relative to project root or absolute)."
                    }
                },
                "required": ["path"]
            }
        }
    })
}

fn execute(args: Value, ctx: ToolContext) -> Result<ToolOutput> {
    let path_str = args
        .get("path")
        .and_then(|v| v.as_str())
        .context("missing 'path' parameter")?;

    let path = resolve_path(path_str, &ctx.project_root);

    if !path.exists() {
        bail!("path does not exist: {}", path.display());
    }

    // Safety: refuse to delete the project root
    let canonical = path.canonicalize()
        .with_context(|| format!("failed to resolve: {}", path.display()))?;
    let root_canonical = ctx.project_root.canonicalize()
        .unwrap_or_else(|_| ctx.project_root.clone());
    if canonical == root_canonical {
        bail!("refusing to delete the project root: {}", path.display());
    }

    let kind = if path.is_dir() {
        fs::remove_dir_all(&path)
            .with_context(|| format!("failed to delete directory: {}", path.display()))?;
        "directory"
    } else {
        fs::remove_file(&path)
            .with_context(|| format!("failed to delete file: {}", path.display()))?;
        "file"
    };

    Ok(ToolOutput {
        title: format!("Delete {path_str}"),
        output: format!("Deleted {kind}: {}", path.display()),
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
    fn delete_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("doomed.txt"), "bye").unwrap();

        let result = execute(json!({"path": "doomed.txt"}), make_ctx(dir.path())).unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("file"));
        assert!(!dir.path().join("doomed.txt").exists());
    }

    #[test]
    fn delete_directory() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("sub/nested")).unwrap();
        fs::write(dir.path().join("sub/nested/f.txt"), "data").unwrap();

        let result = execute(json!({"path": "sub"}), make_ctx(dir.path())).unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("directory"));
        assert!(!dir.path().join("sub").exists());
    }

    #[test]
    fn delete_nonexistent_errors() {
        let dir = tempdir().unwrap();
        let result = execute(json!({"path": "nope.txt"}), make_ctx(dir.path()));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    #[test]
    fn delete_project_root_refused() {
        let dir = tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let result = execute(
            json!({"path": root.to_string_lossy()}),
            make_ctx(dir.path()),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("project root"));
    }
}
