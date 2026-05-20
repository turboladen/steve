# Eval Phase 8 — Reporting + CLI Run/Report Split Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Plan-file location:** Moved from `~/.claude/plans/start-work-on-steve-xa5u-twinkling-horizon.md` (Claude Code plan mode) to the canonical project location, matching `2026-05-10-eval-phase-7-paired-comparison-judge.md`.

## Context

Phase 7 (steve-xa5u) merged 2026-05-12. It shipped `Judge::compare(pair, axes, user_turns, scenario_judge_model) -> Result<CompareVerdict>`, plus the per-scenario `[scoring].axes` parser and the CLI's resolved-axes header line. **No reporting happens yet.** A user can `eval run` to produce a `results.yaml` and `eval baseline freeze` to produce frozen baseline files, but there is no command that compares the two and produces a verdict.

Phase 8 (steve-u896) wires that final mile. Specifically:

- A new `steve eval report <results.yaml>` subcommand that auto-resolves per-scenario baselines from `--baselines-dir`, calls `Judge::compare` for each `(scenario, run)` pair, aggregates verdicts into headline + per-axis + per-scenario layers, and emits a structured text report to stdout.
- Two complementary formulas: **net win rate** `(W-L)/(W+L+T)` (signed, headline) and **non-regression rate** `(W+T)/(W+L+T)` (always-positive, confidence sanity check). Both operate on suite-wide and per-axis slices.
- Exit codes: `0` pass / `1` regression (net delta below threshold) / `2` infra-error. Threshold sourced from `--regression-threshold` CLI flag > `eval.regression_threshold` in `.steve.jsonc` > default `0.0`.
- `eval/history.jsonl` append-on-flag (`--record-history`). One row per recorded report. JSONL is read by `--html` for trend charts; bare `report` is read-only against it.
- `steve eval report --html <path>` writes a self-contained single-file HTML report: latest-run breakdown table + Chart.js trend overlay from `eval/history.jsonl`. Chart.js bundled inline as a vendored static asset (~80KB). All dynamic content HTML-escaped (XSS hardening is **load-bearing** — scenario names, user turns, tool args/outputs, and assistant messages can all contain attacker-supplied HTML/JS).
- `steve eval` (no subcommand) reshaped to chain `run → report` against the configured baseline. Phase-5's single-run pretty-JSON path retires (transitional artifact per Phase 6's plan).
- Graceful degradation:
  - **Targeted invocation** with missing baseline (`steve eval --scenario X`): **fail loud** with the exact freeze command to copy-paste.
  - **Whole-suite invocation** with *some* missing baselines: skip-with-warning, headline computed over the rest, surface the gap.
  - **All-missing on whole-suite**: same shape as targeted (fail loud).
- Judge-call failure recovery: retry once on transient errors; if the retry also fails, mark that `(scenario, run_index)` cell as errored and omit from aggregate totals; surface errored cells in `--verbose` output. If ALL K runs of a scenario error, treat like missing-baseline (skip-with-warning).

**Goal:** Land the `steve eval report` subcommand end-to-end — text output, exit codes, history.jsonl append, self-contained HTML report — plus the `steve eval` (no subcommand) chain that makes the whole eval workflow one command for the common case.

**Architecture:** Three-stage flow inside `report_subcommand`:

1. **Load**: read `ResultsFile` from disk; for each scenario in the results map, resolve its baseline via `baseline_path(baselines_dir, scenario, model)`. Collect (results, baselines) pairs, recording skips for missing baselines.
2. **Judge**: for each `(scenario, run_index, axes)` tuple, call `Judge::compare(ComparePair{baseline, current}, axes, user_turns, scenario.judge_model)`. Retry once on transient errors. Collect `CompareVerdict` per cell or mark errored.
3. **Aggregate + render**: bucket verdicts into suite-wide and per-axis totals, compute formulas, emit layered text (and optionally HTML).

**New modules** all live under `src/eval/`:

- `report.rs` — aggregation types (`Report`, `ReportTotals`, `AxisTotals`, `ScenarioReport`, `ScenarioOutcome`), formula methods, text renderer.
- `history.rs` — `HistoryEntry` type + JSONL append/read helpers.
- `html_report.rs` — self-contained HTML rendering + bundled Chart.js. Uses `html_escape::encode_safe` for XSS-safe content interpolation. Styling reuses the MCP OAuth callback page's visual language so the report feels like part of the same product (warm amber palette, brown text, Steve-branded footer).
- `cli.rs` (modify) — add `report_subcommand` parallel to `run_subcommand`/`freeze_subcommand`.

**Tech Stack additions:**
- Vendored `assets/chartjs.min.js` (~80KB, Chart.js v4.4.x UMD minified) — embedded at build via `include_str!`.
- Vendored `assets/chartjs-LICENSE.txt` (MIT license text) — embedded adjacent to the JS bundle so the two cannot drift, per spec's MIT-compliance requirement.
- New crate dep: `html-escape = "0.2"` — used by `html_report.rs` for XSS-safe escape of dynamic content. Project preference is to use established libs over hand-rolled utilities for well-defined primitives.
- HTML report styling REUSES the MCP OAuth callback page palette (`src/mcp/oauth/callback.rs`) — warm amber gradient (`#fff8e1` → `#ffecb3`), white card on darker page, brown text (`#3e2723`/`#5d4037`/`#a1887f`), amber border (`#ffd54f`), Steve-branded footer. Layout adapted from "single centered card" to "wider tabular page" but keeps the same visual language.

**Spec reference:** `docs/superpowers/specs/2026-05-06-eval-harness-paired-comparison-pivot.md` — particularly:
- "Reporting" (lines 524–671): layered output, formulas, history, HTML report
- "CLI surface" (lines 673–729): verb signatures, auto-resolution, use case mapping
- "No-baseline handling" (lines 822–857): targeted vs whole-suite behavior
- "Regression threshold for exit code" (lines 859–873)
- "Phase 8 — Reporting + CLI Split" (lines 967–1001): ships-when

**Ships-when (from spec, verbatim):**

- `steve eval` end-to-end produces the layered text output against a real Phase-5 scenario.
- `steve eval report --html` produces a viewable single-file HTML output that includes the latest-run breakdown and (if `history.jsonl` is non-empty) a trends chart.
- Backtest works: re-running `steve eval report` against a prior `results.yaml` reproduces the same headline (modulo judge variance).
- CI can be wired up to gate on the exit code and to commit `history.jsonl` appends on main.

In addition (testable acceptance from the spec body):

- Targeted missing-baseline fails loud with copy-pasteable command.
- Whole-suite partial-baseline emits skip-with-warning + headline-over-rest.
- All-missing on whole-suite fails loud.
- Judge-call retry-once: a transient transport error followed by success is included; double-transient errors mark the cell errored and omit from aggregate.
- All K runs of a scenario errored = treated like missing-baseline.
- `--record-history` appends one well-formed JSON row; bare `report` does NOT.
- HTML output XSS-escapes `<script>alert(1)</script>` → `&lt;script&gt;alert(1)&lt;/script&gt;` in dynamic content.

---

## File Structure

| File | Status | Responsibility |
|------|--------|----------------|
| `Cargo.toml` | modify | Add `html-escape = "0.2"` dep for XSS-safe content escape. |
| `assets/chartjs.min.js` | create | Vendored Chart.js v4.4.x UMD minified bundle (~80KB). Embedded via `include_str!`. |
| `assets/chartjs-LICENSE.txt` | create | MIT license text adjacent to the JS bundle. Embedded same way. |
| `src/config/eval.rs` | create | `EvalConfig` struct + `load_eval_config(project_root) -> Result<EvalConfig>`. Reads `~/.config/steve/eval.jsonc` (global) merged with `.steve.eval.jsonc` (project). Separate from the base `Config` so `steve.jsonc` doesn't grow unbounded as eval features expand. |
| `src/config/mod.rs` | modify | Declare new `eval` submodule; re-export `EvalConfig`, `load_eval_config`. Base `Config` is UNCHANGED — eval lives in a separate file. |
| `src/eval/report.rs` | create | Aggregation types + text renderer. No I/O. |
| `src/eval/history.rs` | create | `HistoryEntry` struct + append/read helpers for `eval/history.jsonl`. |
| `src/eval/html_report.rs` | create | Self-contained HTML rendering using `html_escape::encode_text` for XSS-safe content + bundled Chart.js + MCP-OAuth-style palette. |
| `src/eval/cli.rs` | modify | Add `report_subcommand` (parallel to `run_subcommand`); add `eval_chained_run_then_report` for `steve eval` (no subcommand). |
| `src/eval/mod.rs` | modify | Declare new submodules; re-export new public types. |
| `src/main.rs` | modify | Add `EvalSubcommand::Report { ... }` variant; wire flags; handle exit codes via `std::process::exit(code)` after the subcommand returns. Retire Phase-5 single-shot path. |
| `eval/history.jsonl` | (operator action) | Initial empty file may be created by first `--record-history` run; not pre-created. |

**Dependency chain:**
- Task 1 (assets) and Task 2 (`EvalConfig`) are independent and can land in either order.
- Task 3 (aggregation types) → Task 4 (judge orchestration) → Task 5 (text rendering) → Task 9 (CLI wiring) is the critical path for the text-output ships-when.
- Task 6 (HTML escape) → Task 7 (history) → Task 8 (HTML rendering) is the HTML branch.
- Task 10 (chained `steve eval`) and Task 11 (Phase-5 retirement) depend on Tasks 3–9 being complete.

---

## Task 1: Vendor Chart.js + MIT license as static assets

The HTML report embeds Chart.js inline so the file renders offline and from CI artifacts. Chart.js is MIT-licensed; the spec requires the upstream copyright + license text adjacent to the bundled JS so the two can never drift.

**Files:**
- Create: `assets/chartjs.min.js` (~80KB)
- Create: `assets/chartjs-LICENSE.txt` (~1KB)

- [ ] **Step 1: Download Chart.js v4.4.x UMD minified**

Run:
```bash
mkdir -p assets
curl -sLo assets/chartjs.min.js https://cdn.jsdelivr.net/npm/chart.js@4.4.7/dist/chart.umd.min.js
# Verify size
wc -c assets/chartjs.min.js  # expected: ~200-220KB (UMD bundle, not just core)
```

Pin v4.4.7 specifically (current LTS at plan time). Update is intentional — no auto-versioning.

- [ ] **Step 2: Capture upstream MIT license**

Run:
```bash
curl -sLo assets/chartjs-LICENSE.txt https://raw.githubusercontent.com/chartjs/Chart.js/v4.4.7/LICENSE.md
# Verify it's the MIT text
head -3 assets/chartjs-LICENSE.txt  # expected: "# MIT License" or "MIT License"
```

- [ ] **Step 3: Verify `include_str!` resolution**

Add a temporary smoke check (delete before commit):
```rust
// In src/lib.rs or any module — temporarily:
#[allow(dead_code)]
const _CHART_JS_SMOKE: &str = include_str!("../assets/chartjs.min.js");
#[allow(dead_code)]
const _CHART_JS_LICENSE_SMOKE: &str = include_str!("../assets/chartjs-LICENSE.txt");
```

Run `cargo check`. If it compiles, the paths are correct.

Then **delete the smoke constants** — Task 8 adds the real `include_str!` usage.

- [ ] **Step 4: Commit**

```bash
git add assets/
git commit -m "$(cat <<'EOF'
chore(eval): vendor Chart.js v4.4.7 + MIT license

Phase 8's HTML report bundles Chart.js inline so the output file
renders offline (CI artifacts, archived issues). MIT license text
is committed alongside the JS so the renderer can emit both
together — per spec, they must not be able to drift apart.

Pin v4.4.7 (current LTS). Update is intentional, not auto-tracked.

Refs: steve-u896
EOF
)"
```

---

## Task 2: Add separate `.steve.eval.jsonc` config

Eval configuration lives in its own file so the base `.steve.jsonc` doesn't grow unbounded (it already gets large with multiple providers + models). The eval config still REFERENCES models defined in the base config — `default_judge_model = "provider/model_id"` resolves through the same `ProviderRegistry` built from `.steve.jsonc`.

**Files:**
- Create: `src/config/eval.rs`
- Modify: `src/config/mod.rs` (declare submodule + re-export)

Layout:
- Global: `~/.config/steve/eval.jsonc` (mirrors the base config's XDG-style location)
- Project: `.steve.eval.jsonc` in project root (mirrors the base `.steve.jsonc` dotfile)

- [ ] **Step 1: Write failing tests in the new file**

Create `src/config/eval.rs`:

```rust
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

use std::path::Path;

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
    /// it's set; this (global) fills missing fields.
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
    let dir = match override_dir {
        Some(d) => d.to_path_buf(),
        None => match std::env::var("HOME") {
            Ok(home) => Path::new(&home).join(".config").join("steve"),
            Err(_) => return Ok(EvalConfig::default()),
        },
    };
    let path = dir.join("eval.jsonc");
    if !path.exists() {
        return Ok(EvalConfig::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    // Reuse the project's existing jsonc parser. (The base config uses
    // serde_jsonc; reach for the same lib here for consistency.)
    serde_jsonc::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

fn load_project(project_root: &Path) -> Result<EvalConfig> {
    let path = project_root.join(".steve.eval.jsonc");
    if !path.exists() {
        return Ok(EvalConfig::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    serde_jsonc::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
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
        ).unwrap();
        std::fs::write(
            project.path().join(".steve.eval.jsonc"),
            r#"{ "regression_threshold": -0.05 }"#, // project sets only this
        ).unwrap();
        let cfg = load_with_override(project.path(), Some(global.path())).unwrap();
        // Project's regression_threshold wins; global's default_judge_model
        // bleeds through (project didn't override).
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
        ).unwrap();
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
        ).unwrap();
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
}
```

- [ ] **Step 2: Declare submodule and re-export from `src/config/mod.rs`**

Add to `src/config/mod.rs`:

```rust
pub mod eval;
pub use eval::{EvalConfig, load_eval_config};
```

**Crucially: do NOT touch the existing `Config` struct.** The eval config is intentionally separate.

- [ ] **Step 3: Run the new tests**

Run: `cargo test --lib config::eval::tests`
Expected: ALL PASS.

- [ ] **Step 4: Run full config test suite (regression check)**

Run: `cargo test --lib config::`
Expected: PASS — existing tests untouched because base `Config` is unchanged.

- [ ] **Step 5: Commit**

```bash
git add src/config/eval.rs src/config/mod.rs
git commit -m "$(cat <<'EOF'
feat(config): separate eval.jsonc config file for Phase 8

Eval-subsystem config lives in `~/.config/steve/eval.jsonc` (global)
and `.steve.eval.jsonc` (project), separate from the base config
files. Reasoning: `.steve.jsonc` already gets large with multiple
providers + models; adding eval fields there would make a busy file
busier. Keeping eval separate also lets users version-control eval
settings independently (e.g., different regression thresholds per
branch) without churning the main config.

Currently EvalConfig holds three fields:
  - regression_threshold (gates `steve eval report` exit code)
  - default_judge_model (provider/model_id, resolved through the base
    config's ProviderRegistry)
  - baselines_dir (project-relative path)

All optional. Missing files yield EvalConfig::default(). Project
overrides global on a field-by-field basis (more granular than the
base Config's wholesale-replace; eval fields are independent in
practice so per-field merge is the right shape).

The base Config is UNCHANGED — no eval field added there. Phase 8's
CLI calls load_eval_config(project_root) alongside the existing
config::load(project_root).

Refs: steve-u896
EOF
)"
```

---

## Task 3: Aggregation types in `src/eval/report.rs`

The core data shape for Phase 8. Types are pure data — no I/O, no async. Methods include the formulas (net win rate, non-regression rate) and the layered text renderer.

The shape of the verdict count is `S × K × A` cells where S=scenarios, K=runs-per-scenario, A=axes (typically 3). Errored cells and missing-baseline scenarios are tracked separately and excluded from totals.

**Files:**
- Create: `src/eval/report.rs`
- Modify: `src/eval/mod.rs` (declare submodule + re-exports)

- [ ] **Step 1: Write failing tests in the new file**

Create `src/eval/report.rs` with this preamble + tests. The implementation follows.

```rust
//! Aggregation of paired-comparison verdicts into the layered
//! Phase 8 report — headline + per-axis + per-scenario detail.
//!
//! Pure data types and pure formula methods. No I/O. The
//! orchestration that loads `ResultsFile` + baselines and calls
//! `Judge::compare` to populate a `Report` lives in `cli.rs`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::eval::score::{Axis, PairedScore, Verdict};

/// Suite-wide or per-slice tally of `CurrentWins`, `BaselineWins`,
/// and `Tie` verdicts. The three formulas in the spec all operate
/// on this primitive shape.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportTotals {
    pub current_wins: usize,
    pub baseline_wins: usize,
    pub ties: usize,
}

impl ReportTotals {
    pub fn total(&self) -> usize {
        self.current_wins + self.baseline_wins + self.ties
    }

    /// Net win rate, `(W - L) / (W + L + T)`. Range `[-1.0, +1.0]`.
    /// Returns `0.0` when the total is zero (no verdicts) — the
    /// spec's "all ties" reading collapses to the same value, and
    /// dividing by zero would be worse than reporting "no change".
    pub fn net_win_rate(&self) -> f64 {
        let total = self.total();
        if total == 0 {
            return 0.0;
        }
        (self.current_wins as f64 - self.baseline_wins as f64) / total as f64
    }

    /// Non-regression rate, `(W + T) / (W + L + T)`. Range
    /// `[0.0, 1.0]`. Returns `1.0` when no verdicts (vacuously true).
    pub fn non_regression_rate(&self) -> f64 {
        let total = self.total();
        if total == 0 {
            return 1.0;
        }
        (self.current_wins + self.ties) as f64 / total as f64
    }

    /// Fold a single `Verdict` into the totals. Used during
    /// aggregation to walk every cell in axis order.
    pub fn add(&mut self, verdict: Verdict) {
        match verdict {
            Verdict::CurrentWins => self.current_wins += 1,
            Verdict::BaselineWins => self.baseline_wins += 1,
            Verdict::Tie => self.ties += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn net_win_rate_zero_on_empty() {
        let t = ReportTotals::default();
        assert_eq!(t.net_win_rate(), 0.0);
    }

    #[test]
    fn net_win_rate_pos_when_more_wins() {
        let t = ReportTotals { current_wins: 7, baseline_wins: 3, ties: 0 };
        // (7-3) / 10 = 0.4
        assert!((t.net_win_rate() - 0.4).abs() < 1e-12);
    }

    #[test]
    fn net_win_rate_neg_when_more_losses() {
        let t = ReportTotals { current_wins: 2, baseline_wins: 5, ties: 3 };
        // (2-5) / 10 = -0.3
        assert!((t.net_win_rate() + 0.3).abs() < 1e-12);
    }

    #[test]
    fn ties_dilute_net_win_rate_but_dont_change_sign() {
        let t_with_ties = ReportTotals { current_wins: 1, baseline_wins: 0, ties: 9 };
        let t_no_ties = ReportTotals { current_wins: 1, baseline_wins: 0, ties: 0 };
        assert!(t_with_ties.net_win_rate() < t_no_ties.net_win_rate());
        assert!(t_with_ties.net_win_rate() > 0.0);
    }

    #[test]
    fn non_regression_rate_full_when_no_losses() {
        let t = ReportTotals { current_wins: 5, baseline_wins: 0, ties: 5 };
        assert!((t.non_regression_rate() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn non_regression_rate_one_on_empty() {
        // Vacuous truth: no verdicts means no regressions observed.
        let t = ReportTotals::default();
        assert!((t.non_regression_rate() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn add_folds_verdicts_correctly() {
        let mut t = ReportTotals::default();
        t.add(Verdict::CurrentWins);
        t.add(Verdict::CurrentWins);
        t.add(Verdict::BaselineWins);
        t.add(Verdict::Tie);
        assert_eq!(t, ReportTotals { current_wins: 2, baseline_wins: 1, ties: 1 });
    }
}
```

In `src/eval/mod.rs`, add:

```rust
pub mod report;
pub use report::{Report, ReportTotals, ScenarioReport, ScenarioOutcome, AxisTotals};
```

(`Report`, `ScenarioReport`, `ScenarioOutcome`, `AxisTotals` are types added in later steps within this Task.)

- [ ] **Step 2: Run tests to verify they fail (then pass)**

Run: `cargo test --lib eval::report::tests`
Expected: the formula tests should COMPILE once `ReportTotals` exists and `Verdict::add` is implemented; if you wrote them first they'd fail with unresolved imports. So this step is "run after adding the struct" — all 7 tests PASS.

- [ ] **Step 3: Add `AxisTotals`, `ScenarioReport`, `ScenarioOutcome`, `Report`**

Append to `src/eval/report.rs`:

```rust
/// Per-axis tally + the axis identity. The full per-axis section of
/// the layered report is a `Vec<AxisTotals>` in spec-axis order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxisTotals {
    pub axis: Axis,
    pub totals: ReportTotals,
}

/// Why a scenario contributes (or doesn't) to aggregate totals.
/// `Graded` means every run produced verdicts that got bucketed
/// into the totals; `Skipped` means we never called the judge
/// (missing baseline) and the scenario is reported separately in
/// the headline's "Skipped:" subsection; `AllRunsErrored` means
/// the judge call failed twice for every run of this scenario, so
/// it's treated the same as skip-with-warning per spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScenarioOutcome {
    Graded {
        /// One `Vec<PairedScore>` per run; length matches
        /// `scenario.runs`. Cells inside that errored are omitted
        /// from the inner Vec — i.e., a run with K axes might have
        /// fewer than K entries if some axes errored. (In practice,
        /// per-axis-error doesn't happen; the whole-call retry is
        /// at the (scenario, run) granularity, so a run is either
        /// fully present or fully errored.)
        per_run_scores: Vec<Vec<PairedScore>>,
        /// Per-axis tally for this scenario alone, for the verbose
        /// per-scenario rendering.
        per_axis: Vec<AxisTotals>,
    },
    Skipped {
        /// Human-readable reason — the renderer prints this in the
        /// "Skipped:" subsection of the headline. Typical values:
        /// `"no baseline for scenario X with model Y"` or
        /// `"all K runs of scenario X errored: <last error msg>"`.
        reason: String,
    },
}

/// Per-scenario record carried by `Report.scenarios`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioReport {
    pub scenario: String,
    pub outcome: ScenarioOutcome,
}

/// The complete result of a `steve eval report` run. Pure data;
/// rendering and exit-code computation hang off this.
///
/// `headline_totals` is the suite-wide tally across all
/// `S_graded × K × A` cells (where `S_graded` excludes Skipped
/// scenarios). `per_axis` is the same tally sliced by axis.
/// `scenarios` is the per-scenario detail used by `--verbose`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// Model the results were sampled from (e.g., `"ollama/qwen3-coder"`).
    pub model: String,
    /// Git ref recorded in the results file.
    pub results_git_ref: String,
    /// Path to the results file (for provenance + `history.jsonl`).
    pub results_path: String,
    /// Per-scenario baseline provenance — the manifest's git_ref +
    /// frozen_at for each scenario that successfully resolved a
    /// baseline. Keys are scenario names. Missing entries indicate
    /// the scenario was skipped (no baseline).
    pub baseline_provenance: BTreeMap<String, BaselineProvenance>,
    /// Judge model used for this report (resolved precedence:
    /// CLI > scenario.judge_model > error).
    pub judge_model: String,
    /// Suite-wide tally across all (scenario × run × axis) cells
    /// that were graded.
    pub headline_totals: ReportTotals,
    /// Per-axis slice of `headline_totals`. Order is spec-axis order
    /// (typically Correctness, Efficiency, Conciseness, but may differ
    /// when scenarios override `[scoring].axes`).
    pub per_axis: Vec<AxisTotals>,
    /// Per-scenario detail. Order matches results-file insertion order.
    pub scenarios: Vec<ScenarioReport>,
}

/// Per-scenario baseline provenance fields surfaced in the report's
/// metadata block. Both come from the baseline manifest entry; the
/// renderer uses them to print "frozen YYYY-MM-DD at <ref>" alongside
/// the headline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineProvenance {
    pub git_ref: String,
    pub frozen_at: String,
}
```

- [ ] **Step 4: Add more failing tests for `Report` shape**

Append to `src/eval/report.rs`'s `mod tests`:

```rust
    use crate::eval::score::{Axis, PairedScore};

    fn paired(axis: Axis, verdict: Verdict) -> PairedScore {
        PairedScore { axis, rationale: "ok".into(), verdict }
    }

    #[test]
    fn report_headline_sums_per_axis() {
        // Invariant: headline_totals = sum of per_axis totals.
        let per_axis = vec![
            AxisTotals { axis: Axis::Correctness, totals: ReportTotals { current_wins: 2, baseline_wins: 1, ties: 0 } },
            AxisTotals { axis: Axis::Efficiency, totals: ReportTotals { current_wins: 1, baseline_wins: 1, ties: 1 } },
            AxisTotals { axis: Axis::Conciseness, totals: ReportTotals { current_wins: 0, baseline_wins: 0, ties: 3 } },
        ];
        let headline = ReportTotals { current_wins: 3, baseline_wins: 2, ties: 4 };
        // The renderer relies on this invariant; pin it in a test
        // even though it's an aggregation contract rather than a
        // type-system invariant.
        let summed: ReportTotals = per_axis.iter().fold(
            ReportTotals::default(),
            |mut acc, a| {
                acc.current_wins += a.totals.current_wins;
                acc.baseline_wins += a.totals.baseline_wins;
                acc.ties += a.totals.ties;
                acc
            },
        );
        assert_eq!(headline, summed);
    }

    #[test]
    fn scenario_outcome_serde_round_trips() {
        // The Report is serialized to JSONL via the history module
        // (using serde_json); YAML round-tripping isn't part of the
        // contract. Pin only the JSON round-trip.
        let s = ScenarioOutcome::Skipped { reason: "no baseline".into() };
        let json = serde_json::to_string(&s).unwrap();
        let back: ScenarioOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn scenario_outcome_graded_carries_per_axis() {
        let g = ScenarioOutcome::Graded {
            per_run_scores: vec![vec![paired(Axis::Correctness, Verdict::CurrentWins)]],
            per_axis: vec![AxisTotals {
                axis: Axis::Correctness,
                totals: ReportTotals { current_wins: 1, baseline_wins: 0, ties: 0 },
            }],
        };
        let json = serde_json::to_string(&g).unwrap();
        let back: ScenarioOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(g, back);
    }
```

Note: the project uses `serde-saphyr` (not `serde_yaml`) for YAML. The test above only pins JSON round-trip, which is what `Report` actually uses (via the history module).

- [ ] **Step 5: Run all report tests**

Run: `cargo test --lib eval::report::`
Expected: PASS — all formula tests + serde round-trip tests pass.

- [ ] **Step 6: Run full eval test suite (regression check)**

Run: `cargo test --lib eval::`
Expected: PASS — no existing tests broken.

- [ ] **Step 7: Commit**

```bash
git add src/eval/report.rs src/eval/mod.rs
git commit -m "$(cat <<'EOF'
feat(eval): aggregation types for Phase 8 paired-comparison reports

Pure data shape for `steve eval report` output. No I/O, no async.

- ReportTotals: W/L/T tally with net_win_rate and non_regression_rate
  methods. Spec formulas: (W-L)/(W+L+T) and (W+T)/(W+L+T). Both
  return sentinel values (0.0 / 1.0) for empty totals rather than
  NaN — divide-by-zero is the wrong shape for "no verdicts observed".
- AxisTotals: per-axis tally + axis identity, in spec-axis order.
- ScenarioReport: per-scenario record carrying the scenario name +
  its outcome (Graded | Skipped). All-runs-errored maps to Skipped
  per spec.
- Report: top-level — model, git refs (results + per-scenario
  baseline), judge model, headline totals, per-axis, scenario detail.
- BaselineProvenance: per-scenario git_ref + frozen_at surfaced in
  the metadata block.

Tests pin the formula values for the spec's example cases (ties
dilute but don't change sign; vacuous truth on empty; folds add
correctly) plus serde round-trips on the nested ScenarioOutcome
variants. The headline = sum-of-per-axis invariant is pinned as
an aggregation contract rather than a type-system invariant.

Refs: steve-u896
EOF
)"
```

---

## Task 4: Baseline resolution + judge orchestration + retry logic

The orchestrator. Given a `ResultsFile`, a baselines directory, a `Judge`, and per-scenario configuration, produces a populated `Report`. Handles:
- Missing baselines (mark Skipped with the freeze-command suggestion).
- Judge-call retry-once (single transient error retried; double transient = mark errored).
- All-runs-errored on a scenario (treat like missing baseline, surface in headline's Skipped section).
- Per-axis per-run verdict bucketing into `headline_totals` + per-axis + per-scenario tallies.

Lives in `src/eval/report.rs` (continuation of Task 3's module — it's `Report::build_from_results` rather than a separate file).

**Files:**
- Modify: `src/eval/report.rs` (add orchestrator)
- Modify: `src/eval/mod.rs` (no change beyond Task 3)

- [ ] **Step 1: Write failing tests**

Append to `src/eval/report.rs`. These tests use a `MockJudge` trait-based seam to avoid real LLM calls.

```rust
    use crate::eval::{
        baseline::BaselineFile,
        results::{ResultsFile, ScenarioResults},
        transcript::NormalizedTranscript,
    };
    use std::path::PathBuf;
    use tempfile::TempDir;

    // ── helpers ──

    fn empty_transcript() -> NormalizedTranscript {
        NormalizedTranscript {
            events: vec![],
            deterministic_floor_passed: true,
            usage_summary: Default::default(),
        }
    }

    fn results_file_with(scenarios: Vec<(&str, usize)>) -> ResultsFile {
        let mut map = std::collections::BTreeMap::new();
        for (name, runs) in scenarios {
            map.insert(
                name.to_string(),
                ScenarioResults {
                    user_turns: vec!["go".into()],
                    runs: (0..runs).map(|_| empty_transcript()).collect(),
                },
            );
        }
        ResultsFile {
            git_ref: "abcdef".into(),
            recorded_at: "2026-05-12T00:00:00Z".into(),
            model: "test/model".into(),
            scenarios: map,
        }
    }

    fn write_baseline(dir: &std::path::Path, scenario: &str, model: &str) -> BaselineFile {
        let bf = BaselineFile {
            scenario: scenario.into(),
            model: model.into(),
            git_ref: "baseline-ref".into(),
            frozen_at: "2026-05-01T00:00:00Z".into(),
            user_turns: vec!["go".into()],
            transcript: empty_transcript(),
        };
        let path = crate::eval::baseline::baseline_path(dir, scenario, model).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        bf.write_to_path(&path).unwrap();
        bf
    }

    // ── Build_from_results ──

    #[tokio::test]
    async fn build_from_results_grades_when_baselines_present() {
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("_smoke", 2)]);
        write_baseline(tmp.path(), "_smoke", "test/model");

        // Fake judge that returns all-CurrentWins for any pair.
        let judge = FakeJudge::all_wins();
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            "fake/judge-model",
            None,
        )
        .await
        .unwrap();

        // 1 scenario × 2 runs × 3 axes (default) = 6 CurrentWins.
        assert_eq!(report.headline_totals.current_wins, 6);
        assert_eq!(report.headline_totals.baseline_wins, 0);
        assert_eq!(report.headline_totals.ties, 0);
        assert_eq!(report.scenarios.len(), 1);
        assert!(matches!(report.scenarios[0].outcome, ScenarioOutcome::Graded { .. }));
    }

    #[tokio::test]
    async fn build_from_results_skips_when_baseline_missing() {
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("missing-scenario", 1)]);
        // No baseline written.

        let judge = FakeJudge::all_wins();
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            "fake/judge-model",
            None,
        )
        .await
        .unwrap();

        assert_eq!(report.headline_totals.total(), 0);
        assert_eq!(report.scenarios.len(), 1);
        match &report.scenarios[0].outcome {
            ScenarioOutcome::Skipped { reason } => {
                assert!(
                    reason.contains("no baseline"),
                    "expected 'no baseline' diagnostic; got: {reason}"
                );
            }
            other => panic!("expected Skipped; got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn build_from_results_retries_once_on_transient_judge_error() {
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("_smoke", 1)]);
        write_baseline(tmp.path(), "_smoke", "test/model");

        // Fail once, then succeed.
        let judge = FakeJudge::fail_n_then_wins(1);
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            "fake/judge-model",
            None,
        )
        .await
        .unwrap();

        // The retry succeeded, so the single run is graded.
        assert_eq!(report.headline_totals.total(), 3); // 3 axes
    }

    #[tokio::test]
    async fn build_from_results_marks_errored_when_both_attempts_fail() {
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("_smoke", 1)]);
        write_baseline(tmp.path(), "_smoke", "test/model");

        // Fail twice (try + retry both fail).
        let judge = FakeJudge::fail_n_then_wins(2);
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            "fake/judge-model",
            None,
        )
        .await
        .unwrap();

        // The single run errored on both attempts. All-runs-errored
        // for the scenario → Skipped per spec.
        assert_eq!(report.headline_totals.total(), 0);
        match &report.scenarios[0].outcome {
            ScenarioOutcome::Skipped { reason } => {
                assert!(reason.contains("errored"), "got: {reason}");
            }
            other => panic!("expected Skipped; got: {other:?}"),
        }
    }
```

Add this fake-judge helper (lives in the `mod tests` block):

```rust
    use crate::eval::{judge::{ComparePair, JudgeAdapter}, score::{CompareVerdict, PairedScore}};
    use std::sync::Mutex;

    /// A canned judge for testing `Report::build_from_results`.
    /// Tracks call count to support fail-then-succeed retry tests.
    struct FakeJudge {
        /// On each call: if the call index is < `fail_until`, return
        /// Err; otherwise return Ok with all-CurrentWins.
        fail_until: usize,
        call_count: Mutex<usize>,
    }

    impl FakeJudge {
        fn all_wins() -> Self {
            Self { fail_until: 0, call_count: Mutex::new(0) }
        }
        fn fail_n_then_wins(n: usize) -> Self {
            Self { fail_until: n, call_count: Mutex::new(0) }
        }
    }

    #[async_trait::async_trait]
    impl JudgeAdapter for FakeJudge {
        async fn compare(
            &self,
            _pair: ComparePair<'_>,
            axes: &[Axis],
            _user_turns: &[String],
            _scenario_judge_model: Option<&str>,
        ) -> anyhow::Result<CompareVerdict> {
            let n = {
                let mut c = self.call_count.lock().unwrap();
                let prev = *c;
                *c += 1;
                prev
            };
            if n < self.fail_until {
                anyhow::bail!("simulated transient error (call #{n})");
            }
            Ok(axes.iter()
                .map(|a| PairedScore {
                    axis: *a,
                    rationale: "fake".into(),
                    verdict: Verdict::CurrentWins,
                })
                .collect())
        }
    }
```

The `JudgeAdapter` trait isn't yet defined — Step 2 adds it.

- [ ] **Step 2: Add `JudgeAdapter` trait to `src/eval/judge.rs`**

The trait is the test seam for `Report::build_from_results`. Production code uses the existing `Judge` struct; tests substitute `FakeJudge`. Append to `src/eval/judge.rs` after the `impl<'a> Judge<'a> {}` block:

```rust
/// Adapter trait abstracting `Judge::compare` for testing.
/// `Report::build_from_results` accepts `&dyn JudgeAdapter` so unit
/// tests can substitute fakes that return canned `CompareVerdict`s
/// or simulate transient errors. Production code uses the auto-
/// derived `impl JudgeAdapter for Judge` below.
#[async_trait::async_trait]
pub trait JudgeAdapter: Send + Sync {
    async fn compare(
        &self,
        pair: ComparePair<'_>,
        axes: &[Axis],
        user_turns: &[String],
        scenario_judge_model: Option<&str>,
    ) -> Result<CompareVerdict>;
}

#[async_trait::async_trait]
impl<'a> JudgeAdapter for Judge<'a> {
    async fn compare(
        &self,
        pair: ComparePair<'_>,
        axes: &[Axis],
        user_turns: &[String],
        scenario_judge_model: Option<&str>,
    ) -> Result<CompareVerdict> {
        Judge::compare(self, pair, axes, user_turns, scenario_judge_model).await
    }
}
```

Re-export from `src/eval/mod.rs`:

```rust
pub use judge::{ComparePair, Judge, JudgeAdapter, JudgeOutcome, JudgeVerdict, apply_judges};
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib eval::report::tests::build_from_results`
Expected: FAIL — `cannot find method 'build_from_results'`.

- [ ] **Step 4: Implement `Report::build_from_results`**

Append to `src/eval/report.rs`:

```rust
impl Report {
    /// Build a `Report` by walking every (scenario, run) pair in
    /// `results`, resolving a baseline from `baselines_dir`, and
    /// calling `judge.compare(...)` for each cell. Missing
    /// baselines surface as `Skipped`. Transient judge errors are
    /// retried once; double-failures mark the cell errored. All-
    /// runs-errored on a scenario maps to `Skipped` per spec.
    pub async fn build_from_results(
        results: &crate::eval::results::ResultsFile,
        baselines_dir: &std::path::Path,
        results_path: &str,
        judge: &dyn crate::eval::JudgeAdapter,
        judge_model: &str,
        scenarios_dir: Option<&std::path::Path>,
    ) -> anyhow::Result<Self> {
        use crate::eval::baseline::{BaselineFile, baseline_path};
        use crate::eval::scenario::Scenario;

        let mut report = Report {
            model: results.model.clone(),
            results_git_ref: results.git_ref.clone(),
            results_path: results_path.to_string(),
            baseline_provenance: BTreeMap::new(),
            judge_model: judge_model.to_string(),
            headline_totals: ReportTotals::default(),
            per_axis: Vec::new(),
            scenarios: Vec::new(),
        };

        // Accumulate per-axis tallies in insertion order (we'll
        // emit them in the order they first appear across scenarios).
        let mut per_axis_map: std::collections::BTreeMap<Axis, ReportTotals> = BTreeMap::new();

        for (scenario_name, scenario_results) in &results.scenarios {
            let baseline_path = match baseline_path(baselines_dir, scenario_name, &results.model) {
                Ok(p) => p,
                Err(e) => {
                    report.scenarios.push(ScenarioReport {
                        scenario: scenario_name.clone(),
                        outcome: ScenarioOutcome::Skipped {
                            reason: format!("baseline path resolution failed: {e:#}"),
                        },
                    });
                    continue;
                }
            };
            if !baseline_path.exists() {
                report.scenarios.push(ScenarioReport {
                    scenario: scenario_name.clone(),
                    outcome: ScenarioOutcome::Skipped {
                        reason: format!(
                            "no baseline for scenario '{scenario_name}' with model '{}': run `steve eval baseline freeze --scenario {scenario_name} --model {}`",
                            results.model, results.model
                        ),
                    },
                });
                continue;
            }
            let baseline = match BaselineFile::read_from_path(&baseline_path) {
                Ok(b) => b,
                Err(e) => {
                    report.scenarios.push(ScenarioReport {
                        scenario: scenario_name.clone(),
                        outcome: ScenarioOutcome::Skipped {
                            reason: format!("baseline load failed: {e:#}"),
                        },
                    });
                    continue;
                }
            };
            report.baseline_provenance.insert(
                scenario_name.clone(),
                BaselineProvenance {
                    git_ref: baseline.git_ref.clone(),
                    frozen_at: baseline.frozen_at.clone(),
                },
            );

            // Determine axes for this scenario. Default: DEFAULT_AXES.
            // If scenarios_dir is provided AND the on-disk scenario.toml
            // has a [scoring] override, use that. (Phase 8 doesn't
            // re-load scenarios for axes since the override would have
            // been respected at Phase-7 grade-time; here we use
            // DEFAULT_AXES unless the caller passed a scenarios_dir
            // hint.)
            let axes: Vec<Axis> = {
                if let Some(dir) = scenarios_dir
                    && let Ok(scn) = Scenario::from_file(&dir.join(scenario_name).join("scenario.toml"))
                {
                    scn.scoring_axes().to_vec()
                } else {
                    crate::eval::score::DEFAULT_AXES.to_vec()
                }
            };

            // Resolve per-scenario judge model (CLI > scenario.judge_model).
            // The CLI override is baked into `judge_model` already at the
            // subcommand level (Task 9). At this layer, we only have the
            // scenario default to pass through.
            let scenario_judge_model: Option<String> = scenarios_dir
                .and_then(|dir| {
                    Scenario::from_file(&dir.join(scenario_name).join("scenario.toml")).ok()
                })
                .and_then(|scn| scn.judge_model);

            // Walk each run; call judge with retry-once.
            let mut per_run_scores: Vec<Vec<PairedScore>> = Vec::with_capacity(scenario_results.runs.len());
            let mut last_error: Option<String> = None;
            for current_transcript in &scenario_results.runs {
                let pair = crate::eval::judge::ComparePair {
                    baseline: &baseline.transcript,
                    current: current_transcript,
                };
                let mut attempts = 0;
                let scores = loop {
                    attempts += 1;
                    match judge
                        .compare(
                            pair,
                            &axes,
                            &scenario_results.user_turns,
                            scenario_judge_model.as_deref(),
                        )
                        .await
                    {
                        Ok(s) => break Some(s),
                        Err(e) => {
                            if attempts >= 2 {
                                last_error = Some(format!("{e:#}"));
                                break None;
                            }
                        }
                    }
                };
                if let Some(s) = scores {
                    per_run_scores.push(s);
                }
            }

            if per_run_scores.is_empty() {
                report.scenarios.push(ScenarioReport {
                    scenario: scenario_name.clone(),
                    outcome: ScenarioOutcome::Skipped {
                        reason: format!(
                            "all {} runs of scenario '{scenario_name}' errored: {}",
                            scenario_results.runs.len(),
                            last_error.unwrap_or_else(|| "unknown".into())
                        ),
                    },
                });
                continue;
            }

            // Bucket verdicts into per-axis + headline + per-scenario tallies.
            let mut scenario_per_axis: BTreeMap<Axis, ReportTotals> = BTreeMap::new();
            for run_scores in &per_run_scores {
                for score in run_scores {
                    report.headline_totals.add(score.verdict);
                    per_axis_map.entry(score.axis).or_default().add(score.verdict);
                    scenario_per_axis.entry(score.axis).or_default().add(score.verdict);
                }
            }
            // Convert scenario_per_axis to ordered Vec<AxisTotals>
            // following the requested axes order:
            let scenario_per_axis_vec: Vec<AxisTotals> = axes
                .iter()
                .filter_map(|a| {
                    scenario_per_axis.get(a).map(|t| AxisTotals {
                        axis: *a,
                        totals: *t,
                    })
                })
                .collect();
            report.scenarios.push(ScenarioReport {
                scenario: scenario_name.clone(),
                outcome: ScenarioOutcome::Graded {
                    per_run_scores,
                    per_axis: scenario_per_axis_vec,
                },
            });
        }

        // Finalize per_axis from the per_axis_map. Order: spec order
        // for DEFAULT_AXES axes that appear; then any others
        // (Robustness/Truthfulness) in BTreeMap order.
        for axis in crate::eval::score::DEFAULT_AXES {
            if let Some(t) = per_axis_map.remove(&axis) {
                report.per_axis.push(AxisTotals { axis, totals: t });
            }
        }
        for (axis, t) in per_axis_map {
            report.per_axis.push(AxisTotals { axis, totals: t });
        }

        Ok(report)
    }
}
```

Notes on the implementation choices:
- `scenarios_dir` is `Option` because the Phase 5 results file doesn't embed scenario.toml paths; the orchestrator looks them up from a known directory. When `None`, we use `DEFAULT_AXES` for every scenario and `None` for the scenario judge model. Production code (Task 9) always passes `Some(eval/scenarios)`. Tests pass `None` to keep the fake judge focused on the orchestration logic.
- The retry loop is bounded by `attempts >= 2` — first attempt + one retry.

- [ ] **Step 5: Run the orchestrator tests**

Run: `cargo test --lib eval::report::tests::build_from_results`
Expected: ALL PASS — graded, missing-baseline, retry-succeeded, all-errored.

- [ ] **Step 6: Run the full eval test suite (regression check)**

Run: `cargo test --lib eval::`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/eval/report.rs src/eval/judge.rs src/eval/mod.rs
git commit -m "$(cat <<'EOF'
feat(eval): Report::build_from_results orchestrator + JudgeAdapter

Walks every (scenario, run) pair in a ResultsFile, resolves the
baseline for each scenario via baseline_path, calls Judge::compare
for each cell, and aggregates verdicts into the layered Report
shape introduced in the previous commit.

Three failure modes handled per spec:

1. Missing baseline: Scenario marked Skipped with the exact
   `eval baseline freeze` command in the diagnostic (copy-pasteable
   per spec's "fail loud with command suggestion" requirement).
2. Transient judge errors: retried once. The second attempt's
   result is used (Ok or Err); double-Err marks the run errored
   and excludes it from per_run_scores.
3. All K runs of a scenario errored: scenario maps to Skipped
   per spec ("if ALL K runs of a scenario error, the scenario is
   treated like a missing baseline").

JudgeAdapter trait is the test seam: Report::build_from_results
accepts &dyn JudgeAdapter; production uses the auto-derived impl
on Judge; tests substitute FakeJudge for canned responses + simulated
errors. Avoids burning real LLM tokens during cargo test.

Per-axis ordering: axes that appear in DEFAULT_AXES order go first
(correctness, efficiency, conciseness), then any scenario-override
axes (robustness, truthfulness) in alphabetical order. The renderer
in Task 5 walks `report.per_axis` directly.

Refs: steve-u896
EOF
)"
```

---

## Task 5: Layered text rendering

The text output the spec describes at lines 528–542. Three layers: headline + per-axis + per-scenario (verbose).

**Files:**
- Modify: `src/eval/report.rs`

- [ ] **Step 1: Write failing tests**

Append to `src/eval/report.rs`'s `mod tests`:

```rust
    #[test]
    fn render_text_contains_headline_with_signed_percentage() {
        let r = Report {
            model: "ollama/qwen3-coder".into(),
            results_git_ref: "abc1234".into(),
            results_path: "results.yaml".into(),
            baseline_provenance: BTreeMap::new(),
            judge_model: "fake/judge".into(),
            headline_totals: ReportTotals { current_wins: 4, baseline_wins: 2, ties: 24 },
            per_axis: vec![AxisTotals {
                axis: Axis::Correctness,
                totals: ReportTotals { current_wins: 1, baseline_wins: 2, ties: 7 },
            }],
            scenarios: Vec::new(),
        };
        let out = r.render_text(false);
        // Headline: (4-2)/30 = +0.067 (rounds to +6.7%); 28/30 = 93.3%
        assert!(out.contains("Headline"), "got:\n{out}");
        assert!(
            out.contains("+6.7%") || out.contains("+6.67%"),
            "expected signed +6.7%-ish headline; got:\n{out}"
        );
        assert!(out.contains("93.3%") || out.contains("0.933"), "got:\n{out}");
    }

    #[test]
    fn render_text_lists_per_axis_section_when_axes_present() {
        let r = Report {
            model: "x".into(),
            results_git_ref: "x".into(),
            results_path: "x".into(),
            baseline_provenance: BTreeMap::new(),
            judge_model: "x".into(),
            headline_totals: ReportTotals { current_wins: 2, baseline_wins: 1, ties: 0 },
            per_axis: vec![
                AxisTotals {
                    axis: Axis::Correctness,
                    totals: ReportTotals { current_wins: 1, baseline_wins: 1, ties: 0 },
                },
                AxisTotals {
                    axis: Axis::Efficiency,
                    totals: ReportTotals { current_wins: 1, baseline_wins: 0, ties: 0 },
                },
            ],
            scenarios: Vec::new(),
        };
        let out = r.render_text(false);
        assert!(out.contains("Per axis"), "got:\n{out}");
        assert!(out.contains("correctness"), "got:\n{out}");
        assert!(out.contains("efficiency"), "got:\n{out}");
    }

    #[test]
    fn render_text_omits_per_scenario_when_not_verbose() {
        let r = Report {
            model: "x".into(),
            results_git_ref: "x".into(),
            results_path: "x".into(),
            baseline_provenance: BTreeMap::new(),
            judge_model: "x".into(),
            headline_totals: ReportTotals { current_wins: 1, baseline_wins: 0, ties: 0 },
            per_axis: Vec::new(),
            scenarios: vec![ScenarioReport {
                scenario: "_smoke".into(),
                outcome: ScenarioOutcome::Graded {
                    per_run_scores: vec![vec![]],
                    per_axis: Vec::new(),
                },
            }],
            scenarios: vec![],
        };
        let out = r.render_text(false);
        // Non-verbose: scenario detail must NOT appear.
        assert!(!out.contains("_smoke"), "scenario name should not appear in non-verbose; got:\n{out}");
    }

    #[test]
    fn render_text_includes_per_scenario_when_verbose() {
        let r = Report {
            model: "x".into(),
            results_git_ref: "x".into(),
            results_path: "x".into(),
            baseline_provenance: BTreeMap::new(),
            judge_model: "x".into(),
            headline_totals: ReportTotals { current_wins: 1, baseline_wins: 0, ties: 0 },
            per_axis: Vec::new(),
            scenarios: vec![ScenarioReport {
                scenario: "_smoke".into(),
                outcome: ScenarioOutcome::Graded {
                    per_run_scores: vec![vec![]],
                    per_axis: vec![AxisTotals {
                        axis: Axis::Correctness,
                        totals: ReportTotals { current_wins: 1, baseline_wins: 0, ties: 0 },
                    }],
                },
            }],
        };
        let out = r.render_text(true);
        assert!(out.contains("_smoke"), "verbose must include scenario name; got:\n{out}");
    }

    #[test]
    fn render_text_lists_skipped_scenarios_with_reason() {
        let r = Report {
            model: "x".into(),
            results_git_ref: "x".into(),
            results_path: "x".into(),
            baseline_provenance: BTreeMap::new(),
            judge_model: "x".into(),
            headline_totals: ReportTotals::default(),
            per_axis: Vec::new(),
            scenarios: vec![ScenarioReport {
                scenario: "missing-bl".into(),
                outcome: ScenarioOutcome::Skipped {
                    reason: "no baseline for X".into(),
                },
            }],
        };
        let out = r.render_text(false);
        assert!(out.contains("Skipped"), "got:\n{out}");
        assert!(out.contains("missing-bl"), "got:\n{out}");
    }
```

Fix the duplicate `scenarios` field in the third test (I copy-pasted; delete the extra `scenarios: vec![]`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib eval::report::tests::render_text`
Expected: FAIL — `cannot find method 'render_text'`.

- [ ] **Step 3: Implement `Report::render_text`**

Append to `src/eval/report.rs`:

```rust
impl Report {
    /// Render the layered text report. `verbose` enables the
    /// per-scenario section.
    pub fn render_text(&self, verbose: bool) -> String {
        let mut out = String::new();

        // Metadata block.
        out.push_str(&format!(
            "Eval results — current ({} at {}) vs baseline\n",
            self.model, self.results_git_ref
        ));
        if let Some((scn, prov)) = self.baseline_provenance.iter().next() {
            // If all baselines are from the same git_ref, show it
            // once. Otherwise show per-scenario provenance in the
            // verbose section.
            let single_ref = self
                .baseline_provenance
                .values()
                .all(|p| p.git_ref == prov.git_ref);
            if single_ref {
                out.push_str(&format!(
                    "  baseline frozen {} at {} ({} scenarios)\n\n",
                    prov.frozen_at, prov.git_ref, self.baseline_provenance.len()
                ));
            } else {
                out.push_str(&format!(
                    "  baselines from {} scenarios (varied refs — see --verbose)\n\n",
                    self.baseline_provenance.len()
                ));
            }
            drop(scn); // not used
        } else {
            out.push('\n');
        }

        // Headline.
        out.push_str("  Headline:        ");
        out.push_str(&format!(
            "{} net win rate ({:.1}% non-regression)\n",
            format_signed_percent(self.headline_totals.net_win_rate()),
            self.headline_totals.non_regression_rate() * 100.0
        ));

        // Skipped section.
        let skipped: Vec<&ScenarioReport> = self
            .scenarios
            .iter()
            .filter(|s| matches!(s.outcome, ScenarioOutcome::Skipped { .. }))
            .collect();
        if !skipped.is_empty() {
            out.push_str(&format!("  Skipped:         {} scenarios\n", skipped.len()));
            for s in &skipped {
                if let ScenarioOutcome::Skipped { reason } = &s.outcome {
                    out.push_str(&format!("                   - {}: {}\n", s.scenario, reason));
                }
            }
        }
        out.push('\n');

        // Per-axis.
        if !self.per_axis.is_empty() {
            out.push_str("  Per axis:\n");
            for ax in &self.per_axis {
                out.push_str(&format!(
                    "    {:14} {} net win rate (won {} / lost {} / tied {})\n",
                    format!("{}:", ax.axis),
                    format_signed_percent(ax.totals.net_win_rate()),
                    ax.totals.current_wins,
                    ax.totals.baseline_wins,
                    ax.totals.ties,
                ));
            }
            out.push('\n');
        }

        if !verbose {
            out.push_str("  See --verbose for per-scenario breakdown.\n");
            return out;
        }

        // Per-scenario (verbose only).
        out.push_str("  Per scenario:\n");
        for sr in &self.scenarios {
            match &sr.outcome {
                ScenarioOutcome::Graded { per_axis, per_run_scores } => {
                    out.push_str(&format!(
                        "    {} ({} runs):\n",
                        sr.scenario,
                        per_run_scores.len()
                    ));
                    for ax in per_axis {
                        out.push_str(&format!(
                            "      {:14} {} (won {} / lost {} / tied {})\n",
                            format!("{}:", ax.axis),
                            format_signed_percent(ax.totals.net_win_rate()),
                            ax.totals.current_wins,
                            ax.totals.baseline_wins,
                            ax.totals.ties,
                        ));
                    }
                }
                ScenarioOutcome::Skipped { reason } => {
                    out.push_str(&format!("    {} — SKIPPED: {}\n", sr.scenario, reason));
                }
            }
        }

        out
    }
}

/// Format a net win rate as a signed percentage with one decimal.
/// `0.022` → `"+2.2%"`; `-0.014` → `"-1.4%"`; `0.0` → `" 0.0%"`.
fn format_signed_percent(v: f64) -> String {
    if v >= 0.0 {
        format!("+{:.1}%", v * 100.0)
    } else {
        format!("{:.1}%", v * 100.0)
    }
}
```

- [ ] **Step 4: Run the rendering tests**

Run: `cargo test --lib eval::report::tests::render_text`
Expected: PASS — all 5.

- [ ] **Step 5: Commit**

```bash
git add src/eval/report.rs
git commit -m "$(cat <<'EOF'
feat(eval): layered text rendering for Phase 8 report

Report::render_text emits the three-layer output described in
the spec (lines 528-542):

  Eval results — current (model at ref) vs baseline
    baseline frozen YYYY-MM-DD at <ref> (N scenarios)

    Headline:        +X.X% net win rate (YY.Y% non-regression)
    Skipped:         N scenarios
                     - <scenario>: <reason>

    Per axis:
      correctness:   +A.A% net win rate (won W / lost L / tied T)
      efficiency:    ...
      conciseness:   ...

    See --verbose for per-scenario breakdown.

`verbose=true` appends a Per-scenario section with one block per
scenario showing per-axis breakdown for that scenario alone.
Skipped scenarios render their reason inline so the operator sees
the "run `steve eval baseline freeze ...`" suggestion right there.

Refs: steve-u896
EOF
)"
```

---

## Task 6: Add `html-escape` crate dep

XSS-safe content escape in the HTML report uses the `html-escape` crate (well-maintained, focused on the escape-functions primitive; project preference is to use established libs over hand-rolled utilities). The crate exposes `encode_safe(s) -> Cow<str>` for general text-context escape (handles `&`, `<`, `>`, `"`, `'`) and `encode_quoted_attribute(s) -> Cow<str>` for attribute-context escape.

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add the dep**

In `Cargo.toml` under `[dependencies]`, add (alphabetically near other crates):

```toml
html-escape = "0.2"
```

- [ ] **Step 2: Verify it resolves**

Run: `cargo check`
Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
chore(eval): add html-escape 0.2 for Phase 8 HTML report

Phase 8's HTML report renderer (next commit) interpolates dynamic
content (scenario names, user turns, tool args, assistant
messages) into an HTML template. XSS hardening is load-bearing —
scenarios deliberately exercise the agent on real code, so any
attacker-supplied HTML/JS sequence in the agent's transcript
could end up in the report.

Use html-escape::encode_safe for text-context escape (handles
& < > " '). Established lib over hand-rolled — battle-tested
against edge cases (entity references, BOM handling, etc.) that
a 15-line custom function would miss.

Refs: steve-u896
EOF
)"
```

---

## Task 7: History.jsonl module

Append-only JSON Lines file at `eval/history.jsonl`. One row per recorded report. Written on `--record-history`; read by `--html` for trend charts.

**Files:**
- Create: `src/eval/history.rs`
- Modify: `src/eval/mod.rs` (declare submodule + re-exports)

- [ ] **Step 1: Write failing tests**

Create `src/eval/history.rs`:

```rust
//! Append-only JSONL history at `eval/history.jsonl`. One row per
//! recorded `steve eval report --record-history` invocation. Bare
//! `report` is read-only against the file.
//!
//! Schema per row matches spec lines 600-614: git_ref + recorded_at
//! + model + baseline_git_ref + judge_model + headline + per_axis +
//! deterministic_floor + results_file.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::eval::report::{Report, ReportTotals};

/// One row of the history file. Serializes as a single line of JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub git_ref: String,
    pub recorded_at: String,
    pub model: String,
    pub baseline_git_ref: String,
    pub judge_model: String,
    pub headline: HistoryHeadline,
    pub per_axis: BTreeMap<String, HistoryAxisEntry>,
    pub deterministic_floor: HistoryFloor,
    pub results_file: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryHeadline {
    pub net_win_rate: f64,
    pub non_regression_rate: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryAxisEntry {
    pub net_win_rate: f64,
    pub won: usize,
    pub lost: usize,
    pub tied: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryFloor {
    pub passed: usize,
    pub total: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn sample_entry() -> HistoryEntry {
        HistoryEntry {
            git_ref: "def5678".into(),
            recorded_at: "2026-05-12T14:23:00Z".into(),
            model: "ollama/qwen3-coder".into(),
            baseline_git_ref: "abc1234".into(),
            judge_model: "fuel-ix/claude-haiku-4-5".into(),
            headline: HistoryHeadline {
                net_win_rate: 0.022,
                non_regression_rate: 0.978,
            },
            per_axis: {
                let mut m = BTreeMap::new();
                m.insert("correctness".into(), HistoryAxisEntry {
                    net_win_rate: -0.033,
                    won: 1, lost: 2, tied: 27,
                });
                m
            },
            deterministic_floor: HistoryFloor { passed: 10, total: 10 },
            results_file: "path/to/results.yaml".into(),
        }
    }

    #[test]
    fn append_then_read_round_trips() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("history.jsonl");
        let e = sample_entry();
        append_history(&path, &e).unwrap();
        let rows = read_history(&path).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], e);
    }

    #[test]
    fn multiple_appends_preserve_order() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("history.jsonl");
        let mut e1 = sample_entry();
        e1.git_ref = "first".into();
        let mut e2 = sample_entry();
        e2.git_ref = "second".into();
        append_history(&path, &e1).unwrap();
        append_history(&path, &e2).unwrap();
        let rows = read_history(&path).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].git_ref, "first");
        assert_eq!(rows[1].git_ref, "second");
    }

    #[test]
    fn read_missing_file_returns_empty_vec() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("does-not-exist.jsonl");
        let rows = read_history(&path).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn append_each_row_is_single_line() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("history.jsonl");
        append_history(&path, &sample_entry()).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        // Exactly one newline at the end of the only row.
        assert_eq!(raw.matches('\n').count(), 1);
        // No embedded newlines from pretty-printing.
        let body = raw.trim_end();
        assert!(!body.contains('\n'), "JSON row must be single-line; got: {body:?}");
    }

    #[test]
    fn entry_from_report_extracts_spec_fields() {
        let r = Report {
            model: "ollama/qwen3-coder".into(),
            results_git_ref: "def5678".into(),
            results_path: "results.yaml".into(),
            baseline_provenance: {
                let mut m = BTreeMap::new();
                m.insert("_smoke".into(), crate::eval::report::BaselineProvenance {
                    git_ref: "abc1234".into(),
                    frozen_at: "2026-05-01T00:00:00Z".into(),
                });
                m
            },
            judge_model: "fuel-ix/claude-haiku-4-5".into(),
            headline_totals: ReportTotals { current_wins: 1, baseline_wins: 0, ties: 9 },
            per_axis: Vec::new(),
            scenarios: Vec::new(),
        };
        let entry = HistoryEntry::from_report(&r, "2026-05-12T14:23:00Z".into());
        assert_eq!(entry.git_ref, "def5678");
        assert_eq!(entry.model, "ollama/qwen3-coder");
        assert_eq!(entry.baseline_git_ref, "abc1234");
        assert_eq!(entry.judge_model, "fuel-ix/claude-haiku-4-5");
        assert!((entry.headline.net_win_rate - 0.1).abs() < 1e-9);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

In `src/eval/mod.rs`, add:

```rust
pub mod history;
pub use history::{HistoryEntry, append_history, read_history};
```

Run: `cargo test --lib eval::history::tests`
Expected: FAIL — `cannot find function 'append_history'`.

- [ ] **Step 3: Implement `append_history`, `read_history`, `HistoryEntry::from_report`**

In `src/eval/history.rs`, add (between the type definitions and the test module):

```rust
/// Append one `HistoryEntry` as a single line to `path`. Creates
/// the parent directory and the file if absent. Each row is exactly
/// one line of compact JSON (no pretty-printing — JSONL contract).
pub fn append_history(path: &Path, entry: &HistoryEntry) -> Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent dir for {}", path.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {} for append", path.display()))?;
    let line = serde_json::to_string(entry)
        .with_context(|| "serializing history entry")?;
    writeln!(file, "{line}")
        .with_context(|| format!("writing history row to {}", path.display()))?;
    Ok(())
}

/// Read every row in `path`, one per line. Returns an empty Vec if
/// the file doesn't exist (no rows yet ≠ error). Malformed rows
/// propagate as Err so corrupt history is loud rather than silently
/// dropping data.
pub fn read_history(path: &Path) -> Result<Vec<HistoryEntry>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(anyhow::Error::from(e).context(format!("reading {}", path.display()))),
    };
    let mut out = Vec::new();
    for (i, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: HistoryEntry = serde_json::from_str(line).with_context(|| {
            format!("parsing history row {} in {}", i + 1, path.display())
        })?;
        out.push(entry);
    }
    Ok(out)
}

impl HistoryEntry {
    /// Build a history row from a populated `Report`. `recorded_at`
    /// is passed separately so the caller controls the timestamp
    /// (typically `chrono::Utc::now()` formatted as RFC 3339).
    pub fn from_report(report: &Report, recorded_at: String) -> Self {
        // Pick a representative baseline git_ref. Spec assumes a
        // single anchor per report; if multiple appear (one per
        // scenario), use the first one and surface the divergence
        // only in the text render (which Task 5 already handles).
        let baseline_git_ref = report
            .baseline_provenance
            .values()
            .next()
            .map(|p| p.git_ref.clone())
            .unwrap_or_else(|| "unknown".into());
        let per_axis = report
            .per_axis
            .iter()
            .map(|ax| {
                (
                    format!("{}", ax.axis),
                    HistoryAxisEntry {
                        net_win_rate: ax.totals.net_win_rate(),
                        won: ax.totals.current_wins,
                        lost: ax.totals.baseline_wins,
                        tied: ax.totals.ties,
                    },
                )
            })
            .collect();
        HistoryEntry {
            git_ref: report.results_git_ref.clone(),
            recorded_at,
            model: report.model.clone(),
            baseline_git_ref,
            judge_model: report.judge_model.clone(),
            headline: HistoryHeadline {
                net_win_rate: report.headline_totals.net_win_rate(),
                non_regression_rate: report.headline_totals.non_regression_rate(),
            },
            per_axis,
            // Deterministic floor info comes from the legacy
            // assertion channel; not yet plumbed through to Report.
            // Spec lists it as a field; we ship with passed=total=0
            // for now and surface it in a follow-up issue (the
            // floor still runs at Phase-7 grade time; we just don't
            // currently track its tallies in the Report struct).
            deterministic_floor: HistoryFloor { passed: 0, total: 0 },
            results_file: report.results_path.clone(),
        }
    }
}
```

- [ ] **Step 4: Run the history tests**

Run: `cargo test --lib eval::history::tests`
Expected: ALL PASS.

- [ ] **Step 5: File a follow-up bd issue for deterministic_floor plumbing**

The history schema has a `deterministic_floor: {passed, total}` field per spec. Phase 8 doesn't plumb this through `Report` yet because the rule-based assertion channel runs at Phase-7 grade time (in `eval run`), and its tallies aren't currently surfaced into the results file. File a follow-up:

```bash
bd create \
  --title="Eval Phase 8: plumb deterministic_floor tallies into history.jsonl" \
  --description="Phase 8's history.jsonl schema has a deterministic_floor: {passed, total} field per spec (line 600-614). The current implementation ships with passed=total=0 because the rule-based assertion channel runs in Phase-7's eval run (inside Runner::evaluate via expectations.rs) but its per-scenario pass/fail counts aren't surfaced into the ResultsFile that Phase 8 consumes. To wire this up: (a) extend ScenarioResults with a deterministic_floor_passed_runs: usize field that's computed at eval-run time from the existing EvalReport; (b) thread this into Report::build_from_results so HistoryEntry::from_report can fill the field with real values. Low priority because the headline metric already captures correctness; this is for cumulative-floor tracking in the trend chart." \
  --type=task \
  --priority=3
```

- [ ] **Step 6: Commit**

```bash
git add src/eval/history.rs src/eval/mod.rs .beads/issues.jsonl
git commit -m "$(cat <<'EOF'
feat(eval): history.jsonl append + read for Phase 8

Schema mirrors spec lines 600-614:

  { git_ref, recorded_at, model, baseline_git_ref, judge_model,
    headline: { net_win_rate, non_regression_rate },
    per_axis: { <axis>: { net_win_rate, won, lost, tied } },
    deterministic_floor: { passed, total },
    results_file: "path/to/results.yaml" }

append_history: OpenOptions::append + writeln! — one row per
line, compact JSON, single-newline-terminated. Creates the file
+ parent dir if absent.

read_history: reads the whole file, parses each line. Missing
file returns Ok(Vec::new()) — no rows yet is not an error.
Malformed rows propagate as Err so corruption is loud.

HistoryEntry::from_report extracts the spec-required fields from
a populated Report. deterministic_floor ships with passed=total=0
(follow-up issue filed) — the rule-based assertion tallies aren't
yet surfaced through the ResultsFile shape.

Refs: steve-u896
EOF
)"
```

---

## Task 8: HTML report rendering with bundled Chart.js + OAuth-style palette

Build the self-contained HTML report. Reuses the MCP OAuth callback page's visual language (`src/mcp/oauth/callback.rs`): warm amber gradient background, white card on top, brown text palette, "steve · rust tui coding agent" footer. Layout adapted from "single centered card" to "wider tabular page" but keeps the same colors + fonts + Steve-branded chrome. Uses `html_escape::encode_safe` for XSS-safe interpolation.

**Files:**
- Create: `src/eval/html_report.rs`
- Modify: `src/eval/mod.rs` (declare submodule + re-export `render_html`)

- [ ] **Step 1: Write failing tests**

Create `src/eval/html_report.rs`:

```rust
//! Self-contained HTML report renderer for `steve eval report --html`.
//!
//! Single-file output: bundles Chart.js inline (~80KB) so the report
//! renders offline (CI artifacts, archived issues). All dynamic
//! content goes through `html_escape::encode_safe` to prevent XSS —
//! scenario names, user turns, tool args, tool outputs, and assistant
//! messages can all carry attacker-supplied HTML/JS sequences
//! (scenarios deliberately exercise the agent on real code).
//!
//! Visual language matches `src/mcp/oauth/callback.rs` — warm amber
//! gradient, white card on top, brown text palette, Steve-branded
//! footer. Layout is adapted from the OAuth page's single centered
//! card to a wider tabular page for the multi-section report content.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::history::HistoryEntry;
    use crate::eval::report::{Report, ReportTotals, ScenarioReport, ScenarioOutcome, AxisTotals, BaselineProvenance};
    use crate::eval::score::Axis;
    use std::collections::BTreeMap;

    fn report_with_xss_attempt() -> Report {
        Report {
            model: "test/model".into(),
            results_git_ref: "abc1234".into(),
            results_path: "results.yaml".into(),
            baseline_provenance: BTreeMap::new(),
            judge_model: "test/judge".into(),
            headline_totals: ReportTotals { current_wins: 1, baseline_wins: 0, ties: 0 },
            per_axis: Vec::new(),
            scenarios: vec![ScenarioReport {
                scenario: r#"<script>alert("xss")</script>"#.into(),
                outcome: ScenarioOutcome::Skipped {
                    reason: r#"<img onerror="alert(1)" src=x>"#.into(),
                },
            }],
        }
    }

    #[test]
    fn html_report_escapes_dynamic_content_to_prevent_xss() {
        let r = report_with_xss_attempt();
        let html = render_html(&r, &[]);
        // The original payload tags must not appear raw.
        assert!(!html.contains("<script>alert"), "found unescaped script tag in HTML:\n{html}");
        assert!(!html.contains(r#"onerror="alert(1)""#), "found raw event handler:\n{html}");
        // The escaped form should appear (html_escape::encode_safe
        // converts `<` to `&lt;`).
        assert!(html.contains("&lt;script&gt;"), "expected escaped script tag\n{html}");
        assert!(html.contains("&lt;img"), "expected escaped img tag\n{html}");
    }

    #[test]
    fn html_report_contains_chartjs_bundle_marker() {
        // The Chart.js bundle is embedded as a static string. Look
        // for a marker that should appear somewhere in v4 UMD.
        let r = report_with_xss_attempt();
        let html = render_html(&r, &[]);
        assert!(
            html.contains("Chart") && html.contains("MIT License"),
            "expected Chart.js bundle + MIT license to be embedded"
        );
    }

    #[test]
    fn html_report_includes_headline_percentage() {
        let r = Report {
            model: "test/model".into(),
            results_git_ref: "x".into(),
            results_path: "x".into(),
            baseline_provenance: BTreeMap::new(),
            judge_model: "x".into(),
            headline_totals: ReportTotals { current_wins: 5, baseline_wins: 0, ties: 5 },
            per_axis: vec![AxisTotals {
                axis: Axis::Correctness,
                totals: ReportTotals { current_wins: 2, baseline_wins: 1, ties: 0 },
            }],
            scenarios: Vec::new(),
        };
        let html = render_html(&r, &[]);
        // (5-0)/10 = 0.5 → 50.0%
        assert!(html.contains("50.0%") || html.contains("+50.0%"), "got:\n{html}");
    }

    #[test]
    fn html_report_omits_trends_section_when_history_empty() {
        let r = report_with_xss_attempt();
        let html = render_html(&r, &[]);
        // No trend chart canvas when history is empty.
        assert!(!html.contains("trendChart"), "trend canvas should be absent when history is empty");
    }

    #[test]
    fn html_report_emits_trends_section_when_history_has_rows() {
        let r = report_with_xss_attempt();
        let history = vec![/* one row */ HistoryEntry {
            git_ref: "x".into(),
            recorded_at: "2026-05-12T00:00:00Z".into(),
            model: "test/model".into(),
            baseline_git_ref: "x".into(),
            judge_model: "x".into(),
            headline: crate::eval::history::HistoryHeadline {
                net_win_rate: 0.02,
                non_regression_rate: 0.98,
            },
            per_axis: BTreeMap::new(),
            deterministic_floor: crate::eval::history::HistoryFloor { passed: 0, total: 0 },
            results_file: "x".into(),
        }];
        let html = render_html(&r, &history);
        assert!(html.contains("trendChart"), "trend canvas should be present when history has rows");
        // The JSON data for the chart must escape its dynamic content.
        // Confirm git_refs appear in the data array.
        assert!(html.contains(r#""x""#) || html.contains("'x'"), "trend data should include the git_ref");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib eval::html_report::tests::html_report`
Expected: FAIL — `cannot find function 'render_html'`.

- [ ] **Step 3: Implement `render_html` (with OAuth-style palette + html-escape lib)**

Prepend to `src/eval/html_report.rs` (before `#[cfg(test)]`):

```rust
use html_escape::encode_safe;

use crate::eval::history::HistoryEntry;
use crate::eval::report::{Report, ScenarioOutcome};

/// The Chart.js v4.4.7 UMD minified bundle, embedded at build time.
const CHARTJS_BUNDLE: &str = include_str!("../../assets/chartjs.min.js");

/// Chart.js MIT license text, embedded adjacent to the JS bundle so
/// the two cannot drift. Per spec: "must check it in as a static
/// string adjacent to the bundled JS so the two can never drift
/// apart."
const CHARTJS_LICENSE: &str = include_str!("../../assets/chartjs-LICENSE.txt");

/// CSS palette matching `src/mcp/oauth/callback.rs` — warm amber
/// gradient background, white card content, brown text. Layout is
/// adapted from the OAuth page's single centered card to a wider
/// tabular page (`max-width: 900px`) since the report content is
/// multi-section (headline + per-axis + per-scenario + trend chart).
const REPORT_CSS: &str = r#"
* { margin: 0; padding: 0; box-sizing: border-box; }
body {
  min-height: 100vh;
  padding: 2rem 1rem;
  background: linear-gradient(135deg, #fff8e1 0%, #ffecb3 100%);
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
  color: #3e2723;
  line-height: 1.5;
}
.container {
  max-width: 900px;
  margin: 0 auto;
}
.card {
  background: #fff;
  border-radius: 20px;
  box-shadow: 0 8px 32px rgba(0,0,0,0.10);
  padding: 32px 36px;
  margin-bottom: 24px;
}
h1 { font-size: 1.5rem; font-weight: 700; margin-bottom: 0.5rem; }
h2 { font-size: 1.1rem; font-weight: 600; margin: 1.25rem 0 0.5rem; color: #5d4037; }
.meta { font-size: 0.9rem; color: #5d4037; margin-bottom: 0.5rem; }
.meta code { background: #fff8e1; padding: 1px 6px; border-radius: 6px; font-size: 0.85em; }
table { width: 100%; border-collapse: collapse; margin-top: 0.5rem; }
th, td { padding: 0.5rem 0.75rem; text-align: left; border-bottom: 1px solid #f1ede0; }
th { background: #fff8e1; font-weight: 600; color: #5d4037; }
.pos { color: #2e7d32; font-weight: 600; }
.neg { color: #c62828; font-weight: 600; }
.tie { color: #a1887f; }
.status { display: inline-block; padding: 4px 12px; border-radius: 999px; font-size: 0.75rem; font-weight: 600; letter-spacing: 0.02em; }
.status.skipped { background: #fff3e0; color: #e65100; }
.status.graded  { background: #e8f5e9; color: #2e7d32; }
canvas { max-width: 100%; }
.footer { margin-top: 1rem; font-size: 0.8rem; color: #a1887f; text-align: center; }
"#;

/// Render a self-contained HTML report. Inlines all dynamic content
/// XSS-safely via `html_escape::encode_safe`, embeds the Chart.js
/// bundle + MIT license, and (optionally) emits a trend chart from
/// `history`.
pub fn render_html(report: &Report, history: &[HistoryEntry]) -> String {
    let mut out = String::with_capacity(CHARTJS_BUNDLE.len() + 16 * 1024);
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\"><head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str(&format!(
        "<title>Steve — eval report — {} at {}</title>\n",
        encode_safe(&report.model),
        encode_safe(&report.results_git_ref)
    ));
    out.push_str("<style>");
    out.push_str(REPORT_CSS);
    out.push_str("</style>\n");
    out.push_str("</head><body>\n");

    // ── License comment ──
    out.push_str("<!--\n");
    out.push_str("Chart.js is bundled below under the MIT License.\n");
    out.push_str("Copyright (c) Chart.js Contributors. See https://www.chartjs.org/docs/latest/\n");
    out.push_str("Full license text:\n");
    // Escape the license text — `-->` inside it would break comment
    // boundaries. (Real MIT doesn't contain `-->`, but escape-by-
    // default is the load-bearing posture.)
    out.push_str(&encode_safe(CHARTJS_LICENSE));
    out.push_str("\n-->\n");

    out.push_str("<div class=\"container\">\n");

    // ── Header card (metadata) ──
    out.push_str("<div class=\"card\">\n");
    out.push_str("<h1>Eval report</h1>\n");
    out.push_str(&format!(
        "<p class=\"meta\">model: <code>{}</code> · git ref: <code>{}</code> · judge: <code>{}</code></p>\n",
        encode_safe(&report.model),
        encode_safe(&report.results_git_ref),
        encode_safe(&report.judge_model),
    ));
    out.push_str("</div>\n");

    // ── Headline card ──
    let headline = report.headline_totals;
    out.push_str("<div class=\"card\">\n");
    out.push_str("<h2>Headline</h2>\n<table>\n");
    out.push_str(&format!(
        "<tr><th>Net win rate</th><td class=\"{}\">{:+.1}%</td></tr>\n",
        if headline.net_win_rate() >= 0.0 { "pos" } else { "neg" },
        headline.net_win_rate() * 100.0,
    ));
    out.push_str(&format!(
        "<tr><th>Non-regression rate</th><td>{:.1}%</td></tr>\n",
        headline.non_regression_rate() * 100.0,
    ));
    out.push_str(&format!(
        "<tr><th>Verdicts</th><td>won {} · lost {} · tied {}</td></tr>\n",
        headline.current_wins, headline.baseline_wins, headline.ties,
    ));
    out.push_str("</table>\n");
    out.push_str("</div>\n");

    // ── Per-axis card ──
    if !report.per_axis.is_empty() {
        out.push_str("<div class=\"card\">\n");
        out.push_str("<h2>Per axis</h2>\n<table>\n<tr><th>Axis</th><th>Net win rate</th><th>Won</th><th>Lost</th><th>Tied</th></tr>\n");
        for ax in &report.per_axis {
            out.push_str(&format!(
                "<tr><td>{}</td><td class=\"{}\">{:+.1}%</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                encode_safe(&format!("{}", ax.axis)),
                if ax.totals.net_win_rate() >= 0.0 { "pos" } else { "neg" },
                ax.totals.net_win_rate() * 100.0,
                ax.totals.current_wins,
                ax.totals.baseline_wins,
                ax.totals.ties,
            ));
        }
        out.push_str("</table>\n");
        out.push_str("</div>\n");
    }

    // ── Per-scenario card ──
    out.push_str("<div class=\"card\">\n");
    out.push_str("<h2>Per scenario</h2>\n<table>\n<tr><th>Scenario</th><th>Status</th><th>Details</th></tr>\n");
    for sr in &report.scenarios {
        let scenario_escaped = encode_safe(&sr.scenario);
        match &sr.outcome {
            ScenarioOutcome::Graded { per_axis, per_run_scores } => {
                let detail: String = per_axis
                    .iter()
                    .map(|ax| format!("{}: won {} / lost {} / tied {}", ax.axis, ax.totals.current_wins, ax.totals.baseline_wins, ax.totals.ties))
                    .collect::<Vec<_>>()
                    .join("; ");
                out.push_str(&format!(
                    "<tr><td>{scenario_escaped}</td><td><span class=\"status graded\">Graded ({} runs)</span></td><td>{}</td></tr>\n",
                    per_run_scores.len(),
                    encode_safe(&detail),
                ));
            }
            ScenarioOutcome::Skipped { reason } => {
                out.push_str(&format!(
                    "<tr><td>{scenario_escaped}</td><td><span class=\"status skipped\">Skipped</span></td><td>{}</td></tr>\n",
                    encode_safe(reason),
                ));
            }
        }
    }
    out.push_str("</table>\n");
    out.push_str("</div>\n");

    // ── Trend chart card (only if history has rows) ──
    if !history.is_empty() {
        let labels: Vec<String> = history.iter().map(|h| h.git_ref.clone()).collect();
        let values: Vec<f64> = history.iter().map(|h| h.headline.net_win_rate).collect();
        let labels_json = serde_json::to_string(&labels).unwrap_or_else(|_| "[]".into());
        let values_json = serde_json::to_string(&values).unwrap_or_else(|_| "[]".into());

        out.push_str("<div class=\"card\">\n");
        out.push_str("<h2>Trends over time</h2>\n<canvas id=\"trendChart\"></canvas>\n");
        out.push_str("</div>\n");
        out.push_str("<script>\n");
        out.push_str(CHARTJS_BUNDLE);
        out.push_str("\n</script>\n");
        out.push_str("<script>\n");
        out.push_str(&format!(
            "new Chart(document.getElementById('trendChart'), {{type: 'line', data: {{labels: {labels_json}, datasets: [{{label: 'net win rate', data: {values_json}, borderColor: '#2e7d32', backgroundColor: 'rgba(46,125,50,0.1)', tension: 0.2, fill: true}}]}}, options: {{plugins: {{legend: {{display: false}}}}, scales: {{y: {{title: {{display: true, text: 'Net win rate'}}}}, x: {{title: {{display: true, text: 'git ref'}}}}}}}}}});\n"
        ));
        out.push_str("</script>\n");
    }

    out.push_str("<p class=\"footer\">steve · rust tui coding agent</p>\n");
    out.push_str("</div>\n");
    out.push_str("</body></html>\n");
    out
}
```

Important details:
- The Chart.js bundle is inserted between `<script>` and `</script>` tags. Since the bundle is minified UMD, it never contains a literal `</script>` substring (which would otherwise break out of the script context). If a future Chart.js update introduces one, this would fail; document as a load-bearing invariant via a `debug_assert!` later if it becomes a concern.
- `serde_json::to_string` on `Vec<String>` produces a properly-escaped JSON array — that's the right escape boundary for the Chart.js `data` object because the JS parser parses it as JSON syntax (which IS a subset of JS syntax in this context).
- The trend chart only renders when `history` is non-empty (spec: "Skipped if history.jsonl is empty").
- `html_escape::encode_safe` handles the 5 text-context special chars (`&`, `<`, `>`, `"`, `'`). For attribute-value interpolation (none in this template), use `encode_quoted_attribute` instead.

- [ ] **Step 4: Run the HTML render tests**

Run: `cargo test --lib eval::html_report::tests::html_report`
Expected: ALL PASS — XSS escape, Chart.js bundle present, headline percentage, trends omit/include based on history.

- [ ] **Step 5: Smoke a manual HTML render**

Add a temporary integration test that writes a real HTML file to a tempdir, then visually inspect it in a browser:

```rust
    #[test]
    #[ignore]
    fn manual_html_smoke() {
        let report = report_with_xss_attempt(); // or build a richer one
        let html = render_html(&report, &[]);
        let path = std::env::temp_dir().join("steve-eval-smoke.html");
        std::fs::write(&path, html).unwrap();
        println!("HTML smoke written to: {}", path.display());
    }
```

Run with: `cargo test --lib eval::html_report::tests::manual_html_smoke -- --ignored --nocapture`. Open the printed path in a browser. Verify the page renders, no console errors, the headline section is visible.

**Delete this smoke test before committing** (it's marked `#[ignore]` so it won't run in CI, but it's clutter).

- [ ] **Step 6: Commit**

```bash
git add src/eval/html_report.rs
git commit -m "$(cat <<'EOF'
feat(eval): self-contained HTML report renderer for Phase 8

render_html(report, history) produces a single-file HTML output:

- Visual language reuses src/mcp/oauth/callback.rs: warm amber
  gradient background, white card sections on top, brown text
  palette, Steve-branded footer. Layout adapted from the OAuth
  page's single-card form to a wider tabular page (max-width 900px)
  with one card per section.
- Headline + per-axis + per-scenario sections; status pills
  (graded/skipped) matching the OAuth callback's success/error/
  warning styling. Win/loss color coding (green/red) for net
  percentages.
- Trend chart from history.jsonl rows, omitted when history is
  empty (per spec).
- Chart.js v4.4.7 UMD minified bundled inline via include_str!.
- MIT license text embedded in an HTML comment adjacent to the JS
  bundle, also via include_str!, so the two cannot drift.

XSS hardening is load-bearing: ALL caller-supplied text (scenario
names, skip reasons, axis labels, per-scenario details) flows
through html_escape::encode_safe before reaching the output buffer.
Tests pin both common XSS payloads (<script>alert("hi")</script> →
&lt;script&gt;alert(&quot;hi&quot;)&lt;/script&gt;) and an
attribute-breakout payload (` onerror="alert(1)`).

Chart data is JSON-escaped via serde_json::to_string — the values
embed as a JSON array inside the <script> body, where JSON parser
semantics handle further escaping.

Refs: steve-u896
EOF
)"
```

---

## Task 9: `report_subcommand` + CLI wiring + exit codes

Wire `report_subcommand` into `src/eval/cli.rs` and add the `Report` variant to `EvalSubcommand` in `main.rs`. Translate the regression-threshold logic into exit codes 0/1/2.

**Files:**
- Modify: `src/eval/cli.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Implement `report_subcommand`**

In `src/eval/cli.rs`, add (near the existing `run_subcommand` and `freeze_subcommand`):

```rust
/// `steve eval report <results.yaml>` — load a results file, resolve
/// per-scenario baselines from `baselines_dir`, judge each
/// (scenario, run) pair via `Judge::compare`, render the layered
/// text report to stdout, optionally write HTML to `html_out`,
/// optionally append a history row.
///
/// Returns a `ReportExitCode` per spec: Pass (0), Regression (1),
/// or InfraError (2). The caller in main.rs translates this to
/// `std::process::exit(...)`.
#[allow(clippy::too_many_arguments)]
pub async fn report_subcommand(
    results_path: &Path,
    baselines_dir: &Path,
    scenarios_dir: &Path,
    judge_model: Option<&str>,
    cli_regression_threshold: Option<f64>,
    config_regression_threshold: Option<f64>,
    verbose: bool,
    record_history: bool,
    html_out: Option<&Path>,
    history_path: &Path,
    registry: &crate::provider::ProviderRegistry,
) -> Result<ReportExitCode> {
    use crate::eval::{
        Judge, history::{HistoryEntry, append_history, read_history},
        html_report::render_html,
        report::Report,
        results::ResultsFile,
    };

    // Load results.yaml.
    let results = ResultsFile::read_from_path(results_path)
        .with_context(|| format!("loading results from {}", results_path.display()))?;

    // Resolve judge model. CLI > scenario default (handled by
    // build_from_results internally) > error.
    let resolved_judge = judge_model
        .ok_or_else(|| anyhow::anyhow!(
            "no --judge-model and no default available; pass --judge-model <provider/model>"
        ))?;
    let judge = Judge::from_registry(registry, Some(resolved_judge));

    // Orchestrate.
    let report = Report::build_from_results(
        &results,
        baselines_dir,
        &results_path.display().to_string(),
        &judge,
        resolved_judge,
        Some(scenarios_dir),
    )
    .await?;

    // Print layered text to stdout.
    print!("{}", report.render_text(verbose));

    // Optional HTML output.
    if let Some(html_path) = html_out {
        let history = read_history(history_path)
            .with_context(|| format!("reading history from {}", history_path.display()))?;
        let html = render_html(&report, &history);
        std::fs::write(html_path, html)
            .with_context(|| format!("writing HTML to {}", html_path.display()))?;
        println!("wrote HTML report to {}", html_path.display());
    }

    // Optional history append.
    if record_history {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let entry = HistoryEntry::from_report(&report, now);
        append_history(history_path, &entry)?;
        println!("appended history row to {}", history_path.display());
    }

    // Exit code based on regression threshold.
    let threshold = cli_regression_threshold
        .or(config_regression_threshold)
        .unwrap_or(0.0);
    let exit = if report.headline_totals.net_win_rate() < threshold {
        ReportExitCode::Regression
    } else {
        ReportExitCode::Pass
    };
    Ok(exit)
}

/// Phase 8 exit codes. Mapped to process exit by `main.rs`:
/// `Pass=0`, `Regression=1`, `InfraError=2`. The InfraError variant
/// is reserved for the non-`Ok` return of `report_subcommand` (the
/// `?` operator propagates `anyhow::Error`s, which main.rs maps to
/// code 2 generically).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportExitCode {
    Pass,
    Regression,
}

impl ReportExitCode {
    pub fn as_i32(&self) -> i32 {
        match self {
            ReportExitCode::Pass => 0,
            ReportExitCode::Regression => 1,
        }
    }
}
```

- [ ] **Step 2: Add `Report` variant to `EvalSubcommand` in `main.rs`**

Find `enum EvalSubcommand` in `src/main.rs` and add:

```rust
    /// Run the paired-comparison report on an existing results.yaml.
    Report {
        /// Path to the results file produced by `steve eval run`.
        #[arg(value_name = "RESULTS")]
        results: PathBuf,
        /// Override the directory baseline files are resolved from.
        /// Default: `<project_root>/eval/baselines/`.
        #[arg(long)]
        baselines_dir: Option<PathBuf>,
        /// Override the scenarios directory (used to read per-scenario
        /// `[scoring].axes` and `judge_model` overrides).
        /// Default: `<project_root>/eval/scenarios/`.
        #[arg(long)]
        scenarios_dir: Option<PathBuf>,
        /// Override the judge model. Format: `provider/model_id`.
        #[arg(long)]
        judge_model: Option<String>,
        /// Append a row to `eval/history.jsonl` recording this run.
        /// Off by default — local exploratory runs don't pollute history.
        #[arg(long)]
        record_history: bool,
        /// Write a self-contained HTML report to this path.
        #[arg(long, value_name = "PATH")]
        html: Option<PathBuf>,
        /// Net win rate threshold for the exit code. Below this value
        /// = regression (exit 1). Default sourced from
        /// `eval.regression_threshold` in .steve.jsonc, or 0.0.
        #[arg(long, value_name = "FLOAT")]
        regression_threshold: Option<f64>,
        /// Show per-scenario detail in the text output.
        #[arg(long)]
        verbose: bool,
    },
```

Then in the dispatch logic, add a match arm calling `report_subcommand` and handle the `ReportExitCode` return via `std::process::exit(code.as_i32())`:

```rust
        Some(EvalSubcommand::Report {
            results,
            baselines_dir,
            scenarios_dir,
            judge_model,
            record_history,
            html,
            regression_threshold,
            verbose,
        }) => {
            let baselines_dir = baselines_dir
                .unwrap_or_else(|| project_root.join("eval").join("baselines"));
            let scenarios_dir = scenarios_dir
                .unwrap_or_else(|| project_root.join("eval").join("scenarios"));
            let history_path = project_root.join("eval").join("history.jsonl");
            let config = config::load(&project_root)?.0;
            let eval_config = config::load_eval_config(&project_root)?;
            let config_threshold = eval_config.regression_threshold;
            // Resolve default judge model: --judge-model > eval.default_judge_model
            let judge_model_resolved = judge_model
                .or(eval_config.default_judge_model.clone());
            let registry = build_provider_registry(&config)?;
            let exit = eval::cli::report_subcommand(
                &results,
                &baselines_dir,
                &scenarios_dir,
                judge_model_resolved.as_deref(),
                regression_threshold,
                config_threshold,
                verbose,
                record_history,
                html.as_deref(),
                &history_path,
                &registry,
            )
            .await?;
            std::process::exit(exit.as_i32());
        }
```

- [ ] **Step 3: Update `main.rs` error handling to map `anyhow::Error` to exit 2**

At the top of `main()`, wrap the call so any `Err` returns exit 2:

```rust
#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(2);
    }
}

async fn run() -> anyhow::Result<()> {
    // ... existing main body
}
```

(Phase 5 currently has `main()` returning `anyhow::Result<()>`. Restructure so any `Err` from the eval subcommands surfaces as exit 2; `Ok` flows through to exit 0 unless a `std::process::exit` was called inside a sub-handler.)

- [ ] **Step 4: Write an integration test for the end-to-end flow**

In `src/eval/cli.rs`'s `mod integration_tests`, append:

```rust
    #[tokio::test]
    async fn report_subcommand_end_to_end_via_disk() {
        use crate::eval::{
            baseline::BaselineFile,
            results::{ResultsFile, ScenarioResults},
            transcript::NormalizedTranscript,
        };
        use std::collections::BTreeMap;
        let tmp = TempDir::new().unwrap();

        // Set up results file.
        let results = ResultsFile {
            git_ref: "abc1234".into(),
            recorded_at: "2026-05-12T00:00:00Z".into(),
            model: "test/model".into(),
            scenarios: {
                let mut m = BTreeMap::new();
                m.insert("_smoke".into(), ScenarioResults {
                    user_turns: vec!["go".into()],
                    runs: vec![NormalizedTranscript {
                        events: vec![],
                        deterministic_floor_passed: true,
                        usage_summary: Default::default(),
                    }],
                });
                m
            },
        };
        let results_path = tmp.path().join("results.yaml");
        results.write_to_path(&results_path).unwrap();

        // Set up baseline.
        let baselines_dir = tmp.path().join("baselines");
        std::fs::create_dir_all(&baselines_dir).unwrap();
        let baseline = BaselineFile {
            scenario: "_smoke".into(),
            model: "test/model".into(),
            git_ref: "abc1234".into(),
            frozen_at: "2026-05-01T00:00:00Z".into(),
            user_turns: vec!["go".into()],
            transcript: NormalizedTranscript {
                events: vec![],
                deterministic_floor_passed: true,
                usage_summary: Default::default(),
            },
        };
        let baseline_path = crate::eval::baseline::baseline_path(
            &baselines_dir, "_smoke", "test/model",
        ).unwrap();
        std::fs::create_dir_all(baseline_path.parent().unwrap()).unwrap();
        baseline.write_to_path(&baseline_path).unwrap();

        // Set up empty scenarios dir.
        let scenarios_dir = tmp.path().join("scenarios");
        std::fs::create_dir_all(&scenarios_dir).unwrap();

        // We need a registry. Build a minimal one — Phase 5 helpers
        // exist for fake providers; reuse them. (Adjust to match
        // the existing test scaffold.)
        // ... omitted for brevity; consult `freeze_pipeline_round_trip_via_disk`
        // for the registry-build pattern.

        let history_path = tmp.path().join("history.jsonl");
        // ... call report_subcommand and assert exit code == Pass
        // (or Regression, depending on the canned judge response).
    }
```

(This integration test requires the test scaffold for `ProviderRegistry` and a fake judge backend. It's modeled after the existing `freeze_pipeline_round_trip_via_disk` test in `integration_tests`. If the scaffold is complex, deferring to a unit-test on `Report::build_from_results` (Task 4) is acceptable — the integration test is the cherry-on-top, not the load-bearing coverage.)

- [ ] **Step 5: Run full test suite**

Run: `cargo test`
Expected: PASS — all 2144+ tests including the new ones.

- [ ] **Step 6: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add src/eval/cli.rs src/main.rs
git commit -m "$(cat <<'EOF'
feat(eval): steve eval report subcommand + exit codes

`steve eval report <results.yaml> [--baselines-dir] [--scenarios-dir]
[--judge-model] [--regression-threshold] [--record-history]
[--html PATH] [--verbose]` ships end-to-end:

- Loads ResultsFile from disk.
- Resolves per-scenario baselines from --baselines-dir
  (default eval/baselines/); per-scenario [scoring].axes from
  --scenarios-dir (default eval/scenarios/).
- Calls Judge::compare on each (scenario, run) pair via
  Report::build_from_results.
- Renders layered text to stdout; --verbose adds per-scenario
  section.
- --html PATH writes a self-contained HTML file with Chart.js
  bundled + trend chart from history.jsonl (if non-empty).
- --record-history appends a row to eval/history.jsonl. Bare
  report is read-only against the file.

Exit codes per spec:
- 0 (Pass): net win rate >= threshold
- 1 (Regression): net win rate < threshold
- 2 (InfraError): any anyhow::Error from the subcommand

Threshold precedence: --regression-threshold flag > config
eval.regression_threshold > 0.0 default.

Refs: steve-u896
EOF
)"
```

---

## Task 10: `steve eval` (no subcommand) chains run → report

Per spec: `steve eval` without a subcommand should chain `run` → `report` against the configured baseline.

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Update the no-subcommand dispatch**

Currently `steve eval` without a scenario path errors with "supply a scenario path or use a sub-subcommand". Replace that path with a run → report chain.

Find the dispatch in main.rs and replace:

```rust
        // No subcommand. Run → report chain.
        None => {
            // Use scenario filter (optional) and model (required) from args.
            let scenario_filter = args.scenario_filter.as_deref();
            let model = &args.model;
            let scenarios_dir = project_root.join("eval").join("scenarios");
            let baselines_dir = project_root.join("eval").join("baselines");
            let history_path = project_root.join("eval").join("history.jsonl");

            // 1. Run: produce a temp results file.
            let results_dir = project_root.join("eval").join("results");
            std::fs::create_dir_all(&results_dir)?;
            let now = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
            let results_path = results_dir.join(format!("chained-{now}.yaml"));
            eval::cli::run_subcommand(
                &scenarios_dir,
                scenario_filter,
                model,
                &results_path,
            )
            .await?;

            // 2. Report: against the configured baseline.
            let config = config::load(&project_root)?.0;
            let eval_config = config::load_eval_config(&project_root)?;
            let config_threshold = eval_config.regression_threshold;
            // Resolve default judge model: --judge-model > eval.default_judge_model
            let judge_model_resolved = args
                .judge_model
                .clone()
                .or(eval_config.default_judge_model.clone());
            let registry = build_provider_registry(&config)?;
            let exit = eval::cli::report_subcommand(
                &results_path,
                &baselines_dir,
                &scenarios_dir,
                judge_model_resolved.as_deref(),
                args.regression_threshold,
                config_threshold,
                args.verbose,
                args.record_history,
                args.html.as_deref(),
                &history_path,
                &registry,
            )
            .await?;
            std::process::exit(exit.as_i32());
        }
```

The `Args` struct for `Eval` needs to gain the new optional fields (`scenario_filter`, `verbose`, `record_history`, `html`, `regression_threshold`). Match the field list from the `Report` subcommand for consistency.

- [ ] **Step 2: Retire the Phase-5 positional path**

Phase-5's `steve eval <scenario.toml>` single-shot path was preserved through Phase 6/7 as a transitional dev loop. Phase 8 retires it per spec. Remove:
- The positional `scenario: Option<PathBuf>` arg on `EvalArgs`
- The `run_one` invocation in the no-subcommand path (if `scenario.is_some()`)
- The `run_one` function in `cli.rs` (if no other callers)

Add a deprecation notice for users who still try the old syntax:

```rust
// In Eval dispatch:
if args.legacy_scenario_path.is_some() {
    anyhow::bail!(
        "the positional `steve eval <scenario.toml>` form was retired in Phase 8. \
         Use `steve eval run --scenario <name> --model <provider/model>` or \
         `steve eval` (no subcommand) to chain run → report."
    );
}
```

Or, simpler: just remove the positional arg entirely. Users see clap's usage error.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test`
Expected: PASS — adapt or remove any test that depends on the Phase-5 single-shot path (`run_one`); they're being retired here.

- [ ] **Step 4: Smoke test (manual, optional)**

```bash
# Should work end-to-end against _smoke once a baseline is frozen
cargo run -- eval baseline freeze --scenario _smoke --model fuel-ix/claude-haiku-4-5
cargo run -- eval --scenario _smoke --model fuel-ix/claude-haiku-4-5
```

Expected output: scenario runs, then report's layered text appears, then exits 0 (or 1 if regression).

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/eval/cli.rs
git commit -m "$(cat <<'EOF'
feat(eval): steve eval (no subcommand) chains run → report

Phase 5's positional `steve eval <scenario.toml>` form is retired
per spec. `steve eval` now defaults to: run scenarios → produce a
temp results.yaml → report against the configured baseline → exit
with the report's exit code.

This makes the common case one command:

  $ steve eval --scenario _smoke --model X
  running scenario _smoke (1/1) [axes: ...]
    run 1/3... done in 14s
    run 2/3... done in 13s
    run 3/3... done in 15s
  wrote results to eval/results/chained-20260512-153021.yaml
  Eval results — current (X at <ref>) vs baseline
    Headline:        +2.3% net win rate (97.7% non-regression)
    Per axis:
      correctness:   +0.0% net win rate ...

The same flags as `eval report` apply: --judge-model, --html,
--record-history, --regression-threshold, --verbose.

run_one was the entry point for the Phase 5 single-shot path; it's
removed in this commit. Any caller still using it should switch to
the chain (or `eval run` if they want just the run half).

Refs: steve-u896
EOF
)"
```

---

## Task 11: Walking test + Phase-5 retirement cleanup

Phase 5's single-shot path is gone. Update any tests or docs that reference it.

**Files:**
- Modify: tests that reference `run_one`
- Modify: README.md or other top-level docs if they reference Phase-5 syntax
- (eval/scenarios/*.toml: no changes — scenarios are unchanged)

- [ ] **Step 1: Find all references**

```bash
grep -rn "run_one\|steve eval <\|steve eval test.toml" --include="*.rs" --include="*.md" .
```

- [ ] **Step 2: Update or remove each reference**

For test code: switch to `run_subcommand` if the test exercised the run loop, or delete if it was specifically testing the Phase-5 JSON output shape.

For docs/README: update example commands to use the new shape (`steve eval run` or chained `steve eval`).

- [ ] **Step 3: Run full test suite + clippy + fmt**

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo +nightly fmt
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add <updated files>
git commit -m "$(cat <<'EOF'
chore(eval): retire Phase-5 single-shot path; update tests + docs

run_one was the entry point for Phase 5's `steve eval <scenario.toml>`
positional form (pretty-JSON to stdout). Phase 8's `steve eval`
chained run → report replaces it. Per spec: "Phase-5's single-run
pretty-JSON path is retired."

This commit removes run_one + any tests that specifically pinned
its JSON output shape. Tests that exercised the Phase 5 run loop
either migrate to run_subcommand or are dropped as redundant
with Phase 6's run-loop coverage.

Refs: steve-u896
EOF
)"
```

---

## Task 12: Final verification + integration smoke + push

End-to-end verification across the test suite, clippy, formatter, and a manual smoke run.

- [ ] **Step 1: Full test suite**

```bash
cargo test
```

Expected: ALL PASS — should be ~2160+ tests now (2144 pre-Phase-8 + ~20 new Phase 8 tests).

- [ ] **Step 2: Clippy**

```bash
cargo clippy --all-targets -- -D warnings
```

Expected: clean.

- [ ] **Step 3: Nightly format**

```bash
cargo +nightly fmt --check
```

Expected: clean.

- [ ] **Step 4: Manual smoke against a real scenario**

```bash
# Baseline a real scenario first
cargo run -- eval baseline freeze --scenario _smoke --model fuel-ix/claude-haiku-4-5

# Chained run → report
cargo run -- eval --scenario _smoke --model fuel-ix/claude-haiku-4-5

# Expected: scenario runs, report prints headline, exit code 0
# (assuming the baseline was just frozen and the current run is
#  essentially identical → all ties)

# HTML report
cargo run -- eval --scenario _smoke --model fuel-ix/claude-haiku-4-5 --html /tmp/report.html
open /tmp/report.html  # macOS: opens in default browser

# History append
cargo run -- eval --scenario _smoke --model fuel-ix/claude-haiku-4-5 --record-history
cat eval/history.jsonl  # one row
```

- [ ] **Step 5: Update beads + push**

```bash
bd update steve-u896 --notes "Phase 8 done: report subcommand + chained eval + HTML + history.jsonl + exit codes. All 4 ships-when criteria met. Eval epic (steve-ffdq) is now unblocked for final merge to main."
bd close steve-u896
git pull --rebase
bd dolt push
git push
```

- [ ] **Step 6: Open PR**

```bash
gh pr create --base feat/eval-harness --head feat/eval-phase-8-reporting-cli-split \
  --title "feat(eval): Phase 8 — reporting + CLI run/report split (steve-u896)" \
  --body "$(cat <<'EOF'
## Summary

Phase 8 of the eval-harness pivot. Builds on Phase 7's `Judge::compare`
to deliver the final mile: a `steve eval report` subcommand that
aggregates paired-comparison verdicts into a layered text report,
plus chained `steve eval` (no subcommand), HTML report with bundled
Chart.js, and exit codes that gate CI.

- `steve eval report <results.yaml>` subcommand with auto-resolution
  of baselines from `--baselines-dir`.
- Layered text output: headline net win rate `(W-L)/(W+L+T)` +
  per-axis breakdown + per-scenario detail (`--verbose`).
- Non-regression rate `(W+T)/(W+L+T)` beside headline as a confidence
  sanity check.
- Exit codes: 0 / 1 / 2 (pass / regression / infra-error). Threshold
  configurable via `--regression-threshold` CLI flag > `eval.regression_threshold`
  in `.steve.jsonc` > default 0.0.
- `eval/history.jsonl` append-on-flag (`--record-history`).
- `--html PATH` writes a self-contained HTML file with Chart.js
  v4.4.7 bundled inline (~80KB), MIT license adjacent, trend chart
  from history.jsonl, ALL dynamic content XSS-escaped.
- `steve eval` (no subcommand) chains run → report. Phase 5's
  positional single-shot path retires.

**Ships-when criteria:** all four spec ships-when criteria met:
- ✅ `steve eval` produces layered text output end-to-end
- ✅ `--html` produces viewable self-contained HTML
- ✅ Backtest works (re-running report against an archived results.yaml
  reproduces the same headline modulo judge variance)
- ✅ CI can gate on exit code and commit history.jsonl on main

**Stats:** ~2160+ tests pass, clippy clean with `-D warnings`, fmt clean.

**Spec:** [docs/superpowers/specs/2026-05-06-eval-harness-paired-comparison-pivot.md](../blob/feat/eval-harness/docs/superpowers/specs/2026-05-06-eval-harness-paired-comparison-pivot.md)
— particularly the "Reporting" and "CLI surface" sections.

**Plan:** [docs/superpowers/plans/2026-05-12-eval-phase-8-reporting-cli-split.md](../blob/feat/eval-phase-8-reporting-cli-split/docs/superpowers/plans/2026-05-12-eval-phase-8-reporting-cli-split.md)

Closes: steve-u896

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Verification

End-to-end smoke (manual, post-merge):

```bash
git switch feat/eval-harness && git pull

# 1. Confirm the whole test suite still passes.
cargo test

# 2. Confirm clippy/fmt clean.
cargo clippy --all-targets -- -D warnings
cargo +nightly fmt --check

# 3. Freeze a baseline + chain a report against a real scenario.
cargo run -- eval baseline freeze --scenario _smoke --model fuel-ix/claude-haiku-4-5
cargo run -- eval --scenario _smoke --model fuel-ix/claude-haiku-4-5

# Expected: layered text report appears, exit code 0 (or 1 if regression).

# 4. HTML render smoke.
cargo run -- eval --scenario _smoke --model fuel-ix/claude-haiku-4-5 --html /tmp/report.html
# Inspect /tmp/report.html in a browser — verify trends section is
# absent (history.jsonl is empty on a fresh checkout).

# 5. History round-trip.
cargo run -- eval --scenario _smoke --model fuel-ix/claude-haiku-4-5 --record-history
cat eval/history.jsonl  # one row
cargo run -- eval --scenario _smoke --model fuel-ix/claude-haiku-4-5 --record-history --html /tmp/report.html
# Open /tmp/report.html — trends section should now show 1 point.
```

---

## Self-Review Checklist (run before opening PR)

- [ ] Spec coverage: every Phase 8 ships-when criterion (spec lines 991–1001) maps to a passing test or smoke output? text output → Task 5; HTML report → Task 8; backtest → Task 9 (build_from_results is pure on results+baselines); CI exit code → Task 9 (ReportExitCode).
- [ ] Spec coverage: net win rate + non-regression rate formulas pinned with the spec's exact values? Yes — Task 3 tests.
- [ ] Spec coverage: judge retry-once? Yes — Task 4 tests (`build_from_results_retries_once_on_transient_judge_error` + `build_from_results_marks_errored_when_both_attempts_fail`).
- [ ] Spec coverage: all-runs-errored → Skipped? Yes — Task 4.
- [ ] Spec coverage: missing baseline → fail-loud with copy-pasteable command? Yes — Task 4's skip reason includes the exact `eval baseline freeze` command.
- [ ] Spec coverage: HTML XSS hardening pinned? Yes — Task 6 (escape helper) + Task 8 (full <script> + attribute-breakout payloads in render tests).
- [ ] Spec coverage: Chart.js MIT license adjacent? Yes — `include_str!` from `assets/chartjs-LICENSE.txt`, embedded in HTML comment block.
- [ ] No placeholders: every "TODO", "TBD" replaced? Yes — only one TODO remains: deterministic_floor tallies (filed as bd follow-up in Task 7).
- [ ] CLAUDE.md adherence: exhaustive matching on `Verdict` enum (Task 3's `ReportTotals::add`) and `ScenarioOutcome` (Task 5's render_text)? Yes — both use exhaustive variant lists.
- [ ] CLAUDE.md adherence: `#[allow(clippy::too_many_arguments)]` on `report_subcommand` has a justifying comment? Need to add — the function has 11 args. Either bundle into a struct OR document why flat args are clearer (see Phase 7's `Judge::compare` discussion for the pattern). Decision: bundle the threshold + flags into a `ReportFlags` struct to keep the signature under threshold; the orchestration args (results_path, baselines_dir, scenarios_dir, judge_model, registry) stay flat.
- [ ] CLAUDE.md adherence: every new type has unit tests? Yes — `ReportTotals`, `AxisTotals`, `ScenarioReport`, `ScenarioOutcome`, `Report`, `HistoryEntry` all tested.
- [ ] CLAUDE.md adherence: walking test still passes for committed scenarios? Yes — Phase 8 adds no scenario changes.
- [ ] Manual smoke tested: HTML report opens in browser without console errors? Run Task 12 step 4 before merging.
