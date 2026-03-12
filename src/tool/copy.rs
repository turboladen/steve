//! Copy tool — duplicate files or directories.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

pub fn tool() -> ToolEntry {
    let def_json = definition();
    let func = def_json.get("function").unwrap();
    ToolEntry {
        def: ToolDef {
            name: ToolName::Copy,
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
            "name": "copy",
            "description": "Copy a file or directory. For directories, copies recursively. Creates parent directories for the destination if needed.",
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

    if from.is_dir() {
        copy_dir_recursive(&from, &to)
            .with_context(|| format!("failed to copy directory {} → {}", from.display(), to.display()))?;
        Ok(ToolOutput {
            title: format!("Copy {from_str} → {to_str}"),
            output: format!("Copied directory {} → {}", from.display(), to.display()),
            is_error: false,
        })
    } else {
        fs::copy(&from, &to)
            .with_context(|| format!("failed to copy {} → {}", from.display(), to.display()))?;
        Ok(ToolOutput {
            title: format!("Copy {from_str} → {to_str}"),
            output: format!("Copied {} → {}", from.display(), to.display()),
            is_error: false,
        })
    }
}

/// Recursively copy a directory and its contents.
fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}

fn resolve_path(path_str: &str, project_root: &Path) -> PathBuf {
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
    fn copy_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "hello").unwrap();

        let result = execute(
            json!({"from_path": "a.txt", "to_path": "b.txt"}),
            make_ctx(dir.path()),
        )
        .unwrap();
        assert!(!result.is_error);
        // Source still exists
        assert!(dir.path().join("a.txt").exists());
        assert_eq!(fs::read_to_string(dir.path().join("b.txt")).unwrap(), "hello");
    }

    #[test]
    fn copy_directory_recursive() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src/sub")).unwrap();
        fs::write(dir.path().join("src/a.txt"), "a").unwrap();
        fs::write(dir.path().join("src/sub/b.txt"), "b").unwrap();

        let result = execute(
            json!({"from_path": "src", "to_path": "dst"}),
            make_ctx(dir.path()),
        )
        .unwrap();
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(dir.path().join("dst/a.txt")).unwrap(), "a");
        assert_eq!(fs::read_to_string(dir.path().join("dst/sub/b.txt")).unwrap(), "b");
    }

    #[test]
    fn copy_nonexistent_source_errors() {
        let dir = tempdir().unwrap();
        let result = execute(
            json!({"from_path": "nope.txt", "to_path": "dest.txt"}),
            make_ctx(dir.path()),
        );
        assert!(result.is_err());
    }
}
