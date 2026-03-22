use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Configuration for a single MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Command to spawn (e.g., "npx", "uvx", "node").
    pub command: String,

    /// Arguments to the command.
    #[serde(default)]
    pub args: Vec<String>,

    /// Environment variables to set for the server process.
    /// Supports `${VAR}` expansion from the process environment.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Prefix for MCP tool names.
pub const MCP_NAME_PREFIX: &str = "mcp";

/// Separator between prefix, server ID, and tool name.
pub const MCP_PREFIX_SEP: &str = "__";

/// Build a prefixed MCP tool name: `mcp__{server_id}__{tool_name}`.
pub fn prefixed_tool_name(server_id: &str, tool_name: &str) -> String {
    format!("{MCP_NAME_PREFIX}{MCP_PREFIX_SEP}{server_id}{MCP_PREFIX_SEP}{tool_name}")
}

/// Parse a prefixed MCP tool name back into `(server_id, tool_name)`.
/// Returns `None` if the name doesn't match the expected format.
pub fn parse_prefixed_tool_name(prefixed: &str) -> Option<(&str, &str)> {
    let rest = prefixed.strip_prefix(MCP_NAME_PREFIX)?;
    let rest = rest.strip_prefix(MCP_PREFIX_SEP)?;
    let sep_pos = rest.find(MCP_PREFIX_SEP)?;
    let server_id = &rest[..sep_pos];
    let tool_name = &rest[sep_pos + MCP_PREFIX_SEP.len()..];
    if server_id.is_empty() || tool_name.is_empty() {
        return None;
    }
    Some((server_id, tool_name))
}

/// Validate that a server ID is safe for use in prefixed tool names.
/// Rejects IDs containing the separator (`__`) to avoid ambiguous parsing.
pub fn validate_server_id(server_id: &str) -> Result<(), String> {
    if server_id.is_empty() {
        return Err("MCP server ID cannot be empty".into());
    }
    if server_id.contains(MCP_PREFIX_SEP) {
        return Err(format!(
            "MCP server ID '{server_id}' must not contain '{MCP_PREFIX_SEP}' (double underscore)"
        ));
    }
    Ok(())
}

/// Expand `${VAR}` patterns in environment variable values from the process environment.
/// Missing vars are logged as warnings and left unexpanded.
pub fn expand_env(env: &HashMap<String, String>) -> HashMap<String, String> {
    env.iter()
        .map(|(key, value)| {
            let expanded = expand_env_value(value);
            (key.clone(), expanded)
        })
        .collect()
}

/// Expand `${VAR}` patterns in a single string value.
fn expand_env_value(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut var_name = String::new();
            for c in chars.by_ref() {
                if c == '}' {
                    break;
                }
                var_name.push(c);
            }
            match std::env::var(&var_name) {
                Ok(val) => result.push_str(&val),
                Err(_) => {
                    tracing::warn!(var = %var_name, "MCP env var not found, leaving unexpanded");
                    result.push_str(&format!("${{{var_name}}}"));
                }
            }
        } else {
            result.push(ch);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixed_tool_name_format() {
        assert_eq!(
            prefixed_tool_name("github", "search_repos"),
            "mcp__github__search_repos"
        );
    }

    #[test]
    fn parse_prefixed_tool_name_valid() {
        let (server, tool) = parse_prefixed_tool_name("mcp__github__search_repos").unwrap();
        assert_eq!(server, "github");
        assert_eq!(tool, "search_repos");
    }

    #[test]
    fn parse_prefixed_tool_name_with_underscores_in_tool() {
        let (server, tool) =
            parse_prefixed_tool_name("mcp__my_server__my_cool_tool").unwrap();
        assert_eq!(server, "my_server");
        assert_eq!(tool, "my_cool_tool");
    }

    #[test]
    fn parse_prefixed_tool_name_invalid() {
        assert!(parse_prefixed_tool_name("read").is_none());
        assert!(parse_prefixed_tool_name("mcp__").is_none());
        assert!(parse_prefixed_tool_name("mcp____tool").is_none());
        assert!(parse_prefixed_tool_name("notmcp__server__tool").is_none());
    }

    #[test]
    fn parse_roundtrip() {
        let prefixed = prefixed_tool_name("fs", "read_file");
        let (server, tool) = parse_prefixed_tool_name(&prefixed).unwrap();
        assert_eq!(server, "fs");
        assert_eq!(tool, "read_file");
    }

    #[test]
    fn validate_server_id_rejects_double_underscore() {
        assert!(validate_server_id("my__server").is_err());
        assert!(validate_server_id("a__b__c").is_err());
    }

    #[test]
    fn validate_server_id_rejects_empty() {
        assert!(validate_server_id("").is_err());
    }

    #[test]
    fn validate_server_id_accepts_valid() {
        assert!(validate_server_id("github").is_ok());
        assert!(validate_server_id("my_server").is_ok());
        assert!(validate_server_id("server-1").is_ok());
    }

    #[test]
    fn expand_env_value_no_vars() {
        assert_eq!(expand_env_value("hello world"), "hello world");
    }

    #[test]
    fn expand_env_value_with_known_var() {
        // SAFETY: test-only env vars with unique names, unlikely to race with other tests.
        unsafe { std::env::set_var("STEVE_TEST_VAR", "expanded") };
        assert_eq!(expand_env_value("${STEVE_TEST_VAR}"), "expanded");
        assert_eq!(
            expand_env_value("prefix-${STEVE_TEST_VAR}-suffix"),
            "prefix-expanded-suffix"
        );
        unsafe { std::env::remove_var("STEVE_TEST_VAR") };
    }

    #[test]
    fn expand_env_value_unknown_var_left_unexpanded() {
        let result = expand_env_value("${STEVE_NONEXISTENT_VAR_12345}");
        assert_eq!(result, "${STEVE_NONEXISTENT_VAR_12345}");
    }

    #[test]
    fn expand_env_map() {
        // SAFETY: test-only env var with unique name, unlikely to race with other tests.
        unsafe { std::env::set_var("STEVE_TEST_KEY", "val") };
        let mut env = HashMap::new();
        env.insert("TOKEN".into(), "${STEVE_TEST_KEY}".into());
        env.insert("PLAIN".into(), "no-expansion".into());

        let expanded = expand_env(&env);
        assert_eq!(expanded["TOKEN"], "val");
        assert_eq!(expanded["PLAIN"], "no-expansion");
        unsafe { std::env::remove_var("STEVE_TEST_KEY") };
    }

    #[test]
    fn mcp_server_config_deserialize() {
        let json = r#"{
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-github"],
            "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}" }
        }"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.command, "npx");
        assert_eq!(config.args, vec!["-y", "@modelcontextprotocol/server-github"]);
        assert_eq!(config.env["GITHUB_TOKEN"], "${GITHUB_TOKEN}");
    }

    #[test]
    fn mcp_server_config_minimal() {
        let json = r#"{"command": "my-server"}"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.command, "my-server");
        assert!(config.args.is_empty());
        assert!(config.env.is_empty());
    }
}
