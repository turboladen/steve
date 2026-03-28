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
    Diagnostics,
    AgentsUpdate,
    // MCP commands
    Mcp,
    McpTools(Option<String>),
    McpResources(Option<String>),
    McpPrompts(Option<String>),
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
            "/diagnostics" => Ok(Command::Diagnostics),
            "/agents-update" => Ok(Command::AgentsUpdate),
            "/mcp" => match arg {
                None => Ok(Command::Mcp),
                Some(rest) => {
                    let rest = rest.trim();
                    if rest.is_empty() {
                        return Ok(Command::Mcp);
                    }
                    let sub_parts: Vec<&str> = rest.splitn(2, ' ').collect();
                    let sub_arg = sub_parts
                        .get(1)
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                    match sub_parts[0] {
                        "tools" => Ok(Command::McpTools(sub_arg)),
                        "resources" => Ok(Command::McpResources(sub_arg)),
                        "prompts" => Ok(Command::McpPrompts(sub_arg)),
                        other => Err(format!(
                            "Unknown /mcp subcommand: {other}. Available: tools, resources, prompts"
                        )),
                    }
                }
            },
            "/epics" => Ok(Command::Epics),
            "/epic-new" => match arg {
                Some(title) if !title.is_empty() => Ok(Command::EpicNew(title)),
                _ => Err("Usage: /epic-new <title>".to_string()),
            },
            _ => Err(format!(
                "Unknown command: {cmd}. Type /help for available commands."
            )),
        }
    }

    /// Returns metadata for all known commands, in display order.
    pub fn all_commands() -> Vec<CommandInfo> {
        vec![
            CommandInfo {
                name: "/new",
                description: "Start a new session",
            },
            CommandInfo {
                name: "/rename",
                description: "Rename current session",
            },
            CommandInfo {
                name: "/model",
                description: "Switch model",
            },
            CommandInfo {
                name: "/models",
                description: "List available models",
            },
            CommandInfo {
                name: "/compact",
                description: "Compact conversation",
            },
            CommandInfo {
                name: "/sessions",
                description: "Browse sessions",
            },
            CommandInfo {
                name: "/tasks",
                description: "List all tasks",
            },
            CommandInfo {
                name: "/task-new",
                description: "Create a task",
            },
            CommandInfo {
                name: "/task-done",
                description: "Complete a task",
            },
            CommandInfo {
                name: "/task-show",
                description: "Show task details",
            },
            CommandInfo {
                name: "/task-edit",
                description: "Edit a task",
            },
            CommandInfo {
                name: "/epics",
                description: "List epics",
            },
            CommandInfo {
                name: "/epic-new",
                description: "Create an epic",
            },
            CommandInfo {
                name: "/mcp",
                description: "MCP server overview",
            },
            CommandInfo {
                name: "/mcp tools",
                description: "Browse MCP tools",
            },
            CommandInfo {
                name: "/mcp resources",
                description: "Browse MCP resources",
            },
            CommandInfo {
                name: "/mcp prompts",
                description: "Browse MCP prompts",
            },
            CommandInfo {
                name: "/diagnostics",
                description: "Show health dashboard",
            },
            CommandInfo {
                name: "/agents-update",
                description: "Update AGENTS.md",
            },
            CommandInfo {
                name: "/init",
                description: "Create AGENTS.md",
            },
            CommandInfo {
                name: "/export-debug",
                description: "Export session with logs",
            },
            CommandInfo {
                name: "/help",
                description: "Show help",
            },
            CommandInfo {
                name: "/quit",
                description: "Quit",
            },
            CommandInfo {
                name: "/exit",
                description: "Quit",
            },
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
        assert!(names.contains(&"/diagnostics"));
        assert!(names.contains(&"/agents-update"));
        assert!(names.contains(&"/mcp"));
        assert!(names.contains(&"/mcp tools"));
        assert!(names.contains(&"/mcp resources"));
        assert!(names.contains(&"/mcp prompts"));
        assert_eq!(cmds.len(), 24);
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
        assert_eq!(matches.len(), 24);
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
    fn parse_diagnostics_command() {
        assert_eq!(
            Command::parse("/diagnostics").unwrap(),
            Command::Diagnostics
        );
    }

    #[test]
    fn parse_agents_update_command() {
        assert_eq!(
            Command::parse("/agents-update").unwrap(),
            Command::AgentsUpdate
        );
    }

    // -- MCP command tests --

    #[test]
    fn parse_mcp_overview() {
        assert_eq!(Command::parse("/mcp").unwrap(), Command::Mcp);
    }

    #[test]
    fn parse_mcp_tools() {
        assert_eq!(
            Command::parse("/mcp tools").unwrap(),
            Command::McpTools(None)
        );
    }

    #[test]
    fn parse_mcp_tools_with_server() {
        assert_eq!(
            Command::parse("/mcp tools github").unwrap(),
            Command::McpTools(Some("github".to_string()))
        );
    }

    #[test]
    fn parse_mcp_resources() {
        assert_eq!(
            Command::parse("/mcp resources").unwrap(),
            Command::McpResources(None)
        );
    }

    #[test]
    fn parse_mcp_resources_with_server() {
        assert_eq!(
            Command::parse("/mcp resources github").unwrap(),
            Command::McpResources(Some("github".to_string()))
        );
    }

    #[test]
    fn parse_mcp_prompts() {
        assert_eq!(
            Command::parse("/mcp prompts").unwrap(),
            Command::McpPrompts(None)
        );
    }

    #[test]
    fn parse_mcp_prompts_with_server() {
        assert_eq!(
            Command::parse("/mcp prompts github").unwrap(),
            Command::McpPrompts(Some("github".to_string()))
        );
    }

    #[test]
    fn parse_mcp_trailing_spaces() {
        // "/mcp   " should parse as Mcp, not as an unknown subcommand.
        assert_eq!(Command::parse("/mcp   ").unwrap(), Command::Mcp);
    }

    #[test]
    fn parse_mcp_unknown_subcommand() {
        assert!(Command::parse("/mcp foobar").is_err());
    }

    #[test]
    fn filter_commands_mcp_prefix() {
        let matches = Command::matching_commands("/mcp");
        let names: Vec<&str> = matches.iter().map(|c| c.name).collect();
        assert!(names.contains(&"/mcp"));
        assert!(names.contains(&"/mcp tools"));
        assert!(names.contains(&"/mcp resources"));
        assert!(names.contains(&"/mcp prompts"));
        assert_eq!(names.len(), 4);
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
