//! Todo tool — manages a todo list rendered in the sidebar.

use anyhow::{Context, Result};
use serde_json::Value;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

pub fn tool() -> ToolEntry {
    let def_json = definition();
    let func = def_json.get("function").unwrap();
    ToolEntry {
        def: ToolDef {
            name: ToolName::Todo,
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

/// Actions available for the todo tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TodoAction {
    Add,
    Complete,
    Remove,
    List,
    Clear,
}

impl std::str::FromStr for TodoAction {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "add" => Ok(TodoAction::Add),
            "complete" => Ok(TodoAction::Complete),
            "remove" => Ok(TodoAction::Remove),
            "list" => Ok(TodoAction::List),
            "clear" => Ok(TodoAction::Clear),
            _ => Err(format!(
                "Unknown action: {s}. Use add, complete, remove, list, or clear."
            )),
        }
    }
}

/// Get a snapshot of the current todos.
pub fn get_todos() -> Vec<TodoItem> {
    TODOS.lock().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_action_from_str_valid() {
        assert_eq!("add".parse::<TodoAction>().unwrap(), TodoAction::Add);
        assert_eq!("complete".parse::<TodoAction>().unwrap(), TodoAction::Complete);
        assert_eq!("remove".parse::<TodoAction>().unwrap(), TodoAction::Remove);
        assert_eq!("list".parse::<TodoAction>().unwrap(), TodoAction::List);
        assert_eq!("clear".parse::<TodoAction>().unwrap(), TodoAction::Clear);
    }

    #[test]
    fn todo_action_from_str_invalid() {
        assert!("unknown".parse::<TodoAction>().is_err());
        assert!("ADD".parse::<TodoAction>().is_err()); // case-sensitive
        assert!("".parse::<TodoAction>().is_err());
    }

    #[test]
    fn todo_action_error_message_is_helpful() {
        let err = "bogus".parse::<TodoAction>().unwrap_err();
        assert!(err.contains("bogus"), "error should include the bad input");
        assert!(err.contains("add"), "error should list valid actions");
    }
}

fn execute(args: Value, _ctx: ToolContext) -> Result<ToolOutput> {
    let action_str = args
        .get("action")
        .and_then(|v| v.as_str())
        .context("missing 'action' parameter")?;

    let action = match action_str.parse::<TodoAction>() {
        Ok(a) => a,
        Err(msg) => {
            return Ok(ToolOutput {
                title: "Todo".to_string(),
                output: msg,
                is_error: true,
            });
        }
    };

    let mut todos = TODOS.lock().unwrap();

    match action {
        TodoAction::Add => {
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
        TodoAction::Complete => {
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
        TodoAction::Remove => {
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
        TodoAction::List => {
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
        TodoAction::Clear => {
            let count = todos.len();
            todos.clear();
            Ok(ToolOutput {
                title: "Todo: clear".to_string(),
                output: format!("Cleared {count} todos."),
                is_error: false,
            })
        }
    }
}
