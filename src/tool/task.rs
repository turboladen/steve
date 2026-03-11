//! Task tool — manages persistent tasks and epics for multi-step work.

use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};
use crate::task::types::{Priority, TaskStatus, EpicStatus};
use crate::task::TaskStore;

pub fn tool() -> ToolEntry {
    ToolEntry {
        def: ToolDef {
            name: ToolName::Task,
            description: "Manage persistent tasks and epics for multi-step work. Always use this \
                FIRST when given multi-step work: create tasks for each step, then work through \
                them sequentially. Actions: create (new task), list (show tasks), update (change \
                status/fields), complete (mark done), show (details), delete, create_epic (new \
                epic), list_epics, update_epic."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create", "list", "update", "complete", "show", "delete",
                                 "create_epic", "list_epics", "update_epic"],
                        "description": "The action to perform."
                    },
                    "title": {
                        "type": "string",
                        "description": "Title for new task or epic."
                    },
                    "description": {
                        "type": "string",
                        "description": "Description for new task or epic."
                    },
                    "id": {
                        "type": "string",
                        "description": "Task or epic ID (for update, complete, show, delete)."
                    },
                    "epic_id": {
                        "type": "string",
                        "description": "Parent epic ID (for creating a task under an epic)."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Session ID to scope task to current session."
                    },
                    "priority": {
                        "type": "string",
                        "enum": ["high", "medium", "low"],
                        "description": "Task priority (default: medium)."
                    },
                    "status": {
                        "type": "string",
                        "enum": ["open", "in_progress", "done"],
                        "description": "New status (for update action)."
                    },
                    "external_ref": {
                        "type": "string",
                        "description": "External reference like Jira ticket (for create_epic)."
                    }
                },
                "required": ["action"]
            }),
        },
        handler: Box::new(execute),
    }
}

/// Parse a priority string. Returns `None` for unrecognized values;
/// callers should default to Medium only when no priority was provided.
fn parse_priority(s: &str) -> Option<Priority> {
    match s {
        "high" => Some(Priority::High),
        "medium" => Some(Priority::Medium),
        "low" => Some(Priority::Low),
        _ => None,
    }
}

/// Parse a task status string.
fn parse_task_status(s: &str) -> Option<TaskStatus> {
    match s {
        "open" => Some(TaskStatus::Open),
        "in_progress" | "inprogress" => Some(TaskStatus::InProgress),
        "done" => Some(TaskStatus::Done),
        _ => None,
    }
}

/// Parse an epic status string.
fn parse_epic_status(s: &str) -> Option<EpicStatus> {
    match s {
        "open" => Some(EpicStatus::Open),
        "in_progress" | "inprogress" => Some(EpicStatus::InProgress),
        "done" => Some(EpicStatus::Done),
        _ => None,
    }
}

/// Helper to get a required string arg.
fn require_str<'a>(args: &'a Value, field: &str, action: &str) -> std::result::Result<&'a str, ToolOutput> {
    args.get(field)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolOutput {
            title: format!("task: {action}"),
            output: format!("Error: '{field}' is required for {action} action."),
            is_error: true,
        })
}

fn execute(args: Value, ctx: ToolContext) -> Result<ToolOutput> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("list");

    let Some(store) = ctx.task_store.as_ref() else {
        return Ok(ToolOutput {
            title: "task".to_string(),
            output: "Error: task store not configured".to_string(),
            is_error: true,
        });
    };

    match action {
        "create" => action_create(&args, store),
        "list" => action_list(&args, store),
        "update" => action_update(&args, store),
        "complete" => action_complete(&args, store),
        "show" => action_show(&args, store),
        "delete" => action_delete(&args, store),
        "create_epic" => action_create_epic(&args, store),
        "list_epics" => action_list_epics(store),
        "update_epic" => action_update_epic(&args, store),
        _ => Ok(ToolOutput {
            title: "task".to_string(),
            output: format!(
                "Error: unknown action '{action}'. Use create, list, update, complete, \
                 show, delete, create_epic, list_epics, or update_epic."
            ),
            is_error: true,
        }),
    }
}

// ── Action handlers ──

fn action_create(args: &Value, store: &Arc<TaskStore>) -> Result<ToolOutput> {
    let title = match require_str(args, "title", "create") {
        Ok(t) => t,
        Err(e) => return Ok(e),
    };

    let priority = match args.get("priority").and_then(|v| v.as_str()) {
        Some(s) => match parse_priority(s) {
            Some(p) => p,
            None => return Ok(ToolOutput {
                title: "task: create".to_string(),
                output: format!("Error: invalid priority '{s}'. Use high, medium, or low."),
                is_error: true,
            }),
        },
        None => Priority::default(),
    };
    let description = args.get("description").and_then(|v| v.as_str());
    let epic_id = args.get("epic_id").and_then(|v| v.as_str());
    let session_id = args.get("session_id").and_then(|v| v.as_str());

    let task = store.create_task(title, description, epic_id, session_id, priority)?;

    let mut msg = format!("Created task {}: {} [{}]", task.id, task.title, task.priority);
    if let Some(eid) = &task.epic_id {
        msg.push_str(&format!(" (epic: {eid})"));
    }
    Ok(ToolOutput {
        title: "task: create".to_string(),
        output: msg,
        is_error: false,
    })
}

fn action_list(args: &Value, store: &Arc<TaskStore>) -> Result<ToolOutput> {
    let tasks = store.list_tasks()?;

    // Optional filters
    let filter_epic = args.get("epic_id").and_then(|v| v.as_str());
    let filter_session = args.get("session_id").and_then(|v| v.as_str());

    let filtered: Vec<_> = tasks
        .iter()
        .filter(|t| {
            if let Some(eid) = filter_epic {
                if t.epic_id.as_deref() != Some(eid) {
                    return false;
                }
            }
            if let Some(sid) = filter_session {
                if t.session_id.as_deref() != Some(sid) {
                    return false;
                }
            }
            true
        })
        .collect();

    if filtered.is_empty() {
        return Ok(ToolOutput {
            title: "task: list".to_string(),
            output: "No tasks found.".to_string(),
            is_error: false,
        });
    }

    let open_count = filtered.iter().filter(|t| t.status != TaskStatus::Done).count();
    let epics = store.list_epics().unwrap_or_default();

    let mut lines = vec![format!("## Tasks ({open_count} open)")];
    for task in &filtered {
        let marker = if task.status == TaskStatus::Done { "x" } else { " " };
        let mut line = format!("- [{marker}] {}: {} [{}]", task.id, task.title, task.priority);
        if task.status == TaskStatus::InProgress {
            line.push_str(" *in progress*");
        }
        if let Some(eid) = &task.epic_id {
            let epic_title = epics.iter()
                .find(|e| e.id == *eid)
                .map(|e| e.title.as_str())
                .unwrap_or(eid);
            line.push_str(&format!(" (epic: {epic_title})"));
        }
        lines.push(line);
    }

    Ok(ToolOutput {
        title: "task: list".to_string(),
        output: lines.join("\n"),
        is_error: false,
    })
}

fn action_update(args: &Value, store: &Arc<TaskStore>) -> Result<ToolOutput> {
    let id = match require_str(args, "id", "update") {
        Ok(id) => id,
        Err(e) => return Ok(e),
    };

    let mut task = match store.get_task(id) {
        Ok(t) => t,
        Err(_) => {
            return Ok(ToolOutput {
                title: "task: update".to_string(),
                output: format!("Error: task '{id}' not found."),
                is_error: true,
            });
        }
    };

    let mut changed = Vec::new();

    if let Some(title) = args.get("title").and_then(|v| v.as_str()) {
        task.title = title.to_string();
        changed.push("title");
    }
    if let Some(desc) = args.get("description").and_then(|v| v.as_str()) {
        task.description = Some(desc.to_string());
        changed.push("description");
    }
    if let Some(status_str) = args.get("status").and_then(|v| v.as_str()) {
        if let Some(status) = parse_task_status(status_str) {
            task.status = status;
            changed.push("status");
        } else {
            return Ok(ToolOutput {
                title: "task: update".to_string(),
                output: format!("Error: invalid status '{status_str}'. Use open, inprogress, or done."),
                is_error: true,
            });
        }
    }
    if let Some(priority_str) = args.get("priority").and_then(|v| v.as_str()) {
        match parse_priority(priority_str) {
            Some(p) => {
                task.priority = p;
                changed.push("priority");
            }
            None => return Ok(ToolOutput {
                title: "task: update".to_string(),
                output: format!("Error: invalid priority '{priority_str}'. Use high, medium, or low."),
                is_error: true,
            }),
        }
    }

    if changed.is_empty() {
        return Ok(ToolOutput {
            title: "task: update".to_string(),
            output: "No fields to update.".to_string(),
            is_error: true,
        });
    }

    store.update_task(&mut task)?;

    Ok(ToolOutput {
        title: "task: update".to_string(),
        output: format!("Updated task {id}: changed {}.", changed.join(", ")),
        is_error: false,
    })
}

fn action_complete(args: &Value, store: &Arc<TaskStore>) -> Result<ToolOutput> {
    let id = match require_str(args, "id", "complete") {
        Ok(id) => id,
        Err(e) => return Ok(e),
    };

    match store.complete_task(id) {
        Ok(task) => Ok(ToolOutput {
            title: "task: complete".to_string(),
            output: format!("Completed task {}: {}", task.id, task.title),
            is_error: false,
        }),
        Err(_) => Ok(ToolOutput {
            title: "task: complete".to_string(),
            output: format!("Error: task '{id}' not found."),
            is_error: true,
        }),
    }
}

fn action_show(args: &Value, store: &Arc<TaskStore>) -> Result<ToolOutput> {
    let id = match require_str(args, "id", "show") {
        Ok(id) => id,
        Err(e) => return Ok(e),
    };

    let task = match store.get_task(id) {
        Ok(t) => t,
        Err(_) => {
            return Ok(ToolOutput {
                title: "task: show".to_string(),
                output: format!("Error: task '{id}' not found."),
                is_error: true,
            });
        }
    };

    let mut lines = vec![
        format!("ID: {}", task.id),
        format!("Title: {}", task.title),
        format!("Status: {}", task.status),
        format!("Priority: {}", task.priority),
    ];

    if let Some(eid) = &task.epic_id {
        let epic_label = store.get_epic(eid).ok()
            .map(|e| format!("{eid} ({})", e.title))
            .unwrap_or_else(|| eid.clone());
        lines.push(format!("Epic: {epic_label}"));
    }
    if let Some(sid) = &task.session_id {
        lines.push(format!("Session: {sid}"));
    }
    if let Some(desc) = &task.description {
        lines.push(format!("Description: {desc}"));
    }
    lines.push(format!("Created: {}", task.created_at.format("%Y-%m-%d")));
    lines.push(format!("Updated: {}", task.updated_at.format("%Y-%m-%d")));

    Ok(ToolOutput {
        title: "task: show".to_string(),
        output: lines.join("\n"),
        is_error: false,
    })
}

fn action_delete(args: &Value, store: &Arc<TaskStore>) -> Result<ToolOutput> {
    let id = match require_str(args, "id", "delete") {
        Ok(id) => id,
        Err(e) => return Ok(e),
    };

    // Check existence first
    if store.get_task(id).is_err() {
        return Ok(ToolOutput {
            title: "task: delete".to_string(),
            output: format!("Error: task '{id}' not found."),
            is_error: true,
        });
    }

    store.delete_task(id)?;
    Ok(ToolOutput {
        title: "task: delete".to_string(),
        output: format!("Deleted task '{id}'."),
        is_error: false,
    })
}

fn action_create_epic(args: &Value, store: &Arc<TaskStore>) -> Result<ToolOutput> {
    let title = match require_str(args, "title", "create_epic") {
        Ok(t) => t,
        Err(e) => return Ok(e),
    };
    let description = args.get("description").and_then(|v| v.as_str()).unwrap_or("");
    let external_ref = args.get("external_ref").and_then(|v| v.as_str());
    let priority = match args.get("priority").and_then(|v| v.as_str()) {
        Some(s) => match parse_priority(s) {
            Some(p) => p,
            None => return Ok(ToolOutput {
                title: "task: create_epic".to_string(),
                output: format!("Error: invalid priority '{s}'. Use high, medium, or low."),
                is_error: true,
            }),
        },
        None => Priority::default(),
    };

    let epic = store.create_epic(title, description, external_ref, priority)?;

    let mut msg = format!("Created epic {}: {} [{}]", epic.id, epic.title, epic.priority);
    if let Some(ext) = &epic.external_ref {
        msg.push_str(&format!(" (ref: {ext})"));
    }
    Ok(ToolOutput {
        title: "task: create_epic".to_string(),
        output: msg,
        is_error: false,
    })
}

fn action_list_epics(store: &Arc<TaskStore>) -> Result<ToolOutput> {
    let epics = store.list_epics()?;

    if epics.is_empty() {
        return Ok(ToolOutput {
            title: "task: list_epics".to_string(),
            output: "No epics found.".to_string(),
            is_error: false,
        });
    }

    let tasks = store.list_tasks().unwrap_or_default();

    let mut lines = vec![format!("## Epics ({})", epics.len())];
    for epic in &epics {
        let task_count = tasks.iter().filter(|t| t.epic_id.as_deref() == Some(&epic.id)).count();
        let done_count = tasks.iter()
            .filter(|t| t.epic_id.as_deref() == Some(&epic.id) && t.status == TaskStatus::Done)
            .count();
        let mut line = format!(
            "- {}: {} [{}] ({done_count}/{task_count} tasks done)",
            epic.id, epic.title, epic.priority
        );
        if epic.status != EpicStatus::Open {
            line.push_str(&format!(" *{}*", epic.status));
        }
        if let Some(ext) = &epic.external_ref {
            line.push_str(&format!(" ref: {ext}"));
        }
        lines.push(line);
    }

    Ok(ToolOutput {
        title: "task: list_epics".to_string(),
        output: lines.join("\n"),
        is_error: false,
    })
}

fn action_update_epic(args: &Value, store: &Arc<TaskStore>) -> Result<ToolOutput> {
    let id = match require_str(args, "id", "update_epic") {
        Ok(id) => id,
        Err(e) => return Ok(e),
    };

    let mut epic = match store.get_epic(id) {
        Ok(e) => e,
        Err(_) => {
            return Ok(ToolOutput {
                title: "task: update_epic".to_string(),
                output: format!("Error: epic '{id}' not found."),
                is_error: true,
            });
        }
    };

    let mut changed = Vec::new();

    if let Some(title) = args.get("title").and_then(|v| v.as_str()) {
        epic.title = title.to_string();
        changed.push("title");
    }
    if let Some(desc) = args.get("description").and_then(|v| v.as_str()) {
        epic.description = desc.to_string();
        changed.push("description");
    }
    if let Some(status_str) = args.get("status").and_then(|v| v.as_str()) {
        if let Some(status) = parse_epic_status(status_str) {
            epic.status = status;
            changed.push("status");
        } else {
            return Ok(ToolOutput {
                title: "task: update_epic".to_string(),
                output: format!("Error: invalid status '{status_str}'. Use open, inprogress, or done."),
                is_error: true,
            });
        }
    }
    if let Some(priority_str) = args.get("priority").and_then(|v| v.as_str()) {
        match parse_priority(priority_str) {
            Some(p) => {
                epic.priority = p;
                changed.push("priority");
            }
            None => return Ok(ToolOutput {
                title: "task: update_epic".to_string(),
                output: format!("Error: invalid priority '{priority_str}'. Use high, medium, or low."),
                is_error: true,
            }),
        }
    }
    if let Some(ext) = args.get("external_ref").and_then(|v| v.as_str()) {
        epic.external_ref = Some(ext.to_string());
        changed.push("external_ref");
    }

    if changed.is_empty() {
        return Ok(ToolOutput {
            title: "task: update_epic".to_string(),
            output: "No fields to update.".to_string(),
            is_error: true,
        });
    }

    store.update_epic(&mut epic)?;

    Ok(ToolOutput {
        title: "task: update_epic".to_string(),
        output: format!("Updated epic {id}: changed {}.", changed.join(", ")),
        is_error: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use tempfile::tempdir;

    fn test_ctx() -> (ToolContext, tempfile::TempDir) {
        let dir = tempdir().expect("temp dir");
        let storage = Storage::with_base(dir.path().to_path_buf()).expect("storage");
        let store = TaskStore::new(storage);
        let ctx = ToolContext {
            project_root: dir.path().to_path_buf(),
            storage_dir: None,
            task_store: Some(Arc::new(store)),
        };
        (ctx, dir)
    }

    // ── parse_priority ──

    #[test]
    fn parse_priority_valid() {
        assert_eq!(parse_priority("high"), Some(Priority::High));
        assert_eq!(parse_priority("medium"), Some(Priority::Medium));
        assert_eq!(parse_priority("low"), Some(Priority::Low));
    }

    #[test]
    fn parse_priority_rejects_invalid() {
        assert_eq!(parse_priority("critical"), None);
        assert_eq!(parse_priority("urgent"), None);
        assert_eq!(parse_priority(""), None);
    }

    // ── parse_task_status ──

    #[test]
    fn parse_task_status_valid() {
        assert_eq!(parse_task_status("open"), Some(TaskStatus::Open));
        assert_eq!(parse_task_status("in_progress"), Some(TaskStatus::InProgress));
        assert_eq!(parse_task_status("inprogress"), Some(TaskStatus::InProgress));
        assert_eq!(parse_task_status("done"), Some(TaskStatus::Done));
    }

    #[test]
    fn parse_task_status_rejects_invalid() {
        assert_eq!(parse_task_status("blocked"), None);
        assert_eq!(parse_task_status(""), None);
    }

    // ── parse_epic_status ──

    #[test]
    fn parse_epic_status_valid() {
        assert_eq!(parse_epic_status("open"), Some(EpicStatus::Open));
        assert_eq!(parse_epic_status("in_progress"), Some(EpicStatus::InProgress));
        assert_eq!(parse_epic_status("inprogress"), Some(EpicStatus::InProgress));
        assert_eq!(parse_epic_status("done"), Some(EpicStatus::Done));
    }

    #[test]
    fn parse_epic_status_rejects_invalid() {
        assert_eq!(parse_epic_status("closed"), None);
    }

    // ── require_str ──

    #[test]
    fn require_str_present() {
        let args = serde_json::json!({"title": "hello"});
        assert_eq!(require_str(&args, "title", "create").unwrap(), "hello");
    }

    #[test]
    fn require_str_missing() {
        let args = serde_json::json!({});
        let err = require_str(&args, "title", "create").unwrap_err();
        assert!(err.is_error);
        assert!(err.output.contains("'title'"));
    }

    #[test]
    fn require_str_empty() {
        let args = serde_json::json!({"title": ""});
        let err = require_str(&args, "title", "create").unwrap_err();
        assert!(err.is_error);
    }

    // ── action: create ──

    #[test]
    fn create_task_round_trip() {
        let (ctx, _dir) = test_ctx();
        let result = execute(
            serde_json::json!({"action": "create", "title": "Fix bug"}),
            ctx,
        ).unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("Fix bug"));
        assert!(result.output.contains("task-"));
    }

    #[test]
    fn create_task_missing_title() {
        let (ctx, _dir) = test_ctx();
        let result = execute(
            serde_json::json!({"action": "create"}),
            ctx,
        ).unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("'title'"));
    }

    #[test]
    fn create_task_invalid_priority() {
        let (ctx, _dir) = test_ctx();
        let result = execute(
            serde_json::json!({"action": "create", "title": "Test", "priority": "critical"}),
            ctx,
        ).unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("invalid priority"));
    }

    // ── action: list ──

    #[test]
    fn list_empty() {
        let (ctx, _dir) = test_ctx();
        let result = execute(serde_json::json!({"action": "list"}), ctx).unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("No tasks"));
    }

    #[test]
    fn list_shows_created_tasks() {
        let (ctx, _dir) = test_ctx();
        execute(
            serde_json::json!({"action": "create", "title": "Task A"}),
            ctx.clone(),
        ).unwrap();
        execute(
            serde_json::json!({"action": "create", "title": "Task B"}),
            ctx.clone(),
        ).unwrap();
        let result = execute(serde_json::json!({"action": "list"}), ctx).unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("Task A"));
        assert!(result.output.contains("Task B"));
        assert!(result.output.contains("2 open"));
    }

    // ── action: complete ──

    #[test]
    fn complete_task_success() {
        let (ctx, _dir) = test_ctx();
        let create_result = execute(
            serde_json::json!({"action": "create", "title": "Completable"}),
            ctx.clone(),
        ).unwrap();
        // Extract task ID from output
        let id = create_result.output.split(':').next().unwrap()
            .replace("Created task ", "").trim().to_string();

        let result = execute(
            serde_json::json!({"action": "complete", "id": id}),
            ctx,
        ).unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("Completed"));
    }

    #[test]
    fn complete_nonexistent_task() {
        let (ctx, _dir) = test_ctx();
        let result = execute(
            serde_json::json!({"action": "complete", "id": "task-nope"}),
            ctx,
        ).unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("not found"));
    }

    // ── action: show ──

    #[test]
    fn show_nonexistent_task() {
        let (ctx, _dir) = test_ctx();
        let result = execute(
            serde_json::json!({"action": "show", "id": "task-missing"}),
            ctx,
        ).unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("not found"));
    }

    // ── action: delete ──

    #[test]
    fn delete_nonexistent_task() {
        let (ctx, _dir) = test_ctx();
        let result = execute(
            serde_json::json!({"action": "delete", "id": "task-nope"}),
            ctx,
        ).unwrap();
        assert!(result.is_error);
    }

    // ── action: update ──

    #[test]
    fn update_invalid_priority_errors() {
        let (ctx, _dir) = test_ctx();
        let create = execute(
            serde_json::json!({"action": "create", "title": "Updatable"}),
            ctx.clone(),
        ).unwrap();
        let id = create.output.split(':').next().unwrap()
            .replace("Created task ", "").trim().to_string();

        let result = execute(
            serde_json::json!({"action": "update", "id": id, "priority": "urgent"}),
            ctx,
        ).unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("invalid priority"));
    }

    #[test]
    fn update_no_fields_errors() {
        let (ctx, _dir) = test_ctx();
        let create = execute(
            serde_json::json!({"action": "create", "title": "NoChange"}),
            ctx.clone(),
        ).unwrap();
        let id = create.output.split(':').next().unwrap()
            .replace("Created task ", "").trim().to_string();

        let result = execute(
            serde_json::json!({"action": "update", "id": id}),
            ctx,
        ).unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("No fields"));
    }

    // ── action: create_epic ──

    #[test]
    fn create_epic_success() {
        let (ctx, _dir) = test_ctx();
        let result = execute(
            serde_json::json!({"action": "create_epic", "title": "Big Feature"}),
            ctx,
        ).unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("Big Feature"));
        assert!(result.output.contains("epic-"));
    }

    #[test]
    fn create_epic_invalid_priority() {
        let (ctx, _dir) = test_ctx();
        let result = execute(
            serde_json::json!({"action": "create_epic", "title": "Test", "priority": "asap"}),
            ctx,
        ).unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("invalid priority"));
    }

    // ── action: list_epics ──

    #[test]
    fn list_epics_empty() {
        let (ctx, _dir) = test_ctx();
        let result = execute(serde_json::json!({"action": "list_epics"}), ctx).unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("No epics"));
    }

    // ── unknown action ──

    #[test]
    fn unknown_action_errors() {
        let (ctx, _dir) = test_ctx();
        let result = execute(
            serde_json::json!({"action": "fly"}),
            ctx,
        ).unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("unknown action"));
    }

    // ── no task store ──

    #[test]
    fn no_task_store_errors() {
        let ctx = ToolContext {
            project_root: std::path::PathBuf::from("/tmp"),
            storage_dir: None,
            task_store: None,
        };
        let result = execute(serde_json::json!({"action": "list"}), ctx).unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("not configured"));
    }
}
