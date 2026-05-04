//! Rule-based assertion evaluator.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    eval::{
        capture::{CapturedRun, RecordedToolCall},
        scenario::{Expectation, Scenario},
    },
    permission::normalize_tool_path,
    tool::ToolName,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub results: Vec<ExpectationResult>,
}

impl EvalReport {
    /// Skipped is neutral: the report passes iff no expectation Failed.
    /// Unimplemented checks (judge in v1) produce Skipped so a green report
    /// doesn't silently mean "no real checks ran."
    pub fn passed(&self) -> bool {
        !self.results.iter().any(|r| r.outcome.is_failed())
    }
}

/// One expectation's verdict, paired with the original `Expectation` for
/// self-describing output. Because `Expectation` carries `#[serde(tag = "kind")]`,
/// the JSON output includes a `kind` discriminator alongside the per-variant
/// fields, so a reader sees what was checked without consulting scenario.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectationResult {
    pub expectation: Expectation,
    pub outcome: Outcome,
}

/// Skipped is neutral: a report passes iff no expectation Failed. Skipped
/// exists for expectations a phase doesn't yet implement (e.g. Judge while
/// Phase 4 is offline).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Passed,
    Failed { reason: String },
    Skipped { reason: String },
}

impl Outcome {
    fn is_passed(&self) -> bool {
        matches!(self, Outcome::Passed)
    }
    fn is_failed(&self) -> bool {
        matches!(self, Outcome::Failed { .. })
    }
}

pub fn evaluate(scenario: &Scenario, captured: &CapturedRun) -> EvalReport {
    let results = scenario
        .expectations
        .iter()
        .map(|e| evaluate_one(e, captured))
        .collect();
    EvalReport { results }
}

fn evaluate_one(expectation: &Expectation, captured: &CapturedRun) -> ExpectationResult {
    let outcome = match expectation {
        Expectation::ToolCalled { tool } => check_tool_called(tool, captured),
        Expectation::ToolNotCalled { tool } => check_tool_not_called(tool, captured),
        Expectation::RequiresPriorRead {
            tool,
            must_read_one_of,
        } => check_requires_prior_read(tool, must_read_one_of, captured),
        Expectation::FileUnchanged { path } => check_file_unchanged(path, captured),
        Expectation::FileContains {
            path,
            substring,
            case_insensitive,
        } => check_file_contains(path, substring, *case_insensitive, captured),
        Expectation::FinalMessageContains {
            substring,
            case_insensitive,
        } => check_final_message(substring, *case_insensitive, true, captured),
        Expectation::FinalMessageNotContains {
            substring,
            case_insensitive,
        } => check_final_message(substring, *case_insensitive, false, captured),
        Expectation::MaxRepeatAttempts { tool, max } => check_max_repeat(tool, *max, captured),
        Expectation::Judge { .. } => Outcome::Skipped {
            reason: "Phase 4 (LLM-as-judge) not yet implemented".into(),
        },
    };
    ExpectationResult {
        expectation: expectation.clone(),
        outcome,
    }
}

fn check_tool_called(target: &str, captured: &CapturedRun) -> Outcome {
    if captured
        .tool_calls
        .iter()
        .any(|c| c.tool_name.as_str() == target)
    {
        return Outcome::Passed;
    }
    let actual: Vec<&str> = captured
        .tool_calls
        .iter()
        .map(|c| c.tool_name.as_str())
        .collect();
    Outcome::Failed {
        reason: format!("tool {target:?} never called; saw {actual:?}"),
    }
}

fn check_tool_not_called(target: &str, captured: &CapturedRun) -> Outcome {
    if captured
        .tool_calls
        .iter()
        .any(|c| c.tool_name.as_str() == target)
    {
        return Outcome::Failed {
            reason: format!("tool {target:?} was called at least once"),
        };
    }
    Outcome::Passed
}

fn check_requires_prior_read(
    target: &str,
    must_read_one_of: &[PathBuf],
    captured: &CapturedRun,
) -> Outcome {
    // "Before" = call-emission order. The stream emits LlmToolCall events
    // in deterministic partition order even when read-class tools execute
    // in parallel (see the partition-order loop in stream/phases.rs's
    // parallel-results section), so completion-time race conditions cannot
    // reorder this assertion.
    let Some(first_target_idx) = captured
        .tool_calls
        .iter()
        .position(|c| c.tool_name.as_str() == target)
    else {
        // If the protected tool was never called, the requirement is vacuously satisfied.
        return Outcome::Passed;
    };

    // Normalize required paths so equivalent forms (`./foo`, `src/../foo`,
    // `foo/.//bar`, absolute-inside-workspace) compare equal. `must_read_one_of`
    // paths are already validated as workspace-relative at parse time, but
    // running them through normalize_tool_path collapses any redundant
    // segments so the comparison is purely structural.
    let normalized_required: Vec<String> = must_read_one_of
        .iter()
        .map(|p| normalize_tool_path(&p.to_string_lossy(), &captured.workspace_root).0)
        .collect();

    let mut actually_read: Vec<String> = Vec::new();
    let mut outside_workspace: Vec<String> = Vec::new();

    let mut failed_reads: Vec<String> = Vec::new();

    for prior in captured.tool_calls[..first_target_idx].iter() {
        if !is_read_class(prior.tool_name) {
            continue;
        }
        // A failed read tells the agent nothing about the file content, so
        // it cannot satisfy "must have read this before editing." Track
        // separately so the failure message can call out the attempt.
        let path_arg = read_path_arg(prior, &captured.workspace_root);
        if prior.is_error {
            if let Some(PathOrigin::Inside(rel)) = path_arg {
                failed_reads.push(rel);
            } else if let Some(PathOrigin::Outside(raw)) = path_arg {
                outside_workspace.push(raw);
            }
            continue;
        }
        match path_arg {
            Some(PathOrigin::Inside(rel)) => {
                if normalized_required.iter().any(|r| r == &rel) {
                    return Outcome::Passed;
                }
                actually_read.push(rel);
            }
            Some(PathOrigin::Outside(raw)) => {
                outside_workspace.push(raw);
            }
            None => {}
        }
    }

    let required: Vec<String> = must_read_one_of
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    let mut reason = format!(
        "tool {target:?} called without first reading any of {required:?}; \
         read-class tools are read/symbols (v1); actually read: {actually_read:?}"
    );
    if !failed_reads.is_empty() {
        reason.push_str(&format!(
            "; failed (errored) read attempts: {failed_reads:?}"
        ));
    }
    if !outside_workspace.is_empty() {
        reason.push_str(&format!(
            "; ignored outside-workspace paths: {outside_workspace:?}"
        ));
    }
    Outcome::Failed { reason }
}

/// V1 read-class set: tools that load file content from a specific path
/// argument. `read` extracts the file contents directly; `symbols` parses
/// and lists structure (so the agent has at least seen the file). Search
/// tools (grep/glob/find_symbol) and structural-only tools (list/lsp) are
/// excluded — they don't reliably tell the agent what's in the file.
fn is_read_class(name: ToolName) -> bool {
    matches!(name, ToolName::Read | ToolName::Symbols)
}

// ─── Path classification ──────────────────────────────────────────────────────
//
// Two separate enums, intentionally co-located, answering DIFFERENT questions:
// `PathOrigin` classifies what the LLM emitted (no stat); `FsState` classifies
// what's actually at a workspace path right now (does stat, doesn't follow
// symlinks). Every path-touching check uses the appropriate one and
// exhaustively matches all variants — adding a new variant breaks the build
// at every call site, which is the project's "match arms over wildcards"
// safety culture (CLAUDE.md).

/// Classification of a path STRING (typically an LLM-emitted tool arg) as
/// either inside or outside the workspace, after lexical normalization.
/// No filesystem access — see `FsState` for the stat-based question.
enum PathOrigin {
    /// Workspace-relative path, lexically normalized (`..`/`.` collapsed).
    Inside(String),
    /// Path resolves outside the workspace root — surfaced separately so
    /// the failure message can flag it as a likely scenario-author bug.
    Outside(String),
}

/// Extract a tool's primary path arg, normalize it lexically, and classify
/// it as inside or outside the workspace. Reuses the project-wide
/// `normalize_tool_path` helper so the eval evaluator stays in lockstep
/// with the permission system's path semantics.
fn read_path_arg(call: &RecordedToolCall, workspace_root: &Path) -> Option<PathOrigin> {
    let key = *call.tool_name.path_arg_keys().first()?;
    let raw = call.arguments.get(key)?.as_str()?;
    let (normalized, inside) = normalize_tool_path(raw, workspace_root);
    if inside {
        Some(PathOrigin::Inside(normalized))
    } else {
        Some(PathOrigin::Outside(raw.to_string()))
    }
}

/// Classification of an on-disk path WITHOUT following symlinks. The
/// baseline snapshot only tracks regular files (its walk skips symlinks
/// via `is_symlink()`), so the evaluator must mirror that by classifying
/// via `symlink_metadata` rather than `is_file`/`exists` — both of which
/// dereference symlinks and would let scenarios escape the workspace via
/// a planted symlink.
enum FsState {
    Absent,
    RegularFile,
    /// Exists but isn't a regular file. `kind` discriminates for messages.
    NonFile(NonFileKind),
    /// `symlink_metadata` failed for a reason other than NotFound — most
    /// commonly permission denied (e.g. setup script chmod'd a directory).
    /// Surfaced separately so the failure message names the real cause
    /// instead of misleading the operator with "deleted" / "does not exist."
    MetadataError(std::io::Error),
}

#[derive(Debug)]
enum NonFileKind {
    Directory,
    Symlink,
    /// FIFO, device node, socket, etc.
    Other,
}

impl std::fmt::Display for NonFileKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            NonFileKind::Directory => "directory",
            NonFileKind::Symlink => "symlink",
            NonFileKind::Other => "non-regular file",
        })
    }
}

fn classify_fs(abs: &Path) -> FsState {
    match std::fs::symlink_metadata(abs) {
        Ok(m) if m.file_type().is_file() => FsState::RegularFile,
        Ok(m) if m.file_type().is_dir() => FsState::NonFile(NonFileKind::Directory),
        Ok(m) if m.file_type().is_symlink() => FsState::NonFile(NonFileKind::Symlink),
        Ok(_) => FsState::NonFile(NonFileKind::Other),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => FsState::Absent,
        Err(e) => FsState::MetadataError(e),
    }
}

/// Maximum bytes `check_file_contains` will read from a single file. Caps
/// adversarial-file scenarios where the agent (or fixture) creates a
/// multi-GB file at the assertion path; without this, `std::fs::read`
/// would OOM-kill the eval process and the operator would see a SIGKILL
/// instead of a structured `Failed` outcome.
const MAX_FILE_CONTAINS_BYTES: u64 = 16 * 1024 * 1024;

fn check_file_unchanged(path: &Path, captured: &CapturedRun) -> Outcome {
    let abs = captured.workspace_root.join(path);
    let baseline = captured.baseline.files.get(path);

    match (baseline, classify_fs(&abs)) {
        (_, FsState::MetadataError(e)) => Outcome::Failed {
            reason: format!("could not stat {}: {e}", path.display()),
        },
        (Some(baseline_hash), FsState::RegularFile) => match std::fs::read(&abs) {
            Ok(current) => {
                let current_hash: [u8; 32] = Sha256::digest(&current).into();
                if current_hash == *baseline_hash {
                    Outcome::Passed
                } else {
                    Outcome::Failed {
                        reason: format!("{} content changed", path.display()),
                    }
                }
            }
            Err(e) => Outcome::Failed {
                reason: format!("could not read {}: {e}", path.display()),
            },
        },
        (Some(_), FsState::NonFile(kind)) => Outcome::Failed {
            reason: format!(
                "{} was replaced by a {kind}; baseline expected a regular file",
                path.display()
            ),
        },
        (Some(_), FsState::Absent) => Outcome::Failed {
            reason: format!("{} was deleted (present in baseline)", path.display()),
        },
        (None, FsState::Absent) => Outcome::Passed,
        (None, FsState::RegularFile) => Outcome::Failed {
            reason: format!("{} was created (not present in baseline)", path.display()),
        },
        (None, FsState::NonFile(kind)) => Outcome::Failed {
            reason: format!(
                "{} was created as a {kind}; baseline expected nothing at this path",
                path.display()
            ),
        },
    }
}

fn check_file_contains(
    path: &Path,
    substring: &str,
    case_insensitive: bool,
    captured: &CapturedRun,
) -> Outcome {
    let abs = captured.workspace_root.join(path);
    // Refuse to follow symlinks — `std::fs::read` would dereference them
    // and let scenarios assert against host-filesystem content outside
    // the workspace by planting a symlink at `path`.
    match classify_fs(&abs) {
        FsState::Absent => {
            return Outcome::Failed {
                reason: format!("{} does not exist", path.display()),
            };
        }
        FsState::NonFile(kind) => {
            return Outcome::Failed {
                reason: format!(
                    "{} is a {kind}; file_contains only matches regular workspace files",
                    path.display()
                ),
            };
        }
        FsState::MetadataError(e) => {
            return Outcome::Failed {
                reason: format!("could not stat {}: {e}", path.display()),
            };
        }
        FsState::RegularFile => {}
    }
    // Read raw bytes (with size cap) then decode separately so a non-UTF-8
    // file produces a distinct, actionable error rather than a wrapped
    // io::Error("stream did not contain valid UTF-8") that obscures the
    // real cause. The cap prevents an adversarial multi-GB file from
    // OOM-killing the eval process.
    let bytes = match read_capped(&abs, MAX_FILE_CONTAINS_BYTES) {
        Ok(b) => b,
        Err(e) => {
            return Outcome::Failed {
                reason: format!("could not read {}: {e}", path.display()),
            };
        }
    };
    let content = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => {
            return Outcome::Failed {
                reason: format!(
                    "{} is not valid UTF-8 (file_contains only matches text files)",
                    path.display()
                ),
            };
        }
    };
    if substring_match(&content, substring, case_insensitive) {
        Outcome::Passed
    } else {
        Outcome::Failed {
            reason: format!("{} does not contain {substring:?}", path.display()),
        }
    }
}

fn check_final_message(
    substring: &str,
    case_insensitive: bool,
    expect_contains: bool,
    captured: &CapturedRun,
) -> Outcome {
    let Some(message) = captured.assistant_messages.last() else {
        return Outcome::Failed {
            reason: "no user turns completed (no assistant messages recorded)".into(),
        };
    };
    // `assistant_messages.last()` corresponds to the LAST user turn since
    // capture pushes one entry per `LlmFinish` (empty string for tool-only
    // turns). An empty string here means the final turn produced no
    // narration — fail loudly rather than substring-matching against `""`,
    // which would silently report misleading results.
    if message.is_empty() {
        return Outcome::Failed {
            reason: "final user turn produced no assistant text (only tool calls)".into(),
        };
    }
    let contains = substring_match(message, substring, case_insensitive);
    if contains == expect_contains {
        Outcome::Passed
    } else if expect_contains {
        Outcome::Failed {
            reason: format!("final assistant message does not contain {substring:?}"),
        }
    } else {
        Outcome::Failed {
            reason: format!("final assistant message unexpectedly contains {substring:?}"),
        }
    }
}

fn check_max_repeat(target: &str, max: usize, captured: &CapturedRun) -> Outcome {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for call in &captured.tool_calls {
        if call.tool_name.as_str() != target {
            continue;
        }
        let key = canonical_json(&call.arguments);
        *counts.entry(key).or_insert(0) += 1;
    }
    if let Some((args, count)) = counts.iter().max_by_key(|(_, n)| **n)
        && *count > max
    {
        return Outcome::Failed {
            reason: format!(
                "tool {target:?} called {count} times with the same args (max={max}): args={args}"
            ),
        };
    }
    Outcome::Passed
}

/// Sort object keys recursively so the dedup key for max_repeat_attempts
/// is stable even if `serde_json`'s `preserve_order` feature is enabled
/// transitively (which would otherwise let identical args with different
/// key orderings count as distinct calls).
fn canonical_json(v: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &serde_json::Value, out: &mut String) {
    match v {
        serde_json::Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).expect("string serialization"));
                out.push(':');
                write_canonical(&m[*k], out);
            }
            out.push('}');
        }
        serde_json::Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// Read at most `cap` bytes from `path`. Returns the bytes read; if the
/// file is larger than `cap`, returns `Err` carrying a context-bearing
/// `io::ErrorKind::Other`. Used by `check_file_contains` to bound memory
/// usage on adversarial inputs (the only caller folds the error into a
/// `Failed` reason, so `Other` is fine — no caller branches on the kind).
fn read_capped(path: &Path, cap: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > cap {
        return Err(std::io::Error::other(format!(
            "file size {} exceeds file_contains cap of {} bytes",
            metadata.len(),
            cap
        )));
    }
    let mut buf = Vec::with_capacity(metadata.len() as usize);
    file.take(cap).read_to_end(&mut buf)?;
    Ok(buf)
}

fn substring_match(haystack: &str, needle: &str, case_insensitive: bool) -> bool {
    if case_insensitive {
        haystack.to_lowercase().contains(&needle.to_lowercase())
    } else {
        haystack.contains(needle)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::eval::workspace::WorkspaceSnapshot;

    fn empty_capture(workspace_root: PathBuf) -> CapturedRun {
        CapturedRun::new(
            workspace_root,
            WorkspaceSnapshot {
                files: BTreeMap::new(),
            },
        )
    }

    fn call(call_id: &str, tool_name: ToolName, arguments: serde_json::Value) -> RecordedToolCall {
        RecordedToolCall {
            call_id: call_id.into(),
            tool_name,
            arguments,
            output: Some("ok".into()),
            is_error: false,
        }
    }

    // ── tool_called / tool_not_called ──

    #[test]
    fn tool_called_passes_when_present() {
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls.push(call("c1", ToolName::Read, json!({})));
        let r = check_tool_called("read", &cap);
        assert!(r.is_passed());
    }

    #[test]
    fn tool_called_fails_when_absent() {
        let cap = empty_capture(PathBuf::from("/tmp"));
        let r = check_tool_called("read", &cap);
        assert!(r.is_failed());
    }

    #[test]
    fn tool_not_called_passes_when_absent() {
        let cap = empty_capture(PathBuf::from("/tmp"));
        let r = check_tool_not_called("bash", &cap);
        assert!(r.is_passed());
    }

    #[test]
    fn tool_not_called_fails_when_present() {
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls.push(call("c1", ToolName::Bash, json!({})));
        let r = check_tool_not_called("bash", &cap);
        assert!(r.is_failed());
    }

    // ── requires_prior_read ──

    #[test]
    fn requires_prior_read_passes_when_target_never_called() {
        let cap = empty_capture(PathBuf::from("/tmp"));
        let r = check_requires_prior_read("edit", &[PathBuf::from(".teller.yml")], &cap);
        assert!(r.is_passed());
    }

    #[test]
    fn requires_prior_read_passes_when_read_precedes_target() {
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls
            .push(call("c1", ToolName::Read, json!({"path": ".teller.yml"})));
        cap.tool_calls.push(call(
            "c2",
            ToolName::Edit,
            json!({"file_path": ".teller.yml"}),
        ));
        let r = check_requires_prior_read("edit", &[PathBuf::from(".teller.yml")], &cap);
        assert!(r.is_passed());
    }

    #[test]
    fn requires_prior_read_fails_when_no_read_before_target() {
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls.push(call(
            "c1",
            ToolName::Edit,
            json!({"file_path": ".teller.yml"}),
        ));
        let r = check_requires_prior_read("edit", &[PathBuf::from(".teller.yml")], &cap);
        assert!(r.is_failed());
    }

    #[test]
    fn requires_prior_read_fails_when_read_targets_different_file() {
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls
            .push(call("c1", ToolName::Read, json!({"path": "other.txt"})));
        cap.tool_calls.push(call(
            "c2",
            ToolName::Edit,
            json!({"file_path": ".teller.yml"}),
        ));
        let r = check_requires_prior_read("edit", &[PathBuf::from(".teller.yml")], &cap);
        assert!(r.is_failed());
        // Failure message must list what was actually read so debugging is
        // possible without re-running the scenario.
        let Outcome::Failed { reason } = &r else {
            panic!("expected Failed, got {r:?}");
        };
        assert!(
            reason.contains("other.txt"),
            "failure must surface the actually-read path: {reason}"
        );
    }

    #[test]
    fn requires_prior_read_passes_when_one_of_multiple_paths_is_read() {
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls
            .push(call("c1", ToolName::Read, json!({"path": "AGENTS.md"})));
        cap.tool_calls.push(call(
            "c2",
            ToolName::Edit,
            json!({"file_path": ".teller.yml"}),
        ));
        let r = check_requires_prior_read(
            "edit",
            &[PathBuf::from(".teller.yml"), PathBuf::from("AGENTS.md")],
            &cap,
        );
        assert!(r.is_passed());
    }

    #[test]
    fn requires_prior_read_handles_absolute_path_in_args() {
        let mut cap = empty_capture(PathBuf::from("/tmp/eval-ws"));
        cap.tool_calls.push(call(
            "c1",
            ToolName::Read,
            json!({"path": "/tmp/eval-ws/.teller.yml"}),
        ));
        cap.tool_calls.push(call(
            "c2",
            ToolName::Edit,
            json!({"file_path": ".teller.yml"}),
        ));
        let r = check_requires_prior_read("edit", &[PathBuf::from(".teller.yml")], &cap);
        assert!(r.is_passed());
    }

    #[test]
    fn requires_prior_read_ignores_grep_as_not_read_class() {
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls.push(call(
            "c1",
            ToolName::Grep,
            json!({"path": ".teller.yml", "pattern": "x"}),
        ));
        cap.tool_calls.push(call(
            "c2",
            ToolName::Edit,
            json!({"file_path": ".teller.yml"}),
        ));
        let r = check_requires_prior_read("edit", &[PathBuf::from(".teller.yml")], &cap);
        assert!(r.is_failed());
    }

    #[test]
    fn requires_prior_read_fails_when_read_happens_after_target() {
        // Regression guard: reading the file AFTER calling the protected
        // tool does not retroactively satisfy the requirement.
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls.push(call(
            "c1",
            ToolName::Edit,
            json!({"file_path": ".teller.yml"}),
        ));
        cap.tool_calls
            .push(call("c2", ToolName::Read, json!({"path": ".teller.yml"})));
        let r = check_requires_prior_read("edit", &[PathBuf::from(".teller.yml")], &cap);
        assert!(r.is_failed());
    }

    #[test]
    fn requires_prior_read_only_first_target_invocation_gates() {
        // First edit had no prior read → fail, even though a later edit
        // would have been preceded by one.
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls.push(call(
            "c1",
            ToolName::Edit,
            json!({"file_path": ".teller.yml"}),
        ));
        cap.tool_calls
            .push(call("c2", ToolName::Read, json!({"path": ".teller.yml"})));
        cap.tool_calls.push(call(
            "c3",
            ToolName::Edit,
            json!({"file_path": ".teller.yml"}),
        ));
        let r = check_requires_prior_read("edit", &[PathBuf::from(".teller.yml")], &cap);
        assert!(r.is_failed());
    }

    #[test]
    fn requires_prior_read_traverses_multiple_non_matching_reads() {
        // Three reads of unrelated paths before the target — none satisfy
        // the requirement. Guards against off-by-one or short-circuit bugs
        // in the prior-call loop.
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls
            .push(call("c1", ToolName::Read, json!({"path": "a.txt"})));
        cap.tool_calls
            .push(call("c2", ToolName::Read, json!({"path": "b.txt"})));
        cap.tool_calls
            .push(call("c3", ToolName::Read, json!({"path": "c.txt"})));
        cap.tool_calls.push(call(
            "c4",
            ToolName::Edit,
            json!({"file_path": ".teller.yml"}),
        ));
        let r = check_requires_prior_read("edit", &[PathBuf::from(".teller.yml")], &cap);
        assert!(r.is_failed());
        let Outcome::Failed { reason } = &r else {
            panic!("expected Failed");
        };
        for p in ["a.txt", "b.txt", "c.txt"] {
            assert!(
                reason.contains(p),
                "failure must list every read path; missing {p}: {reason}"
            );
        }
    }

    #[test]
    fn requires_prior_read_does_not_count_failed_reads() {
        // A read tool call that errored tells the agent NOTHING about the
        // file content, so it cannot satisfy the "must have read first"
        // requirement. Previously, my code counted any matching-path read
        // as satisfying — a file-not-found error on the target path could
        // let a destructive edit slip through.
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls.push(RecordedToolCall {
            call_id: "c1".into(),
            tool_name: ToolName::Read,
            arguments: json!({"path": ".teller.yml"}),
            output: Some("file not found".into()),
            is_error: true,
        });
        cap.tool_calls.push(call(
            "c2",
            ToolName::Edit,
            json!({"file_path": ".teller.yml"}),
        ));
        let r = check_requires_prior_read("edit", &[PathBuf::from(".teller.yml")], &cap);
        assert!(
            r.is_failed(),
            "failed read must NOT satisfy requires_prior_read"
        );
        let Outcome::Failed { reason } = &r else {
            panic!("expected Failed");
        };
        assert!(
            reason.contains("failed (errored) read attempts") && reason.contains(".teller.yml"),
            "failure must surface the failed read attempt: {reason}"
        );
    }

    #[test]
    fn requires_prior_read_handles_dotdot_in_args() {
        // Agent reads `src/../config.yml`, scenario expects `config.yml` —
        // these must compare equal after lexical normalization.
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls.push(call(
            "c1",
            ToolName::Read,
            json!({"path": "src/../config.yml"}),
        ));
        cap.tool_calls.push(call(
            "c2",
            ToolName::Edit,
            json!({"file_path": "config.yml"}),
        ));
        let r = check_requires_prior_read("edit", &[PathBuf::from("config.yml")], &cap);
        assert!(r.is_passed());
    }

    #[test]
    fn requires_prior_read_handles_dotdot_in_required_path() {
        // Symmetric: scenario lists `./required/../target.yml`, agent reads
        // `target.yml`. Both reduce to `target.yml`.
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls
            .push(call("c1", ToolName::Read, json!({"path": "target.yml"})));
        cap.tool_calls.push(call(
            "c2",
            ToolName::Edit,
            json!({"file_path": "target.yml"}),
        ));
        let r =
            check_requires_prior_read("edit", &[PathBuf::from("./required/../target.yml")], &cap);
        assert!(r.is_passed());
    }

    #[test]
    fn requires_prior_read_handles_redundant_separators() {
        // `foo/.//bar` and `foo/bar` collapse to the same path.
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls
            .push(call("c1", ToolName::Read, json!({"path": "foo/.//bar"})));
        cap.tool_calls
            .push(call("c2", ToolName::Edit, json!({"file_path": "foo/bar"})));
        let r = check_requires_prior_read("edit", &[PathBuf::from("foo/bar")], &cap);
        assert!(r.is_passed());
    }

    #[test]
    fn requires_prior_read_flags_outside_workspace_paths_in_failure() {
        // LLM emits an absolute path to a file outside the workspace —
        // that path can never satisfy the requirement, but the failure
        // message should call it out so the operator notices.
        let mut cap = empty_capture(PathBuf::from("/tmp/eval-ws"));
        cap.tool_calls
            .push(call("c1", ToolName::Read, json!({"path": "/etc/passwd"})));
        cap.tool_calls.push(call(
            "c2",
            ToolName::Edit,
            json!({"file_path": ".teller.yml"}),
        ));
        let r = check_requires_prior_read("edit", &[PathBuf::from(".teller.yml")], &cap);
        assert!(r.is_failed());
        let Outcome::Failed { reason } = &r else {
            panic!("expected Failed");
        };
        assert!(
            reason.contains("outside-workspace") && reason.contains("/etc/passwd"),
            "failure must flag outside-workspace paths: {reason}"
        );
    }

    // ── file_unchanged ──

    #[test]
    fn file_unchanged_passes_when_baseline_and_current_match() {
        let tmp = tempfile::tempdir().unwrap();
        let path = Path::new("data.txt");
        let abs = tmp.path().join(path);
        std::fs::write(&abs, "hello\n").unwrap();
        let hash: [u8; 32] = Sha256::digest(b"hello\n").into();
        let mut baseline = BTreeMap::new();
        baseline.insert(path.to_path_buf(), hash);
        let cap = CapturedRun::new(
            tmp.path().to_path_buf(),
            WorkspaceSnapshot { files: baseline },
        );

        let r = check_file_unchanged(path, &cap);
        assert!(r.is_passed());
    }

    #[test]
    fn file_unchanged_fails_when_content_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = Path::new("data.txt");
        let abs = tmp.path().join(path);
        std::fs::write(&abs, "modified\n").unwrap();
        let original_hash: [u8; 32] = Sha256::digest(b"original\n").into();
        let mut baseline = BTreeMap::new();
        baseline.insert(path.to_path_buf(), original_hash);
        let cap = CapturedRun::new(
            tmp.path().to_path_buf(),
            WorkspaceSnapshot { files: baseline },
        );

        let r = check_file_unchanged(path, &cap);
        assert!(r.is_failed());
    }

    #[test]
    fn file_unchanged_fails_when_file_is_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let path = Path::new("gone.txt");
        let original_hash: [u8; 32] = Sha256::digest(b"x").into();
        let mut baseline = BTreeMap::new();
        baseline.insert(path.to_path_buf(), original_hash);
        let cap = CapturedRun::new(
            tmp.path().to_path_buf(),
            WorkspaceSnapshot { files: baseline },
        );

        let r = check_file_unchanged(path, &cap);
        assert!(r.is_failed());
    }

    #[test]
    fn file_unchanged_fails_when_file_is_created() {
        let tmp = tempfile::tempdir().unwrap();
        let path = Path::new("new.txt");
        std::fs::write(tmp.path().join(path), "x").unwrap();
        let cap = CapturedRun::new(
            tmp.path().to_path_buf(),
            WorkspaceSnapshot {
                files: BTreeMap::new(),
            },
        );

        let r = check_file_unchanged(path, &cap);
        assert!(r.is_failed());
    }

    #[test]
    fn file_unchanged_fails_when_directory_created_at_path() {
        // Creating a directory at a baseline-absent path is a real change.
        let tmp = tempfile::tempdir().unwrap();
        let path = Path::new("became_dir");
        std::fs::create_dir(tmp.path().join(path)).unwrap();
        let cap = CapturedRun::new(
            tmp.path().to_path_buf(),
            WorkspaceSnapshot {
                files: BTreeMap::new(),
            },
        );
        let r = check_file_unchanged(path, &cap);
        assert!(r.is_failed(), "directory creation must fail file_unchanged");
        let Outcome::Failed { reason } = &r else {
            panic!("expected Failed");
        };
        assert!(
            reason.contains("directory"),
            "expected directory-specific reason: {reason}"
        );
    }

    #[test]
    fn file_unchanged_fails_when_baseline_file_replaced_by_directory() {
        // Replacing a baseline file with a directory at the same path
        // should fail with "replaced by directory", not "deleted".
        let tmp = tempfile::tempdir().unwrap();
        let path = Path::new("file_to_dir");
        std::fs::create_dir(tmp.path().join(path)).unwrap();
        let original_hash: [u8; 32] = Sha256::digest(b"original").into();
        let mut baseline = BTreeMap::new();
        baseline.insert(path.to_path_buf(), original_hash);
        let cap = CapturedRun::new(
            tmp.path().to_path_buf(),
            WorkspaceSnapshot { files: baseline },
        );

        let r = check_file_unchanged(path, &cap);
        assert!(r.is_failed());
        let Outcome::Failed { reason } = &r else {
            panic!("expected Failed");
        };
        assert!(
            reason.contains("replaced by a directory"),
            "expected directory-replacement reason: {reason}"
        );
    }

    #[test]
    fn file_unchanged_fails_when_baseline_file_replaced_by_symlink() {
        // Security: replacing a baseline file with a symlink-to-something-
        // with-the-same-bytes would silently report Passed if we used
        // is_file()/read() (both dereference symlinks). symlink_metadata
        // catches the symlink even when the target's content matches.
        #[cfg(unix)]
        {
            let tmp = tempfile::tempdir().unwrap();
            let path = Path::new("config.yml");
            // Plant a target file with specific content.
            let target = tmp.path().join("evil-target.yml");
            std::fs::write(&target, "providers: []\n").unwrap();
            // The baseline records a hash for the SAME content.
            let original_hash: [u8; 32] = Sha256::digest(b"providers: []\n").into();
            let mut baseline = BTreeMap::new();
            baseline.insert(path.to_path_buf(), original_hash);
            // Replace the workspace path with a symlink to the target.
            std::os::unix::fs::symlink(&target, tmp.path().join(path)).unwrap();

            let cap = CapturedRun::new(
                tmp.path().to_path_buf(),
                WorkspaceSnapshot { files: baseline },
            );
            let r = check_file_unchanged(path, &cap);
            assert!(
                r.is_failed(),
                "symlink replacement must fail even when target content matches baseline"
            );
            let Outcome::Failed { reason } = &r else {
                panic!("expected Failed");
            };
            assert!(
                reason.contains("symlink"),
                "expected symlink-specific reason: {reason}"
            );
        }
    }

    #[test]
    fn file_unchanged_surfaces_metadata_error_distinctly_from_absent() {
        // Permission-denied / EIO must surface as the real cause, not be
        // misreported as "deleted" or "does not exist."
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let tmp = tempfile::tempdir().unwrap();
            // Make a subdirectory we can drop perms on (so symlink_metadata
            // of a path inside it fails with EACCES).
            let locked_dir = tmp.path().join("locked");
            std::fs::create_dir(&locked_dir).unwrap();
            std::fs::write(locked_dir.join("inner.txt"), "x").unwrap();
            // 0o000 = no perms; classify_path → MetadataError
            std::fs::set_permissions(&locked_dir, std::fs::Permissions::from_mode(0o000)).unwrap();

            let cap = empty_capture(tmp.path().to_path_buf());
            let r = check_file_unchanged(Path::new("locked/inner.txt"), &cap);
            // Restore perms before the assertion so the tempdir cleans up.
            std::fs::set_permissions(&locked_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

            assert!(r.is_failed());
            let Outcome::Failed { reason } = &r else {
                panic!("expected Failed");
            };
            assert!(
                reason.contains("could not stat"),
                "metadata error must surface as stat failure, not as 'deleted': {reason}"
            );
        }
    }

    #[test]
    fn file_unchanged_passes_when_neither_baseline_nor_current_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let cap = CapturedRun::new(
            tmp.path().to_path_buf(),
            WorkspaceSnapshot {
                files: BTreeMap::new(),
            },
        );
        let r = check_file_unchanged(Path::new("nope.txt"), &cap);
        assert!(r.is_passed());
    }

    // ── file_contains ──

    #[test]
    fn file_contains_passes_on_substring_match() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("log.txt"), "WARNING: something").unwrap();
        let cap = empty_capture(tmp.path().to_path_buf());
        let r = check_file_contains(Path::new("log.txt"), "WARNING", false, &cap);
        assert!(r.is_passed());
    }

    #[test]
    fn file_contains_case_insensitive() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("log.txt"), "warning: something").unwrap();
        let cap = empty_capture(tmp.path().to_path_buf());
        let r = check_file_contains(Path::new("log.txt"), "WARNING", true, &cap);
        assert!(r.is_passed());
        let r2 = check_file_contains(Path::new("log.txt"), "WARNING", false, &cap);
        assert!(r2.is_failed(), "case-sensitive must fail here");
    }

    #[test]
    fn file_contains_rejects_symlinks() {
        // Security: a symlink at `path` would let scenarios assert against
        // host content outside the workspace. classify_path uses
        // symlink_metadata so the symlink is caught BEFORE any read.
        #[cfg(unix)]
        {
            let tmp = tempfile::tempdir().unwrap();
            let target = tmp.path().join("outside-target.txt");
            std::fs::write(&target, "matching content").unwrap();
            std::os::unix::fs::symlink(&target, tmp.path().join("link")).unwrap();

            let cap = empty_capture(tmp.path().to_path_buf());
            let r = check_file_contains(Path::new("link"), "matching", false, &cap);
            assert!(r.is_failed(), "symlink must be rejected, not followed");
            let Outcome::Failed { reason } = &r else {
                panic!("expected Failed");
            };
            assert!(
                reason.contains("symlink"),
                "expected symlink-specific reason: {reason}"
            );
        }
    }

    #[test]
    fn file_contains_fails_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let cap = empty_capture(tmp.path().to_path_buf());
        let r = check_file_contains(Path::new("nope.txt"), "x", false, &cap);
        assert!(r.is_failed());
    }

    #[test]
    fn file_contains_distinguishes_non_utf8_from_io_error() {
        // A binary file the agent created should produce an actionable
        // "not valid UTF-8" reason, not a generic I/O wrapper.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("blob.bin"), [0xff, 0xfe, 0x00, 0xff]).unwrap();
        let cap = empty_capture(tmp.path().to_path_buf());
        let r = check_file_contains(Path::new("blob.bin"), "anything", false, &cap);
        assert!(r.is_failed());
        let Outcome::Failed { reason } = &r else {
            panic!("expected Failed");
        };
        assert!(
            reason.contains("UTF-8"),
            "non-UTF-8 file must produce a UTF-8 reason: {reason}"
        );
    }

    // ── final_message_contains / not_contains ──

    #[test]
    fn final_message_contains_passes_on_match() {
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.assistant_messages.push("hello world".into());
        let r = check_final_message("hello", false, true, &cap);
        assert!(r.is_passed());
    }

    #[test]
    fn final_message_contains_fails_when_no_turns_completed() {
        // Zero entries in assistant_messages = no LlmFinish ever fired
        // (likely a stream error before completion).
        let cap = empty_capture(PathBuf::from("/tmp"));
        let r = check_final_message("hello", false, true, &cap);
        assert!(r.is_failed());
        let Outcome::Failed { reason } = &r else {
            panic!("expected Failed");
        };
        assert!(
            reason.contains("no user turns completed"),
            "expected zero-turns reason: {reason}"
        );
    }

    #[test]
    fn final_message_contains_fails_when_final_turn_has_no_narration() {
        // Multi-turn case: earlier turn had text, final turn was tool-only.
        // Capture pushes "" for the final turn; check_final_message must
        // treat it as "no narration" rather than substring-matching against "".
        // Without this distinction, an earlier turn's text could masquerade
        // as the final response.
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.assistant_messages.push("first turn had hello".into());
        cap.assistant_messages.push(String::new()); // final turn: tool-only
        let r = check_final_message("hello", false, true, &cap);
        assert!(r.is_failed(), "must not match earlier turn's 'hello'");
        let Outcome::Failed { reason } = &r else {
            panic!("expected Failed");
        };
        assert!(
            reason.contains("no assistant text"),
            "expected empty-final-turn reason: {reason}"
        );
    }

    #[test]
    fn final_message_not_contains_passes_when_absent() {
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.assistant_messages.push("all good".into());
        let r = check_final_message("no way to recover", true, false, &cap);
        assert!(r.is_passed());
    }

    #[test]
    fn final_message_not_contains_fails_when_present() {
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.assistant_messages
            .push("there's no way to recover this".into());
        let r = check_final_message("no way to recover", true, false, &cap);
        assert!(r.is_failed());
    }

    #[test]
    fn final_message_uses_last_message_when_multiple_turns() {
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.assistant_messages.push("first turn".into());
        cap.assistant_messages.push("second turn".into());
        let r = check_final_message("second", false, true, &cap);
        assert!(r.is_passed());
        let r2 = check_final_message("first", false, true, &cap);
        assert!(r2.is_failed(), "should only inspect the LAST message");
    }

    // ── max_repeat_attempts ──

    #[test]
    fn max_repeat_attempts_passes_at_limit() {
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        for i in 0..2 {
            cap.tool_calls
                .push(call(&format!("c{i}"), ToolName::Edit, json!({"x": "same"})));
        }
        let r = check_max_repeat("edit", 2, &cap);
        assert!(r.is_passed());
    }

    #[test]
    fn max_repeat_attempts_fails_above_limit() {
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        for i in 0..3 {
            cap.tool_calls
                .push(call(&format!("c{i}"), ToolName::Edit, json!({"x": "same"})));
        }
        let r = check_max_repeat("edit", 2, &cap);
        assert!(r.is_failed());
    }

    #[test]
    fn max_repeat_attempts_distinguishes_by_args() {
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls
            .push(call("c1", ToolName::Edit, json!({"x": "a"})));
        cap.tool_calls
            .push(call("c2", ToolName::Edit, json!({"x": "b"})));
        cap.tool_calls
            .push(call("c3", ToolName::Edit, json!({"x": "c"})));
        let r = check_max_repeat("edit", 1, &cap);
        assert!(r.is_passed());
    }

    #[test]
    fn max_repeat_attempts_dedup_is_key_order_independent() {
        // Regression guard: serde_json with `preserve_order` enabled
        // anywhere in the dep graph would emit Object keys in insertion
        // order. Without canonical_json, semantically-identical args with
        // different key orderings would NOT dedup, silently letting a
        // misbehaving agent slip past the limit.
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        // Build the same logical args two different ways. serde_json's
        // Map preserves order with the feature on; without it sorts by key.
        // Either way, the canonical form must collapse them.
        let args_ab: serde_json::Value = serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
        let args_ba: serde_json::Value = serde_json::from_str(r#"{"b":2,"a":1}"#).unwrap();
        cap.tool_calls.push(call("c1", ToolName::Edit, args_ab));
        cap.tool_calls.push(call("c2", ToolName::Edit, args_ba));
        let r = check_max_repeat("edit", 1, &cap);
        assert!(
            r.is_failed(),
            "two semantically-identical arg orderings must count as a repeat"
        );
    }

    // ── judge skip ──

    #[test]
    fn skipped_only_report_passes() {
        // A report containing nothing but Skipped outcomes (e.g. a scenario
        // composed entirely of Judge expectations on Phase 3) must pass —
        // the contract is "no Failed flips passed", and Skipped is neutral.
        let report = EvalReport {
            results: vec![
                ExpectationResult {
                    expectation: Expectation::Judge {
                        pass_when: "a".into(),
                        fail_when: "b".into(),
                        judge_model: None,
                    },
                    outcome: Outcome::Skipped {
                        reason: "phase 4".into(),
                    },
                },
                ExpectationResult {
                    expectation: Expectation::Judge {
                        pass_when: "c".into(),
                        fail_when: "d".into(),
                        judge_model: None,
                    },
                    outcome: Outcome::Skipped {
                        reason: "phase 4".into(),
                    },
                },
            ],
        };
        assert!(report.passed(), "Skipped-only report must pass");
    }

    #[test]
    fn eval_report_round_trips_through_json() {
        // Phase 6's `compare` will deserialize JSONL records — pin that the
        // serde tag names and per-variant fields survive a round trip. A
        // future tag rename or `#[serde(skip_serializing_if = ...)]` on a
        // field would silently break compare; this test catches it.
        let original = EvalReport {
            results: vec![
                ExpectationResult {
                    expectation: Expectation::ToolCalled {
                        tool: "read".into(),
                    },
                    outcome: Outcome::Passed,
                },
                ExpectationResult {
                    expectation: Expectation::FileContains {
                        path: PathBuf::from("foo.txt"),
                        substring: "hello".into(),
                        case_insensitive: true,
                    },
                    outcome: Outcome::Failed {
                        reason: "no match".into(),
                    },
                },
                ExpectationResult {
                    expectation: Expectation::Judge {
                        pass_when: "x".into(),
                        fail_when: "y".into(),
                        judge_model: Some("anthropic/claude-haiku-4-5".into()),
                    },
                    outcome: Outcome::Skipped {
                        reason: "phase 4".into(),
                    },
                },
            ],
        };
        let json = serde_json::to_string(&original).unwrap();
        let reparsed: EvalReport = serde_json::from_str(&json).unwrap();
        assert_eq!(reparsed.results.len(), 3);
        assert!(matches!(reparsed.results[0].outcome, Outcome::Passed));
        assert!(matches!(
            reparsed.results[0].expectation,
            Expectation::ToolCalled { ref tool } if tool == "read"
        ));
        assert!(matches!(
            reparsed.results[1].outcome,
            Outcome::Failed { ref reason } if reason == "no match"
        ));
        assert!(matches!(
            reparsed.results[2].outcome,
            Outcome::Skipped { .. }
        ));
        assert!(matches!(
            reparsed.results[2].expectation,
            Expectation::Judge { ref pass_when, .. } if pass_when == "x"
        ));
    }

    #[test]
    fn judge_returns_skipped() {
        let scenario = Scenario {
            name: "x".into(),
            description: "x".into(),
            runs: std::num::NonZeroUsize::new(1).unwrap(),
            setup: crate::eval::scenario::Setup::default(),
            user_turns: vec!["hi".into()],
            expectations: vec![Expectation::Judge {
                pass_when: "x".into(),
                fail_when: "y".into(),
                judge_model: None,
            }],
        };
        let cap = empty_capture(PathBuf::from("/tmp"));
        let report = evaluate(&scenario, &cap);
        assert_eq!(report.results.len(), 1);
        assert!(matches!(report.results[0].outcome, Outcome::Skipped { .. }));
        assert!(report.passed());
    }

    // ── evaluate roll-up ──

    #[test]
    fn evaluate_embeds_source_expectation_in_each_result() {
        // Each ExpectationResult must carry the original Expectation so the
        // JSON output is self-describing without cross-referencing
        // scenario.toml. Pins the evaluate_one wrap-with-clone behavior.
        let scenario = Scenario {
            name: "x".into(),
            description: "x".into(),
            runs: std::num::NonZeroUsize::new(1).unwrap(),
            setup: crate::eval::scenario::Setup::default(),
            user_turns: vec!["hi".into()],
            expectations: vec![
                Expectation::ToolCalled {
                    tool: "read".into(),
                },
                Expectation::FinalMessageContains {
                    substring: "hello".into(),
                    case_insensitive: true,
                },
            ],
        };
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls.push(call("c1", ToolName::Read, json!({})));
        cap.assistant_messages.push("hello world".into());

        let report = evaluate(&scenario, &cap);
        assert_eq!(report.results.len(), 2);
        assert!(matches!(
            report.results[0].expectation,
            Expectation::ToolCalled { ref tool } if tool == "read"
        ));
        assert!(matches!(
            report.results[1].expectation,
            Expectation::FinalMessageContains { ref substring, case_insensitive: true }
                if substring == "hello"
        ));
    }

    #[test]
    fn evaluate_passed_only_when_no_failures() {
        let scenario = Scenario {
            name: "x".into(),
            description: "x".into(),
            runs: std::num::NonZeroUsize::new(1).unwrap(),
            setup: crate::eval::scenario::Setup::default(),
            user_turns: vec!["hi".into()],
            expectations: vec![
                Expectation::ToolCalled {
                    tool: "read".into(),
                },
                Expectation::ToolNotCalled {
                    tool: "bash".into(),
                },
            ],
        };
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls.push(call("c1", ToolName::Read, json!({})));
        let report = evaluate(&scenario, &cap);
        assert!(report.passed());
        assert_eq!(report.results.len(), 2);
    }

    #[test]
    fn evaluate_fails_if_any_expectation_fails() {
        let scenario = Scenario {
            name: "x".into(),
            description: "x".into(),
            runs: std::num::NonZeroUsize::new(1).unwrap(),
            setup: crate::eval::scenario::Setup::default(),
            user_turns: vec!["hi".into()],
            expectations: vec![
                Expectation::ToolCalled {
                    tool: "read".into(),
                },
                Expectation::ToolCalled {
                    tool: "bash".into(),
                },
            ],
        };
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls.push(call("c1", ToolName::Read, json!({})));
        let report = evaluate(&scenario, &cap);
        assert!(!report.passed());
    }

    #[test]
    fn evaluate_mixed_pass_skip_fail_rollup() {
        // The contract Phase 6's `compare` will rely on: Skipped is
        // neutral (doesn't flip passed), Failed flips it.
        let scenario = Scenario {
            name: "x".into(),
            description: "x".into(),
            runs: std::num::NonZeroUsize::new(1).unwrap(),
            setup: crate::eval::scenario::Setup::default(),
            user_turns: vec!["hi".into()],
            expectations: vec![
                Expectation::ToolCalled {
                    tool: "read".into(),
                },
                Expectation::Judge {
                    pass_when: "x".into(),
                    fail_when: "y".into(),
                    judge_model: None,
                },
                Expectation::ToolCalled {
                    tool: "bash".into(),
                },
            ],
        };
        let mut cap = empty_capture(PathBuf::from("/tmp"));
        cap.tool_calls.push(call("c1", ToolName::Read, json!({})));
        let report = evaluate(&scenario, &cap);
        assert_eq!(report.results.len(), 3);
        assert!(
            report.results[0].outcome.is_passed(),
            "tool_called: read present"
        );
        assert!(matches!(report.results[1].outcome, Outcome::Skipped { .. }));
        assert!(
            report.results[2].outcome.is_failed(),
            "tool_called: bash absent"
        );
        assert!(!report.passed(), "any Failed must flip the report");

        // Same scenario without the failing expectation: passed stays true
        // even though Skipped is in the middle.
        let scenario_no_fail = Scenario {
            expectations: vec![
                Expectation::ToolCalled {
                    tool: "read".into(),
                },
                Expectation::Judge {
                    pass_when: "x".into(),
                    fail_when: "y".into(),
                    judge_model: None,
                },
            ],
            ..scenario
        };
        let report = evaluate(&scenario_no_fail, &cap);
        assert!(report.passed(), "Pass + Skip alone must pass");
    }

    // ── canonical_json helper unit tests ──

    #[test]
    fn canonical_json_sorts_object_keys_recursively() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"b":{"d":2,"c":1},"a":[1,{"y":0,"x":1}]}"#).unwrap();
        let s = canonical_json(&v);
        assert_eq!(s, r#"{"a":[1,{"x":1,"y":0}],"b":{"c":1,"d":2}}"#);
    }

    #[test]
    fn canonical_json_identical_for_reordered_inputs() {
        let v1: serde_json::Value = serde_json::from_str(r#"{"a":1,"b":{"c":2,"d":3}}"#).unwrap();
        let v2: serde_json::Value = serde_json::from_str(r#"{"b":{"d":3,"c":2},"a":1}"#).unwrap();
        assert_eq!(canonical_json(&v1), canonical_json(&v2));
    }
}
