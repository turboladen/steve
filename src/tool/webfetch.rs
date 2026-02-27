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

    // Use a blocking HTTP request since our tool handlers are synchronous
    let response = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
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
        // Truncate very long content
        if text.len() > 50_000 {
            format!("{}\n\n... (content truncated at 50KB)", &text[..50_000])
        } else {
            text
        }
    } else {
        // Return raw text (JSON, plain text, etc.)
        if body.len() > 50_000 {
            format!("{}\n\n... (content truncated at 50KB)", &body[..50_000])
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
