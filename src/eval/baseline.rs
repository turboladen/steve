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
        assert_eq!(
            p,
            Path::new("eval/baselines/_smoke/ollama/qwen3-coder.yaml")
        );
    }

    #[test]
    fn baseline_path_handles_provider_model_with_internal_dashes() {
        let dir = Path::new("eval/baselines");
        let p = baseline_path(
            dir,
            "stop-guessing-after-failures",
            "anthropic/claude-haiku-4-5",
        )
        .unwrap();
        assert_eq!(
            p,
            Path::new(
                "eval/baselines/stop-guessing-after-failures/anthropic/claude-haiku-4-5.yaml"
            )
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
        assert!(
            yaml.contains("user_turns"),
            "user_turns must be a top-level field: {yaml}"
        );
        assert!(
            yaml.contains("transcript"),
            "transcript must be a top-level field: {yaml}"
        );
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
