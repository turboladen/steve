//! `steve eval` subcommand entry points.

use std::{collections::BTreeMap, io::Write, path::Path};

use anyhow::{Context, Result};

use crate::eval::{
    Runner, Scenario,
    baseline::{BaselineFile, Manifest, ManifestEntry, baseline_path, manifest_path},
    evaluate,
    results::{ResultsFile, ScenarioResults},
    scenario::discover_scenarios,
    transcript::Normalizer,
};

/// Write `s` to stdout, treating a closed pipe (`head`, `less q`) as
/// success. The default `print!` panics with exit 101 on EPIPE, which
/// would break the documented `Pass=0`/`Regression=1`/`NoData=2`
/// contract for callers piping output. Non-EPIPE I/O errors (disk
/// full on a redirect, write-zero) propagate to the caller so the
/// process can exit 2 instead of returning success with truncated
/// output.
fn write_stdout_lossy(s: &str) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    match lock.write_all(s.as_bytes()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e),
    }
}

fn writeln_stdout_lossy(s: &str) -> std::io::Result<()> {
    write_stdout_lossy(s)?;
    write_stdout_lossy("\n")
}

/// Flush stdout, treating EPIPE as success. Used after a partial-line
/// progress message (`running scenario X...`) so the prefix renders
/// before the work runs to completion. Other I/O errors propagate.
fn flush_stdout_lossy() -> std::io::Result<()> {
    match std::io::Write::flush(&mut std::io::stdout()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e),
    }
}

/// Format the per-scenario header line printed at the start of each
/// scenario's runs. Surfaces the resolved scoring axes (and whether
/// they came from a `[scoring]` block or `DEFAULT_AXES`) so the
/// override is observable from the `eval run` output, not just unit
/// tests. `(override)` is tagged only when overridden — the default
/// case is the expected baseline, so leaving it untagged keeps the
/// load-bearing signal load-bearing.
fn format_scenario_header(name: &str, index: usize, total: usize, scenario: &Scenario) -> String {
    let axes_str = scenario
        .scoring_axes()
        .iter()
        .map(|a| format!("{a}"))
        .collect::<Vec<_>>()
        .join(", ");
    let axes_label = if scenario.scoring.is_some() {
        "axes (override)"
    } else {
        "axes"
    };
    format!(
        "running scenario {name} ({}/{}) [{axes_label}: {axes_str}]...",
        index + 1,
        total,
    )
}

/// `steve eval run` — runs scenarios K times each (K from `scenario.runs`),
/// writes a normalized `ResultsFile` YAML. No judging.
///
/// `scenario_filter` is the `--scenario` value (a name like "_smoke", not
/// a path). When `None`, every scenario under `scenarios_dir` is run.
/// `out_path` is where to write the YAML.
pub async fn run_subcommand(
    scenarios_dir: &Path,
    scenario_filter: Option<&str>,
    model: &str,
    out_path: &Path,
) -> Result<()> {
    let discovered = discover_scenarios(scenarios_dir)?;
    let selected: Vec<(String, std::path::PathBuf)> = match scenario_filter {
        Some(name) => discovered.into_iter().filter(|(n, _)| n == name).collect(),
        None => discovered,
    };
    if selected.is_empty() {
        match scenario_filter {
            Some(name) => anyhow::bail!(
                "no scenario named {name:?} found under {}",
                scenarios_dir.display()
            ),
            None => anyhow::bail!(
                "no scenarios found under {} (does the directory contain <name>/scenario.toml files?)",
                scenarios_dir.display()
            ),
        }
    }

    let git_ref = current_git_ref().unwrap_or_else(|| {
        eprintln!(
            "warning: could not determine git ref (not a git repo, or `git` not in PATH); \
             output will be tagged git_ref=\"unknown\""
        );
        "unknown".to_string()
    });
    let recorded_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let mut scenarios_out: BTreeMap<String, ScenarioResults> = BTreeMap::new();
    let total = selected.len();

    for (i, (name, scenario_path)) in selected.iter().enumerate() {
        let scenario = Scenario::from_file(scenario_path)
            .with_context(|| format!("loading scenario {}", scenario_path.display()))?;
        let scenario_dir = scenario_path
            .parent()
            .with_context(|| format!("scenario path has no parent: {}", scenario_path.display()))?;
        writeln_stdout_lossy(&format_scenario_header(name, i, total, &scenario))?;

        let runs = scenario.runs.get();
        let mut transcripts = Vec::with_capacity(runs);
        for run_idx in 0..runs {
            let started = std::time::Instant::now();
            write_stdout_lossy(&format!("  run {}/{}...", run_idx + 1, runs))?;
            flush_stdout_lossy()?;
            // Fresh Runner per run -> fresh tempdir workspace. Without this,
            // `setup.shell` mutations from a prior run would persist into
            // the next run's working state. Each run is a clean sample.
            let mut runner = Runner::build(&scenario, scenario_dir, model)
                .with_context(|| format!("building runner for {name} run #{}", run_idx + 1))?;
            let captured = runner
                .run(&scenario)
                .await
                .with_context(|| format!("running scenario {name} run #{}", run_idx + 1))?;
            // Deterministic-floor verdict: report.passed() AND the run
            // completed normally. Either rule-failures or timeout/abort
            // counts as "floor failed".
            let report = evaluate(&scenario, &captured);
            let floor_passed = report.passed() && captured.completed_normally();
            transcripts.push(Normalizer::normalize(&captured, floor_passed));
            writeln_stdout_lossy(&format!(" done in {:.1}s", started.elapsed().as_secs_f32()))?;
        }

        scenarios_out.insert(
            name.clone(),
            ScenarioResults {
                user_turns: scenario.user_turns.clone(),
                runs: transcripts,
            },
        );
    }

    let results = ResultsFile {
        git_ref,
        recorded_at,
        model: model.to_string(),
        scenarios: scenarios_out,
    };
    results.write_to_path(out_path)?;
    writeln_stdout_lossy(&format!("wrote results to {}", out_path.display()))?;
    Ok(())
}

/// Exit codes for `steve eval report`. Mapped to process exit by
/// main.rs: `Pass=0`, `Regression=1`, `NoData=2`. (Generic `Err`
/// paths from `report_subcommand` also map to exit 2 via main's outer
/// `if let Err` handler.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportExitCode {
    Pass,
    Regression,
    /// Every scenario was Skipped (no baselines, all-errored, etc).
    /// The `net_win_rate` formula returns its `0.0` sentinel when
    /// totals are empty — without this variant a green CI gate
    /// would hide "no scenarios graded" from the operator.
    NoData,
}

impl ReportExitCode {
    pub fn as_i32(&self) -> i32 {
        match self {
            ReportExitCode::Pass => 0,
            ReportExitCode::Regression => 1,
            ReportExitCode::NoData => 2,
        }
    }
}

/// Determine the exit code from a final `net_win_rate`, the CLI
/// `--regression-threshold` (highest), and the config
/// `eval.regression_threshold` (next). Falls back to `0.0`.
/// Returns `Pass` for ≥ threshold, `Regression` for < threshold.
/// Returns `Err` for malformed input (non-finite threshold) so the
/// top-level main.rs handler maps it to exit 2 with a clear message —
/// reusing `NoData` (which means "every scenario was Skipped") for
/// malformed input would conflate two distinct failure modes for
/// future readers.
/// Caller is responsible for short-circuiting to `NoData` when
/// no scenarios were graded.
pub fn resolve_exit_code(
    net_win_rate: f64,
    cli_threshold: Option<f64>,
    config_threshold: Option<f64>,
) -> Result<ReportExitCode> {
    let threshold = cli_threshold.or(config_threshold).unwrap_or(0.0);
    // A non-finite threshold would make `net_win_rate < threshold`
    // always false (NaN compares as false everywhere) and silently
    // exit Pass on a regression.
    if !threshold.is_finite() {
        anyhow::bail!(
            "regression threshold {threshold} is not a finite number; \
             aborting (would silently mask regressions via NaN comparison)"
        );
    }
    Ok(if net_win_rate < threshold {
        ReportExitCode::Regression
    } else {
        ReportExitCode::Pass
    })
}

/// Determine the final exit code from a populated `Report` plus the
/// threshold sources. Wraps `resolve_exit_code` with the NoData
/// short-circuit: when nothing was graded (every scenario Skipped),
/// the `net_win_rate` formula returns its `0.0` sentinel — without
/// this short-circuit a CI gate would silently exit 0 on a config
/// that produces zero graded scenarios.
pub fn report_exit_code(
    report: &crate::eval::report::Report,
    cli_threshold: Option<f64>,
    config_threshold: Option<f64>,
) -> Result<ReportExitCode> {
    if report.headline_totals.total() == 0 {
        return Ok(ReportExitCode::NoData);
    }
    resolve_exit_code(
        report.headline_totals.net_win_rate(),
        cli_threshold,
        config_threshold,
    )
}

/// Bundled inputs to `report_subcommand`. Grouped because the
/// subcommand takes 9+ arguments — flat args would trip clippy's
/// too-many-arguments lint and obscure intent. The three groups
/// are: paths, judge-model resolution, report flags.
pub struct ReportArgs<'a> {
    pub results_path: &'a Path,
    pub baselines_dir: &'a Path,
    pub scenarios_dir: &'a Path,
    pub history_path: &'a Path,
    pub html_out: Option<&'a Path>,
    /// `--judge-model` CLI override (highest precedence). When set,
    /// overrides BOTH `scenario.judge_model` and the eval-config
    /// `default_judge_model`. The orchestrator passes this directly
    /// into `Judge::from_registry` as the uniform model.
    pub cli_judge_model: Option<&'a str>,
    /// `eval.default_judge_model` from `.steve.eval.jsonc`. Applied
    /// as a fallback ONLY when both `cli_judge_model` and the per-
    /// scenario `judge_model` are unset — so a per-scenario override
    /// always wins over the config default. MUST stay separate from
    /// `cli_judge_model`: collapsing them into one field would make
    /// the config default beat per-scenario overrides (since the
    /// judge's `cli_model` slot has highest precedence).
    pub config_default_judge_model: Option<&'a str>,
    /// CLI `--regression-threshold` (highest precedence).
    pub cli_regression_threshold: Option<f64>,
    /// `eval.regression_threshold` from `.steve.eval.jsonc`.
    pub config_regression_threshold: Option<f64>,
    pub verbose: bool,
    pub record_history: bool,
    pub registry: &'a crate::provider::ProviderRegistry,
}

/// `steve eval report <results.yaml>` — load a results file, resolve
/// per-scenario baselines from `baselines_dir`, judge each
/// (scenario, run) pair via `Judge::compare`, render the layered
/// text report to stdout, optionally write HTML and/or append a
/// history row.
///
/// Returns a `ReportExitCode` per spec (Pass / Regression). The caller
/// in main.rs translates this to `std::process::exit(code.as_i32())`.
/// `anyhow::Error` returns map to exit code 2 (InfraError) in main.
pub async fn report_subcommand(args: ReportArgs<'_>) -> Result<ReportExitCode> {
    use crate::eval::{
        Judge,
        history::{HistoryEntry, append_history, read_history},
        html_report::render_html,
        report::Report,
        results::ResultsFile,
    };

    let results = ResultsFile::read_from_path(args.results_path)
        .with_context(|| format!("loading results from {}", args.results_path.display()))?;

    // Precedence: CLI > scenario.judge_model > config default. The
    // Judge holds ONLY the CLI override; the config default and
    // per-scenario judge_model are threaded into build_from_results
    // for per-cell resolution. This preserves the "scenario beats
    // config default" semantic that .steve.eval.jsonc docs promise.
    let judge = Judge::from_registry(args.registry, args.cli_judge_model);

    // Report.judge_model records what was UNIFORMLY applied across
    // every scenario for history.jsonl trend grouping. With CLI set,
    // every scenario uses it. Without CLI, each scenario can pick
    // its own — so the field is None and the renderer surfaces
    // "per-scenario" prose. Config default isn't surfaced as the
    // "uniform" judge because it can be overridden per-scenario.
    let report_judge_model = args.cli_judge_model;
    let report = Report::build_from_results(
        &results,
        args.baselines_dir,
        &args.results_path.display().to_string(),
        &judge,
        report_judge_model,
        Some(args.scenarios_dir),
        args.config_default_judge_model,
    )
    .await?;

    // Side effects FIRST — HTML and history are explicit user-requested
    // outputs that must not be hostage to stdout draining.
    //
    // History append is gated on `headline_totals.total() > 0`:
    // recording a row when every scenario was Skipped would pollute
    // history.jsonl with a meaningless `net_win_rate=0.0` point
    // (the sentinel value `ReportTotals::net_win_rate` returns for
    // empty totals). Downstream trend grouping and the HTML chart
    // would treat that as a real data point. Especially dangerous
    // for "all scenarios drifted from baseline" runs which would
    // otherwise silently log as a flat 0.0 trend.
    let nothing_graded = report.headline_totals.total() == 0;
    let appended_history = args.record_history && !nothing_graded;
    if appended_history {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let entry = HistoryEntry::from_report(&report, now);
        append_history(args.history_path, &entry)?;
    } else if args.record_history && nothing_graded {
        eprintln!(
            "warning: skipping history append — no scenarios graded \
             (would record a meaningless net_win_rate=0.0 trend point)"
        );
    }

    if let Some(html_path) = args.html_out {
        let history = read_history(args.history_path).with_context(|| {
            // If append_history just succeeded, the operator now
            // has an orphaned line on disk that won't be reflected in
            // any HTML report from this invocation. Tell them.
            let prefix = if appended_history {
                format!(
                    "a new history row was already appended to {}; \
                     you may want to remove the last line manually. ",
                    args.history_path.display()
                )
            } else {
                String::new()
            };
            format!(
                "{prefix}reading history from {}",
                args.history_path.display()
            )
        })?;
        let html = render_html(&report, &history);
        std::fs::write(html_path, html)
            .with_context(|| format!("writing HTML to {}", html_path.display()))?;
    }

    // Stdout writes go through *_stdout_lossy so EPIPE from `head`/`less`
    // downgrades to a clean exit (preserving the Pass/Regression/NoData
    // contract). Non-EPIPE errors propagate to main as exit 2.
    write_stdout_lossy(&report.render_text(args.verbose))?;
    if appended_history {
        writeln_stdout_lossy(&format!(
            "appended history row to {}",
            args.history_path.display()
        ))?;
    }
    if let Some(html_path) = args.html_out {
        writeln_stdout_lossy(&format!("wrote HTML report to {}", html_path.display()))?;
    }

    // Exit code: NoData (2) when nothing was graded, otherwise the
    // threshold comparison. The helper `report_exit_code` encodes
    // both branches so the predicate stays testable in isolation.
    let exit = report_exit_code(
        &report,
        args.cli_regression_threshold,
        args.config_regression_threshold,
    )?;
    if exit == ReportExitCode::NoData {
        eprintln!(
            "warning: no scenarios were graded (all skipped or errored); exiting with code 2"
        );
    }
    Ok(exit)
}

/// Best-effort current git ref (short hash). Returns `None` outside a git
/// repo or if `git` is missing — callers fall back to `"unknown"` rather
/// than failing the whole eval. The build script's STEVE_GIT_REV is at
/// build time; this is the runtime ref of the workspace at run time, so
/// shelling out is the correct approach.
fn current_git_ref() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// `steve eval baseline freeze` — captures one fresh transcript per
/// (scenario, model) and writes it to
/// `eval/baselines/<scenario>/<provider>/<model>.yaml`, plus a manifest
/// entry.
///
/// **K=1 regardless of `scenario.runs`.** Per spec: the baseline is the
/// fixed reference; the current side runs K samples and aggregates.
/// Doing N runs at freeze time would require defining "best run," which
/// requires a judge — circular, since the judge is what we're trying to
/// use the baseline to enable.
///
/// Filters compose: `(scenario_filter, model)` together select what to
/// freeze. `scenario_filter = None` runs every scenario.
pub async fn freeze_subcommand(
    scenarios_dir: &Path,
    baselines_dir: &Path,
    scenario_filter: Option<&str>,
    model: &str,
) -> Result<()> {
    let discovered = discover_scenarios(scenarios_dir)?;
    let selected: Vec<(String, std::path::PathBuf)> = match scenario_filter {
        Some(name) => discovered.into_iter().filter(|(n, _)| n == name).collect(),
        None => discovered,
    };
    if selected.is_empty() {
        match scenario_filter {
            Some(name) => anyhow::bail!(
                "no scenario named {name:?} found under {}",
                scenarios_dir.display()
            ),
            None => anyhow::bail!(
                "no scenarios found under {} (does the directory contain <name>/scenario.toml files?)",
                scenarios_dir.display()
            ),
        }
    }

    let mfst_path = manifest_path(baselines_dir);

    // Pre-flight: validate the model string maps to safe paths AND the
    // existing manifest (if any) is readable, BEFORE burning agent tokens.
    // Without this, a typo in --model or a corrupt manifest.toml would
    // surface only after every scenario ran. baseline_path validates the
    // model string structure; Manifest::read_from_path returns
    // Manifest::default() on NotFound (fresh-checkout case) and propagates
    // parse / permission errors otherwise.
    let resolved_paths: Vec<std::path::PathBuf> = selected
        .iter()
        .map(|(name, _)| baseline_path(baselines_dir, name, model))
        .collect::<Result<_>>()?;
    let mut manifest = Manifest::read_from_path(&mfst_path)?;

    let git_ref = current_git_ref().unwrap_or_else(|| {
        eprintln!(
            "warning: could not determine git ref (not a git repo, or `git` not in PATH); \
             output will be tagged git_ref=\"unknown\""
        );
        "unknown".to_string()
    });
    let frozen_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // Collect: run every scenario and accumulate results in memory. No disk writes
    // happen here. If any scenario fails the error propagates immediately,
    // leaving the baselines/ tree exactly as it was before this call.
    let mut pending: Vec<(std::path::PathBuf, BaselineFile, ManifestEntry)> =
        Vec::with_capacity(selected.len());
    let total = selected.len();
    for (i, ((name, scenario_path), path)) in selected.iter().zip(resolved_paths).enumerate() {
        let started = std::time::Instant::now();
        write_stdout_lossy(&format!("running scenario {name} ({}/{})...", i + 1, total))?;
        flush_stdout_lossy()?;
        let scenario = Scenario::from_file(scenario_path)
            .with_context(|| format!("loading scenario {}", scenario_path.display()))?;
        let scenario_dir = scenario_path
            .parent()
            .with_context(|| format!("scenario path has no parent: {}", scenario_path.display()))?;

        let mut runner = Runner::build(&scenario, scenario_dir, model)
            .with_context(|| format!("building runner for {name}"))?;
        let captured = runner
            .run(&scenario)
            .await
            .with_context(|| format!("running scenario {name} for freeze"))?;
        let report = evaluate(&scenario, &captured);
        let floor_passed = report.passed() && captured.completed_normally();
        let transcript = Normalizer::normalize(&captured, floor_passed);
        writeln_stdout_lossy(&format!(" done in {:.1}s", started.elapsed().as_secs_f32()))?;

        let baseline = BaselineFile {
            scenario: name.clone(),
            model: model.to_string(),
            git_ref: git_ref.clone(),
            frozen_at: frozen_at.clone(),
            user_turns: scenario.user_turns.clone(),
            transcript,
        };
        let entry = ManifestEntry {
            scenario: name.clone(),
            model: model.to_string(),
            git_ref: git_ref.clone(),
            frozen_at: frozen_at.clone(),
        };
        pending.push((path, baseline, entry));
    }

    // Write: all runs succeeded — commit to disk.
    let pending_count = pending.len();
    for (idx, (path, baseline, _)) in pending.iter().enumerate() {
        baseline.write_to_path(path).with_context(|| {
            format!(
                "writing baseline for {} ({} earlier baseline(s) in this run already on disk; \
                 re-run freeze to restore consistent state)",
                baseline.scenario, idx
            )
        })?;
        writeln_stdout_lossy(&format!(
            "froze {} -> {}",
            baseline.scenario,
            path.display()
        ))?;
    }
    for (_, _, entry) in pending {
        manifest.upsert(entry);
    }
    // Surface the baseline-vs-manifest skew explicitly: if this write
    // fails after every baseline succeeded, the on-disk YAMLs are new
    // but the manifest still reports the previous git_ref/frozen_at.
    // History rows written by future `report --record-history` would
    // cite the wrong baseline anchor.
    manifest.write_to_path(&mfst_path).with_context(|| {
        format!(
            "writing manifest TOML to {} after {pending_count} baseline(s) already \
             on disk; baselines and manifest are now out of sync — \
             re-run freeze to update the manifest",
            mfst_path.display()
        )
    })?;
    writeln_stdout_lossy(&format!("updated manifest: {}", mfst_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod header_tests {
    use super::*;
    use crate::eval::{
        scenario::{Scenario, Scoring},
        score::Axis,
    };

    fn scenario_with_optional_scoring(scoring: Option<Scoring>) -> Scenario {
        Scenario {
            name: "x".into(),
            description: "x".into(),
            runs: std::num::NonZeroUsize::new(1).unwrap(),
            setup: Default::default(),
            user_turns: vec!["go".into()],
            expectations: vec![crate::eval::scenario::Expectation::ToolCalled {
                tool: crate::tool::ToolName::Read,
            }],
            judge_model: None,
            scoring,
        }
    }

    #[test]
    fn header_shows_default_axes_unannotated_when_no_scoring_block() {
        let scenario = scenario_with_optional_scoring(None);
        let line = format_scenario_header("x", 0, 3, &scenario);
        assert!(line.contains("(1/3)"), "got: {line}");
        assert!(
            line.contains("[axes: correctness, efficiency, conciseness]"),
            "got: {line}"
        );
        assert!(
            !line.contains("override"),
            "default axes must NOT carry an annotation; got: {line}"
        );
    }

    #[test]
    fn header_annotates_override_axes_when_scoring_block_present() {
        // The `(override)` tag is the load-bearing signal — readers
        // expect defaults silently, so only the override case earns
        // a label. Without this tag the override is only observable
        // through unit tests, not the CLI smoke.
        let scenario = scenario_with_optional_scoring(Some(Scoring {
            axes: vec![Axis::Robustness, Axis::Efficiency],
        }));
        let line = format_scenario_header("stop-guessing", 2, 5, &scenario);
        assert!(line.contains("(3/5)"), "got: {line}");
        assert!(
            line.contains("[axes (override): robustness, efficiency]"),
            "got: {line}"
        );
        assert!(
            !line.contains("correctness"),
            "override must NOT print the default axes; got: {line}"
        );
    }

    // ── exit-code semantics ──
    //
    // The threshold-resolution logic is the entire CI-gating contract.
    // Pin each precedence step + the < boundary that distinguishes
    // Pass from Regression at the threshold value.

    #[test]
    fn resolve_exit_code_cli_wins_over_config_and_default() {
        // CLI=0.05, config=-0.10, net=0.0 → 0.0 < 0.05 → Regression
        assert_eq!(
            resolve_exit_code(0.0, Some(0.05), Some(-0.10)).unwrap(),
            ReportExitCode::Regression
        );
    }

    #[test]
    fn resolve_exit_code_config_used_when_cli_absent() {
        // CLI=None, config=-0.10, net=-0.05 → -0.05 ≥ -0.10 → Pass
        assert_eq!(
            resolve_exit_code(-0.05, None, Some(-0.10)).unwrap(),
            ReportExitCode::Pass
        );
    }

    #[test]
    fn resolve_exit_code_default_zero_used_when_both_absent() {
        // CLI=None, config=None, net=-0.001 → -0.001 < 0.0 → Regression
        assert_eq!(
            resolve_exit_code(-0.001, None, None).unwrap(),
            ReportExitCode::Regression
        );
    }

    #[test]
    fn resolve_exit_code_boundary_zero_is_pass() {
        // CLI=None, config=None, net=0.0 → 0.0 < 0.0 is false → Pass.
        // This is the load-bearing `<` vs `<=` distinction — flipping
        // it would silently fail every CI on an exactly-zero result.
        assert_eq!(
            resolve_exit_code(0.0, None, None).unwrap(),
            ReportExitCode::Pass
        );
    }

    #[test]
    fn report_exit_code_maps_to_process_codes() {
        // Pass=0, Regression=1, NoData=2.
        assert_eq!(ReportExitCode::Pass.as_i32(), 0);
        assert_eq!(ReportExitCode::Regression.as_i32(), 1);
        assert_eq!(ReportExitCode::NoData.as_i32(), 2);
    }

    // ── report_exit_code: the NoData short-circuit ──

    use crate::eval::report::{
        BaselineProvenance, Report, ReportTotals, ScenarioOutcome, ScenarioReport,
    };
    use std::collections::BTreeMap;

    fn empty_report() -> Report {
        Report {
            model: "test/model".into(),
            results_git_ref: "x".into(),
            results_path: "x".into(),
            baseline_provenance: BTreeMap::new(),
            judge_model: None,
            headline_totals: ReportTotals::default(),
            per_axis: Vec::new(),
            scenarios: Vec::new(),
            deterministic_floor: Default::default(),
        }
    }

    #[test]
    fn resolve_exit_code_rejects_non_finite_threshold() {
        // `--regression-threshold NaN` parses cleanly through clap's
        // f64 parser. A NaN threshold makes `x < NaN` always false,
        // which would silently exit Pass on a real regression.
        // Return Err so the top-level main.rs handler maps to exit 2
        // with a clear diagnostic — reusing NoData (which means
        // "every scenario was Skipped") would conflate two distinct
        // failure modes.
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = resolve_exit_code(-0.99, Some(bad), None)
                .expect_err("non-finite CLI threshold must Err");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("not a finite number"),
                "expected `not a finite number` diagnostic; got: {msg}"
            );
        }
        // Config-source non-finite is equally bad.
        let err = resolve_exit_code(-0.99, None, Some(f64::NAN))
            .expect_err("non-finite config threshold must Err");
        assert!(format!("{err:#}").contains("not a finite number"));
    }

    #[test]
    fn report_exit_code_returns_no_data_when_totals_empty() {
        // Critical: this short-circuit is the only thing preventing
        // a green CI gate when every scenario is Skipped. The
        // net_win_rate sentinel collapses to 0.0 on empty totals,
        // which would otherwise read as Pass.
        let r = empty_report();
        assert_eq!(r.headline_totals.total(), 0);
        assert_eq!(
            report_exit_code(&r, None, None).unwrap(),
            ReportExitCode::NoData
        );
    }

    #[test]
    fn report_exit_code_no_data_overrides_permissive_threshold() {
        // Even with a CI threshold like -0.5 (allow 50% regression),
        // zero graded scenarios must still exit NoData. A permissive
        // threshold shouldn't mask the "no signal" case.
        let r = empty_report();
        assert_eq!(
            report_exit_code(&r, Some(-0.5), Some(-0.5)).unwrap(),
            ReportExitCode::NoData
        );
    }

    #[test]
    fn report_exit_code_no_data_overrides_when_skipped_scenarios_present() {
        // Scenarios present but all Skipped → totals still zero
        // → NoData.
        let mut r = empty_report();
        r.scenarios = vec![ScenarioReport {
            scenario: "X".into(),
            outcome: ScenarioOutcome::Skipped {
                reason: "no baseline".into(),
            },
        }];
        assert_eq!(
            report_exit_code(&r, None, None).unwrap(),
            ReportExitCode::NoData
        );
    }

    #[test]
    fn report_exit_code_passes_through_to_threshold_when_some_graded() {
        // Real verdicts present → defer to resolve_exit_code's threshold.
        let mut r = empty_report();
        r.headline_totals = ReportTotals {
            current_wins: 5,
            baseline_wins: 0,
            ties: 5,
        };
        r.baseline_provenance.insert(
            "X".into(),
            BaselineProvenance {
                git_ref: "abc".into(),
                frozen_at: "2026-05-12T00:00:00Z".into(),
            },
        );
        // net_win_rate = (5-0)/10 = 0.5; default threshold 0.0 → Pass.
        assert_eq!(
            report_exit_code(&r, None, None).unwrap(),
            ReportExitCode::Pass
        );
    }
}

#[cfg(test)]
mod integration_tests {
    use std::{collections::BTreeMap, path::PathBuf, time::Duration};

    use serde_json::json;

    use crate::{
        eval::{
            baseline::{BaselineFile, Manifest, ManifestEntry, baseline_path, manifest_path},
            capture::CapturedRun,
            results::{ResultsFile, ScenarioResults},
            transcript::{Normalizer, TranscriptEvent},
            workspace::WorkspaceSnapshot,
        },
        event::AppEvent,
        tool::{ToolName, ToolOutput},
    };

    fn fake_captured() -> CapturedRun {
        let mut cap = CapturedRun::new(
            PathBuf::from("/tmp/fake-eval"),
            WorkspaceSnapshot {
                files: BTreeMap::new(),
            },
        );
        cap.observe(&AppEvent::LlmDelta {
            text: "Reading.".into(),
        });
        cap.observe(&AppEvent::LlmToolCall {
            call_id: "uuid-1".into(),
            tool_name: ToolName::Read,
            arguments: json!({"path": "/tmp/fake-eval/foo.txt"}),
        });
        cap.observe(&AppEvent::ToolResult {
            call_id: "uuid-1".into(),
            tool_name: ToolName::Read,
            output: ToolOutput {
                title: "read".into(),
                output: "hello".into(),
                is_error: false,
            },
        });
        cap.observe(&AppEvent::LlmFinish { usage: None });
        cap.duration = Duration::from_millis(123);
        cap
    }

    /// End-to-end YAML pipeline: build a fake CapturedRun, normalize it,
    /// wrap in a BaselineFile, write to disk, read back. Verifies the
    /// freeze-side data path.
    #[test]
    fn freeze_pipeline_round_trip_via_disk() {
        let dir = tempfile::tempdir().unwrap();
        let baselines = dir.path().to_path_buf();

        let cap = fake_captured();
        let transcript = Normalizer::normalize(&cap, true);

        let baseline = BaselineFile {
            scenario: "_fake".into(),
            model: "ollama/qwen3-coder".into(),
            git_ref: "abc1234".into(),
            frozen_at: "2026-05-07T00:00:00Z".into(),
            user_turns: vec!["Read the file.".into()],
            transcript,
        };
        let path = baseline_path(&baselines, "_fake", "ollama/qwen3-coder").unwrap();
        baseline.write_to_path(&path).unwrap();
        assert!(path.exists());
        assert_eq!(
            path.strip_prefix(&baselines).unwrap(),
            std::path::Path::new("_fake/ollama/qwen3-coder.yaml"),
            "path layout must match the spec"
        );

        let mut manifest = Manifest::read_from_path(&manifest_path(&baselines)).unwrap();
        manifest.upsert(ManifestEntry {
            scenario: "_fake".into(),
            model: "ollama/qwen3-coder".into(),
            git_ref: "abc1234".into(),
            frozen_at: "2026-05-07T00:00:00Z".into(),
        });
        manifest.write_to_path(&manifest_path(&baselines)).unwrap();

        // Read everything back.
        let back = BaselineFile::read_from_path(&path).unwrap();
        assert_eq!(back, baseline);
        let back_manifest = Manifest::read_from_path(&manifest_path(&baselines)).unwrap();
        assert_eq!(back_manifest.baseline.len(), 1);
        assert_eq!(back_manifest.baseline[0].scenario, "_fake");

        // Workspace-tempdir leak check: serialized YAML must not contain
        // the fake captured tempdir path.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("/tmp/fake-eval"),
            "workspace path leaked into baseline YAML: {raw}"
        );
    }

    /// End-to-end results pipeline: build several fake CapturedRuns,
    /// normalize each, assemble a ResultsFile with K=3 transcripts, write
    /// to disk, read back. Verifies the run-side data path.
    #[test]
    fn run_pipeline_round_trip_via_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("results.yaml");

        let transcripts: Vec<_> = (0..3)
            .map(|_| Normalizer::normalize(&fake_captured(), true))
            .collect();
        let mut scenarios = BTreeMap::new();
        scenarios.insert(
            "_fake".to_string(),
            ScenarioResults {
                user_turns: vec!["Read the file.".into()],
                runs: transcripts,
            },
        );
        let results = ResultsFile {
            git_ref: "abc1234".into(),
            recorded_at: "2026-05-07T12:00:00Z".into(),
            model: "ollama/qwen3-coder".into(),
            scenarios,
        };
        results.write_to_path(&path).unwrap();

        let back = ResultsFile::read_from_path(&path).unwrap();
        assert_eq!(back, results);
        assert_eq!(back.scenarios.get("_fake").unwrap().runs.len(), 3);

        // Sanity: each transcript has the expected event shape.
        let evts = &back.scenarios.get("_fake").unwrap().runs[0].events;
        assert!(evts.iter().any(|e| matches!(e, TranscriptEvent::ToolCall { tool_name, .. } if *tool_name == ToolName::Read)));
        assert!(
            evts.iter()
                .any(|e| matches!(e, TranscriptEvent::AssistantMessage { .. }))
        );
    }
}
