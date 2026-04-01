//! Ephemeral localhost HTTP server to receive OAuth authorization callbacks.

use std::{collections::HashMap, sync::Arc};

use axum::{extract::Query, response::Html, routing::get};
use base64::Engine;
use tokio::{net::TcpListener, sync::oneshot};

/// Steve logo PNG, embedded at compile time and base64-encoded on first access.
static LOGO_BASE64: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let bytes = include_bytes!("../../../i-am-steve.png");
    base64::engine::general_purpose::STANDARD.encode(bytes)
});

fn callback_page(title: &str, message: &str, status_class: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Steve — {title}</title>
<style>
  * {{ margin: 0; padding: 0; box-sizing: border-box; }}
  body {{
    min-height: 100vh;
    display: flex;
    align-items: center;
    justify-content: center;
    background: linear-gradient(135deg, #fff8e1 0%, #ffecb3 100%);
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
    color: #3e2723;
  }}
  .card {{
    background: #fff;
    border-radius: 20px;
    box-shadow: 0 8px 32px rgba(0,0,0,0.10);
    padding: 48px 40px 36px;
    max-width: 420px;
    width: 90%;
    text-align: center;
  }}
  .logo {{
    width: 120px;
    height: 120px;
    border-radius: 50%;
    object-fit: cover;
    object-position: center top;
    border: 4px solid #ffd54f;
    margin-bottom: 24px;
  }}
  h1 {{
    font-size: 1.5rem;
    margin-bottom: 8px;
    font-weight: 700;
  }}
  .message {{
    font-size: 1.05rem;
    color: #5d4037;
    line-height: 1.5;
    margin-bottom: 24px;
  }}
  .status {{
    display: inline-block;
    padding: 6px 18px;
    border-radius: 999px;
    font-size: 0.85rem;
    font-weight: 600;
    letter-spacing: 0.02em;
  }}
  .success {{ background: #e8f5e9; color: #2e7d32; }}
  .error   {{ background: #fbe9e7; color: #c62828; }}
  .warning {{ background: #fff3e0; color: #e65100; }}
  .footer {{
    margin-top: 28px;
    font-size: 0.8rem;
    color: #a1887f;
  }}
</style>
</head>
<body>
<div class="card">
  <img class="logo" src="data:image/png;base64,{logo}" alt="Steve">
  <h1>{title}</h1>
  <p class="message">{message}</p>
  <span class="status {status_class}">{title}</span>
  <p class="footer">steve · rust tui coding agent</p>
</div>
</body>
</html>"#,
        title = title,
        message = message,
        status_class = status_class,
        logo = &*LOGO_BASE64,
    )
}

/// The authorization code and state received from the OAuth callback.
#[derive(Debug, Clone)]
pub struct CallbackResult {
    pub code: String,
    pub state: String,
}

/// Start an ephemeral HTTP server on a random local port to receive the OAuth callback.
///
/// Returns `(callback_url, receiver)` where `callback_url` is the full URL to use as
/// the OAuth `redirect_uri`, and `receiver` yields the authorization code + state once
/// the user completes the browser flow.
pub async fn start_callback_server() -> anyhow::Result<(
    String,
    oneshot::Receiver<CallbackResult>,
    tokio::task::JoinHandle<()>,
)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let callback_url = format!("http://127.0.0.1:{port}/callback");

    let (tx, rx) = oneshot::channel();
    let tx = Arc::new(tokio::sync::Mutex::new(Some(tx)));

    let app = axum::Router::new().route(
        "/callback",
        get({
            let tx = Arc::clone(&tx);
            move |Query(params): Query<HashMap<String, String>>| {
                let tx = Arc::clone(&tx);
                async move {
                    if let Some(error) = params.get("error") {
                        let desc = params
                            .get("error_description")
                            .map(|s| s.as_str())
                            .unwrap_or("");
                        tracing::error!(
                            error = %error,
                            description = %desc,
                            "OAuth callback received error"
                        );
                        // Drop the sender so the oneshot resolves immediately
                        // (RecvError) instead of waiting for the 5-minute timeout.
                        drop(tx.lock().await.take());
                        // Static HTML — never interpolate untrusted query params.
                        return Html(callback_page(
                            "Authorization Failed",
                            "Something went wrong during authorization. Please return to Steve for details.",
                            "error",
                        ));
                    }

                    let code = params.get("code").cloned().unwrap_or_default();
                    let state = params.get("state").cloned().unwrap_or_default();

                    if !code.is_empty() && !state.is_empty() {
                        if let Some(sender) = tx.lock().await.take() {
                            let _ = sender.send(CallbackResult { code, state });
                        }
                        Html(callback_page(
                            "Authorization Successful",
                            "You're all set! You can close this tab and return to Steve.",
                            "success",
                        ))
                    } else {
                        Html(callback_page(
                            "Missing Parameters",
                            "The authorization response was incomplete. Please try again from Steve.",
                            "warning",
                        ))
                    }
                }
            }
        }),
    );

    let server_handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "OAuth callback server error");
        }
    });

    Ok((callback_url, rx, server_handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn callback_server_binds_and_returns_url() {
        let (url, _rx, handle) = start_callback_server().await.unwrap();
        assert!(url.starts_with("http://127.0.0.1:"));
        assert!(url.contains("/callback"));
        handle.abort();
    }

    #[tokio::test]
    async fn callback_server_handles_valid_callback() {
        let (url, rx, handle) = start_callback_server().await.unwrap();
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{url}?code=test_code&state=test_state"))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body = resp.text().await.unwrap();
        assert!(body.contains("Authorization Successful"));

        let result = rx.await.unwrap();
        assert_eq!(result.code, "test_code");
        assert_eq!(result.state, "test_state");
        handle.abort();
    }

    #[tokio::test]
    async fn callback_server_handles_error_response() {
        let (url, _rx, handle) = start_callback_server().await.unwrap();
        let client = reqwest::Client::new();
        let resp = client
            .get(format!(
                "{url}?error=access_denied&error_description=User+denied"
            ))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body = resp.text().await.unwrap();
        assert!(body.contains("Authorization Failed"));
        handle.abort();
    }

    #[tokio::test]
    async fn callback_server_handles_missing_params() {
        let (url, _rx, handle) = start_callback_server().await.unwrap();
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{url}?code=only_code"))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body = resp.text().await.unwrap();
        assert!(body.contains("Missing Parameters"));
        handle.abort();
    }
}
