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
