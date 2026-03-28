//! OAuth2 authentication for remote MCP servers.
//!
//! Flow:
//! 1. Check for stored credentials (skip browser if valid)
//! 2. Discover OAuth metadata (RFC 9728 / RFC 8414)
//! 3. Register client (dynamic or config-provided)
//! 4. Generate auth URL with PKCE
//! 5. Open browser, wait for callback
//! 6. Exchange code for token (direct HTTP — no RFC 8707 `resource` param)
//! 7. Return `AuthClient` wrapping `reqwest::Client`
//!
//! We manage PKCE and token exchange ourselves rather than using rmcp's
//! `get_authorization_url` / `exchange_code_for_token`, because rmcp
//! unconditionally sends an RFC 8707 `resource` parameter that some
//! providers (notably GitHub) reject.

pub mod callback;
pub mod credential_store;

pub use callback::{CallbackResult, start_callback_server};
pub use credential_store::FileCredentialStore;

use std::path::PathBuf;

use anyhow::{Context, Result};
use rmcp::transport::auth::{
    AuthClient, AuthorizationManager, AuthorizationMetadata, CredentialStore, StoredCredentials,
};

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
/// Otherwise, the function discovers OAuth metadata, registers a client (or
/// uses a config/built-in client_id), opens the user's browser for
/// authorization, exchanges the code for a token, and persists it.
pub async fn authorize(
    server_id: &str,
    base_url: &str,
    credential_path: PathBuf,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    status_tx: Option<OAuthStatusTx>,
) -> Result<AuthClient<reqwest::Client>> {
    send_status(
        &status_tx,
        format!("MCP '{server_id}': starting OAuth authorization..."),
    );

    let mut auth_mgr =
        init_auth_manager(server_id, base_url, credential_path.clone(), &status_tx).await?;

    // Try stored credentials first — avoids browser flow if a valid token exists.
    if auth_mgr.initialize_from_store().await.unwrap_or(false) {
        tracing::info!(server = %server_id, "reusing stored OAuth credentials");
        send_status(
            &status_tx,
            format!("MCP '{server_id}': reusing stored OAuth credentials"),
        );
        return Ok(wrap_client(auth_mgr));
    }

    // Discover OAuth endpoints
    let metadata = match discover_metadata(server_id, &auth_mgr, &status_tx).await {
        Ok(m) => m,
        Err(e) => {
            let log_hint = log_path_hint();
            send_status(
                &status_tx,
                format!("\u{26a0} MCP '{server_id}': OAuth discovery failed \u{2014} {e:#}"),
            );
            send_status(
                &status_tx,
                format!("\u{26a0} MCP '{server_id}': {log_hint}"),
            );
            return Err(e).context(format!(
                "OAuth metadata discovery failed for MCP '{server_id}'"
            ));
        }
    };
    auth_mgr.set_metadata(metadata.clone());

    // Run the interactive browser flow
    if let Err(e) = browser_auth_flow(
        server_id,
        base_url,
        &mut auth_mgr,
        &metadata,
        &credential_path,
        client_id,
        client_secret,
        &status_tx,
    )
    .await
    {
        let log_hint = log_path_hint();
        send_status(
            &status_tx,
            format!("\u{26a0} MCP '{server_id}': authorization failed \u{2014} {e:#}"),
        );
        send_status(
            &status_tx,
            format!("\u{26a0} MCP '{server_id}': {log_hint}"),
        );
        return Err(e).context(format!("OAuth authorization failed for MCP '{server_id}'"));
    }

    // Reload credentials from store so AuthClient has the token
    if auth_mgr.initialize_from_store().await.unwrap_or(false) {
        send_status(
            &status_tx,
            format!("MCP '{server_id}': authorized successfully"),
        );
        Ok(wrap_client(auth_mgr))
    } else {
        Err(anyhow::anyhow!(
            "OAuth token was saved but could not be reloaded"
        ))
    }
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
    auth_mgr: &AuthorizationManager,
    status_tx: &Option<OAuthStatusTx>,
) -> Result<AuthorizationMetadata> {
    tracing::info!(server = %server_id, "discovering OAuth metadata");
    send_status(
        status_tx,
        format!("MCP '{server_id}': discovering OAuth metadata..."),
    );

    auth_mgr
        .discover_metadata()
        .await
        .context("OAuth metadata discovery failed")
}

/// Run the interactive browser-based OAuth flow.
///
/// Manages PKCE and token exchange directly (without rmcp's exchange_code_for_token)
/// to avoid the unsupported RFC 8707 `resource` parameter.
async fn browser_auth_flow(
    server_id: &str,
    base_url: &str,
    auth_mgr: &mut AuthorizationManager,
    metadata: &AuthorizationMetadata,
    credential_path: &std::path::Path,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    status_tx: &Option<OAuthStatusTx>,
) -> Result<()> {
    let (callback_url, callback_rx, server_handle) = callback::start_callback_server()
        .await
        .context("failed to start OAuth callback server")?;

    let result = async {
        // Resolve client_id (dynamic registration → config → well-known)
        let resolved = resolve_client_id(
            server_id, base_url, auth_mgr, &callback_url, client_id, client_secret,
        ).await?;

        // Generate PKCE challenge ourselves
        let pkce_verifier = generate_pkce_verifier();
        let pkce_challenge = generate_pkce_challenge(&pkce_verifier);

        // Build authorization URL
        let state = generate_random_state();
        let scopes = auth_mgr.select_scopes(None, &[]);
        let scope_str = scopes.join(" ");
        let mut auth_url = format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256",
            metadata.authorization_endpoint,
            urlencoding::encode(&resolved.client_id),
            urlencoding::encode(&callback_url),
            urlencoding::encode(&state),
            urlencoding::encode(&pkce_challenge),
        );
        if !scope_str.is_empty() {
            auth_url.push_str(&format!("&scope={}", urlencoding::encode(&scope_str)));
        }

        // Open browser
        tracing::info!(server = %server_id, "opening browser for OAuth authorization");
        send_status(status_tx, format!(
            "\u{26a0} MCP '{server_id}': ACTION REQUIRED \u{2014} Authorize in your browser to continue"
        ));
        if let Err(e) = webbrowser::open(&auth_url) {
            tracing::error!(error = %e, "failed to open browser — authorize manually");
            tracing::info!(url = %auth_url, "open this URL manually to authorize");
            send_status(status_tx, format!("MCP '{server_id}': failed to open browser — check logs for URL"));
        }

        // Wait for callback
        send_status(status_tx, format!("MCP '{server_id}': waiting for browser authorization..."));
        let callback_result = tokio::time::timeout(
            std::time::Duration::from_secs(300),
            callback_rx,
        )
        .await
        .context("OAuth authorization timed out (5 minutes)")?
        .context("OAuth callback channel closed — authorization may have been denied")?;

        // Verify CSRF state
        if callback_result.state != state {
            return Err(anyhow::anyhow!("CSRF state mismatch in OAuth callback"));
        }

        // Exchange code for token (direct HTTP, no `resource` param)
        send_status(status_tx, format!("MCP '{server_id}': exchanging authorization code..."));
        let token = exchange_code(
            &metadata.token_endpoint,
            &callback_result.code,
            &callback_url,
            &resolved.client_id,
            resolved.client_secret.as_deref(),
            &pkce_verifier,
        ).await?;

        // Save credentials to file for reuse
        save_credentials(credential_path, &resolved.client_id, &token).await?;

        tracing::info!(server = %server_id, "OAuth authorization successful");
        Ok(())
    }
    .await;

    server_handle.abort();
    result
}

/// Resolved OAuth client credentials.
struct ResolvedClient {
    client_id: String,
    client_secret: Option<String>,
}

/// Try to resolve a client_id via: dynamic registration → config → well-known defaults.
async fn resolve_client_id(
    server_id: &str,
    base_url: &str,
    auth_mgr: &mut AuthorizationManager,
    callback_url: &str,
    config_client_id: Option<&str>,
    config_client_secret: Option<&str>,
) -> Result<ResolvedClient> {
    let scopes = auth_mgr.select_scopes(None, &[]);
    let scope_refs: Vec<&str> = scopes.iter().map(|s| s.as_str()).collect();

    // Attempt 1: dynamic client registration (RFC 7591)
    tracing::info!(server = %server_id, "attempting dynamic OAuth client registration");
    match auth_mgr
        .register_client("steve", callback_url, &scope_refs)
        .await
    {
        Ok(resp) => {
            tracing::info!(server = %server_id, "dynamic client registration succeeded");
            return Ok(ResolvedClient {
                client_id: resp.client_id,
                client_secret: resp.client_secret,
            });
        }
        Err(e) => {
            tracing::info!(server = %server_id, error = %e, "dynamic registration not supported");
        }
    }

    // Attempt 2: config-provided client_id
    if let Some(id) = config_client_id {
        tracing::info!(server = %server_id, "using config-provided client_id");
        return Ok(ResolvedClient {
            client_id: id.to_string(),
            client_secret: config_client_secret.map(|s| s.to_string()),
        });
    }

    // Attempt 3: well-known built-in credentials for popular services
    if let Some(creds) = well_known_credentials(base_url) {
        tracing::info!(server = %server_id, client_id = %creds.client_id, "using built-in credentials");
        return Ok(ResolvedClient {
            client_id: creds.client_id.to_string(),
            // Config-provided secret overrides built-in (allows user customization)
            client_secret: config_client_secret
                .map(|s| s.to_string())
                .or_else(|| creds.client_secret.map(|s| s.to_string())),
        });
    }

    Err(anyhow::anyhow!(
        "MCP server '{server_id}' does not support dynamic client registration \
         and no client_id was provided in config. Add a \"client_id\" field to \
         the server config."
    ))
}

/// Exchange an authorization code for an access token via direct HTTP POST.
///
/// Does NOT send the RFC 8707 `resource` parameter (which rmcp adds and
/// GitHub rejects).
async fn exchange_code(
    token_endpoint: &str,
    code: &str,
    redirect_uri: &str,
    client_id: &str,
    client_secret: Option<&str>,
    pkce_verifier: &str,
) -> Result<serde_json::Value> {
    let http = reqwest::Client::new();
    let mut form_parts = vec![
        format!("grant_type=authorization_code"),
        format!("code={}", urlencoding::encode(code)),
        format!("redirect_uri={}", urlencoding::encode(redirect_uri)),
        format!("client_id={}", urlencoding::encode(client_id)),
        format!("code_verifier={}", urlencoding::encode(pkce_verifier)),
    ];
    if let Some(secret) = client_secret {
        form_parts.push(format!("client_secret={}", urlencoding::encode(secret)));
    }
    let form_body = form_parts.join("&");

    let resp = http
        .post(token_endpoint)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form_body)
        .send()
        .await
        .context("token exchange HTTP request failed")?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .context("failed to read token response body")?;

    if !status.is_success() {
        return Err(anyhow::anyhow!(
            "token exchange failed (HTTP {status}): {body}"
        ));
    }

    let token: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("failed to parse token response: {body}"))?;

    if token.get("access_token").and_then(|v| v.as_str()).is_none() {
        return Err(anyhow::anyhow!(
            "token response missing access_token: {body}"
        ));
    }

    Ok(token)
}

/// Save OAuth credentials to the file store for cross-session reuse.
async fn save_credentials(
    credential_path: &std::path::Path,
    client_id: &str,
    token: &serde_json::Value,
) -> Result<()> {
    // Build a minimal StoredCredentials that rmcp can reload
    let granted_scopes: Vec<String> = token["scope"]
        .as_str()
        .map(|s| {
            s.split(&[',', ' '][..])
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    // Deserialize into rmcp's OAuthTokenResponse type for credential store compatibility.
    // GitHub's token response may not include all fields rmcp expects (e.g., token_type
    // might be missing). Add defaults if needed.
    let mut token_with_defaults = token.clone();
    if token_with_defaults.get("token_type").is_none() {
        token_with_defaults["token_type"] = serde_json::json!("bearer");
    }
    let token_response: Option<rmcp::transport::auth::OAuthTokenResponse> =
        match serde_json::from_value(token_with_defaults) {
            Ok(resp) => Some(resp),
            Err(e) => {
                tracing::warn!(error = %e, "could not parse token into rmcp format — credentials may not persist across sessions");
                None
            }
        };

    // StoredCredentials is #[non_exhaustive] in rmcp — construct via serde.
    let stored: StoredCredentials = serde_json::from_value(serde_json::json!({
        "client_id": client_id,
        "token_response": token_response,
        "granted_scopes": granted_scopes,
        "token_received_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    }))
    .context("failed to build StoredCredentials")?;

    let store = FileCredentialStore::new(credential_path.to_path_buf());
    store
        .save(stored)
        .await
        .context("failed to save OAuth credentials")?;

    Ok(())
}

// -- PKCE helpers --

fn generate_pkce_verifier() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.random()).collect();
    base64_url_encode(&bytes)
}

fn generate_pkce_challenge(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(verifier.as_bytes());
    base64_url_encode(&hash)
}

fn generate_random_state() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..16).map(|_| rng.random()).collect();
    base64_url_encode(&bytes)
}

fn base64_url_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// -- Well-known client IDs --

/// Built-in OAuth credentials for well-known MCP servers.
///
/// Shipping client secrets in native app binaries is standard practice
/// (VS Code, Claude Code, etc. do the same). GitHub considers this acceptable
/// for native/desktop OAuth apps — the secret prevents casual impersonation
/// but is not a security boundary for distributed binaries.
struct WellKnownCredentials {
    client_id: &'static str,
    client_secret: Option<&'static str>,
}

/// Look up built-in OAuth credentials for well-known MCP server URLs.
fn well_known_credentials(base_url: &str) -> Option<WellKnownCredentials> {
    let lower = base_url.to_lowercase();
    if lower.contains("githubcopilot.com") || lower.contains("github.com") {
        Some(WellKnownCredentials {
            client_id: "Iv23liXXwVqGAPlUVvVv",
            client_secret: Some("cb594175ff0564a05f7dbc13d3ceee3e411a074b"),
        })
    } else {
        None
    }
}

// -- Helpers --

/// Best-effort hint about where logs are stored for error messages.
fn log_path_hint() -> String {
    directories::ProjectDirs::from("", "", "steve")
        .map(|d| format!("Check logs at: {}", d.data_dir().join("logs").display()))
        .unwrap_or_else(|| "Check steve log files for details".to_string())
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
        send_status(&Some(tx), "dropped".into());
    }

    #[test]
    fn log_path_hint_returns_non_empty_string() {
        let hint = log_path_hint();
        assert!(!hint.is_empty());
        assert!(
            hint.contains("logs") || hint.contains("log"),
            "hint should mention logs: {hint}"
        );
    }

    #[test]
    fn well_known_credentials_github_copilot() {
        let creds = well_known_credentials("https://api.githubcopilot.com/mcp/").unwrap();
        assert_eq!(creds.client_id, "Iv23liXXwVqGAPlUVvVv");
        assert!(creds.client_secret.is_some());
    }

    #[test]
    fn well_known_credentials_github_domain() {
        let creds = well_known_credentials("https://mcp.github.com/something").unwrap();
        assert_eq!(creds.client_id, "Iv23liXXwVqGAPlUVvVv");
    }

    #[test]
    fn well_known_credentials_unknown_returns_none() {
        assert!(well_known_credentials("https://mcp.example.com").is_none());
    }

    #[test]
    fn pkce_challenge_is_deterministic_for_same_verifier() {
        let challenge1 = generate_pkce_challenge("test-verifier");
        let challenge2 = generate_pkce_challenge("test-verifier");
        assert_eq!(challenge1, challenge2);
    }

    #[test]
    fn pkce_verifier_is_random() {
        let v1 = generate_pkce_verifier();
        let v2 = generate_pkce_verifier();
        assert_ne!(v1, v2);
    }

    #[test]
    fn csrf_state_is_random() {
        let s1 = generate_random_state();
        let s2 = generate_random_state();
        assert_ne!(s1, s2);
    }

    #[tokio::test]
    async fn init_auth_manager_rejects_invalid_url() {
        let result = init_auth_manager("test", "not a url", "/tmp/fake.json".into(), &None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn init_auth_manager_accepts_valid_url() {
        let result = init_auth_manager(
            "test",
            "https://example.com",
            "/tmp/fake.json".into(),
            &None,
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn authorize_fails_for_unreachable_server() {
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("creds.json");
        let result = authorize(
            "test",
            "https://127.0.0.1:1/nonexistent",
            cred_path,
            None,
            None,
            None,
        )
        .await;
        assert!(result.is_err());
    }
}
