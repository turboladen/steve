//! `steve eval` subcommand entry points.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::json;

use crate::eval::{Runner, Scenario, evaluate};

/// Run a single scenario and emit the captured trace + assertion report as
/// pretty JSON to stdout. Exit code stays 0 even when expectations fail —
/// the JSON's `passed` field carries the verdict, and we don't want to
/// lose the trace in the user's pipeline on a failed run.
pub async fn run_one(scenario_path: &Path, model: &str) -> Result<()> {
    let scenario = Scenario::from_file(scenario_path)
        .with_context(|| format!("loading scenario from {}", scenario_path.display()))?;
    let scenario_dir = scenario_path.parent().with_context(|| {
        format!(
            "scenario path has no parent dir: {}",
            scenario_path.display()
        )
    })?;

    let runner = Runner::build(&scenario, scenario_dir, model)?;
    let captured = runner.run(&scenario).await?;
    let report = evaluate(&scenario, &captured);

    // Top-level verdict combines BOTH expectation outcomes AND run
    // completion. A scenario that aborts via LlmError or hits a per-turn
    // timeout must NOT report passed=true even if an early expectation was
    // satisfied before the abort — `errors` and `timed_out` are not just
    // a side channel for diagnostics.
    let passed = report.passed() && captured.completed_normally();
    let output = json!({
        "scenario": scenario.name,
        "model": model,
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
