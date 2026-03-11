//! Slash command enum — compile-time checked command dispatch.

/// Slash commands available in the TUI input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Exit,
    New,
    Rename(String),
    Models,
    Model(String),
    Init,
    Compact,
    Sessions,
    ExportDebug,
    Help,
    // Task management commands
    Tasks,
    TaskNew(String),
    TaskDone(String),
    TaskShow(String),
    TaskEdit(String),
    Epics,
    EpicNew(String),
}

/// Metadata for a known slash command, used for autocomplete.
#[derive(Debug, Clone)]
pub struct CommandInfo {
    /// The command string (e.g., "/exit").
    pub name: &'static str,
    /// Short description shown in autocomplete popup.
    pub description: &'static str,
}

impl Command {
    /// Parse a slash-command string (e.g. "/model openai/gpt-4o") into a `Command`.
    ///
    /// Returns `Err` with an error message for unknown commands.
    pub fn parse(text: &str) -> Result<Command, String> {
        let parts: Vec<&str> = text.splitn(2, ' ').collect();
        let cmd = parts[0];
        let arg = parts.get(1).map(|s| s.trim().to_string());

        match cmd {
            "/exit" | "/quit" => Ok(Command::Exit),
            "/new" => Ok(Command::New),
            "/rename" => match arg {
                Some(title) if !title.is_empty() => Ok(Command::Rename(title)),
                _ => Err("Usage: /rename <title>".to_string()),
            },
            "/models" => {
                // "/models <ref>" is an alias for "/model <ref>"
                match arg {
                    Some(model_ref) if !model_ref.is_empty() => Ok(Command::Model(model_ref)),
                    _ => Ok(Command::Models),
                }
            }
            "/model" => match arg {
                Some(model_ref) if !model_ref.is_empty() => Ok(Command::Model(model_ref)),
                _ => Ok(Command::Models),
            },
            "/init" => Ok(Command::Init),
            "/compact" => Ok(Command::Compact),
            "/sessions" => Ok(Command::Sessions),
            "/export-debug" => Ok(Command::ExportDebug),
            "/help" => Ok(Command::Help),
            // Task management commands
            "/tasks" => Ok(Command::Tasks),
            "/task-new" => match arg {
                Some(title) if !title.is_empty() => Ok(Command::TaskNew(title)),
                _ => Err("Usage: /task-new <title>".to_string()),
            },
            "/task-done" => match arg {
                Some(id) if !id.is_empty() => Ok(Command::TaskDone(id)),
                _ => Err("Usage: /task-done <task-id>".to_string()),
            },
            "/task-show" => match arg {
                Some(id) if !id.is_empty() => Ok(Command::TaskShow(id)),
                _ => Err("Usage: /task-show <task-id>".to_string()),
            },
            "/task-edit" => match arg {
                Some(args) if !args.is_empty() => Ok(Command::TaskEdit(args)),
                _ => Err("Usage: /task-edit <task-id> <field>=<value> ...".to_string()),
            },
            "/epics" => Ok(Command::Epics),
            "/epic-new" => match arg {
                Some(title) if !title.is_empty() => Ok(Command::EpicNew(title)),
                _ => Err("Usage: /epic-new <title>".to_string()),
            },
            _ => Err(format!("Unknown command: {cmd}. Type /help for available commands.")),
        }
    }

    /// Returns metadata for all known commands, in display order.
    pub fn all_commands() -> Vec<CommandInfo> {
        vec![
            CommandInfo { name: "/new", description: "Start a new session" },
            CommandInfo { name: "/rename", description: "Rename current session" },
            CommandInfo { name: "/model", description: "Switch model" },
            CommandInfo { name: "/models", description: "List available models" },
            CommandInfo { name: "/compact", description: "Compact conversation" },
            CommandInfo { name: "/sessions", description: "Browse sessions" },
            CommandInfo { name: "/tasks", description: "List all tasks" },
            CommandInfo { name: "/task-new", description: "Create a task" },
            CommandInfo { name: "/task-done", description: "Complete a task" },
            CommandInfo { name: "/task-show", description: "Show task details" },
            CommandInfo { name: "/task-edit", description: "Edit a task" },
            CommandInfo { name: "/epics", description: "List epics" },
            CommandInfo { name: "/epic-new", description: "Create an epic" },
            CommandInfo { name: "/init", description: "Create AGENTS.md" },
            CommandInfo { name: "/export-debug", description: "Export session with logs" },
            CommandInfo { name: "/help", description: "Show help" },
            CommandInfo { name: "/quit", description: "Quit" },
            CommandInfo { name: "/exit", description: "Quit" },
        ]
    }

    /// Returns commands matching the given prefix (case-sensitive).
    pub fn matching_commands(prefix: &str) -> Vec<CommandInfo> {
        Self::all_commands()
            .into_iter()
            .filter(|c| c.name.starts_with(prefix))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_commands() {
        assert_eq!(Command::parse("/exit").unwrap(), Command::Exit);
        assert_eq!(Command::parse("/quit").unwrap(), Command::Exit);
        assert_eq!(Command::parse("/new").unwrap(), Command::New);
        assert_eq!(Command::parse("/init").unwrap(), Command::Init);
        assert_eq!(Command::parse("/compact").unwrap(), Command::Compact);
        assert_eq!(Command::parse("/help").unwrap(), Command::Help);
    }

    #[test]
    fn parse_rename_with_argument() {
        assert_eq!(
            Command::parse("/rename My Session").unwrap(),
            Command::Rename("My Session".to_string())
        );
    }

    #[test]
    fn parse_rename_without_argument() {
        assert!(Command::parse("/rename").is_err());
        assert!(Command::parse("/rename   ").is_err());
    }

    #[test]
    fn parse_model_commands() {
        assert_eq!(Command::parse("/models").unwrap(), Command::Models);
        assert_eq!(
            Command::parse("/model openai/gpt-4o").unwrap(),
            Command::Model("openai/gpt-4o".to_string())
        );
        // /model without arg lists models
        assert_eq!(Command::parse("/model").unwrap(), Command::Models);
        // /models with arg switches model
        assert_eq!(
            Command::parse("/models openai/gpt-4o").unwrap(),
            Command::Model("openai/gpt-4o".to_string())
        );
    }

    #[test]
    fn parse_sessions_command() {
        assert_eq!(Command::parse("/sessions").unwrap(), Command::Sessions);
    }

    #[test]
    fn parse_unknown_command() {
        assert!(Command::parse("/unknown").is_err());
    }

    #[test]
    fn all_commands_returns_all_entries() {
        let cmds = Command::all_commands();
        let names: Vec<&str> = cmds.iter().map(|c| c.name).collect();
        assert!(names.contains(&"/exit"));
        assert!(names.contains(&"/new"));
        assert!(names.contains(&"/rename"));
        assert!(names.contains(&"/models"));
        assert!(names.contains(&"/model"));
        assert!(names.contains(&"/init"));
        assert!(names.contains(&"/compact"));
        assert!(names.contains(&"/sessions"));
        assert!(names.contains(&"/help"));
        assert!(names.contains(&"/export-debug"));
        assert!(names.contains(&"/quit"));
        assert!(names.contains(&"/tasks"));
        assert!(names.contains(&"/task-new"));
        assert!(names.contains(&"/task-done"));
        assert!(names.contains(&"/task-show"));
        assert!(names.contains(&"/epics"));
        assert!(names.contains(&"/task-edit"));
        assert!(names.contains(&"/epic-new"));
        assert_eq!(cmds.len(), 18);
    }

    #[test]
    fn parse_export_debug_command() {
        assert_eq!(
            Command::parse("/export-debug").unwrap(),
            Command::ExportDebug
        );
    }

    #[test]
    fn filter_commands_by_prefix() {
        let matches = Command::matching_commands("/m");
        let names: Vec<&str> = matches.iter().map(|c| c.name).collect();
        assert!(names.contains(&"/models"));
        assert!(names.contains(&"/model"));
        assert!(!names.contains(&"/exit"));
    }

    #[test]
    fn filter_commands_slash_only() {
        let matches = Command::matching_commands("/");
        assert_eq!(matches.len(), 18);
    }

    #[test]
    fn filter_commands_no_match() {
        let matches = Command::matching_commands("/zzz");
        assert!(matches.is_empty());
    }

    // -- Task command tests --

    #[test]
    fn parse_tasks_command() {
        assert_eq!(Command::parse("/tasks").unwrap(), Command::Tasks);
    }

    #[test]
    fn parse_task_new_with_title() {
        assert_eq!(
            Command::parse("/task-new Fix the bug").unwrap(),
            Command::TaskNew("Fix the bug".to_string())
        );
    }

    #[test]
    fn parse_task_new_without_title() {
        assert!(Command::parse("/task-new").is_err());
    }

    #[test]
    fn parse_task_done_with_id() {
        assert_eq!(
            Command::parse("/task-done task-abc123").unwrap(),
            Command::TaskDone("task-abc123".to_string())
        );
    }

    #[test]
    fn parse_task_done_without_id() {
        assert!(Command::parse("/task-done").is_err());
    }

    #[test]
    fn parse_task_show_with_id() {
        assert_eq!(
            Command::parse("/task-show task-abc123").unwrap(),
            Command::TaskShow("task-abc123".to_string())
        );
    }

    #[test]
    fn parse_task_edit_with_args() {
        assert_eq!(
            Command::parse("/task-edit task-abc123 title=New Title").unwrap(),
            Command::TaskEdit("task-abc123 title=New Title".to_string())
        );
    }

    #[test]
    fn parse_task_edit_without_args() {
        assert!(Command::parse("/task-edit").is_err());
    }

    #[test]
    fn parse_epics_command() {
        assert_eq!(Command::parse("/epics").unwrap(), Command::Epics);
    }

    #[test]
    fn parse_epic_new_with_title() {
        assert_eq!(
            Command::parse("/epic-new Auth Overhaul").unwrap(),
            Command::EpicNew("Auth Overhaul".to_string())
        );
    }

    #[test]
    fn parse_epic_new_without_title() {
        assert!(Command::parse("/epic-new").is_err());
    }

    #[test]
    fn filter_commands_task_prefix() {
        let matches = Command::matching_commands("/task");
        let names: Vec<&str> = matches.iter().map(|c| c.name).collect();
        assert!(names.contains(&"/tasks"));
        assert!(names.contains(&"/task-new"));
        assert!(names.contains(&"/task-done"));
        assert!(names.contains(&"/task-show"));
        assert!(names.contains(&"/task-edit"));
        assert_eq!(names.len(), 5);
    }
}
