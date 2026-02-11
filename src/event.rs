use crossterm::event::Event;
use serde_json::Value;

use crate::permission::types::PermissionRequest;
use crate::tool::ToolOutput;

#[derive(Debug)]
pub enum AppEvent {
    /// Terminal input event (keyboard, mouse, resize)
    Input(Event),
    /// Periodic tick for UI refresh (spinners, etc.)
    Tick,

    // -- LLM streaming events --

    /// A text delta from the LLM stream (token-by-token).
    LlmDelta { text: String },
    /// A tool call has been assembled from the stream and is ready to execute.
    LlmToolCall {
        call_id: String,
        tool_name: String,
        arguments: Value,
    },
    /// A tool call has finished executing.
    ToolResult {
        call_id: String,
        tool_name: String,
        output: ToolOutput,
    },
    /// The LLM stream has finished (no more tool calls). Contains token usage if available.
    LlmFinish { usage: Option<StreamUsage> },
    /// LLM error (stream failure or API error).
    LlmError { error: String },

    // -- Permission events --

    /// A tool call needs user permission before executing.
    PermissionRequest(PermissionRequest),

    // -- Compact events --

    /// Compaction completed successfully with a summary.
    CompactFinish { summary: String },
    /// Compaction failed.
    CompactError { error: String },
}

/// Token usage reported at the end of a streaming response.
#[derive(Debug, Clone, Default)]
pub struct StreamUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}
