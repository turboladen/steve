#![allow(dead_code)]

mod app;
mod command;
mod config;
mod context;
mod event;
mod export;
mod permission;
mod project;
mod provider;
mod session;
mod storage;
mod stream;
mod tool;
mod ui;

use anyhow::Result;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[tokio::main]
async fn main() -> Result<()> {
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

    // Detect project root
    let project_info = project::detect_or_cwd();
    tracing::info!(root = %project_info.root.display(), id = %project_info.id, "project detected");

    // Load config
    let cfg = config::load(&project_info.root)?;
    tracing::info!(providers = cfg.providers.len(), "config loaded");

    // Initialize storage
    let store = storage::Storage::new(&project_info.id)?;

    // Load AGENTS.md (if present)
    let agents_md = config::load_agents_md(&project_info.root);
    if agents_md.is_some() {
        tracing::info!("AGENTS.md loaded");
    }

    // Build provider registry (may fail if env vars not set)
    let (provider_registry, provider_error) = match provider::ProviderRegistry::from_config(&cfg) {
        Ok(registry) => {
            tracing::info!("provider registry initialized");
            (Some(registry), None)
        }
        Err(e) => {
            tracing::warn!(error = %e, "provider registry failed");
            (None, Some(e.to_string()))
        }
    };

    let mut app = app::App::new(project_info, cfg, store, agents_md, provider_registry, provider_error);
    app.run().await?;

    tracing::info!("steve shutting down");
    Ok(())
}
