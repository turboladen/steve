//! Per-run capture state.
//!
//! `Capture::observe` is invoked for every `AppEvent` that flows through
//! `App::run_until_idle`. It accumulates the trace data the Phase 3
//! evaluator needs (tool calls in stream-emit order, the assistant message
//! text per turn, final token usage). The match is exhaustive so adding a
//! new `AppEvent` variant is a compile error here — the new event might be
//! relevant to scenario behavior and the eval suite shouldn't silently drop it.

use std::{path::PathBuf, time::Duration};

use crossterm::event::Event;
use serde::Serialize;
use serde_json::Value;

use crate::{
    eval::workspace::WorkspaceSnapshot,
    event::{AppEvent, StreamUsage},
    tool::ToolName,
};

#[derive(Debug, Clone)]
pub struct CapturedRun {
    pub workspace_root: PathBuf,
    pub baseline: WorkspaceSnapshot,
    pub tool_calls: Vec<RecordedToolCall>,
    /// One entry per terminated user turn — pushed on `LlmFinish` (normal
    /// completion) AND on `LlmError` (abnormal termination). Empty string
    /// when the turn produced only tool calls and no narration, or when
    /// the turn errored before emitting any text. Preserved as `""` so
    /// `assistant_messages.last()` always corresponds to the LAST user
    /// turn's response, not a stale earlier turn's text. Per-turn
    /// correspondence holds for both completion paths.
    pub assistant_messages: Vec<String>,
    pub usage: Option<StreamUsage>,
    pub duration: Duration,
    pub timed_out: bool,
    /// Errors emitted by the LLM stream (transient retries are logged but not
    /// stored; only `LlmError` terminal failures land here).
    pub errors: Vec<String>,
    /// Scratch buffer for the in-progress assistant message text. Flushed
    /// into `assistant_messages` on `LlmFinish`. Not part of the public
    /// output contract.
    pending_assistant_text: String,
    /// 0-based index of the turn currently being captured. Stamped onto
    /// each `RecordedToolCall.turn_index` and incremented on `LlmFinish`
    /// / `LlmError` (the events that flush a completed turn). Not part
    /// of the public output contract.
    current_turn: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordedToolCall {
    pub call_id: String,
    pub tool_name: ToolName,
    pub arguments: Value,
    /// `Some(text)` once the matching `ToolResult` event is observed; `None`
    /// for tools whose execution never completed (timeout, panic).
    pub output: Option<String>,
    pub is_error: bool,
    /// 0-based turn index this call was emitted during. Lets the judge
    /// prompt render tool calls interleaved with their turn's final
    /// assistant message rather than grouping all calls before all
    /// messages — without this, multi-turn scenarios would show turn 2's
    /// tool calls *before* turn 1's final message in the prompt and the
    /// judge would infer wrong cross-turn chronology from layout order.
    pub turn_index: usize,
}

impl CapturedRun {
    /// Whether the run terminated normally — no stream errors and no
    /// per-turn timeout. The CLI verdict combines this with
    /// `EvalReport::passed()` so a scenario that aborts mid-stream
    /// (LlmError) doesn't report `passed = true` even when an early
    /// expectation was satisfied before the abort.
    pub fn completed_normally(&self) -> bool {
        !self.timed_out && self.errors.is_empty()
    }

    pub fn new(workspace_root: PathBuf, baseline: WorkspaceSnapshot) -> Self {
        Self {
            workspace_root,
            baseline,
            tool_calls: Vec::new(),
            assistant_messages: Vec::new(),
            usage: None,
            duration: Duration::ZERO,
            timed_out: false,
            errors: Vec::new(),
            pending_assistant_text: String::new(),
            current_turn: 0,
        }
    }

    pub fn observe(&mut self, event: &AppEvent) {
        match event {
            AppEvent::LlmResponseStart => {
                // Defensive: if a previous turn's text never flushed (no
                // LlmFinish), drop it here so the new turn starts clean.
                self.pending_assistant_text.clear();
            }
            AppEvent::LlmDelta { text } => {
                self.pending_assistant_text.push_str(text);
            }
            AppEvent::LlmToolCall {
                call_id,
                tool_name,
                arguments,
            } => {
                self.tool_calls.push(RecordedToolCall {
                    call_id: call_id.clone(),
                    tool_name: *tool_name,
                    arguments: arguments.clone(),
                    output: None,
                    is_error: false,
                    turn_index: self.current_turn,
                });
            }
            AppEvent::ToolResult {
                call_id, output, ..
            } => {
                // Match by call_id; rev() so retried call_ids fill the
                // most-recent record (defensive — call_ids are expected unique).
                if let Some(call) = self
                    .tool_calls
                    .iter_mut()
                    .rev()
                    .find(|c| c.call_id == *call_id)
                {
                    call.output = Some(output.output.clone());
                    call.is_error = output.is_error;
                }
            }
            AppEvent::LlmFinish { usage } => {
                // Always push, even when empty — preserves per-turn
                // correspondence so `assistant_messages.last()` is always
                // the LAST turn's response (not a stale earlier turn whose
                // text happened to be non-empty).
                let text = std::mem::take(&mut self.pending_assistant_text);
                self.assistant_messages.push(text);
                if let Some(u) = usage {
                    self.usage = Some(u.clone());
                }
                self.current_turn += 1;
            }
            AppEvent::LlmError { error } => {
                self.errors.push(error.clone());
                // LlmError is terminal for the turn — also flush pending
                // text so per-turn correspondence holds even on errors.
                // Without this, an errored final turn would leave the
                // previous turn's text as `assistant_messages.last()` and
                // `final_message_*` would evaluate against stale content.
                let text = std::mem::take(&mut self.pending_assistant_text);
                self.assistant_messages.push(text);
                self.current_turn += 1;
            }

            AppEvent::Input(Event::Key(_))
            | AppEvent::Input(Event::Mouse(_))
            | AppEvent::Input(Event::Paste(_))
            | AppEvent::Input(Event::Resize(_, _))
            | AppEvent::Input(Event::FocusGained)
            | AppEvent::Input(Event::FocusLost) => {}
            AppEvent::Tick => {}
            AppEvent::LlmReasoning { .. } => {}
            AppEvent::LlmToolCallStreaming { .. } => {}
            AppEvent::LlmUsageUpdate { .. } => {}
            AppEvent::LlmRetry { .. } => {}
            AppEvent::StreamNotice { .. } => {}
            AppEvent::AgentProgress { .. } => {}
            AppEvent::McpStatus { .. } => {}
            AppEvent::LspRestartNeeded { .. } => {}
            AppEvent::PermissionRequest(_) => {}
            AppEvent::QuestionRequest(_) => {}
            AppEvent::CompactFinish { .. } => {}
            AppEvent::CompactError { .. } => {}
            AppEvent::AgentsUpdateFinish { .. } => {}
            AppEvent::AgentsUpdateError { .. } => {}
            AppEvent::TitleGenerated { .. } => {}
            AppEvent::TitleError { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::tool::ToolOutput;

    fn empty_capture() -> CapturedRun {
        CapturedRun::new(
            PathBuf::from("/tmp/eval-test"),
            WorkspaceSnapshot {
                files: BTreeMap::new(),
            },
        )
    }

    fn ok_output(text: &str) -> ToolOutput {
        ToolOutput {
            title: "test".into(),
            output: text.into(),
            is_error: false,
        }
    }

    #[test]
    fn observe_records_single_tool_call_with_result() {
        let mut cap = empty_capture();
        cap.observe(&AppEvent::LlmToolCall {
            call_id: "call-1".into(),
            tool_name: ToolName::Read,
            arguments: json!({"path": "foo.txt"}),
        });
        cap.observe(&AppEvent::ToolResult {
            call_id: "call-1".into(),
            tool_name: ToolName::Read,
            output: ok_output("file contents"),
        });
        assert_eq!(cap.tool_calls.len(), 1);
        let call = &cap.tool_calls[0];
        assert_eq!(call.call_id, "call-1");
        assert_eq!(call.tool_name, ToolName::Read);
        assert_eq!(call.arguments, json!({"path": "foo.txt"}));
        assert_eq!(call.output.as_deref(), Some("file contents"));
        assert!(!call.is_error);
    }

    #[test]
    fn observe_records_tool_calls_in_emit_order() {
        let mut cap = empty_capture();
        for (idx, name) in [
            ("c1", ToolName::Read),
            ("c2", ToolName::Grep),
            ("c3", ToolName::Edit),
        ]
        .iter()
        .enumerate()
        {
            cap.observe(&AppEvent::LlmToolCall {
                call_id: name.0.into(),
                tool_name: name.1,
                arguments: json!({"i": idx}),
            });
        }
        let tools: Vec<_> = cap.tool_calls.iter().map(|c| c.tool_name).collect();
        assert_eq!(tools, vec![ToolName::Read, ToolName::Grep, ToolName::Edit]);
    }

    #[test]
    fn observe_records_tool_call_without_result_as_pending() {
        let mut cap = empty_capture();
        cap.observe(&AppEvent::LlmToolCall {
            call_id: "abandoned".into(),
            tool_name: ToolName::Bash,
            arguments: json!({}),
        });
        assert_eq!(cap.tool_calls.len(), 1);
        assert!(cap.tool_calls[0].output.is_none());
    }

    #[test]
    fn observe_records_tool_error() {
        let mut cap = empty_capture();
        cap.observe(&AppEvent::LlmToolCall {
            call_id: "c".into(),
            tool_name: ToolName::Bash,
            arguments: json!({}),
        });
        cap.observe(&AppEvent::ToolResult {
            call_id: "c".into(),
            tool_name: ToolName::Bash,
            output: ToolOutput {
                title: "bash".into(),
                output: "command not found".into(),
                is_error: true,
            },
        });
        assert!(cap.tool_calls[0].is_error);
    }

    #[test]
    fn observe_accumulates_assistant_text_per_turn() {
        let mut cap = empty_capture();
        cap.observe(&AppEvent::LlmDelta {
            text: "Hello, ".into(),
        });
        cap.observe(&AppEvent::LlmDelta {
            text: "world.".into(),
        });
        cap.observe(&AppEvent::LlmFinish { usage: None });
        assert_eq!(cap.assistant_messages, vec!["Hello, world."]);
    }

    #[test]
    fn observe_records_one_message_per_turn() {
        let mut cap = empty_capture();
        // Turn 1
        cap.observe(&AppEvent::LlmDelta {
            text: "first turn".into(),
        });
        cap.observe(&AppEvent::LlmFinish { usage: None });
        // Turn 2 — the runner sends another user_turn after this
        cap.observe(&AppEvent::LlmResponseStart);
        cap.observe(&AppEvent::LlmDelta {
            text: "second turn".into(),
        });
        cap.observe(&AppEvent::LlmFinish { usage: None });

        assert_eq!(
            cap.assistant_messages,
            vec!["first turn".to_string(), "second turn".to_string()]
        );
    }

    #[test]
    fn observe_records_empty_string_for_tool_only_turns() {
        // A turn that emits only tool calls (no narration) MUST still push
        // an empty `""` so `assistant_messages.last()` reliably corresponds
        // to the final user turn — otherwise an earlier non-empty turn's
        // text would masquerade as the "final" message.
        let mut cap = empty_capture();
        cap.observe(&AppEvent::LlmToolCall {
            call_id: "c".into(),
            tool_name: ToolName::Read,
            arguments: json!({}),
        });
        cap.observe(&AppEvent::ToolResult {
            call_id: "c".into(),
            tool_name: ToolName::Read,
            output: ok_output("x"),
        });
        cap.observe(&AppEvent::LlmFinish { usage: None });
        assert_eq!(cap.assistant_messages, vec![String::new()]);
    }

    #[test]
    fn observe_preserves_per_turn_correspondence_with_mixed_turns() {
        // Two turns: first has narration, second is tool-only. The final-turn
        // assertion in expectations.rs depends on `assistant_messages.last()`
        // being the SECOND turn's empty response, not the first turn's text.
        let mut cap = empty_capture();
        cap.observe(&AppEvent::LlmDelta {
            text: "narrated turn".into(),
        });
        cap.observe(&AppEvent::LlmFinish { usage: None });
        cap.observe(&AppEvent::LlmResponseStart);
        cap.observe(&AppEvent::LlmToolCall {
            call_id: "c".into(),
            tool_name: ToolName::Read,
            arguments: json!({}),
        });
        cap.observe(&AppEvent::ToolResult {
            call_id: "c".into(),
            tool_name: ToolName::Read,
            output: ok_output("x"),
        });
        cap.observe(&AppEvent::LlmFinish { usage: None });

        assert_eq!(cap.assistant_messages.len(), 2);
        assert_eq!(cap.assistant_messages[0], "narrated turn");
        assert_eq!(cap.assistant_messages[1], "");
    }

    #[test]
    fn observe_response_start_clears_unflushed_text() {
        // Pathological case: text deltas without a finish, then a new response.
        // Stale text from the previous response shouldn't bleed in.
        let mut cap = empty_capture();
        cap.observe(&AppEvent::LlmDelta {
            text: "stale".into(),
        });
        cap.observe(&AppEvent::LlmResponseStart);
        cap.observe(&AppEvent::LlmDelta {
            text: "fresh".into(),
        });
        cap.observe(&AppEvent::LlmFinish { usage: None });
        assert_eq!(cap.assistant_messages, vec!["fresh"]);
    }

    #[test]
    fn observe_records_final_usage() {
        let mut cap = empty_capture();
        let usage = StreamUsage {
            prompt_tokens: 1000,
            completion_tokens: 100,
            total_tokens: 1100,
        };
        cap.observe(&AppEvent::LlmFinish {
            usage: Some(usage.clone()),
        });
        assert_eq!(cap.usage.as_ref().map(|u| u.total_tokens), Some(1100));
        assert_eq!(cap.usage.as_ref().map(|u| u.prompt_tokens), Some(1000));
    }

    #[test]
    fn observe_records_llm_errors() {
        let mut cap = empty_capture();
        cap.observe(&AppEvent::LlmError {
            error: "rate limit exceeded".into(),
        });
        assert_eq!(cap.errors, vec!["rate limit exceeded"]);
    }

    #[test]
    fn observe_llm_error_flushes_pending_text_to_preserve_per_turn_correspondence() {
        // Regression: `LlmError` is terminal for the turn but used to skip
        // the assistant_messages push, so an erroring final turn would
        // leave the previous turn's text as `last()` and final_message_*
        // would evaluate stale content.
        let mut cap = empty_capture();
        // Turn 1: completes normally with text.
        cap.observe(&AppEvent::LlmDelta {
            text: "first turn".into(),
        });
        cap.observe(&AppEvent::LlmFinish { usage: None });
        // Turn 2: emits some delta then errors before LlmFinish.
        cap.observe(&AppEvent::LlmResponseStart);
        cap.observe(&AppEvent::LlmDelta {
            text: "partial".into(),
        });
        cap.observe(&AppEvent::LlmError {
            error: "stream broken".into(),
        });

        assert_eq!(cap.assistant_messages.len(), 2, "error must push a turn");
        assert_eq!(cap.assistant_messages[0], "first turn");
        assert_eq!(
            cap.assistant_messages[1], "partial",
            "errored turn's partial text must be preserved as the last entry"
        );
        assert_eq!(cap.errors, vec!["stream broken"]);
    }

    #[test]
    fn completed_normally_reflects_errors_and_timeout() {
        let mut cap = empty_capture();
        assert!(cap.completed_normally(), "fresh run should be normal");

        cap.errors.push("rate limit".into());
        assert!(!cap.completed_normally(), "errors must flip verdict");
        cap.errors.clear();
        assert!(cap.completed_normally());

        cap.timed_out = true;
        assert!(!cap.completed_normally(), "timeout must flip verdict");
    }

    #[test]
    fn observe_ignores_irrelevant_events() {
        // Confirms the no-op arms truly are no-ops — nothing gets recorded.
        let mut cap = empty_capture();
        cap.observe(&AppEvent::Tick);
        cap.observe(&AppEvent::StreamNotice { text: "x".into() });
        cap.observe(&AppEvent::LlmReasoning {
            text: "thinking...".into(),
        });
        cap.observe(&AppEvent::LlmRetry {
            attempt: 1,
            max_attempts: 3,
            error: "transient".into(),
        });
        assert!(cap.tool_calls.is_empty());
        assert!(cap.assistant_messages.is_empty());
        assert!(cap.errors.is_empty());
        assert!(cap.usage.is_none());
    }

    #[test]
    fn full_turn_sequence_records_everything() {
        let mut cap = empty_capture();
        cap.observe(&AppEvent::LlmResponseStart);
        cap.observe(&AppEvent::LlmDelta {
            text: "Looking at the file. ".into(),
        });
        cap.observe(&AppEvent::LlmToolCall {
            call_id: "c1".into(),
            tool_name: ToolName::Read,
            arguments: json!({"path": "foo.txt"}),
        });
        cap.observe(&AppEvent::ToolResult {
            call_id: "c1".into(),
            tool_name: ToolName::Read,
            output: ok_output("hello\n"),
        });
        cap.observe(&AppEvent::LlmDelta {
            text: "It says hello.".into(),
        });
        cap.observe(&AppEvent::LlmFinish {
            usage: Some(StreamUsage {
                prompt_tokens: 50,
                completion_tokens: 12,
                total_tokens: 62,
            }),
        });

        assert_eq!(cap.tool_calls.len(), 1);
        assert_eq!(cap.tool_calls[0].tool_name, ToolName::Read);
        assert_eq!(cap.tool_calls[0].output.as_deref(), Some("hello\n"));
        assert_eq!(
            cap.assistant_messages,
            vec!["Looking at the file. It says hello."]
        );
        assert_eq!(cap.usage.as_ref().map(|u| u.total_tokens), Some(62));
    }
}
