//! Webfetch tool — HTTP GET with HTML-to-text conversion.

use anyhow::{Context, Result};
use serde_json::Value;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

pub fn tool() -> ToolEntry {
    let def_json = definition();
    let func = def_json.get("function").unwrap();
    ToolEntry {
        def: ToolDef {
            name: ToolName::Webfetch,
            description: func.get("description").unwrap().as_str().unwrap().to_string(),
            parameters: func.get("parameters").cloned().unwrap(),
        },
        handler: Box::new(execute),
    }
}

fn definition() -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "webfetch",
            "description": "Fetch a URL and return its content as plain text. HTML pages are converted to readable text. Useful for reading documentation, API responses, or web pages.",
            "parameters": {
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch."
                    }
                },
                "required": ["url"]
            }
        }
    })
}

fn execute(args: Value, _ctx: ToolContext) -> Result<ToolOutput> {
    let url = args
        .get("url")
        .and_then(|v| v.as_str())
        .context("missing 'url' parameter")?;

    // Reject non-HTTP(S) schemes to prevent SSRF via file://, data:, etc.
    let parsed = reqwest::Url::parse(url)
        .with_context(|| format!("invalid URL: {url}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        scheme => anyhow::bail!("disallowed URL scheme '{scheme}': only http and https are permitted"),
    }

    // Use a blocking HTTP request since our tool handlers are synchronous.
    // Custom redirect policy rejects scheme changes (e.g., http→file:// SSRF).
    let response = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            match attempt.url().scheme() {
                "http" | "https" => attempt.follow(),
                _ => attempt.error(anyhow::anyhow!("redirect to non-http scheme blocked")),
            }
        }))
        .user_agent("steve/0.1")
        .build()?
        .get(url)
        .send()
        .with_context(|| format!("failed to fetch URL: {url}"))?;

    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let body = response.text().context("failed to read response body")?;

    let output = if content_type.contains("text/html") {
        // Convert HTML to plain text
        let text = html2text::from_read(body.as_bytes(), 100)
            .unwrap_or_else(|_| body.clone());
        // Truncate very long content (use floor_char_boundary for UTF-8 safety)
        if text.len() > 50_000 {
            let boundary = floor_char_boundary(&text, 50_000);
            format!("{}\n\n... (content truncated at 50KB)", &text[..boundary])
        } else {
            text
        }
    } else {
        // Return raw text (JSON, plain text, etc.)
        if body.len() > 50_000 {
            let boundary = floor_char_boundary(&body, 50_000);
            format!("{}\n\n... (content truncated at 50KB)", &body[..boundary])
        } else {
            body
        }
    };

    let title = format!("Fetch {url}");
    Ok(ToolOutput {
        title,
        output: format!("[{status}]\n{output}"),
        is_error: !status.is_success(),
    })
}

/// Find the largest valid UTF-8 char boundary at or before `index`.
/// This is a polyfill for the unstable `str::floor_char_boundary`.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_char_boundary_ascii_exact() {
        let s = "hello world";
        assert_eq!(floor_char_boundary(s, 5), 5);
    }

    #[test]
    fn floor_char_boundary_multibyte_snaps_back() {
        // '日' is 3 bytes in UTF-8
        let s = "a日b";
        // s = [0x61, 0xE6, 0x97, 0xA5, 0x62]
        // index 2 is mid-character (inside '日'), should snap back to 1
        assert_eq!(floor_char_boundary(s, 2), 1);
        assert_eq!(floor_char_boundary(s, 3), 1);
        // index 4 is exactly at 'b'
        assert_eq!(floor_char_boundary(s, 4), 4);
    }

    #[test]
    fn floor_char_boundary_beyond_length() {
        let s = "abc";
        assert_eq!(floor_char_boundary(s, 100), 3);
    }

    #[test]
    fn floor_char_boundary_empty_string() {
        assert_eq!(floor_char_boundary("", 0), 0);
    }

    #[test]
    fn floor_char_boundary_four_byte_emoji() {
        // '🦀' is 4 bytes: F0 9F A6 80
        let s = "a🦀b";
        // s = [0x61, 0xF0, 0x9F, 0xA6, 0x80, 0x62]
        // Indices 2, 3, 4 are mid-emoji, should snap back to 1
        assert_eq!(floor_char_boundary(s, 2), 1);
        assert_eq!(floor_char_boundary(s, 3), 1);
        assert_eq!(floor_char_boundary(s, 4), 1);
        // index 5 is exactly 'b'
        assert_eq!(floor_char_boundary(s, 5), 5);
    }

    #[test]
    fn execute_missing_url_returns_error() {
        let ctx = ToolContext {
            project_root: std::path::PathBuf::from("/tmp"),
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        };
        let result = execute(serde_json::json!({}), ctx);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("url"), "error should mention missing url param: {err}");
    }

    #[test]
    fn tool_definition_parses() {
        let entry = tool();
        assert_eq!(entry.def.name, ToolName::Webfetch);
        assert!(!entry.def.description.is_empty());
    }

    #[test]
    fn rejects_file_scheme() {
        let ctx = ToolContext {
            project_root: std::path::PathBuf::from("/tmp"),
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        };
        let result = execute(serde_json::json!({"url": "file:///etc/passwd"}), ctx);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("disallowed"), "should reject file:// scheme: {err}");
    }

    #[test]
    fn rejects_data_scheme() {
        let ctx = ToolContext {
            project_root: std::path::PathBuf::from("/tmp"),
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        };
        let result = execute(serde_json::json!({"url": "data:text/plain,hello"}), ctx);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("disallowed"), "should reject data: scheme: {err}");
    }
}
