use std::process::Stdio;

use anyhow::{Context, Result};
use rmcp::{
    ServiceExt,
    model::{
        CallToolRequestParams, CallToolResult, Prompt, ReadResourceRequestParams, Resource,
        ResourceContents, Tool,
    },
    service::{Peer, RoleClient, RunningService},
};
use serde_json::Value;
use tokio::io::AsyncBufReadExt;

use super::{McpServerConfig, expand_env, expand_env_single, oauth, transport};

pub(super) struct McpServer {
    pub(super) server_id: String,
    service: RunningService<RoleClient, ()>,
    pub(super) cached_tools: Vec<Tool>,
    pub(super) cached_resources: Vec<Resource>,
    pub(super) cached_prompts: Vec<Prompt>,
}

impl McpServer {
    pub(super) async fn spawn(
        server_id: String,
        config: &McpServerConfig,
        credential_dir: Option<&std::path::Path>,
        status_tx: Option<oauth::OAuthStatusTx>,
    ) -> Result<Self> {
        let service = match config {
            McpServerConfig::Stdio { command, args, env } => {
                let expanded_env = expand_env(env);

                let mut cmd = tokio::process::Command::new(command);
                cmd.args(args);
                for (key, value) in &expanded_env {
                    cmd.env(key, value);
                }

                let (transport, stderr) = rmcp::transport::TokioChildProcess::builder(cmd)
                    .stderr(Stdio::piped())
                    .spawn()
                    .context("failed to spawn MCP server process")?;

                if let Some(stderr) = stderr {
                    let sid = server_id.clone();
                    tokio::spawn(async move {
                        let reader = tokio::io::BufReader::new(stderr);
                        let mut lines = reader.lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            tracing::debug!(server = %sid, "mcp stderr: {line}");
                        }
                    });
                }

                let svc: RunningService<RoleClient, ()> = ()
                    .serve(transport)
                    .await
                    .map_err(|e| anyhow::anyhow!("MCP handshake failed for '{server_id}': {e}"))?;
                svc
            }
            McpServerConfig::Http {
                url,
                headers,
                client_id,
                client_secret,
            } => {
                let expanded_secret = client_secret.as_ref().map(|s| expand_env_single(s));
                transport::connect_http(
                    &server_id,
                    url,
                    headers.as_ref(),
                    client_id.as_deref(),
                    expanded_secret.as_deref(),
                    credential_dir,
                    status_tx,
                )
                .await?
            }
        };

        let cached_tools = service.peer().list_all_tools().await.unwrap_or_else(|e| {
            tracing::warn!(server = %server_id, error = %e, "failed to list MCP tools");
            Vec::new()
        });

        let cached_resources = service
            .peer()
            .list_all_resources()
            .await
            .unwrap_or_else(|e| {
                tracing::debug!(server = %server_id, error = %e, "failed to list MCP resources (may not be supported)");
                Vec::new()
            });

        let cached_prompts = service
            .peer()
            .list_all_prompts()
            .await
            .unwrap_or_else(|e| {
                tracing::debug!(server = %server_id, error = %e, "failed to list MCP prompts (may not be supported)");
                Vec::new()
            });

        let transport_kind = if config.is_http() { "http" } else { "stdio" };
        tracing::info!(
            server = %server_id,
            tools = cached_tools.len(),
            resources = cached_resources.len(),
            prompts = cached_prompts.len(),
            transport = transport_kind,
            "MCP server connected"
        );

        Ok(Self {
            server_id,
            service,
            cached_tools,
            cached_resources,
            cached_prompts,
        })
    }

    pub(super) fn peer(&self) -> &Peer<RoleClient> {
        self.service.peer()
    }

    pub(super) async fn call_tool(
        &self,
        name: &str,
        args: Option<serde_json::Map<String, Value>>,
    ) -> Result<CallToolResult> {
        let mut params = CallToolRequestParams::new(name.to_string());
        if let Some(args) = args {
            params = params.with_arguments(args);
        }
        self.peer()
            .call_tool(params)
            .await
            .map_err(|e| anyhow::anyhow!("MCP tool call failed on '{}': {e}", self.server_id))
    }

    pub(super) async fn read_resource(&self, uri: &str) -> Result<String> {
        let params = ReadResourceRequestParams::new(uri);
        let result = self.peer().read_resource(params).await.map_err(|e| {
            anyhow::anyhow!("MCP read_resource failed on '{}': {e}", self.server_id)
        })?;

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

    pub(super) async fn cancel(self) {
        let _ = self.service.cancel().await;
    }
}
