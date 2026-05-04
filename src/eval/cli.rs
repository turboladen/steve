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
