//! Transport construction for MCP server connections.

use std::{collections::HashMap, path::Path};

use anyhow::Result;
use rmcp::{
    ServiceExt,
    service::{RoleClient, RunningService},
    transport::{
        streamable_http_client::{StreamableHttpClientTransportConfig, StreamableHttpClientWorker},
        worker::WorkerTransport,
    },
};

use super::{oauth::OAuthStatusTx, types::expand_env};

/// Connect to a remote MCP server over Streamable HTTP.
///
/// If `headers` contains an `Authorization` key, it is used as the auth header
/// and no OAuth flow is attempted.
///
/// When no explicit Authorization header is present and `credential_dir` is
/// provided, the function first attempts an unauthenticated connection. If that
/// fails with an auth-related error (HTTP 401 / MCP handshake auth rejection),
/// it falls back to the full OAuth2 flow via [`super::oauth::authorize`].
///
/// `status_tx` is forwarded to the OAuth flow so progress messages can be
/// displayed in the TUI.
pub async fn connect_http(
    server_id: &str,
    url: &str,
    headers: Option<&HashMap<String, String>>,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    credential_dir: Option<&Path>,
    status_tx: Option<OAuthStatusTx>,
) -> Result<RunningService<RoleClient, ()>> {
    let expanded = headers.map(|h| expand_env(h)).unwrap_or_default();
    let has_explicit_auth = expanded
        .keys()
        .any(|k| k.eq_ignore_ascii_case("Authorization"));

    // --- Attempt 1: direct (unauthenticated or explicit-header) connection ---
    let direct_result = connect_direct(server_id, url, &expanded).await;

    match &direct_result {
        Ok(_) => return direct_result,
        Err(e) if has_explicit_auth => {
            // Explicit Authorization header was given but failed — don't silently
            // fall back to OAuth, propagate the error so the user can fix config.
            return Err(anyhow::anyhow!(
                "MCP HTTP connection failed for '{server_id}' with explicit auth: {e}"
            ));
        }
        Err(e) => {
            // Only attempt OAuth if the error looks auth-related.
            if !is_auth_error(e) {
                return direct_result;
            }
            tracing::info!(
                server = %server_id,
                error = %e,
                "unauthenticated connection failed with auth error, attempting OAuth"
            );
        }
    }

    // --- Attempt 2: OAuth flow ---
    let Some(cred_dir) = credential_dir else {
        return Err(anyhow::anyhow!(
            "MCP server '{server_id}' requires authentication but no data directory \
             is available for credential storage"
        ));
    };

    // Sanitize server_id before using as filename to prevent path traversal
    // (server_id comes from user config keys which may contain `/`, `..`, etc.).
    let safe_id: String = server_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let credential_path = cred_dir.join(format!("{safe_id}.json"));

    // First attempt — may use stored (cached) credentials.
    let first_result = oauth_connect(
        server_id,
        url,
        &expanded,
        credential_path.clone(),
        client_id,
        client_secret,
        status_tx.clone(),
    )
    .await;

    match first_result {
        Ok(service) => return Ok(service),
        Err(e) if is_auth_error(&e) && credential_path.exists() => {
            // Stored credentials are stale (expired/revoked) — clear them and
            // re-authorize, which will trigger the browser-based OAuth flow.
            tracing::info!(
                server = %server_id,
                "stored OAuth credentials rejected, clearing and re-authorizing"
            );
            let _ = tokio::fs::remove_file(&credential_path).await;

            oauth_connect(
                server_id,
                url,
                &expanded,
                credential_path,
                client_id,
                client_secret,
                status_tx,
            )
            .await
        }
        other => other,
    }
}

/// Connect using a plain `reqwest::Client` (no OAuth).
async fn connect_direct(
    server_id: &str,
    url: &str,
    expanded: &HashMap<String, String>,
) -> Result<RunningService<RoleClient, ()>> {
    let config = build_config(url, expanded);
    let client = reqwest::Client::new();
    let worker = StreamableHttpClientWorker::new(client, config);
    let transport = WorkerTransport::spawn(worker);

    let service: RunningService<RoleClient, ()> = ()
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!("MCP HTTP handshake failed for '{server_id}': {e}"))?;

    Ok(service)
}

/// Run the OAuth flow and perform the MCP handshake with the resulting `AuthClient`.
async fn oauth_connect(
    server_id: &str,
    url: &str,
    expanded: &HashMap<String, String>,
    credential_path: std::path::PathBuf,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    status_tx: Option<OAuthStatusTx>,
) -> Result<RunningService<RoleClient, ()>> {
    let auth_client = super::oauth::authorize(
        server_id,
        url,
        credential_path,
        client_id,
        client_secret,
        status_tx,
    )
    .await?;

    let config = build_config(url, expanded);
    let worker = StreamableHttpClientWorker::new(auth_client, config);
    let transport = WorkerTransport::spawn(worker);

    let service: RunningService<RoleClient, ()> = ()
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!("MCP HTTP handshake failed for '{server_id}' (OAuth): {e}"))?;

    Ok(service)
}

/// Build the `StreamableHttpClientTransportConfig` from the URL and expanded headers.
fn build_config(
    url: &str,
    expanded: &HashMap<String, String>,
) -> StreamableHttpClientTransportConfig {
    let mut config = StreamableHttpClientTransportConfig::with_uri(url);

    // Extract Authorization header specially — rmcp sends it via a dedicated field.
    // Case-insensitive lookup to match the has_explicit_auth check in connect_http().
    if let Some((_, auth)) = expanded
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Authorization"))
    {
        config = config.auth_header(auth.clone());
    }

    // Remaining headers as custom headers
    let mut custom = HashMap::new();
    for (key, value) in expanded {
        if key.eq_ignore_ascii_case("Authorization") {
            continue;
        }
        if let (Ok(name), Ok(val)) = (key.parse(), value.parse()) {
            custom.insert(name, val);
        } else {
            tracing::warn!(header = %key, "invalid HTTP header, skipping");
        }
    }
    if !custom.is_empty() {
        config = config.custom_headers(custom);
    }

    config
}

/// Heuristic: does this error look like an authentication/authorization failure?
///
/// We check for common patterns in the error chain that indicate the server
/// requires auth, NOT network/DNS/TLS errors that should propagate immediately.
fn is_auth_error(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    let lower = msg.to_lowercase();
    // HTTP 401 / 403, or rmcp auth-related error strings
    lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("auth required")       // "Auth required" from rmcp
        || lower.contains("authrequired")         // "AuthRequired" (no space)
        || lower.contains("authorization required")
        || lower.contains("www-authenticate")
        || lower.contains("insufficient_scope")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_auth_error_detects_401() {
        let err = anyhow::anyhow!("HTTP 401 Unauthorized");
        assert!(is_auth_error(&err));
    }

    #[test]
    fn is_auth_error_detects_403() {
        let err = anyhow::anyhow!("HTTP 403 Forbidden");
        assert!(is_auth_error(&err));
    }

    #[test]
    fn is_auth_error_detects_authorization_required() {
        let err = anyhow::anyhow!("OAuth authorization required");
        assert!(is_auth_error(&err));
    }

    #[test]
    fn is_auth_error_ignores_network_errors() {
        let err = anyhow::anyhow!("dns resolution failed: no such host");
        assert!(!is_auth_error(&err));
    }

    #[test]
    fn is_auth_error_ignores_connection_refused() {
        let err = anyhow::anyhow!("connection refused (os error 61)");
        assert!(!is_auth_error(&err));
    }

    #[test]
    fn is_auth_error_ignores_timeout() {
        let err = anyhow::anyhow!("request timed out");
        assert!(!is_auth_error(&err));
    }

    #[test]
    fn is_auth_error_detects_insufficient_scope() {
        let err = anyhow::anyhow!("insufficient_scope: need read access");
        assert!(is_auth_error(&err));
    }

    #[test]
    fn is_auth_error_detects_authrequired() {
        let err = anyhow::anyhow!("AuthRequired: please log in");
        assert!(is_auth_error(&err));
    }

    #[test]
    fn is_auth_error_detects_rmcp_auth_required() {
        // Exact error string from rmcp when server returns 401
        let err = anyhow::anyhow!(
            "MCP HTTP handshake failed for 'github': Send message error \
             Transport [...] error: Auth required, when send initialize request"
        );
        assert!(is_auth_error(&err));
    }

    #[test]
    fn is_auth_error_ignores_author() {
        let err = anyhow::anyhow!("commit author not found");
        assert!(!is_auth_error(&err));
    }

    #[test]
    fn is_auth_error_ignores_authentication_not_required() {
        let err = anyhow::anyhow!("authentication not required for this endpoint");
        assert!(!is_auth_error(&err));
    }

    #[test]
    fn build_config_with_auth_header() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer tok123".to_string());
        let _config = build_config("http://example.com/mcp", &headers);
        // Smoke test — config is opaque, but should not panic.
    }

    #[test]
    fn build_config_with_custom_headers() {
        let mut headers = HashMap::new();
        headers.insert("X-Custom".to_string(), "value".to_string());
        let _config = build_config("http://example.com/mcp", &headers);
    }

    #[test]
    fn build_config_empty_headers() {
        let headers = HashMap::new();
        let _config = build_config("http://example.com/mcp", &headers);
    }
}
