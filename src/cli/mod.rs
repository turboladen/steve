use std::str::FromStr;

use anyhow::{Result, bail};
use clap::Subcommand;

use crate::task::{
    Epic, EpicStatus, Priority, Task, TaskKind, TaskStatus, TaskStore,
};

/// Subcommands under `steve task`.
#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    /// List tasks and epics
    List {
        /// Show only tasks
        #[arg(long)]
        tasks: bool,
        /// Show only epics
        #[arg(long)]
        epics: bool,
        /// Filter by status (open, in_progress, done)
        #[arg(short, long)]
        status: Option<String>,
        /// Filter tasks by epic ID
        #[arg(short, long)]
        epic: Option<String>,
    },
    /// Show details of a task or epic
    Show {
        /// Task or epic ID (auto-detected by prefix)
        id: String,
    },
    /// Create a new task (or epic with --epic, bug with --bug)
    Create {
        /// Title for the task or epic
        title: String,
        /// Create an epic instead of a task
        #[arg(long, conflicts_with = "bug")]
        epic: bool,
        /// Create a bug report instead of a task
        #[arg(long, conflicts_with = "epic")]
        bug: bool,
        /// Description
        #[arg(short, long)]
        description: Option<String>,
        /// Parent epic ID (tasks only)
        #[arg(short = 'E', long)]
        epic_id: Option<String>,
        /// Priority: high, medium, low
        #[arg(short, long, default_value = "medium")]
        priority: String,
        /// External reference URL (epics only)
        #[arg(short = 'r', long)]
        external_ref: Option<String>,
    },
    /// Mark a task or epic as done
    Complete {
        /// Task or epic ID (auto-detected by prefix)
        id: String,
    },
    /// Delete a task or epic
    Delete {
        /// Task or epic ID (auto-detected by prefix)
        id: String,
    },
    /// Update task or epic fields
    Update {
        /// Task or epic ID (auto-detected by prefix)
        id: String,
        /// New title
        #[arg(short, long)]
        title: Option<String>,
        /// New description
        #[arg(short, long)]
        description: Option<String>,
        /// New status (open, in_progress, done)
        #[arg(short, long)]
        status: Option<String>,
        /// New priority (high, medium, low)
        #[arg(short, long)]
        priority: Option<String>,
        /// External reference URL (epics only)
        #[arg(short = 'r', long)]
        external_ref: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntityKind {
    Task,
    Epic,
}

fn detect_entity(id: &str) -> Result<EntityKind> {
    if id.starts_with("task-") || id.starts_with("bug-") {
        Ok(EntityKind::Task)
    } else if id.starts_with("epic-") {
        Ok(EntityKind::Epic)
    } else {
        bail!("unknown ID prefix: {id} (expected task-*, bug-*, or epic-*)")
    }
}

fn parse_priority(s: &str) -> Result<Priority> {
    Priority::from_str(s).map_err(|_| anyhow::anyhow!("invalid priority: {s} (expected high, medium, low)"))
}

fn parse_task_status(s: &str) -> Result<TaskStatus> {
    TaskStatus::from_str(s)
        .map_err(|_| anyhow::anyhow!("invalid status: {s} (expected open, in_progress, done)"))
}

fn parse_epic_status(s: &str) -> Result<EpicStatus> {
    EpicStatus::from_str(s)
        .map_err(|_| anyhow::anyhow!("invalid status: {s} (expected open, in_progress, done)"))
}

/// Entry point for `steve task <subcommand>`.
pub fn run_task(command: TaskCommand) -> Result<()> {
    let project_info = crate::project::detect_or_cwd();
    let storage = crate::storage::Storage::new(&project_info.id)?;
    let store = TaskStore::new(storage);

    match command {
        TaskCommand::List {
            tasks,
            epics,
            status,
            epic,
        } => cmd_list(&store, tasks, epics, status.as_deref(), epic.as_deref()),

        TaskCommand::Show { id } => cmd_show(&store, &id),

        TaskCommand::Create {
            title,
            epic,
            bug,
            description,
            epic_id,
            priority,
            external_ref,
        } => {
            let priority = parse_priority(&priority)?;
            if epic {
                cmd_create_epic(&store, &title, description.as_deref(), external_ref.as_deref(), priority)
            } else {
                let kind = if bug { TaskKind::Bug } else { TaskKind::Task };
                cmd_create_task(&store, &title, description.as_deref(), epic_id.as_deref(), priority, kind)
            }
        }

        TaskCommand::Complete { id } => cmd_complete(&store, &id),
        TaskCommand::Delete { id } => cmd_delete(&store, &id),
        TaskCommand::Update {
            id,
            title,
            description,
            status,
            priority,
            external_ref,
        } => cmd_update(&store, &id, title, description, status, priority, external_ref),
    }
}

// ── Command handlers ──

fn cmd_list(
    store: &TaskStore,
    tasks_only: bool,
    epics_only: bool,
    status_filter: Option<&str>,
    epic_filter: Option<&str>,
) -> Result<()> {
    let show_tasks = !epics_only;
    let show_epics = !tasks_only;

    let mut found_any = false;

    if show_epics && epic_filter.is_none() {
        let epics = store.list_epics()?;
        let epics: Vec<Epic> = if let Some(s) = status_filter {
            let status = parse_epic_status(s)?;
            epics.into_iter().filter(|e| e.status == status).collect()
        } else {
            epics
        };
        if !epics.is_empty() {
            print!("{}", format_epic_table(&epics, store));
            found_any = true;
        }
    }

    if show_tasks {
        let mut tasks = if let Some(eid) = epic_filter {
            store.tasks_by_epic(eid)?
        } else {
            store.list_tasks()?
        };

        if let Some(s) = status_filter {
            let status = parse_task_status(s)?;
            tasks.retain(|t| t.status == status);
        }

        if !tasks.is_empty() {
            print!("{}", format_task_table(&tasks));
            found_any = true;
        }
    }

    if !found_any {
        println!("No items found.");
    }

    Ok(())
}

fn cmd_show(store: &TaskStore, id: &str) -> Result<()> {
    match detect_entity(id)? {
        EntityKind::Task => {
            let task = store.get_task(id)?;
            print!("{}", format_task_detail(&task, store));
        }
        EntityKind::Epic => {
            let epic = store.get_epic(id)?;
            let tasks = store.tasks_by_epic(id)?;
            print!("{}", format_epic_detail(&epic, &tasks));
        }
    }
    Ok(())
}

fn cmd_create_task(
    store: &TaskStore,
    title: &str,
    description: Option<&str>,
    epic_id: Option<&str>,
    priority: Priority,
    kind: TaskKind,
) -> Result<()> {
    let task = store.create_task(title, description, epic_id, None, priority, kind)?;
    let label = match task.kind {
        TaskKind::Task => "task",
        TaskKind::Bug => "bug",
    };
    println!("Created {} {} \"{}\"", label, task.id, task.title);
    Ok(())
}

fn cmd_create_epic(
    store: &TaskStore,
    title: &str,
    description: Option<&str>,
    external_ref: Option<&str>,
    priority: Priority,
) -> Result<()> {
    let desc = description.unwrap_or("");
    let epic = store.create_epic(title, desc, external_ref, priority)?;
    println!("Created epic {} \"{}\"", epic.id, epic.title);
    Ok(())
}

fn cmd_complete(store: &TaskStore, id: &str) -> Result<()> {
    match detect_entity(id)? {
        EntityKind::Task => {
            let task = store.complete_task(id)?;
            println!("Completed task {} \"{}\"", task.id, task.title);
        }
        EntityKind::Epic => {
            let epic = store.complete_epic(id)?;
            println!("Completed epic {} \"{}\"", epic.id, epic.title);
        }
    }
    Ok(())
}

fn cmd_delete(store: &TaskStore, id: &str) -> Result<()> {
    match detect_entity(id)? {
        EntityKind::Task => {
            let task = store.get_task(id)?;
            store.delete_task(id)?;
            println!("Deleted task {} \"{}\"", task.id, task.title);
        }
        EntityKind::Epic => {
            let epic = store.get_epic(id)?;
            store.delete_epic(id)?;
            println!("Deleted epic {} \"{}\"", epic.id, epic.title);
        }
    }
    Ok(())
}

fn cmd_update(
    store: &TaskStore,
    id: &str,
    title: Option<String>,
    description: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    external_ref: Option<String>,
) -> Result<()> {
    match detect_entity(id)? {
        EntityKind::Task => {
            let mut task = store.get_task(id)?;
            if let Some(t) = title {
                task.title = t;
            }
            if let Some(d) = description {
                task.description = Some(d);
            }
            if let Some(s) = status {
                task.status = parse_task_status(&s)?;
            }
            if let Some(p) = priority {
                task.priority = parse_priority(&p)?;
            }
            store.update_task(&mut task)?;
            println!("Updated task {} \"{}\"", task.id, task.title);
        }
        EntityKind::Epic => {
            let mut epic = store.get_epic(id)?;
            if let Some(t) = title {
                epic.title = t;
            }
            if let Some(d) = description {
                epic.description = d;
            }
            if let Some(s) = status {
                epic.status = parse_epic_status(&s)?;
            }
            if let Some(p) = priority {
                epic.priority = parse_priority(&p)?;
            }
            if let Some(r) = external_ref {
                epic.external_ref = Some(r);
            }
            store.update_epic(&mut epic)?;
            println!("Updated epic {} \"{}\"", epic.id, epic.title);
        }
    }
    Ok(())
}

// ── Formatting (returns String for testability) ──

fn format_task_table(tasks: &[Task]) -> String {
    let mut out = String::new();
    out.push_str("Tasks:\n");

    // Column widths
    let id_w = tasks.iter().map(|t| t.id.len()).max().unwrap_or(4).max(2);
    let status_w = tasks
        .iter()
        .map(|t| t.status.to_string().len())
        .max()
        .unwrap_or(6)
        .max(6);
    let prio_w = 6; // "medium" is longest

    for task in tasks {
        let title_display = if task.kind == TaskKind::Bug {
            format!("[bug] {}", truncate(&task.title, 54))
        } else {
            truncate(&task.title, 60).to_string()
        };
        out.push_str(&format!(
            "  {:<id_w$}  {:<status_w$}  {:<prio_w$}  {}\n",
            task.id,
            task.status.to_string(),
            task.priority.to_string(),
            title_display,
            id_w = id_w,
            status_w = status_w,
            prio_w = prio_w,
        ));
    }
    out
}

fn format_epic_table(epics: &[Epic], store: &TaskStore) -> String {
    let mut out = String::new();
    out.push_str("Epics:\n");

    let id_w = epics.iter().map(|e| e.id.len()).max().unwrap_or(4).max(2);
    let status_w = epics
        .iter()
        .map(|e| e.status.to_string().len())
        .max()
        .unwrap_or(6)
        .max(6);

    for epic in epics {
        let task_count = store
            .tasks_by_epic(&epic.id)
            .map(|t| t.len())
            .unwrap_or(0);
        out.push_str(&format!(
            "  {:<id_w$}  {:<status_w$}  {:<6}  {} ({} tasks)\n",
            epic.id,
            epic.status.to_string(),
            epic.priority.to_string(),
            truncate(&epic.title, 50),
            task_count,
            id_w = id_w,
            status_w = status_w,
        ));
    }
    out
}

fn format_task_detail(task: &Task, store: &TaskStore) -> String {
    let mut out = String::new();
    let header = match task.kind {
        TaskKind::Task => "Task",
        TaskKind::Bug => "Bug",
    };
    out.push_str(&format!("{}: {}\n", header, task.id));
    out.push_str(&format!("  Title:       {}\n", task.title));
    if task.kind == TaskKind::Bug {
        out.push_str(&format!("  Type:        {}\n", task.kind));
    }
    out.push_str(&format!("  Status:      {}\n", task.status));
    out.push_str(&format!("  Priority:    {}\n", task.priority));
    if let Some(ref desc) = task.description {
        out.push_str(&format!("  Description: {}\n", desc));
    }
    if let Some(ref eid) = task.epic_id {
        let epic_title = store
            .get_epic(eid)
            .map(|e| e.title)
            .unwrap_or_else(|_| eid.clone());
        out.push_str(&format!("  Epic:        {} ({})\n", eid, epic_title));
    }
    if let Some(ref sid) = task.session_id {
        out.push_str(&format!("  Session:     {}\n", sid));
    }
    out.push_str(&format!("  Created:     {}\n", task.created_at.format("%Y-%m-%d %H:%M")));
    out.push_str(&format!("  Updated:     {}\n", task.updated_at.format("%Y-%m-%d %H:%M")));
    out
}

fn format_epic_detail(epic: &Epic, tasks: &[Task]) -> String {
    let mut out = String::new();
    out.push_str(&format!("Epic: {}\n", epic.id));
    out.push_str(&format!("  Title:       {}\n", epic.title));
    out.push_str(&format!("  Status:      {}\n", epic.status));
    out.push_str(&format!("  Priority:    {}\n", epic.priority));
    if !epic.description.is_empty() {
        out.push_str(&format!("  Description: {}\n", epic.description));
    }
    if let Some(ref ext) = epic.external_ref {
        out.push_str(&format!("  External:    {}\n", ext));
    }
    out.push_str(&format!("  Created:     {}\n", epic.created_at.format("%Y-%m-%d %H:%M")));
    out.push_str(&format!("  Updated:     {}\n", epic.updated_at.format("%Y-%m-%d %H:%M")));

    if !tasks.is_empty() {
        out.push_str(&format!("  Tasks ({}):\n", tasks.len()));
        for task in tasks {
            let check = if task.status == TaskStatus::Done { "x" } else { " " };
            out.push_str(&format!("    [{}] {} — {}\n", check, task.id, task.title));
        }
    }
    out
}

fn truncate(s: &str, max_chars: usize) -> &str {
    if s.chars().count() <= max_chars {
        s
    } else {
        // Find byte offset of the max_chars-th character
        let byte_end = s
            .char_indices()
            .nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        &s[..byte_end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_store() -> (TaskStore, tempfile::TempDir) {
        let dir = tempdir().expect("temp dir");
        let storage =
            crate::storage::Storage::with_base(dir.path().to_path_buf()).expect("storage");
        (TaskStore::new(storage), dir)
    }

    // ── detect_entity ──

    #[test]
    fn detect_entity_task() {
        assert_eq!(detect_entity("task-abc123").unwrap(), EntityKind::Task);
    }

    #[test]
    fn detect_entity_epic() {
        assert_eq!(detect_entity("epic-abc123").unwrap(), EntityKind::Epic);
    }

    #[test]
    fn detect_entity_bug() {
        assert_eq!(detect_entity("bug-abc123").unwrap(), EntityKind::Task);
    }

    #[test]
    fn detect_entity_unknown() {
        assert!(detect_entity("foo-123").is_err());
        assert!(detect_entity("").is_err());
        assert!(detect_entity("taskfoo").is_err());
    }

    // ── parse helpers ──

    #[test]
    fn parse_priority_valid() {
        assert_eq!(parse_priority("high").unwrap(), Priority::High);
        assert_eq!(parse_priority("medium").unwrap(), Priority::Medium);
        assert_eq!(parse_priority("low").unwrap(), Priority::Low);
    }

    #[test]
    fn parse_priority_invalid() {
        assert!(parse_priority("urgent").is_err());
        assert!(parse_priority("").is_err());
    }

    #[test]
    fn parse_task_status_valid() {
        assert_eq!(parse_task_status("open").unwrap(), TaskStatus::Open);
        assert_eq!(parse_task_status("in_progress").unwrap(), TaskStatus::InProgress);
        assert_eq!(parse_task_status("done").unwrap(), TaskStatus::Done);
    }

    #[test]
    fn parse_task_status_invalid() {
        assert!(parse_task_status("blocked").is_err());
    }

    #[test]
    fn parse_epic_status_valid() {
        assert_eq!(parse_epic_status("open").unwrap(), EpicStatus::Open);
        assert_eq!(parse_epic_status("in_progress").unwrap(), EpicStatus::InProgress);
        assert_eq!(parse_epic_status("done").unwrap(), EpicStatus::Done);
    }

    // ── format_task_table ──

    #[test]
    fn format_task_table_contains_all_tasks() {
        let (store, _dir) = test_store();
        let t1 = store.create_task("Fix bug", None, None, None, Priority::High, TaskKind::default()).unwrap();
        let t2 = store.create_task("Add feature", None, None, None, Priority::Low, TaskKind::default()).unwrap();

        let tasks = store.list_tasks().unwrap();
        let output = format_task_table(&tasks);

        assert!(output.contains("Tasks:"));
        assert!(output.contains(&t1.id));
        assert!(output.contains(&t2.id));
        assert!(output.contains("high"));
        assert!(output.contains("low"));
        assert!(output.contains("Fix bug"));
        assert!(output.contains("Add feature"));
    }

    // ── format_epic_table ──

    #[test]
    fn format_epic_table_shows_task_count() {
        let (store, _dir) = test_store();
        let epic = store
            .create_epic("Big feature", "desc", None, Priority::Medium)
            .unwrap();
        store
            .create_task("Sub-task 1", None, Some(&epic.id), None, Priority::Medium, TaskKind::default())
            .unwrap();
        store
            .create_task("Sub-task 2", None, Some(&epic.id), None, Priority::Medium, TaskKind::default())
            .unwrap();

        let epics = store.list_epics().unwrap();
        let output = format_epic_table(&epics, &store);

        assert!(output.contains("Epics:"));
        assert!(output.contains(&epic.id));
        assert!(output.contains("2 tasks"));
    }

    // ── format_task_detail ──

    #[test]
    fn format_task_detail_shows_all_fields() {
        let (store, _dir) = test_store();
        let epic = store.create_epic("My Epic", "desc", None, Priority::Medium).unwrap();
        let task = store
            .create_task("Fix bug", Some("Segfault on exit"), Some(&epic.id), None, Priority::High, TaskKind::default())
            .unwrap();

        let output = format_task_detail(&task, &store);
        assert!(output.contains(&task.id));
        assert!(output.contains("Fix bug"));
        assert!(output.contains("high"));
        assert!(output.contains("Segfault on exit"));
        assert!(output.contains(&epic.id));
        assert!(output.contains("My Epic"));
    }

    #[test]
    fn format_task_detail_without_optional_fields() {
        let (store, _dir) = test_store();
        let task = store
            .create_task("Simple", None, None, None, Priority::Medium, TaskKind::default())
            .unwrap();

        let output = format_task_detail(&task, &store);
        assert!(output.contains("Simple"));
        assert!(!output.contains("Description:"));
        assert!(!output.contains("Epic:"));
        assert!(!output.contains("Session:"));
    }

    // ── format_epic_detail ──

    #[test]
    fn format_epic_detail_shows_child_tasks() {
        let (store, _dir) = test_store();
        let epic = store
            .create_epic("Big feature", "Do everything", Some("https://gh.com/1"), Priority::High)
            .unwrap();
        let t1 = store
            .create_task("Step 1", None, Some(&epic.id), None, Priority::Medium, TaskKind::default())
            .unwrap();
        store.complete_task(&t1.id).unwrap();
        let t2 = store
            .create_task("Step 2", None, Some(&epic.id), None, Priority::Medium, TaskKind::default())
            .unwrap();

        let tasks = store.tasks_by_epic(&epic.id).unwrap();
        let output = format_epic_detail(&epic, &tasks);

        assert!(output.contains(&epic.id));
        assert!(output.contains("Big feature"));
        assert!(output.contains("Do everything"));
        assert!(output.contains("https://gh.com/1"));
        assert!(output.contains(&format!("Tasks ({})", tasks.len())));
        // The completed task has a refreshed version in storage
        assert!(output.contains(&t1.id));
        assert!(output.contains(&t2.id));
    }

    // ── list filtering ──

    #[test]
    fn cmd_list_status_filter() {
        let (store, _dir) = test_store();
        store.create_task("Open one", None, None, None, Priority::Medium, TaskKind::default()).unwrap();
        let t2 = store.create_task("Done one", None, None, None, Priority::Medium, TaskKind::default()).unwrap();
        store.complete_task(&t2.id).unwrap();

        let mut tasks = store.list_tasks().unwrap();
        let status = parse_task_status("done").unwrap();
        tasks.retain(|t| t.status == status);

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, t2.id);
    }

    #[test]
    fn cmd_list_epic_filter() {
        let (store, _dir) = test_store();
        let epic = store.create_epic("E1", "desc", None, Priority::Medium).unwrap();
        store.create_task("In epic", None, Some(&epic.id), None, Priority::Medium, TaskKind::default()).unwrap();
        store.create_task("No epic", None, None, None, Priority::Medium, TaskKind::default()).unwrap();

        let tasks = store.tasks_by_epic(&epic.id).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "In epic");
    }

    // ── truncate ──

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string() {
        assert_eq!(truncate("hello world", 5), "hello");
    }

    #[test]
    fn truncate_multibyte_chars() {
        // 5 CJK characters = 15 bytes, truncate to 3 chars
        let s = "日本語文字";
        let result = truncate(s, 3);
        assert_eq!(result, "日本語");
        assert_eq!(result.chars().count(), 3);
    }

    // ── bug kind support ──

    #[test]
    fn format_task_table_labels_bugs() {
        let (store, _dir) = test_store();
        store.create_task("Normal task", None, None, None, Priority::Medium, TaskKind::Task).unwrap();
        store.create_task("Crash on exit", None, None, None, Priority::High, TaskKind::Bug).unwrap();

        let tasks = store.list_tasks().unwrap();
        let output = format_task_table(&tasks);

        assert!(output.contains("[bug] Crash on exit"));
        assert!(output.contains("Normal task"));
        assert!(!output.contains("[bug] Normal task"));
    }

    #[test]
    fn format_task_detail_shows_bug_header() {
        let (store, _dir) = test_store();
        let bug = store
            .create_task("Crash on exit", Some("Segfault"), None, None, Priority::High, TaskKind::Bug)
            .unwrap();

        let output = format_task_detail(&bug, &store);
        assert!(output.starts_with("Bug: "));
        assert!(output.contains("Type:        bug"));
        assert!(output.contains(&bug.id));
    }

    #[test]
    fn format_task_detail_task_has_no_type_line() {
        let (store, _dir) = test_store();
        let task = store
            .create_task("Normal", None, None, None, Priority::Medium, TaskKind::Task)
            .unwrap();

        let output = format_task_detail(&task, &store);
        assert!(output.starts_with("Task: "));
        assert!(!output.contains("Type:"));
    }

    // ── delete_epic integration ──

    #[test]
    fn delete_epic_removes_it() {
        let (store, _dir) = test_store();
        let epic = store.create_epic("Temp", "desc", None, Priority::Medium).unwrap();
        assert!(store.get_epic(&epic.id).is_ok());
        store.delete_epic(&epic.id).unwrap();
        assert!(store.get_epic(&epic.id).is_err());
    }

    // ── update roundtrips ──

    #[test]
    fn update_task_changes_fields() {
        let (store, _dir) = test_store();
        let task = store.create_task("Original", None, None, None, Priority::Low, TaskKind::default()).unwrap();

        let mut task = store.get_task(&task.id).unwrap();
        task.title = "Updated".to_string();
        task.priority = Priority::High;
        task.status = TaskStatus::InProgress;
        store.update_task(&mut task).unwrap();

        let fetched = store.get_task(&task.id).unwrap();
        assert_eq!(fetched.title, "Updated");
        assert_eq!(fetched.priority, Priority::High);
        assert_eq!(fetched.status, TaskStatus::InProgress);
    }

    #[test]
    fn update_epic_changes_fields() {
        let (store, _dir) = test_store();
        let epic = store.create_epic("Original", "desc", None, Priority::Low).unwrap();

        let mut epic = store.get_epic(&epic.id).unwrap();
        epic.title = "Updated".to_string();
        epic.external_ref = Some("https://gh.com/42".to_string());
        store.update_epic(&mut epic).unwrap();

        let fetched = store.get_epic(&epic.id).unwrap();
        assert_eq!(fetched.title, "Updated");
        assert_eq!(fetched.external_ref.as_deref(), Some("https://gh.com/42"));
    }
}
