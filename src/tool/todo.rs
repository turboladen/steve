//! Todo tool — manages a todo list rendered in the sidebar.

use anyhow::{Context, Result};
use serde_json::Value;

use super::{ToolContext, ToolDef, ToolEntry, ToolOutput};

pub fn tool() -> ToolEntry {
    let def_json = definition();
    let func = def_json.get("function").unwrap();
    ToolEntry {
        def: ToolDef {
            name: "todo".to_string(),
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
            "name": "todo",
            "description": "Manage a todo list that is displayed in the sidebar. You can add, complete, or clear items. Use this to track progress on multi-step tasks.",
            "parameters": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["add", "complete", "remove", "list", "clear"],
                        "description": "The action to perform on the todo list."
                    },
                    "text": {
                        "type": "string",
                        "description": "The todo item text (for add action)."
                    },
                    "index": {
                        "type": "integer",
                        "description": "The index of the todo item (for complete/remove actions, 0-based)."
                    }
                },
                "required": ["action"]
            }
        }
    })
}

/// Shared todo list state. In a real implementation this would be managed
/// through events, but for simplicity we use a static mutex.
use std::sync::Mutex;

static TODOS: Mutex<Vec<TodoItem>> = Mutex::new(Vec::new());

#[derive(Debug, Clone)]
pub struct TodoItem {
    pub text: String,
    pub done: bool,
}

/// Get a snapshot of the current todos.
pub fn get_todos() -> Vec<TodoItem> {
    TODOS.lock().unwrap().clone()
}

fn execute(args: Value, _ctx: ToolContext) -> Result<ToolOutput> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .context("missing 'action' parameter")?;

    let mut todos = TODOS.lock().unwrap();

    match action {
        "add" => {
            let text = args
                .get("text")
                .and_then(|v| v.as_str())
                .context("missing 'text' parameter for add action")?;
            todos.push(TodoItem {
                text: text.to_string(),
                done: false,
            });
            Ok(ToolOutput {
                title: "Todo: add".to_string(),
                output: format!("Added todo: {text}"),
                is_error: false,
            })
        }
        "complete" => {
            let index = args
                .get("index")
                .and_then(|v| v.as_u64())
                .context("missing 'index' parameter for complete action")? as usize;
            if index >= todos.len() {
                Ok(ToolOutput {
                    title: "Todo: complete".to_string(),
                    output: format!("Invalid index: {index}. Todo list has {} items.", todos.len()),
                    is_error: true,
                })
            } else {
                todos[index].done = true;
                Ok(ToolOutput {
                    title: "Todo: complete".to_string(),
                    output: format!("Completed: {}", todos[index].text),
                    is_error: false,
                })
            }
        }
        "remove" => {
            let index = args
                .get("index")
                .and_then(|v| v.as_u64())
                .context("missing 'index' parameter for remove action")? as usize;
            if index >= todos.len() {
                Ok(ToolOutput {
                    title: "Todo: remove".to_string(),
                    output: format!("Invalid index: {index}. Todo list has {} items.", todos.len()),
                    is_error: true,
                })
            } else {
                let removed = todos.remove(index);
                Ok(ToolOutput {
                    title: "Todo: remove".to_string(),
                    output: format!("Removed: {}", removed.text),
                    is_error: false,
                })
            }
        }
        "list" => {
            if todos.is_empty() {
                Ok(ToolOutput {
                    title: "Todo: list".to_string(),
                    output: "No todos.".to_string(),
                    is_error: false,
                })
            } else {
                let list = todos
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let marker = if t.done { "✓" } else { "○" };
                        format!("{i}. {marker} {}", t.text)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(ToolOutput {
                    title: "Todo: list".to_string(),
                    output: list,
                    is_error: false,
                })
            }
        }
        "clear" => {
            let count = todos.len();
            todos.clear();
            Ok(ToolOutput {
                title: "Todo: clear".to_string(),
                output: format!("Cleared {count} todos."),
                is_error: false,
            })
        }
        _ => Ok(ToolOutput {
            title: "Todo".to_string(),
            output: format!("Unknown action: {action}. Use add, complete, remove, list, or clear."),
            is_error: true,
        }),
    }
}
