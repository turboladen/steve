use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter, EnumString};

/// Distinguishes tasks from bugs. Both share the same [`Task`] struct
/// but carry different ID prefixes and display treatment.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, EnumIter,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum TaskKind {
    Task,
    Bug,
}

impl Default for TaskKind {
    fn default() -> Self {
        Self::Task
    }
}

/// Priority level for epics and tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum Priority {
    High,
    Medium,
    Low,
}

impl Default for Priority {
    fn default() -> Self {
        Self::Medium
    }
}

/// Lifecycle status of an epic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "snake_case")]
pub enum EpicStatus {
    #[strum(serialize = "open")]
    Open,
    #[strum(serialize = "in_progress")]
    InProgress,
    #[strum(serialize = "done")]
    Done,
}

impl Default for EpicStatus {
    fn default() -> Self {
        Self::Open
    }
}

/// Lifecycle status of a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[strum(serialize = "open")]
    Open,
    #[strum(serialize = "in_progress")]
    InProgress,
    #[strum(serialize = "done")]
    Done,
}

impl Default for TaskStatus {
    fn default() -> Self {
        Self::Open
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use strum::IntoEnumIterator;

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
}
