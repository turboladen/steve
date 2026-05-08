//! Top-level shape of `results.yaml` — output of `steve eval run`.
//!
//! Multi-scenario container. Each scenario carries its inputs (user_turns)
//! and the K transcripts produced by running the scenario K times.
//! Distinct from `BaselineFile` (single-scenario, single-transcript,
//! sharded on disk) but they share `NormalizedTranscript`.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
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

#[cfg(test)]
mod tests {
    use std::fs;

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
        assert!(
            raw.contains("git_ref"),
            "expected human-readable YAML: {raw}"
        );
        assert!(
            raw.contains("ollama/qwen3-coder"),
            "model identity should round-trip: {raw}"
        );
    }
}
