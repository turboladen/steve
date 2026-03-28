mod commands;
mod format;

use std::str::FromStr;

use anyhow::{Result, bail};
use clap::Subcommand;

use crate::task::{EpicStatus, Priority, TaskKind, TaskStatus, TaskStore};

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
pub(super) enum EntityKind {
    Task,
    Epic,
}

pub(super) fn detect_entity(id: &str) -> Result<EntityKind> {
    // New format first: {project}-{kind_char}{hash} — kind is first char after last dash.
    // Must come before legacy checks because project names like "task-runner" would
    // false-match the legacy "task-*" prefix.
    if let Some(pos) = id.rfind('-') {
        match id.as_bytes().get(pos + 1) {
            Some(b't') | Some(b'b') => return Ok(EntityKind::Task),
            Some(b'e') => return Ok(EntityKind::Epic),
            _ => {}
        }
    }
    // Legacy format fallback: task-{8hex}, bug-{8hex}, epic-{8hex}
    // These have hex chars (0-9, a-f) after the dash, which don't match t/b/e above.
    if id.starts_with("task-") || id.starts_with("bug-") {
        return Ok(EntityKind::Task);
    }
    if id.starts_with("epic-") {
        return Ok(EntityKind::Epic);
    }
    bail!("unknown ID format: {id}")
}

pub(super) fn parse_priority(s: &str) -> Result<Priority> {
    Priority::from_str(s)
        .map_err(|_| anyhow::anyhow!("invalid priority: {s} (expected high, medium, low)"))
}

pub(super) fn parse_task_status(s: &str) -> Result<TaskStatus> {
    TaskStatus::from_str(s)
        .map_err(|_| anyhow::anyhow!("invalid status: {s} (expected open, in_progress, done)"))
}

pub(super) fn parse_epic_status(s: &str) -> Result<EpicStatus> {
    EpicStatus::from_str(s)
        .map_err(|_| anyhow::anyhow!("invalid status: {s} (expected open, in_progress, done)"))
}

/// Entry point for `steve task <subcommand>`.
pub fn run_task(command: TaskCommand) -> Result<()> {
    let project_info = crate::project::detect_or_cwd();
    let storage = crate::storage::Storage::new(&project_info.id)?;
    let repo_name =
        crate::project::git_repo_name(&project_info.root).unwrap_or_else(|| "proj".to_string());
    let store = TaskStore::new(storage, repo_name);

    match command {
        TaskCommand::List {
            tasks,
            epics,
            status,
            epic,
        } => commands::cmd_list(&store, tasks, epics, status.as_deref(), epic.as_deref()),

        TaskCommand::Show { id } => commands::cmd_show(&store, &id),

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
                commands::cmd_create_epic(
                    &store,
                    &title,
                    description.as_deref(),
                    external_ref.as_deref(),
                    priority,
                )
            } else {
                let kind = if bug { TaskKind::Bug } else { TaskKind::Task };
                commands::cmd_create_task(
                    &store,
                    &title,
                    description.as_deref(),
                    epic_id.as_deref(),
                    priority,
                    kind,
                )
            }
        }

        TaskCommand::Complete { id } => commands::cmd_complete(&store, &id),
        TaskCommand::Delete { id } => commands::cmd_delete(&store, &id),
        TaskCommand::Update {
            id,
            title,
            description,
            status,
            priority,
            external_ref,
        } => commands::cmd_update(
            &store,
            &id,
            title,
            description,
            status,
            priority,
            external_ref,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(detect_entity("").is_err());
        assert!(detect_entity("taskfoo").is_err());
        assert!(detect_entity("foo-x12").is_err());
    }

    #[test]
    fn detect_entity_new_format_task() {
        assert_eq!(detect_entity("steve-ta3f").unwrap(), EntityKind::Task);
        assert_eq!(detect_entity("my-app-t01c").unwrap(), EntityKind::Task);
    }

    #[test]
    fn detect_entity_new_format_bug() {
        assert_eq!(detect_entity("steve-b01c").unwrap(), EntityKind::Task);
        assert_eq!(detect_entity("my-app-b7ff").unwrap(), EntityKind::Task);
    }

    #[test]
    fn detect_entity_new_format_epic() {
        assert_eq!(detect_entity("steve-e7ff").unwrap(), EntityKind::Epic);
        assert_eq!(detect_entity("proj-e001").unwrap(), EntityKind::Epic);
    }

    #[test]
    fn detect_entity_project_name_starting_with_task() {
        // Project "task-runner" produces IDs like "task-runner-ea3f" — must not
        // false-match the legacy "task-*" prefix.
        assert_eq!(detect_entity("task-runner-ea3f").unwrap(), EntityKind::Epic);
        assert_eq!(detect_entity("task-runner-t01c").unwrap(), EntityKind::Task);
        assert_eq!(detect_entity("bug-tracker-b7ff").unwrap(), EntityKind::Task);
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
        assert_eq!(
            parse_task_status("in_progress").unwrap(),
            TaskStatus::InProgress
        );
        assert_eq!(parse_task_status("done").unwrap(), TaskStatus::Done);
    }

    #[test]
    fn parse_task_status_invalid() {
        assert!(parse_task_status("blocked").is_err());
    }

    #[test]
    fn parse_epic_status_valid() {
        assert_eq!(parse_epic_status("open").unwrap(), EpicStatus::Open);
        assert_eq!(
            parse_epic_status("in_progress").unwrap(),
            EpicStatus::InProgress
        );
        assert_eq!(parse_epic_status("done").unwrap(), EpicStatus::Done);
    }
}
