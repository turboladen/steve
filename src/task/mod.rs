use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter, EnumString};

use crate::storage::Storage;

/// Distinguishes tasks from bugs. Both share the same [`Task`] struct
/// but carry different ID prefixes and display treatment.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, EnumIter,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
#[derive(Default)]
pub enum TaskKind {
    #[default]
    Task,
    Bug,
}

/// Priority level for epics and tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
#[derive(Default)]
pub enum Priority {
    High,
    #[default]
    Medium,
    Low,
}

/// Lifecycle status of an epic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum EpicStatus {
    #[strum(serialize = "open")]
    #[default]
    Open,
    #[strum(serialize = "in_progress")]
    InProgress,
    #[strum(serialize = "done")]
    Done,
}

/// Lifecycle status of a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TaskStatus {
    #[strum(serialize = "open")]
    #[default]
    Open,
    #[strum(serialize = "in_progress")]
    InProgress,
    #[strum(serialize = "done")]
    Done,
}

/// A high-level work item that groups related tasks.
///
/// Epics represent large features or initiatives that span multiple sessions.
/// Each epic can contain many tasks and optionally links to an external
/// tracker (e.g., a GitHub issue URL).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Epic {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Detailed description of the epic's scope and goals.
    pub description: String,
    /// Optional reference to an external tracker (e.g., GitHub issue URL).
    pub external_ref: Option<String>,
    /// How urgent this epic is.
    #[serde(default)]
    pub priority: Priority,
    /// Current lifecycle status.
    #[serde(default)]
    pub status: EpicStatus,
    /// When the epic was created.
    pub created_at: DateTime<Utc>,
    /// When the epic was last updated.
    pub updated_at: DateTime<Utc>,
}

/// A concrete unit of work, optionally belonging to an epic.
///
/// Tasks represent individual steps that the agent can tackle within a
/// single session. They track progress independently and can be linked
/// to both an epic (for grouping) and a session (for provenance).
/// The `kind` field distinguishes regular tasks from bugs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Unique identifier (prefixed `task-` or `bug-` by kind).
    pub id: String,
    /// Whether this is a regular task or a bug report.
    #[serde(default)]
    pub kind: TaskKind,
    /// Human-readable title.
    pub title: String,
    /// Optional detailed description.
    pub description: Option<String>,
    /// The epic this task belongs to, if any.
    pub epic_id: Option<String>,
    /// The session that last worked on this task, if any.
    pub session_id: Option<String>,
    /// How urgent this task is.
    #[serde(default)]
    pub priority: Priority,
    /// Current lifecycle status.
    #[serde(default)]
    pub status: TaskStatus,
    /// When the task was created.
    pub created_at: DateTime<Utc>,
    /// When the task was last updated.
    pub updated_at: DateTime<Utc>,
}

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
        Self {
            storage,
            project_prefix,
        }
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
        self.storage.write(&["tasks", "items", &task.id], &task)?;
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
        self.create_task(
            title,
            description,
            epic_id,
            session_id,
            priority,
            TaskKind::Bug,
        )
    }

    /// Read a task by ID.
    pub fn get_task(&self, id: &str) -> Result<Task> {
        self.storage.read(&["tasks", "items", id])
    }

    /// Write an updated task back to storage, refreshing `updated_at`.
    pub fn update_task(&self, task: &mut Task) -> Result<()> {
        task.updated_at = Utc::now();
        self.storage.write(&["tasks", "items", &task.id], task)
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
        self.storage.write(&["tasks", "items", &task.id], &task)?;
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
        self.storage.write(&["tasks", "epics", &epic.id], &epic)?;
        Ok(epic)
    }

    /// Read an epic by ID.
    pub fn get_epic(&self, id: &str) -> Result<Epic> {
        self.storage.read(&["tasks", "epics", id])
    }

    /// Write an updated epic back to storage, refreshing `updated_at`.
    pub fn update_epic(&self, epic: &mut Epic) -> Result<()> {
        epic.updated_at = Utc::now();
        self.storage.write(&["tasks", "epics", &epic.id], epic)
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
        self.storage.write(&["tasks", "epics", &epic.id], &epic)?;
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
/// A per-process salt (sampled once at first call) is XOR'd with the
/// Knuth-multiplicative-hash of a monotonic counter, then masked to 16 bits.
/// Within a single process, sequential `seq` values map to distinct 16-bit
/// outputs (the multiplicative-hash by an odd constant is a bijection mod
/// 2^16), so the first 65536 IDs are guaranteed unique. Across processes,
/// the salt varies, so two separate sessions are unlikely to collide.
fn generate_id(project_prefix: &str, kind_char: char) -> String {
    use std::{
        sync::{
            OnceLock,
            atomic::{AtomicU32, Ordering},
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    static COUNTER: AtomicU32 = AtomicU32::new(0);
    static SALT: OnceLock<u32> = OnceLock::new();

    let salt = *SALT.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
            ^ std::process::id().wrapping_mul(2654435761)
    });
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mixed = salt.wrapping_add(seq.wrapping_mul(2654435761)); // Knuth multiplicative hash
    let short_hash = mixed & 0xFFFF;
    format!("{project_prefix}-{kind_char}{short_hash:04x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use strum::IntoEnumIterator;
    use tempfile::tempdir;

    fn test_store() -> (TaskStore, tempfile::TempDir) {
        let dir = tempdir().expect("failed to create temp dir");
        let storage =
            Storage::with_base(dir.path().to_path_buf()).expect("failed to create storage");
        (TaskStore::new(storage, "test".to_string()), dir)
    }

    // ── TaskKind ──

    #[test]
    fn task_kind_default_is_task() {
        assert_eq!(TaskKind::default(), TaskKind::Task);
    }

    #[test]
    fn task_kind_display_fromstr_round_trip() {
        for variant in TaskKind::iter() {
            let displayed = variant.to_string();
            let parsed: TaskKind = TaskKind::from_str(&displayed).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn task_kind_serde_round_trip() {
        for variant in TaskKind::iter() {
            let json = serde_json::to_string(&variant).unwrap();
            let back: TaskKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn task_kind_rejects_invalid() {
        assert!(TaskKind::from_str("epic").is_err());
        assert!(TaskKind::from_str("").is_err());
        assert!(TaskKind::from_str("TASK").is_err());
    }

    // ── Priority ──

    #[test]
    fn priority_default_is_medium() {
        assert_eq!(Priority::default(), Priority::Medium);
    }

    #[test]
    fn priority_display_fromstr_round_trip() {
        for (variant, expected) in [
            (Priority::High, "high"),
            (Priority::Medium, "medium"),
            (Priority::Low, "low"),
        ] {
            let displayed = variant.to_string();
            assert_eq!(displayed, expected);
            let parsed: Priority = Priority::from_str(&displayed).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn priority_serde_round_trip() {
        for variant in [Priority::High, Priority::Medium, Priority::Low] {
            let json = serde_json::to_string(&variant).unwrap();
            let back: Priority = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn priority_rejects_invalid() {
        assert!(Priority::from_str("urgent").is_err());
        assert!(Priority::from_str("").is_err());
        assert!(Priority::from_str("HIGH").is_err()); // case-sensitive
    }

    // ── EpicStatus ──

    #[test]
    fn epic_status_default_is_open() {
        assert_eq!(EpicStatus::default(), EpicStatus::Open);
    }

    #[test]
    fn epic_status_display_fromstr_round_trip() {
        for (variant, expected) in [
            (EpicStatus::Open, "open"),
            (EpicStatus::InProgress, "in_progress"),
            (EpicStatus::Done, "done"),
        ] {
            let displayed = variant.to_string();
            assert_eq!(displayed, expected);
            let parsed: EpicStatus = EpicStatus::from_str(&displayed).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn epic_status_serde_round_trip() {
        for variant in [EpicStatus::Open, EpicStatus::InProgress, EpicStatus::Done] {
            let json = serde_json::to_string(&variant).unwrap();
            let back: EpicStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn epic_status_rejects_invalid() {
        assert!(EpicStatus::from_str("closed").is_err());
        assert!(EpicStatus::from_str("").is_err());
        assert!(EpicStatus::from_str("inprogress").is_err());
    }

    // ── TaskStatus ──

    #[test]
    fn task_status_default_is_open() {
        assert_eq!(TaskStatus::default(), TaskStatus::Open);
    }

    #[test]
    fn task_status_display_fromstr_round_trip() {
        for (variant, expected) in [
            (TaskStatus::Open, "open"),
            (TaskStatus::InProgress, "in_progress"),
            (TaskStatus::Done, "done"),
        ] {
            let displayed = variant.to_string();
            assert_eq!(displayed, expected);
            let parsed: TaskStatus = TaskStatus::from_str(&displayed).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn task_status_serde_round_trip() {
        for variant in [TaskStatus::Open, TaskStatus::InProgress, TaskStatus::Done] {
            let json = serde_json::to_string(&variant).unwrap();
            let back: TaskStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn task_status_rejects_invalid() {
        assert!(TaskStatus::from_str("blocked").is_err());
        assert!(TaskStatus::from_str("").is_err());
        assert!(TaskStatus::from_str("IN_PROGRESS").is_err());
    }

    // ── Epic serde round-trip ──

    #[test]
    fn epic_serde_round_trip() {
        let now = Utc::now();
        let epic = Epic {
            id: "epic-001".into(),
            title: "Implement task system".into(),
            description: "Add epics and tasks to Steve".into(),
            external_ref: Some("https://github.com/org/repo/issues/42".into()),
            priority: Priority::High,
            status: EpicStatus::InProgress,
            created_at: now,
            updated_at: now,
        };
        let json = serde_json::to_string_pretty(&epic).unwrap();
        let back: Epic = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, epic.id);
        assert_eq!(back.title, epic.title);
        assert_eq!(back.description, epic.description);
        assert_eq!(back.external_ref, epic.external_ref);
        assert_eq!(back.priority, epic.priority);
        assert_eq!(back.status, epic.status);
    }

    #[test]
    fn epic_serde_defaults_for_optional_fields() {
        let json = r#"{
            "id": "epic-002",
            "title": "Minimal epic",
            "description": "No priority or status specified",
            "external_ref": null,
            "created_at": "2026-03-10T00:00:00Z",
            "updated_at": "2026-03-10T00:00:00Z"
        }"#;
        let epic: Epic = serde_json::from_str(json).unwrap();
        assert_eq!(epic.priority, Priority::Medium);
        assert_eq!(epic.status, EpicStatus::Open);
        assert!(epic.external_ref.is_none());
    }

    // ── Task serde round-trip ──

    #[test]
    fn task_serde_round_trip() {
        let now = Utc::now();
        let task = Task {
            id: "task-001".into(),
            kind: TaskKind::Task,
            title: "Create types module".into(),
            description: Some("Define Epic, Task, and enum types".into()),
            epic_id: Some("epic-001".into()),
            session_id: Some("sess-abc".into()),
            priority: Priority::Low,
            status: TaskStatus::Done,
            created_at: now,
            updated_at: now,
        };
        let json = serde_json::to_string_pretty(&task).unwrap();
        let back: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, task.id);
        assert_eq!(back.title, task.title);
        assert_eq!(back.description, task.description);
        assert_eq!(back.epic_id, task.epic_id);
        assert_eq!(back.session_id, task.session_id);
        assert_eq!(back.kind, task.kind);
        assert_eq!(back.priority, task.priority);
        assert_eq!(back.status, task.status);
    }

    #[test]
    fn task_serde_bug_kind_round_trip() {
        let now = Utc::now();
        let bug = Task {
            id: "bug-001".into(),
            kind: TaskKind::Bug,
            title: "Crash on empty input".into(),
            description: Some("Segfault when stdin is empty".into()),
            epic_id: None,
            session_id: None,
            priority: Priority::High,
            status: TaskStatus::Open,
            created_at: now,
            updated_at: now,
        };
        let json = serde_json::to_string_pretty(&bug).unwrap();
        let back: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, TaskKind::Bug);
        assert_eq!(back.id, "bug-001");
    }

    #[test]
    fn task_serde_defaults_for_optional_fields() {
        let json = r#"{
            "id": "task-002",
            "title": "Minimal task",
            "description": null,
            "epic_id": null,
            "session_id": null,
            "created_at": "2026-03-10T00:00:00Z",
            "updated_at": "2026-03-10T00:00:00Z"
        }"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert_eq!(task.kind, TaskKind::Task); // absent kind defaults to Task
        assert_eq!(task.priority, Priority::Medium);
        assert_eq!(task.status, TaskStatus::Open);
        assert!(task.description.is_none());
        assert!(task.epic_id.is_none());
        assert!(task.session_id.is_none());
    }

    // ── TaskStore CRUD ──

    #[test]
    fn create_and_get_task_round_trip() {
        let (store, _dir) = test_store();
        let task = store
            .create_task(
                "Fix bug",
                Some("Segfault on exit"),
                None,
                None,
                Priority::High,
                TaskKind::Task,
            )
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
            .create_epic(
                "Big feature",
                "Implement everything",
                None,
                Priority::Medium,
            )
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
            .create_task(
                "Finish it",
                None,
                None,
                None,
                Priority::Medium,
                TaskKind::Task,
            )
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
            .create_task(
                "In epic",
                None,
                Some(&epic.id),
                None,
                Priority::Medium,
                TaskKind::Task,
            )
            .unwrap();
        let t2 = store
            .create_task(
                "No epic",
                None,
                None,
                None,
                Priority::Medium,
                TaskKind::Task,
            )
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
            .create_task(
                "Session A",
                None,
                None,
                Some("sess-a"),
                Priority::Medium,
                TaskKind::Task,
            )
            .unwrap();
        let t2 = store
            .create_task(
                "Session B",
                None,
                None,
                Some("sess-b"),
                Priority::Medium,
                TaskKind::Task,
            )
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
            .create_task(
                "Open one",
                None,
                None,
                None,
                Priority::Medium,
                TaskKind::Task,
            )
            .unwrap();
        let t2 = store
            .create_task(
                "Done one",
                None,
                None,
                None,
                Priority::Medium,
                TaskKind::Task,
            )
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
            .create_task(
                "Session task",
                None,
                None,
                Some("sess-1"),
                Priority::Medium,
                TaskKind::Task,
            )
            .unwrap();
        store
            .create_task(
                "Other task",
                None,
                None,
                None,
                Priority::High,
                TaskKind::Task,
            )
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
            .create_bug(
                "Crash on empty input",
                Some("Segfault"),
                None,
                None,
                Priority::High,
            )
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
            .create_task(
                "Regular task",
                None,
                None,
                None,
                Priority::Medium,
                TaskKind::Task,
            )
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
        assert!(
            summary.contains("[bug]"),
            "bug should be prefixed in summary, got:\n{summary}"
        );
        assert!(summary.contains("Crash on exit"));
    }
}
