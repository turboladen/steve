//! MCP (Model Context Protocol) client integration.
//!
//! Manages connections to MCP servers that provide dynamic tools and resources.
//! MCP tools bypass the `ToolName` enum entirely — they have their own registry
//! and execution path, integrated surgically into `stream.rs`.

pub mod types;

use std::collections::HashMap;

use anyhow::{Context, Result};
use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, RawContent, ReadResourceRequestParams, Resource,
    ResourceContents, Tool,
};
use rmcp::service::{Peer, RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;
use serde_json::Value;

use types::{McpServerConfig, expand_env, parse_prefixed_tool_name, prefixed_tool_name};

/// A connected MCP server instance wrapping an `rmcp` client session.
struct McpServer {
    server_id: String,
    /// The running rmcp service (holds the peer + cancel token).
    service: RunningService<RoleClient, ()>,
    /// Cached tool definitions from the server.
    cached_tools: Vec<Tool>,
    /// Cached resource list from the server.
    cached_resources: Vec<Resource>,
}

impl McpServer {
    /// Spawn a child process, complete the MCP handshake, and cache tool definitions.
    async fn spawn(server_id: String, config: &McpServerConfig) -> Result<Self> {
        let expanded_env = expand_env(&config.env);

        // Build the child process command
        let mut cmd = tokio::process::Command::new(&config.command);
        cmd.args(&config.args);
        for (key, value) in &expanded_env {
            cmd.env(key, value);
        }

        // Spawn via rmcp's TokioChildProcess transport
        let transport = TokioChildProcess::new(cmd)
            .context("failed to spawn MCP server process")?;

        // Perform the MCP handshake — `()` implements the default ClientHandler
        let service: RunningService<RoleClient, ()> = ().serve(transport).await
            .map_err(|e| anyhow::anyhow!("MCP handshake failed for '{server_id}': {e}"))?;

        // Cache tool definitions
        let cached_tools = service
            .peer()
            .list_all_tools()
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(server = %server_id, error = %e, "failed to list MCP tools");
                Vec::new()
            });

        // Cache resource list (best-effort)
        let cached_resources = service
            .peer()
            .list_all_resources()
            .await
            .unwrap_or_else(|e| {
                tracing::debug!(server = %server_id, error = %e, "failed to list MCP resources (may not be supported)");
                Vec::new()
            });

        tracing::info!(
            server = %server_id,
            tools = cached_tools.len(),
            resources = cached_resources.len(),
            "MCP server connected"
        );

        Ok(Self {
            server_id,
            service,
            cached_tools,
            cached_resources,
        })
    }

    fn peer(&self) -> &Peer<RoleClient> {
        self.service.peer()
    }

    /// Call a tool on this server.
    async fn call_tool(&self, name: &str, args: Option<serde_json::Map<String, Value>>) -> Result<CallToolResult> {
        let mut params = CallToolRequestParams::new(name.to_string());
        if let Some(args) = args {
            params = params.with_arguments(args);
        }
        self.peer()
            .call_tool(params)
            .await
            .map_err(|e| anyhow::anyhow!("MCP tool call failed on '{}': {e}", self.server_id))
    }

    /// Read a resource by URI.
    async fn read_resource(&self, uri: &str) -> Result<String> {
        let params = ReadResourceRequestParams::new(uri);
        let result = self
            .peer()
            .read_resource(params)
            .await
            .map_err(|e| anyhow::anyhow!("MCP read_resource failed on '{}': {e}", self.server_id))?;

        // Concatenate all text content from the resource
        let text: String = result
            .contents
            .iter()
            .filter_map(|rc| match rc {
                ResourceContents::TextResourceContents { text, .. } => Some(text.as_str()),
                ResourceContents::BlobResourceContents { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(text)
    }
}

/// Singleton coordinator for all MCP server connections.
pub struct McpManager {
    servers: HashMap<String, McpServer>,
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            servers: HashMap::new(),
        }
    }

    /// Spawn all configured MCP servers. Failures are logged per-server; healthy
    /// servers continue. This should be called from a background task.
    pub async fn start_servers(&mut self, configs: &HashMap<String, McpServerConfig>) {
        for (server_id, config) in configs {
            match McpServer::spawn(server_id.clone(), config).await {
                Ok(server) => {
                    self.servers.insert(server_id.clone(), server);
                }
                Err(e) => {
                    tracing::error!(server = %server_id, error = %e, "failed to start MCP server");
                }
            }
        }
    }

    /// Whether any MCP servers are connected.
    pub fn has_servers(&self) -> bool {
        !self.servers.is_empty()
    }

    /// Aggregate tool definitions from all servers, paired with their server ID.
    pub fn all_tool_defs(&self) -> Vec<(&str, &Tool)> {
        self.servers
            .iter()
            .flat_map(|(id, server)| {
                server.cached_tools.iter().map(move |tool| (id.as_str(), tool))
            })
            .collect()
    }

    /// Build OpenAI-compatible function tool definitions for all MCP tools.
    /// Tool names are prefixed as `mcp__{server_id}__{tool_name}`.
    pub fn tool_definitions(&self) -> Vec<Value> {
        self.all_tool_defs()
            .into_iter()
            .map(|(server_id, tool)| {
                let prefixed = prefixed_tool_name(server_id, &tool.name);
                let description = tool
                    .description
                    .as_deref()
                    .unwrap_or("MCP tool");
                let parameters = tool.schema_as_json_value();
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": prefixed,
                        "description": description,
                        "parameters": parameters,
                    }
                })
            })
            .collect()
    }

    /// Check whether a prefixed tool name belongs to an MCP server.
    pub fn has_tool(&self, prefixed_name: &str) -> bool {
        if let Some((server_id, tool_name)) = parse_prefixed_tool_name(prefixed_name) {
            self.servers.get(server_id).is_some_and(|s| {
                s.cached_tools.iter().any(|t| t.name.as_ref() == tool_name)
            })
        } else {
            false
        }
    }

    /// Call an MCP tool by its prefixed name. Returns the text result.
    pub async fn call_tool(&self, prefixed_name: &str, args: Value) -> Result<String> {
        let (server_id, tool_name) = parse_prefixed_tool_name(prefixed_name)
            .ok_or_else(|| anyhow::anyhow!("invalid MCP tool name: {prefixed_name}"))?;

        let server = self
            .servers
            .get(server_id)
            .ok_or_else(|| anyhow::anyhow!("MCP server '{server_id}' not found"))?;

        let args_map = match args {
            Value::Object(map) => Some(map),
            Value::Null => None,
            _ => Some(serde_json::Map::from_iter([(
                "input".to_string(),
                args,
            )])),
        };

        let result = server.call_tool(tool_name, args_map).await?;
        Ok(format_call_result(&result))
    }

    /// Whether a tool call result was an error.
    pub async fn call_tool_raw(&self, prefixed_name: &str, args: Value) -> Result<(String, bool)> {
        let (server_id, tool_name) = parse_prefixed_tool_name(prefixed_name)
            .ok_or_else(|| anyhow::anyhow!("invalid MCP tool name: {prefixed_name}"))?;

        let server = self
            .servers
            .get(server_id)
            .ok_or_else(|| anyhow::anyhow!("MCP server '{server_id}' not found"))?;

        let args_map = match args {
            Value::Object(map) => Some(map),
            Value::Null => None,
            _ => None,
        };

        let result = server.call_tool(tool_name, args_map).await?;
        let is_error = result.is_error.unwrap_or(false);
        Ok((format_call_result(&result), is_error))
    }

    /// Aggregate resources from all servers.
    pub fn all_resources(&self) -> Vec<(&str, &Resource)> {
        self.servers
            .iter()
            .flat_map(|(id, server)| {
                server.cached_resources.iter().map(move |r| (id.as_str(), r))
            })
            .collect()
    }

    /// Read a specific resource from a server.
    pub async fn read_resource(&self, server_id: &str, uri: &str) -> Result<String> {
        let server = self
            .servers
            .get(server_id)
            .ok_or_else(|| anyhow::anyhow!("MCP server '{server_id}' not found"))?;
        server.read_resource(uri).await
    }

    /// Refresh cached resources from all servers (e.g., on /new or session switch).
    pub async fn refresh_resources(&mut self) {
        for (server_id, server) in &mut self.servers {
            match server.peer().list_all_resources().await {
                Ok(resources) => server.cached_resources = resources,
                Err(e) => {
                    tracing::debug!(server = %server_id, error = %e, "failed to refresh MCP resources");
                }
            }
        }
    }

    /// Graceful shutdown of all servers.
    pub async fn shutdown(&mut self) {
        for (server_id, server) in self.servers.drain() {
            tracing::info!(server = %server_id, "shutting down MCP server");
            // cancel() consumes self and returns the quit reason
            let _ = server.service.cancel().await;
        }
    }

    /// Summary of connected servers for status display.
    pub fn server_summary(&self) -> Vec<String> {
        self.servers
            .iter()
            .map(|(id, server)| {
                format!(
                    "{id} ({} tools, {} resources)",
                    server.cached_tools.len(),
                    server.cached_resources.len()
                )
            })
            .collect()
    }
}

/// Extract text content from a `CallToolResult`.
fn format_call_result(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|content| match &content.raw {
            RawContent::Text(text) => Some(text.text.as_str()),
            RawContent::Image(_) => Some("[image content]"),
            RawContent::Audio(_) => Some("[audio content]"),
            RawContent::Resource(_) => Some("[embedded resource]"),
            RawContent::ResourceLink(_) => Some("[resource link]"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build a human-readable permission summary for an MCP tool call.
pub fn mcp_permission_summary(prefixed_name: &str, args: &Value) -> String {
    if let Some((server_id, tool_name)) = parse_prefixed_tool_name(prefixed_name) {
        let args_preview = serde_json::to_string(args)
            .unwrap_or_default();
        let truncated = if args_preview.len() > 80 {
            format!("{}...", &args_preview[..80])
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
                    let mut end = max_chars;
                    while end > 0 && !content.is_char_boundary(end) {
                        end -= 1;
                    }
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
    use rmcp::model::RawTextContent;

    fn text_content(text: &str) -> rmcp::model::Content {
        rmcp::model::Annotated::new(
            RawContent::Text(RawTextContent {
                text: text.into(),
                meta: None,
            }),
            None,
        )
    }

    #[test]
    fn format_call_result_text_content() {
        let result = CallToolResult::success(vec![text_content("hello world")]);
        assert_eq!(format_call_result(&result), "hello world");
    }

    #[test]
    fn format_call_result_multiple_contents() {
        let result = CallToolResult::success(vec![
            text_content("line 1"),
            text_content("line 2"),
        ]);
        assert_eq!(format_call_result(&result), "line 1\nline 2");
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
    fn mcp_manager_new_has_no_servers() {
        let mgr = McpManager::new();
        assert!(!mgr.has_servers());
        assert!(mgr.all_tool_defs().is_empty());
        assert!(mgr.tool_definitions().is_empty());
        assert!(mgr.all_resources().is_empty());
    }

    #[test]
    fn has_tool_returns_false_for_native_tools() {
        let mgr = McpManager::new();
        assert!(!mgr.has_tool("read"));
        assert!(!mgr.has_tool("edit"));
        assert!(!mgr.has_tool("bash"));
    }

    #[test]
    fn has_tool_returns_false_for_missing_server() {
        let mgr = McpManager::new();
        assert!(!mgr.has_tool("mcp__ghost__some_tool"));
    }
}
