mod app;
mod config;
mod event;
mod project;
mod provider;
mod storage;
mod ui;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Detect project root
    let project_info = project::detect_or_cwd();

    // Load config
    let cfg = config::load(&project_info.root)?;

    // Initialize storage
    let store = storage::Storage::new(&project_info.id)?;

    // Load AGENTS.md (if present)
    let agents_md = config::load_agents_md(&project_info.root);

    // Build provider registry (may fail if env vars not set — that's ok, handle gracefully)
    let provider_registry = match provider::ProviderRegistry::from_config(&cfg) {
        Ok(registry) => Some(registry),
        Err(_) => None,
    };

    let mut app = app::App::new(project_info, cfg, store, agents_md, provider_registry);
    app.run().await?;
    Ok(())
}
