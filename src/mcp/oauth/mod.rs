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
    client_id: Option<&str>,
    client_secret: Option<&str>,
    status_tx: Option<OAuthStatusTx>,
) -> Result<AuthClient<reqwest::Client>> {
    send_status(&status_tx, format!("MCP '{server_id}': starting OAuth authorization..."));

    let mut auth_mgr = init_auth_manager(server_id, base_url, credential_path, &status_tx).await?;

    // Try stored credentials first — avoids browser flow if a valid token exists.
    if auth_mgr.initialize_from_store().await.unwrap_or(false) {
        tracing::info!(server = %server_id, "reusing stored OAuth credentials");
        send_status(&status_tx, format!("MCP '{server_id}': reusing stored OAuth credentials"));
        return Ok(wrap_client(auth_mgr));
    }

    // Discover OAuth endpoints, then run the interactive browser flow.
    if let Err(e) = discover_metadata(server_id, &mut auth_mgr, &status_tx).await {
        let log_hint = log_path_hint();
        // Show the actual error in the TUI, not just "check logs"
        send_status(&status_tx, format!(
            "\u{26a0} MCP '{server_id}': OAuth discovery failed \u{2014} {e:#}"
        ));
        send_status(&status_tx, format!(
            "\u{26a0} MCP '{server_id}': {log_hint}"
        ));
        return Err(e).context(format!(
            "OAuth metadata discovery failed for MCP '{server_id}'"
        ));
    }

    if let Err(e) = browser_auth_flow(server_id, base_url, &mut auth_mgr, client_id, client_secret, &status_tx).await {
        let log_hint = log_path_hint();
        send_status(&status_tx, format!(
            "\u{26a0} MCP '{server_id}': authorization failed \u{2014} {e:#}"
        ));
        send_status(&status_tx, format!(
            "\u{26a0} MCP '{server_id}': {log_hint}"
        ));
        return Err(e).context(format!(
            "OAuth authorization failed for MCP '{server_id}'"
        ));
    }

    send_status(&status_tx, format!("MCP '{server_id}': authorized successfully"));
    Ok(wrap_client(auth_mgr))
}

/// Create and configure an `AuthorizationManager` with persistent credential storage.
async fn init_auth_manager(
    server_id: &str,
    base_url: &str,
    credential_path: PathBuf,
    _status_tx: &Option<OAuthStatusTx>,
) -> Result<AuthorizationManager> {
    let mut auth_mgr = AuthorizationManager::new(base_url)
        .await
        .with_context(|| format!("failed to create AuthorizationManager for '{server_id}'"))?;

    let store = FileCredentialStore::new(credential_path);
    auth_mgr.set_credential_store(store);

    Ok(auth_mgr)
}

/// Discover OAuth metadata via RFC 9728 Protected Resource Metadata → RFC 8414.
async fn discover_metadata(
    server_id: &str,
    auth_mgr: &mut AuthorizationManager,
    status_tx: &Option<OAuthStatusTx>,
) -> Result<()> {
    tracing::info!(server = %server_id, "discovering OAuth metadata");
    send_status(status_tx, format!("MCP '{server_id}': discovering OAuth metadata..."));

    let metadata = auth_mgr
        .discover_metadata()
        .await
        .context("OAuth metadata discovery failed")?;
    auth_mgr.set_metadata(metadata);

    Ok(())
}

/// Run the interactive browser-based OAuth flow: register client, open browser,
/// wait for callback, exchange code for token.
///
/// Starts an ephemeral callback server and ensures it is shut down regardless
/// of success or failure.
async fn browser_auth_flow(
    server_id: &str,
    base_url: &str,
    auth_mgr: &mut AuthorizationManager,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    status_tx: &Option<OAuthStatusTx>,
) -> Result<()> {
    let (callback_url, callback_rx, server_handle) = callback::start_callback_server()
        .await
        .context("failed to start OAuth callback server")?;

    // Ensure the callback server is always shut down.
    let result = async {
        register_client(server_id, base_url, auth_mgr, &callback_url, client_id, client_secret).await?;
        let callback_result = open_browser_and_wait(server_id, auth_mgr, callback_rx, status_tx).await?;
        exchange_token(server_id, auth_mgr, &callback_result, status_tx).await
    }
    .await;

    server_handle.abort();
    result
}

/// Register an OAuth client for authorization.
///
/// Tries dynamic client registration (RFC 7591) first. If the server doesn't
/// support it, falls back to a config-provided `client_id`. If neither is
/// available, returns an error.
async fn register_client(
    server_id: &str,
    base_url: &str,
    auth_mgr: &mut AuthorizationManager,
    callback_url: &str,
    config_client_id: Option<&str>,
    config_client_secret: Option<&str>,
) -> Result<()> {
    let scopes = auth_mgr.select_scopes(None, &[]);
    let scope_refs: Vec<&str> = scopes.iter().map(|s| s.as_str()).collect();

    // Attempt 1: dynamic client registration (RFC 7591)
    tracing::info!(server = %server_id, "attempting dynamic OAuth client registration");
    match auth_mgr.register_client("steve", callback_url, &scope_refs).await {
        Ok(_client_config) => {
            // rmcp's register_client() internally calls configure_client()
            tracing::info!(server = %server_id, "dynamic client registration succeeded");
            return Ok(());
        }
        Err(e) => {
            tracing::info!(
                server = %server_id,
                error = %e,
                "dynamic client registration not supported, trying fallback"
            );
        }
    }

    // Attempt 2: use config-provided client_id (or well-known default)
    let (client_id, client_secret) = match config_client_id {
        Some(id) => {
            tracing::info!(server = %server_id, "using config-provided client_id");
            (id.to_string(), config_client_secret.map(|s| s.to_string()))
        }
        None => match well_known_client_id(base_url) {
            Some(id) => {
                tracing::info!(server = %server_id, client_id = %id, "using built-in client_id");
                (id.to_string(), None)
            }
            None => {
                return Err(anyhow::anyhow!(
                    "MCP server '{server_id}' does not support dynamic client registration \
                     and no client_id was provided in config. Add a \"client_id\" field to \
                     the server config."
                ));
            }
        },
    };

    let config = rmcp::transport::auth::OAuthClientConfig {
        client_id,
        client_secret,
        scopes: scope_refs.iter().map(|s| s.to_string()).collect(),
        redirect_uri: callback_url.to_string(),
    };
    auth_mgr
        .configure_client(config)
        .context("failed to configure OAuth client")?;
    Ok(())
}

/// Open the user's browser with the authorization URL and wait for the callback.
async fn open_browser_and_wait(
    server_id: &str,
    auth_mgr: &mut AuthorizationManager,
    callback_rx: tokio::sync::oneshot::Receiver<CallbackResult>,
    status_tx: &Option<OAuthStatusTx>,
) -> Result<CallbackResult> {
    let scopes = auth_mgr.select_scopes(None, &[]);
    let scope_refs: Vec<&str> = scopes.iter().map(|s| s.as_str()).collect();

    let auth_url = auth_mgr
        .get_authorization_url(&scope_refs)
        .await
        .context("failed to generate authorization URL")?;

    tracing::info!(server = %server_id, "opening browser for OAuth authorization");
    send_status(status_tx, format!("\u{26a0} MCP '{server_id}': ACTION REQUIRED \u{2014} Authorize in your browser to continue"));

    if let Err(e) = webbrowser::open(&auth_url) {
        tracing::error!(error = %e, "failed to open browser — authorize manually");
        tracing::info!(url = %auth_url, "open this URL manually to authorize");
        send_status(status_tx, format!("MCP '{server_id}': failed to open browser — check logs for URL"));
    }

    send_status(status_tx, format!("MCP '{server_id}': waiting for browser authorization..."));

    tokio::time::timeout(std::time::Duration::from_secs(300), callback_rx)
        .await
        .context("OAuth authorization timed out (5 minutes)")?
        .context("OAuth callback channel closed")
}

/// Exchange the authorization code for an access token.
///
/// CSRF state validation happens inside rmcp's `exchange_code_for_token()`:
/// it looks up the CSRF token in the state store (saved during
/// `get_authorization_url`), verifies it matches, and deletes it after use.
async fn exchange_token(
    server_id: &str,
    auth_mgr: &mut AuthorizationManager,
    callback_result: &CallbackResult,
    status_tx: &Option<OAuthStatusTx>,
) -> Result<()> {
    tracing::info!(server = %server_id, "exchanging authorization code for token");
    send_status(status_tx, format!("MCP '{server_id}': exchanging authorization code..."));

    auth_mgr
        .exchange_code_for_token(&callback_result.code, &callback_result.state)
        .await
        .context("failed to exchange authorization code for token")?;

    tracing::info!(server = %server_id, "OAuth authorization successful");
    Ok(())
}

/// Best-effort hint about where logs are stored for error messages.
fn log_path_hint() -> String {
    directories::ProjectDirs::from("", "", "steve")
        .map(|d| format!("Check logs at: {}", d.data_dir().join("logs").display()))
        .unwrap_or_else(|| "Check steve log files for details".to_string())
}

/// Look up a built-in client_id for well-known MCP server URLs.
///
/// This allows users to configure popular servers with just a URL — no manual
/// OAuth app registration needed. The client_ids here are for Steve's registered
/// apps with each provider.
fn well_known_client_id(base_url: &str) -> Option<&'static str> {
    let lower = base_url.to_lowercase();
    if lower.contains("githubcopilot.com") || lower.contains("github.com") {
        // Steve's GitHub App (public client, PKCE, no secret needed)
        Some("Iv23liXXwVqGAPlUVvVv")
    } else {
        None
    }
}

/// Wrap an `AuthorizationManager` into a ready-to-use `AuthClient`.
fn wrap_client(auth_mgr: AuthorizationManager) -> AuthClient<reqwest::Client> {
    AuthClient::new(reqwest::Client::new(), auth_mgr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_status_with_sender_delivers() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        send_status(&Some(tx), "hello".into());
        assert_eq!(rx.try_recv().unwrap(), "hello");
    }

    #[test]
    fn send_status_with_none_does_not_panic() {
        send_status(&None, "should be silently dropped".into());
    }

    #[test]
    fn send_status_with_closed_sender_does_not_panic() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        drop(rx);
        // Sending to a closed channel should not panic (we ignore the error).
        send_status(&Some(tx), "dropped".into());
    }

    #[test]
    fn log_path_hint_returns_non_empty_string() {
        let hint = log_path_hint();
        assert!(!hint.is_empty());
        // Should contain either a path or a fallback message
        assert!(
            hint.contains("logs") || hint.contains("log"),
            "hint should mention logs: {hint}"
        );
    }

    #[test]
    fn well_known_client_id_github_copilot() {
        assert_eq!(
            well_known_client_id("https://api.githubcopilot.com/mcp/"),
            Some("Iv23liXXwVqGAPlUVvVv"),
        );
    }

    #[test]
    fn well_known_client_id_github_domain() {
        assert_eq!(
            well_known_client_id("https://mcp.github.com/something"),
            Some("Iv23liXXwVqGAPlUVvVv"),
        );
    }

    #[test]
    fn well_known_client_id_unknown_returns_none() {
        assert_eq!(well_known_client_id("https://mcp.example.com"), None);
    }

    #[tokio::test]
    async fn init_auth_manager_rejects_invalid_url() {
        let result = init_auth_manager("test", "not a url", "/tmp/fake.json".into(), &None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn init_auth_manager_accepts_valid_url() {
        let result =
            init_auth_manager("test", "https://example.com", "/tmp/fake.json".into(), &None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn authorize_fails_for_unreachable_server() {
        // A server that doesn't exist should fail during metadata discovery,
        // not hang or panic.
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("creds.json");
        let result =
            authorize("test", "https://127.0.0.1:1/nonexistent", cred_path, None, None, None).await;
        assert!(result.is_err());
    }
}
