//! Memory tool — persistent per-project knowledge store.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

/// Maximum memory size in bytes. Older content is truncated when exceeded.
const MAX_MEMORY_BYTES: usize = 4096;

pub fn tool() -> ToolEntry {
    ToolEntry {
        def: ToolDef {
            name: ToolName::Memory,
            description: "Read or update the project memory — a persistent scratchpad for \
                knowledge that should survive across sessions. Use this to record architectural \
                decisions, file purposes, patterns discovered, and other project context. \
                Memory is auto-loaded into your context at session start. Use 'replace' to \
                consolidate when memory grows long."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["read", "append", "replace"],
                        "description": "read: view current memory. append: add new content. replace: overwrite entire memory with new content (use to consolidate/curate)."
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write (required for append and replace actions). Use markdown."
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

            // Prune if over size limit
            let pruned = prune_if_needed(&path);

            let msg = if pruned {
                "Memory updated. (Oldest entries pruned to stay within size limit.)"
            } else {
                "Memory updated."
            };
            Ok(ToolOutput {
                title: "memory append".to_string(),
                output: msg.to_string(),
                is_error: false,
            })
        }
        "replace" => {
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if content.is_empty() {
                return Ok(ToolOutput {
                    title: "memory replace".to_string(),
                    output: "Error: content is required for replace action".to_string(),
                    is_error: true,
                });
            }
            // Ensure parent dir exists
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Write with exclusive lock
            use std::io::Write;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&path)?;
            file.lock()?;
            write!(&file, "{content}\n")?;
            let _ = file.unlock();

            Ok(ToolOutput {
                title: "memory replace".to_string(),
                output: format!("Memory replaced ({} bytes).", content.len()),
                is_error: false,
            })
        }
        _ => Ok(ToolOutput {
            title: "memory".to_string(),
            output: format!("Error: unknown action '{action}'. Use 'read', 'append', or 'replace'."),
            is_error: true,
        }),
    }
}

/// If the memory file exceeds MAX_MEMORY_BYTES, truncate from the beginning
/// (removing oldest entries) to fit. Returns true if pruning occurred.
fn prune_if_needed(path: &std::path::Path) -> bool {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    if content.len() <= MAX_MEMORY_BYTES {
        return false;
    }

    // Find the first line boundary after the excess
    let excess = content.len() - MAX_MEMORY_BYTES;
    let start = content[excess..]
        .find('\n')
        .map(|i| excess + i + 1)
        .unwrap_or(excess);

    let truncated = &content[start..];
    let _ = std::fs::write(path, truncated);
    true
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
            execute(serde_json::json!({"action": "badaction"}), test_ctx(dir.path())).unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("unknown action"));
    }

    #[test]
    fn replace_overwrites_memory() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        execute(
            serde_json::json!({"action": "append", "content": "old stuff"}),
            ctx.clone(),
        )
        .unwrap();
        execute(
            serde_json::json!({"action": "replace", "content": "# Fresh\nNew content only"}),
            ctx.clone(),
        )
        .unwrap();
        let result = execute(serde_json::json!({"action": "read"}), ctx).unwrap();
        assert!(!result.output.contains("old stuff"), "old content should be gone");
        assert!(result.output.contains("New content only"));
    }

    #[test]
    fn replace_empty_content_errors() {
        let dir = tempfile::tempdir().unwrap();
        let result = execute(
            serde_json::json!({"action": "replace", "content": ""}),
            test_ctx(dir.path()),
        )
        .unwrap();
        assert!(result.is_error);
    }

    #[test]
    fn prune_truncates_oversized_memory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.md");
        // Write more than MAX_MEMORY_BYTES
        let big_content = "x".repeat(MAX_MEMORY_BYTES + 500);
        std::fs::write(&path, &big_content).unwrap();
        assert!(prune_if_needed(&path));
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.len() <= MAX_MEMORY_BYTES, "should be pruned to fit");
    }

    #[test]
    fn prune_does_nothing_when_small() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.md");
        std::fs::write(&path, "small content").unwrap();
        assert!(!prune_if_needed(&path));
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
