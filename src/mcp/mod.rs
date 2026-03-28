//! MCP (Model Context Protocol) client integration.
//!
//! Manages connections to MCP servers that provide dynamic tools and resources.
//! MCP tools bypass the `ToolName` enum entirely — they have their own registry
//! and execution path, integrated surgically into `stream.rs`.

mod manager;
pub mod oauth;
mod server;
mod transport;

pub use manager::McpManager;

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Configuration for a single MCP server.
///
/// Uses `#[serde(untagged)]` so that a JSON object with `"url"` deserializes as
/// `Http` and one with `"command"` deserializes as `Stdio`. **Http must come
/// first** — serde tries variants in declaration order.
///
/// # Examples
///
/// GitHub MCP server (just a URL — Steve has built-in OAuth credentials):
///
/// ```
/// # use steve::mcp::McpServerConfig;
/// let config: McpServerConfig = serde_json::from_str(r#"{
///     "url": "https://api.githubcopilot.com/mcp/"
/// }"#).unwrap();
/// assert!(config.is_http());
/// ```
///
/// Remote server with a custom OAuth client_id (for servers that don't
/// support dynamic client registration and aren't in Steve's built-in list):
///
/// ```
/// # use steve::mcp::McpServerConfig;
/// let config: McpServerConfig = serde_json::from_str(r#"{
///     "url": "https://mcp.example.com",
///     "client_id": "my-app-client-id"
/// }"#).unwrap();
/// assert!(config.is_http());
/// ```
///
/// Remote server with a static bearer token:
///
/// ```
/// # use steve::mcp::McpServerConfig;
/// let config: McpServerConfig = serde_json::from_str(r#"{
///     "url": "https://mcp.example.com",
///     "headers": { "Authorization": "Bearer ${MCP_TOKEN}" }
/// }"#).unwrap();
/// assert!(config.is_http());
/// ```
///
/// Local stdio server (child process):
///
/// ```
/// # use steve::mcp::McpServerConfig;
/// let config: McpServerConfig = serde_json::from_str(r#"{
///     "command": "npx",
///     "args": ["-y", "@modelcontextprotocol/server-github"],
///     "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}" }
/// }"#).unwrap();
/// assert!(!config.is_http());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpServerConfig {
    /// HTTP/SSE remote server — just needs a URL.
    Http {
        url: String,
        /// Optional static headers (e.g., for pre-configured bearer tokens).
        /// Values support `${VAR}` expansion.
        #[serde(default)]
        headers: Option<HashMap<String, String>>,
        /// Optional OAuth client_id for servers that don't support dynamic
        /// client registration (RFC 7591). If omitted, Steve tries dynamic
        /// registration first, then fails if the server doesn't support it.
        #[serde(default)]
        client_id: Option<String>,
        /// Optional OAuth client_secret. Required by some providers (e.g.,
        /// GitHub OAuth Apps). Supports `${VAR}` expansion for secrets stored
        /// in environment variables.
        #[serde(default)]
        client_secret: Option<String>,
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

/// Expand `${VAR}` patterns in a single string value (public API).
pub fn expand_env_single(value: &str) -> String {
    expand_env_value(value)
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

/// Lock-free snapshot of MCP tool metadata.
///
/// Populated after `start_servers()` completes and shared via `Arc` so that
/// `stream.rs` can check tool existence and build tool definitions without
/// locking the `McpManager` mutex. This avoids holding the mutex across
/// the LLM API call or during server initialization.
#[derive(Clone, Default)]
pub struct McpToolSnapshot {
    /// Set of all prefixed MCP tool names for O(1) lookup.
    pub(crate) known_tools: HashSet<String>,
    /// Pre-built OpenAI-compatible tool definitions.
    pub(crate) tool_defs: Vec<Value>,
}

impl McpToolSnapshot {
    /// Check whether a prefixed tool name is a known MCP tool.
    pub fn has_tool(&self, prefixed_name: &str) -> bool {
        self.known_tools.contains(prefixed_name)
    }

    /// Get pre-built OpenAI function tool definitions for all MCP tools.
    pub fn tool_definitions(&self) -> &[Value] {
        &self.tool_defs
    }

    /// Whether any MCP tools are available.
    pub fn is_empty(&self) -> bool {
        self.known_tools.is_empty()
    }
}

/// Build a human-readable permission summary for an MCP tool call.
pub fn mcp_permission_summary(prefixed_name: &str, args: &Value) -> String {
    if let Some((server_id, tool_name)) = parse_prefixed_tool_name(prefixed_name) {
        let args_preview = serde_json::to_string(args).unwrap_or_default();
        let truncated = if args_preview.len() > 80 {
            let end = crate::floor_char_boundary(&args_preview, 80);
            format!("{}...", &args_preview[..end])
        } else {
            args_preview
        };
        format!("MCP: {server_id}/{tool_name}({truncated})")
    } else {
        format!("MCP: {prefixed_name}")
    }
}

/// Build MCP resource content for injection into the system prompt.
/// Truncates large resources to `max_chars` (same pattern as project memory).
pub async fn build_resource_context(manager: &McpManager, max_chars: usize) -> Option<String> {
    let resources = manager.all_resources();
    if resources.is_empty() {
        return None;
    }

    let mut section = String::from("\n## MCP Context\n");
    let mut total_len = 0;

    for (server_id, resource) in &resources {
        let name = &resource.name;
        let uri = resource.uri.as_str();

        match manager.read_resource(server_id, uri).await {
            Ok(content) if !content.trim().is_empty() => {
                let truncated = if content.len() > max_chars {
                    let end = crate::floor_char_boundary(&content, max_chars);
                    format!("{}...\n(truncated)", &content[..end])
                } else {
                    content
                };

                let entry = format!("\n### {server_id}: {name}\n\n{truncated}\n");
                total_len += entry.len();
                if total_len > max_chars * 3 {
                    section.push_str("\n(additional resources omitted for context budget)\n");
                    break;
                }
                section.push_str(&entry);
            }
            Ok(_) => {} // empty content, skip
            Err(e) => {
                tracing::debug!(server = %server_id, resource = %name, error = %e, "failed to read MCP resource");
            }
        }
    }

    if section.len() > "\n## MCP Context\n".len() {
        Some(section)
    } else {
        None
    }
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
        let (server, tool) = parse_prefixed_tool_name("mcp__my_server__my_cool_tool").unwrap();
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
                assert_eq!(
                    args,
                    &vec![
                        "-y".to_string(),
                        "@modelcontextprotocol/server-github".to_string()
                    ]
                );
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
            McpServerConfig::Http { url, headers, .. } => {
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
            McpServerConfig::Http { url, headers, .. } => {
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
    fn mcp_server_config_ambiguous_picks_http() {
        let json = r#"{"url": "https://example.com", "command": "npx"}"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config, McpServerConfig::Http { .. }));
    }

    #[test]
    fn mcp_server_config_roundtrip_http() {
        let config = McpServerConfig::Http {
            url: "https://example.com".into(),
            headers: Some(HashMap::from([("X-Key".into(), "abc".into())])),
            client_id: None,
            client_secret: None,
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: McpServerConfig = serde_json::from_str(&json).unwrap();
        match back {
            McpServerConfig::Http { url, headers, .. } => {
                assert_eq!(url, "https://example.com");
                assert_eq!(headers.unwrap()["X-Key"], "abc");
            }
            McpServerConfig::Stdio { .. } => panic!("expected Http variant"),
        }
    }

    #[test]
    fn mcp_permission_summary_valid() {
        let summary = mcp_permission_summary(
            "mcp__github__search_repos",
            &serde_json::json!({"query": "rust"}),
        );
        assert!(summary.contains("github"));
        assert!(summary.contains("search_repos"));
    }

    #[test]
    fn mcp_permission_summary_truncates_safely() {
        let long_val = "a".repeat(75) + "🦀🦀🦀";
        let args = serde_json::json!({"key": long_val});
        let summary = mcp_permission_summary("mcp__s__t", &args);
        assert!(summary.contains("..."));
    }

    #[test]
    fn tool_snapshot_empty_by_default() {
        let snap = McpToolSnapshot::default();
        assert!(snap.is_empty());
        assert!(!snap.has_tool("mcp__x__y"));
        assert!(snap.tool_definitions().is_empty());
    }

    #[test]
    fn has_tool_returns_false_for_native_tools() {
        let snap = McpToolSnapshot::default();
        assert!(!snap.has_tool("read"));
        assert!(!snap.has_tool("edit"));
        assert!(!snap.has_tool("bash"));
    }

    #[test]
    fn has_tool_returns_false_for_missing_server() {
        let snap = McpToolSnapshot::default();
        assert!(!snap.has_tool("mcp__ghost__some_tool"));
    }
}
