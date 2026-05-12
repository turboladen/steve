//! Eval-subsystem configuration, loaded from a file separate from the
//! main `Config` so the base `.steve.jsonc` doesn't grow unbounded as
//! eval features expand.
//!
//! - Global: `~/.config/steve/eval.jsonc`
//! - Project: `.steve.eval.jsonc` in the project root
//!
//! Project overrides global on a field-by-field basis. Model
//! references (e.g., `default_judge_model = "fuel-ix/claude-haiku-4-5"`)
//! resolve through the same `ProviderRegistry` built from the base
//! `Config` — the eval config supplies identifiers, not provider
//! definitions.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvalConfig {
    /// Threshold for `steve eval report` exit code. Report exits with
    /// code 1 when the net win rate is strictly less than this value;
    /// otherwise 0 (or 2 on infra failure). Defaults to `0.0`. The
    /// `--regression-threshold` CLI flag overrides this.
    pub regression_threshold: Option<f64>,

    /// Default judge model in `provider/model_id` format. Used when
    /// `--judge-model` isn't passed and the scenario doesn't declare
    /// its own. Refers to a model defined in `.steve.jsonc`'s
    /// `providers` section; resolution still goes through the base
    /// `ProviderRegistry`.
    pub default_judge_model: Option<String>,

    /// Default baselines directory. Relative paths anchored to the
    /// project root. Defaults to `eval/baselines/`.
    pub baselines_dir: Option<String>,
}

impl EvalConfig {
    /// Merge two configs field-by-field. `other` (project) wins where
    /// it's set; `self` (global) fills missing fields.
    pub fn merge(self, other: EvalConfig) -> EvalConfig {
        EvalConfig {
            regression_threshold: other.regression_threshold.or(self.regression_threshold),
            default_judge_model: other.default_judge_model.or(self.default_judge_model),
            baselines_dir: other.baselines_dir.or(self.baselines_dir),
        }
    }
}

/// Load `EvalConfig` from `~/.config/steve/eval.jsonc` (global) merged
/// with `.steve.eval.jsonc` (project). Missing files are treated as
/// empty configs — eval ships with all defaults if neither exists.
pub fn load_eval_config(project_root: &Path) -> Result<EvalConfig> {
    load_with_override(project_root, None)
}

/// Test-friendly variant of `load_eval_config` that takes an explicit
/// global-config dir override (defaults to `~/.config/steve/`).
fn load_with_override(project_root: &Path, global_override: Option<&Path>) -> Result<EvalConfig> {
    let global = load_global(global_override)?;
    let project = load_project(project_root)?;
    Ok(global.merge(project))
}

fn load_global(override_dir: Option<&Path>) -> Result<EvalConfig> {
    let dir: PathBuf = match override_dir {
        Some(d) => d.to_path_buf(),
        None => match std::env::var("HOME") {
            Ok(home) => Path::new(&home).join(".config").join("steve"),
            Err(_) => return Ok(EvalConfig::default()),
        },
    };
    let path = dir.join("eval.jsonc");
    parse_eval_jsonc(&path)
}

fn load_project(project_root: &Path) -> Result<EvalConfig> {
    let path = project_root.join(".steve.eval.jsonc");
    parse_eval_jsonc(&path)
}

/// Parse a single `eval.jsonc` (or `.steve.eval.jsonc`) file. Missing
/// file returns `Ok(default)`. Parser pipeline mirrors the base
/// config's `load_jsonc_file`: jsonc_parser → serde_json::Value →
/// serde_json::from_value into `EvalConfig`.
fn parse_eval_jsonc(path: &Path) -> Result<EvalConfig> {
    if !path.exists() {
        return Ok(EvalConfig::default());
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let json_value: Option<serde_json::Value> =
        jsonc_parser::parse_to_serde_value(&content, &Default::default())
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
    match json_value {
        Some(value) => serde_json::from_value(value)
            .with_context(|| format!("failed to deserialize eval config from {}", path.display())),
        None => Ok(EvalConfig::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn missing_files_yields_defaults() {
        let project = TempDir::new().unwrap();
        let global = TempDir::new().unwrap(); // empty
        let cfg = load_with_override(project.path(), Some(global.path())).unwrap();
        assert_eq!(cfg, EvalConfig::default());
        assert!(cfg.regression_threshold.is_none());
        assert!(cfg.default_judge_model.is_none());
    }

    #[test]
    fn project_overrides_global_per_field() {
        let project = TempDir::new().unwrap();
        let global = TempDir::new().unwrap();
        std::fs::write(
            global.path().join("eval.jsonc"),
            r#"{ "regression_threshold": -0.01, "default_judge_model": "global/judge" }"#,
        )
        .unwrap();
        std::fs::write(
            project.path().join(".steve.eval.jsonc"),
            // project sets only regression_threshold; global's
            // default_judge_model should bleed through.
            r#"{ "regression_threshold": -0.05 }"#,
        )
        .unwrap();
        let cfg = load_with_override(project.path(), Some(global.path())).unwrap();
        assert_eq!(cfg.regression_threshold, Some(-0.05));
        assert_eq!(cfg.default_judge_model.as_deref(), Some("global/judge"));
    }

    #[test]
    fn unknown_field_in_project_rejected_at_parse_time() {
        // deny_unknown_fields catches typos like `regression_thresold`.
        let project = TempDir::new().unwrap();
        let global = TempDir::new().unwrap();
        std::fs::write(
            project.path().join(".steve.eval.jsonc"),
            r#"{ "regression_thresold": -0.05 }"#,
        )
        .unwrap();
        let err = load_with_override(project.path(), Some(global.path())).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("regression_thresold") || msg.contains("unknown field"),
            "got: {msg}"
        );
    }

    #[test]
    fn default_judge_model_round_trips() {
        let project = TempDir::new().unwrap();
        let global = TempDir::new().unwrap();
        std::fs::write(
            project.path().join(".steve.eval.jsonc"),
            r#"{ "default_judge_model": "fuel-ix/claude-haiku-4-5" }"#,
        )
        .unwrap();
        let cfg = load_with_override(project.path(), Some(global.path())).unwrap();
        assert_eq!(
            cfg.default_judge_model.as_deref(),
            Some("fuel-ix/claude-haiku-4-5")
        );
    }

    #[test]
    fn merge_preserves_unset_fields() {
        let g = EvalConfig {
            regression_threshold: Some(-0.01),
            default_judge_model: Some("g/m".into()),
            baselines_dir: None,
        };
        let p = EvalConfig {
            regression_threshold: None,
            default_judge_model: None,
            baselines_dir: Some("override/path".into()),
        };
        let m = g.merge(p);
        assert_eq!(m.regression_threshold, Some(-0.01));
        assert_eq!(m.default_judge_model.as_deref(), Some("g/m"));
        assert_eq!(m.baselines_dir.as_deref(), Some("override/path"));
    }

    #[test]
    fn jsonc_with_comments_parses() {
        // The base config supports JSONC (JSON-with-comments).
        // EvalConfig must too since it uses the same parser pipeline.
        let project = TempDir::new().unwrap();
        let global = TempDir::new().unwrap();
        std::fs::write(
            project.path().join(".steve.eval.jsonc"),
            r#"{
              // be strict about regressions
              "regression_threshold": 0.0
            }"#,
        )
        .unwrap();
        let cfg = load_with_override(project.path(), Some(global.path())).unwrap();
        assert_eq!(cfg.regression_threshold, Some(0.0));
    }
}
