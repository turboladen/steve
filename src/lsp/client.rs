//! JSON-RPC transport over stdio for LSP communication.
//!
//! Implements Content-Length framing per the LSP specification:
//! ```text
//! Content-Length: {len}\r\n\r\n{json_body}
//! ```

use std::{
    io::{BufRead, BufReader, BufWriter, Read as IoRead, Write as IoWrite},
    process::{ChildStdin, ChildStdout},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Timeout for waiting for a response from the language server.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A JSON-RPC message (request, response, or notification).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcMessage {
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
}

/// A JSON-RPC response (has an `id` field).
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcResponse {
    pub id: Value,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
}

/// A JSON-RPC notification (no `id` field, has `method`).
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcNotification {
    pub method: String,
    pub params: Option<Value>,
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl std::fmt::Display for JsonRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LSP error {}: {}", self.code, self.message)
    }
}

/// JSON-RPC transport over child process stdin/stdout.
pub struct JsonRpcTransport {
    writer: BufWriter<ChildStdin>,
    reader: BufReader<ChildStdout>,
    next_id: i64,
}

impl JsonRpcTransport {
    pub fn new(stdin: ChildStdin, stdout: ChildStdout) -> Self {
        Self {
            writer: BufWriter::new(stdin),
            reader: BufReader::new(stdout),
            next_id: 1,
        }
    }

    /// Send a JSON-RPC request and wait for the matching response.
    ///
    /// Any notifications received while waiting are returned separately.
    pub fn send_request<P: Serialize>(
        &mut self,
        method: &str,
        params: P,
    ) -> Result<(Value, Vec<JsonRpcNotification>)> {
        let id = self.next_id;
        self.next_id += 1;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        self.write_message(&request)?;

        // Read messages until we get the matching response (with timeout)
        let mut notifications = Vec::new();
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        loop {
            if Instant::now() > deadline {
                return Err(anyhow::anyhow!(
                    "LSP request '{method}' timed out after {}s",
                    REQUEST_TIMEOUT.as_secs()
                ));
            }

            let msg = self.read_message()?;
            match msg {
                JsonRpcMessage::Response(resp) => {
                    if resp.id == serde_json::json!(id) {
                        if let Some(err) = resp.error {
                            return Err(anyhow::anyhow!("{err}"));
                        }
                        return Ok((resp.result.unwrap_or(Value::Null), notifications));
                    }
                    // Response for a different id — shouldn't happen in single-threaded usage
                    tracing::warn!(expected = id, got = %resp.id, "unexpected response id");
                }
                JsonRpcMessage::Notification(notif) => {
                    notifications.push(notif);
                }
            }
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    pub fn send_notification<P: Serialize>(&mut self, method: &str, params: P) -> Result<()> {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&notification)
    }

    /// Write a Content-Length-framed JSON-RPC message.
    fn write_message(&mut self, msg: &Value) -> Result<()> {
        let body = serde_json::to_string(msg)?;
        write!(
            self.writer,
            "Content-Length: {}\r\n\r\n{}",
            body.len(),
            body
        )?;
        self.writer.flush()?;
        Ok(())
    }

    /// Read one Content-Length-framed JSON-RPC message.
    pub fn read_message(&mut self) -> Result<JsonRpcMessage> {
        // Read headers until we find Content-Length
        let mut content_length: Option<usize> = None;
        loop {
            let mut header_line = String::new();
            self.reader.read_line(&mut header_line)?;
            let header_line = header_line.trim();

            if header_line.is_empty() {
                // Empty line separates headers from body
                break;
            }

            if let Some(len_str) = header_line.strip_prefix("Content-Length: ") {
                content_length = Some(
                    len_str
                        .trim()
                        .parse()
                        .context("invalid Content-Length value")?,
                );
            }
            // Ignore other headers (e.g., Content-Type)
        }

        let length =
            content_length.ok_or_else(|| anyhow::anyhow!("missing Content-Length header"))?;

        // Read exactly `length` bytes
        let mut body = vec![0u8; length];
        self.reader.read_exact(&mut body)?;

        let msg: JsonRpcMessage =
            serde_json::from_slice(&body).context("failed to parse JSON-RPC message")?;
        Ok(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_rpc_response_deserialize() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        match msg {
            JsonRpcMessage::Response(resp) => {
                assert_eq!(resp.id, serde_json::json!(1));
                assert!(resp.result.is_some());
                assert!(resp.error.is_none());
            }
            JsonRpcMessage::Notification(_) => panic!("expected response"),
        }
    }

    #[test]
    fn json_rpc_notification_deserialize() {
        let json = r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///test.rs","diagnostics":[]}}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        match msg {
            JsonRpcMessage::Notification(notif) => {
                assert_eq!(notif.method, "textDocument/publishDiagnostics");
                assert!(notif.params.is_some());
            }
            JsonRpcMessage::Response(_) => panic!("expected notification"),
        }
    }

    #[test]
    fn json_rpc_error_response_deserialize() {
        let json =
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        match msg {
            JsonRpcMessage::Response(resp) => {
                assert!(resp.error.is_some());
                let err = resp.error.unwrap();
                assert_eq!(err.code, -32601);
                assert_eq!(err.message, "Method not found");
            }
            JsonRpcMessage::Notification(_) => panic!("expected response"),
        }
    }

    #[test]
    fn json_rpc_error_display() {
        let err = JsonRpcError {
            code: -32601,
            message: "Method not found".into(),
            data: None,
        };
        assert_eq!(err.to_string(), "LSP error -32601: Method not found");
    }
}
