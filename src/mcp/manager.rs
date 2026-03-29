use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use rmcp::model::{CallToolResult, Prompt, RawContent, Resource, Tool};
use serde_json::Value;

use super::{
    McpServerConfig, McpToolSnapshot, oauth, parse_prefixed_tool_name, prefixed_tool_name,
    server::McpServer, validate_server_id,
};

pub struct McpManager {
    servers: HashMap<String, McpServer>,
    failed_servers: HashMap<String, String>,
    snapshot: Arc<McpToolSnapshot>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            servers: HashMap::new(),
            failed_servers: HashMap::new(),
            snapshot: Arc::new(McpToolSnapshot::default()),
        }
    }

    pub async fn start_servers(
        &mut self,
        configs: &HashMap<String, McpServerConfig>,
        data_dir: Option<&std::path::Path>,
        status_tx: Option<oauth::OAuthStatusTx>,
    ) {
        let credential_dir = data_dir.map(|d| d.join("oauth"));
        if let Some(ref dir) = credential_dir
            && let Err(e) = tokio::fs::create_dir_all(dir).await
        {
            tracing::warn!(error = %e, "failed to create OAuth credential directory");
        }

        for (server_id, config) in configs {
            if let Err(e) = validate_server_id(server_id) {
                tracing::error!(server = %server_id, error = %e, "invalid MCP server ID, skipping");
                if let Some(ref tx) = status_tx {
                    let _ = tx.send(format!("\u{26a0} MCP '{server_id}': {e}"));
                }
                self.failed_servers.insert(server_id.clone(), e.to_string());
                continue;
            }
            if let McpServerConfig::Http { url, .. } = config
                && url::Url::parse(url).is_err()
            {
                tracing::error!(server = %server_id, url = %url, "invalid MCP server URL, skipping");
                let msg = format!("invalid URL: {url}");
                if let Some(ref tx) = status_tx {
                    let _ = tx.send(format!("\u{26a0} MCP '{server_id}': {msg}"));
                }
                self.failed_servers.insert(server_id.clone(), msg);
                continue;
            }
            match McpServer::spawn(
                server_id.clone(),
                config,
                credential_dir.as_deref(),
                status_tx.clone(),
            )
            .await
            {
                Ok(server) => {
                    self.servers.insert(server_id.clone(), server);
                }
                Err(e) => {
                    tracing::error!(server = %server_id, error = %e, "failed to start MCP server");
                    let msg = e.to_string();
                    let short = msg
                        .lines()
                        .next()
                        .unwrap_or("connection failed")
                        .to_string();
                    self.failed_servers.insert(server_id.clone(), short.clone());
                    if let Some(ref tx) = status_tx {
                        let _ = tx.send(format!(
                            "\u{26a0} MCP '{server_id}': failed to start \u{2014} {short}"
                        ));
                    }
                }
            }
        }
        self.rebuild_snapshot();
    }

    fn rebuild_snapshot(&mut self) {
        let mut known_tools = std::collections::HashSet::new();
        let mut tool_defs = Vec::new();

        for (server_id, server) in &self.servers {
            for tool in &server.cached_tools {
                let prefixed = prefixed_tool_name(server_id, &tool.name);
                known_tools.insert(prefixed.clone());

                let description = tool.description.as_deref().unwrap_or("MCP tool");
                let parameters = tool.schema_as_json_value();
                tool_defs.push(serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": prefixed,
                        "description": description,
                        "parameters": parameters,
                    }
                }));
            }
        }

        self.snapshot = Arc::new(McpToolSnapshot {
            known_tools,
            tool_defs,
        });
    }

    pub fn tool_snapshot(&self) -> Arc<McpToolSnapshot> {
        Arc::clone(&self.snapshot)
    }

    pub fn has_servers(&self) -> bool {
        !self.servers.is_empty()
    }

    pub fn all_tool_defs(&self) -> Vec<(&str, &Tool)> {
        self.servers
            .iter()
            .flat_map(|(id, server)| {
                server
                    .cached_tools
                    .iter()
                    .map(move |tool| (id.as_str(), tool))
            })
            .collect()
    }

    pub async fn call_tool(&self, prefixed_name: &str, args: Value) -> Result<(String, bool)> {
        let (server_id, tool_name) = parse_prefixed_tool_name(prefixed_name)
            .ok_or_else(|| anyhow::anyhow!("invalid MCP tool name: {prefixed_name}"))?;

        let server = self
            .servers
            .get(server_id)
            .ok_or_else(|| anyhow::anyhow!("MCP server '{server_id}' not found"))?;

        let args_map = match args {
            Value::Object(map) => Some(map),
            Value::Null => None,
            other => {
                tracing::warn!(
                    tool = %prefixed_name,
                    "MCP tool args is not an object or null, wrapping in {{\"input\": ...}}"
                );
                Some(serde_json::Map::from_iter([("input".to_string(), other)]))
            }
        };

        let result = server.call_tool(tool_name, args_map).await?;
        let is_error = result.is_error.unwrap_or(false);
        Ok((format_call_result(&result), is_error))
    }

    pub fn all_prompts(&self) -> Vec<(&str, &Prompt)> {
        self.servers
            .iter()
            .flat_map(|(id, server)| server.cached_prompts.iter().map(move |p| (id.as_str(), p)))
            .collect()
    }

    pub fn all_resources(&self) -> Vec<(&str, &Resource)> {
        self.servers
            .iter()
            .flat_map(|(id, server)| {
                server
                    .cached_resources
                    .iter()
                    .map(move |r| (id.as_str(), r))
            })
            .collect()
    }

    pub async fn read_resource(&self, server_id: &str, uri: &str) -> Result<String> {
        let server = self
            .servers
            .get(server_id)
            .ok_or_else(|| anyhow::anyhow!("MCP server '{server_id}' not found"))?;
        server.read_resource(uri).await
    }

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

    pub async fn shutdown(&mut self) {
        for (server_id, server) in self.servers.drain() {
            tracing::info!(server = %server_id, "shutting down MCP server");
            server.cancel().await;
        }
    }

    pub fn server_status(&self) -> Vec<crate::ui::sidebar::SidebarMcp> {
        let mut result: Vec<crate::ui::sidebar::SidebarMcp> = self
            .servers
            .iter()
            .map(|(id, server)| crate::ui::sidebar::SidebarMcp {
                server_id: id.clone(),
                tool_count: server.cached_tools.len(),
                resource_count: server.cached_resources.len(),
                prompt_count: server.cached_prompts.len(),
                connected: true,
                error: None,
            })
            .collect();

        for (id, error) in &self.failed_servers {
            result.push(crate::ui::sidebar::SidebarMcp {
                server_id: id.clone(),
                tool_count: 0,
                resource_count: 0,
                prompt_count: 0,
                connected: false,
                error: Some(error.clone()),
            });
        }

        result
    }

    pub fn overlay_snapshot(
        &self,
        configs: &HashMap<String, McpServerConfig>,
    ) -> crate::ui::mcp_overlay::McpSnapshot {
        use crate::ui::mcp_overlay::*;

        let mut servers: Vec<McpServerInfo> = self
            .servers
            .iter()
            .map(|(id, server)| {
                let transport = configs
                    .get(id)
                    .map(|c| if c.is_http() { "http" } else { "stdio" })
                    .unwrap_or("unknown");

                McpServerInfo {
                    server_id: id.clone(),
                    connected: true,
                    error: None,
                    transport,
                    tools: server
                        .cached_tools
                        .iter()
                        .map(|t| McpToolInfo {
                            name: t.name.to_string(),
                            description: t.description.as_deref().unwrap_or("").to_string(),
                        })
                        .collect(),
                    resources: server
                        .cached_resources
                        .iter()
                        .map(|r| McpResourceInfo {
                            name: r.name.clone(),
                            uri: r.uri.as_str().to_string(),
                            description: r.description.as_deref().unwrap_or("").to_string(),
                        })
                        .collect(),
                    prompts: server
                        .cached_prompts
                        .iter()
                        .map(|p| McpPromptInfo {
                            name: p.name.clone(),
                            description: p.description.as_deref().unwrap_or("").to_string(),
                            arguments: p
                                .arguments
                                .as_deref()
                                .unwrap_or(&[])
                                .iter()
                                .map(|a| McpPromptArg {
                                    name: a.name.clone(),
                                    description: a.description.as_deref().unwrap_or("").to_string(),
                                    required: a.required.unwrap_or(false),
                                })
                                .collect(),
                        })
                        .collect(),
                }
            })
            .collect();

        for (id, error) in &self.failed_servers {
            let transport = configs
                .get(id)
                .map(|c| if c.is_http() { "http" } else { "stdio" })
                .unwrap_or("unknown");

            servers.push(McpServerInfo {
                server_id: id.clone(),
                connected: false,
                error: Some(error.clone()),
                transport,
                tools: vec![],
                resources: vec![],
                prompts: vec![],
            });
        }

        servers.sort_by(|a, b| a.server_id.cmp(&b.server_id));

        McpSnapshot { servers }
    }

    pub fn server_summary(&self) -> Vec<String> {
        self.servers
            .iter()
            .map(|(id, server)| {
                let mut parts = Vec::new();
                if !server.cached_tools.is_empty() {
                    parts.push(format!("{} tools", server.cached_tools.len()));
                }
                if !server.cached_resources.is_empty() {
                    parts.push(format!("{} resources", server.cached_resources.len()));
                }
                if !server.cached_prompts.is_empty() {
                    parts.push(format!("{} prompts", server.cached_prompts.len()));
                }
                if parts.is_empty() {
                    id.clone()
                } else {
                    format!("{id} ({})", parts.join(", "))
                }
            })
            .collect()
    }
}

fn format_call_result(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .map(|content| match &content.raw {
            RawContent::Text(text) => text.text.as_str(),
            RawContent::Image(_) => "[image content]",
            RawContent::Audio(_) => "[audio content]",
            RawContent::Resource(_) => "[embedded resource]",
            RawContent::ResourceLink(_) => "[resource link]",
        })
        .collect::<Vec<_>>()
        .join("\n")
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
        let result = CallToolResult::success(vec![text_content("line 1"), text_content("line 2")]);
        assert_eq!(format_call_result(&result), "line 1\nline 2");
    }

    #[test]
    fn mcp_manager_new_has_no_servers() {
        let mgr = McpManager::new();
        assert!(!mgr.has_servers());
        assert!(mgr.all_tool_defs().is_empty());
        assert!(mgr.all_resources().is_empty());
        assert!(mgr.tool_snapshot().is_empty());
    }
}
