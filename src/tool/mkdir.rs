//! Mkdir tool — create directories.

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
            name: ToolName::Mkdir,
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
            "name": "mkdir",
            "description": "Create a directory and any necessary parent directories.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to create (relative to project root or absolute)."
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

    if path.exists() {
        return Ok(ToolOutput {
            title: format!("Mkdir {path_str}"),
            output: format!("Directory already exists: {}", path.display()),
            is_error: false,
        });
    }

    fs::create_dir_all(&path)
        .with_context(|| format!("failed to create directory: {}", path.display()))?;

    Ok(ToolOutput {
        title: format!("Mkdir {path_str}"),
        output: format!("Created directory: {}", path.display()),
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
            task_store: None,
            lsp_manager: None,
        }
    }

    #[test]
    fn create_directory() {
        let dir = tempdir().unwrap();
        let result = execute(json!({"path": "new_dir"}), make_ctx(dir.path())).unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("Created"));
        assert!(dir.path().join("new_dir").is_dir());
    }

    #[test]
    fn create_nested_directories() {
        let dir = tempdir().unwrap();
        let result = execute(json!({"path": "a/b/c"}), make_ctx(dir.path())).unwrap();
        assert!(!result.is_error);
        assert!(dir.path().join("a/b/c").is_dir());
    }

    #[test]
    fn already_exists_not_error() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("existing")).unwrap();
        let result = execute(json!({"path": "existing"}), make_ctx(dir.path())).unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("already exists"));
    }

    #[test]
    fn missing_path_errors() {
        let dir = tempdir().unwrap();
        assert!(execute(json!({}), make_ctx(dir.path())).is_err());
    }
}
