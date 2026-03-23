//! Ephemeral localhost HTTP server to receive OAuth authorization callbacks.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::Query;
use axum::response::Html;
use axum::routing::get;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

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
pub async fn start_callback_server()
    -> anyhow::Result<(String, oneshot::Receiver<CallbackResult>, tokio::task::JoinHandle<()>)>
{
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
                        return Html(format!(
                            "<html><body>\
                             <h2>Authorization failed: {error}</h2>\
                             <p>{desc}</p>\
                             </body></html>"
                        ));
                    }

                    let code = params.get("code").cloned().unwrap_or_default();
                    let state = params.get("state").cloned().unwrap_or_default();

                    if !code.is_empty() && !state.is_empty() {
                        if let Some(sender) = tx.lock().await.take() {
                            let _ = sender.send(CallbackResult { code, state });
                        }
                        Html(
                            "<html><body>\
                             <h2>Authorization successful! You can close this tab.</h2>\
                             </body></html>"
                                .into(),
                        )
                    } else {
                        Html(
                            "<html><body>\
                             <h2>Missing or empty code/state parameter.</h2>\
                             </body></html>"
                                .into(),
                        )
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
        assert!(body.contains("successful"));

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
        assert!(body.contains("access_denied"));
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
        assert!(body.contains("Missing or empty code/state"));
        handle.abort();
    }
}
