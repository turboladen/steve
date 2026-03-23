//! OAuth2 authentication for remote MCP servers.
//!
//! Flow:
//! 1. Check for stored credentials (skip browser if valid)
//! 2. Discover OAuth metadata (RFC 9728 / RFC 8414)
//! 3. Dynamically register client (RFC 7591)
//! 4. Generate auth URL with PKCE
//! 5. Open browser, wait for callback
//! 6. Exchange code for token
//! 7. Return `AuthClient` wrapping `reqwest::Client`

pub mod callback;
pub mod credential_store;

pub use callback::{CallbackResult, start_callback_server};
pub use credential_store::FileCredentialStore;

use std::path::PathBuf;

use anyhow::{Context, Result};
use rmcp::transport::auth::{AuthClient, AuthorizationManager};

/// Sender for OAuth status messages displayed in the TUI.
pub type OAuthStatusTx = tokio::sync::mpsc::UnboundedSender<String>;

/// Send a status message to the TUI if a sender is available.
fn send_status(tx: &Option<OAuthStatusTx>, msg: String) {
    if let Some(tx) = tx {
        let _ = tx.send(msg);
    }
}

/// Run the full OAuth2 authorization flow for a remote MCP server.
///
/// If valid stored credentials exist at `credential_path`, the browser flow is
/// skipped entirely and an `AuthClient` is returned immediately.
///
/// Otherwise, the function discovers OAuth metadata, performs dynamic client
/// registration, opens the user's browser for authorization, waits for the
/// callback, and exchanges the code for a token.
///
/// When `status_tx` is provided, human-readable status messages are sent at
/// each stage so the TUI can display progress to the user.
pub async fn authorize(
    server_id: &str,
    base_url: &str,
    credential_path: PathBuf,
    status_tx: Option<OAuthStatusTx>,
) -> Result<AuthClient<reqwest::Client>> {
    send_status(&status_tx, format!("MCP '{server_id}': starting OAuth authorization..."));

    let mut auth_mgr = AuthorizationManager::new(base_url)
        .await
        .context("failed to create AuthorizationManager")?;

    // Persistent credential storage
    let store = FileCredentialStore::new(credential_path);
    auth_mgr.set_credential_store(store);

    // Try stored credentials first — avoids browser flow if a valid token exists.
    if auth_mgr.initialize_from_store().await.unwrap_or(false) {
        tracing::info!(server = %server_id, "reusing stored OAuth credentials");
        send_status(&status_tx, format!("MCP '{server_id}': reusing stored OAuth credentials"));
        let client = reqwest::Client::new();
        return Ok(AuthClient::new(client, auth_mgr));
    }

    // Discover OAuth metadata (RFC 9728 Protected Resource Metadata → RFC 8414)
    tracing::info!(server = %server_id, "discovering OAuth metadata");
    send_status(&status_tx, format!("MCP '{server_id}': discovering OAuth metadata..."));
    let metadata = auth_mgr
        .discover_metadata()
        .await
        .context("OAuth metadata discovery failed")?;
    auth_mgr.set_metadata(metadata);

    // Start ephemeral callback server for the redirect URI
    let (callback_url, callback_rx, server_handle) = callback::start_callback_server()
        .await
        .context("failed to start OAuth callback server")?;

    // Run the browser-based OAuth flow, ensuring the callback server is
    // always shut down regardless of success or failure.
    let result = async {
        // Dynamic client registration (RFC 7591)
        // `select_scopes()` picks the best scope set from metadata/headers/defaults.
        let scopes = auth_mgr.select_scopes(None, &[]);
        let scope_refs: Vec<&str> = scopes.iter().map(|s| s.as_str()).collect();

        tracing::info!(server = %server_id, "registering OAuth client");
        // rmcp's register_client() internally calls configure_client(),
        // so the returned config doesn't need separate handling.
        let _client_config = auth_mgr
            .register_client("steve", &callback_url, &scope_refs)
            .await
            .context("OAuth dynamic client registration failed")?;

        // Generate authorization URL with PKCE
        let auth_url = auth_mgr
            .get_authorization_url(&scope_refs)
            .await
            .context("failed to generate authorization URL")?;

        // Open the user's browser
        tracing::info!(server = %server_id, "opening browser for OAuth authorization");
        send_status(&status_tx, format!("MCP '{server_id}': opening browser for authorization..."));
        if let Err(e) = webbrowser::open(&auth_url) {
            tracing::error!(error = %e, "failed to open browser — authorize manually");
            tracing::info!(url = %auth_url, "open this URL manually to authorize");
            send_status(&status_tx, format!("MCP '{server_id}': failed to open browser — check logs for URL"));
        }

        // Wait for the callback (5 minute timeout)
        send_status(&status_tx, format!("MCP '{server_id}': waiting for browser authorization..."));
        let callback_result = tokio::time::timeout(
            std::time::Duration::from_secs(300),
            callback_rx,
        )
        .await
        .context("OAuth authorization timed out (5 minutes)")?
        .context("OAuth callback channel closed")?;

        // Exchange authorization code for token.
        // CSRF state validation happens inside rmcp's exchange_code_for_token():
        // it looks up the CSRF token in the state store (saved during
        // get_authorization_url), verifies it matches, and deletes it after use.
        tracing::info!(server = %server_id, "exchanging authorization code for token");
        send_status(&status_tx, format!("MCP '{server_id}': exchanging authorization code..."));
        auth_mgr
            .exchange_code_for_token(&callback_result.code, &callback_result.state)
            .await
            .context("failed to exchange authorization code for token")?;

        tracing::info!(server = %server_id, "OAuth authorization successful");
        send_status(&status_tx, format!("MCP '{server_id}': authorized successfully"));
        let client = reqwest::Client::new();
        Ok(AuthClient::new(client, auth_mgr))
    }
    .await;

    // Always shut down the callback server, whether we succeeded or failed.
    server_handle.abort();

    result
}
