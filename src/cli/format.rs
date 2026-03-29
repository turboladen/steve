use crate::{
    DateTimeExt,
    task::{Epic, Task, TaskKind, TaskStatus},
};

pub(super) fn format_task_table(tasks: &[Task]) -> String {
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

pub(super) fn format_epic_table(epics: &[Epic], store: &crate::task::TaskStore) -> String {
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
        let task_count = store.tasks_by_epic(&epic.id).map(|t| t.len()).unwrap_or(0);
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

pub(super) fn format_task_detail(task: &Task, store: &crate::task::TaskStore) -> String {
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
    out.push_str(&format!(
        "  Created:     {}\n",
        task.created_at.display_short()
    ));
    out.push_str(&format!(
        "  Updated:     {}\n",
        task.updated_at.display_short()
    ));
    out
}

pub(super) fn format_epic_detail(epic: &Epic, tasks: &[Task]) -> String {
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
    out.push_str(&format!(
        "  Created:     {}\n",
        epic.created_at.display_short()
    ));
    out.push_str(&format!(
        "  Updated:     {}\n",
        epic.updated_at.display_short()
    ));

    if !tasks.is_empty() {
        out.push_str(&format!("  Tasks ({}):\n", tasks.len()));
        for task in tasks {
            let check = if task.status == TaskStatus::Done {
                "x"
            } else {
                " "
            };
            out.push_str(&format!("    [{}] {} — {}\n", check, task.id, task.title));
        }
    }
    out
}

pub(super) fn truncate(s: &str, max_chars: usize) -> &str {
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
    use crate::task::{Priority, TaskKind, TaskStore};
    use tempfile::tempdir;

    fn test_store() -> (TaskStore, tempfile::TempDir) {
        let dir = tempdir().expect("temp dir");
        let storage =
            crate::storage::Storage::with_base(dir.path().to_path_buf()).expect("storage");
        (TaskStore::new(storage, "test".to_string()), dir)
    }

    // ── format_task_table ──

    #[test]
    fn format_task_table_contains_all_tasks() {
        let (store, _dir) = test_store();
        let t1 = store
            .create_task(
                "Fix bug",
                None,
                None,
                None,
                Priority::High,
                TaskKind::default(),
            )
            .unwrap();
        let t2 = store
            .create_task(
                "Add feature",
                None,
                None,
                None,
                Priority::Low,
                TaskKind::default(),
            )
            .unwrap();

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
            .create_task(
                "Sub-task 1",
                None,
                Some(&epic.id),
                None,
                Priority::Medium,
                TaskKind::default(),
            )
            .unwrap();
        store
            .create_task(
                "Sub-task 2",
                None,
                Some(&epic.id),
                None,
                Priority::Medium,
                TaskKind::default(),
            )
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
        let epic = store
            .create_epic("My Epic", "desc", None, Priority::Medium)
            .unwrap();
        let task = store
            .create_task(
                "Fix bug",
                Some("Segfault on exit"),
                Some(&epic.id),
                None,
                Priority::High,
                TaskKind::default(),
            )
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
            .create_task(
                "Simple",
                None,
                None,
                None,
                Priority::Medium,
                TaskKind::default(),
            )
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
            .create_epic(
                "Big feature",
                "Do everything",
                Some("https://gh.com/1"),
                Priority::High,
            )
            .unwrap();
        let t1 = store
            .create_task(
                "Step 1",
                None,
                Some(&epic.id),
                None,
                Priority::Medium,
                TaskKind::default(),
            )
            .unwrap();
        store.complete_task(&t1.id).unwrap();
        let t2 = store
            .create_task(
                "Step 2",
                None,
                Some(&epic.id),
                None,
                Priority::Medium,
                TaskKind::default(),
            )
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
        store
            .create_task(
                "Normal task",
                None,
                None,
                None,
                Priority::Medium,
                TaskKind::Task,
            )
            .unwrap();
        store
            .create_task(
                "Crash on exit",
                None,
                None,
                None,
                Priority::High,
                TaskKind::Bug,
            )
            .unwrap();

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
            .create_task(
                "Crash on exit",
                Some("Segfault"),
                None,
                None,
                Priority::High,
                TaskKind::Bug,
            )
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
}
