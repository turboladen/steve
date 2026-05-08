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

/// Pure transformation from `CapturedRun` to `NormalizedTranscript`. No
/// I/O. Used at two boundaries: freeze time (wraps the result in a
/// `BaselineFile` for disk write) and report time (passes the result to
/// the judge).
pub struct Normalizer;

impl Normalizer {
    /// `deterministic_floor_passed` is computed by the caller from
    /// `EvalReport::passed() && captured.completed_normally()`. Passing
    /// it in keeps `Normalizer` decoupled from `expectations.rs`.
    pub fn normalize(
        captured: &crate::eval::capture::CapturedRun,
        deterministic_floor_passed: bool,
    ) -> NormalizedTranscript {
        let workspace_str = captured.workspace_root.to_string_lossy().into_owned();
        let mut events =
            Vec::with_capacity(captured.tool_calls.len() * 2 + captured.assistant_messages.len());

        // Walk turns in order. For each turn t: emit (ToolCall, ToolResult)
        // pairs for every recorded call whose turn_index == t (in their
        // original emit order — `captured.tool_calls` is already in emit
        // order), then emit the AssistantMessage for that turn (skipping
        // empties — see test rationale).
        for (turn_idx, msg) in captured.assistant_messages.iter().enumerate() {
            for call in captured
                .tool_calls
                .iter()
                .filter(|c| c.turn_index == turn_idx)
            {
                let stripped_args = strip_workspace(&call.arguments, &workspace_str);
                events.push(TranscriptEvent::ToolCall {
                    tool_name: call.tool_name,
                    arguments: stripped_args,
                });
                if let Some(output) = &call.output {
                    events.push(TranscriptEvent::ToolResult {
                        tool_name: call.tool_name,
                        output: output.replace(&workspace_str, ""),
                        is_error: call.is_error,
                    });
                }
            }
            if !msg.is_empty() {
                events.push(TranscriptEvent::AssistantMessage { text: msg.clone() });
            }
        }

        let usage_summary = UsageSummary {
            prompt_tokens: captured
                .usage
                .as_ref()
                .map(|u| u.prompt_tokens)
                .unwrap_or(0),
            completion_tokens: captured
                .usage
                .as_ref()
                .map(|u| u.completion_tokens)
                .unwrap_or(0),
            total_tokens: captured.usage.as_ref().map(|u| u.total_tokens).unwrap_or(0),
            duration_ms: captured.duration.as_millis() as u64,
        };

        NormalizedTranscript {
            events,
            deterministic_floor_passed,
            usage_summary,
        }
    }
}

/// Recursively walk a `serde_json::Value` and replace `workspace_root`
/// substrings inside string fields with the empty string. The resulting
/// path becomes "/foo.txt" rather than "<workspace>/foo.txt"; that's
/// fine for diff stability — what matters is that the UUID-bearing
/// tempdir prefix is gone.
fn strip_workspace(v: &Value, workspace: &str) -> Value {
    match v {
        Value::String(s) => Value::String(s.replace(workspace, "")),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|x| strip_workspace(x, workspace))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, vv)| (k.clone(), strip_workspace(vv, workspace)))
                .collect(),
        ),
        Value::Null | Value::Bool(_) | Value::Number(_) => v.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::{
        eval::{capture::CapturedRun, workspace::WorkspaceSnapshot},
        event::{AppEvent, StreamUsage},
        tool::ToolOutput,
    };
    use std::{collections::BTreeMap, path::PathBuf, time::Duration};

    fn captured_with_workspace(root: &str) -> CapturedRun {
        CapturedRun::new(
            PathBuf::from(root),
            WorkspaceSnapshot {
                files: BTreeMap::new(),
            },
        )
    }

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

    #[test]
    fn normalize_interleaves_tool_call_result_then_assistant_message_per_turn() {
        let mut cap = captured_with_workspace("/tmp/eval-x");
        cap.observe(&AppEvent::LlmDelta {
            text: "Looking. ".into(),
        });
        cap.observe(&AppEvent::LlmToolCall {
            call_id: "uuid-1".into(),
            tool_name: ToolName::Read,
            arguments: json!({"path": "foo.txt"}),
        });
        cap.observe(&AppEvent::ToolResult {
            call_id: "uuid-1".into(),
            tool_name: ToolName::Read,
            output: ToolOutput {
                title: "read".into(),
                output: "hello".into(),
                is_error: false,
            },
        });
        cap.observe(&AppEvent::LlmDelta {
            text: "Done.".into(),
        });
        cap.observe(&AppEvent::LlmFinish { usage: None });

        let t = Normalizer::normalize(&cap, true);
        assert_eq!(t.events.len(), 3, "1 call + 1 result + 1 message");
        assert!(matches!(t.events[0], TranscriptEvent::ToolCall { .. }));
        assert!(matches!(t.events[1], TranscriptEvent::ToolResult { .. }));
        match &t.events[2] {
            TranscriptEvent::AssistantMessage { text } => assert_eq!(text, "Looking. Done."),
            other => panic!("expected assistant_message, got {other:?}"),
        }
    }

    #[test]
    fn normalize_preserves_per_turn_order_across_multiple_turns() {
        let mut cap = captured_with_workspace("/tmp/eval-x");
        // Turn 1
        cap.observe(&AppEvent::LlmDelta {
            text: "first".into(),
        });
        cap.observe(&AppEvent::LlmFinish { usage: None });
        // Turn 2
        cap.observe(&AppEvent::LlmResponseStart);
        cap.observe(&AppEvent::LlmToolCall {
            call_id: "u".into(),
            tool_name: ToolName::Grep,
            arguments: json!({"pattern": "x"}),
        });
        cap.observe(&AppEvent::ToolResult {
            call_id: "u".into(),
            tool_name: ToolName::Grep,
            output: ToolOutput {
                title: "grep".into(),
                output: "match".into(),
                is_error: false,
            },
        });
        cap.observe(&AppEvent::LlmDelta {
            text: "second".into(),
        });
        cap.observe(&AppEvent::LlmFinish { usage: None });

        let t = Normalizer::normalize(&cap, true);
        let kinds: Vec<&'static str> = t
            .events
            .iter()
            .map(|e| match e {
                TranscriptEvent::ToolCall { .. } => "call",
                TranscriptEvent::ToolResult { .. } => "result",
                TranscriptEvent::AssistantMessage { .. } => "msg",
            })
            .collect();
        // Expect: msg(turn1), call(turn2), result(turn2), msg(turn2)
        assert_eq!(kinds, vec!["msg", "call", "result", "msg"]);
    }

    #[test]
    fn normalize_drops_empty_assistant_messages_for_tool_only_turns() {
        // For diff stability we WANT to drop them: keeping `""` events makes
        // a "tool-only turn" indistinguishable from "agent emitted nothing"
        // in the rendered transcript, and the empty event has no content
        // for the judge to grade against. Per-turn ordering is encoded by
        // the surrounding events, not by an empty marker.
        let mut cap = captured_with_workspace("/tmp/eval-x");
        cap.observe(&AppEvent::LlmToolCall {
            call_id: "u".into(),
            tool_name: ToolName::Read,
            arguments: json!({}),
        });
        cap.observe(&AppEvent::ToolResult {
            call_id: "u".into(),
            tool_name: ToolName::Read,
            output: ToolOutput {
                title: "x".into(),
                output: "y".into(),
                is_error: false,
            },
        });
        cap.observe(&AppEvent::LlmFinish { usage: None });

        let t = Normalizer::normalize(&cap, true);
        let has_empty_msg = t.events.iter().any(|e| {
            matches!(
                e,
                TranscriptEvent::AssistantMessage { text } if text.is_empty()
            )
        });
        assert!(
            !has_empty_msg,
            "empty assistant_message events must be dropped: {:?}",
            t.events
        );
    }

    #[test]
    fn normalize_drops_pending_tool_results_when_call_never_completed() {
        // A tool whose ToolResult never arrived (timeout, panic) has
        // output: None on RecordedToolCall. We emit only the ToolCall event;
        // dropping the never-completed ToolResult keeps the transcript
        // honest about what actually happened.
        let mut cap = captured_with_workspace("/tmp/eval-x");
        cap.observe(&AppEvent::LlmToolCall {
            call_id: "abandoned".into(),
            tool_name: ToolName::Bash,
            arguments: json!({}),
        });
        cap.observe(&AppEvent::LlmFinish { usage: None });

        let t = Normalizer::normalize(&cap, false);
        let calls = t
            .events
            .iter()
            .filter(|e| matches!(e, TranscriptEvent::ToolCall { .. }))
            .count();
        let results = t
            .events
            .iter()
            .filter(|e| matches!(e, TranscriptEvent::ToolResult { .. }))
            .count();
        assert_eq!(calls, 1);
        assert_eq!(results, 0);
    }

    #[test]
    fn normalize_strips_workspace_root_from_argument_strings() {
        let workspace = "/tmp/eval-abc-123";
        let mut cap = captured_with_workspace(workspace);
        cap.observe(&AppEvent::LlmToolCall {
            call_id: "u".into(),
            tool_name: ToolName::Read,
            arguments: json!({"path": format!("{workspace}/foo.txt")}),
        });
        cap.observe(&AppEvent::ToolResult {
            call_id: "u".into(),
            tool_name: ToolName::Read,
            output: ToolOutput {
                title: "x".into(),
                output: format!("opened {workspace}/foo.txt"),
                is_error: false,
            },
        });
        cap.observe(&AppEvent::LlmFinish { usage: None });

        let t = Normalizer::normalize(&cap, true);
        let s = serde_json::to_string(&t).unwrap();
        assert!(
            !s.contains(workspace),
            "workspace root {workspace:?} must be stripped from the normalized transcript: {s}"
        );
    }

    #[test]
    fn normalize_does_not_emit_call_id_uuid() {
        let mut cap = captured_with_workspace("/tmp/eval-x");
        cap.observe(&AppEvent::LlmToolCall {
            call_id: "deadbeef-cafe-1234-5678-abcdef012345".into(),
            tool_name: ToolName::Read,
            arguments: json!({}),
        });
        cap.observe(&AppEvent::LlmFinish { usage: None });

        let t = Normalizer::normalize(&cap, true);
        let s = serde_json::to_string(&t).unwrap();
        assert!(
            !s.contains("deadbeef"),
            "call_id UUID leaked into transcript: {s}"
        );
    }

    #[test]
    fn normalize_rounds_duration_to_whole_milliseconds() {
        let mut cap = captured_with_workspace("/tmp/eval-x");
        cap.duration = Duration::from_micros(1_234_567); // 1234.567 ms
        let t = Normalizer::normalize(&cap, true);
        assert_eq!(
            t.usage_summary.duration_ms, 1234,
            "should truncate sub-ms jitter"
        );
    }

    #[test]
    fn normalize_carries_token_counts() {
        let mut cap = captured_with_workspace("/tmp/eval-x");
        cap.usage = Some(StreamUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
        });
        let t = Normalizer::normalize(&cap, true);
        assert_eq!(t.usage_summary.prompt_tokens, 100);
        assert_eq!(t.usage_summary.completion_tokens, 50);
        assert_eq!(t.usage_summary.total_tokens, 150);
    }

    #[test]
    fn normalize_passes_through_floor_verdict() {
        let cap = captured_with_workspace("/tmp/eval-x");
        let passed = Normalizer::normalize(&cap, true);
        assert!(passed.deterministic_floor_passed);
        let failed = Normalizer::normalize(&cap, false);
        assert!(!failed.deterministic_floor_passed);
    }
}
