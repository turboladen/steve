use crossterm::event::Event;
use serde_json::Value;

use crate::permission::types::PermissionRequest;
use crate::tool::{ToolName, ToolOutput};

pub struct QuestionRequest {
    pub call_id: String,
    pub question: String,
    pub options: Vec<String>,
    pub response_tx: tokio::sync::oneshot::Sender<String>,
}

impl std::fmt::Debug for QuestionRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuestionRequest")
            .field("call_id", &self.call_id)
            .field("question", &self.question)
            .field("options", &self.options)
            .field("response_tx", &"<oneshot::Sender>")
            .finish()
    }
}

#[derive(Debug)]
pub enum AppEvent {
    /// Terminal input event (keyboard, mouse, resize)
    Input(Event),
    /// Periodic tick for UI refresh (spinners, etc.)
    Tick,

    // -- LLM streaming events --

    /// A text delta from the LLM stream (token-by-token).
    LlmDelta { text: String },
    /// Reasoning/thinking tokens from the LLM.
    LlmReasoning { text: String },
    /// A new tool call is being streamed (name just identified, not yet complete).
    LlmToolCallStreaming {
        /// Number of tool calls seen so far in this response.
        count: usize,
        tool_name: ToolName,
    },
    /// A tool call has been assembled from the stream and is ready to execute.
    LlmToolCall {
        call_id: String,
        tool_name: ToolName,
        arguments: Value,
    },
    /// A tool call has finished executing.
    ToolResult {
        call_id: String,
        tool_name: ToolName,
        output: ToolOutput,
    },
    /// The LLM stream has finished (no more tool calls). Contains token usage if available.
    LlmFinish { usage: Option<StreamUsage> },
    /// Intermediate token usage update during a tool call loop.
    /// Sent after each API response so the UI can show incremental token counts.
    LlmUsageUpdate { usage: StreamUsage },
    /// LLM connection is being retried after a transient error.
    LlmRetry { attempt: u32, max_attempts: u32, error: String },
    /// LLM error (stream failure or API error).
    LlmError { error: String },
    /// System-level notification from the stream task (e.g., tool loop warnings).
    /// Displayed as a MessageBlock::System in the TUI.
    StreamNotice { text: String },
    /// LSP servers have been initialized; carries detected server binaries with running status.
    LspStatus { servers: Vec<(String, bool)> },

    // -- Permission events --

    /// A tool call needs user permission before executing.
    PermissionRequest(PermissionRequest),

    /// The question tool needs user input.
    QuestionRequest(QuestionRequest),

    // -- Compact events --

    /// Compaction completed successfully with a summary.
    CompactFinish { summary: String },
    /// Compaction failed.
    CompactError { error: String },

    // -- Title generation events --

    /// Async LLM title generation completed.
    TitleGenerated {
        session_id: String,
        title: String,
    },
    /// Async LLM title generation failed; carry pre-computed fallback title.
    TitleError {
        session_id: String,
        fallback_title: String,
    },
}

/// Token usage reported at the end of a streaming response.
#[derive(Debug, Clone, Default)]
pub struct StreamUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}
