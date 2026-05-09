# Eval Phase 6 — Data Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the data layer for the paired-comparison eval pivot — new schema types, a `Normalizer`, multi-run runner support, on-disk baseline storage with TOML manifest, and two new `steve eval` subcommands (`run`, `baseline freeze`). No judging, no reporting; those land in Phases 7 and 8.

**Architecture:** Extends the existing `src/eval/` module with four new submodules. `score.rs` holds the paired-comparison scalar types. `transcript.rs` holds `NormalizedTranscript` plus the `Normalizer` helper that converts a `CapturedRun` into a diff-stable transcript. `results.rs` holds `ResultsFile` + `ScenarioResults` and YAML round-trip helpers. `baseline.rs` holds `BaselineFile`, the TOML manifest reader/writer, and path resolution at `<dir>/<scenario>/<provider>/<model>.yaml`. The runner's `runs > 1` bail is removed; multi-run only fires through the new `eval run` subcommand. The existing `steve eval <scenario.toml> --model X` path (Phase 5's single-shot pretty-JSON dump) keeps working unchanged — it's the dev loop the team uses today, and the spec's ships-when criterion #4 requires it to survive Phase 6 untouched. **It is transitional, not legacy** — Phase 8 explicitly retires it. The whole eval module is unshipped, so there is no external back-compat obligation; this preservation is purely so Phase 6 doesn't break the in-flight feedback loop on `feat/eval-harness`.

**Schema invariant (load-bearing — re-read the spec if tempted to deviate):** `ResultsFile` and `BaselineFile` are distinct top-level shapes; they share `NormalizedTranscript` (the per-transcript schema the judge will consume in Phase 7). `user_turns` lives **one level up** from the transcript — on `ScenarioResults` (inside `ResultsFile`) and on `BaselineFile` itself — because user_turns are scenario-level data, identical across the K transcripts of a multi-run scenario. They are **NOT** stored inside `NormalizedTranscript`.

**Tech Stack:** Rust 2024, `serde-saphyr` 0.0.26 for YAML, existing `toml` 0.9 for the manifest, existing `chrono` for ISO 8601 timestamps, existing `tempfile` for tests, `std::fs::read_dir` for scenario discovery.

**Spec reference:** `docs/superpowers/specs/2026-05-06-eval-harness-paired-comparison-pivot.md` — particularly the "Schema", "File format", "Baselines as files in git", "Per-model is non-negotiable", and "Phase 6 — Data Foundation" sections.

**Ships-when (from spec, copied verbatim for sign-off):**

- A user can `steve eval baseline freeze --scenario _smoke --model X` and inspect the YAML by hand.
- A user can `steve eval run --scenario _smoke --model X` and get a multi-run results.yaml.
- All Phase-5 scenarios baseline successfully against a configured default model.
- No reporting yet; `steve eval` (no subcommand) preserves the Phase-5 single-run pretty-JSON output untouched.

---

## File Structure

| File | Status | Responsibility |
|------|--------|----------------|
| `Cargo.toml` | modify | Add `serde-saphyr = "0.0.26"`. |
| `src/eval/mod.rs` | modify | Declare new submodules; re-export new public types alongside existing ones. |
| `src/eval/score.rs` | create | `Axis`, `Verdict`, `PairedScore`, `CompareVerdict`, `ScenarioScore`. Self-contained; no dependencies on other new modules. |
| `src/eval/transcript.rs` | create | `TranscriptEvent`, `UsageSummary`, `NormalizedTranscript`, `Normalizer`. Depends on `capture::CapturedRun`. |
| `src/eval/results.rs` | create | `ScenarioResults`, `ResultsFile`, YAML read/write helpers. Depends on `transcript`. |
| `src/eval/baseline.rs` | create | `BaselineFile`, `Manifest`, `ManifestEntry`, path-resolution + YAML/TOML I/O. Depends on `transcript`. |
| `src/eval/scenario.rs` | modify | Bump `default_runs` from 1 to 3 (spec: "Default 3, per-scenario override allowed"). Add `discover_scenarios` helper. |
| `src/eval/runner.rs` | modify | Remove `runs > 1` bail; add `Runner::run_n` for symmetry. |
| `src/eval/cli.rs` | modify | Add `run_subcommand` and `freeze_subcommand` entry points. |
| `src/main.rs` | modify | Restructure `Commands::Eval` to accept optional sub-subcommand (`Run`, `Baseline { Freeze }`) while preserving the existing Phase-5 positional-scenario path (transitional; Phase 8 retires it). |

**No new file pulls in code from another new file before that file's task lands** — the dependency chain is `score.rs` (independent) → `transcript.rs` (independent) → `results.rs` (depends on `transcript`) → `baseline.rs` (depends on `transcript`) → `runner.rs` changes (independent) → `cli.rs` + `main.rs` (depends on the rest).

---

## Task 1: Add `serde-saphyr` dependency

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add the dependency**

Edit `Cargo.toml` — under `[dependencies]`, alphabetically near `serde`:

```toml
serde-saphyr = "0.0.26"
```

- [ ] **Step 2: Verify the dependency resolves**

Run: `cargo check`
Expected: clean build (no compile errors).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore(eval): add serde-saphyr 0.0.26 for YAML serialization

Phase 6 of the eval harness pivot stores baselines and run results
as plain YAML in git for diff-friendly history. Per spec, serde-saphyr
is the chosen serializer (active maintenance, panic-free parsing).
The choice is reversible — every type that hits disk derives Serde."
```

---

## Task 2: Score primitives — `Axis`, `Verdict`, `PairedScore`, `ScenarioScore`

These are self-contained scalars used by the Phase 7 judge but added now per the issue scope ("`Axis` enum is added in this phase but is NOT yet wired into `scenario.toml`'s `[scoring]` block — that parser lands in Phase 7"). No `serde-saphyr` interaction yet.

**Files:**
- Create: `src/eval/score.rs`
- Modify: `src/eval/mod.rs`

- [ ] **Step 1: Write the failing test (in the new file)**

Create `src/eval/score.rs` with:

```rust
//! Paired-comparison score primitives.
//!
//! `Axis` enumerates the dimensions a judge can score on. `Verdict` is the
//! per-axis paired-comparison outcome. `PairedScore` bundles a verdict with
//! its rationale. `ScenarioScore` is the per-(scenario, run) grading record.
//!
//! These types are added in Phase 6 so downstream schema (NormalizedTranscript,
//! ResultsFile, BaselineFile) can compile against a stable shape. The
//! `[scoring].axes` parser in `scenario.toml` lands in Phase 7 where the
//! judge actually consumes the chosen axes.

use serde::{Deserialize, Serialize};

/// Dimensions a judge can paired-compare two transcripts on.
///
/// Closed enum (no Custom(String) variant): a typo in the future
/// `[scoring]` block of `scenario.toml` should fail at load time, not
/// silently produce an unknown-axis judge prompt. New axes are added by
/// adding a variant when there's a concrete use case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Correctness,
    Efficiency,
    Conciseness,
    Robustness,
    Truthfulness,
}

/// Paired-comparison outcome on a single axis. `Tie` is first-class — the
/// judge prompt explicitly invites it to mitigate halo-effect bias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    CurrentWins,
    BaselineWins,
    Tie,
}

/// One axis's slice of a `Judge::compare` response. `rationale` precedes
/// `verdict` deliberately: the judge prompt requires per-axis reasoning to
/// be emitted before the winner is named, so the LLM commits to the
/// reasoning before anchoring on a verdict (see "Halo-effect mitigation
/// in the prompt" in the spec).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedScore {
    pub axis: Axis,
    pub rationale: String,
    pub verdict: Verdict,
}

/// One `Judge::compare` invocation returns one `Verdict` per axis the
/// judge was asked to score on, in axis order. Type alias rather than a
/// wrapper struct because every contextual field a caller would want
/// (scenario, model, run_index) is already known at the call site —
/// the verdict alone is what comes back from the LLM.
pub type CompareVerdict = Vec<PairedScore>;

/// Per-(scenario, run) grading record. `deterministic_floor_passed` is
/// copied from the existing rule-based assertion channel; failing the
/// floor short-circuits paired-comparison grading at report time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioScore {
    pub scenario: String,
    pub model: String,
    pub run_index: usize,
    pub deterministic_floor_passed: bool,
    pub axes: Vec<PairedScore>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_round_trips_via_json() {
        for axis in [
            Axis::Correctness,
            Axis::Efficiency,
            Axis::Conciseness,
            Axis::Robustness,
            Axis::Truthfulness,
        ] {
            let s = serde_json::to_string(&axis).unwrap();
            let back: Axis = serde_json::from_str(&s).unwrap();
            assert_eq!(axis, back);
        }
    }

    #[test]
    fn axis_serializes_as_snake_case() {
        assert_eq!(serde_json::to_string(&Axis::Correctness).unwrap(), "\"correctness\"");
        assert_eq!(serde_json::to_string(&Axis::Robustness).unwrap(), "\"robustness\"");
    }

    #[test]
    fn verdict_round_trips_via_json() {
        for v in [Verdict::CurrentWins, Verdict::BaselineWins, Verdict::Tie] {
            let s = serde_json::to_string(&v).unwrap();
            let back: Verdict = serde_json::from_str(&s).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn verdict_serializes_as_snake_case() {
        assert_eq!(serde_json::to_string(&Verdict::CurrentWins).unwrap(), "\"current_wins\"");
        assert_eq!(serde_json::to_string(&Verdict::BaselineWins).unwrap(), "\"baseline_wins\"");
        assert_eq!(serde_json::to_string(&Verdict::Tie).unwrap(), "\"tie\"");
    }

    #[test]
    fn paired_score_round_trips() {
        let score = PairedScore {
            axis: Axis::Efficiency,
            rationale: "current used 2 fewer tool calls".to_string(),
            verdict: Verdict::CurrentWins,
        };
        let s = serde_json::to_string(&score).unwrap();
        let back: PairedScore = serde_json::from_str(&s).unwrap();
        assert_eq!(score, back);
    }

    #[test]
    fn paired_score_field_order_is_rationale_before_verdict() {
        // Halo-mitigation invariant: rationale must be emitted before
        // verdict in the serialized form, because the judge prompt
        // explicitly asks the LLM to write rationale first. If a future
        // refactor reorders the struct fields, this catches it.
        let score = PairedScore {
            axis: Axis::Correctness,
            rationale: "R".into(),
            verdict: Verdict::Tie,
        };
        let s = serde_json::to_string(&score).unwrap();
        let r_pos = s.find("rationale").unwrap();
        let v_pos = s.find("verdict").unwrap();
        assert!(r_pos < v_pos, "rationale must serialize before verdict, got: {s}");
    }

    #[test]
    fn scenario_score_round_trips() {
        let score = ScenarioScore {
            scenario: "no-hallucinated-tool-output".into(),
            model: "ollama/qwen3-coder".into(),
            run_index: 2,
            deterministic_floor_passed: true,
            axes: vec![PairedScore {
                axis: Axis::Truthfulness,
                rationale: "current did not hallucinate".into(),
                verdict: Verdict::CurrentWins,
            }],
        };
        let s = serde_json::to_string(&score).unwrap();
        let back: ScenarioScore = serde_json::from_str(&s).unwrap();
        assert_eq!(score, back);
    }
}
```

- [ ] **Step 2: Wire the module into `mod.rs`**

In `src/eval/mod.rs`, add `pub mod score;` alphabetically near other `pub mod` declarations, and re-export the public types:

```rust
pub mod capture;
pub mod cli;
pub mod expectations;
pub mod judge;
pub mod runner;
pub mod scenario;
pub mod score;
pub mod workspace;

pub use capture::{CapturedRun, RecordedToolCall};
pub use expectations::{EvalReport, ExpectationResult, JudgeRecord, Outcome, evaluate};
pub use judge::{Judge, JudgeOutcome, JudgeVerdict, apply_judges};
pub use runner::Runner;
pub use scenario::{Expectation, Scenario, Setup};
pub use score::{Axis, CompareVerdict, PairedScore, ScenarioScore, Verdict};
pub use workspace::{ScenarioWorkspace, WorkspaceSnapshot};
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test eval::score::`
Expected: 6 tests pass.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/eval/score.rs src/eval/mod.rs
git commit -m "feat(eval): add paired-comparison score primitives

Adds Axis, Verdict, PairedScore, CompareVerdict, ScenarioScore. These
are the scalar types the Phase 7 judge will consume and the Phase 8
report will aggregate over. PairedScore field order (rationale before
verdict) is load-bearing for halo-effect mitigation in the prompt
design — pinned by a unit test.

The Axis enum is closed (no Custom(String)) so a typo in the
future scenario.toml [scoring] block fails at load time rather than
silently producing an unknown-axis judge prompt.

Wired into the prelude via src/eval/mod.rs re-exports.
Phase 6 (steve-tk30) of the paired-comparison pivot."
```

---

## Task 3: Transcript primitives — `TranscriptEvent`, `UsageSummary`, `NormalizedTranscript`

`NormalizedTranscript` is the per-transcript schema both `ResultsFile` and `BaselineFile` embed. This task adds the types only; the `Normalizer` impl that converts a `CapturedRun` into one is the next task.

**Files:**
- Create: `src/eval/transcript.rs`
- Modify: `src/eval/mod.rs`

- [ ] **Step 1: Write the file with types and tests**

Create `src/eval/transcript.rs`:

```rust
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
```

- [ ] **Step 2: Wire the module into `mod.rs`**

Add `pub mod transcript;` alphabetically and extend the re-exports:

```rust
pub use transcript::{NormalizedTranscript, TranscriptEvent, UsageSummary};
```

(Add `Normalizer` to this re-export in Task 4.)

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test eval::transcript::`
Expected: 6 tests pass.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/eval/transcript.rs src/eval/mod.rs
git commit -m "feat(eval): add NormalizedTranscript schema

Adds TranscriptEvent (tool_call | tool_result | assistant_message
tagged enum), UsageSummary (rounded usage stats), and
NormalizedTranscript (the per-transcript shape both ResultsFile and
BaselineFile embed). Per the spec, NormalizedTranscript captures
only what the agent did — user_turns and provenance live one level
up. Pinning that shape in a separate module so the next task
(Normalizer) and Phase 7's judge can both depend on it without
churn.

Phase 6 (steve-tk30) of the paired-comparison pivot."
```

---

## Task 4: `Normalizer::normalize` — `CapturedRun` to `NormalizedTranscript`

**Files:**
- Modify: `src/eval/transcript.rs` (add `Normalizer` impl + tests)
- Modify: `src/eval/mod.rs` (re-export `Normalizer`)

- [ ] **Step 1: Write failing tests for Normalizer**

Append to `src/eval/transcript.rs`'s test module (just before the closing `}` of `mod tests`):

```rust
    use crate::eval::capture::CapturedRun;
    use crate::eval::workspace::WorkspaceSnapshot;
    use crate::event::StreamUsage;
    use crate::tool::ToolOutput;
    use crate::event::AppEvent;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::Duration;

    fn captured_with_workspace(root: &str) -> CapturedRun {
        CapturedRun::new(
            PathBuf::from(root),
            WorkspaceSnapshot { files: BTreeMap::new() },
        )
    }

    #[test]
    fn normalize_interleaves_tool_call_result_then_assistant_message_per_turn() {
        let mut cap = captured_with_workspace("/tmp/eval-x");
        cap.observe(&AppEvent::LlmDelta { text: "Looking. ".into() });
        cap.observe(&AppEvent::LlmToolCall {
            call_id: "uuid-1".into(),
            tool_name: ToolName::Read,
            arguments: json!({"path": "foo.txt"}),
        });
        cap.observe(&AppEvent::ToolResult {
            call_id: "uuid-1".into(),
            tool_name: ToolName::Read,
            output: ToolOutput { title: "read".into(), output: "hello".into(), is_error: false },
        });
        cap.observe(&AppEvent::LlmDelta { text: "Done.".into() });
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
        cap.observe(&AppEvent::LlmDelta { text: "first".into() });
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
            output: ToolOutput { title: "grep".into(), output: "match".into(), is_error: false },
        });
        cap.observe(&AppEvent::LlmDelta { text: "second".into() });
        cap.observe(&AppEvent::LlmFinish { usage: None });

        let t = Normalizer::normalize(&cap, true);
        let kinds: Vec<&'static str> = t.events.iter().map(|e| match e {
            TranscriptEvent::ToolCall { .. } => "call",
            TranscriptEvent::ToolResult { .. } => "result",
            TranscriptEvent::AssistantMessage { .. } => "msg",
        }).collect();
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
            output: ToolOutput { title: "x".into(), output: "y".into(), is_error: false },
        });
        cap.observe(&AppEvent::LlmFinish { usage: None });

        let t = Normalizer::normalize(&cap, true);
        let has_empty_msg = t.events.iter().any(|e| matches!(
            e,
            TranscriptEvent::AssistantMessage { text } if text.is_empty()
        ));
        assert!(!has_empty_msg, "empty assistant_message events must be dropped: {:?}", t.events);
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
        let calls = t.events.iter().filter(|e| matches!(e, TranscriptEvent::ToolCall { .. })).count();
        let results = t.events.iter().filter(|e| matches!(e, TranscriptEvent::ToolResult { .. })).count();
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
        assert!(!s.contains("deadbeef"), "call_id UUID leaked into transcript: {s}");
    }

    #[test]
    fn normalize_rounds_duration_to_whole_milliseconds() {
        let mut cap = captured_with_workspace("/tmp/eval-x");
        cap.duration = Duration::from_micros(1_234_567); // 1234.567 ms
        let t = Normalizer::normalize(&cap, true);
        assert_eq!(t.usage_summary.duration_ms, 1234, "should truncate sub-ms jitter");
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test eval::transcript::`
Expected: compile error — `Normalizer` is not defined yet.

- [ ] **Step 3: Implement `Normalizer`**

Add this above the `#[cfg(test)] mod tests` block in `src/eval/transcript.rs`:

```rust
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
        let mut events = Vec::with_capacity(
            captured.tool_calls.len() * 2 + captured.assistant_messages.len(),
        );

        // Walk turns in order. For each turn t: emit (ToolCall, ToolResult)
        // pairs for every recorded call whose turn_index == t (in their
        // original emit order — `captured.tool_calls` is already in emit
        // order), then emit the AssistantMessage for that turn (skipping
        // empties — see test rationale).
        for (turn_idx, msg) in captured.assistant_messages.iter().enumerate() {
            for call in captured.tool_calls.iter().filter(|c| c.turn_index == turn_idx) {
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
            prompt_tokens: captured.usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
            completion_tokens: captured.usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0),
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
        Value::Array(items) => Value::Array(items.iter().map(|x| strip_workspace(x, workspace)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, vv)| (k.clone(), strip_workspace(vv, workspace)))
                .collect(),
        ),
        Value::Null | Value::Bool(_) | Value::Number(_) => v.clone(),
    }
}
```

- [ ] **Step 4: Add the re-export to `src/eval/mod.rs`**

Update the existing `pub use transcript::{...}` line to:

```rust
pub use transcript::{NormalizedTranscript, Normalizer, TranscriptEvent, UsageSummary};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test eval::transcript::`
Expected: all 9+ tests pass.

- [ ] **Step 6: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: clean. (Sub-millisecond truncation cast `as u64` may need `#[allow(clippy::cast_possible_truncation)]` — only add if the lint actually fires, with a `// reason: durations >2^63 ms = 292M years; not a real concern` comment.)

- [ ] **Step 7: Commit**

```bash
git add src/eval/transcript.rs src/eval/mod.rs
git commit -m "feat(eval): add Normalizer (CapturedRun -> NormalizedTranscript)

Pure transformation: strips workspace tempdir paths from tool
arguments and outputs, drops tool-call UUIDs (call_id), drops
empty assistant_messages from tool-only turns (kept in CapturedRun
for per-turn correspondence; not useful for the judge), rounds
duration to whole milliseconds. Idempotent and I/O-free so it can
be used at both freeze time (BaselineFile) and report time (judge
input).

Phase 6 (steve-tk30) of the paired-comparison pivot."
```

---

## Task 5: `ScenarioResults` and `ResultsFile` — top-level shape for `eval run` output

This task adds the types only. YAML I/O helpers come in Task 6 (separate task because YAML round-trip needs careful test cases for block-scalar preservation of multi-line content).

**Files:**
- Create: `src/eval/results.rs`
- Modify: `src/eval/mod.rs`

- [ ] **Step 1: Write the file with types and round-trip tests**

Create `src/eval/results.rs`:

```rust
//! Top-level shape of `results.yaml` — output of `steve eval run`.
//!
//! Multi-scenario container. Each scenario carries its inputs (user_turns)
//! and the K transcripts produced by running the scenario K times.
//! Distinct from `BaselineFile` (single-scenario, single-transcript,
//! sharded on disk) but they share `NormalizedTranscript`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::eval::transcript::NormalizedTranscript;

/// Per-scenario container inside a `ResultsFile`. `user_turns` lives here
/// (NOT on each transcript) because they're scenario-level data, identical
/// across the K transcripts produced for a multi-run scenario. Storing
/// them once is the right cardinality.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioResults {
    pub user_turns: Vec<String>,
    pub runs: Vec<NormalizedTranscript>,
}

/// Top-level shape of `results.yaml`. Multi-scenario; each scenario carries
/// its inputs + K transcripts. `BTreeMap` (not `HashMap`) for stable
/// scenario ordering on serialize — per-transcript content still varies
/// with wall-clock-affected fields the Normalizer doesn't strip (token
/// counts), so byte-identical files require both stable ordering AND
/// identical sampling. Stable ordering is the guarantee here, not
/// byte-identical output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultsFile {
    pub git_ref: String,
    pub recorded_at: String, // ISO 8601 UTC
    pub model: String,       // "provider/model"
    pub scenarios: BTreeMap<String, ScenarioResults>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::transcript::{TranscriptEvent, UsageSummary};
    use crate::tool::ToolName;
    use serde_json::json;

    fn sample_transcript() -> NormalizedTranscript {
        NormalizedTranscript {
            events: vec![
                TranscriptEvent::ToolCall {
                    tool_name: ToolName::Read,
                    arguments: json!({"path": "foo.txt"}),
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
        }
    }

    #[test]
    fn scenario_results_round_trips_via_json() {
        let sr = ScenarioResults {
            user_turns: vec!["First prompt".into(), "Second prompt".into()],
            runs: vec![sample_transcript(), sample_transcript()],
        };
        let s = serde_json::to_string(&sr).unwrap();
        let back: ScenarioResults = serde_json::from_str(&s).unwrap();
        assert_eq!(sr, back);
    }

    #[test]
    fn results_file_round_trips_via_json() {
        let mut scenarios = BTreeMap::new();
        scenarios.insert(
            "_smoke".to_string(),
            ScenarioResults {
                user_turns: vec!["Read the file.".into()],
                runs: vec![sample_transcript()],
            },
        );
        let rf = ResultsFile {
            git_ref: "abc1234".into(),
            recorded_at: "2026-05-07T12:00:00Z".into(),
            model: "ollama/qwen3-coder".into(),
            scenarios,
        };
        let s = serde_json::to_string(&rf).unwrap();
        let back: ResultsFile = serde_json::from_str(&s).unwrap();
        assert_eq!(rf, back);
    }

    #[test]
    fn results_file_serializes_scenarios_in_btreemap_order() {
        let mut scenarios = BTreeMap::new();
        // Insert in non-alphabetical order — BTreeMap should still serialize sorted.
        scenarios.insert("zoo".to_string(), ScenarioResults { user_turns: vec![], runs: vec![] });
        scenarios.insert("alpha".to_string(), ScenarioResults { user_turns: vec![], runs: vec![] });
        scenarios.insert("middle".to_string(), ScenarioResults { user_turns: vec![], runs: vec![] });
        let rf = ResultsFile {
            git_ref: "x".into(),
            recorded_at: "y".into(),
            model: "z".into(),
            scenarios,
        };
        let s = serde_json::to_string(&rf).unwrap();
        let alpha = s.find("alpha").unwrap();
        let middle = s.find("middle").unwrap();
        let zoo = s.find("zoo").unwrap();
        assert!(alpha < middle && middle < zoo, "scenarios must be alphabetical, got: {s}");
    }

    #[test]
    fn results_file_rejects_unknown_top_level_fields() {
        // deny_unknown_fields on the manifest format — typos in a hand-edited
        // results.yaml become hard errors instead of silent default values.
        let bad = r#"{"git_ref":"x","recorded_at":"y","model":"z","scenarios":{},"unknown":"oops"}"#;
        let r: Result<ResultsFile, _> = serde_json::from_str(bad);
        assert!(r.is_err(), "unknown field 'unknown' must be rejected");
    }
}
```

- [ ] **Step 2: Wire the module**

In `src/eval/mod.rs`:

```rust
pub mod results;
// ...
pub use results::{ResultsFile, ScenarioResults};
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test eval::results::`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/eval/results.rs src/eval/mod.rs
git commit -m "feat(eval): add ScenarioResults and ResultsFile types

Top-level shape of results.yaml. Multi-scenario; each scenario
carries user_turns once + K NormalizedTranscripts. BTreeMap for
stable serialization order. deny_unknown_fields on both shapes so
hand-edited typos in results.yaml fail loud instead of silently
defaulting.

YAML I/O helpers land in the next task; this one pins the type
contract via serde_json round-trips.

Phase 6 (steve-tk30) of the paired-comparison pivot."
```

---

## Task 6: YAML I/O helpers for `ResultsFile`

This task wires `serde-saphyr` and verifies multi-line block-scalar preservation (the load-bearing reason we picked YAML over JSON in the spec).

**Files:**
- Modify: `src/eval/results.rs` (add `read_from_path`, `write_to_path`, `to_yaml_string`, `from_yaml_str` + tests)

- [ ] **Step 1: Write failing tests for YAML I/O**

Append to `src/eval/results.rs`'s test module:

```rust
    use std::fs;

    #[test]
    fn results_file_round_trips_via_yaml() {
        let mut scenarios = BTreeMap::new();
        scenarios.insert(
            "_smoke".to_string(),
            ScenarioResults {
                user_turns: vec!["Read the file.".into()],
                runs: vec![sample_transcript()],
            },
        );
        let rf = ResultsFile {
            git_ref: "abc1234".into(),
            recorded_at: "2026-05-07T12:00:00Z".into(),
            model: "ollama/qwen3-coder".into(),
            scenarios,
        };

        let yaml = rf.to_yaml_string().expect("serialize");
        let back = ResultsFile::from_yaml_str(&yaml).expect("deserialize");
        assert_eq!(rf, back);
    }

    #[test]
    fn results_file_yaml_preserves_multiline_strings_as_block_scalars() {
        // Load-bearing: the whole reason we picked YAML over JSON is so
        // that multi-line content (assistant text, tool-call diffs) renders
        // as readable block scalars instead of \n-escaped one-liners. If
        // serde-saphyr's emitter ever flips to flow style for multi-line
        // strings, this test catches it.
        let mut scenarios = BTreeMap::new();
        scenarios.insert(
            "_smoke".into(),
            ScenarioResults {
                user_turns: vec!["line one\nline two\nline three".into()],
                runs: vec![],
            },
        );
        let rf = ResultsFile {
            git_ref: "x".into(),
            recorded_at: "y".into(),
            model: "z".into(),
            scenarios,
        };

        let yaml = rf.to_yaml_string().expect("serialize");
        // The raw \n-escape form would look like "line one\nline two";
        // a block scalar (| or >) renders the lines literal. Either form
        // round-trips correctly, but for diff-friendliness we want the
        // literal lines visible. A sanity gate: if the emitted YAML still
        // contains the escaped form, something is wrong.
        assert!(
            !yaml.contains("\\n"),
            "multi-line strings must NOT round-trip as escaped \\n (lost diff-friendliness): {yaml}"
        );

        // Round-trip equality regardless of form.
        let back = ResultsFile::from_yaml_str(&yaml).expect("deserialize");
        assert_eq!(rf, back);
    }

    #[test]
    fn results_file_write_then_read_via_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("results.yaml");

        let rf = ResultsFile {
            git_ref: "x".into(),
            recorded_at: "2026-05-07T00:00:00Z".into(),
            model: "ollama/qwen3-coder".into(),
            scenarios: BTreeMap::new(),
        };
        rf.write_to_path(&path).expect("write");
        assert!(path.exists());

        let back = ResultsFile::read_from_path(&path).expect("read");
        assert_eq!(rf, back);

        // The file is plain text — `cat`-able and grep-friendly.
        let raw = fs::read_to_string(&path).expect("read raw");
        assert!(raw.contains("git_ref"), "expected human-readable YAML: {raw}");
        assert!(raw.contains("ollama/qwen3-coder"), "model identity should round-trip: {raw}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test eval::results::`
Expected: compile error — `to_yaml_string`, `from_yaml_str`, `write_to_path`, `read_from_path` not defined.

- [ ] **Step 3: Implement the YAML helpers**

Add to `src/eval/results.rs`, just below the type definitions:

```rust
use std::path::Path;

use anyhow::{Context, Result};

impl ResultsFile {
    /// Serialize to a pretty-printed YAML string. Multi-line content
    /// (user_turns, assistant_messages, tool outputs) is emitted as
    /// block scalars by the underlying emitter — that's the diff-
    /// friendliness property we picked YAML for.
    pub fn to_yaml_string(&self) -> Result<String> {
        serde_saphyr::to_string(self).context("serializing ResultsFile to YAML")
    }

    pub fn from_yaml_str(s: &str) -> Result<Self> {
        serde_saphyr::from_str(s).context("parsing ResultsFile from YAML")
    }

    pub fn write_to_path(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating parent dir for {}", path.display()))?;
        }
        let yaml = self.to_yaml_string()?;
        std::fs::write(path, yaml)
            .with_context(|| format!("writing results YAML to {}", path.display()))
    }

    pub fn read_from_path(path: &Path) -> Result<Self> {
        let yaml = std::fs::read_to_string(path)
            .with_context(|| format!("reading results YAML from {}", path.display()))?;
        Self::from_yaml_str(&yaml)
    }
}
```

(Note: the exact `serde_saphyr` function names — `to_string`, `from_str` — match the crate's documented API as of 0.0.26. If the API differs, adjust to the actual function names; the rest of the impl is shape-stable.)

- [ ] **Step 4: Run tests**

Run: `cargo test eval::results::`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/eval/results.rs
git commit -m "feat(eval): YAML read/write for ResultsFile via serde-saphyr

Adds to_yaml_string / from_yaml_str / read_from_path / write_to_path.
write_to_path creates parent dirs as needed (no manual mkdir at the
caller). A regression test pins the block-scalar property — the
whole point of choosing YAML over JSON is that multi-line tool
results and assistant messages diff readably.

Phase 6 (steve-tk30) of the paired-comparison pivot."
```

---

## Task 7: `BaselineFile` — type, YAML I/O, and path resolution

**Files:**
- Create: `src/eval/baseline.rs`
- Modify: `src/eval/mod.rs`

- [ ] **Step 1: Create the file with type, helpers, and tests**

Create `src/eval/baseline.rs`:

```rust
//! Baseline storage at `eval/baselines/<scenario>/<provider>/<model>.yaml`
//! with `eval/baselines/manifest.toml` as the authoritative provenance index.
//!
//! Why split the path on the slash in `provider/model` rather than encoding
//! it in a single filename: the codebase already uses `provider/model_id`
//! everywhere (see CLAUDE.md), and the filesystem hierarchy mirrors that
//! convention naturally. Listing all baselines for a model is
//! `find eval/baselines -path '*/ollama/qwen3-coder.yaml'`. No encoding
//! gymnastics needed.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::eval::transcript::NormalizedTranscript;

/// Top-level shape of an individual baseline file at
/// `eval/baselines/<scenario>/<provider>/<model>.yaml`. Single scenario,
/// single transcript, plus the scenario's `user_turns` for self-describing
/// readability — a baseline file is independently interpretable without
/// cross-referencing scenario.toml at the right git ref.
///
/// Provenance fields here describe the file in-place; the same fields
/// (with matching names) are mirrored into the manifest. Read the manifest
/// for cross-baseline indexing; read the file for the transcript. If the
/// two ever disagree, the manifest wins (`freeze` writes them together;
/// a manifest-only edit is the supported fix).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineFile {
    pub scenario: String,
    pub model: String,
    pub git_ref: String,
    pub frozen_at: String, // ISO 8601 UTC
    pub user_turns: Vec<String>,
    pub transcript: NormalizedTranscript,
}

impl BaselineFile {
    pub fn to_yaml_string(&self) -> Result<String> {
        serde_saphyr::to_string(self).context("serializing BaselineFile to YAML")
    }

    pub fn from_yaml_str(s: &str) -> Result<Self> {
        serde_saphyr::from_str(s).context("parsing BaselineFile from YAML")
    }

    pub fn write_to_path(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating parent dir for {}", path.display()))?;
        }
        std::fs::write(path, self.to_yaml_string()?)
            .with_context(|| format!("writing baseline YAML to {}", path.display()))
    }

    pub fn read_from_path(path: &Path) -> Result<Self> {
        let yaml = std::fs::read_to_string(path)
            .with_context(|| format!("reading baseline YAML from {}", path.display()))?;
        Self::from_yaml_str(&yaml)
    }
}

/// Resolve the baseline file path for `(scenario, model)` rooted at
/// `baselines_dir`. The model id MUST be in `provider/model_id` form;
/// the slash is what makes the directory hierarchy mirror the model
/// naming convention. Returns
/// `baselines_dir/scenario/provider/model_id.yaml`.
pub fn baseline_path(baselines_dir: &Path, scenario: &str, model: &str) -> Result<PathBuf> {
    let (provider, model_id) = model.split_once('/').with_context(|| {
        format!(
            "model {model:?} must be in 'provider/model_id' form (matches the project-wide convention; see CLAUDE.md)"
        )
    })?;
    if provider.is_empty() || model_id.is_empty() {
        anyhow::bail!("model {model:?} has empty provider or model id");
    }
    Ok(baselines_dir
        .join(scenario)
        .join(provider)
        .join(format!("{model_id}.yaml")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::transcript::{TranscriptEvent, UsageSummary};
    use crate::tool::ToolName;
    use serde_json::json;

    fn sample_baseline() -> BaselineFile {
        BaselineFile {
            scenario: "_smoke".into(),
            model: "ollama/qwen3-coder".into(),
            git_ref: "abc1234".into(),
            frozen_at: "2026-05-07T00:00:00Z".into(),
            user_turns: vec!["Read the file.".into()],
            transcript: NormalizedTranscript {
                events: vec![TranscriptEvent::AssistantMessage {
                    text: "It says hello.".into(),
                }],
                deterministic_floor_passed: true,
                usage_summary: UsageSummary {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                    duration_ms: 0,
                },
            },
        }
    }

    #[test]
    fn baseline_file_round_trips_via_yaml() {
        let bf = sample_baseline();
        let yaml = bf.to_yaml_string().unwrap();
        let back = BaselineFile::from_yaml_str(&yaml).unwrap();
        assert_eq!(bf, back);
    }

    #[test]
    fn baseline_file_write_then_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deep/baseline.yaml");
        let bf = sample_baseline();
        bf.write_to_path(&path).unwrap();
        assert!(path.exists());
        let back = BaselineFile::read_from_path(&path).unwrap();
        assert_eq!(bf, back);
    }

    #[test]
    fn baseline_path_splits_provider_and_model_on_first_slash() {
        let dir = Path::new("eval/baselines");
        let p = baseline_path(dir, "_smoke", "ollama/qwen3-coder").unwrap();
        assert_eq!(p, Path::new("eval/baselines/_smoke/ollama/qwen3-coder.yaml"));
    }

    #[test]
    fn baseline_path_handles_provider_model_with_internal_dashes() {
        let dir = Path::new("eval/baselines");
        let p = baseline_path(dir, "stop-guessing-after-failures", "anthropic/claude-haiku-4-5").unwrap();
        assert_eq!(
            p,
            Path::new("eval/baselines/stop-guessing-after-failures/anthropic/claude-haiku-4-5.yaml")
        );
    }

    #[test]
    fn baseline_path_rejects_model_without_slash() {
        let err = baseline_path(Path::new("x"), "_smoke", "no-slash").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("provider/model_id"), "got: {msg}");
    }

    #[test]
    fn baseline_path_rejects_empty_provider_or_model() {
        assert!(baseline_path(Path::new("x"), "_smoke", "/qwen").is_err());
        assert!(baseline_path(Path::new("x"), "_smoke", "ollama/").is_err());
    }

    #[test]
    fn baseline_file_yaml_contains_user_turns_and_transcript() {
        let yaml = sample_baseline().to_yaml_string().unwrap();
        assert!(yaml.contains("user_turns"), "user_turns must be a top-level field: {yaml}");
        assert!(yaml.contains("transcript"), "transcript must be a top-level field: {yaml}");
    }

    #[test]
    fn baseline_file_yaml_does_not_have_user_turns_inside_transcript() {
        // Schema invariant: user_turns lives ON BaselineFile, NOT on
        // NormalizedTranscript.transcript. If a refactor ever moves it
        // into the transcript, the file shape regresses and the judge's
        // shared-NormalizedTranscript contract breaks.
        let yaml = sample_baseline().to_yaml_string().unwrap();
        let transcript_idx = yaml.find("transcript:").expect("transcript field present");
        let user_turns_idx = yaml.find("user_turns:").expect("user_turns field present");
        assert!(
            user_turns_idx < transcript_idx,
            "user_turns must serialize at the BaselineFile level (before transcript:): {yaml}"
        );
    }
}
```

- [ ] **Step 2: Wire the module**

In `src/eval/mod.rs`:

```rust
pub mod baseline;
// ...
pub use baseline::{BaselineFile, baseline_path};
```

- [ ] **Step 3: Run tests**

Run: `cargo test eval::baseline::`
Expected: 8 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/eval/baseline.rs src/eval/mod.rs
git commit -m "feat(eval): add BaselineFile + path resolution

BaselineFile is the on-disk shape for one frozen baseline. user_turns
lives at this level (NOT inside the transcript) — schema invariant
pinned by a unit test. baseline_path() splits provider/model on the
first slash, making the filesystem hierarchy mirror the codebase's
provider/model convention naturally.

YAML I/O via serde-saphyr; tempdir round-trip test confirms the
file is plain text and parent dirs are auto-created.

Phase 6 (steve-tk30) of the paired-comparison pivot."
```

---

## Task 8: Manifest TOML — `Manifest`, `ManifestEntry`, read/write/upsert

**Files:**
- Modify: `src/eval/baseline.rs` (add Manifest types and helpers + tests)

- [ ] **Step 1: Write failing tests for the manifest**

Append to `src/eval/baseline.rs`'s test module:

```rust
    #[test]
    fn manifest_round_trips_via_toml() {
        let m = Manifest {
            baseline: vec![
                ManifestEntry {
                    scenario: "_smoke".into(),
                    model: "ollama/qwen3-coder".into(),
                    git_ref: "abc1234".into(),
                    frozen_at: "2026-05-07T00:00:00Z".into(),
                },
                ManifestEntry {
                    scenario: "no-hallucinated-tool-output".into(),
                    model: "ollama/qwen3-coder".into(),
                    git_ref: "abc1234".into(),
                    frozen_at: "2026-05-07T00:00:00Z".into(),
                },
            ],
        };
        let s = m.to_toml_string().unwrap();
        let back = Manifest::from_toml_str(&s).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn manifest_empty_round_trips() {
        let m = Manifest { baseline: vec![] };
        let s = m.to_toml_string().unwrap();
        let back = Manifest::from_toml_str(&s).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn manifest_upsert_replaces_matching_scenario_and_model() {
        let mut m = Manifest {
            baseline: vec![ManifestEntry {
                scenario: "_smoke".into(),
                model: "ollama/qwen3-coder".into(),
                git_ref: "old".into(),
                frozen_at: "2026-01-01T00:00:00Z".into(),
            }],
        };
        m.upsert(ManifestEntry {
            scenario: "_smoke".into(),
            model: "ollama/qwen3-coder".into(),
            git_ref: "new".into(),
            frozen_at: "2026-05-07T00:00:00Z".into(),
        });
        assert_eq!(m.baseline.len(), 1, "upsert must not duplicate existing rows");
        assert_eq!(m.baseline[0].git_ref, "new");
        assert_eq!(m.baseline[0].frozen_at, "2026-05-07T00:00:00Z");
    }

    #[test]
    fn manifest_upsert_appends_when_no_match() {
        let mut m = Manifest {
            baseline: vec![ManifestEntry {
                scenario: "_smoke".into(),
                model: "ollama/qwen3-coder".into(),
                git_ref: "x".into(),
                frozen_at: "y".into(),
            }],
        };
        m.upsert(ManifestEntry {
            scenario: "different".into(),
            model: "ollama/qwen3-coder".into(),
            git_ref: "z".into(),
            frozen_at: "w".into(),
        });
        assert_eq!(m.baseline.len(), 2);
    }

    #[test]
    fn manifest_upsert_treats_scenario_and_model_as_composite_key() {
        // Same scenario, different model -> two entries.
        let mut m = Manifest { baseline: vec![] };
        m.upsert(ManifestEntry {
            scenario: "_smoke".into(),
            model: "ollama/qwen3-coder".into(),
            git_ref: "x".into(),
            frozen_at: "y".into(),
        });
        m.upsert(ManifestEntry {
            scenario: "_smoke".into(),
            model: "anthropic/claude-haiku-4-5".into(),
            git_ref: "x".into(),
            frozen_at: "y".into(),
        });
        assert_eq!(m.baseline.len(), 2);
    }

    #[test]
    fn manifest_write_then_read_via_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.toml");
        let m = Manifest {
            baseline: vec![ManifestEntry {
                scenario: "_smoke".into(),
                model: "ollama/qwen3-coder".into(),
                git_ref: "abc1234".into(),
                frozen_at: "2026-05-07T00:00:00Z".into(),
            }],
        };
        m.write_to_path(&path).unwrap();
        let back = Manifest::read_from_path(&path).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn manifest_read_from_missing_path_returns_empty() {
        // Bare-metal `eval baseline freeze` against a fresh checkout has no
        // manifest yet; read_from_path must not error in that case so the
        // freeze flow can do read-modify-write blindly.
        let dir = tempfile::tempdir().unwrap();
        let m = Manifest::read_from_path(&dir.path().join("does-not-exist.toml")).unwrap();
        assert!(m.baseline.is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test eval::baseline::`
Expected: compile error — `Manifest`, `ManifestEntry` not defined.

- [ ] **Step 3: Implement Manifest types and helpers**

Add to `src/eval/baseline.rs`, just below `BaselineFile`:

```rust
/// Authoritative cross-baseline provenance index. Lives at
/// `eval/baselines/manifest.toml`. The same fields are mirrored into
/// each individual `BaselineFile` for self-describing readability,
/// but the manifest is the source of truth for "which baselines
/// exist for model X, frozen when?" queries.
///
/// No `judge_model` field — freeze runs the agent only, not the judge,
/// so a baseline is a behavioral snapshot, not a graded artifact.
/// The judge model used for any specific report is recorded in that
/// report's metadata block (Phase 8).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// One entry per (scenario, model) pair. The TOML idiom is
    /// `[[baseline]]` (array of tables); `serde(rename = "baseline")` is
    /// not needed because the field name already matches the array
    /// element name.
    #[serde(default)]
    pub baseline: Vec<ManifestEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestEntry {
    pub scenario: String,
    pub model: String,
    pub git_ref: String,
    pub frozen_at: String, // ISO 8601 UTC
}

impl Manifest {
    pub fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self).context("serializing Manifest to TOML")
    }

    pub fn from_toml_str(s: &str) -> Result<Self> {
        toml::from_str(s).context("parsing Manifest from TOML")
    }

    pub fn write_to_path(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating parent dir for {}", path.display()))?;
        }
        std::fs::write(path, self.to_toml_string()?)
            .with_context(|| format!("writing manifest TOML to {}", path.display()))
    }

    /// Read the manifest from disk. Returns `Manifest::default()` (empty)
    /// if the path does not exist — the freeze flow blindly does
    /// read-modify-write, and a fresh checkout has no manifest yet.
    /// Other I/O errors (permission denied, partial read) propagate.
    pub fn read_from_path(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::from_toml_str(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(anyhow::Error::from(e).context(format!(
                "reading manifest TOML from {}",
                path.display()
            ))),
        }
    }

    /// Composite key is (scenario, model). Matching entry is replaced;
    /// non-matching entries are unchanged. New entries are appended at
    /// the end (no implicit sort — manifest order reflects freeze order,
    /// which has minor diagnostic value for "which scenario was frozen
    /// most recently").
    pub fn upsert(&mut self, entry: ManifestEntry) {
        if let Some(existing) = self
            .baseline
            .iter_mut()
            .find(|e| e.scenario == entry.scenario && e.model == entry.model)
        {
            *existing = entry;
        } else {
            self.baseline.push(entry);
        }
    }
}

/// Resolve the manifest path rooted at `baselines_dir` ->
/// `baselines_dir/manifest.toml`.
pub fn manifest_path(baselines_dir: &Path) -> PathBuf {
    baselines_dir.join("manifest.toml")
}
```

Update `src/eval/mod.rs` re-export:

```rust
pub use baseline::{BaselineFile, Manifest, ManifestEntry, baseline_path, manifest_path};
```

- [ ] **Step 4: Run tests**

Run: `cargo test eval::baseline::`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/eval/baseline.rs src/eval/mod.rs
git commit -m "feat(eval): add Manifest TOML reader/writer with upsert

Manifest is the authoritative cross-baseline index at
eval/baselines/manifest.toml. Composite key is (scenario, model);
upsert replaces matching entries in place. read_from_path returns
an empty default on NotFound so a fresh-checkout freeze can do
read-modify-write blindly; other I/O errors still propagate.

No judge_model field on the manifest per spec — freeze runs the
agent only, so a baseline is a behavioral snapshot. Judge attribution
lives on each report (Phase 8), not on the baseline.

Phase 6 (steve-tk30) of the paired-comparison pivot."
```

---

## Task 9: Bump default `runs` from 1 to 3

**Files:**
- Modify: `src/eval/scenario.rs`

- [ ] **Step 1: Update the test for the new default first**

In `src/eval/scenario.rs` test module, find any test that depends on the default of 1 (search for `default_runs` or scenarios that omit `runs` and assert `runs.get() == 1`). If such a test exists, update its expectation to 3. If none exists, write a new pinning test:

```rust
    #[test]
    fn default_runs_is_three() {
        // Per spec: "Default 3, per-scenario override allowed." Multi-run
        // becomes load-bearing in Phase 6 — the default sample count is 3.
        let toml = r#"
            name = "x"
            description = "x"
            user_turns = ["hi"]
            expectations = []
        "#;
        let s = Scenario::from_toml_str(toml).unwrap();
        assert_eq!(s.runs.get(), 3);
    }
```

- [ ] **Step 2: Update the default**

In `src/eval/scenario.rs`, find:

```rust
fn default_runs() -> NonZeroUsize {
    NonZeroUsize::new(1).expect("1 != 0")
}
```

Replace with:

```rust
fn default_runs() -> NonZeroUsize {
    // Per spec: "Default 3, per-scenario override allowed." Multi-run
    // is the new norm for the paired-comparison pivot; the existing
    // Phase-5 `steve eval <scenario.toml>` path (transitional until
    // Phase 8 retires it) forces runs = 1 internally via cli::run_one,
    // so this default only fires through the new `eval run` subcommand.
    NonZeroUsize::new(3).expect("3 != 0")
}
```

- [ ] **Step 3: Update existing scenario tests if needed**

Search the test module for assertions on `runs.get() == 1` against scenarios that omit the `runs` field. If found, change them to expect `3` and add a comment explaining this is the new default. **Do NOT** touch tests that explicitly set `runs = 1` (those still expect 1).

Run: `cargo test eval::scenario::`
Expected: any "default-runs" tests pass with the new value; no regressions.

- [ ] **Step 4: Run all eval tests**

Run: `cargo test eval::`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/eval/scenario.rs
git commit -m "feat(eval): bump default scenario.runs from 1 to 3

Per spec, multi-run becomes load-bearing in Phase 6 — a 3-sample
default reduces single-run noise in paired-comparison verdicts.

The existing Phase-5 'steve eval <scenario.toml>' path (transitional
until Phase 8 retires it) forces runs = 1 internally via cli::run_one,
so this default only fires through the new 'eval run' subcommand.

Phase 6 (steve-tk30) of the paired-comparison pivot."
```

---

## Task 10: Multi-run runner — remove `runs > 1` bail and add `Runner::run_n`

**Files:**
- Modify: `src/eval/runner.rs`

- [ ] **Step 1: Update the bail-on-`runs>1` test to verify the bail is GONE**

In `src/eval/runner.rs` test module, find `build_bails_when_runs_greater_than_one`. Replace it with:

```rust
    /// Multi-run scenarios used to bail at build time pending Phase 6 work.
    /// Phase 6 lifted the restriction; build now succeeds for runs > 1.
    #[test]
    fn build_succeeds_for_runs_greater_than_one() {
        let scenario_dir = tempfile::tempdir().unwrap();
        let scenario = scenario_with_runs(3);
        // Build will still fail downstream because no provider is configured
        // in the test env, but it must NOT fail with the "multi-run not
        // implemented" message — that's the regression gate.
        let err = match Runner::build(&scenario, scenario_dir.path(), "fake/model") {
            Ok(_) => return, // happy path: build succeeded outright (unlikely without API keys)
            Err(e) => format!("{e:#}"),
        };
        assert!(
            !err.contains("multi-run execution is not yet implemented"),
            "the runs>1 bail must be removed; got: {err}"
        );
    }
```

- [ ] **Step 2: Run tests to verify the new test fails (because the bail still exists)**

Run: `cargo test eval::runner::build_succeeds_for_runs_greater_than_one`
Expected: FAIL with the bail message in the chain.

- [ ] **Step 3: Remove the bail**

In `src/eval/runner.rs`, delete this block from `Runner::build`:

```rust
        if scenario.runs.get() > 1 {
            anyhow::bail!(
                "scenario {:?} sets runs={} but multi-run execution is not yet implemented \
                 (tracked as steve-paeu, blocks Phase 6); rerun with runs=1 or omit the field",
                scenario.name,
                scenario.runs.get()
            );
        }
```

(Plus the comment block above it that references multi-run support landing alongside Phase 6 — clean it up entirely.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test eval::runner::build_succeeds_for_runs_greater_than_one`
Expected: pass.

- [ ] **Step 5: Add `Runner::run_n` returning `Vec<CapturedRun>`**

After the existing `pub async fn run`, add:

```rust
    /// Drive the same scenario `count` times, returning one `CapturedRun`
    /// per run. Each run reuses the SAME `App` and SAME workspace tempdir
    /// — the agent's conversation history persists across runs unless the
    /// caller re-builds the Runner. For Phase 6, every multi-run scenario
    /// is independent across runs (the runner is rebuilt per run by the
    /// `eval run` subcommand), so this method exists for symmetry and as
    /// a convenience for tests that don't care about per-run isolation.
    pub async fn run_n(
        &mut self,
        scenario: &Scenario,
        count: std::num::NonZeroUsize,
    ) -> Result<Vec<CapturedRun>> {
        let mut out = Vec::with_capacity(count.get());
        for _ in 0..count.get() {
            out.push(self.run(scenario).await?);
        }
        Ok(out)
    }
```

(The `eval run` subcommand — Task 12 — actually rebuilds the Runner per run rather than calling `run_n`, because each run needs a fresh tempdir/workspace. This method is here for completeness and tests; the subcommand uses it only for in-process sanity tests, not the production loop.)

- [ ] **Step 6: Run all eval tests**

Run: `cargo test eval::`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add src/eval/runner.rs
git commit -m "feat(eval): remove runs>1 bail; add Runner::run_n

Multi-run is the new norm. Removed Runner::build's bail on
scenario.runs > 1 (the existing Phase-5 'steve eval <scenario.toml>'
path — transitional until Phase 8 retires it — forces runs = 1
internally via cli::run_one, so this doesn't change its behavior).
Added Runner::run_n for symmetry — production callers in 'eval run'
rebuild the Runner per run for fresh-workspace isolation, but
in-process tests can use run_n directly.

Phase 6 (steve-tk30) of the paired-comparison pivot."
```

---

## Task 11: Scenario discovery helper — `discover_scenarios`

The `eval baseline freeze` and `eval run` subcommands need to find every scenario when no `--scenario` filter is given. Centralize the logic so both subcommands share it.

**Files:**
- Modify: `src/eval/scenario.rs` (add `discover_scenarios` + tests)

- [ ] **Step 1: Write failing tests**

Append to `src/eval/scenario.rs`'s test module:

```rust
    #[test]
    fn discover_scenarios_returns_empty_for_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let scenarios = discover_scenarios(&dir.path().join("does-not-exist")).unwrap();
        assert!(scenarios.is_empty());
    }

    #[test]
    fn discover_scenarios_returns_empty_for_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let scenarios = discover_scenarios(dir.path()).unwrap();
        assert!(scenarios.is_empty());
    }

    #[test]
    fn discover_scenarios_finds_subdirs_with_scenario_toml() {
        let dir = tempfile::tempdir().unwrap();
        // Subdir with a scenario.toml — should be discovered.
        std::fs::create_dir_all(dir.path().join("foo")).unwrap();
        std::fs::write(dir.path().join("foo/scenario.toml"), b"# pretend manifest").unwrap();
        // Subdir without scenario.toml — should be skipped.
        std::fs::create_dir_all(dir.path().join("bar")).unwrap();
        // File at top level — should be skipped (not a scenario dir).
        std::fs::write(dir.path().join("loose.toml"), b"").unwrap();

        let scenarios = discover_scenarios(dir.path()).unwrap();
        let names: Vec<&str> = scenarios.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, vec!["foo"]);
    }

    #[test]
    fn discover_scenarios_returns_alphabetical_order() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["zebra", "alpha", "middle"] {
            std::fs::create_dir_all(dir.path().join(name)).unwrap();
            std::fs::write(dir.path().join(name).join("scenario.toml"), b"#").unwrap();
        }
        let scenarios = discover_scenarios(dir.path()).unwrap();
        let names: Vec<&str> = scenarios.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "middle", "zebra"]);
    }
```

- [ ] **Step 2: Implement `discover_scenarios`**

In `src/eval/scenario.rs`, add (placing it after `Scenario::validate`):

```rust
/// Walk `scenarios_dir` for subdirectories that contain a `scenario.toml`.
/// Returns `(name, scenario_toml_path)` pairs in alphabetical order by
/// name. Missing `scenarios_dir` is treated as "no scenarios" (empty
/// result, not an error) — this matches the freeze flow's expectation
/// of being callable on a fresh checkout.
pub fn discover_scenarios(scenarios_dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    if !scenarios_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(scenarios_dir)
        .with_context(|| format!("reading {}", scenarios_dir.display()))?
    {
        let entry = entry.with_context(|| format!("iterating {}", scenarios_dir.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest = path.join("scenario.toml");
        if !manifest.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from)
            .with_context(|| format!("scenario dir {} has non-UTF-8 name", path.display()))?;
        out.push((name, manifest));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}
```

Re-export from `src/eval/mod.rs`:

```rust
pub use scenario::{Expectation, Scenario, Setup, discover_scenarios};
```

- [ ] **Step 3: Run tests**

Run: `cargo test eval::scenario::discover_scenarios`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/eval/scenario.rs src/eval/mod.rs
git commit -m "feat(eval): add discover_scenarios helper

Walks <dir>/<name>/scenario.toml. Used by 'eval run' and
'eval baseline freeze' to honor --scenario filtering OR iterate the
full set when no filter is given. Missing directory returns an empty
result (matches the fresh-checkout freeze flow). Subdirs without
scenario.toml are skipped silently.

Phase 6 (steve-tk30) of the paired-comparison pivot."
```

---

## Task 12: `cli::run_subcommand` — `steve eval run` end-to-end

Builds a `Runner` per scenario per run (fresh tempdir each time so workspace state from prior runs doesn't leak), normalizes each `CapturedRun`, assembles a `ResultsFile`, writes YAML.

**Files:**
- Modify: `src/eval/cli.rs` (add `run_subcommand`)

- [ ] **Step 1: Add the `run_subcommand` function**

Append to `src/eval/cli.rs`:

```rust
/// `steve eval run` — runs scenarios K times each (K from `scenario.runs`),
/// writes a normalized `ResultsFile` YAML. No judging.
///
/// `scenario_filter` is the `--scenario` value (a name like "_smoke", not
/// a path). When `None`, every scenario under `scenarios_dir` is run.
/// `out_path` is where to write the YAML.
pub async fn run_subcommand(
    scenarios_dir: &Path,
    scenario_filter: Option<&str>,
    model: &str,
    out_path: &Path,
) -> Result<()> {
    use std::collections::BTreeMap;

    use crate::eval::results::{ResultsFile, ScenarioResults};
    use crate::eval::scenario::discover_scenarios;
    use crate::eval::transcript::Normalizer;

    let discovered = discover_scenarios(scenarios_dir)?;
    let selected: Vec<(String, std::path::PathBuf)> = match scenario_filter {
        Some(name) => discovered
            .into_iter()
            .filter(|(n, _)| n == name)
            .collect(),
        None => discovered,
    };
    if selected.is_empty() {
        match scenario_filter {
            Some(name) => anyhow::bail!(
                "no scenario named {name:?} found under {}",
                scenarios_dir.display()
            ),
            None => anyhow::bail!(
                "no scenarios found under {} (does the directory contain <name>/scenario.toml files?)",
                scenarios_dir.display()
            ),
        }
    }

    let mut scenarios_out: BTreeMap<String, ScenarioResults> = BTreeMap::new();

    for (name, scenario_path) in &selected {
        let scenario = Scenario::from_file(scenario_path)
            .with_context(|| format!("loading scenario {}", scenario_path.display()))?;
        let scenario_dir = scenario_path
            .parent()
            .with_context(|| format!("scenario path has no parent: {}", scenario_path.display()))?;

        let mut transcripts = Vec::with_capacity(scenario.runs.get());
        for run_idx in 0..scenario.runs.get() {
            // Fresh Runner per run -> fresh tempdir workspace. Without this,
            // `setup.shell` mutations from a prior run would persist into
            // the next run's working state. Each run is a clean sample.
            let mut runner = Runner::build(&scenario, scenario_dir, model)
                .with_context(|| format!("building runner for {name} run #{}", run_idx + 1))?;
            let captured = runner
                .run(&scenario)
                .await
                .with_context(|| format!("running scenario {name} run #{}", run_idx + 1))?;
            // Compute deterministic-floor verdict the same way `run_one` does:
            // expectations.passed() && captured.completed_normally().
            let report = evaluate(&scenario, &captured);
            let floor_passed = report.passed() && captured.completed_normally();
            transcripts.push(Normalizer::normalize(&captured, floor_passed));
        }

        scenarios_out.insert(
            name.clone(),
            ScenarioResults {
                user_turns: scenario.user_turns.clone(),
                runs: transcripts,
            },
        );
    }

    let git_ref = current_git_ref().unwrap_or_else(|| "unknown".to_string());
    let recorded_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let results = ResultsFile {
        git_ref,
        recorded_at,
        model: model.to_string(),
        scenarios: scenarios_out,
    };
    results.write_to_path(out_path)?;
    println!("wrote results to {}", out_path.display());
    Ok(())
}

/// Best-effort current git ref (short hash). Returns `None` outside a git
/// repo or if `git` is missing — callers fall back to `"unknown"` rather
/// than failing the whole eval. The build script's STEVE_GIT_REV is at
/// build time; this is the runtime ref of the workspace at run time, so
/// shelling out is the correct approach.
fn current_git_ref() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}
```

Imports at the top of `cli.rs` should remain:

```rust
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::json;

use crate::eval::{
    Judge, Runner, Scenario,
    apply_judges, evaluate,
    judge::validate_judge_config,
};
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: clean.

- [ ] **Step 3: Smoke-test against `_smoke` (manual, since this needs API keys)**

This step is for the implementer's sanity check. It is NOT required to be automated — the corresponding offline integration test in Task 15 covers the data path, and the manual end-to-end check is part of Task 15 as a ships-when verification step.

- [ ] **Step 4: Run unit tests for cli**

Run: `cargo test eval::`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/eval/cli.rs
git commit -m "feat(eval): add run_subcommand for 'steve eval run'

Discovers scenarios under <dir>, optionally filtered by --scenario.
For each scenario, builds a fresh Runner per run (fresh tempdir
workspace per sample), drives the agent, normalizes each capture,
and assembles a ResultsFile. Writes YAML via serde-saphyr.

git_ref comes from 'git rev-parse --short HEAD' at runtime (best-
effort; falls back to 'unknown' outside a git repo). recorded_at
is ISO 8601 UTC.

Phase 6 (steve-tk30) of the paired-comparison pivot."
```

---

## Task 13: `cli::freeze_subcommand` — `steve eval baseline freeze` end-to-end

**Files:**
- Modify: `src/eval/cli.rs` (add `freeze_subcommand`)

- [ ] **Step 1: Add the `freeze_subcommand` function**

Append to `src/eval/cli.rs`:

```rust
/// `steve eval baseline freeze` — captures one fresh transcript per
/// (scenario, model) and writes it to
/// `eval/baselines/<scenario>/<provider>/<model>.yaml`, plus a manifest
/// entry.
///
/// **K=1 regardless of `scenario.runs`.** Per spec: the baseline is the
/// fixed reference; the current side runs K samples and aggregates.
/// Doing N runs at freeze time would require defining "best run," which
/// requires a judge — circular, since the judge is what we're trying to
/// use the baseline to enable.
///
/// Filters compose: `(scenario_filter, model)` together select what to
/// freeze. `scenario_filter = None` runs every scenario.
pub async fn freeze_subcommand(
    scenarios_dir: &Path,
    baselines_dir: &Path,
    scenario_filter: Option<&str>,
    model: &str,
) -> Result<()> {
    use crate::eval::baseline::{
        BaselineFile, Manifest, ManifestEntry, baseline_path, manifest_path,
    };
    use crate::eval::scenario::discover_scenarios;
    use crate::eval::transcript::Normalizer;

    let discovered = discover_scenarios(scenarios_dir)?;
    let selected: Vec<(String, std::path::PathBuf)> = match scenario_filter {
        Some(name) => discovered
            .into_iter()
            .filter(|(n, _)| n == name)
            .collect(),
        None => discovered,
    };
    if selected.is_empty() {
        match scenario_filter {
            Some(name) => anyhow::bail!(
                "no scenario named {name:?} found under {}",
                scenarios_dir.display()
            ),
            None => anyhow::bail!(
                "no scenarios found under {}",
                scenarios_dir.display()
            ),
        }
    }

    // Read-modify-write the manifest. read_from_path returns Manifest::default()
    // on NotFound, so the fresh-checkout case Just Works.
    let mfst_path = manifest_path(baselines_dir);
    let mut manifest = Manifest::read_from_path(&mfst_path)?;

    let git_ref = current_git_ref().unwrap_or_else(|| "unknown".to_string());
    let frozen_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    for (name, scenario_path) in &selected {
        let scenario = Scenario::from_file(scenario_path)
            .with_context(|| format!("loading scenario {}", scenario_path.display()))?;
        let scenario_dir = scenario_path
            .parent()
            .with_context(|| format!("scenario path has no parent: {}", scenario_path.display()))?;

        let mut runner = Runner::build(&scenario, scenario_dir, model)
            .with_context(|| format!("building runner for {name}"))?;
        let captured = runner
            .run(&scenario)
            .await
            .with_context(|| format!("running scenario {name} for freeze"))?;
        let report = evaluate(&scenario, &captured);
        let floor_passed = report.passed() && captured.completed_normally();
        let transcript = Normalizer::normalize(&captured, floor_passed);

        let baseline = BaselineFile {
            scenario: name.clone(),
            model: model.to_string(),
            git_ref: git_ref.clone(),
            frozen_at: frozen_at.clone(),
            user_turns: scenario.user_turns.clone(),
            transcript,
        };
        let path = baseline_path(baselines_dir, name, model)?;
        baseline.write_to_path(&path)?;

        manifest.upsert(ManifestEntry {
            scenario: name.clone(),
            model: model.to_string(),
            git_ref: git_ref.clone(),
            frozen_at: frozen_at.clone(),
        });

        println!("froze {name} -> {}", path.display());
    }

    manifest.write_to_path(&mfst_path)?;
    println!("updated manifest: {}", mfst_path.display());
    Ok(())
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: clean.

- [ ] **Step 3: Run all tests**

Run: `cargo test eval::`
Expected: clean (no regressions).

- [ ] **Step 4: Commit**

```bash
git add src/eval/cli.rs
git commit -m "feat(eval): add freeze_subcommand for 'eval baseline freeze'

K=1 per spec — the baseline is the fixed reference. Reads the manifest
(Manifest::default() if missing), runs each selected scenario once,
normalizes the capture, writes BaselineFile YAML at
<baselines_dir>/<scenario>/<provider>/<model>.yaml, and upserts a
manifest entry. Manifest is written once at the end so a partial
failure doesn't leave the manifest claiming baselines exist that
weren't actually written (file writes happen first, manifest write
last).

Phase 6 (steve-tk30) of the paired-comparison pivot."
```

---

## Task 14: Wire the new clap subcommands in `main.rs`

The existing `steve eval <scenario.toml> --model X --judge-model X` form must keep working unchanged through Phase 6 (it's the dev loop the team uses today; Phase 8 retires it). The new shape adds optional sub-subcommands `run` and `baseline freeze` alongside it.

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Restructure `Commands::Eval`**

In `src/main.rs`, replace the existing `Commands::Eval { scenario, model, judge_model }` arm with:

```rust
    /// Run scenarios. Without a sub-subcommand, runs ONE scenario
    /// end-to-end and emits the captured trace as JSON (the existing
    /// Phase-5 path; transitional, retired in Phase 8).
    Eval(EvalArgs),
```

Add new types below the `Commands` enum:

```rust
/// `args_conflicts_with_subcommands` lets us keep the existing positional
/// `<scenario>` form (`steve eval scenarios/_smoke/scenario.toml --model X`,
/// the Phase-5 dev loop) while also offering the new sub-subcommands.
/// When a sub-subcommand is given, the positional args are not allowed
/// (and vice versa). The positional form is transitional — Phase 8
/// retires it.
#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
struct EvalArgs {
    /// Phase-5 single-shot path: `scenario.toml` to run end-to-end with a
    /// captured-trace JSON dump on stdout. Mutually exclusive with the
    /// sub-subcommands below. Internally forces runs = 1 regardless of
    /// `scenario.runs`. Transitional; Phase 8 retires this shape.
    #[arg(value_name = "SCENARIO")]
    scenario: Option<std::path::PathBuf>,
    /// Model to run against, in `provider/model_id` format. Required for
    /// the positional form.
    #[arg(long)]
    model: Option<String>,
    /// Override the judge model for `Judge` expectations (positional form).
    #[arg(long)]
    judge_model: Option<String>,
    #[command(subcommand)]
    command: Option<EvalSubcommand>,
}

#[derive(clap::Subcommand)]
enum EvalSubcommand {
    /// Run scenarios K times each (K from `scenario.runs`), writing a
    /// normalized results YAML. No judging.
    Run {
        /// Scenario name (e.g. `_smoke`). When omitted, runs every
        /// scenario under `eval/scenarios/`.
        #[arg(long)]
        scenario: Option<String>,
        /// Model to run against, in `provider/model_id` format.
        #[arg(long)]
        model: String,
        /// Output path for the results YAML. Defaults to a timestamped
        /// path in the current directory.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Manage frozen baselines.
    Baseline {
        #[command(subcommand)]
        command: BaselineSubcommand,
    },
}

#[derive(clap::Subcommand)]
enum BaselineSubcommand {
    /// Freeze (capture and overwrite) baseline files for selected scenarios.
    /// `K = 1` regardless of `scenario.runs`; the baseline is the fixed
    /// reference, not a multi-sample artifact. No flags = all scenarios
    /// with the supplied (or configured-default) model.
    Freeze {
        /// Scenario name. When omitted, freezes every scenario.
        #[arg(long)]
        scenario: Option<String>,
        /// Model to freeze for, in `provider/model_id` format.
        #[arg(long)]
        model: String,
    },
}
```

- [ ] **Step 2: Update the dispatch**

Replace the existing `Some(Commands::Eval { ... })` arm:

```rust
        Some(Commands::Eval(args)) => {
            return dispatch_eval(args).await;
        }
```

Add a `dispatch_eval` helper at the bottom of `main.rs` (or wherever helpers live):

```rust
async fn dispatch_eval(args: EvalArgs) -> Result<()> {
    let scenarios_dir = std::path::Path::new("eval/scenarios");
    let baselines_dir = std::path::Path::new("eval/baselines");

    // Sub-subcommand path — new shapes.
    if let Some(sub) = args.command {
        match sub {
            EvalSubcommand::Run { scenario, model, out } => {
                let out_path = out.unwrap_or_else(|| {
                    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                    std::path::PathBuf::from(format!("eval-results-{ts}.yaml"))
                });
                return steve::eval::cli::run_subcommand(
                    scenarios_dir,
                    scenario.as_deref(),
                    &model,
                    &out_path,
                )
                .await;
            }
            EvalSubcommand::Baseline { command } => match command {
                BaselineSubcommand::Freeze { scenario, model } => {
                    return steve::eval::cli::freeze_subcommand(
                        scenarios_dir,
                        baselines_dir,
                        scenario.as_deref(),
                        &model,
                    )
                    .await;
                }
            },
        }
    }

    // Phase-5 positional path — preserved through Phase 6, retired in Phase 8.
    // Required: scenario + --model.
    let Some(scenario) = args.scenario else {
        anyhow::bail!(
            "supply a scenario path (e.g. 'steve eval eval/scenarios/_smoke/scenario.toml --model X') \
             or use a sub-subcommand ('steve eval run', 'steve eval baseline freeze')"
        );
    };
    let Some(model) = args.model else {
        anyhow::bail!("'steve eval <scenario>' requires --model");
    };
    steve::eval::cli::run_one(&scenario, &model, args.judge_model.as_deref()).await
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check`
Expected: clean.

- [ ] **Step 4: Verify clap parses both shapes**

Run:
```bash
cargo run -- eval --help
cargo run -- eval run --help
cargo run -- eval baseline freeze --help
```

Expected output: each form prints clean help text. No clap parse errors.

Run: `cargo run -- eval eval/scenarios/_smoke/scenario.toml --model fake/model 2>&1 | head -5`
Expected: positional path attempts to run (will fail downstream because `fake/model` isn't configured, but the parse succeeds and the call reaches `run_one`).

- [ ] **Step 5: Run all tests**

Run: `cargo test`
Expected: clean.

- [ ] **Step 6: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs
git commit -m "feat(eval): wire 'eval run' and 'eval baseline freeze' subcommands

clap restructure: Commands::Eval now takes an EvalArgs struct that
supports two shapes via args_conflicts_with_subcommands:

  - Phase-5 positional: 'steve eval <scenario.toml> --model X' ->
    dispatches to cli::run_one (single-shot pretty-JSON dump).
  - New sub-subcommands: 'steve eval run' (multi-run results.yaml)
    and 'steve eval baseline freeze' (baseline file + manifest).

The positional form is preserved through Phase 6 per the spec's
ships-when criterion #4 — the team's current dev loop runs through
it. Phase 8 retires it. The whole eval module is on feat/eval-harness
and unshipped, so this is purely about not disturbing the in-flight
feedback loop.

The new forms use scenarios_dir = 'eval/scenarios' and
baselines_dir = 'eval/baselines' relative to CWD.

Phase 6 (steve-tk30) of the paired-comparison pivot."
```

---

## Task 15: End-to-end ships-when verification

This is the integration check. It exercises the whole pipeline against the existing `_smoke` scenario using a configured provider/model. It is partly manual (requires API keys) and partly automatic (the no-API-key bits run as `cargo test`).

**Files:**
- Modify: `src/eval/cli.rs` (add an integration test that exercises freeze + run on a fixture scenario without an LLM round-trip)

- [ ] **Step 1: Add an offline round-trip test for the YAML/manifest pipeline**

The full `freeze_subcommand` and `run_subcommand` need a real LLM (which is out of scope for unit tests — the existing eval tests don't drive a Runner end-to-end either). But we CAN test the YAML+manifest assembly path by manually constructing a `CapturedRun`, normalizing it, and writing/reading via `BaselineFile` and `ResultsFile`. Add to `src/eval/cli.rs`:

```rust
#[cfg(test)]
mod integration_tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::Duration;

    use serde_json::json;

    use crate::eval::baseline::{
        BaselineFile, Manifest, ManifestEntry, baseline_path, manifest_path,
    };
    use crate::eval::capture::CapturedRun;
    use crate::eval::results::{ResultsFile, ScenarioResults};
    use crate::eval::transcript::{Normalizer, TranscriptEvent};
    use crate::eval::workspace::WorkspaceSnapshot;
    use crate::event::AppEvent;
    use crate::tool::{ToolName, ToolOutput};

    fn fake_captured() -> CapturedRun {
        let mut cap = CapturedRun::new(
            PathBuf::from("/tmp/fake-eval"),
            WorkspaceSnapshot { files: BTreeMap::new() },
        );
        cap.observe(&AppEvent::LlmDelta { text: "Reading.".into() });
        cap.observe(&AppEvent::LlmToolCall {
            call_id: "uuid-1".into(),
            tool_name: ToolName::Read,
            arguments: json!({"path": "/tmp/fake-eval/foo.txt"}),
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
        cap.observe(&AppEvent::LlmFinish { usage: None });
        cap.duration = Duration::from_millis(123);
        cap
    }

    /// End-to-end YAML pipeline: build a fake CapturedRun, normalize it,
    /// wrap in a BaselineFile, write to disk, read back. Verifies the
    /// freeze-side data path.
    #[test]
    fn freeze_pipeline_round_trip_via_disk() {
        let dir = tempfile::tempdir().unwrap();
        let baselines = dir.path().to_path_buf();

        let cap = fake_captured();
        let transcript = Normalizer::normalize(&cap, true);

        let baseline = BaselineFile {
            scenario: "_fake".into(),
            model: "ollama/qwen3-coder".into(),
            git_ref: "abc1234".into(),
            frozen_at: "2026-05-07T00:00:00Z".into(),
            user_turns: vec!["Read the file.".into()],
            transcript,
        };
        let path = baseline_path(&baselines, "_fake", "ollama/qwen3-coder").unwrap();
        baseline.write_to_path(&path).unwrap();
        assert!(path.exists());
        assert_eq!(
            path.strip_prefix(&baselines).unwrap(),
            std::path::Path::new("_fake/ollama/qwen3-coder.yaml"),
            "path layout must match the spec"
        );

        let mut manifest = Manifest::read_from_path(&manifest_path(&baselines)).unwrap();
        manifest.upsert(ManifestEntry {
            scenario: "_fake".into(),
            model: "ollama/qwen3-coder".into(),
            git_ref: "abc1234".into(),
            frozen_at: "2026-05-07T00:00:00Z".into(),
        });
        manifest.write_to_path(&manifest_path(&baselines)).unwrap();

        // Read everything back.
        let back = BaselineFile::read_from_path(&path).unwrap();
        assert_eq!(back, baseline);
        let back_manifest = Manifest::read_from_path(&manifest_path(&baselines)).unwrap();
        assert_eq!(back_manifest.baseline.len(), 1);
        assert_eq!(back_manifest.baseline[0].scenario, "_fake");

        // Workspace-tempdir leak check: serialized YAML must not contain
        // the fake captured tempdir path.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("/tmp/fake-eval"), "workspace path leaked into baseline YAML: {raw}");
    }

    /// End-to-end results pipeline: build several fake CapturedRuns,
    /// normalize each, assemble a ResultsFile with K=3 transcripts, write
    /// to disk, read back. Verifies the run-side data path.
    #[test]
    fn run_pipeline_round_trip_via_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("results.yaml");

        let transcripts: Vec<_> = (0..3)
            .map(|_| Normalizer::normalize(&fake_captured(), true))
            .collect();
        let mut scenarios = BTreeMap::new();
        scenarios.insert(
            "_fake".to_string(),
            ScenarioResults {
                user_turns: vec!["Read the file.".into()],
                runs: transcripts,
            },
        );
        let results = ResultsFile {
            git_ref: "abc1234".into(),
            recorded_at: "2026-05-07T12:00:00Z".into(),
            model: "ollama/qwen3-coder".into(),
            scenarios,
        };
        results.write_to_path(&path).unwrap();

        let back = ResultsFile::read_from_path(&path).unwrap();
        assert_eq!(back, results);
        assert_eq!(back.scenarios.get("_fake").unwrap().runs.len(), 3);

        // Sanity: each transcript has the expected event shape.
        let evts = &back.scenarios.get("_fake").unwrap().runs[0].events;
        assert!(evts.iter().any(|e| matches!(e, TranscriptEvent::ToolCall { tool_name, .. } if *tool_name == ToolName::Read)));
        assert!(evts.iter().any(|e| matches!(e, TranscriptEvent::AssistantMessage { .. })));
    }
}
```

- [ ] **Step 2: Run the integration tests**

Run: `cargo test eval::cli::integration_tests`
Expected: 2 tests pass.

- [ ] **Step 3: Manual smoke test (requires API key + configured model)**

These are the spec's ships-when criteria. They cannot run as automated tests because they hit a real LLM provider. The implementer must verify by hand:

```bash
# 1. Freeze a baseline for the smoke scenario.
cargo run -- eval baseline freeze --scenario _smoke --model <provider>/<model>

# Expected on success:
#   froze _smoke -> eval/baselines/_smoke/<provider>/<model>.yaml
#   updated manifest: eval/baselines/manifest.toml
ls eval/baselines/_smoke/<provider>/<model>.yaml
cat eval/baselines/manifest.toml

# 2. Run the smoke scenario (3 runs by default).
cargo run -- eval run --scenario _smoke --model <provider>/<model> --out /tmp/smoke-results.yaml
# Expected on success: "wrote results to /tmp/smoke-results.yaml"
grep -c "^  -" /tmp/smoke-results.yaml  # at least 3 entries under runs:
grep "model: <provider>/<model>" /tmp/smoke-results.yaml

# 3. Freeze every Phase-5 scenario for the configured model.
cargo run -- eval baseline freeze --model <provider>/<model>
# Expected: one "froze X" line per scenario directory under eval/scenarios/.

# 4. Confirm the Phase-5 positional path still works (ships-when criterion #4):
cargo run -- eval eval/scenarios/_smoke/scenario.toml --model <provider>/<model>
# Expected: pretty JSON with passed/results/tool_calls fields, exit 0.
```

- [ ] **Step 4: Commit**

```bash
git add src/eval/cli.rs
git commit -m "test(eval): integration round-trip for freeze + run pipelines

Builds a fake CapturedRun (no LLM), normalizes it, exercises both the
BaselineFile-on-disk path (with manifest read/write) and the
ResultsFile-on-disk path. Pins:
  - Path layout for baselines (<dir>/<scenario>/<provider>/<model>.yaml)
  - Manifest read-modify-write semantics across a real tempdir
  - Workspace-tempdir prefix is stripped from the on-disk YAML
  - 3-transcript results.yaml round-trips byte-stable via the type contract

End-to-end LLM-driven checks against _smoke and the Phase-5 scenarios
require API keys and remain a manual ships-when verification step.

Phase 6 (steve-tk30) of the paired-comparison pivot."
```

---

## Task 16: Close the beads issue

- [ ] **Step 1: Confirm ships-when criteria are met**

The spec lists four ships-when criteria for Phase 6:

1. A user can `steve eval baseline freeze --scenario _smoke --model X` and inspect the YAML by hand. (Verified in Task 15 step 3.)
2. A user can `steve eval run --scenario _smoke --model X` and get a multi-run results.yaml. (Verified in Task 15 step 3.)
3. All Phase-5 scenarios baseline successfully against a configured default model. (Verified in Task 15 step 3.)
4. No reporting yet; `steve eval` (no subcommand) preserves the Phase-5 single-run pretty-JSON output untouched. (Verified in Task 15 step 3.)

- [ ] **Step 2: Push the branch**

```bash
git push
```

- [ ] **Step 3: Update beads**

```bash
bd update steve-tk30 --notes "Phase 6 shipped 2026-05-07 — schema overhaul, Normalizer, multi-run, baseline storage on disk, manifest TOML, eval run + eval baseline freeze subcommands. Phase-5 positional 'steve eval <path>' path preserved through Phase 6 (Phase 8 retires it). Manual ships-when smoke against _smoke + all Phase-5 scenarios passed against the configured default model."
bd close steve-tk30
```

- [ ] **Step 4: PR (optional, depending on workflow)**

If shipping as one PR rather than rolling commits onto `feat/eval-harness`, see `superpowers:finishing-a-development-branch` for the merge/PR options. The spec notes Phase 6 may land as one or more PRs targeted at `feat/eval-harness`.

---

## Self-Review (recorded after writing the plan)

**Spec coverage check** — every Phase 6 scope item from the spec maps to a task:

| Spec scope item | Task |
|---|---|
| Schema: PairedScore, ScenarioScore, Axis, Verdict | Task 2 |
| Schema: NormalizedTranscript | Task 3 |
| Schema: ScenarioResults, ResultsFile | Task 5 |
| Schema: BaselineFile | Task 7 |
| Axis enum (no `[scoring]` parser yet) | Task 2 (parser deferred to Phase 7 explicitly) |
| Normalizer helper | Task 4 |
| Multi-run: honor `Scenario.runs`; runner produces `Vec<CapturedRun>` | Task 10 |
| Multi-run: remove `runs > 1` bail | Task 10 |
| Default `runs = 3` | Task 9 |
| Baseline storage YAML at `<scenario>/<provider>/<model>.yaml` | Task 7 (file) + Task 13 (subcommand writes it) |
| `manifest.toml` reader/writer | Task 8 |
| `steve eval baseline freeze` subcommand | Task 13 + Task 14 |
| `steve eval run` subcommand | Task 12 + Task 14 |

**Placeholder scan** — no TBD/TODO/"add appropriate" placeholders. Every code block is the literal code to type. Test code is concrete with named asserts. Commands have expected outputs. Error messages quote the spec's policy.

**Type consistency check** — names that recur across tasks:
- `Axis` (Task 2), `Verdict` (Task 2), `PairedScore` (Task 2), `ScenarioScore` (Task 2), `CompareVerdict` (Task 2). Consistent.
- `TranscriptEvent` (Task 3), `UsageSummary` (Task 3), `NormalizedTranscript` (Task 3), `Normalizer` (Task 4). Consistent.
- `ScenarioResults` (Task 5), `ResultsFile` (Task 5). Methods: `to_yaml_string`, `from_yaml_str`, `write_to_path`, `read_from_path` (Task 6). Consistent.
- `BaselineFile` (Task 7) with same four method names (Task 7). `Manifest`, `ManifestEntry` (Task 8) with same four method names. Consistent.
- `baseline_path(baselines_dir, scenario, model)` (Task 7) called from Task 13 with same signature. Consistent.
- `manifest_path(baselines_dir)` (Task 8) called from Task 13 with same signature. Consistent.
- `Manifest::upsert(entry: ManifestEntry)` (Task 8) called from Task 13 with same signature. Consistent.
- `discover_scenarios(scenarios_dir) -> Vec<(String, PathBuf)>` (Task 11) called from Tasks 12 and 13 with same signature. Consistent.
- `Normalizer::normalize(captured, deterministic_floor_passed)` (Task 4) called from Tasks 12, 13, and 15 with same signature. Consistent.
- `Runner::run_n` (Task 10) — added but not called by Tasks 12/13 (those rebuild the Runner per run for fresh-tempdir isolation). Documented in the doc comment. Consistent.
- `run_subcommand` and `freeze_subcommand` signatures match between Tasks 12, 13, and 14.

**Ordering / dependency check** — each task only depends on tasks before it:
- Task 1 (deps) -> 2 (score) -> 3 (transcript types) -> 4 (Normalizer) -> 5 (results types) -> 6 (results YAML) -> 7 (baseline + path) -> 8 (manifest) -> 9 (default runs) -> 10 (runner multi-run) -> 11 (discovery) -> 12 (run subcommand) -> 13 (freeze subcommand) -> 14 (clap wiring) -> 15 (integration test) -> 16 (close).

The plan is self-consistent and complete.

---

## Execution Handoff

**Plan saved to `docs/superpowers/plans/2026-05-07-eval-phase-6-data-foundation.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — Dispatch a fresh subagent per task, review between tasks, fast iteration. Good for a 16-task plan where the per-task scope is small and the integration risk is concentrated in Tasks 12/13/14 (where reviews catch the most).

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints for review.

**Which approach?**
