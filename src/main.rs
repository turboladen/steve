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

/// `args_conflicts_with_subcommands` lets us keep the existing positional
/// `<scenario>` form (`steve eval eval/scenarios/_smoke/scenario.toml --model X`)
/// while also offering the new sub-subcommands. When a sub-subcommand is
/// given, the positional args are not allowed (and vice versa).
#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
struct EvalArgs {
    /// Single-shot positional path: `scenario.toml` to run end-to-end with
    /// a captured-trace JSON dump on stdout. Mutually exclusive with the
    /// sub-subcommands below.
    #[arg(value_name = "SCENARIO")]
    scenario: Option<std::path::PathBuf>,
    /// Model to run against, in `provider/model_id` format. Required for
    /// the positional form.
    #[arg(long)]
    model: Option<String>,
    /// Override the judge model for `Judge` expectations (positional form).
    #[arg(long)]
    judge_model: Option<String>,
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
    /// Exit code 0 (pass) / 1 (regression below threshold) / 2 (infra error).
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
    // Phase 8 exit-code contract: 0 (pass) / 1 (regression, set via
    // `std::process::exit` inside the report dispatch) / 2 (infra
    // error — any Err from `run`). Other CLI paths exit 2 on Err too
    // to keep the contract uniform.
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
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("steve=info")))
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

    // Same symlink-rejection posture for baselines_dir: if it exists, it
    // must be a real directory. NotFound is allowed because freeze creates
    // the directory on first run via create_dir_all. Without this guard, a
    // symlinked eval/baselines would let baseline.write_to_path() write
    // outside the repo root.
    let baselines_dir_ok = match std::fs::symlink_metadata(&baselines_dir) {
        Ok(m) => m.file_type().is_dir(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            return Err(anyhow::Error::from(e).context(format!(
                "checking eval/baselines at {}",
                baselines_dir.display()
            )));
        }
    };
    if !baselines_dir_ok {
        anyhow::bail!(
            "eval/baselines exists but is a symlink or non-directory at {} \
             (detected project root: {}); refusing to write through it \
             (would escape the repo).",
            baselines_dir.display(),
            project.root.display()
        );
    }

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
                    return steve::eval::cli::freeze_subcommand(
                        &scenarios_dir,
                        &baselines_dir,
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
                // Resolve the baselines dir. CLI flag wins over the
                // project default we computed above.
                let baselines_dir_used = match baselines_dir_override {
                    Some(p) if p.is_absolute() => p,
                    Some(p) => project.root.join(p),
                    None => baselines_dir.clone(),
                };
                // Load the base config (for the ProviderRegistry that
                // resolves judge models) AND the eval-specific config
                // (regression threshold + default_judge_model).
                let (cfg, _warnings) = steve::config::load(&project.root)?;
                let eval_cfg = steve::config::load_eval_config(&project.root)?;
                let (registry, missing) = steve::provider::ProviderRegistry::from_config(&cfg);
                if !missing.is_empty() {
                    let names: Vec<String> =
                        missing.iter().map(|w| w.provider_id.clone()).collect();
                    anyhow::bail!(
                        "eval report requires API keys for the configured provider(s): {}",
                        names.join(", ")
                    );
                }
                let history_path = project.root.join("eval/history.jsonl");
                // Judge model precedence: --judge-model CLI flag >
                // .steve.eval.jsonc's default_judge_model > None.
                // (Per-scenario judge_model is applied inside
                // Report::build_from_results when it's available.)
                let judge_model_resolved =
                    judge_model.clone().or(eval_cfg.default_judge_model.clone());
                let exit = steve::eval::cli::report_subcommand(steve::eval::cli::ReportArgs {
                    results_path: &results,
                    baselines_dir: &baselines_dir_used,
                    scenarios_dir: &scenarios_dir,
                    history_path: &history_path,
                    html_out: html.as_deref(),
                    judge_model: judge_model_resolved.as_deref(),
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

    // Single-shot positional path: scenario + --model required.
    let Some(scenario) = args.scenario else {
        anyhow::bail!(
            "supply a scenario path (e.g. 'steve eval eval/scenarios/_smoke/scenario.toml --model X') \
             or use a sub-subcommand ('steve eval run', 'steve eval baseline freeze')"
        );
    };
    let Some(model) = args.model else {
        anyhow::bail!("'steve eval <scenario>' requires --model");
    };
    steve::eval::cli::run_one(&scenario, &model, args.judge_model.as_deref()).await
}
