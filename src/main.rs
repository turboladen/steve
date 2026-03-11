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
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_target(true)
                .with_thread_ids(false),
        )
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("steve=info")),
        )
        .init();

    tracing::info!("steve starting up");

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

    // Load AGENTS.md (if present)
    let agents_md = steve::config::load_agents_md(&project_info.root);
    if agents_md.is_some() {
        tracing::info!("AGENTS.md loaded");
    }

    // Build provider registry (may fail if env vars not set)
    let (provider_registry, provider_error) = match steve::provider::ProviderRegistry::from_config(&cfg) {
        Ok(registry) => {
            tracing::info!("provider registry initialized");
            (Some(registry), None)
        }
        Err(e) => {
            tracing::warn!(error = %e, "provider registry failed");
            (None, Some(e.to_string()))
        }
    };

    // Initialize usage analytics (SQLite background writer)
    let data_dir = directories::ProjectDirs::from("", "", "steve")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/steve-data"));
    std::fs::create_dir_all(&data_dir)?;
    let usage_handle = steve::usage::spawn_usage_writer(&data_dir.join("usage.db"))?;
    usage_handle.writer.upsert_project(steve::usage::types::ProjectRecord {
        project_id: project_info.id.clone(),
        display_name: project_info.root.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| project_info.id.clone()),
        root_path: project_info.root.display().to_string(),
    });

    let mut app = steve::app::App::new(project_info, cfg, store, agents_md, provider_registry, provider_error, config_warnings, usage_handle.writer.clone());
    app.run().await?;

    usage_handle.shutdown_and_wait();
    tracing::info!("steve shutting down");
    Ok(())
}
