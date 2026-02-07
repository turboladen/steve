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

    // Build provider registry (may fail if env vars not set)
    let (provider_registry, provider_error) = match provider::ProviderRegistry::from_config(&cfg) {
        Ok(registry) => (Some(registry), None),
        Err(e) => (None, Some(e.to_string())),
    };

    let mut app = app::App::new(project_info, cfg, store, agents_md, provider_registry, provider_error);
    app.run().await?;
    Ok(())
}
