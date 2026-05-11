# Eval Phase 7 — Paired-Comparison Judge Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Plan-file location:** Moved from `~/.claude/plans/start-work-on-steve-xa5u-twinkling-horizon.md` (Claude Code plan mode) to the canonical project location, matching `2026-05-07-eval-phase-6-data-foundation.md`.

## Context

Phase 6 (steve-tk30) just merged. It shipped the data layer: `Axis`, `Verdict`, `PairedScore`, `CompareVerdict`, `ScenarioScore`, `NormalizedTranscript`, `ResultsFile`, `BaselineFile`, the `Normalizer`, multi-run support, and the `steve eval baseline freeze` / `steve eval run` subcommands. **No judging happens yet.** Without Phase 7, paired-compare data exists on disk but nothing grades it.

Phase 7 builds the paired-comparison judge that consumes that data. Specifically, it adds `Judge::compare(...)` alongside the existing `Judge::evaluate(...)` (which stays untouched), wires the `[scoring].axes` override in `scenario.toml`, and ships a halo-mitigation prompt design (per-axis-rationale-before-verdict, A/B order randomization, tie as first-class).

Phase 8 (steve-u896) will then wire `compare` into a `steve eval report` subcommand and aggregate verdicts into the layered headline. Phase 7 is purely the building block — no orchestration changes, no new CLI surface, no aggregation. **The deliverable is `Judge::compare` returning plausible verdicts on hand-crafted pairs, plus the `[scoring]` parser, both with full unit-test coverage.**

**Goal:** Add `Judge::compare(baseline, current, axes, user_turns) -> anyhow::Result<CompareVerdict>` plus per-scenario `[scoring]` parsing, with halo-mitigation built into the prompt and full unit-test coverage on canned transcript pairs.

**Architecture:** Append to existing `src/eval/judge.rs` (do NOT split into a submodule — adds review noise for ~400 net new lines). Append to existing `src/eval/score.rs` for the `DEFAULT_AXES` constant. Modify `src/eval/scenario.rs` to add the `Scoring` struct and the optional `scoring: Option<Scoring>` field on `Scenario`. The `walking test` (`all_committed_scenarios_parse_and_validate`) auto-covers the new field across every committed scenario without code changes.

**Schema invariant (load-bearing — re-read the spec if tempted to deviate):** `rationale` precedes `verdict` in BOTH the wire format the LLM emits AND the Rust struct field order. The LLM is generative left-to-right; verdict-first defeats the chain-of-thought halo mitigation. The parser is **strict** about this: a response with verdict before rationale on any axis is rejected as malformed (`Err`, not `Ok` with a fallback verdict).

**A/B order randomization:** Each `Judge::compare` call randomizes which transcript is labeled "Transcript A" and which is "Transcript B" in the prompt — the LLM has no idea which is current vs baseline. The verdict letters the LLM emits are `a | b | tie` (neutral); the Rust code translates them to `Verdict::{CurrentWins, BaselineWins, Tie}` based on the per-call swap flag. The test seam is a private `compare_with_swap(swap: bool, ...)` helper that takes the flag explicitly so tests can pin both orderings deterministically; the public `compare` wraps it with `rand::random::<bool>()`.

**Tech Stack:** Rust 2024, existing `serde-saphyr` 0.0.26 for YAML parsing of judge response, existing `rand` 0.10 for A/B randomization, existing `async-openai` for LLM transport (via `JudgeBackend`), existing `serde_json` for a fallback display of malformed-response snippets. **No new dependencies.**

**Spec reference:** `docs/superpowers/specs/2026-05-06-eval-harness-paired-comparison-pivot.md` — particularly the "Judge" section (lines 425–522), the "Per-scenario axis override" subsection (lines 380–394), and "Phase 7 — Paired-Comparison Judge" (lines 948–966). Re-read end-to-end before starting.

**Ships-when (from spec, copied verbatim for sign-off):**

- `Judge::compare` returns plausible verdicts on hand-crafted pairs.
- The prompt is robust enough that swapping A/B in the same call produces inverted but otherwise consistent verdicts.

In addition (testable acceptance criteria derived from the spec):

- Unit tests cover: clear win on each axis, mixed verdicts (won correctness, lost efficiency), all-tie, baseline-wins, A/B-swap consistency, malformed responses (verdict before rationale, missing axis, unknown verdict variant, transport error).
- `scenario.toml` accepts an optional `[scoring]` block with `axes = [...]`; an unknown axis name (e.g., `"speed"`) fails at parse time with a clear error.
- The walking test (`all_committed_scenarios_parse_and_validate`) passes against all existing scenarios (none of which declare `[scoring]` yet — they inherit `DEFAULT_AXES`).

---

## File Structure

| File | Status | Responsibility |
|------|--------|----------------|
| `src/eval/score.rs` | modify | Add `pub const DEFAULT_AXES: [Axis; 3]`. |
| `src/eval/scenario.rs` | modify | Add `pub struct Scoring { axes: Vec<Axis> }`; add optional `scoring: Option<Scoring>` field on `Scenario` with `#[serde(default)]`; validate non-empty axes when present; add `Scenario::scoring_axes()` helper that returns the override-or-default slice. |
| `src/eval/judge.rs` | modify | Append: `COMPARE_SYSTEM_PROMPT` constant, `build_compare_user_prompt`, `parse_compare_response`, `Judge::compare`, private `Judge::compare_with_swap`, plus tests. **Do NOT touch existing `Judge::evaluate` code paths.** |
| `src/eval/mod.rs` | modify | Re-export `DEFAULT_AXES` and `Scoring` if not already accessible. |

**No new files.** The compare logic adds ~400 lines to `judge.rs` (currently 1064). At ~1450 lines post-Phase-7, the file is at the upper edge of what the project considers comfortable; the convention in CLAUDE.md ("`mod.rs` owns types and public API, submodules split by concern") suggests a future split of `judge.rs` into `judge/{evaluate.rs, compare.rs}` when Phase 8 adds aggregation. Defer that split — it would balloon the Phase 7 PR and Phase 8 has the natural moment for it.

---

## Task 1: Add `DEFAULT_AXES` constant

The constant is the single source of truth for "what does a scenario without a `[scoring]` block get judged on." Phase 6 deliberately did NOT add this constant ("No constant exists. Per spec, defaults are `[correctness, efficiency, conciseness]` — to be hardcoded or constified when Phase 7 wires `[scoring]` parsing"). Phase 7 wires both halves at once.

**Files:**
- Modify: `src/eval/score.rs`
- Modify: `src/eval/mod.rs` (re-export)

- [ ] **Step 1: Write the failing test**

In `src/eval/score.rs`, append to the `#[cfg(test)] mod tests {}` block:

```rust
    #[test]
    fn default_axes_are_correctness_efficiency_conciseness_in_that_order() {
        // Order is load-bearing: the judge prompt presents axes in this
        // order, and PairedScore Vec ordering follows it. A reorder here
        // would silently change the per-axis report sequence.
        assert_eq!(
            DEFAULT_AXES,
            [Axis::Correctness, Axis::Efficiency, Axis::Conciseness]
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib eval::score::tests::default_axes`
Expected: FAIL — `cannot find value 'DEFAULT_AXES' in this scope`.

- [ ] **Step 3: Add the constant**

In `src/eval/score.rs`, after the `Verdict` enum and before `pub type CompareVerdict = ...`:

```rust
/// Default axes a `Judge::compare` call grades on when a scenario
/// declares no `[scoring]` block. Order is load-bearing: the prompt
/// presents axes in this order, and per-axis reporting follows the
/// same order. Spec: "Default axes: correctness, efficiency,
/// conciseness."
pub const DEFAULT_AXES: [Axis; 3] = [
    Axis::Correctness,
    Axis::Efficiency,
    Axis::Conciseness,
];
```

- [ ] **Step 4: Re-export from `mod.rs`**

In `src/eval/mod.rs`, find the existing `pub use score::{...}` line and add `DEFAULT_AXES` to the list (alongside `Axis`, `Verdict`, `PairedScore`, etc.).

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib eval::score::tests::default_axes`
Expected: PASS.

- [ ] **Step 6: Run full test suite**

Run: `cargo test --lib eval::`
Expected: PASS — no regressions in existing eval tests.

- [ ] **Step 7: Commit**

```bash
git add src/eval/score.rs src/eval/mod.rs
git commit -m "$(cat <<'EOF'
feat(eval): add DEFAULT_AXES constant for paired-comparison judging

Phase 7 prep: scenarios without a [scoring] block are graded on
correctness, efficiency, conciseness in that order. Order is
load-bearing — the judge prompt and per-axis report both follow
the constant's ordering.

Refs: steve-xa5u
EOF
)"
```

---

## Task 2: `Scoring` struct + optional `[scoring]` field on `Scenario`

Mirrors the existing `Setup` pattern in `scenario.rs:65-75`: a small struct with `#[serde(default)]` + `deny_unknown_fields`, attached to `Scenario` via an optional field. Validation (non-empty axes) lands in `Scenario::validate()` alongside the existing user_turns / expectations checks.

The walking test (`all_committed_scenarios_parse_and_validate` in `scenario.rs:1207-1319`) automatically covers this — every committed scenario will be re-parsed against the new schema, and any that accidentally have `[scoring]` blocks parsed as unknown fields would have already failed Phase 6's `deny_unknown_fields`. (None do; this is hypothetical.)

**Files:**
- Modify: `src/eval/scenario.rs`
- Modify: `src/eval/mod.rs` (re-export `Scoring`)

- [ ] **Step 1: Write failing tests**

In `src/eval/scenario.rs`'s `#[cfg(test)] mod tests {}` block, add:

```rust
    #[test]
    fn scenario_without_scoring_block_uses_default_axes() {
        let toml = r#"
name = "x"
description = "x"
user_turns = ["go"]

[[expectations]]
kind = "tool_called"
tool = "read"
"#;
        let scenario = Scenario::from_toml_str(toml).unwrap();
        assert_eq!(scenario.scoring, None);
        assert_eq!(
            scenario.scoring_axes(),
            &[Axis::Correctness, Axis::Efficiency, Axis::Conciseness]
        );
    }

    #[test]
    fn scenario_with_scoring_override_returns_overridden_axes() {
        let toml = r#"
name = "x"
description = "x"
user_turns = ["go"]

[scoring]
axes = ["robustness", "efficiency"]

[[expectations]]
kind = "tool_called"
tool = "read"
"#;
        let scenario = Scenario::from_toml_str(toml).unwrap();
        let axes = scenario.scoring_axes();
        assert_eq!(axes, &[Axis::Robustness, Axis::Efficiency]);
    }

    #[test]
    fn scenario_rejects_unknown_axis_name_in_scoring() {
        // Closed enum: typos like "speed" must fail at load time, not
        // silently produce an unknown-axis judge prompt.
        let toml = r#"
name = "x"
description = "x"
user_turns = ["go"]

[scoring]
axes = ["speed"]

[[expectations]]
kind = "tool_called"
tool = "read"
"#;
        let err = Scenario::from_toml_str(toml).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("speed") || msg.contains("variant") || msg.contains("axes"),
            "expected error to mention the unknown axis name; got: {msg}"
        );
    }

    #[test]
    fn scenario_rejects_empty_scoring_axes() {
        let toml = r#"
name = "x"
description = "x"
user_turns = ["go"]

[scoring]
axes = []

[[expectations]]
kind = "tool_called"
tool = "read"
"#;
        let err = Scenario::from_toml_str(toml).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("axes") && (msg.contains("empty") || msg.contains("at least one")),
            "expected error about empty axes list; got: {msg}"
        );
    }

    #[test]
    fn scoring_block_rejects_unknown_field() {
        // deny_unknown_fields on Scoring catches typos like `axis` (singular).
        let toml = r#"
name = "x"
description = "x"
user_turns = ["go"]

[scoring]
axes = ["correctness"]
extra = "oops"

[[expectations]]
kind = "tool_called"
tool = "read"
"#;
        assert!(Scenario::from_toml_str(toml).is_err());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib eval::scenario::tests::scenario_without_scoring`
Expected: FAIL — `no field 'scoring' on type 'Scenario'`.

- [ ] **Step 3: Add `Scoring` struct**

In `src/eval/scenario.rs`, after the `Setup` struct (line ~75), add:

```rust
/// Per-scenario override of the default judging axes. When absent
/// (the common case), `Scenario::scoring_axes()` returns
/// `DEFAULT_AXES`. When present, `axes` must be non-empty —
/// validated by `Scenario::validate`.
///
/// `Axis` is a closed enum; serde rejects unknown axis names at
/// parse time so a typo in `scenario.toml` fails loud rather than
/// silently producing an unknown-axis judge prompt.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scoring {
    pub axes: Vec<Axis>,
}
```

Add the import at the top of the file (alongside other crate uses):

```rust
use crate::eval::score::{Axis, DEFAULT_AXES};
```

- [ ] **Step 4: Add `scoring` field on `Scenario`**

In `Scenario` (currently lines 32–55), append after `judge_model`:

```rust
    /// Optional override of the axes a `Judge::compare` call grades on
    /// for this scenario. When absent, `scoring_axes()` returns
    /// `DEFAULT_AXES`. Spec: "Per-scenario axis override — most
    /// scenarios inherit the defaults; the few with specialized
    /// lenses (postmortem-derived ones) declare their own."
    #[serde(default)]
    pub scoring: Option<Scoring>,
```

- [ ] **Step 5: Add `scoring_axes()` helper on `Scenario`**

In the `impl Scenario` block (around line 231), append:

```rust
    /// Returns the axes this scenario should be paired-compared on.
    /// Override-or-default: if `scoring` is present, returns its
    /// `axes` slice; otherwise returns `DEFAULT_AXES`.
    pub fn scoring_axes(&self) -> &[Axis] {
        match &self.scoring {
            Some(s) => &s.axes,
            None => &DEFAULT_AXES,
        }
    }
```

- [ ] **Step 6: Add validation for non-empty axes**

In `Scenario::validate()` (line 270), after the existing `expectations` checks, append:

```rust
        if let Some(scoring) = &self.scoring
            && scoring.axes.is_empty()
        {
            anyhow::bail!(
                "scenario {:?} has [scoring] block with empty `axes`; remove the block to use defaults, or list at least one axis",
                self.name
            );
        }
```

- [ ] **Step 7: Re-export `Scoring` from `mod.rs`**

In `src/eval/mod.rs`, find the existing `pub use scenario::{...}` line and add `Scoring` to the list.

- [ ] **Step 8: Run the new tests**

Run: `cargo test --lib eval::scenario::tests::scenario_with_scoring`
Run: `cargo test --lib eval::scenario::tests::scenario_without_scoring`
Run: `cargo test --lib eval::scenario::tests::scenario_rejects`
Run: `cargo test --lib eval::scenario::tests::scoring_block`
Expected: ALL PASS.

- [ ] **Step 9: Run the walking test**

Run: `cargo test --lib eval::scenario::tests::all_committed_scenarios_parse_and_validate`
Expected: PASS — no committed scenario currently declares `[scoring]`, so they all parse against the new schema unchanged.

- [ ] **Step 10: Run full test suite**

Run: `cargo test --lib eval::`
Expected: PASS.

- [ ] **Step 11: Commit**

```bash
git add src/eval/scenario.rs src/eval/mod.rs
git commit -m "$(cat <<'EOF'
feat(eval): wire [scoring].axes override in scenario.toml

Phase 7: Add `Scoring` struct + optional `scoring` field on
`Scenario`. `Scenario::scoring_axes()` returns the override
or DEFAULT_AXES. Empty axes list and unknown axis names both
fail at parse/validate time — closed-enum invariant from Phase 6
is now end-to-end.

Walking test still passes: no committed scenario declares
[scoring] yet; they all inherit the defaults.

Refs: steve-xa5u
EOF
)"
```

---

## Task 3: Compare prompt — system + user prompt builders

The prompt design is the load-bearing artifact of Phase 7. Three halo-mitigation invariants from the spec (lines 507–519):

1. **Per-axis-rationale-before-verdict**: rationale is required *before* verdict on each axis, in source order. The judge commits per-axis reasoning to text before naming a winner.
2. **Tie is first-class**: explicitly invited in the prompt; not a fallback when the judge can't decide.
3. **A/B order randomization**: prompt uses neutral `Transcript A` / `Transcript B` labels; verdict letters are `a | b | tie`. The Rust caller maps to `Verdict::{CurrentWins, BaselineWins, Tie}` based on the per-call swap flag.

The user prompt presents:
- The user_turns (the scenario inputs both transcripts were responding to).
- The list of axes the judge should grade on, with one-line definitions (so the judge knows what "efficiency" means in context: tool-call count and avoidance of redundant work, not token count).
- Transcript A's events (rendered in chronological order).
- Transcript B's events (same).
- The output schema with explicit key order constraint.

**Files:**
- Modify: `src/eval/judge.rs`

- [ ] **Step 1: Write failing test for `build_compare_user_prompt`**

In `src/eval/judge.rs`'s `mod tests`, append:

```rust
    fn make_test_transcript(events: Vec<TranscriptEvent>) -> NormalizedTranscript {
        NormalizedTranscript {
            events,
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
    fn compare_user_prompt_lists_requested_axes_in_order() {
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let prompt = build_compare_user_prompt(
            &baseline,
            &current,
            &[Axis::Correctness, Axis::Efficiency, Axis::Conciseness],
            &["go".to_string()],
            false, // swap = false → A is baseline, B is current
        );
        // Each axis must appear in the prompt in the requested order.
        let c = prompt.find("correctness").expect("correctness in prompt");
        let e = prompt.find("efficiency").expect("efficiency in prompt");
        let n = prompt.find("conciseness").expect("conciseness in prompt");
        assert!(c < e && e < n, "axes must be presented in requested order");
    }

    #[test]
    fn compare_user_prompt_swap_flag_swaps_a_and_b_assignment() {
        // With swap=false, Transcript A is baseline and B is current.
        // With swap=true, the labels invert. The two prompts must
        // differ — otherwise A/B randomization is a no-op.
        let baseline = make_test_transcript(vec![TranscriptEvent::AssistantMessage {
            text: "BASELINE_MARKER".into(),
        }]);
        let current = make_test_transcript(vec![TranscriptEvent::AssistantMessage {
            text: "CURRENT_MARKER".into(),
        }]);
        let unswapped = build_compare_user_prompt(
            &baseline,
            &current,
            &[Axis::Correctness],
            &[],
            false,
        );
        let swapped = build_compare_user_prompt(
            &baseline,
            &current,
            &[Axis::Correctness],
            &[],
            true,
        );
        // In unswapped, BASELINE appears in the A section (before CURRENT).
        let a_pos_un = unswapped.find("Transcript A").unwrap();
        let b_pos_un = unswapped.find("Transcript B").unwrap();
        let baseline_pos_un = unswapped.find("BASELINE_MARKER").unwrap();
        let current_pos_un = unswapped.find("CURRENT_MARKER").unwrap();
        assert!(a_pos_un < baseline_pos_un && baseline_pos_un < b_pos_un);
        assert!(b_pos_un < current_pos_un);
        // In swapped, CURRENT is in the A section and BASELINE in the B section.
        let a_pos_sw = swapped.find("Transcript A").unwrap();
        let b_pos_sw = swapped.find("Transcript B").unwrap();
        let baseline_pos_sw = swapped.find("BASELINE_MARKER").unwrap();
        let current_pos_sw = swapped.find("CURRENT_MARKER").unwrap();
        assert!(a_pos_sw < current_pos_sw && current_pos_sw < b_pos_sw);
        assert!(b_pos_sw < baseline_pos_sw);
    }

    #[test]
    fn compare_user_prompt_includes_user_turns() {
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let prompt = build_compare_user_prompt(
            &baseline,
            &current,
            &[Axis::Correctness],
            &["READ_THE_README".to_string(), "FOLLOW_UP_QUESTION".to_string()],
            false,
        );
        assert!(prompt.contains("READ_THE_README"));
        assert!(prompt.contains("FOLLOW_UP_QUESTION"));
    }

    #[test]
    fn compare_user_prompt_uses_neutral_a_b_labels_not_baseline_current() {
        // Halo invariant: the LLM must NOT see which transcript is
        // baseline vs current. If "baseline" or "current" leak into
        // the prompt body, position-bias mitigation is defeated.
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let prompt = build_compare_user_prompt(
            &baseline,
            &current,
            &[Axis::Correctness],
            &[],
            false,
        );
        // The schema description naturally uses these words for the
        // *output* keys, so we check the transcript-section labels
        // specifically. Walk to the "Transcript A" header and confirm
        // neither word appears in lowercase between A and B labels.
        let a_idx = prompt.find("Transcript A").unwrap();
        let b_idx = prompt.find("Transcript B").unwrap();
        let between = &prompt[a_idx..b_idx];
        assert!(
            !between.to_lowercase().contains("baseline"),
            "transcript section must not label its content 'baseline': {between}"
        );
        assert!(
            !between.to_lowercase().contains("current"),
            "transcript section must not label its content 'current': {between}"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib eval::judge::tests::compare_user_prompt`
Expected: FAIL — `cannot find function 'build_compare_user_prompt'` and `cannot find type 'TranscriptEvent'` (need imports).

- [ ] **Step 3: Add the system prompt constant**

In `src/eval/judge.rs`, after the existing `SYSTEM_PROMPT` constant (line ~95), add:

```rust
/// System prompt for `Judge::compare`. Three halo-mitigation invariants
/// are encoded here:
///
/// 1. **Per-axis-rationale-before-verdict** — the LLM emits its rationale
///    *before* the verdict on each axis. Because LLM output is generated
///    left-to-right, this forces per-axis reasoning to be committed to
///    text before any winner is named. Verdict-first would let the model
///    anchor on a winner and rationalize backward — defeating chain-of-
///    thought. The parser is strict about this order; reversed responses
///    are rejected.
///
/// 2. **Tie is first-class** — repeated explicitly so the model treats
///    "roughly equivalent" as the right call rather than forcing a
///    coin-flip between A and B.
///
/// 3. **A/B order randomization** — the user prompt uses neutral
///    "Transcript A" / "Transcript B" labels (the Rust caller swaps
///    which is current vs baseline per call); verdict letters in the
///    response are `a | b | tie`, never `current_wins | baseline_wins`.
///    The Rust caller maps `a`/`b` back to `Verdict::CurrentWins` /
///    `BaselineWins` based on the per-call swap flag.
const COMPARE_SYSTEM_PROMPT: &str = "\
You are an evaluator comparing two transcripts of an AI coding agent's \
behavior on the same task. You will be given:

1. The user turns the agent was responding to.
2. The list of axes to score on (with definitions).
3. Transcript A and Transcript B (chronological event sequences).

For each axis, in the order requested, output a YAML mapping with TWO \
keys IN THIS ORDER:

  rationale: <one or two sentences citing specific evidence from both \
             transcripts (turn numbers, tool names, message excerpts) \
             that support the chosen verdict>
  verdict:   a | b | tie

Halo-mitigation rules — follow these even if one transcript looks \
obviously better overall:

- Score each axis INDEPENDENTLY. Do not let a strong showing on one \
  axis bias your judgment on another.
- Emit the `rationale` BEFORE the `verdict` on each axis. Reversed \
  order will be rejected as malformed.
- If both transcripts are roughly equivalent on an axis, return `tie`. \
  Tie is a first-class verdict, NOT a fallback.

Output ONLY the YAML mapping with one top-level key per axis (in the \
requested order), no preamble, no markdown fences, no trailing \
commentary. Example shape:

correctness:
  rationale: \"...\"
  verdict: a
efficiency:
  rationale: \"...\"
  verdict: tie";
```

- [ ] **Step 4: Add `build_compare_user_prompt`**

After the existing `build_user_prompt` function (line ~681), append:

```rust
/// Render the user prompt for a paired-comparison judge call.
///
/// The `swap` flag determines which transcript is shown as "Transcript A":
/// `swap=false` → A=baseline, B=current; `swap=true` → A=current, B=baseline.
/// The flag is set by the caller via `rand::random::<bool>()` (or a
/// deterministic value in tests). The LLM never sees the original labels;
/// the verdict letters it emits (`a | b | tie`) are mapped back to
/// `Verdict::CurrentWins` / `BaselineWins` by `parse_compare_response`,
/// taking `swap` into account.
pub(crate) fn build_compare_user_prompt(
    baseline: &NormalizedTranscript,
    current: &NormalizedTranscript,
    axes: &[Axis],
    user_turns: &[String],
    swap: bool,
) -> String {
    let mut out = String::new();

    // 1. User turns.
    out.push_str("USER TURNS — the prompts both transcripts were responding to:\n");
    if user_turns.is_empty() {
        out.push_str("  (no user turns recorded)\n");
    } else {
        for (i, turn) in user_turns.iter().enumerate() {
            out.push_str(&format!("  {}. {}\n", i + 1, turn));
        }
    }
    out.push('\n');

    // 2. Axes with one-line definitions, in the requested order.
    out.push_str("AXES TO SCORE — output one YAML block per axis, in this order:\n");
    for axis in axes {
        let definition = match axis {
            Axis::Correctness =>
                "did the agent produce the right outcome for the user's task?",
            Axis::Efficiency =>
                "did the agent achieve the outcome with fewer/better tool calls (no \
                 redundant work, no thrashing)?",
            Axis::Conciseness =>
                "were the agent's assistant messages succinct and on-point (no \
                 padding, no repetition)?",
            Axis::Robustness =>
                "did the agent handle errors and unexpected results well, or did it \
                 spin / give up / make things worse?",
            Axis::Truthfulness =>
                "did the agent ground its claims in actual tool output, without \
                 fabrication or hallucinated content?",
        };
        out.push_str(&format!("  - {axis}: {definition}\n"));
    }
    out.push('\n');

    // 3 & 4. Transcripts A and B.
    let (a, b) = if swap { (current, baseline) } else { (baseline, current) };
    out.push_str("Transcript A — events in chronological order:\n");
    render_transcript_events(a, &mut out);
    out.push('\n');
    out.push_str("Transcript B — events in chronological order:\n");
    render_transcript_events(b, &mut out);
    out.push('\n');

    out.push_str(
        "Now produce the YAML mapping. Remember: rationale BEFORE verdict on each \
         axis; tie is allowed; output ONLY the mapping with no preamble.\n",
    );

    out
}

/// Render a `NormalizedTranscript`'s event sequence into the user prompt.
/// Trims long tool outputs and assistant messages to the same caps used
/// by `build_user_prompt` so the two judge prompts stay comparable in
/// token cost.
fn render_transcript_events(t: &NormalizedTranscript, out: &mut String) {
    if t.events.is_empty() {
        out.push_str("  (no events)\n");
        return;
    }
    for (i, event) in t.events.iter().enumerate() {
        match event {
            TranscriptEvent::ToolCall { tool_name, arguments } => {
                let mut compact = String::new();
                format_args_compact(arguments, MAX_TOOL_ARGS_CHARS, &mut compact);
                let args = truncate_chars(&compact, MAX_TOOL_ARGS_CHARS);
                out.push_str(&format!(
                    "  {}. tool_call {}({})\n",
                    i + 1,
                    tool_name.as_str(),
                    args
                ));
            }
            TranscriptEvent::ToolResult { tool_name, output, is_error } => {
                let prefix = if *is_error { "(error) " } else { "" };
                let body = truncate_chars(output, MAX_TOOL_OUTPUT_CHARS);
                out.push_str(&format!(
                    "  {}. tool_result {} -> {}{}\n",
                    i + 1,
                    tool_name.as_str(),
                    prefix,
                    body
                ));
            }
            TranscriptEvent::AssistantMessage { text } => {
                let body = truncate_chars(text, MAX_ASSISTANT_MSG_CHARS);
                let indented = body.replace('\n', "\n      ");
                out.push_str(&format!("  {}. assistant_message:\n      {indented}\n", i + 1));
            }
        }
    }
}
```

- [ ] **Step 5: Add the necessary imports at the top of `judge.rs`**

If not already imported (Phase 6 may have added some), ensure these are present in `judge.rs`:

```rust
use crate::eval::{
    score::{Axis, DEFAULT_AXES},
    transcript::{NormalizedTranscript, TranscriptEvent},
};
```

(`DEFAULT_AXES` will be used in the public `Judge::compare` wrapper in Task 5; pre-importing now keeps Task 5 small.)

- [ ] **Step 6: Make `format_args_compact` accessible**

The existing `build_user_prompt` calls `format_args_compact` (a private helper in the same module). The new `render_transcript_events` reuses it. Verify the helper is `fn format_args_compact(...)` (module-private), which is sufficient — both functions live in `judge.rs`.

If `format_args_compact` is currently inlined inside `build_user_prompt`, extract it to a module-level `fn` first, in a separate commit before Task 3's main commit. Run `grep -n 'fn format_args_compact' src/eval/judge.rs` to check.

- [ ] **Step 7: Run the prompt-builder tests**

Run: `cargo test --lib eval::judge::tests::compare_user_prompt`
Expected: ALL PASS.

- [ ] **Step 8: Run the full judge test suite (regression check)**

Run: `cargo test --lib eval::judge::`
Expected: PASS — existing `Judge::evaluate` tests untouched.

- [ ] **Step 9: Commit**

```bash
git add src/eval/judge.rs
git commit -m "$(cat <<'EOF'
feat(eval): add compare prompt builder for paired-comparison judge

Phase 7: COMPARE_SYSTEM_PROMPT encodes the three halo-mitigation
invariants (rationale-before-verdict, tie-as-first-class, A/B
randomization). build_compare_user_prompt renders user turns +
axis list with definitions + Transcript A/B sections, swapping
A and B based on the per-call flag.

Tests cover: axis order in the prompt, swap-flag inversion,
user_turns presence, and the no-leak invariant (the words
"baseline"/"current" never appear in the transcript labels —
preserves position-bias mitigation).

Refs: steve-xa5u
EOF
)"
```

---

## Task 4: Compare response parser — `parse_compare_response`

The parser is **strict** about three things:
1. **Schema**: every requested axis must appear; unknown axes (LLM hallucinates an axis we didn't ask for) are an error; unknown verdict letters are an error.
2. **Order within each axis**: `rationale` appears before `verdict` in the source text. Verdict-first is rejected as malformed.
3. **Verdict translation**: `a` and `b` are mapped to `CurrentWins` or `BaselineWins` based on the swap flag (passed alongside the raw text). `tie` is `Verdict::Tie` regardless of swap.

Returns `anyhow::Result<CompareVerdict>`. All schema/order failures propagate as `Err` so the caller (`Judge::compare_with_swap`) can attach a snippet of the raw response for diagnostics.

**Files:**
- Modify: `src/eval/judge.rs`

- [ ] **Step 1: Write failing tests**

Append to `judge.rs`'s `mod tests`:

```rust
    fn requested_axes() -> Vec<Axis> {
        vec![Axis::Correctness, Axis::Efficiency, Axis::Conciseness]
    }

    #[test]
    fn parse_compare_response_returns_per_axis_verdicts_in_requested_order() {
        let raw = r#"
correctness:
  rationale: "A handles the edge case; B does not."
  verdict: a
efficiency:
  rationale: "A made one fewer tool call."
  verdict: a
conciseness:
  rationale: "Both transcripts were similarly succinct."
  verdict: tie
"#;
        // swap=false → A is baseline, so verdict=a → BaselineWins;
        // verdict=tie → Tie regardless of swap.
        let scores = parse_compare_response(raw, &requested_axes(), false).unwrap();
        assert_eq!(scores.len(), 3);
        assert_eq!(scores[0].axis, Axis::Correctness);
        assert_eq!(scores[0].verdict, Verdict::BaselineWins);
        assert!(scores[0].rationale.contains("edge case"));
        assert_eq!(scores[1].axis, Axis::Efficiency);
        assert_eq!(scores[1].verdict, Verdict::BaselineWins);
        assert_eq!(scores[2].axis, Axis::Conciseness);
        assert_eq!(scores[2].verdict, Verdict::Tie);
    }

    #[test]
    fn parse_compare_response_translates_verdict_letters_with_swap_flag() {
        // Same raw response, swap=true → A is current, so verdict=a → CurrentWins.
        let raw = r#"
correctness:
  rationale: "A wins on this axis."
  verdict: a
efficiency:
  rationale: "B is more efficient."
  verdict: b
conciseness:
  rationale: "Tie."
  verdict: tie
"#;
        let scores = parse_compare_response(raw, &requested_axes(), true).unwrap();
        assert_eq!(scores[0].verdict, Verdict::CurrentWins);    // a + swap=true
        assert_eq!(scores[1].verdict, Verdict::BaselineWins);   // b + swap=true
        assert_eq!(scores[2].verdict, Verdict::Tie);
        // Verify the unswapped translation for sanity:
        let scores2 = parse_compare_response(raw, &requested_axes(), false).unwrap();
        assert_eq!(scores2[0].verdict, Verdict::BaselineWins);
        assert_eq!(scores2[1].verdict, Verdict::CurrentWins);
        assert_eq!(scores2[2].verdict, Verdict::Tie);
    }

    #[test]
    fn parse_compare_response_rejects_verdict_before_rationale() {
        // Halo-mitigation invariant: the strict order check.
        let raw = r#"
correctness:
  verdict: a
  rationale: "out-of-order"
efficiency:
  rationale: "ok"
  verdict: tie
conciseness:
  rationale: "ok"
  verdict: tie
"#;
        let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("rationale") && msg.contains("verdict"),
            "expected error to mention key order; got: {msg}"
        );
        assert!(msg.contains("correctness"), "expected error to name the offending axis; got: {msg}");
    }

    #[test]
    fn parse_compare_response_rejects_missing_axis() {
        // We asked for 3 axes; LLM only emitted 2.
        let raw = r#"
correctness:
  rationale: "ok"
  verdict: a
efficiency:
  rationale: "ok"
  verdict: tie
"#;
        let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("conciseness"),
            "expected error to name the missing axis; got: {msg}"
        );
    }

    #[test]
    fn parse_compare_response_rejects_unknown_verdict_letter() {
        let raw = r#"
correctness:
  rationale: "garbage"
  verdict: maybe
efficiency:
  rationale: "ok"
  verdict: a
conciseness:
  rationale: "ok"
  verdict: tie
"#;
        let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.to_lowercase().contains("verdict") || msg.contains("maybe"),
            "expected error to flag the unknown verdict letter; got: {msg}"
        );
    }

    #[test]
    fn parse_compare_response_rejects_extra_unrequested_axis() {
        // LLM hallucinated a "speed" axis we didn't ask for.
        let raw = r#"
correctness:
  rationale: "ok"
  verdict: a
efficiency:
  rationale: "ok"
  verdict: a
conciseness:
  rationale: "ok"
  verdict: a
speed:
  rationale: "ok"
  verdict: a
"#;
        let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("speed") || msg.contains("unexpected"), "got: {msg}");
    }

    #[test]
    fn parse_compare_response_strips_markdown_fences() {
        // Some models wrap structured output in ```yaml ... ``` despite
        // the prompt saying not to. The existing strip_markdown_fences
        // helper handles this; reuse it.
        let raw = "```yaml\ncorrectness:\n  rationale: \"ok\"\n  verdict: a\nefficiency:\n  rationale: \"ok\"\n  verdict: a\nconciseness:\n  rationale: \"ok\"\n  verdict: tie\n```";
        let scores = parse_compare_response(raw, &requested_axes(), false).unwrap();
        assert_eq!(scores.len(), 3);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib eval::judge::tests::parse_compare_response`
Expected: FAIL — `cannot find function 'parse_compare_response'`.

- [ ] **Step 3: Implement `parse_compare_response`**

In `src/eval/judge.rs`, after `build_judge_outcome` (line ~756), append:

```rust
/// Strict parser for the YAML response from `Judge::compare`.
///
/// Three layers of validation:
///
/// 1. **Order check** (positional, on the raw text BEFORE
///    deserialization): for each requested axis, locate the axis
///    key in the source, then within that axis's region locate
///    `rationale` and `verdict`. Reject if `verdict` appears first.
///    This enforces the halo-mitigation invariant — a deserializer
///    that re-orders fields would silently strip it.
///
/// 2. **Schema deserialization** via `serde-saphyr`: deserialize
///    into `BTreeMap<String, RawAxisResponse>`. Empty rationale is
///    allowed at the parser level (the judge prompt asks for
///    non-empty, but a single brief rationale is still useful);
///    unknown verdict letters fail via the `RawVerdict` enum's
///    `deny_unknown_fields`-style derive.
///
/// 3. **Coverage check**: every requested axis must appear; no
///    extra axes the LLM hallucinated may appear.
///
/// On success, translates verdict letters `a|b|tie` to
/// `Verdict::{CurrentWins, BaselineWins, Tie}` based on the per-call
/// `swap` flag. Returns scores in the requested-axes order (NOT the
/// LLM's emit order), matching `DEFAULT_AXES` semantics.
pub(crate) fn parse_compare_response(
    raw: &str,
    requested_axes: &[Axis],
    swap: bool,
) -> Result<CompareVerdict> {
    let stripped = strip_markdown_fences(raw);

    // 1. Order check. Must run on the raw stripped text, before any
    //    parser potentially re-orders mappings.
    enforce_rationale_before_verdict(stripped, requested_axes)?;

    // 2. Schema deserialization.
    let parsed: std::collections::BTreeMap<String, RawAxisResponse> =
        serde_saphyr::from_str(stripped)
            .with_context(|| {
                let snippet = truncate_chars(raw, MAX_RAW_RESPONSE_IN_REASON);
                format!("compare response did not parse as YAML; raw: {snippet}")
            })?;

    // 3a. Every requested axis present.
    let mut out = Vec::with_capacity(requested_axes.len());
    for axis in requested_axes {
        let key = format!("{axis}");
        let raw_resp = parsed.get(&key).ok_or_else(|| {
            anyhow::anyhow!("compare response missing axis {key:?}")
        })?;
        let verdict = match raw_resp.verdict {
            RawVerdict::A => if swap { Verdict::CurrentWins } else { Verdict::BaselineWins },
            RawVerdict::B => if swap { Verdict::BaselineWins } else { Verdict::CurrentWins },
            RawVerdict::Tie => Verdict::Tie,
        };
        out.push(PairedScore {
            axis: *axis,
            rationale: raw_resp.rationale.clone(),
            verdict,
        });
    }

    // 3b. No extra unrequested axes.
    let requested_keys: std::collections::BTreeSet<String> =
        requested_axes.iter().map(|a| format!("{a}")).collect();
    for key in parsed.keys() {
        if !requested_keys.contains(key) {
            anyhow::bail!("compare response has unexpected axis key {key:?}");
        }
    }

    Ok(out)
}

/// Schema of one axis's slice of the raw judge response. Field order
/// reflects what the prompt asks the LLM to emit (rationale BEFORE
/// verdict). The serde derive's tolerance of either order is
/// intentional — `enforce_rationale_before_verdict` does the order
/// check separately, on the source text, before this struct sees the
/// data.
#[derive(Debug, Deserialize)]
struct RawAxisResponse {
    rationale: String,
    verdict: RawVerdict,
}

/// Verdict letters as the LLM emits them. `serde(rename_all)` matches
/// the lowercased letters in the prompt schema; an unknown letter
/// fails to deserialize and propagates as `Err` from
/// `parse_compare_response`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawVerdict {
    A,
    B,
    Tie,
}

/// Positional check: for each requested axis key, find its position
/// in `raw`; then within the slice from that key to the next axis key
/// (or end-of-string), assert `rationale` precedes `verdict`. Returns
/// `Err` naming the first offending axis on violation.
///
/// Operates on the raw text (after markdown-fence stripping) so it
/// catches verdict-first responses regardless of how serde-saphyr
/// orders mapping iteration.
fn enforce_rationale_before_verdict(stripped: &str, requested_axes: &[Axis]) -> Result<()> {
    // Build the list of (axis, key) so we can walk in declared order.
    let keys: Vec<String> = requested_axes.iter().map(|a| format!("{a}")).collect();

    for (i, key) in keys.iter().enumerate() {
        // Locate the axis key as a line-leading token (e.g., "correctness:").
        // Searching for "<key>:" anchored at line start avoids false matches
        // on "...correctness..." inside a rationale string.
        let key_line = format!("{key}:");
        let axis_start = match find_line_start_token(stripped, &key_line) {
            Some(p) => p,
            None => continue, // missing-axis is caught by the schema layer; not our job here
        };
        // End of this axis's region: the start of the next axis key, or
        // end-of-string.
        let axis_end = keys[i + 1..]
            .iter()
            .filter_map(|k| find_line_start_token(stripped, &format!("{k}:")))
            .filter(|&p| p > axis_start)
            .min()
            .unwrap_or(stripped.len());
        let region = &stripped[axis_start..axis_end];

        let r_pos = region.find("rationale");
        let v_pos = region.find("verdict");
        if let (Some(r), Some(v)) = (r_pos, v_pos)
            && r >= v
        {
            anyhow::bail!(
                "compare response on axis {key:?} has `verdict` before `rationale`; \
                 the prompt requires rationale-before-verdict for halo-mitigation"
            );
        }
    }
    Ok(())
}

/// Find the byte offset of `token` only when it appears at the start
/// of a line (after optional whitespace from a multi-line YAML
/// indentation context). Used by `enforce_rationale_before_verdict`
/// to locate axis-key headers without false-matching on the same word
/// inside a rationale string body.
fn find_line_start_token(haystack: &str, token: &str) -> Option<usize> {
    // First line: no preceding newline.
    if haystack.starts_with(token) {
        return Some(0);
    }
    let needle = format!("\n{token}");
    haystack.find(&needle).map(|p| p + 1) // +1 to skip the newline
}
```

- [ ] **Step 4: Add necessary imports**

At the top of `judge.rs`, ensure these are imported (some may already be present from Phase 6 paths or Task 3):

```rust
use crate::eval::score::{Axis, CompareVerdict, PairedScore, Verdict};
```

- [ ] **Step 5: Run the parser tests**

Run: `cargo test --lib eval::judge::tests::parse_compare_response`
Expected: ALL PASS.

- [ ] **Step 6: Run full eval test suite**

Run: `cargo test --lib eval::`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/eval/judge.rs
git commit -m "$(cat <<'EOF'
feat(eval): strict parser for paired-comparison judge response

Phase 7: parse_compare_response validates the YAML response from
the compare prompt in three layers:

1. Positional check enforces rationale-before-verdict on each axis,
   on the raw text before deserialization. Verdict-first is rejected
   as malformed (load-bearing for halo mitigation).
2. Schema deserialization via serde-saphyr into a BTreeMap<axis,
   RawAxisResponse>. Unknown verdict letters fail at deserialize time.
3. Coverage check: every requested axis must appear; the LLM may
   not introduce axes we didn't ask for.

Verdict letters `a` and `b` are translated to CurrentWins/BaselineWins
based on the per-call swap flag; `tie` maps to Verdict::Tie regardless.

Refs: steve-xa5u
EOF
)"
```

---

## Task 5: `Judge::compare` public method + `compare_with_swap` helper

The orchestrator: build the prompt, call the backend, parse the response. Wraps the test seam (`compare_with_swap`) with a public `compare` that randomizes the swap flag via `rand::random::<bool>()`.

The public method takes `&[Axis]` so the caller (Phase 8 will be `eval report`) can pass `scenario.scoring_axes()` directly. `user_turns` is `&[String]` (from `BaselineFile.user_turns` or `ScenarioResults.user_turns` per spec line 458–464).

**Files:**
- Modify: `src/eval/judge.rs`

- [ ] **Step 1: Write failing tests**

Append to `judge.rs`'s `mod tests`:

```rust
    #[tokio::test]
    async fn compare_returns_paired_scores_for_each_requested_axis() {
        // Canonical happy path: backend returns a well-formed YAML
        // response; compare returns CompareVerdict in axis order.
        let canned = r#"
correctness:
  rationale: "A is more correct."
  verdict: a
efficiency:
  rationale: "Tie on efficiency."
  verdict: tie
conciseness:
  rationale: "B was more concise."
  verdict: b
"#;
        let backend = MockBackend::new(vec![ok_response(canned)]);
        let judge = Judge::with_backend(Box::new(backend), Some("test/model"));
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        // Use the deterministic test seam (swap=false).
        let scores = judge
            .compare_with_swap(
                &baseline,
                &current,
                &[Axis::Correctness, Axis::Efficiency, Axis::Conciseness],
                &["go".to_string()],
                false,
                None, // no scenario_judge_model
                None, // no expectation_judge_model
            )
            .await
            .unwrap();
        assert_eq!(scores.len(), 3);
        assert_eq!(scores[0].verdict, Verdict::BaselineWins);  // a + swap=false → BaselineWins
        assert_eq!(scores[1].verdict, Verdict::Tie);
        assert_eq!(scores[2].verdict, Verdict::CurrentWins);   // b + swap=false → CurrentWins
    }

    #[tokio::test]
    async fn compare_with_swap_inverts_a_and_b_to_baseline_and_current() {
        // Same response, but swap=true. verdict=a → CurrentWins;
        // verdict=b → BaselineWins; verdict=tie → Tie.
        let canned = r#"
correctness:
  rationale: "A is more correct."
  verdict: a
efficiency:
  rationale: "Tie on efficiency."
  verdict: tie
conciseness:
  rationale: "B was more concise."
  verdict: b
"#;
        let backend = MockBackend::new(vec![ok_response(canned)]);
        let judge = Judge::with_backend(Box::new(backend), Some("test/model"));
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let scores = judge
            .compare_with_swap(
                &baseline,
                &current,
                &[Axis::Correctness, Axis::Efficiency, Axis::Conciseness],
                &[],
                true,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(scores[0].verdict, Verdict::CurrentWins);
        assert_eq!(scores[1].verdict, Verdict::Tie);
        assert_eq!(scores[2].verdict, Verdict::BaselineWins);
    }

    #[tokio::test]
    async fn compare_propagates_transport_error_as_err() {
        // Spec: "two distinct failure modes — the LLM call can fail
        // (transport, API rate limits, timeouts), and the parser is
        // strict... infrastructure failures propagate as Err."
        let backend = MockBackend::new(vec![Err(anyhow::anyhow!("simulated 503"))]);
        let judge = Judge::with_backend(Box::new(backend), Some("test/model"));
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let result = judge
            .compare_with_swap(
                &baseline,
                &current,
                &[Axis::Correctness, Axis::Efficiency, Axis::Conciseness],
                &[],
                false,
                None,
                None,
            )
            .await;
        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("503") || msg.contains("simulated"));
    }

    #[tokio::test]
    async fn compare_with_swap_yields_inverted_verdicts_on_clear_winner() {
        // A/B-swap consistency: the same underlying transcript pair,
        // judged twice in opposite swap orderings, must yield inverted
        // verdicts on a clear-winner axis (and tie stays tie).
        //
        // We simulate this via TWO identical responses where the
        // semantic content is "A clearly wins correctness" — but with
        // swap=false the "A=baseline" reading gives BaselineWins, and
        // with swap=true the "A=current" reading gives CurrentWins.
        let canned = r#"
correctness:
  rationale: "A is clearly correct."
  verdict: a
efficiency:
  rationale: "Tie."
  verdict: tie
conciseness:
  rationale: "Tie."
  verdict: tie
"#;
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let axes = [Axis::Correctness, Axis::Efficiency, Axis::Conciseness];

        let backend1 = MockBackend::new(vec![ok_response(canned)]);
        let judge1 = Judge::with_backend(Box::new(backend1), Some("test/model"));
        let scores_unswapped = judge1
            .compare_with_swap(&baseline, &current, &axes, &[], false, None, None)
            .await
            .unwrap();

        let backend2 = MockBackend::new(vec![ok_response(canned)]);
        let judge2 = Judge::with_backend(Box::new(backend2), Some("test/model"));
        let scores_swapped = judge2
            .compare_with_swap(&baseline, &current, &axes, &[], true, None, None)
            .await
            .unwrap();

        // On the clear-winner axis (correctness), verdicts invert.
        assert_eq!(scores_unswapped[0].verdict, Verdict::BaselineWins);
        assert_eq!(scores_swapped[0].verdict, Verdict::CurrentWins);
        // Ties stay ties under swap.
        assert_eq!(scores_unswapped[1].verdict, Verdict::Tie);
        assert_eq!(scores_swapped[1].verdict, Verdict::Tie);
        assert_eq!(scores_unswapped[2].verdict, Verdict::Tie);
        assert_eq!(scores_swapped[2].verdict, Verdict::Tie);
    }

    #[tokio::test]
    async fn compare_returns_err_when_no_judge_model_configured() {
        // Mirrors the existing evaluate behavior at judge.rs:273-289 —
        // no judge model from any source is a hard error. For compare,
        // the spec wants this as Err (infrastructure failure), not as
        // an inline verdict.
        let backend = MockBackend::new(vec![]);
        let judge = Judge::with_backend(Box::new(backend), None); // no CLI model
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let result = judge
            .compare_with_swap(
                &baseline,
                &current,
                &[Axis::Correctness, Axis::Efficiency, Axis::Conciseness],
                &[],
                false,
                None, // no scenario model
                None, // no expectation model
            )
            .await;
        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("judge model"), "got: {msg}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib eval::judge::tests::compare`
Expected: FAIL — `no method named 'compare_with_swap'`.

- [ ] **Step 3: Implement `Judge::compare` and `Judge::compare_with_swap`**

In the `impl<'a> Judge<'a>` block in `src/eval/judge.rs` (line ~233), append after the existing `evaluate` method:

```rust
    /// Paired-compare two transcripts on the given axes. Returns
    /// `anyhow::Result<CompareVerdict>` per spec: infrastructure
    /// failures (transport, parser strictness, no judge model) are
    /// `Err`; per-axis judge opinions are inside the `Ok` variant.
    ///
    /// `axes` is the ordered list of dimensions to grade on (typically
    /// `scenario.scoring_axes()` from `Scenario::scoring_axes`, which
    /// returns either the per-scenario override or `DEFAULT_AXES`).
    /// `user_turns` is the scenario's input prompts; both transcripts
    /// were responding to these (passed separately rather than read
    /// from either transcript because they're scenario-level data).
    ///
    /// A/B order is randomized per call (`rand::random::<bool>()`) to
    /// neutralize position bias. The randomization is opaque at this
    /// API level — call `compare_with_swap` directly only from tests
    /// that need deterministic A/B ordering.
    pub async fn compare(
        &self,
        baseline: &NormalizedTranscript,
        current: &NormalizedTranscript,
        axes: &[Axis],
        user_turns: &[String],
        scenario_judge_model: Option<&str>,
        expectation_judge_model: Option<&str>,
    ) -> Result<CompareVerdict> {
        let swap = rand::random::<bool>();
        self.compare_with_swap(
            baseline,
            current,
            axes,
            user_turns,
            swap,
            scenario_judge_model,
            expectation_judge_model,
        )
        .await
    }

    /// Test-and-internal entry point with explicit `swap` control.
    /// Tests call this directly; `compare` is the production wrapper
    /// that chooses `swap` randomly.
    pub(crate) async fn compare_with_swap(
        &self,
        baseline: &NormalizedTranscript,
        current: &NormalizedTranscript,
        axes: &[Axis],
        user_turns: &[String],
        swap: bool,
        scenario_judge_model: Option<&str>,
        expectation_judge_model: Option<&str>,
    ) -> Result<CompareVerdict> {
        let model = resolve_judge_model(
            self.cli_model.as_deref(),
            expectation_judge_model,
            scenario_judge_model,
        )
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no judge model configured: pass --judge-model on the CLI, \
                 or set judge_model on the scenario or per-expectation"
            )
        })?
        .to_string();

        let user_prompt = build_compare_user_prompt(baseline, current, axes, user_turns, swap);
        let (raw, _usage) = self
            .backend
            .complete(&model, COMPARE_SYSTEM_PROMPT, &user_prompt)
            .await
            .context("compare judge call failed")?;

        if raw.trim().is_empty() {
            anyhow::bail!("compare judge returned empty response (model: {model})");
        }
        parse_compare_response(&raw, axes, swap)
    }
```

- [ ] **Step 4: Run the compare tests**

Run: `cargo test --lib eval::judge::tests::compare`
Expected: ALL PASS.

- [ ] **Step 5: Run full eval test suite**

Run: `cargo test --lib eval::`
Expected: PASS.

- [ ] **Step 6: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: PASS — no new warnings (CI requires this).

- [ ] **Step 7: Commit**

```bash
git add src/eval/judge.rs
git commit -m "$(cat <<'EOF'
feat(eval): Judge::compare for paired-comparison grading

Phase 7 (steve-xa5u): add Judge::compare alongside the existing
Judge::evaluate. Single call per (scenario, run) pair, returns
anyhow::Result<CompareVerdict> with per-axis verdicts.

A/B order randomization via rand::random::<bool>() per call.
The compare_with_swap private helper is the deterministic test
seam — production code goes through compare which randomizes;
tests pin both orderings via direct compare_with_swap calls.

Failure modes per spec: infrastructure errors (transport, parser
strictness, missing judge model, empty response) propagate as
Err; per-axis judge opinions encode their result inside Ok.

Refs: steve-xa5u
EOF
)"
```

---

## Task 6: Final integration check + run any cross-cutting tests

Phase 7 ships `Judge::compare` as a building block. There's no orchestration call site yet (Phase 8 will wire `eval report`). Sanity-check that nothing in the workspace got accidentally broken.

**Files:**
- (No new code — verification step.)

- [ ] **Step 1: Run the full `cargo test` (not just `--lib`)**

Run: `cargo test`
Expected: ALL PASS — both unit tests and integration tests.

- [ ] **Step 2: Run clippy with cargo-level lints**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 3: Run nightly format**

Run: `cargo +nightly fmt`
Then: `cargo +nightly fmt --check` (verify the working tree is clean post-format).

- [ ] **Step 4: Run the eval-walking-test alone for sanity**

Run: `cargo test --lib all_committed_scenarios_parse_and_validate`
Expected: PASS — every committed `scenario.toml` parses and validates against the post-Phase-7 `Scenario` shape.

- [ ] **Step 5: Manually inspect a generated compare prompt (optional)**

If desired, add a temporary `#[ignore]` test that prints `build_compare_user_prompt` output for a non-trivial transcript pair, run it with `cargo test -- --ignored --nocapture`, eyeball the output, then DELETE the test before committing. (Spec phase ships-when criterion #1: "Judge::compare returns plausible verdicts on hand-crafted pairs" — this is the visual sanity check.)

- [ ] **Step 6: Update beads issue**

```bash
bd update steve-xa5u --notes "Phase 7 done: Judge::compare + [scoring] block parsing landed on feat/eval-harness. Ships-when criteria met. Phase 8 (steve-u896) is unblocked."
bd close steve-xa5u
```

- [ ] **Step 7: Push branch**

```bash
git push
```

(The `feat/eval-harness` branch is the long-lived integration branch per spec line 911. Phase 8 will land directly on it. No PR-to-main yet — final consolidated review of the whole eval epic happens before the merge to main.)

---

## Verification

End-to-end smoke test (manual, post-merge):

```bash
# 1. Land Phase 7 on feat/eval-harness.
git switch feat/eval-harness && git pull

# 2. Confirm the new types compile and the [scoring] override works.
cargo test --lib eval::

# 3. Confirm a real scenario.toml accepts an optional [scoring] block.
#    Add it temporarily to one of the postmortem-derived scenarios
#    (e.g., stop-guessing-after-failures), parse it, then revert:
echo '[scoring]' >> eval/scenarios/stop-guessing-after-failures/scenario.toml
echo 'axes = ["robustness", "efficiency"]' >> eval/scenarios/stop-guessing-after-failures/scenario.toml
cargo test --lib all_committed_scenarios_parse_and_validate
git checkout eval/scenarios/stop-guessing-after-failures/scenario.toml

# 4. There is no `steve eval report` yet — that's Phase 8.
#    Phase 7 is verified by the test suite alone.
```

---

## Self-Review Checklist (run before declaring done)

- [ ] Spec coverage: every Phase 7 ships-when criterion (lines 962–966) maps to a passing test in this plan? `Judge::compare returns plausible verdicts` → Task 5's `compare_returns_paired_scores...`. `Swapping A/B yields inverted verdicts` → Task 5's `compare_with_swap_yields_inverted_verdicts_on_clear_winner`.
- [ ] Spec coverage: per-scenario axis override (line 380) implemented and tested? Task 2.
- [ ] Spec coverage: halo-mitigation invariants (lines 507–519)? Per-axis-rationale-before-verdict → Task 4 parser + COMPARE_SYSTEM_PROMPT. Tie as first-class → COMPARE_SYSTEM_PROMPT. A/B randomization → Task 5 (rand::random) + test seam.
- [ ] Spec coverage: Result<CompareVerdict> failure shape (lines 441–456)? Tested in Task 5.
- [ ] No placeholders: every "TODO", "TBD", "implement later" replaced with concrete content? Yes.
- [ ] Type consistency: `compare_with_swap` signature in Task 5 matches calls in Task 5's test bodies? Yes — 7 args (`baseline`, `current`, `axes`, `user_turns`, `swap`, `scenario_judge_model`, `expectation_judge_model`).
- [ ] All function names referenced are defined in the same plan (no forward refs to undeclared helpers): `build_compare_user_prompt` (Task 3), `parse_compare_response` (Task 4), `Judge::compare_with_swap` (Task 5), `enforce_rationale_before_verdict` (Task 4), `find_line_start_token` (Task 4), `render_transcript_events` (Task 3), `format_args_compact` (existing, see Task 3 step 6).
- [ ] CLAUDE.md adherence: tests use exhaustive matching where possible; new enums round-trip; no `#[allow(clippy::...)]` introduced; `cargo +nightly fmt` clean.
