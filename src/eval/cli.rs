//! `steve eval` subcommand entry points.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::json;

use crate::eval::{Judge, Runner, Scenario, apply_judges, evaluate, judge::validate_judge_config};

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
    use std::collections::BTreeMap;

    use crate::eval::{
        results::{ResultsFile, ScenarioResults},
        scenario::discover_scenarios,
        transcript::Normalizer,
    };

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

    let mut scenarios_out: BTreeMap<String, ScenarioResults> = BTreeMap::new();

    for (name, scenario_path) in &selected {
        let scenario = Scenario::from_file(scenario_path)
            .with_context(|| format!("loading scenario {}", scenario_path.display()))?;
        let scenario_dir = scenario_path
            .parent()
            .with_context(|| format!("scenario path has no parent: {}", scenario_path.display()))?;

        let mut transcripts = Vec::with_capacity(scenario.runs.get());
        for run_idx in 0..scenario.runs.get() {
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
        }

        scenarios_out.insert(
            name.clone(),
            ScenarioResults {
                user_turns: scenario.user_turns.clone(),
                runs: transcripts,
            },
        );
    }

    let git_ref = current_git_ref().unwrap_or_else(|| "unknown".to_string());
    let recorded_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
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
    use crate::eval::{
        baseline::{BaselineFile, Manifest, ManifestEntry, baseline_path, manifest_path},
        scenario::discover_scenarios,
        transcript::Normalizer,
    };

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
            None => anyhow::bail!("no scenarios found under {}", scenarios_dir.display()),
        }
    }

    // Read-modify-write the manifest. read_from_path returns Manifest::default()
    // on NotFound, so the fresh-checkout case Just Works.
    let mfst_path = manifest_path(baselines_dir);
    let mut manifest = Manifest::read_from_path(&mfst_path)?;

    let git_ref = current_git_ref().unwrap_or_else(|| "unknown".to_string());
    let frozen_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    for (name, scenario_path) in &selected {
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

        let baseline = BaselineFile {
            scenario: name.clone(),
            model: model.to_string(),
            git_ref: git_ref.clone(),
            frozen_at: frozen_at.clone(),
            user_turns: scenario.user_turns.clone(),
            transcript,
        };
        let path = baseline_path(baselines_dir, name, model)?;
        baseline.write_to_path(&path)?;

        manifest.upsert(ManifestEntry {
            scenario: name.clone(),
            model: model.to_string(),
            git_ref: git_ref.clone(),
            frozen_at: frozen_at.clone(),
        });

        println!("froze {name} -> {}", path.display());
    }

    manifest.write_to_path(&mfst_path)?;
    println!("updated manifest: {}", mfst_path.display());
    Ok(())
}
