//! List tool — directory listing, .gitignore-aware.

use serde_json::Value;

use ignore::WalkBuilder;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

pub fn tool() -> ToolEntry {
    ToolEntry {
        def: ToolDef {
            name: ToolName::List,
            description: "List files and directories at a given path. Respects .gitignore. Shows file types (file/dir) and sizes.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory to list (relative to project root). Defaults to project root."
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Maximum depth to recurse. Defaults to 1 (immediate children only)."
                    }
                },
                "required": []
            }),
        },
        handler: Box::new(|args, ctx| execute(args, ctx)),
    }
}

fn execute(args: Value, ctx: ToolContext) -> anyhow::Result<ToolOutput> {
    let list_path = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(|p| {
            if std::path::Path::new(p).is_absolute() {
                std::path::PathBuf::from(p)
            } else {
                ctx.project_root.join(p)
            }
        })
        .unwrap_or_else(|| ctx.project_root.clone());

    let depth = args
        .get("depth")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(1);

    if !list_path.exists() {
        return Ok(ToolOutput {
            title: "list".to_string(),
            output: format!("Error: path not found: {}", list_path.display()),
            is_error: true,
        });
    }

    let walker = WalkBuilder::new(&list_path)
        .max_depth(Some(depth + 1)) // +1 because the root itself is depth 0
        .hidden(true)
        .git_ignore(true)
        .sort_by_file_name(|a, b| a.cmp(b))
        .build();

    let mut entries: Vec<String> = Vec::new();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let file_path = entry.path();

        // Skip the root directory itself
        if file_path == list_path {
            continue;
        }

        let relative = file_path
            .strip_prefix(&ctx.project_root)
            .unwrap_or(file_path);

        let relative_str = relative.to_string_lossy();

        if let Some(ft) = entry.file_type() {
            if ft.is_dir() {
                entries.push(format!("{}/", relative_str));
            } else if ft.is_file() {
                // Show file size
                let size = entry
                    .metadata()
                    .ok()
                    .map(|m| format_size(m.len()))
                    .unwrap_or_default();
                entries.push(format!("{} {}", relative_str, size));
            }
        }

        if entries.len() >= 500 {
            entries.push("... (truncated)".to_string());
            break;
        }
    }

    let relative_display = list_path
        .strip_prefix(&ctx.project_root)
        .unwrap_or(&list_path)
        .display()
        .to_string();

    let display_path = if relative_display.is_empty() {
        ".".to_string()
    } else {
        relative_display
    };

    let output = if entries.is_empty() {
        format!("{display_path}/ (empty)")
    } else {
        entries.join("\n")
    };

    Ok(ToolOutput {
        title: format!("list {display_path}"),
        output,
        is_error: false,
    })
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("({bytes}B)")
    } else if bytes < 1024 * 1024 {
        format!("({:.1}KB)", bytes as f64 / 1024.0)
    } else {
        format!("({:.1}MB)", bytes as f64 / (1024.0 * 1024.0))
    }
}
