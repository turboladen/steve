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
    Help,
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
            "/help" => Ok(Command::Help),
            _ => Err(format!("Unknown command: {cmd}. Type /help for available commands.")),
        }
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
    fn parse_unknown_command() {
        assert!(Command::parse("/unknown").is_err());
    }
}
