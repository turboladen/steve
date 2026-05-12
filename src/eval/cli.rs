//! `steve eval` subcommand entry points.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use serde_json::json;

use crate::eval::{
    Judge, Runner, Scenario, apply_judges,
    baseline::{BaselineFile, Manifest, ManifestEntry, baseline_path, manifest_path},
    evaluate,
    judge::validate_judge_config,
    results::{ResultsFile, ScenarioResults},
    scenario::discover_scenarios,
    transcript::Normalizer,
};

/// Run a single scenario and emit the captured trace + assertion report as
/// pretty JSON to stdout. Exit code stays 0 even when expectations fail —
/// the JSON's `passed` field carries the verdict, and we don't want to
/// lose the trace in the user's pipeline on a failed run.
///
/// `judge_model` overrides every Judge expectation's model selection
/// (CLI > per-expectation > scenario-level); when `None` and no other
/// source is set, Judge expectations fail loudly with a clear message
/// rather than being silently skipped.
pub async fn run_one(scenario_path: &Path, model: &str, judge_model: Option<&str>) -> Result<()> {
    let scenario = Scenario::from_file(scenario_path)
        .with_context(|| format!("loading scenario from {}", scenario_path.display()))?;
    let scenario_dir = scenario_path.parent().with_context(|| {
        format!(
            "scenario path has no parent dir: {}",
            scenario_path.display()
        )
    })?;

    let mut runner = Runner::build(&scenario, scenario_dir, model)?;

    // Fail loud on missing/unresolvable judge models BEFORE running the
    // scenario — same posture as Runner::build's API-key check. Otherwise
    // the user burns the agent's token budget only to find at the end
    // that the judge couldn't grade the result.
    validate_judge_config(&scenario, runner.judge_registry(), judge_model)?;

    let captured = runner.run(&scenario).await?;
    let mut report = evaluate(&scenario, &captured);

    let judge = Judge::from_registry(runner.judge_registry(), judge_model);
    apply_judges(&mut report, &scenario, &captured, &judge).await;

    // Top-level verdict combines BOTH expectation outcomes AND run
    // completion. A scenario that aborts via LlmError or hits a per-turn
    // timeout must NOT report passed=true even if an early expectation was
    // satisfied before the abort — `errors` and `timed_out` are not just
    // a side channel for diagnostics.
    let passed = report.passed() && captured.completed_normally();
    let output = json!({
        "scenario": scenario.name,
        "model": model,
        "judge_model_cli": judge_model,
        "passed": passed,
        "results": report.results,
        "tool_calls": captured.tool_calls,
        "assistant_messages": captured.assistant_messages,
        "usage": captured.usage,
        "duration_ms": captured.duration.as_millis() as u64,
        "timed_out": captured.timed_out,
        "errors": captured.errors,
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Format the per-scenario header line printed at the start of each
/// scenario's runs. Surfaces the resolved scoring axes (and whether
/// they came from a `[scoring]` block or `DEFAULT_AXES`) so a manual
/// CLI smoke can verify Phase 7's `[scoring]` override end-to-end —
/// the override would otherwise only be observable through unit
/// tests, not the `eval run` output.
fn format_scenario_header(name: &str, index: usize, total: usize, scenario: &Scenario) -> String {
    let axes_str = scenario
        .scoring_axes()
        .iter()
        .map(|a| format!("{a}"))
        .collect::<Vec<_>>()
        .join(", ");
    // Annotate only when the axes came from a [scoring] override —
    // the default case is what readers expect, so the `(override)`
    // marker is the load-bearing signal. Tagging both cases would
    // just add noise to every line.
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
        // Print resolved scoring axes so Phase 7's [scoring] block
        // override is observable from a real run — without this, the
        // override is only visible through `cargo test`, not through
        // the CLI smoke path.
        println!("{}", format_scenario_header(name, i, total, &scenario));

        let runs = scenario.runs.get();
        let mut transcripts = Vec::with_capacity(runs);
        for run_idx in 0..runs {
            let started = std::time::Instant::now();
            print!("  run {}/{}...", run_idx + 1, runs);
            std::io::Write::flush(&mut std::io::stdout()).ok();
            // Fresh Runner per run -> fresh tempdir workspace. Without this,
            // `setup.shell` mutations from a prior run would persist into
            // the next run's working state. Each run is a clean sample.
            let mut runner = Runner::build(&scenario, scenario_dir, model)
                .with_context(|| format!("building runner for {name} run #{}", run_idx + 1))?;
            let captured = runner
                .run(&scenario)
                .await
                .with_context(|| format!("running scenario {name} run #{}", run_idx + 1))?;
            // Compute deterministic-floor verdict the same way `run_one` does:
            // expectations.passed() && captured.completed_normally().
            let report = evaluate(&scenario, &captured);
            let floor_passed = report.passed() && captured.completed_normally();
            transcripts.push(Normalizer::normalize(&captured, floor_passed));
            println!(" done in {:.1}s", started.elapsed().as_secs_f32());
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
    println!("wrote results to {}", out_path.display());
    Ok(())
}

/// Phase 8 exit codes. Mapped to process exit by main.rs:
/// `Pass=0`, `Regression=1`. (InfraError=2 comes from main's
/// generic `Err` handler, not this enum.)
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
    /// Judge model resolved upstream: CLI `--judge-model` flag >
    /// eval-config `default_judge_model` > None. The orchestrator
    /// then layers scenario.judge_model on top of None.
    pub judge_model: Option<&'a str>,
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

    // Resolve judge model. CLI/config-supplied model > error. (The
    // orchestrator then layers per-scenario `judge_model` from the
    // scenario.toml on top of None where set; that's
    // Report::build_from_results's job.)
    let resolved_judge = args.judge_model.ok_or_else(|| {
        anyhow::anyhow!(
            "no judge model configured: pass --judge-model <provider/model>, or set \
             `default_judge_model` in `.steve.eval.jsonc`, or set `judge_model` on the scenario"
        )
    })?;
    let judge = Judge::from_registry(args.registry, Some(resolved_judge));

    let report = Report::build_from_results(
        &results,
        args.baselines_dir,
        &args.results_path.display().to_string(),
        &judge,
        resolved_judge,
        Some(args.scenarios_dir),
    )
    .await?;

    print!("{}", report.render_text(args.verbose));

    if let Some(html_path) = args.html_out {
        let history = read_history(args.history_path)
            .with_context(|| format!("reading history from {}", args.history_path.display()))?;
        let html = render_html(&report, &history);
        std::fs::write(html_path, html)
            .with_context(|| format!("writing HTML to {}", html_path.display()))?;
        println!("wrote HTML report to {}", html_path.display());
    }

    if args.record_history {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let entry = HistoryEntry::from_report(&report, now);
        append_history(args.history_path, &entry)?;
        println!("appended history row to {}", args.history_path.display());
    }

    let threshold = args
        .cli_regression_threshold
        .or(args.config_regression_threshold)
        .unwrap_or(0.0);
    let exit = if report.headline_totals.net_win_rate() < threshold {
        ReportExitCode::Regression
    } else {
        ReportExitCode::Pass
    };
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
        print!("running scenario {name} ({}/{})...", i + 1, total);
        std::io::Write::flush(&mut std::io::stdout()).ok();
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
        println!(" done in {:.1}s", started.elapsed().as_secs_f32());

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
    for (idx, (path, baseline, _)) in pending.iter().enumerate() {
        baseline.write_to_path(path).with_context(|| {
            format!(
                "writing baseline for {} ({} earlier baseline(s) in this run already on disk; \
                 re-run freeze to restore consistent state)",
                baseline.scenario, idx
            )
        })?;
        println!("froze {} -> {}", baseline.scenario, path.display());
    }
    for (_, _, entry) in pending {
        manifest.upsert(entry);
    }
    manifest.write_to_path(&mfst_path)?;
    println!("updated manifest: {}", mfst_path.display());
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
        // Phase 7 contract: the override is observable from the CLI
        // smoke. The `(override)` tag is the load-bearing signal —
        // readers expect defaults silently, so only the override
        // case earns a label.
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
