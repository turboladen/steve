//! Memory tool — persistent per-project knowledge store.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

pub fn tool() -> ToolEntry {
    ToolEntry {
        def: ToolDef {
            name: ToolName::Memory,
            description: "Read or update the project memory — a persistent scratchpad for \
                knowledge that should survive across sessions. Use this to record architectural \
                decisions, file purposes, patterns discovered, and other project context."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["read", "append"],
                        "description": "read: view current memory. append: add new content."
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to append (required for append action). Use markdown."
                    }
                },
                "required": ["action"]
            }),
        },
        handler: Box::new(execute),
    }
}

fn memory_path(ctx: &ToolContext) -> Option<PathBuf> {
    ctx.storage_dir.as_ref().map(|dir| dir.join("memory.md"))
}

fn execute(args: Value, ctx: ToolContext) -> Result<ToolOutput> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("read");

    let Some(path) = memory_path(&ctx) else {
        return Ok(ToolOutput {
            title: "memory".to_string(),
            output: "Error: storage directory not configured".to_string(),
            is_error: true,
        });
    };

    match action {
        "read" => {
            let content = match std::fs::File::open(&path) {
                Ok(file) => {
                    use std::io::Read;
                    let _ = file.lock_shared();
                    let mut buf = String::new();
                    (&file).read_to_string(&mut buf).unwrap_or_default();
                    let _ = file.unlock();
                    buf
                }
                Err(_) => String::new(),
            };
            if content.is_empty() {
                Ok(ToolOutput {
                    title: "memory read".to_string(),
                    output: "(empty — no project memory recorded yet)".to_string(),
                    is_error: false,
                })
            } else {
                Ok(ToolOutput {
                    title: "memory read".to_string(),
                    output: content,
                    is_error: false,
                })
            }
        }
        "append" => {
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if content.is_empty() {
                return Ok(ToolOutput {
                    title: "memory append".to_string(),
                    output: "Error: content is required for append action".to_string(),
                    is_error: true,
                });
            }
            // Ensure parent dir exists
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Append with exclusive lock for safe concurrent access
            use std::io::Write;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            file.lock()?;
            write!(&file, "\n{content}\n")?;
            let _ = file.unlock();
            Ok(ToolOutput {
                title: "memory append".to_string(),
                output: "Memory updated.".to_string(),
                is_error: false,
            })
        }
        _ => Ok(ToolOutput {
            title: "memory".to_string(),
            output: format!("Error: unknown action '{action}'. Use 'read' or 'append'."),
            is_error: true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            project_root: dir.to_path_buf(),
            storage_dir: Some(dir.to_path_buf()),
        }
    }

    #[test]
    fn read_empty_memory() {
        let dir = tempfile::tempdir().unwrap();
        let result =
            execute(serde_json::json!({"action": "read"}), test_ctx(dir.path())).unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("empty"));
    }

    #[test]
    fn append_then_read() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        execute(
            serde_json::json!({"action": "append", "content": "# Architecture\nEvent-driven TUI"}),
            ctx.clone(),
        )
        .unwrap();
        let result = execute(serde_json::json!({"action": "read"}), ctx).unwrap();
        assert!(result.output.contains("Architecture"));
        assert!(result.output.contains("Event-driven"));
    }

    #[test]
    fn append_empty_content_errors() {
        let dir = tempfile::tempdir().unwrap();
        let result = execute(
            serde_json::json!({"action": "append", "content": ""}),
            test_ctx(dir.path()),
        )
        .unwrap();
        assert!(result.is_error);
    }

    #[test]
    fn unknown_action_errors() {
        let dir = tempfile::tempdir().unwrap();
        let result =
            execute(serde_json::json!({"action": "delete"}), test_ctx(dir.path())).unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("unknown action"));
    }

    #[test]
    fn no_storage_dir_errors() {
        let ctx = ToolContext {
            project_root: std::path::PathBuf::from("/tmp"),
            storage_dir: None,
        };
        let result = execute(serde_json::json!({"action": "read"}), ctx).unwrap();
        assert!(result.is_error);
    }
}
