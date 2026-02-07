pub mod types;

use std::path::Path;

use anyhow::{Context, Result};

use types::Config;

/// Load configuration from the project root.
/// Looks for `steve.json` or `steve.jsonc` in the given directory.
pub fn load(project_root: &Path) -> Result<Config> {
    // Try steve.json first, then steve.jsonc
    let json_path = project_root.join("steve.json");
    let jsonc_path = project_root.join("steve.jsonc");

    let path = if json_path.exists() {
        json_path
    } else if jsonc_path.exists() {
        jsonc_path
    } else {
        // No config file found — return defaults
        return Ok(Config::default());
    };

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;

    // Always parse through JSONC parser — it handles both plain JSON and JSONC (with comments)
    let json_value = jsonc_parser::parse_to_serde_value(&content, &Default::default())
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;

    let config = match json_value {
        Some(value) => serde_json::from_value(value)
            .with_context(|| format!("failed to deserialize config from {}", path.display()))?,
        None => Config::default(),
    };

    Ok(config)
}

/// Load the AGENTS.md file from the project root, if it exists.
pub fn load_agents_md(project_root: &Path) -> Option<String> {
    let path = project_root.join("AGENTS.md");
    std::fs::read_to_string(path).ok()
}
