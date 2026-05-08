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
//! Substring (not regex) keeps `regex` out of the dep tree; behavioral checks
//! go through the `Judge` variant.

use std::{
    num::NonZeroUsize,
    path::{Component, Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;

use crate::tool::ToolName;

/// `deny_unknown_fields` makes typos like `case_insenstive` or `judge_modle`
/// hard errors at parse time instead of silently parsing as default — for a
/// regression-manifest format, silent acceptance defeats the entire point.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// Must match the scenario directory name; mismatch is treated as rename drift.
    pub name: String,
    pub description: String,
    /// Pass criterion across multi-run scenarios is `>= ceil(runs / 2)` passes.
    #[serde(default = "default_runs")]
    pub runs: NonZeroUsize,
    #[serde(default)]
    pub setup: Setup,
    /// First entry is the initial prompt; rest are FIFO follow-ups, one per
    /// completed assistant response. v1 has no trigger-based scheduling.
    pub user_turns: Vec<String>,
    pub expectations: Vec<Expectation>,
    /// Default judge model for every `Judge` expectation in this scenario,
    /// in `provider/model_id` format. Per-expectation `judge_model` overrides
    /// this; the `--judge-model` CLI flag overrides both. When none of the
    /// three sources is set, Judge expectations fail loudly — there is no
    /// hardcoded default.
    #[serde(default)]
    pub judge_model: Option<String>,
}

fn default_runs() -> NonZeroUsize {
    // Per spec: "Default 3, per-scenario override allowed." Multi-run
    // is the new norm for the paired-comparison pivot; the existing
    // Phase-5 `steve eval <scenario.toml>` path (transitional until
    // Phase 8 retires it) forces runs = 1 internally via cli::run_one,
    // so this default only fires through the new `eval run` subcommand.
    NonZeroUsize::new(3).expect("3 != 0")
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Setup {
    /// Paths relative to the scenario directory; copied into the tempdir
    /// preserving their relative directory structure.
    #[serde(default)]
    pub copy_fixtures: Vec<PathBuf>,
    /// Run inside the tempdir, in order, AFTER `copy_fixtures` is applied.
    #[serde(default)]
    pub shell: Vec<String>,
}

/// Tagged enum: TOML authors write `kind = "tool_called"` (snake_case) to select
/// a variant. Unknown kinds are rejected at parse time by serde, and
/// `deny_unknown_fields` rejects typos in variant fields too.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Expectation {
    ToolCalled {
        #[serde(deserialize_with = "deserialize_tool_name")]
        tool: ToolName,
    },
    ToolNotCalled {
        #[serde(deserialize_with = "deserialize_tool_name")]
        tool: ToolName,
    },
    /// Asserts that a "read-class" call against one of `must_read_one_of`
    /// preceded the first invocation of `tool`. The exact set of read-class
    /// tools is implementation-defined by the evaluator (see expectations.rs).
    RequiresPriorRead {
        #[serde(deserialize_with = "deserialize_tool_name")]
        tool: ToolName,
        must_read_one_of: Vec<PathBuf>,
    },
    FileUnchanged {
        path: PathBuf,
    },
    FileContains {
        path: PathBuf,
        substring: String,
        #[serde(default)]
        case_insensitive: bool,
    },
    FinalMessageContains {
        substring: String,
        #[serde(default)]
        case_insensitive: bool,
    },
    /// The "surrender" check — assert the assistant did not give up
    /// (e.g., emit "no way to recover").
    FinalMessageNotContains {
        substring: String,
        #[serde(default)]
        case_insensitive: bool,
    },
    /// Asserts that no `(tool, arguments)` pair appears more than `max` times
    /// in the captured tool-call sequence. Argument equality is structural —
    /// key ordering doesn't matter — so two calls with the same fields in
    /// different orderings count as one repeat.
    MaxRepeatAttempts {
        #[serde(deserialize_with = "deserialize_tool_name")]
        tool: ToolName,
        max: usize,
    },
    /// LLM-as-judge expectation. The judge model is configured at the eval
    /// level; `judge_model` (when set) overrides it for this expectation.
    /// Phase 4 (steve-bh3r) implements the actual evaluation; until then,
    /// the evaluator returns `Skipped` for every Judge expectation.
    ///
    /// `pass_when` and `fail_when` are separate fields (not a single freeform
    /// rubric) so the judge prompt template can construct a structured prompt
    /// and so authors don't have to remember a PASS=/FAIL= convention.
    Judge {
        pass_when: String,
        fail_when: String,
        /// Format: `provider/model_id`.
        #[serde(default)]
        judge_model: Option<String>,
    },
}

/// Custom deserializer for `Expectation` `tool` fields. Reads a string and
/// maps it to a `ToolName` variant, but layers four pre-checks that produce
/// friendlier errors than the bare `ToolName::from_str` failure (a strum
/// `ParseError` with no surrounding context) — and crucially, that catch
/// failure modes which would otherwise let a misconfigured scenario report
/// green forever:
///
/// 1. Empty string — surface "must not be empty" instead of letting the
///    strum parse fail with a bare "Matching variant not found".
/// 2. Leading/trailing whitespace — `" read"` parses fine as a String but
///    can never match any variant; without this guard the operator would
///    see only the strum ParseError, hiding the whitespace as the real bug.
/// 3. MCP-shaped names (containing `__`) — capture cannot observe MCP
///    calls (`execute_mcp_tools` emits `StreamNotice`, not `LlmToolCall`,
///    and `RecordedToolCall.tool_name` is `ToolName`, which has no MCP
///    variant), so an assertion on an MCP tool would silently never match.
///    Point at the tracking issue (steve-ap0q) instead of just rejecting.
/// 4. Unknown builtin — surface the full known-variant list in the message
///    so the operator can see what they meant to type.
fn deserialize_tool_name<'de, D>(deserializer: D) -> std::result::Result<ToolName, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    if raw.is_empty() {
        return Err(serde::de::Error::custom("tool name must not be empty"));
    }
    if raw != raw.trim() || raw.trim().is_empty() {
        return Err(serde::de::Error::custom(format!(
            "tool {raw:?} must not have leading/trailing whitespace; \
             tool names are matched exactly against the builtin enum"
        )));
    }
    if raw.contains("__") {
        return Err(serde::de::Error::custom(format!(
            "tool {raw:?} looks like an MCP tool name, but MCP capture is not yet \
             implemented — assertions on MCP calls would never succeed. \
             Tracked: steve-ap0q. Use a builtin tool name instead."
        )));
    }
    ToolName::from_str(&raw).map_err(|_| {
        let known: Vec<&'static str> = ToolName::iter().map(|t| t.as_str()).collect();
        serde::de::Error::custom(format!(
            "tool {raw:?} is not a known builtin (one of {known:?})"
        ))
    })
}

/// Reject paths that escape the scenario workspace, contain garbage that
/// will fail at the OS layer with useless EINVAL, or vary in spelling from
/// the canonical workspace-relative form (which would prevent baseline
/// lookups from matching). Symlink resolution is a runtime concern; this
/// is the parse-time gate.
fn validate_workspace_relative_path(path: &Path, label: &str) -> Result<()> {
    if path.is_absolute() {
        anyhow::bail!("{label} {path:?} must be relative to the scenario dir (got absolute path)");
    }
    // Check for NUL bytes (and other non-printable garbage) by round-tripping
    // through the bytes representation. A NUL byte slips past `is_absolute`
    // and `.components()` checks but is rejected by syscalls with a useless
    // EINVAL — fail at parse with a clear reason instead.
    let lossy = path.to_string_lossy();
    if lossy.bytes().any(|b| b == 0) {
        anyhow::bail!("{label} {path:?} must not contain NUL bytes");
    }
    for component in path.components() {
        match component {
            Component::ParentDir => anyhow::bail!(
                "{label} {path:?} must not contain `..` segments (would escape workspace)"
            ),
            Component::CurDir => anyhow::bail!(
                "{label} {path:?} must not contain `.` segments — write the path canonically \
                 (e.g. `foo/bar`, not `./foo/bar`); baseline lookups are key-equality and a \
                 leading `./` would never match"
            ),
            Component::Normal(_) => {}
            Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("{label} {path:?} must not contain a root or prefix component")
            }
        }
    }
    Ok(())
}

impl Scenario {
    pub fn from_file(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading scenario manifest at {}", path.display()))?;
        let scenario: Scenario = toml::from_str(&raw)
            .with_context(|| format!("parsing scenario manifest at {}", path.display()))?;
        scenario.validate()?;
        // Parent-directory match is a filesystem-only invariant; not part of
        // `validate()` so the Phase 7 generator can validate without a path.
        let parent_name = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str());
        if let Some(dir_name) = parent_name
            && dir_name != scenario.name
        {
            anyhow::bail!(
                "scenario name {:?} does not match parent directory {:?} ({})",
                scenario.name,
                dir_name,
                path.display()
            );
        }
        Ok(scenario)
    }

    /// Skips the parent-directory match check — useful for in-memory scenarios
    /// (tests, the Phase 7 debug-export generator).
    pub fn from_toml_str(toml_src: &str) -> Result<Self> {
        let scenario: Scenario =
            toml::from_str(toml_src).context("parsing scenario manifest from string")?;
        scenario.validate()?;
        Ok(scenario)
    }

    /// Self-consistency checks that don't depend on filesystem state. Public so
    /// the Phase 7 generator can run validation after struct-literal construction.
    pub fn validate(&self) -> Result<()> {
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
        for fixture in &self.setup.copy_fixtures {
            validate_workspace_relative_path(fixture, "setup.copy_fixtures path")
                .with_context(|| format!("scenario {}", self.name))?;
        }
        for (idx, expectation) in self.expectations.iter().enumerate() {
            expectation
                .validate()
                .with_context(|| format!("scenario {} expectation #{}", self.name, idx + 1))?;
        }
        Ok(())
    }
}

impl Expectation {
    fn validate(&self) -> Result<()> {
        match self {
            // tool: ToolName is enforced by serde at parse time via
            // deserialize_tool_name; nothing left to check on these arms.
            Self::ToolCalled { .. } | Self::ToolNotCalled { .. } => {}
            Self::RequiresPriorRead {
                tool: _,
                must_read_one_of,
            } => {
                if must_read_one_of.is_empty() {
                    anyhow::bail!("must_read_one_of must contain at least one path");
                }
                for p in must_read_one_of {
                    validate_workspace_relative_path(p, "must_read_one_of path")?;
                }
            }
            Self::FileUnchanged { path } => {
                validate_workspace_relative_path(path, "file_unchanged path")?;
            }
            Self::FileContains {
                path, substring, ..
            } => {
                validate_workspace_relative_path(path, "file_contains path")?;
                if substring.is_empty() {
                    anyhow::bail!("file_contains substring must not be empty");
                }
            }
            Self::FinalMessageContains { substring, .. }
            | Self::FinalMessageNotContains { substring, .. } => {
                if substring.is_empty() {
                    anyhow::bail!("final_message substring must not be empty");
                }
            }
            Self::MaxRepeatAttempts { tool: _, max } => {
                if *max == 0 {
                    anyhow::bail!(
                        "max_repeat_attempts max must be >= 1 (use tool_not_called for max=0 semantics)"
                    );
                }
            }
            Self::Judge {
                pass_when,
                fail_when,
                ..
            } => {
                if pass_when.trim().is_empty() {
                    anyhow::bail!("judge pass_when must not be empty");
                }
                if fail_when.trim().is_empty() {
                    anyhow::bail!("judge fail_when must not be empty");
                }
            }
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

    fn kitchen_sink_toml() -> &'static str {
        r#"
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
kind = "requires_prior_read"
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
pass_when = "Assistant attempted reconstruction using available tools and adjacent files."
fail_when = "Assistant emitted surrender language like 'no way to recover' or asked the user to provide content from memory."

[[expectations]]
kind = "judge"
pass_when = "Change is minimal and on-topic."
fail_when = "Unrelated files were touched."
judge_model = "anthropic/claude-haiku-4-5"
"#
    }

    #[test]
    fn parse_minimal_scenario() {
        let s = Scenario::from_toml_str(minimal_scenario_toml()).unwrap();
        assert_eq!(s.name, "minimal");
        assert_eq!(s.description, "smallest valid scenario");
        assert_eq!(
            s.runs.get(),
            3,
            "default runs = 3 when omitted (Phase 6: multi-run is the norm)"
        );
        assert!(s.setup.copy_fixtures.is_empty());
        assert!(s.setup.shell.is_empty());
        assert_eq!(s.user_turns, vec!["hello"]);
        assert_eq!(s.expectations.len(), 1);
        assert!(matches!(
            s.expectations[0],
            Expectation::ToolCalled {
                tool: ToolName::Read
            }
        ));
    }

    #[test]
    fn parse_full_scenario_with_all_expectation_kinds() {
        let s = Scenario::from_toml_str(kitchen_sink_toml()).unwrap();
        assert_eq!(s.name, "kitchen-sink");
        assert_eq!(s.runs.get(), 3);
        assert_eq!(s.setup.copy_fixtures.len(), 2);
        assert_eq!(s.setup.shell.len(), 2);
        assert_eq!(s.user_turns.len(), 3);
        assert_eq!(s.expectations.len(), 11);

        // Spot-check non-trivial variants — order matters because the parser is
        // expected to preserve TOML array order.
        match &s.expectations[2] {
            Expectation::RequiresPriorRead {
                tool,
                must_read_one_of,
            } => {
                assert_eq!(*tool, ToolName::Edit);
                assert_eq!(must_read_one_of.len(), 2);
            }
            other => panic!("expected RequiresPriorRead, got {other:?}"),
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
    fn round_trip_minimal_scenario() {
        let original = Scenario::from_toml_str(minimal_scenario_toml()).unwrap();
        let serialized = toml::to_string(&original).unwrap();
        let reparsed = Scenario::from_toml_str(&serialized).unwrap();
        assert_eq!(original, reparsed);
    }

    #[test]
    fn round_trip_kitchen_sink_scenario() {
        // Phase 7 generator emits TOML — round-tripping the full variant set
        // is the only thing standing between it and silently dropping fields.
        let original = Scenario::from_toml_str(kitchen_sink_toml()).unwrap();
        let serialized = toml::to_string(&original).unwrap();
        let reparsed = Scenario::from_toml_str(&serialized).unwrap();
        assert_eq!(original, reparsed);
    }

    #[test]
    fn scenario_level_judge_model_round_trips() {
        // Scenario-level `judge_model` is the middle tier of the Phase 4
        // resolution chain (CLI > per-expectation > scenario > fail). Pin
        // that it parses, round-trips, and survives in serialized TOML.
        let toml_src = r#"
name = "judge-model-pinned"
description = "scenario pins a judge model for all judges"
user_turns = ["go"]
judge_model = "fuel-ix/claude-haiku-4-5"

[[expectations]]
kind = "judge"
pass_when = "did the right thing"
fail_when = "gave up"
"#;
        let s = Scenario::from_toml_str(toml_src).unwrap();
        assert_eq!(s.judge_model.as_deref(), Some("fuel-ix/claude-haiku-4-5"));
        let serialized = toml::to_string(&s).unwrap();
        assert!(
            serialized.contains("judge_model = \"fuel-ix/claude-haiku-4-5\""),
            "serialized TOML must include judge_model: {serialized}"
        );
        let reparsed = Scenario::from_toml_str(&serialized).unwrap();
        assert_eq!(s, reparsed);
    }

    #[test]
    fn scenario_judge_model_omitted_defaults_to_none() {
        let s = Scenario::from_toml_str(minimal_scenario_toml()).unwrap();
        assert!(
            s.judge_model.is_none(),
            "no scenario-level judge_model means None — not a hardcoded default"
        );
    }

    #[test]
    fn setup_omitted_equals_setup_explicit_empty() {
        // Both forms must produce an equivalent Setup so the Phase 2 runner
        // doesn't branch on author style.
        let omitted = Scenario::from_toml_str(minimal_scenario_toml()).unwrap();
        let explicit = Scenario::from_toml_str(
            r#"
name = "minimal"
description = "smallest valid scenario"
user_turns = ["hello"]

[setup]
copy_fixtures = []
shell = []

[[expectations]]
kind = "tool_called"
tool = "read"
"#,
        )
        .unwrap();
        assert_eq!(omitted.setup, explicit.setup);
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
    fn rejects_whitespace_only_name() {
        let toml_src = r#"
name = "   "
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
        // NonZeroUsize rejects 0 at the deserialization layer — the error
        // surfaces in the toml::from_str step, not our validate().
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("zero") || chain.contains("0") || chain.contains("non-zero"),
            "unexpected error: {chain}"
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
        // {:#} dumps the full anyhow source chain — without it, only the outer
        // context "parsing scenario manifest from string" shows up.
        let chain = format!("{err:#}");
        assert!(
            chain.contains("unknown variant"),
            "expected 'unknown variant' in chain: {chain}"
        );
        assert!(
            chain.contains("this_does_not_exist"),
            "expected unknown-variant name in chain: {chain}"
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
        // missing `tool` field on ToolCalled
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("missing field") && chain.contains("tool"),
            "expected both 'missing field' and 'tool' in chain: {chain}"
        );
    }

    #[test]
    fn rejects_max_repeat_attempts_zero() {
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "max_repeat_attempts"
tool = "edit"
max = 0
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(chain.contains("must be >= 1"), "unexpected error: {chain}");
    }

    #[test]
    fn rejects_empty_must_read_one_of() {
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "requires_prior_read"
tool = "edit"
must_read_one_of = []
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("must contain at least one path"),
            "unexpected error: {chain}"
        );
    }

    #[test]
    fn rejects_empty_tool_name() {
        // The empty-string branch in deserialize_tool_name is the friendliest
        // landing for a TOML author who left `tool = ""` mid-edit; without
        // this guard they'd see strum's bare "Matching variant not found"
        // and have to guess what the empty input means.
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "tool_called"
tool = ""
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("must not be empty"),
            "unexpected error: {chain}"
        );
    }

    #[test]
    fn rejects_typoed_tool_name_in_tool_called() {
        // A misspelled tool name (`raed` for `read`) would silently never
        // match any tool call, vacuously passing tool_not_called and
        // requires_prior_read while reporting green forever. Catch at parse.
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "tool_called"
tool = "raed"
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("not a known builtin") && chain.contains("raed"),
            "unexpected error: {chain}"
        );
    }

    #[test]
    fn rejects_typoed_tool_name_in_max_repeat_attempts() {
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "max_repeat_attempts"
tool = "edti"
max = 2
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("not a known builtin") && chain.contains("edti"),
            "unexpected error: {chain}"
        );
    }

    #[test]
    fn accepts_all_builtin_tool_names() {
        // Sanity: every ToolName variant should round-trip through the
        // validator. Iterates the strum enum so adding a new variant is
        // automatically covered.
        use crate::tool::ToolName;
        use strum::IntoEnumIterator;
        for name in ToolName::iter() {
            let toml_src = format!(
                r#"
name = "x"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "tool_called"
tool = "{}"
"#,
                name.as_str()
            );
            Scenario::from_toml_str(&toml_src)
                .unwrap_or_else(|e| panic!("builtin {} rejected: {e:#}", name.as_str()));
        }
    }

    #[test]
    fn rejects_tool_name_with_leading_or_trailing_whitespace() {
        // ToolName::from_str(name.trim()) succeeds for " read ", but the
        // scenario stores the un-trimmed string and the runtime comparison
        // against ToolName::as_str() ("read") would never match — silent
        // false negative. Reject at parse.
        for bad in [" read", "read ", "  edit  ", "\tbash"] {
            let toml_src = format!(
                r#"
name = "x"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "tool_called"
tool = "{bad}"
"#
            );
            let err = Scenario::from_toml_str(&toml_src).unwrap_err();
            let chain = format!("{err:#}");
            assert!(
                chain.contains("must not have leading/trailing whitespace"),
                "expected whitespace rejection for {bad:?}: {chain}"
            );
        }
    }

    #[test]
    fn rejects_mcp_tool_name_until_capture_supports_it() {
        // MCP tool names parse "looking valid by convention" but capture
        // can't see MCP calls (execute_mcp_tools emits StreamNotice, not
        // LlmToolCall) — so the assertion would silently pass forever.
        // Reject at parse with a pointer to the tracking issue.
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "tool_called"
tool = "mcp__github__create_issue"
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("MCP capture is not yet implemented") && chain.contains("steve-ap0q"),
            "unexpected error: {chain}"
        );
    }

    #[test]
    fn rejects_empty_substring_in_file_contains() {
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "file_contains"
path = "foo"
substring = ""
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("substring must not be empty"),
            "unexpected error: {chain}"
        );
    }

    #[test]
    fn rejects_empty_judge_pass_when() {
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "judge"
pass_when = "   "
fail_when = "concrete fail"
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("pass_when must not be empty"),
            "unexpected error: {chain}"
        );
    }

    #[test]
    fn rejects_empty_judge_fail_when() {
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "judge"
pass_when = "concrete pass"
fail_when = ""
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("fail_when must not be empty"),
            "unexpected error: {chain}"
        );
    }

    #[test]
    fn rejects_unknown_field_typo_in_file_contains() {
        // `case_insenstive` (typo) should be rejected, not silently default.
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "file_contains"
path = "foo"
substring = "bar"
case_insenstive = true
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("unknown field") && chain.contains("case_insenstive"),
            "expected serde 'unknown field' rejection: {chain}"
        );
    }

    #[test]
    fn rejects_unknown_field_typo_at_scenario_level() {
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
runss = 3
[[expectations]]
kind = "tool_called"
tool = "read"
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("unknown field") && chain.contains("runss"),
            "expected serde 'unknown field' rejection: {chain}"
        );
    }

    #[test]
    fn rejects_dot_segment_in_copy_fixtures() {
        // `./foo` slips past `..` and absolute checks but reads as
        // [CurDir, Normal("foo")] in components — different from the
        // baseline's `Normal("foo")` key, so file_unchanged would silently
        // miss-match. Reject at parse time and tell authors to write
        // canonically.
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[setup]
copy_fixtures = ["./foo"]
[[expectations]]
kind = "tool_called"
tool = "read"
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("must not contain `.` segments"),
            "expected `./` rejection: {chain}"
        );
    }

    #[test]
    fn rejects_nul_byte_in_path() {
        // NUL slips past every check that uses Path components but is
        // rejected by syscalls with EINVAL. Catch at parse with a clear
        // reason instead of a useless runtime error.
        let toml_src = "
name = \"x\"
description = \"x\"
user_turns = [\"hi\"]
[[expectations]]
kind = \"file_unchanged\"
path = \"foo\\u0000bar\"
";
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("must not contain NUL bytes"),
            "expected NUL-byte rejection: {chain}"
        );
    }

    #[test]
    fn validate_reports_failing_expectation_index() {
        // The validator wraps per-expectation errors with `expectation #N`
        // — pin that contract so a future refactor that drops the index
        // wrapping breaks loudly. Build a scenario with three expectations
        // where the THIRD is malformed.
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]

[[expectations]]
kind = "tool_called"
tool = "read"

[[expectations]]
kind = "tool_called"
tool = "edit"

[[expectations]]
kind = "max_repeat_attempts"
tool = "edit"
max = 0
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("expectation #3"),
            "must surface failing expectation index: {chain}"
        );
        assert!(
            chain.contains("must be >= 1"),
            "must surface inner cause: {chain}"
        );
    }

    #[test]
    fn rejects_parent_dir_in_copy_fixtures() {
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[setup]
copy_fixtures = ["../../etc/passwd"]
[[expectations]]
kind = "tool_called"
tool = "read"
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("must not contain `..`"),
            "expected `..` rejection: {chain}"
        );
    }

    #[test]
    fn rejects_absolute_path_in_must_read_one_of() {
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "requires_prior_read"
tool = "edit"
must_read_one_of = ["/etc/passwd"]
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("must be relative"),
            "expected absolute-path rejection: {chain}"
        );
    }

    #[test]
    fn rejects_parent_dir_in_file_contains_path() {
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "file_contains"
path = "../outside"
substring = "x"
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("must not contain `..`"),
            "expected `..` rejection: {chain}"
        );
    }

    #[test]
    fn rejects_absolute_path_in_file_unchanged() {
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[[expectations]]
kind = "file_unchanged"
path = "/etc/hosts"
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("must be relative"),
            "expected absolute-path rejection: {chain}"
        );
    }

    #[test]
    fn rejects_absolute_copy_fixture_path() {
        let toml_src = r#"
name = "x"
description = "x"
user_turns = ["hi"]
[setup]
copy_fixtures = ["/etc/passwd"]
[[expectations]]
kind = "tool_called"
tool = "read"
"#;
        let err = Scenario::from_toml_str(toml_src).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("must be relative"),
            "unexpected error: {chain}"
        );
    }

    #[test]
    fn all_committed_scenarios_parse_and_validate() {
        // Walk every directory under `eval/scenarios/` (relative to the
        // crate root) and round-trip each `scenario.toml` through
        // `Scenario::from_file`. Catches authoring typos — wrong field
        // names, unknown tool variants, malformed must_read paths,
        // typo'd manifest filenames, and missing fixture files — at
        // `cargo test` time so authors don't have to spend an LLM-bound
        // smoke run to find them.
        let scenarios_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/scenarios");
        let entries =
            std::fs::read_dir(&scenarios_dir).expect("eval/scenarios/ should exist alongside src/");
        let mut parsed = Vec::new();
        for entry in entries {
            let entry = entry.expect("readable directory entry");
            let path = entry.path();
            // Use file_type() (does NOT follow symlinks) rather than
            // path.is_dir() (follows symlinks). A symlinked entry could
            // otherwise let the test traverse outside the repo, and
            // ScenarioWorkspace::build itself rejects symlink fixtures —
            // mirror that defensive posture here.
            let file_type = entry
                .file_type()
                .unwrap_or_else(|err| panic!("could not stat {}: {err:#}", path.display()));
            if file_type.is_symlink() {
                panic!(
                    "symlink entry {} under eval/scenarios/ is not allowed — the workspace builder rejects symlinks and the walking test must not silently traverse them",
                    path.display()
                );
            }
            if !file_type.is_dir() {
                continue;
            }
            let manifest = path.join("scenario.toml");
            // A directory under eval/scenarios/ without a manifest is
            // never legitimate (no cases like "fixtures dir at the top
            // level" exist). A typo'd `senario.toml` would otherwise
            // silently skip and the author wouldn't notice. Use
            // symlink_metadata (does NOT follow symlinks) so a
            // symlinked manifest doesn't slip past the top-level
            // symlink defense.
            let manifest_meta = std::fs::symlink_metadata(&manifest).unwrap_or_else(|_| {
                panic!(
                    "scenario directory {} is missing manifest at {} — likely a typo'd manifest filename",
                    path.display(),
                    manifest.display()
                )
            });
            assert!(
                manifest_meta.file_type().is_file(),
                "scenario manifest {} must be a regular file (got {}) — symlinked or directory manifests are not allowed",
                manifest.display(),
                describe_file_type(manifest_meta.file_type())
            );
            let scenario = Scenario::from_file(&manifest).unwrap_or_else(|err| {
                panic!(
                    "scenario manifest {} failed to parse: {err:#}",
                    manifest.display()
                )
            });
            // Verify each fixture file actually exists AND is a regular
            // file (not a directory or symlink). This mirrors
            // ScenarioWorkspace::build, which uses std::fs::copy (fails
            // on directories) and explicitly rejects symlinks. Catching
            // these mismatches here saves an LLM-bound smoke run.
            for fixture in &scenario.setup.copy_fixtures {
                let resolved = path.join(fixture);
                let meta = std::fs::symlink_metadata(&resolved).unwrap_or_else(|err| {
                    panic!(
                        "scenario {}: copy_fixtures entry {} not found at {} ({err})",
                        scenario.name,
                        fixture.display(),
                        resolved.display()
                    )
                });
                assert!(
                    meta.file_type().is_file(),
                    "scenario {}: copy_fixtures entry {} at {} must be a regular file (got {})",
                    scenario.name,
                    fixture.display(),
                    resolved.display(),
                    describe_file_type(meta.file_type())
                );
            }
            parsed.push(scenario.name);
        }
        assert!(
            !parsed.is_empty(),
            "expected at least one scenario under {}",
            scenarios_dir.display()
        );
        // _smoke is the canonical baseline scenario invoked by the
        // manual smoke run (`cargo run -- eval
        // eval/scenarios/_smoke/scenario.toml`). It's a developer
        // convention, not a hardcoded code path — but pinning its
        // presence here catches an accidental delete or rename that
        // the bare !is_empty() guard would miss.
        assert!(
            parsed.iter().any(|n| n == "_smoke"),
            "_smoke scenario missing from {}; parsed scenarios: {parsed:?}",
            scenarios_dir.display()
        );
        // VALIDATION.md tracks per-scenario FAIL-then-PASS validation
        // results for the modified scenarios. Pin its presence so a
        // rename/delete trips the test rather than silently losing the
        // validation history. Not a structural check — just existence.
        let validation_md =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/VALIDATION.md");
        assert!(
            validation_md.is_file(),
            "eval/VALIDATION.md missing (or not a regular file) at {}",
            validation_md.display()
        );
    }

    /// Render a `FileType` in human-readable form for panic messages.
    /// `FileType`'s `Debug` impl on Unix prints raw `st_mode` bits
    /// (`FileType { mode: 0o040755 }`), which doesn't help an author
    /// diagnose "why is the walking test panicking?"
    fn describe_file_type(ft: std::fs::FileType) -> &'static str {
        if ft.is_dir() {
            "directory"
        } else if ft.is_symlink() {
            "symlink"
        } else if ft.is_file() {
            "regular file"
        } else {
            "non-regular file (fifo/socket/device)"
        }
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
