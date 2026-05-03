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
    /// Run one eval scenario end-to-end and emit the captured trace as JSON
    /// (chat/coding regression harness — see `eval/scenarios/_smoke/`).
    Eval {
        /// Path to the `scenario.toml` file inside a scenario directory.
        scenario: std::path::PathBuf,
        /// Model to run against, in `provider/model_id` format.
        #[arg(long)]
        model: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
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
        Some(Commands::Eval { scenario, model }) => {
            return steve::eval::cli::run_one(&scenario, &model).await;
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
