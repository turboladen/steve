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
    ExportDebug { include_logs: bool },
    Help,
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
            "/exit" => Ok(Command::Exit),
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
            "/export-debug" => Ok(Command::ExportDebug { include_logs: false }),
            "/export-debug-with-logs" => Ok(Command::ExportDebug { include_logs: true }),
            "/help" => Ok(Command::Help),
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
            CommandInfo { name: "/init", description: "Create AGENTS.md" },
            CommandInfo { name: "/export-debug", description: "Export session as markdown" },
            CommandInfo { name: "/export-debug-with-logs", description: "Export session with logs" },
            CommandInfo { name: "/help", description: "Show help" },
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
        assert!(names.contains(&"/export-debug-with-logs"));
        assert_eq!(cmds.len(), 11);
    }

    #[test]
    fn parse_export_debug_commands() {
        assert_eq!(
            Command::parse("/export-debug").unwrap(),
            Command::ExportDebug { include_logs: false }
        );
        assert_eq!(
            Command::parse("/export-debug-with-logs").unwrap(),
            Command::ExportDebug { include_logs: true }
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
        assert_eq!(matches.len(), 11);
    }

    #[test]
    fn filter_commands_no_match() {
        let matches = Command::matching_commands("/zzz");
        assert!(matches.is_empty());
    }
}
