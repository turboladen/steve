#![warn(clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Steve — a TUI AI coding agent
#[derive(Parser)]
#[command(version = concat!(env!("CARGO_PKG_VERSION"), "-", env!("STEVE_GIT_REV")))]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Browse usage data and cost analytics
    Data,
    /// Manage tasks and epics
    Task {
        #[command(subcommand)]
        command: steve::cli::TaskCommand,
    },
    /// Run scenarios. Without a sub-subcommand, runs ONE scenario
    /// end-to-end and emits the captured trace as JSON (the existing
    /// single-shot positional path; mutually exclusive with the
    /// sub-subcommands below).
    Eval(EvalArgs),
}

/// `args_conflicts_with_subcommands` lets the no-subcommand form
/// (`steve eval --scenario X --model Y`) chain run → report, while
/// sub-subcommands (`run`, `baseline freeze`, `report`) own their own
/// args. When a sub-subcommand is given, the top-level args are
/// rejected (and vice versa).
#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
struct EvalArgs {
    /// Scenario name (e.g. `_smoke`). When omitted, runs every scenario
    /// under `eval/scenarios/`.
    #[arg(long)]
    scenario: Option<String>,
    /// Model to run against, in `provider/model_id` format.
    #[arg(long)]
    model: Option<String>,
    /// Override the judge model. Format: `provider/model_id`. Takes
    /// precedence over `.steve.eval.jsonc`'s `default_judge_model` and
    /// per-scenario `judge_model`.
    #[arg(long)]
    judge_model: Option<String>,
    /// Append a row to `eval/history.jsonl` recording this run.
    #[arg(long)]
    record_history: bool,
    /// Write a self-contained HTML report to this path.
    #[arg(long, value_name = "PATH")]
    html: Option<std::path::PathBuf>,
    /// Net win rate threshold for the exit code. Below this value
    /// = regression (exit 1).
    #[arg(long, value_name = "FLOAT")]
    regression_threshold: Option<f64>,
    /// Show per-scenario detail in the text output.
    #[arg(long)]
    verbose: bool,
    /// Override the baselines directory for the chained `run → report`
    /// path. Mirrors the `--baselines-dir` flag on `steve eval report`,
    /// so users on the no-subcommand form can point at a custom
    /// baselines tree without dropping to subcommands. Precedence:
    /// this flag > `eval.baselines_dir` in `.steve.eval.jsonc` >
    /// `<project_root>/eval/baselines/`.
    #[arg(long)]
    baselines_dir: Option<std::path::PathBuf>,
    #[command(subcommand)]
    command: Option<EvalSubcommand>,
}

#[derive(clap::Subcommand)]
enum EvalSubcommand {
    /// Run scenarios K times each (K from `scenario.runs`), writing a
    /// normalized results YAML. No judging.
    Run {
        /// Scenario name (e.g. `_smoke`). When omitted, runs every
        /// scenario under `eval/scenarios/`.
        #[arg(long)]
        scenario: Option<String>,
        /// Model to run against, in `provider/model_id` format.
        #[arg(long)]
        model: String,
        /// Output path for the results YAML. Defaults to a timestamped
        /// path under `<project_root>/eval/results/`. Relative paths are
        /// anchored to the project root; absolute paths are used as-is.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Manage frozen baselines.
    Baseline {
        #[command(subcommand)]
        command: BaselineSubcommand,
    },
    /// Run the paired-comparison report on an existing results.yaml.
    /// Loads the file, resolves per-scenario baselines from
    /// `--baselines-dir`, calls `Judge::compare` for each
    /// (scenario, run) pair, and renders the layered text report.
    /// Exit codes: 0 (pass), 1 (regression below threshold),
    /// 2 (infra error OR no scenarios graded — all skipped/errored).
    Report {
        /// Path to the results file produced by `steve eval run`.
        #[arg(value_name = "RESULTS")]
        results: std::path::PathBuf,
        /// Override the baselines directory.
        /// Default: `<project_root>/eval/baselines/`.
        #[arg(long)]
        baselines_dir: Option<std::path::PathBuf>,
        /// Override the judge model. Format: `provider/model_id`.
        /// Takes precedence over `.steve.eval.jsonc`'s
        /// `default_judge_model` and per-scenario `judge_model`.
        #[arg(long)]
        judge_model: Option<String>,
        /// Append a row to `eval/history.jsonl` recording this run.
        /// Off by default — local exploratory runs don't pollute history.
        #[arg(long)]
        record_history: bool,
        /// Write a self-contained HTML report to this path.
        #[arg(long, value_name = "PATH")]
        html: Option<std::path::PathBuf>,
        /// Net win rate threshold for the exit code. Below this value
        /// = regression (exit 1). Default sourced from
        /// `eval.regression_threshold` in `.steve.eval.jsonc`, or 0.0.
        #[arg(long, value_name = "FLOAT")]
        regression_threshold: Option<f64>,
        /// Show per-scenario detail in the text output.
        #[arg(long)]
        verbose: bool,
    },
}

#[derive(clap::Subcommand)]
enum BaselineSubcommand {
    /// Freeze (capture and overwrite) baseline files for selected scenarios.
    /// `K = 1` regardless of `scenario.runs`; the baseline is the fixed
    /// reference, not a multi-sample artifact. No flags = all scenarios
    /// with the supplied (or configured-default) model.
    Freeze {
        /// Scenario name. When omitted, freezes every scenario.
        #[arg(long)]
        scenario: Option<String>,
        /// Model to freeze for, in `provider/model_id` format.
        #[arg(long)]
        model: String,
    },
}

#[tokio::main]
async fn main() {
    // Exit-code contract:
    //   0 = pass
    //   1 = regression below threshold (report dispatch sets this
    //       via `std::process::exit`)
    //   2 = infra error (any Err from `run`) OR "no scenarios were
    //       graded" — every scenario was Skipped or errored, so the
    //       net win rate is the meaningless 0.0 sentinel and CI must
    //       not interpret it as Pass. Both map to 2 because both
    //       mean "the report didn't produce a usable verdict."
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(2);
    }
}

async fn run() -> Result<()> {
    // Parse CLI args (handles --version, --help automatically)
    let cli = Cli::parse();

    // Set up file-based tracing (TUI owns stdout, so we log to file)
    let log_dir = directories::ProjectDirs::from("", "", "steve")
        .map(|d| d.data_dir().join("logs"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/steve-logs"));

    std::fs::create_dir_all(&log_dir)?;

    let file_appender = tracing_appender::rolling::daily(&log_dir, "steve.log");

    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(file_appender)
                .with_ansi(false)
                .with_target(true)
                .with_thread_ids(false),
        )
        // `steve=info` is target-prefixed: events whose target doesn't start
        // with `steve` (notably `rmcp::*`) would otherwise be filtered to OFF.
        // Adding `rmcp=warn` keeps the MCP transport's own warnings/errors
        // visible without info-level chatter.
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("steve=info,rmcp=warn")),
        )
        .init();

    tracing::info!("steve starting up");

    // Idempotent sweep of orphan memory.md files left by the removed memory tool.
    let removed = steve::storage::sweep_legacy_memory_files();
    if removed > 0 {
        tracing::info!(count = removed, "removed legacy memory.md files");
    }

    // Handle subcommands that don't need the full chat TUI setup
    match cli.command {
        Some(Commands::Data) => {
            let data_dir = directories::ProjectDirs::from("", "", "steve")
                .map(|d| d.data_dir().to_path_buf())
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp/steve-data"));
            let db_path = data_dir.join("usage.db");
            return steve::data::run(&db_path);
        }
        Some(Commands::Task { command }) => {
            return steve::cli::run_task(command);
        }
        Some(Commands::Eval(args)) => {
            return dispatch_eval(args).await;
        }
        None => {}
    }

    // Detect project root
    let project_info = steve::project::detect_or_cwd();
    tracing::info!(root = %project_info.root.display(), id = %project_info.id, "project detected");

    // Load config
    let (cfg, config_warnings) = steve::config::load(&project_info.root)?;
    tracing::info!(providers = cfg.providers.len(), "config loaded");

    // Initialize storage
    let store = steve::storage::Storage::new(&project_info.id)?;

    // Load AGENTS.md chain (walk from CWD up to project root)
    let agents_files = steve::config::load_agents_md_chain(&project_info.root, &project_info.cwd);
    if !agents_files.is_empty() {
        tracing::info!(count = agents_files.len(), "AGENTS.md file(s) loaded");
    }

    // Build provider registry. Providers whose api_key env var is unset are
    // skipped and reported as warnings — the registry still contains any
    // provider whose env var IS set, so partial failures don't disable steve.
    let (provider_registry, missing_api_keys) =
        steve::provider::ProviderRegistry::from_config(&cfg);
    tracing::info!(
        missing = missing_api_keys.len(),
        "provider registry initialized",
    );
    for warning in &missing_api_keys {
        let reason = match warning.reason {
            steve::provider::ProviderInitReason::MissingEnvVar => "env var not set",
            steve::provider::ProviderInitReason::NonUtf8EnvVar => "env var is not valid UTF-8",
        };
        tracing::warn!(
            provider = %warning.provider_id,
            env_var = %warning.env_var,
            "provider disabled: {reason}",
        );
    }

    // Initialize usage analytics (SQLite background writer)
    let data_dir = directories::ProjectDirs::from("", "", "steve")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/steve-data"));
    std::fs::create_dir_all(&data_dir)?;
    let usage_handle = steve::usage::spawn_usage_writer(&data_dir.join("usage.db"))?;
    usage_handle
        .writer
        .upsert_project(steve::usage::types::ProjectRecord {
            project_id: project_info.id.clone(),
            display_name: project_info
                .root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| project_info.id.clone()),
            root_path: project_info.root.display().to_string(),
        });

    let mut app = steve::app::App::new(
        project_info,
        cfg,
        store,
        agents_files,
        Some(provider_registry),
        missing_api_keys,
        config_warnings,
        usage_handle.writer.clone(),
    );
    app.run().await?;

    usage_handle.shutdown_and_wait();
    tracing::info!("steve shutting down");
    Ok(())
}

/// Resolve the baselines directory by precedence:
/// 1. `--baselines-dir` CLI flag (highest)
/// 2. `eval.baselines_dir` from `.steve.eval.jsonc`
/// 3. project default (`<project_root>/eval/baselines/`)
///
/// Relative paths are anchored to `project_root`. Absolute paths used
/// as-is. Relative overrides containing `..` components are rejected
/// up front — they would escape project_root via path joining. Caller
/// is responsible for symlink-rejection via `validate_baselines_dir`
/// after resolution.
fn resolve_baselines_dir(
    cli_override: Option<&std::path::Path>,
    config_override: Option<&str>,
    project_default: &std::path::Path,
    project_root: &std::path::Path,
) -> Result<std::path::PathBuf> {
    let reject_parent_components = |path: &std::path::Path, source: &str| -> Result<()> {
        if path.is_relative()
            && path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            anyhow::bail!(
                "baselines dir from {source} contains `..` components ({}); \
                 refusing to escape project root",
                path.display()
            );
        }
        Ok(())
    };
    if let Some(p) = cli_override {
        reject_parent_components(p, "--baselines-dir CLI flag")?;
        return Ok(if p.is_absolute() {
            p.to_path_buf()
        } else {
            project_root.join(p)
        });
    }
    if let Some(s) = config_override {
        let p = std::path::Path::new(s);
        reject_parent_components(p, ".steve.eval.jsonc eval.baselines_dir")?;
        return Ok(if p.is_absolute() {
            p.to_path_buf()
        } else {
            project_root.join(p)
        });
    }
    Ok(project_default.to_path_buf())
}

/// Reject a baselines_dir that exists as a symlink or non-directory.
/// NotFound is allowed (freeze creates it). Used both on the default
/// `eval/baselines/` and on any `--baselines-dir` override / config
/// override — without this, a symlinked override would let
/// baseline.write_to_path escape the project root.
fn validate_baselines_dir(path: &std::path::Path, project_root: &std::path::Path) -> Result<()> {
    let ok = match std::fs::symlink_metadata(path) {
        Ok(m) => m.file_type().is_dir(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            return Err(anyhow::Error::from(e)
                .context(format!("checking baselines dir at {}", path.display())));
        }
    };
    if !ok {
        anyhow::bail!(
            "baselines dir at {} exists but is a symlink or non-directory \
             (project root: {}); refusing to write through it (would escape the repo).",
            path.display(),
            project_root.display()
        );
    }
    Ok(())
}

async fn dispatch_eval(args: EvalArgs) -> Result<()> {
    // Resolve eval directories against the detected project root so the user
    // can invoke `steve eval ...` from a subdirectory of the repo (e.g.
    // `cd src/eval && steve eval baseline freeze`) without hitting "no
    // scenarios found" simply because CWD doesn't contain `eval/scenarios`.
    // detect_or_cwd walks up looking for a `.git/` directory and falls back
    // to CWD outside a repo (the silent fallback is fine because the
    // existence check below catches the common misuse cases).
    let project = steve::project::detect_or_cwd();
    let scenarios_dir = project.root.join("eval/scenarios");
    let baselines_dir = project.root.join("eval/baselines");

    // Guard the common misuse case: invoking `steve eval ...` outside the
    // steve repo (e.g. from /tmp, where detect_or_cwd silently falls back
    // to CWD) or from an unrelated cargo project (whose .git/ detect_or_cwd
    // *will* find but which has no eval/scenarios). Without this guard the
    // user would see a misleading "no scenarios found" error pointing at
    // a directory that doesn't even exist.
    //
    // Use symlink_metadata + is_dir() rather than exists() so a symlinked
    // eval/scenarios is rejected — matching discover_scenarios's posture
    // (and ScenarioWorkspace::build's symlink-rejection rationale: a
    // symlinked scenarios dir could exfiltrate file content from outside
    // the repo).
    //
    // Distinguish NotFound (the misuse case we want to give a friendly
    // message for) from other I/O errors (permission denied, transient
    // failure) so we don't mask real diagnostics behind a generic
    // "not found" message.
    let scenarios_dir_ok = match std::fs::symlink_metadata(&scenarios_dir) {
        Ok(m) => m.file_type().is_dir(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            return Err(anyhow::Error::from(e).context(format!(
                "checking eval/scenarios at {}",
                scenarios_dir.display()
            )));
        }
    };
    if !scenarios_dir_ok {
        anyhow::bail!(
            "eval/scenarios not found (or is a symlink) at {} (detected project root: {}). \
             Run `steve eval ...` from inside the steve repository.",
            scenarios_dir.display(),
            project.root.display()
        );
    }

    // baselines_dir validation is deliberately NOT done here. Each
    // subcommand that actually reads or writes baselines validates
    // its OWN resolved path (freeze and report each call
    // `validate_baselines_dir(&baselines_dir_used, ...)` after
    // applying CLI/config overrides). Validating the default
    // upstream would (a) block `steve eval run` even though it
    // doesn't touch baselines, and (b) reject a valid
    // `--baselines-dir /elsewhere` invocation when the default
    // `eval/baselines/` happened to be a symlink.

    // Sub-subcommand path — new shapes.
    if let Some(sub) = args.command {
        match sub {
            EvalSubcommand::Run {
                scenario,
                model,
                out,
            } => {
                // --out resolution: absolute path used as-is; relative path
                // anchored against project.root for symmetry with the default
                // (otherwise `--out results/x.yaml` from a subdir would land
                // somewhere different from the same default-construction).
                let out_path = match out {
                    Some(p) if p.is_absolute() => p,
                    Some(p) => project.root.join(p),
                    None => {
                        let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                        let scope = scenario.as_deref().unwrap_or("all");
                        project.root.join(format!("eval/results/{scope}-{ts}.yaml"))
                    }
                };
                return steve::eval::cli::run_subcommand(
                    &scenarios_dir,
                    scenario.as_deref(),
                    &model,
                    &out_path,
                )
                .await;
            }
            EvalSubcommand::Baseline { command } => match command {
                BaselineSubcommand::Freeze { scenario, model } => {
                    // Freeze must write to the SAME baselines dir that
                    // report will read from — otherwise freeze + report
                    // silently diverge. Layer eval_cfg.baselines_dir in
                    // here too. Freeze has no CLI override today; the
                    // resolution chain is config > default.
                    let eval_cfg = steve::config::load_eval_config(&project.root)?;
                    let baselines_dir_used = resolve_baselines_dir(
                        None,
                        eval_cfg.baselines_dir.as_deref(),
                        &baselines_dir,
                        &project.root,
                    )?;
                    validate_baselines_dir(&baselines_dir_used, &project.root)?;
                    return steve::eval::cli::freeze_subcommand(
                        &scenarios_dir,
                        &baselines_dir_used,
                        scenario.as_deref(),
                        &model,
                    )
                    .await;
                }
            },
            EvalSubcommand::Report {
                results,
                baselines_dir: baselines_dir_override,
                judge_model,
                record_history,
                html,
                regression_threshold,
                verbose,
            } => {
                // Load configs early so we can layer `eval_cfg.baselines_dir`
                // into the resolution precedence.
                let (cfg, warnings) = steve::config::load(&project.root)?;
                for w in &warnings {
                    eprintln!("warning: {w}");
                }
                let eval_cfg = steve::config::load_eval_config(&project.root)?;
                // Resolve baselines dir: CLI flag > eval_cfg.baselines_dir
                // > project default. Relative paths anchored to project
                // root for symmetry with the default.
                let baselines_dir_used = resolve_baselines_dir(
                    baselines_dir_override.as_deref(),
                    eval_cfg.baselines_dir.as_deref(),
                    &baselines_dir,
                    &project.root,
                )?;
                // Pre-flight: if the override resolves to a symlink or
                // non-directory, reject up front rather than silently
                // skipping every scenario downstream.
                validate_baselines_dir(&baselines_dir_used, &project.root)?;
                // ProviderRegistry::from_config returns warnings for
                // providers missing API keys — these are non-fatal:
                // the report only needs the *judge model's* provider
                // configured. Surfacing them as bail!() would force
                // an operator with extra unused providers in
                // .steve.jsonc to set env vars they don't use.
                // Judge::from_registry below errors loudly if the
                // resolved judge model itself is unreachable.
                let (registry, missing) = steve::provider::ProviderRegistry::from_config(&cfg);
                for w in &missing {
                    eprintln!(
                        "warning: provider {} missing API key — judge calls to it will fail",
                        w.provider_id
                    );
                }
                let history_path = project.root.join("eval/history.jsonl");
                // Judge model precedence: CLI > scenario.judge_model
                // > eval_cfg.default_judge_model. CLI and
                // config-default thread through separately;
                // collapsing them upstream would make config_default
                // beat per-scenario overrides.
                let exit = steve::eval::cli::report_subcommand(steve::eval::cli::ReportArgs {
                    results_path: &results,
                    baselines_dir: &baselines_dir_used,
                    scenarios_dir: &scenarios_dir,
                    history_path: &history_path,
                    html_out: html.as_deref(),
                    cli_judge_model: judge_model.as_deref(),
                    config_default_judge_model: eval_cfg.default_judge_model.as_deref(),
                    cli_regression_threshold: regression_threshold,
                    config_regression_threshold: eval_cfg.regression_threshold,
                    verbose,
                    record_history,
                    registry: &registry,
                })
                .await?;
                std::process::exit(exit.as_i32());
            }
        }
    }

    // No subcommand: chain run → report against the configured baseline.
    let Some(model) = args.model else {
        anyhow::bail!(
            "'steve eval' (no subcommand) requires --model <provider/model_id>; \
             alternatively use 'steve eval run', 'steve eval baseline freeze', \
             or 'steve eval report'"
        );
    };

    // 1. Run: produce a temp results file under eval/results/.
    let results_dir = project.root.join("eval/results");
    std::fs::create_dir_all(&results_dir)?;
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let scope = args.scenario.as_deref().unwrap_or("all");
    let results_path = results_dir.join(format!("chained-{scope}-{ts}.yaml"));
    steve::eval::cli::run_subcommand(
        &scenarios_dir,
        args.scenario.as_deref(),
        &model,
        &results_path,
    )
    .await?;

    // 2. Report: against the configured baseline.
    let (cfg, warnings) = steve::config::load(&project.root)?;
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    let eval_cfg = steve::config::load_eval_config(&project.root)?;
    // Missing-API-key warnings are non-fatal — only the resolved
    // judge model's provider needs to be reachable. See the
    // report-subcommand site for rationale.
    let (registry, missing) = steve::provider::ProviderRegistry::from_config(&cfg);
    for w in &missing {
        eprintln!(
            "warning: provider {} missing API key — judge calls to it will fail",
            w.provider_id
        );
    }
    let baselines_dir_used = resolve_baselines_dir(
        args.baselines_dir.as_deref(),
        eval_cfg.baselines_dir.as_deref(),
        &baselines_dir,
        &project.root,
    )?;
    validate_baselines_dir(&baselines_dir_used, &project.root)?;
    let history_path = project.root.join("eval/history.jsonl");
    let exit = steve::eval::cli::report_subcommand(steve::eval::cli::ReportArgs {
        results_path: &results_path,
        baselines_dir: &baselines_dir_used,
        scenarios_dir: &scenarios_dir,
        history_path: &history_path,
        html_out: args.html.as_deref(),
        cli_judge_model: args.judge_model.as_deref(),
        config_default_judge_model: eval_cfg.default_judge_model.as_deref(),
        cli_regression_threshold: args.regression_threshold,
        config_regression_threshold: eval_cfg.regression_threshold,
        verbose: args.verbose,
        record_history: args.record_history,
        registry: &registry,
    })
    .await?;
    std::process::exit(exit.as_i32());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    // ── resolve_baselines_dir precedence ──

    #[test]
    fn resolve_baselines_dir_cli_wins_when_set() {
        let root = PathBuf::from("/project");
        let default = root.join("eval/baselines");
        let out = resolve_baselines_dir(
            Some(std::path::Path::new("/cli/abs")),
            Some("config/rel"),
            &default,
            &root,
        )
        .unwrap();
        assert_eq!(out, PathBuf::from("/cli/abs"));
    }

    #[test]
    fn resolve_baselines_dir_cli_relative_anchored_to_project_root() {
        let root = PathBuf::from("/project");
        let default = root.join("eval/baselines");
        let out =
            resolve_baselines_dir(Some(std::path::Path::new("rel/cli")), None, &default, &root)
                .unwrap();
        assert_eq!(out, PathBuf::from("/project/rel/cli"));
    }

    #[test]
    fn resolve_baselines_dir_config_used_when_no_cli() {
        let root = PathBuf::from("/project");
        let default = root.join("eval/baselines");
        let out = resolve_baselines_dir(None, Some("/config/abs"), &default, &root).unwrap();
        assert_eq!(out, PathBuf::from("/config/abs"));
    }

    #[test]
    fn resolve_baselines_dir_config_relative_anchored_to_project_root() {
        let root = PathBuf::from("/project");
        let default = root.join("eval/baselines");
        let out = resolve_baselines_dir(None, Some("config/rel"), &default, &root).unwrap();
        assert_eq!(out, PathBuf::from("/project/config/rel"));
    }

    #[test]
    fn resolve_baselines_dir_falls_back_to_default_when_both_unset() {
        let root = PathBuf::from("/project");
        let default = root.join("eval/baselines");
        let out = resolve_baselines_dir(None, None, &default, &root).unwrap();
        assert_eq!(out, default);
    }

    /// With a CLI override on report (but none on freeze, which has
    /// no CLI flag today), the resolved paths MUST differ — CLI is
    /// a report-only knob. A future change that wired the override
    /// into freeze would silently re-freeze baselines under a path
    /// that report can no longer read.
    #[test]
    fn resolve_baselines_dir_freeze_and_report_diverge_when_report_has_cli_override() {
        let root = PathBuf::from("/project");
        let default = root.join("eval/baselines");
        let config = Some("from/config");
        let freeze_path = resolve_baselines_dir(None, config, &default, &root).unwrap();
        let report_path =
            resolve_baselines_dir(Some(&PathBuf::from("/from/cli")), config, &default, &root)
                .unwrap();
        assert_ne!(
            freeze_path, report_path,
            "CLI override is report-only — freeze must not see it",
        );
        assert_eq!(freeze_path, root.join("from/config"));
        assert_eq!(report_path, PathBuf::from("/from/cli"));
    }

    #[test]
    fn resolve_baselines_dir_rejects_parent_components_in_relative_cli_override() {
        // `--baselines-dir ../outside` would escape project_root via
        // path joining. Reject before any FS touch.
        let root = PathBuf::from("/project");
        let default = root.join("eval/baselines");
        let result = resolve_baselines_dir(
            Some(std::path::Path::new("../outside")),
            None,
            &default,
            &root,
        );
        let err = result.expect_err("relative `..` override must be rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("..") && msg.contains("CLI"),
            "expected `..` + CLI in diagnostic; got: {msg}"
        );
    }

    #[test]
    fn resolve_baselines_dir_rejects_parent_components_in_relative_config_override() {
        let root = PathBuf::from("/project");
        let default = root.join("eval/baselines");
        let result = resolve_baselines_dir(None, Some("eval/../outside"), &default, &root);
        let err = result.expect_err("relative `..` config override must be rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("..") && msg.contains(".steve.eval.jsonc"),
            "expected `..` + config source in diagnostic; got: {msg}"
        );
    }

    #[test]
    fn resolve_baselines_dir_allows_absolute_path_outside_project_root() {
        // Absolute overrides are an explicit user choice — they bypass
        // the project_root anchor by design. The `..` rejection only
        // applies to relative paths (which would silently escape).
        let root = PathBuf::from("/project");
        let default = root.join("eval/baselines");
        let out = resolve_baselines_dir(
            Some(std::path::Path::new("/somewhere/else")),
            None,
            &default,
            &root,
        )
        .unwrap();
        assert_eq!(out, PathBuf::from("/somewhere/else"));
    }

    // ── validate_baselines_dir ──

    #[test]
    fn validate_baselines_dir_accepts_real_directory() {
        let tmp = TempDir::new().unwrap();
        validate_baselines_dir(tmp.path(), tmp.path()).unwrap();
    }

    #[test]
    fn validate_baselines_dir_accepts_not_found() {
        // NotFound is OK because freeze creates the dir on first run.
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist");
        validate_baselines_dir(&missing, tmp.path()).unwrap();
    }

    #[test]
    fn validate_baselines_dir_rejects_regular_file() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("file.txt");
        std::fs::write(&file, b"x").unwrap();
        let err = validate_baselines_dir(&file, tmp.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("symlink or non-directory"),
            "expected diagnostic to mention symlink-or-non-dir; got: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn validate_baselines_dir_rejects_symlink() {
        // The whole point of this validator: symlinked baselines_dir
        // would let baseline.write_to_path escape the project root.
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("real-target");
        std::fs::create_dir_all(&target).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = validate_baselines_dir(&link, tmp.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("symlink or non-directory"),
            "expected symlink-rejection diagnostic; got: {msg}"
        );
    }
}
