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
    use crate::{
        eval::transcript::{TranscriptEvent, UsageSummary},
        tool::ToolName,
    };
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
        scenarios.insert(
            "zoo".to_string(),
            ScenarioResults {
                user_turns: vec![],
                runs: vec![],
            },
        );
        scenarios.insert(
            "alpha".to_string(),
            ScenarioResults {
                user_turns: vec![],
                runs: vec![],
            },
        );
        scenarios.insert(
            "middle".to_string(),
            ScenarioResults {
                user_turns: vec![],
                runs: vec![],
            },
        );
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
        assert!(
            alpha < middle && middle < zoo,
            "scenarios must be alphabetical, got: {s}"
        );
    }

    #[test]
    fn results_file_rejects_unknown_top_level_fields() {
        // deny_unknown_fields on the manifest format — typos in a hand-edited
        // results.yaml become hard errors instead of silent default values.
        let bad =
            r#"{"git_ref":"x","recorded_at":"y","model":"z","scenarios":{},"unknown":"oops"}"#;
        let r: Result<ResultsFile, _> = serde_json::from_str(bad);
        assert!(r.is_err(), "unknown field 'unknown' must be rejected");
    }
}
