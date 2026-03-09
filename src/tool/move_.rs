//! Move tool — rename or relocate files and directories.

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
            name: ToolName::Move,
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
            "name": "move",
            "description": "Move or rename a file or directory. Creates parent directories for the destination if needed.",
            "parameters": {
                "type": "object",
                "properties": {
                    "from_path": {
                        "type": "string",
                        "description": "Source path (relative to project root or absolute)."
                    },
                    "to_path": {
                        "type": "string",
                        "description": "Destination path (relative to project root or absolute)."
                    }
                },
                "required": ["from_path", "to_path"]
            }
        }
    })
}

fn execute(args: Value, ctx: ToolContext) -> Result<ToolOutput> {
    let from_str = args
        .get("from_path")
        .and_then(|v| v.as_str())
        .context("missing 'from_path' parameter")?;

    let to_str = args
        .get("to_path")
        .and_then(|v| v.as_str())
        .context("missing 'to_path' parameter")?;

    let from = resolve_path(from_str, &ctx.project_root);
    let to = resolve_path(to_str, &ctx.project_root);

    if !from.exists() {
        bail!("source does not exist: {}", from.display());
    }

    // Create parent directories for destination
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directories for: {}", to.display()))?;
    }

    fs::rename(&from, &to)
        .with_context(|| format!("failed to move {} → {}", from.display(), to.display()))?;

    Ok(ToolOutput {
        title: format!("Move {from_str} → {to_str}"),
        output: format!("Moved {} → {}", from.display(), to.display()),
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
    fn move_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "hello").unwrap();

        let result = execute(
            json!({"from_path": "a.txt", "to_path": "b.txt"}),
            make_ctx(dir.path()),
        )
        .unwrap();
        assert!(!result.is_error);
        assert!(!dir.path().join("a.txt").exists());
        assert_eq!(fs::read_to_string(dir.path().join("b.txt")).unwrap(), "hello");
    }

    #[test]
    fn move_creates_parent_dirs() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "data").unwrap();

        let result = execute(
            json!({"from_path": "a.txt", "to_path": "sub/dir/b.txt"}),
            make_ctx(dir.path()),
        )
        .unwrap();
        assert!(!result.is_error);
        assert!(dir.path().join("sub/dir/b.txt").exists());
    }

    #[test]
    fn move_nonexistent_source_errors() {
        let dir = tempdir().unwrap();
        let result = execute(
            json!({"from_path": "nope.txt", "to_path": "dest.txt"}),
            make_ctx(dir.path()),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    #[test]
    fn move_missing_param_errors() {
        let dir = tempdir().unwrap();
        assert!(execute(json!({"from_path": "a.txt"}), make_ctx(dir.path())).is_err());
        assert!(execute(json!({"to_path": "b.txt"}), make_ctx(dir.path())).is_err());
    }
}
