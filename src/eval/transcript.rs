//! Diff-stable, judge-consumable transcript shape.
//!
//! `CapturedRun` carries fields that are noise for diffs and baselines:
//! exact timestamps, full duration in nanoseconds, workspace tempdir paths
//! (UUID-bearing), tool-call UUIDs. `Normalizer` strips or canonicalizes
//! those, producing a `NormalizedTranscript` that's stable across runs of
//! the same scenario and that the Phase 7 judge can paired-compare against
//! a baseline.
//!
//! The shape captures only what the agent did. Scenario-level inputs
//! (user_turns) live one level up on `ScenarioResults` and `BaselineFile`;
//! provenance (model identity, git_ref, recorded_at, frozen_at) lives on
//! `ResultsFile` and `BaselineFile`. A `NormalizedTranscript` in isolation
//! is intentionally not self-describing — it's the wire format for the
//! judge, not a record format for humans.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tool::ToolName;

/// One event in the agent's behavior trace. Tool calls and tool results
/// are emitted as separate events so the judge prompt can see both the
/// arguments the agent supplied AND the response the tool produced —
/// both are load-bearing for "did the agent do the right thing."
///
/// Order in `NormalizedTranscript.events` is execution order: a turn's
/// tool calls (interleaved with their matching results) appear before
/// that turn's final assistant message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TranscriptEvent {
    ToolCall {
        tool_name: ToolName,
        arguments: Value,
    },
    ToolResult {
        tool_name: ToolName,
        output: String,
        is_error: bool,
    },
    AssistantMessage {
        text: String,
    },
}

/// Aggregate run-level numbers kept in the transcript for informational
/// signal. Token counts are tracked across runs as a usage signal but are
/// NOT used by the judge (efficiency on tool-call count is a different
/// dimension from token efficiency, and conflating them muddies the axis).
/// `duration_ms` is rounded to whole milliseconds so jitter doesn't dirty
/// the diff.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSummary {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub duration_ms: u64,
}

/// A single agent run, normalized for diff-stable storage and apples-to-
/// apples judge input. Per-transcript shape: captures only what the agent
/// did, NOT the scenario inputs (user_turns) it was responding to and NOT
/// the run-level provenance (model, recorded_at, git_ref) — those live on
/// `ResultsFile` / `ScenarioResults` / `BaselineFile`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedTranscript {
    pub events: Vec<TranscriptEvent>,
    pub deterministic_floor_passed: bool,
    pub usage_summary: UsageSummary,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn transcript_event_tool_call_round_trips() {
        let evt = TranscriptEvent::ToolCall {
            tool_name: ToolName::Read,
            arguments: json!({"path": "foo.txt"}),
        };
        let s = serde_json::to_string(&evt).unwrap();
        let back: TranscriptEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(evt, back);
    }

    #[test]
    fn transcript_event_tool_result_round_trips() {
        let evt = TranscriptEvent::ToolResult {
            tool_name: ToolName::Read,
            output: "hello\n".into(),
            is_error: false,
        };
        let s = serde_json::to_string(&evt).unwrap();
        let back: TranscriptEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(evt, back);
    }

    #[test]
    fn transcript_event_assistant_message_round_trips() {
        let evt = TranscriptEvent::AssistantMessage {
            text: "I read the file.".into(),
        };
        let s = serde_json::to_string(&evt).unwrap();
        let back: TranscriptEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(evt, back);
    }

    #[test]
    fn transcript_event_uses_kind_tag_with_snake_case() {
        let evt = TranscriptEvent::AssistantMessage { text: "x".into() };
        let s = serde_json::to_string(&evt).unwrap();
        assert!(s.contains("\"kind\":\"assistant_message\""), "got: {s}");
    }

    #[test]
    fn usage_summary_default_all_zeros() {
        let u = UsageSummary::default();
        assert_eq!(u.prompt_tokens, 0);
        assert_eq!(u.completion_tokens, 0);
        assert_eq!(u.total_tokens, 0);
        assert_eq!(u.duration_ms, 0);
    }

    #[test]
    fn normalized_transcript_round_trips_via_json() {
        let t = NormalizedTranscript {
            events: vec![
                TranscriptEvent::ToolCall {
                    tool_name: ToolName::Read,
                    arguments: json!({"path": "foo.txt"}),
                },
                TranscriptEvent::ToolResult {
                    tool_name: ToolName::Read,
                    output: "hello".into(),
                    is_error: false,
                },
                TranscriptEvent::AssistantMessage {
                    text: "It says hello.".into(),
                },
            ],
            deterministic_floor_passed: true,
            usage_summary: UsageSummary {
                prompt_tokens: 100,
                completion_tokens: 20,
                total_tokens: 120,
                duration_ms: 1234,
            },
        };
        let s = serde_json::to_string(&t).unwrap();
        let back: NormalizedTranscript = serde_json::from_str(&s).unwrap();
        assert_eq!(t, back);
    }
}
