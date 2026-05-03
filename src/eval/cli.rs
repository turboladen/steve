//! `steve eval` subcommand entry points.
//!
//! v1: a single command — `steve eval <scenario.toml> --model <provider/id>` —
//! that runs one scenario end-to-end and prints the captured trace as
//! pretty JSON to stdout. Phase 6 expands this into suite execution +
//! `compare` + structured JSONL output.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::json;

use crate::eval::{Runner, Scenario};

/// Run a single scenario and emit the captured trace as pretty JSON to
/// stdout. Returns `Ok(())` when the run completes regardless of whether
/// any expectations would have passed (assertion eval is Phase 3).
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

    // Custom output shape (not just `serde_json::to_string(&captured)`) so
    // we can hide internal scratch fields, surface the duration as ms, and
    // include the scenario name + model up-front for human readability.
    let output = json!({
        "scenario": scenario.name,
        "model": model,
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
