//! Scenario types and TOML parser.
//!
//! A scenario lives in its own directory under `eval/scenarios/<name>/`:
//!
//! ```text
//! eval/scenarios/
//!   recover-after-destructive-edit/
//!     scenario.toml             # manifest
//!     fixtures/                 # files copied to scenario tempdir at setup
//!       .teller.yml
//!       .env.tpl
//! ```
//!
//! The manifest enumerates fixture files to copy (paths relative to the scenario dir),
//! shell commands to run after copying, the scripted user turns, and the expectations
//! to evaluate.
//!
//! v1 message-content assertions are substring-based (`final_message_contains` /
//! `final_message_not_contains`), not regex. Anything fuzzy goes through the `Judge`
//! variant. This keeps the dep surface off the `regex` crate (CLAUDE.md guidance) and
//! steers authors toward the more durable judge-based check for behavioral phrasing.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One scenario manifest, parsed from `scenario.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Scenario {
    /// Human-readable identifier — must match the scenario directory name.
    pub name: String,
    /// One-line description of what failure mode this scenario tests.
    pub description: String,
    /// Number of independent runs per (scenario, model). Default 1. Increase for known-flaky
    /// scenarios; pass criterion is then `>= ceil(runs / 2)` passes.
    #[serde(default = "default_runs")]
    pub runs: usize,
    /// Filesystem setup applied before the conversation starts.
    #[serde(default)]
    pub setup: Setup,
    /// Scripted user turns. The first is the initial prompt; subsequent entries are sent
    /// in order, each after the previous assistant response completes. v1 has no
    /// trigger-based scheduling — straight FIFO.
    pub user_turns: Vec<String>,
    /// Assertions evaluated against the captured run.
    pub expectations: Vec<Expectation>,
}

fn default_runs() -> usize {
    1
}

/// Filesystem setup: copy fixtures into the scenario tempdir and optionally run shell
/// commands (e.g., `git init`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Setup {
    /// Paths relative to the scenario directory. Each path is copied into the tempdir,
    /// preserving its directory structure relative to the scenario root.
    #[serde(default)]
    pub copy_fixtures: Vec<PathBuf>,
    /// Shell commands run inside the tempdir, in order, after fixtures are copied.
    #[serde(default)]
    pub shell: Vec<String>,
}

/// One assertion to evaluate against a captured run.
///
/// Tagged enum: TOML authors write `kind = "tool_called"` (etc.) to select a variant.
/// Variants are split into rule-based (structural checks on the trace) and judge-based
/// (LLM evaluation against a plain-English rubric).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Expectation {
    /// Tool name appears in the tool-call sequence at least once.
    ToolCalled { tool: String },
    /// Tool name never appears in the tool-call sequence.
    ToolNotCalled { tool: String },
    /// `tool` was called, AND at least one of `must_read_one_of` paths was the target
    /// of a `read`/`grep`/`list`/`glob`/`symbols`/`find_symbol` call before `tool`'s
    /// first invocation. Ordering uses the sequential ordering of *completed* tool
    /// calls (parallel reads in stream Phase 2 are ordered by completion time).
    ToolCalledBefore {
        tool: String,
        must_read_one_of: Vec<PathBuf>,
    },
    /// File content at scenario end matches the initial fixture content byte-for-byte.
    FileUnchanged { path: PathBuf },
    /// File at scenario end contains `substring`. Substring match, not regex.
    FileContains {
        path: PathBuf,
        substring: String,
        #[serde(default)]
        case_insensitive: bool,
    },
    /// Last assistant message contains `substring`. Substring match — for fuzzier checks
    /// use `Judge`.
    FinalMessageContains {
        substring: String,
        #[serde(default)]
        case_insensitive: bool,
    },
    /// Last assistant message does NOT contain `substring`. The "surrender" check.
    /// For phrasing-robust variants, use `Judge`.
    FinalMessageNotContains {
        substring: String,
        #[serde(default)]
        case_insensitive: bool,
    },
    /// Same `(tool, args_hash)` invocation appears at most `max` times in the sequence.
    /// For the "stop guessing after failures" pattern.
    MaxRepeatAttempts { tool: String, max: usize },
    /// LLM-as-judge: a small judge model (default Haiku 4.5, temperature 0) evaluates
    /// the rubric against the final message + tool-call summary. Judge inputs and outputs
    /// are recorded in the JSONL output for reproducibility.
    Judge {
        /// Plain-English rubric. Should explicitly state PASS and FAIL conditions.
        rubric: String,
        /// Override the global judge model for this expectation. Format: `provider/model_id`.
        #[serde(default)]
        judge_model: Option<String>,
    },
}

impl Scenario {
    /// Parse a scenario manifest from a TOML file.
    ///
    /// `path` should point to the `scenario.toml` file inside a scenario directory.
    /// The scenario `name` field is validated against the parent directory name to catch
    /// rename drift early.
    pub fn from_file(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading scenario manifest at {}", path.display()))?;
        let scenario: Scenario = toml::from_str(&raw)
            .with_context(|| format!("parsing scenario manifest at {}", path.display()))?;
        scenario.validate(path)?;
        Ok(scenario)
    }

    /// Parse a scenario manifest from a TOML string. Skips parent-directory validation —
    /// useful for tests and for in-memory scenarios.
    ///
    /// Named `from_toml_str` (not `from_str`) to avoid ambiguity with `std::str::FromStr`,
    /// which has different error semantics and would constrain the error type.
    pub fn from_toml_str(toml_src: &str) -> Result<Self> {
        let scenario: Scenario =
            toml::from_str(toml_src).context("parsing scenario manifest from string")?;
        scenario.validate_self_only()?;
        Ok(scenario)
    }

    /// Self-consistency checks that don't depend on filesystem state.
    fn validate_self_only(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            anyhow::bail!("scenario name must not be empty");
        }
        if self.user_turns.is_empty() {
            anyhow::bail!(
                "scenario {} must have at least one user_turn (the initial prompt)",
                self.name
            );
        }
        if self.expectations.is_empty() {
            anyhow::bail!("scenario {} must have at least one expectation", self.name);
        }
        if self.runs == 0 {
            anyhow::bail!("scenario {} runs must be >= 1", self.name);
        }
        Ok(())
    }

    /// Full validation including parent-directory name check.
    fn validate(&self, manifest_path: &Path) -> Result<()> {
        self.validate_self_only()?;
        let parent_name = manifest_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str());
        if let Some(dir_name) = parent_name
            && dir_name != self.name
        {
            anyhow::bail!(
                "scenario name {:?} does not match parent directory {:?} ({})",
                self.name,
                dir_name,
                manifest_path.display()
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_scenario_toml() -> &'static str {
        r#"
name = "minimal"
description = "smallest valid scenario"
user_turns = ["hello"]

[[expectations]]
kind = "tool_called"
tool = "read"
"#
    }

    #[test]
    fn parse_minimal_scenario() {
        let s = Scenario::from_toml_str(minimal_scenario_toml()).unwrap();
        assert_eq!(s.name, "minimal");
        assert_eq!(s.description, "smallest valid scenario");
        assert_eq!(s.runs, 1, "default runs = 1 when omitted");
        assert!(s.setup.copy_fixtures.is_empty());
        assert!(s.setup.shell.is_empty());
        assert_eq!(s.user_turns, vec!["hello"]);
        assert_eq!(s.expectations.len(), 1);
        assert!(matches!(
            s.expectations[0],
            Expectation::ToolCalled { ref tool } if tool == "read"
        ));
    }

    #[test]
    fn parse_full_scenario_with_all_expectation_kinds() {
        let toml_src = r#"
name = "kitchen-sink"
description = "exercise every expectation kind"
runs = 3
user_turns = ["first", "second", "third"]

[setup]
copy_fixtures = ["fixtures/.teller.yml", "fixtures/AGENTS.md"]
shell = ["git init -q", "echo .teller.yml > .gitignore"]

[[expectations]]
kind = "tool_called"
tool = "read"

[[expectations]]
kind = "tool_not_called"
tool = "bash"

[[expectations]]
kind = "tool_called_before"
tool = "edit"
must_read_one_of = [".teller.yml", "AGENTS.md"]

[[expectations]]
kind = "file_unchanged"
path = "AGENTS.md"

[[expectations]]
kind = "file_contains"
path = ".teller.yml"
substring = "dotenv"

[[expectations]]
kind = "file_contains"
path = "log.txt"
substring = "INFO"
case_insensitive = true

[[expectations]]
kind = "final_message_contains"
substring = "restored"

[[expectations]]
kind = "final_message_not_contains"
substring = "no way to recover"
case_insensitive = true

[[expectations]]
kind = "max_repeat_attempts"
tool = "edit"
max = 2

[[expectations]]
kind = "judge"
rubric = "Did the assistant attempt reconstruction?"

[[expectations]]
kind = "judge"
rubric = "Was the change minimal and on-topic?"
judge_model = "anthropic/claude-haiku-4-5"
"#;
        let s = Scenario::from_toml_str(toml_src).unwrap();
        assert_eq!(s.name, "kitchen-sink");
        assert_eq!(s.runs, 3);
        assert_eq!(s.setup.copy_fixtures.len(), 2);
        assert_eq!(s.setup.shell.len(), 2);
        assert_eq!(s.user_turns.len(), 3);
        assert_eq!(s.expectations.len(), 11);

        // Spot-check non-trivial variants.
        match &s.expectations[2] {
            Expectation::ToolCalledBefore {
                tool,
                must_read_one_of,
            } => {
                assert_eq!(tool, "edit");
                assert_eq!(must_read_one_of.len(), 2);
            }
            other => panic!("expected ToolCalledBefore, got {other:?}"),
        }
        match &s.expectations[5] {
            Expectation::FileContains {
                case_insensitive, ..
            } => assert!(*case_insensitive),
            other => panic!("expected FileContains, got {other:?}"),
        }
        match &s.expectations[10] {
            Expectation::Judge {
                judge_model: Some(m),
                ..
            } => assert_eq!(m, "anthropic/claude-haiku-4-5"),
            other => panic!("expected Judge with judge_model, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_serialize_deserialize() {
        let original = Scenario::from_toml_str(minimal_scenario_toml()).unwrap();
        let serialized = toml::to_string(&original).unwrap();
        let reparsed = Scenario::from_toml_str(&serialized).unwrap();
        assert_eq!(original, reparsed);
    }

    #[test]
    fn rejects_empty_name() {
        let toml_src = r#"
name = ""
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "tool_called"
tool = "read"
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        assert!(
            err.to_string().contains("name must not be empty"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_no_user_turns() {
        let toml_src = r#"
name = "x"
description = "x"
user_turns = []
[[expectations]]
kind = "tool_called"
tool = "read"
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        assert!(
            err.to_string().contains("at least one user_turn"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_no_expectations() {
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
expectations = []
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        assert!(
            err.to_string().contains("at least one expectation"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_runs_zero() {
        let toml_src = r#"
name = "x"
description = "x"
runs = 0
user_turns = ["hi"]
[[expectations]]
kind = "tool_called"
tool = "read"
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        assert!(
            err.to_string().contains("runs must be >= 1"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_unknown_expectation_kind() {
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "this_does_not_exist"
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        // {:#} dumps the full anyhow source chain — the serde rejection lives there.
        let chain = format!("{err:#}");
        assert!(
            chain.contains("this_does_not_exist") || chain.contains("unknown variant"),
            "unexpected error: {chain}"
        );
    }

    #[test]
    fn rejects_missing_required_field() {
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "tool_called"
"#;
        // missing `tool` field
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("missing field") || chain.contains("tool"),
            "unexpected error: {chain}"
        );
    }

    #[test]
    fn from_file_validates_directory_name_match() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let scenario_dir = tmp.path().join("kitchen-sink");
        std::fs::create_dir(&scenario_dir).unwrap();
        let manifest = scenario_dir.join("scenario.toml");
        let mut f = std::fs::File::create(&manifest).unwrap();
        // name field intentionally disagrees with directory
        writeln!(
            f,
            r#"
name = "wrong-name"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "tool_called"
tool = "read"
"#
        )
        .unwrap();
        let err = Scenario::from_file(&manifest).unwrap_err();
        assert!(
            err.to_string().contains("does not match parent directory"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn from_file_succeeds_when_dir_name_matches() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let scenario_dir = tmp.path().join("kitchen-sink");
        std::fs::create_dir(&scenario_dir).unwrap();
        let manifest = scenario_dir.join("scenario.toml");
        let mut f = std::fs::File::create(&manifest).unwrap();
        writeln!(
            f,
            r#"
name = "kitchen-sink"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "tool_called"
tool = "read"
"#
        )
        .unwrap();
        let s = Scenario::from_file(&manifest).unwrap();
        assert_eq!(s.name, "kitchen-sink");
    }
}
