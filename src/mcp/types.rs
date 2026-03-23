use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Configuration for a single MCP server.
///
/// Uses `#[serde(untagged)]` so that a JSON object with `"url"` deserializes as
/// `Http` and one with `"command"` deserializes as `Stdio`. **Http must come
/// first** — serde tries variants in declaration order.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpServerConfig {
    /// HTTP/SSE remote server — just needs a URL.
    Http {
        url: String,
        #[serde(default)]
        headers: Option<HashMap<String, String>>,
    },
    /// Stdio child-process server (existing behavior).
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
}

impl McpServerConfig {
    /// Whether this is an HTTP remote server.
    pub fn is_http(&self) -> bool {
        matches!(self, Self::Http { .. })
    }
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
            let mut found_closing_brace = false;
            for c in chars.by_ref() {
                if c == '}' {
                    found_closing_brace = true;
                    break;
                }
                var_name.push(c);
            }
            if found_closing_brace {
                match std::env::var(&var_name) {
                    Ok(val) => result.push_str(&val),
                    Err(_) => {
                        tracing::warn!(var = %var_name, "MCP env var not found, leaving unexpanded");
                        result.push_str(&format!("${{{var_name}}}"));
                    }
                }
            } else {
                // No closing brace found — treat the sequence as literal text
                result.push_str("${");
                result.push_str(&var_name);
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
    fn expand_env_value_unclosed_brace_treated_as_literal() {
        assert_eq!(expand_env_value("${UNCLOSED"), "${UNCLOSED");
        assert_eq!(expand_env_value("prefix-${NO_CLOSE"), "prefix-${NO_CLOSE");
        // Normal expansion still works alongside
        // SAFETY: test-only env var with unique name, unlikely to race with other tests.
        unsafe { std::env::set_var("STEVE_TEST_VAR", "ok") };
        assert_eq!(
            expand_env_value("${STEVE_TEST_VAR}-${UNCLOSED"),
            "ok-${UNCLOSED"
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
    fn mcp_server_config_stdio_deserialize() {
        let json = r#"{
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-github"],
            "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}" }
        }"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        match &config {
            McpServerConfig::Stdio { command, args, env } => {
                assert_eq!(command, "npx");
                assert_eq!(args, &vec!["-y".to_string(), "@modelcontextprotocol/server-github".to_string()]);
                assert_eq!(env["GITHUB_TOKEN"], "${GITHUB_TOKEN}");
            }
            McpServerConfig::Http { .. } => panic!("expected Stdio variant"),
        }
        assert!(!config.is_http());
    }

    #[test]
    fn mcp_server_config_minimal_stdio() {
        let json = r#"{"command": "my-server"}"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        match &config {
            McpServerConfig::Stdio { command, args, env } => {
                assert_eq!(command, "my-server");
                assert!(args.is_empty());
                assert!(env.is_empty());
            }
            McpServerConfig::Http { .. } => panic!("expected Stdio variant"),
        }
        assert!(!config.is_http());
    }

    #[test]
    fn mcp_server_config_http_deserialize() {
        let json = r#"{"url": "https://mcp.example.com/sse"}"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        match &config {
            McpServerConfig::Http { url, headers } => {
                assert_eq!(url, "https://mcp.example.com/sse");
                assert!(headers.is_none());
            }
            McpServerConfig::Stdio { .. } => panic!("expected Http variant"),
        }
        assert!(config.is_http());
    }

    #[test]
    fn mcp_server_config_http_with_headers() {
        let json = r#"{
            "url": "https://mcp.example.com/sse",
            "headers": { "Authorization": "Bearer tok_123" }
        }"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        match &config {
            McpServerConfig::Http { url, headers } => {
                assert_eq!(url, "https://mcp.example.com/sse");
                let h = headers.as_ref().expect("headers should be Some");
                assert_eq!(h["Authorization"], "Bearer tok_123");
            }
            McpServerConfig::Stdio { .. } => panic!("expected Http variant"),
        }
        assert!(config.is_http());
    }

    #[test]
    fn mcp_server_config_roundtrip_stdio() {
        let config = McpServerConfig::Stdio {
            command: "npx".into(),
            args: vec!["-y".into(), "server".into()],
            env: HashMap::from([("KEY".into(), "val".into())]),
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: McpServerConfig = serde_json::from_str(&json).unwrap();
        match back {
            McpServerConfig::Stdio { command, args, env } => {
                assert_eq!(command, "npx");
                assert_eq!(args, vec!["-y", "server"]);
                assert_eq!(env["KEY"], "val");
            }
            McpServerConfig::Http { .. } => panic!("expected Stdio variant"),
        }
    }

    #[test]
    fn mcp_server_config_roundtrip_http() {
        let config = McpServerConfig::Http {
            url: "https://example.com".into(),
            headers: Some(HashMap::from([("X-Key".into(), "abc".into())])),
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: McpServerConfig = serde_json::from_str(&json).unwrap();
        match back {
            McpServerConfig::Http { url, headers } => {
                assert_eq!(url, "https://example.com");
                assert_eq!(headers.unwrap()["X-Key"], "abc");
            }
            McpServerConfig::Stdio { .. } => panic!("expected Http variant"),
        }
    }
}
