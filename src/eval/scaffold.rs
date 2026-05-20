//! Scenario scaffold builder — emits a `scenario.toml` scaffold the user
//! then hand-edits. Consumed by the `/export-scenario` slash command.
//!
//! The expectations seeded into the scaffold are intentionally TODO-filled
//! placeholders. The default uses `final_message_contains` (the cheapest
//! deterministic backstop); see EVAL.md for the full list of rule kinds.
//!
//! This module is a **pure emitter** — it takes pre-built `user_turns` and
//! `fixture_paths` and serializes them. The caller (`app/commands.rs`)
//! owns extraction from session state, because tool calls live in UI
//! `MessageBlock`s rather than persisted `Message`s and the scaffold
//! shouldn't know about either type.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::eval::scenario::Scenario;

pub struct ScaffoldInput<'a> {
    pub name: &'a str,
    pub user_turns: Vec<String>,
    /// Already-deduplicated, ordered, validated workspace-relative paths
    /// the agent touched during the session. Each must satisfy
    /// [`is_valid_fixture_candidate`] — the caller filters with that
    /// helper so the emitted suggestion block never points the user at
    /// a path that `Scenario::validate` would later reject.
    pub fixture_paths: Vec<PathBuf>,
}

/// Build a `scenario.toml` scaffold string. Round-trip-validated before
/// returning: a successful return guarantees the output parses + validates
/// via [`Scenario::from_toml_str`].
pub fn build_scaffold(input: ScaffoldInput<'_>) -> Result<String> {
    if input.user_turns.is_empty() {
        bail!("session has no user messages — cannot scaffold a scenario");
    }

    let mut out = String::new();
    // Leading header points the author at the sidecar + the project's
    // authoring guide. Both are critical for filling in the TODOs below
    // with anything more useful than the placeholders.
    out.push_str(
        "# Scenario scaffold — replace every TODO below before committing.\n\
         #\n\
         # Authoring references:\n\
         #   - SESSION_TRACE.md in this directory captures the original\n\
         #     session's tool calls + final assistant message.\n\
         #   - EVAL.md (in the steve repo) is the full authoring guide.\n\
         #   - eval/scenarios/no-hallucinated-tool-output/scenario.toml\n\
         #     is a good single-substring reference.\n\n",
    );

    out.push_str(&format!("name = {}\n", toml_string(input.name)));
    out.push_str(
        "description = \"TODO: one sentence describing what behavior this scenario captures. \
         e.g. 'agent should ask before deleting a config key it cannot restore'\"\n\n",
    );

    out.push_str("user_turns = [\n");
    for turn in &input.user_turns {
        out.push_str("  ");
        out.push_str(&toml_string(turn));
        out.push_str(",\n");
    }
    out.push_str("]\n\n");

    // The `copy_fixtures` key is emitted exactly once. When the agent
    // touched files during the session, individual entries are written
    // commented-out *inside* the array — the user removes the `#` on
    // the ones that are actually scenario fixtures. This avoids the
    // duplicate-key footgun of also emitting an active
    // `copy_fixtures = []` next to a commented-out array literal.
    out.push_str("[setup]\n");
    if input.fixture_paths.is_empty() {
        out.push_str("copy_fixtures = []\n\n");
    } else {
        out.push_str(
            "# Two-step workflow to enable any of these as a real fixture:\n\
             #   1. Copy the actual file from your source project into THIS\n\
             #      scenario directory. The runner copies files FROM HERE\n\
             #      into the scenario workspace — uncommenting the entry\n\
             #      below alone does not pull the file in, the file must\n\
             #      physically exist at this relative path within the\n\
             #      scenario directory. Preserve the relative path so the\n\
             #      agent sees the same layout it saw in the original\n\
             #      session.\n\
             #   2. Uncomment the matching entry below.\n\
             # Entries the agent read but that aren't real scenario fixtures\n\
             # should stay commented (or be deleted).\n",
        );
        out.push_str("copy_fixtures = [\n");
        for p in &input.fixture_paths {
            out.push_str("  # ");
            out.push_str(&toml_string(&p.to_string_lossy()));
            out.push_str(",\n");
        }
        out.push_str("]\n\n");
    }

    out.push_str(
        "[[expectations]]\n\
         # Replace \"TODO\" with an unguessable token from the expected\n\
         # final assistant message — e.g. a specific figure (`42,331`), an\n\
         # error phrase (`cannot recover`), or a path the agent should\n\
         # name explicitly. SESSION_TRACE.md in this directory has the\n\
         # original final message verbatim; pull a phrase from there.\n\
         #\n\
         # See EVAL.md for the full list of expectation kinds — for tool\n\
         # invariants (`tool_called`, `requires_prior_read`,\n\
         # `max_repeat_attempts`), file invariants (`file_contains`,\n\
         # `file_unchanged`), or output negation (`final_message_not_contains`).\n\
         kind = \"final_message_contains\"\n\
         substring = \"TODO\"\n",
    );

    Scenario::from_toml_str(&out)
        .context("scaffold emitter produced TOML that does not parse — emitter bug")?;

    Ok(out)
}

/// Render a Rust string as a TOML string literal (quoted, with escapes).
/// Single-line strings emit as `"..."`; multi-line user turns get
/// emitted as a `"""..."""` block, which deviates from the
/// single-line convention seen in `eval/scenarios/_smoke/scenario.toml`
/// but remains valid TOML — the scaffold is hand-edit-required anyway.
fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// True iff `path` would pass `Scenario::validate`'s workspace-relative
/// fixture check. Rejects:
/// - Absolute paths (cover `/tmp`, `/var/folders`, Windows drives, etc.)
/// - `..` parent-dir components (would escape the workspace)
/// - `.` current-dir components (validate rejects `./foo` because baseline
///   lookups are key-equality and a leading `./` would never match)
/// - Root or platform prefix components
///
/// Mirrors `validate_workspace_relative_path` at `src/eval/scenario.rs:223`
/// so suggested fixtures are guaranteed-valid the moment the user
/// uncomments them.
pub fn is_valid_fixture_candidate(path: &Path) -> bool {
    if path.is_absolute() {
        return false;
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::ParentDir | Component::CurDir => return false,
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_user_turns_errors() {
        let res = build_scaffold(ScaffoldInput {
            name: "demo",
            user_turns: Vec::new(),
            fixture_paths: Vec::new(),
        });
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("no user messages"));
    }

    #[test]
    fn single_user_turn_round_trips() {
        let out = build_scaffold(ScaffoldInput {
            name: "single-turn",
            user_turns: vec!["hello world".to_string()],
            fixture_paths: Vec::new(),
        })
        .expect("ok");
        let parsed = Scenario::from_toml_str(&out).expect("parses");
        assert_eq!(parsed.name, "single-turn");
        assert_eq!(parsed.user_turns, vec!["hello world".to_string()]);
        assert_eq!(parsed.expectations.len(), 1);
        assert!(parsed.setup.copy_fixtures.is_empty());
    }

    #[test]
    fn multiple_user_turns_preserve_order() {
        let out = build_scaffold(ScaffoldInput {
            name: "multi-turn",
            user_turns: vec![
                "first".to_string(),
                "second".to_string(),
                "third".to_string(),
            ],
            fixture_paths: Vec::new(),
        })
        .expect("ok");
        let parsed = Scenario::from_toml_str(&out).expect("parses");
        assert_eq!(
            parsed.user_turns,
            vec![
                "first".to_string(),
                "second".to_string(),
                "third".to_string()
            ]
        );
    }

    #[test]
    fn user_turn_with_toml_special_chars_escapes_correctly() {
        let tricky = "she said \"hi\" \\ then\nleft";
        let out = build_scaffold(ScaffoldInput {
            name: "tricky",
            user_turns: vec![tricky.to_string()],
            fixture_paths: Vec::new(),
        })
        .expect("ok");
        let parsed = Scenario::from_toml_str(&out).expect("parses");
        assert_eq!(parsed.user_turns.len(), 1);
        assert!(parsed.user_turns[0].contains("\"hi\""));
        assert!(parsed.user_turns[0].contains("\\"));
        assert!(parsed.user_turns[0].contains('\n'));
    }

    #[test]
    fn fixture_paths_emit_commented_entries_inside_single_array() {
        let out = build_scaffold(ScaffoldInput {
            name: "with-fixtures",
            user_turns: vec!["hi".to_string()],
            fixture_paths: vec![PathBuf::from("config.toml"), PathBuf::from("src/main.rs")],
        })
        .expect("ok");
        assert!(out.contains("# Two-step workflow"));
        assert!(out.contains("\"config.toml\""));
        assert!(out.contains("\"src/main.rs\""));
        // `copy_fixtures` MUST appear exactly once. Two keys (commented +
        // active) would fail to parse the moment the user uncomments one.
        assert_eq!(
            out.matches("copy_fixtures").count(),
            1,
            "exactly one `copy_fixtures` key expected, got:\n{out}"
        );
        // Entries live inside a real array, each commented individually so
        // the user can selectively uncomment without juggling array syntax.
        assert!(out.contains("copy_fixtures = ["));
        assert!(out.contains("  # \"config.toml\","));
        assert!(out.contains("  # \"src/main.rs\","));
        let parsed = Scenario::from_toml_str(&out).expect("parses");
        // With all entries commented, the parsed value is empty.
        assert!(parsed.setup.copy_fixtures.is_empty());
    }

    #[test]
    fn user_uncommenting_a_suggestion_still_parses() {
        let out = build_scaffold(ScaffoldInput {
            name: "user-edit-sim",
            user_turns: vec!["hi".to_string()],
            fixture_paths: vec![PathBuf::from("config.toml"), PathBuf::from("src/main.rs")],
        })
        .expect("ok");
        // Simulate the user removing the `# ` from the first entry.
        let edited = out.replacen("  # \"config.toml\",", "  \"config.toml\",", 1);
        let parsed = Scenario::from_toml_str(&edited).expect("user-edited scaffold parses");
        assert_eq!(
            parsed.setup.copy_fixtures,
            vec![PathBuf::from("config.toml")]
        );
    }

    #[test]
    fn no_fixture_paths_emits_empty_active_array() {
        let out = build_scaffold(ScaffoldInput {
            name: "no-tools",
            user_turns: vec!["just chat".to_string()],
            fixture_paths: Vec::new(),
        })
        .expect("ok");
        assert!(!out.contains("# Two-step workflow"));
        assert!(out.contains("\ncopy_fixtures = []\n"));
        assert_eq!(out.matches("copy_fixtures").count(), 1);
    }

    #[test]
    fn scaffold_seeds_final_message_contains_expectation() {
        // The scaffold's default expectation is `final_message_contains` —
        // the cheapest deterministic backstop. Authors typically replace
        // or extend it with tool/file invariants from EVAL.md after pulling
        // a real sentinel from SESSION_TRACE.md.
        let out = build_scaffold(ScaffoldInput {
            name: "exp-shape",
            user_turns: vec!["a".to_string()],
            fixture_paths: Vec::new(),
        })
        .expect("ok");
        let parsed = Scenario::from_toml_str(&out).unwrap();
        assert_eq!(parsed.expectations.len(), 1);
        assert!(matches!(
            parsed.expectations[0],
            crate::eval::Expectation::FinalMessageContains { .. }
        ));
    }

    #[test]
    fn is_valid_fixture_candidate_accepts_workspace_relative() {
        assert!(is_valid_fixture_candidate(Path::new("foo.txt")));
        assert!(is_valid_fixture_candidate(Path::new("src/lib.rs")));
        assert!(is_valid_fixture_candidate(Path::new(
            "fixtures/data/file.json"
        )));
    }

    #[test]
    fn is_valid_fixture_candidate_rejects_invalid() {
        // Absolute paths
        assert!(!is_valid_fixture_candidate(Path::new("/etc/passwd")));
        assert!(!is_valid_fixture_candidate(Path::new("/tmp/scratch")));
        assert!(!is_valid_fixture_candidate(Path::new(
            "/var/folders/xy/abc/T/scratch"
        )));
        // Parent-dir traversal would escape the workspace
        assert!(!is_valid_fixture_candidate(Path::new("../secret.txt")));
        assert!(!is_valid_fixture_candidate(Path::new("fixtures/../escape")));
        // Current-dir prefix doesn't round-trip cleanly through validate
        assert!(!is_valid_fixture_candidate(Path::new("./foo.txt")));
        assert!(!is_valid_fixture_candidate(Path::new("./src/lib.rs")));
    }
}
