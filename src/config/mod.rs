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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_steve_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("steve.json"),
            r#"{"model": "openai/gpt-4o", "providers": {}}"#,
        )
        .unwrap();
        let config = load(dir.path()).unwrap();
        assert_eq!(config.model, Some("openai/gpt-4o".into()));
    }

    #[test]
    fn load_steve_jsonc_with_comments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("steve.jsonc"),
            "{\n  // this is a comment\n  \"model\": \"openai/gpt-4o\",\n  \"providers\": {}\n}",
        )
        .unwrap();
        let config = load(dir.path()).unwrap();
        assert_eq!(config.model, Some("openai/gpt-4o".into()));
    }

    #[test]
    fn json_takes_priority_over_jsonc() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("steve.json"),
            r#"{"model": "openai/json-wins", "providers": {}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("steve.jsonc"),
            r#"{"model": "openai/jsonc-loses", "providers": {}}"#,
        )
        .unwrap();
        let config = load(dir.path()).unwrap();
        assert_eq!(config.model, Some("openai/json-wins".into()));
    }

    #[test]
    fn no_config_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let config = load(dir.path()).unwrap();
        let default = Config::default();
        assert_eq!(config.model, default.model);
        assert_eq!(config.auto_compact, default.auto_compact);
    }

    #[test]
    fn invalid_json_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("steve.json"), "{{invalid").unwrap();
        assert!(load(dir.path()).is_err());
    }

    #[test]
    fn agents_md_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "# My Agent\nHello").unwrap();
        let content = load_agents_md(dir.path());
        assert_eq!(content, Some("# My Agent\nHello".into()));
    }

    #[test]
    fn agents_md_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_agents_md(dir.path()).is_none());
    }

    #[test]
    fn partial_config_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("steve.json"),
            r#"{"model": "test/m"}"#,
        )
        .unwrap();
        let config = load(dir.path()).unwrap();
        assert_eq!(config.model, Some("test/m".into()));
        assert!(config.auto_compact);
    }
}
