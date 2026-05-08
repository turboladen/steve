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
        assert_eq!(
            serde_json::to_string(&Axis::Correctness).unwrap(),
            "\"correctness\""
        );
        assert_eq!(
            serde_json::to_string(&Axis::Robustness).unwrap(),
            "\"robustness\""
        );
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
        assert_eq!(
            serde_json::to_string(&Verdict::CurrentWins).unwrap(),
            "\"current_wins\""
        );
        assert_eq!(
            serde_json::to_string(&Verdict::BaselineWins).unwrap(),
            "\"baseline_wins\""
        );
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
        assert!(
            r_pos < v_pos,
            "rationale must serialize before verdict, got: {s}"
        );
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
