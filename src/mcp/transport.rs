//! Transport construction for MCP server connections.

use std::collections::HashMap;

use anyhow::Result;
use rmcp::ServiceExt;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransportConfig, StreamableHttpClientWorker,
};
use rmcp::transport::worker::WorkerTransport;

use super::types::expand_env;

/// Connect to a remote MCP server over Streamable HTTP.
///
/// If `headers` contains an `Authorization` key, it is used as the auth header.
/// Other headers are passed as custom headers on every request.
pub async fn connect_http(
    server_id: &str,
    url: &str,
    headers: Option<&HashMap<String, String>>,
) -> Result<RunningService<RoleClient, ()>> {
    let expanded = headers.map(|h| expand_env(h)).unwrap_or_default();

    let mut config = StreamableHttpClientTransportConfig::with_uri(url);

    // Extract Authorization header specially — rmcp sends it via a dedicated field
    if let Some(auth) = expanded.get("Authorization") {
        config = config.auth_header(auth.clone());
    }

    // Remaining headers as custom headers
    let mut custom = HashMap::new();
    for (key, value) in &expanded {
        if key.eq_ignore_ascii_case("Authorization") {
            continue;
        }
        // Use the http types from the config's public field type (same http crate as rmcp)
        if let (Ok(name), Ok(val)) = (key.parse(), value.parse()) {
            custom.insert(name, val);
        } else {
            tracing::warn!(
                server = %server_id,
                header = %key,
                "invalid HTTP header, skipping"
            );
        }
    }
    if !custom.is_empty() {
        config = config.custom_headers(custom);
    }

    // Build the worker with reqwest's default client and our customized config.
    let client = reqwest::Client::new();
    let worker = StreamableHttpClientWorker::new(client, config);
    let transport = WorkerTransport::spawn(worker);

    let service: RunningService<RoleClient, ()> = ()
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!("MCP HTTP handshake failed for '{server_id}': {e}"))?;

    Ok(service)
}
