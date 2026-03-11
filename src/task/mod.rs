pub mod types;

use anyhow::Result;
use chrono::Utc;

use crate::storage::Storage;
pub use types::*;

/// Wraps the [`Storage`] layer for task and epic CRUD operations.
///
/// Storage key paths:
/// - Epics: `["tasks", "epics", &id]`
/// - Tasks: `["tasks", "items", &id]`
#[derive(Debug, Clone)]
pub struct TaskStore {
    storage: Storage,
    /// Project name prefix used for generating task/epic IDs (e.g., "steve").
    project_prefix: String,
}

impl TaskStore {
    /// Create a new `TaskStore` backed by the given storage instance.
    ///
    /// `project_prefix` is the project name used in generated IDs (e.g., "steve"
    /// produces IDs like `steve-ta3f`).
    pub fn new(storage: Storage, project_prefix: String) -> Self {
        Self { storage, project_prefix }
    }

    // ── Task CRUD ──

    /// Create a new task or bug with the given fields, persisting it immediately.
    ///
    /// The `kind` parameter controls the ID prefix (`task-` or `bug-`).
    pub fn create_task(
        &self,
        title: &str,
        description: Option<&str>,
        epic_id: Option<&str>,
        session_id: Option<&str>,
        priority: Priority,
        kind: TaskKind,
    ) -> Result<Task> {
        let now = Utc::now();
        let kind_char = match kind {
            TaskKind::Task => 't',
            TaskKind::Bug => 'b',
        };
        let task = Task {
            id: generate_id(&self.project_prefix, kind_char),
            kind,
            title: title.to_string(),
            description: description.map(String::from),
            epic_id: epic_id.map(String::from),
            session_id: session_id.map(String::from),
            priority,
            status: TaskStatus::Open,
            created_at: now,
            updated_at: now,
        };
        self.storage
            .write(&["tasks", "items", &task.id], &task)?;
        Ok(task)
    }

    /// Convenience: create a bug report (shorthand for `create_task` with `TaskKind::Bug`).
    pub fn create_bug(
        &self,
        title: &str,
        description: Option<&str>,
        epic_id: Option<&str>,
        session_id: Option<&str>,
        priority: Priority,
    ) -> Result<Task> {
        self.create_task(title, description, epic_id, session_id, priority, TaskKind::Bug)
    }

    /// Read a task by ID.
    pub fn get_task(&self, id: &str) -> Result<Task> {
        self.storage.read(&["tasks", "items", id])
    }

    /// Write an updated task back to storage, refreshing `updated_at`.
    pub fn update_task(&self, task: &mut Task) -> Result<()> {
        task.updated_at = Utc::now();
        self.storage
            .write(&["tasks", "items", &task.id], task)
    }

    /// List all tasks.
    pub fn list_tasks(&self) -> Result<Vec<Task>> {
        let ids = self.storage.list(&["tasks", "items"])?;
        let mut tasks = Vec::with_capacity(ids.len());
        for id in &ids {
            match self.storage.read::<Task>(&["tasks", "items", id]) {
                Ok(task) => tasks.push(task),
                Err(e) => {
                    tracing::warn!("skipping unreadable task {id}: {e}");
                }
            }
        }
        Ok(tasks)
    }

    /// Mark a task as done and return the updated task.
    pub fn complete_task(&self, id: &str) -> Result<Task> {
        let mut task: Task = self.get_task(id)?;
        task.status = TaskStatus::Done;
        task.updated_at = Utc::now();
        self.storage
            .write(&["tasks", "items", &task.id], &task)?;
        Ok(task)
    }

    /// Delete a task from storage.
    pub fn delete_task(&self, id: &str) -> Result<()> {
        self.storage.delete(&["tasks", "items", id])
    }

    // ── Epic CRUD ──

    /// Create a new epic with the given fields, persisting it immediately.
    pub fn create_epic(
        &self,
        title: &str,
        description: &str,
        external_ref: Option<&str>,
        priority: Priority,
    ) -> Result<Epic> {
        let now = Utc::now();
        let epic = Epic {
            id: generate_id(&self.project_prefix, 'e'),
            title: title.to_string(),
            description: description.to_string(),
            external_ref: external_ref.map(String::from),
            priority,
            status: EpicStatus::Open,
            created_at: now,
            updated_at: now,
        };
        self.storage
            .write(&["tasks", "epics", &epic.id], &epic)?;
        Ok(epic)
    }

    /// Read an epic by ID.
    pub fn get_epic(&self, id: &str) -> Result<Epic> {
        self.storage.read(&["tasks", "epics", id])
    }

    /// Write an updated epic back to storage, refreshing `updated_at`.
    pub fn update_epic(&self, epic: &mut Epic) -> Result<()> {
        epic.updated_at = Utc::now();
        self.storage
            .write(&["tasks", "epics", &epic.id], epic)
    }

    /// List all epics.
    pub fn list_epics(&self) -> Result<Vec<Epic>> {
        let ids = self.storage.list(&["tasks", "epics"])?;
        let mut epics = Vec::with_capacity(ids.len());
        for id in &ids {
            match self.storage.read::<Epic>(&["tasks", "epics", id]) {
                Ok(epic) => epics.push(epic),
                Err(e) => {
                    tracing::warn!("skipping unreadable epic {id}: {e}");
                }
            }
        }
        Ok(epics)
    }

    /// Mark an epic as done and return the updated epic.
    pub fn complete_epic(&self, id: &str) -> Result<Epic> {
        let mut epic: Epic = self.get_epic(id)?;
        epic.status = EpicStatus::Done;
        epic.updated_at = Utc::now();
        self.storage
            .write(&["tasks", "epics", &epic.id], &epic)?;
        Ok(epic)
    }

    /// Delete an epic from storage.
    pub fn delete_epic(&self, id: &str) -> Result<()> {
        self.storage.delete(&["tasks", "epics", id])
    }

    // ── Query helpers ──

    /// Return all tasks belonging to the given epic.
    pub fn tasks_by_epic(&self, epic_id: &str) -> Result<Vec<Task>> {
        Ok(self
            .list_tasks()?
            .into_iter()
            .filter(|t| t.epic_id.as_deref() == Some(epic_id))
            .collect())
    }

    /// Return all tasks linked to the given session.
    pub fn tasks_by_session(&self, session_id: &str) -> Result<Vec<Task>> {
        Ok(self
            .list_tasks()?
            .into_iter()
            .filter(|t| t.session_id.as_deref() == Some(session_id))
            .collect())
    }

    /// Return all tasks whose status is not `Done`.
    pub fn open_tasks(&self) -> Result<Vec<Task>> {
        Ok(self
            .list_tasks()?
            .into_iter()
            .filter(|t| t.status != TaskStatus::Done)
            .collect())
    }

    /// Return only items with `TaskKind::Bug`.
    pub fn list_bugs(&self) -> Result<Vec<Task>> {
        Ok(self
            .list_tasks()?
            .into_iter()
            .filter(|t| t.kind == TaskKind::Bug)
            .collect())
    }

    /// Return open bugs (not Done, kind == Bug).
    pub fn open_bugs(&self) -> Result<Vec<Task>> {
        Ok(self
            .list_tasks()?
            .into_iter()
            .filter(|t| t.kind == TaskKind::Bug && t.status != TaskStatus::Done)
            .collect())
    }

    // ── System prompt summary ──

    /// Build a compact markdown summary suitable for injection into the LLM
    /// system prompt. Session-scoped tasks appear first, then persistent
    /// tasks grouped by epic. Capped at approximately 1500 characters.
    pub fn summary_for_prompt(&self, current_session_id: &str) -> String {
        const MAX_CHARS: usize = 1500;

        let all_tasks = match self.list_tasks() {
            Ok(t) => t,
            Err(_) => return String::new(),
        };
        let all_epics = self.list_epics().unwrap_or_default();

        if all_tasks.is_empty() {
            return String::new();
        }

        let mut out = String::new();

        // Session tasks first
        let session_tasks: Vec<&Task> = all_tasks
            .iter()
            .filter(|t| t.session_id.as_deref() == Some(current_session_id))
            .collect();

        if !session_tasks.is_empty() {
            out.push_str("## Session Tasks\n");
            for task in &session_tasks {
                append_task_line(&mut out, task);
                if out.len() >= MAX_CHARS {
                    truncate_summary(&mut out, MAX_CHARS);
                    return out;
                }
            }
            out.push('\n');
        }

        // Persistent tasks grouped by epic
        let persistent_tasks: Vec<&Task> = all_tasks
            .iter()
            .filter(|t| t.session_id.as_deref() != Some(current_session_id))
            .collect();

        if persistent_tasks.is_empty() {
            return out;
        }

        // Tasks with an epic, grouped
        for epic in &all_epics {
            let epic_tasks: Vec<&&Task> = persistent_tasks
                .iter()
                .filter(|t| t.epic_id.as_deref() == Some(&epic.id))
                .collect();
            if epic_tasks.is_empty() {
                continue;
            }

            out.push_str(&format!("## {}\n", epic.title));
            for task in &epic_tasks {
                append_task_line(&mut out, task);
                if out.len() >= MAX_CHARS {
                    truncate_summary(&mut out, MAX_CHARS);
                    return out;
                }
            }
            out.push('\n');
        }

        // Orphan persistent tasks (no epic)
        let orphans: Vec<&&Task> = persistent_tasks
            .iter()
            .filter(|t| t.epic_id.is_none())
            .collect();

        if !orphans.is_empty() {
            out.push_str("## Tasks\n");
            for task in &orphans {
                append_task_line(&mut out, task);
                if out.len() >= MAX_CHARS {
                    truncate_summary(&mut out, MAX_CHARS);
                    return out;
                }
            }
        }

        out
    }
}

/// Append a checkbox-style task line: `- [ ] title` or `- [x] title`.
/// Bugs are prefixed with `[bug]` for visibility.
fn append_task_line(out: &mut String, task: &Task) {
    let check = if task.status == TaskStatus::Done {
        "x"
    } else {
        " "
    };
    let kind_prefix = match task.kind {
        TaskKind::Task => "",
        TaskKind::Bug => "[bug] ",
    };
    out.push_str(&format!("- [{check}] {kind_prefix}{}\n", task.title));
}

/// Truncate the summary to approximately `max` characters, ending cleanly
/// at a line boundary with a `...` marker.
fn truncate_summary(out: &mut String, max: usize) {
    if out.len() <= max {
        return;
    }
    // Find the last newline before the limit
    let truncated = &out[..max];
    if let Some(pos) = truncated.rfind('\n') {
        out.truncate(pos + 1);
    } else {
        out.truncate(max);
    }
    out.push_str("...\n");
}

/// Generate a short project-scoped ID using time + atomic counter for uniqueness.
///
/// Format: `{project_prefix}-{kind_char}{4_hex_chars}` (e.g., `steve-ta3f0`).
/// Combines subsecond nanos with a process-local atomic counter to reduce
/// collision probability. With 16 bits (65536 values per kind), birthday
/// paradox gives ~50% collision chance at ~256 IDs — sufficient for typical
/// task counts but not guaranteed unique.
fn generate_id(project_prefix: &str, kind_char: char) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    // Mix nanos and counter, mask to 16 bits (4 hex chars = 65536 values)
    let mixed = nanos.wrapping_add(seq.wrapping_mul(2654435761)); // Knuth multiplicative hash
    let short_hash = mixed & 0xFFFF;
    format!("{project_prefix}-{kind_char}{short_hash:04x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_store() -> (TaskStore, tempfile::TempDir) {
        let dir = tempdir().expect("failed to create temp dir");
        let storage =
            Storage::with_base(dir.path().to_path_buf()).expect("failed to create storage");
        (TaskStore::new(storage, "test".to_string()), dir)
    }

    #[test]
    fn create_and_get_task_round_trip() {
        let (store, _dir) = test_store();
        let task = store
            .create_task("Fix bug", Some("Segfault on exit"), None, None, Priority::High, TaskKind::Task)
            .expect("create_task");
        assert!(task.id.starts_with("test-t"), "got: {}", task.id);
        assert_eq!(task.title, "Fix bug");
        assert_eq!(task.description.as_deref(), Some("Segfault on exit"));
        assert_eq!(task.status, TaskStatus::Open);
        assert_eq!(task.priority, Priority::High);

        let fetched = store.get_task(&task.id).expect("get_task");
        assert_eq!(fetched.id, task.id);
        assert_eq!(fetched.title, task.title);
    }

    #[test]
    fn create_and_get_epic_round_trip() {
        let (store, _dir) = test_store();
        let epic = store
            .create_epic("Big feature", "Implement everything", None, Priority::Medium)
            .expect("create_epic");
        assert!(epic.id.starts_with("test-e"), "got: {}", epic.id);
        assert_eq!(epic.title, "Big feature");
        assert_eq!(epic.description, "Implement everything");
        assert_eq!(epic.status, EpicStatus::Open);

        let fetched = store.get_epic(&epic.id).expect("get_epic");
        assert_eq!(fetched.id, epic.id);
        assert_eq!(fetched.title, epic.title);
    }

    #[test]
    fn list_tasks_returns_all() {
        let (store, _dir) = test_store();
        store
            .create_task("Task A", None, None, None, Priority::Low, TaskKind::Task)
            .unwrap();
        store
            .create_task("Task B", None, None, None, Priority::Medium, TaskKind::Task)
            .unwrap();
        store
            .create_task("Task C", None, None, None, Priority::High, TaskKind::Task)
            .unwrap();

        let tasks = store.list_tasks().expect("list_tasks");
        assert_eq!(tasks.len(), 3, "expected 3 tasks, got {}", tasks.len());
    }

    #[test]
    fn complete_task_changes_status() {
        let (store, _dir) = test_store();
        let task = store
            .create_task("Finish it", None, None, None, Priority::Medium, TaskKind::Task)
            .unwrap();
        assert_eq!(task.status, TaskStatus::Open);

        let completed = store.complete_task(&task.id).expect("complete_task");
        assert_eq!(completed.status, TaskStatus::Done);

        let fetched = store.get_task(&task.id).expect("get after complete");
        assert_eq!(fetched.status, TaskStatus::Done);
    }

    #[test]
    fn delete_task_removes_it() {
        let (store, _dir) = test_store();
        let task = store
            .create_task("Ephemeral", None, None, None, Priority::Low, TaskKind::Task)
            .unwrap();
        assert!(store.get_task(&task.id).is_ok());

        store.delete_task(&task.id).expect("delete_task");
        assert!(store.get_task(&task.id).is_err());
    }

    #[test]
    fn tasks_by_epic_filters_correctly() {
        let (store, _dir) = test_store();
        let epic = store
            .create_epic("My Epic", "desc", None, Priority::Medium)
            .unwrap();
        let t1 = store
            .create_task("In epic", None, Some(&epic.id), None, Priority::Medium, TaskKind::Task)
            .unwrap();
        let t2 = store
            .create_task("No epic", None, None, None, Priority::Medium, TaskKind::Task)
            .unwrap();

        let filtered = store.tasks_by_epic(&epic.id).expect("tasks_by_epic");
        let ids: Vec<&str> = filtered.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&t1.id.as_str()));
        assert!(!ids.contains(&t2.id.as_str()));
    }

    #[test]
    fn tasks_by_session_filters_correctly() {
        let (store, _dir) = test_store();
        let t1 = store
            .create_task("Session A", None, None, Some("sess-a"), Priority::Medium, TaskKind::Task)
            .unwrap();
        let t2 = store
            .create_task("Session B", None, None, Some("sess-b"), Priority::Medium, TaskKind::Task)
            .unwrap();

        let filtered = store.tasks_by_session("sess-a").expect("tasks_by_session");
        let ids: Vec<&str> = filtered.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&t1.id.as_str()));
        assert!(!ids.contains(&t2.id.as_str()));
    }

    #[test]
    fn open_tasks_excludes_done() {
        let (store, _dir) = test_store();
        let t1 = store
            .create_task("Open one", None, None, None, Priority::Medium, TaskKind::Task)
            .unwrap();
        let t2 = store
            .create_task("Done one", None, None, None, Priority::Medium, TaskKind::Task)
            .unwrap();
        store.complete_task(&t2.id).unwrap();

        let open = store.open_tasks().expect("open_tasks");
        let ids: Vec<&str> = open.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&t1.id.as_str()));
        assert!(!ids.contains(&t2.id.as_str()));
    }

    #[test]
    fn summary_for_prompt_produces_output() {
        let (store, _dir) = test_store();
        store
            .create_task("Session task", None, None, Some("sess-1"), Priority::Medium, TaskKind::Task)
            .unwrap();
        store
            .create_task("Other task", None, None, None, Priority::High, TaskKind::Task)
            .unwrap();

        let summary = store.summary_for_prompt("sess-1");
        assert!(!summary.is_empty());
        assert!(summary.contains("Session Tasks"));
        assert!(summary.contains("Session task"));
        assert!(summary.contains("[ ]"));
    }

    #[test]
    fn summary_for_prompt_truncation() {
        let (store, _dir) = test_store();
        // Create many tasks with long titles to exceed 1500 chars
        for i in 0..100 {
            let title = format!(
                "Task number {i:03} with a deliberately long title to consume characters quickly padding"
            );
            let _ = store.create_task(&title, None, None, None, Priority::Medium, TaskKind::Task);
        }

        let summary = store.summary_for_prompt("no-session");
        // Should be capped near 1500 chars (plus the "...\n" marker)
        assert!(
            summary.len() <= 1600,
            "summary too long: {} chars",
            summary.len()
        );
    }

    #[test]
    fn generate_id_has_correct_format() {
        let id = generate_id("test", 't');
        assert!(id.starts_with("test-t"), "got: {id}");
        // "test" + "-" + kind_char + 4 hex chars = 4 + 1 + 1 + 4 = 10
        assert_eq!(id.len(), "test-t".len() + 4, "got: {id}");

        let eid = generate_id("test", 'e');
        assert!(eid.starts_with("test-e"), "got: {eid}");
        assert_eq!(eid.len(), "test-e".len() + 4, "got: {eid}");
    }

    #[test]
    fn generate_id_uniqueness() {
        // Rapid-fire generation should produce unique IDs thanks to atomic counter
        let mut ids: Vec<String> = (0..100).map(|_| generate_id("test", 't')).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 100, "expected 100 unique IDs");
    }

    #[test]
    fn generate_id_bug_prefix() {
        let id = generate_id("test", 'b');
        assert!(id.starts_with("test-b"), "got: {id}");
        assert_eq!(id.len(), "test-b".len() + 4, "got: {id}");
    }

    // ── Bug creation and queries ──

    #[test]
    fn create_bug_uses_bug_prefix_and_kind() {
        let (store, _dir) = test_store();
        let bug = store
            .create_bug("Crash on empty input", Some("Segfault"), None, None, Priority::High)
            .expect("create_bug");
        assert!(bug.id.starts_with("test-b"), "got: {}", bug.id);
        assert_eq!(bug.kind, TaskKind::Bug);
        assert_eq!(bug.title, "Crash on empty input");
        assert_eq!(bug.status, TaskStatus::Open);
    }

    #[test]
    fn list_bugs_filters_by_kind() {
        let (store, _dir) = test_store();
        store
            .create_task("Regular task", None, None, None, Priority::Medium, TaskKind::Task)
            .unwrap();
        store
            .create_bug("A bug", None, None, None, Priority::High)
            .unwrap();
        store
            .create_bug("Another bug", None, None, None, Priority::Low)
            .unwrap();

        let bugs = store.list_bugs().expect("list_bugs");
        assert_eq!(bugs.len(), 2);
        assert!(bugs.iter().all(|b| b.kind == TaskKind::Bug));

        // list_tasks returns all (tasks + bugs)
        let all = store.list_tasks().expect("list_tasks");
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn open_bugs_excludes_done() {
        let (store, _dir) = test_store();
        let b1 = store
            .create_bug("Open bug", None, None, None, Priority::Medium)
            .unwrap();
        let b2 = store
            .create_bug("Fixed bug", None, None, None, Priority::Low)
            .unwrap();
        store.complete_task(&b2.id).unwrap();

        let open = store.open_bugs().expect("open_bugs");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, b1.id);
    }

    #[test]
    fn summary_for_prompt_shows_bug_prefix() {
        let (store, _dir) = test_store();
        store
            .create_bug("Crash on exit", None, None, Some("sess-1"), Priority::High)
            .unwrap();

        let summary = store.summary_for_prompt("sess-1");
        assert!(summary.contains("[bug]"), "bug should be prefixed in summary, got:\n{summary}");
        assert!(summary.contains("Crash on exit"));
    }
}
