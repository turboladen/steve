use anyhow::Result;

use crate::task::{Priority, TaskKind, TaskStore};

use super::{
    EntityKind, detect_entity,
    format::{format_epic_detail, format_epic_table, format_task_detail, format_task_table},
    parse_epic_status, parse_priority, parse_task_status,
};

pub(super) fn cmd_list(
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
        let epics: Vec<crate::task::Epic> = if let Some(s) = status_filter {
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

pub(super) fn cmd_show(store: &TaskStore, id: &str) -> Result<()> {
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

pub(super) fn cmd_create_task(
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

pub(super) fn cmd_create_epic(
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

pub(super) fn cmd_complete(store: &TaskStore, id: &str) -> Result<()> {
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

pub(super) fn cmd_delete(store: &TaskStore, id: &str) -> Result<()> {
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

pub(super) fn cmd_update(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{Priority, TaskKind, TaskStatus, TaskStore};
    use tempfile::tempdir;

    fn test_store() -> (TaskStore, tempfile::TempDir) {
        let dir = tempdir().expect("temp dir");
        let storage =
            crate::storage::Storage::with_base(dir.path().to_path_buf()).expect("storage");
        (TaskStore::new(storage, "test".to_string()), dir)
    }

    // ── list filtering ──

    #[test]
    fn cmd_list_status_filter() {
        let (store, _dir) = test_store();
        store
            .create_task(
                "Open one",
                None,
                None,
                None,
                Priority::Medium,
                TaskKind::default(),
            )
            .unwrap();
        let t2 = store
            .create_task(
                "Done one",
                None,
                None,
                None,
                Priority::Medium,
                TaskKind::default(),
            )
            .unwrap();
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
        let epic = store
            .create_epic("E1", "desc", None, Priority::Medium)
            .unwrap();
        store
            .create_task(
                "In epic",
                None,
                Some(&epic.id),
                None,
                Priority::Medium,
                TaskKind::default(),
            )
            .unwrap();
        store
            .create_task(
                "No epic",
                None,
                None,
                None,
                Priority::Medium,
                TaskKind::default(),
            )
            .unwrap();

        let tasks = store.tasks_by_epic(&epic.id).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "In epic");
    }

    // ── delete_epic integration ──

    #[test]
    fn delete_epic_removes_it() {
        let (store, _dir) = test_store();
        let epic = store
            .create_epic("Temp", "desc", None, Priority::Medium)
            .unwrap();
        assert!(store.get_epic(&epic.id).is_ok());
        store.delete_epic(&epic.id).unwrap();
        assert!(store.get_epic(&epic.id).is_err());
    }

    // ── update roundtrips ──

    #[test]
    fn update_task_changes_fields() {
        let (store, _dir) = test_store();
        let task = store
            .create_task(
                "Original",
                None,
                None,
                None,
                Priority::Low,
                TaskKind::default(),
            )
            .unwrap();

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
        let epic = store
            .create_epic("Original", "desc", None, Priority::Low)
            .unwrap();

        let mut epic = store.get_epic(&epic.id).unwrap();
        epic.title = "Updated".to_string();
        epic.external_ref = Some("https://gh.com/42".to_string());
        store.update_epic(&mut epic).unwrap();

        let fetched = store.get_epic(&epic.id).unwrap();
        assert_eq!(fetched.title, "Updated");
        assert_eq!(fetched.external_ref.as_deref(), Some("https://gh.com/42"));
    }
}
