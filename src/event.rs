use crossterm::event::Event;
use serde_json::Value;

use crate::{
    permission::types::PermissionRequest,
    tool::{ToolName, ToolOutput},
};

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
    /// The LLM is starting a new response (e.g., after an interjection).
    /// The UI should push a fresh Assistant block and persistence message.
    LlmResponseStart,
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
    LlmRetry {
        attempt: u32,
        max_attempts: u32,
        error: String,
    },
    /// LLM error (stream failure or API error).
    LlmError { error: String },
    /// System-level notification from the stream task (e.g., tool loop warnings).
    /// Displayed as a `MessageBlock::System` in the TUI. Identical consecutive
    /// `text` values fold into a single block with a `" (×N)"` repeat counter.
    ///
    /// Emit-site convention: avoid endings of the form `" (×N)"` in `text`,
    /// since the renderer reserves that exact suffix for the dedupe counter.
    /// A literal trailing `" (×N)"` won't crash anything, but may produce a
    /// cosmetically odd `"… (×N) (×M)"` display when the same literal
    /// notice repeats. Every current emit site is safe by construction.
    StreamNotice { text: String },
    /// Progress update from a sub-agent — updates the agent tool call's inline progress.
    /// Unlike `StreamNotice`, this does NOT push a new `MessageBlock::System` — it
    /// modifies the existing `ToolCall` in the assistant block, keeping the assistant
    /// block as the last message so follow-up text remains visible.
    AgentProgress {
        call_id: String,
        tool_name: ToolName,
        args_summary: String,
        result_summary: Option<String>,
    },
    /// MCP servers have been initialized; carries status for all configured servers
    /// (both connected and failed).
    McpStatus {
        servers: Vec<crate::ui::sidebar::SidebarMcp>,
    },
    /// An LSP server crashed and should be restarted. Sent by the crash
    /// watcher after a backoff delay. The event loop handles this by
    /// calling `LspManager::restart_server(lang)` in a `spawn_blocking`.
    LspRestartNeeded { lang: crate::lsp::Language },

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

    // -- AGENTS.md update events --
    /// LLM has generated a proposed AGENTS.md update.
    AgentsUpdateFinish { proposed_content: String },
    /// AGENTS.md update generation failed.
    AgentsUpdateError { error: String },

    // -- Title generation events --
    /// Async LLM title generation completed.
    TitleGenerated { session_id: String, title: String },
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

impl std::ops::AddAssign for StreamUsage {
    fn add_assign(&mut self, rhs: Self) {
        self.prompt_tokens += rhs.prompt_tokens;
        self.completion_tokens += rhs.completion_tokens;
        self.total_tokens += rhs.total_tokens;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_usage_default_all_zeros() {
        let usage = StreamUsage::default();
        assert_eq!(usage.prompt_tokens, 0);
        assert_eq!(usage.completion_tokens, 0);
        assert_eq!(usage.total_tokens, 0);
    }

    #[test]
    fn stream_usage_add_assign_sums_fields() {
        let mut a = StreamUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
        };
        let b = StreamUsage {
            prompt_tokens: 200,
            completion_tokens: 75,
            total_tokens: 275,
        };
        a += b;
        assert_eq!(a.prompt_tokens, 300);
        assert_eq!(a.completion_tokens, 125);
        assert_eq!(a.total_tokens, 425);
    }

    #[test]
    fn stream_usage_add_assign_identity() {
        let mut usage = StreamUsage {
            prompt_tokens: 42,
            completion_tokens: 13,
            total_tokens: 55,
        };
        usage += StreamUsage::default();
        assert_eq!(usage.prompt_tokens, 42);
        assert_eq!(usage.completion_tokens, 13);
        assert_eq!(usage.total_tokens, 55);
    }

    #[test]
    fn question_request_debug_contains_fields_and_redacts_sender() {
        let (tx, _rx) = tokio::sync::oneshot::channel::<String>();
        let req = QuestionRequest {
            call_id: "call-123".to_string(),
            question: "What color?".to_string(),
            options: vec!["red".to_string(), "blue".to_string()],
            response_tx: tx,
        };
        let debug = format!("{req:?}");
        assert!(debug.contains("call_id"));
        assert!(debug.contains("call-123"));
        assert!(debug.contains("question"));
        assert!(debug.contains("What color?"));
        assert!(debug.contains("options"));
        assert!(debug.contains("red"));
        assert!(debug.contains("<oneshot::Sender>"));
        // Must NOT contain the actual Sender debug repr
        assert!(!debug.contains("Sender {"));
    }
}
