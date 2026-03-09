pub mod types;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use types::Config;

/// Load configuration with global + project merge.
/// Global config at `~/.config/steve/config.json` (or `.jsonc`) provides defaults;
/// project-level `steve.json` overlays on top.
pub fn load(project_root: &Path) -> Result<Config> {
    let global = load_global();
    let project = load_project(project_root)?;

    Ok(global.merge(project))
}

/// Load global config from the platform config directory.
/// Returns `Config::default()` if no global config exists.
fn load_global() -> Config {
    let Some(path) = global_config_path() else {
        return Config::default();
    };
    load_jsonc_file(&path).unwrap_or_default()
}

/// Load project-level config from the project root.
/// Looks for `steve.json` or `steve.jsonc`.
fn load_project(project_root: &Path) -> Result<Config> {
    let json_path = project_root.join("steve.json");
    let jsonc_path = project_root.join("steve.jsonc");

    let path = if json_path.exists() {
        json_path
    } else if jsonc_path.exists() {
        jsonc_path
    } else {
        return Ok(Config::default());
    };

    load_jsonc_file(&path)
}

/// Parse a JSONC file into a Config. Works for both `.json` and `.jsonc`.
fn load_jsonc_file(path: &Path) -> Result<Config> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;

    let json_value = jsonc_parser::parse_to_serde_value(&content, &Default::default())
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;

    match json_value {
        Some(value) => serde_json::from_value(value)
            .with_context(|| format!("failed to deserialize config from {}", path.display())),
        None => Ok(Config::default()),
    }
}

/// Returns the path to the global config file, if the config directory can be determined.
/// Checks for `config.json` first, then `config.jsonc`.
fn global_config_path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "steve")?;
    let config_dir = dirs.config_dir();
    let json_path = config_dir.join("config.json");
    if json_path.exists() {
        return Some(json_path);
    }
    let jsonc_path = config_dir.join("config.jsonc");
    if jsonc_path.exists() {
        return Some(jsonc_path);
    }
    None
}

/// Exposed for logging/diagnostics — returns the global config directory path.
pub fn global_config_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "steve")
        .map(|d| d.config_dir().to_path_buf())
}

/// Persist a tool name to the project config's `allow_tools` list.
///
/// Reads the existing project config (or creates a minimal one), adds the tool
/// to `allow_tools` if not already present, and writes back. Uses JSONC parser
/// for reading but writes clean JSON (comments are not preserved).
pub fn persist_allow_tool(project_root: &Path, tool_name: &str) -> Result<()> {
    let json_path = project_root.join("steve.json");
    let jsonc_path = project_root.join("steve.jsonc");

    // Determine which config file to use (prefer existing, default to steve.json)
    let config_path = if json_path.exists() {
        json_path.clone()
    } else if jsonc_path.exists() {
        jsonc_path.clone()
    } else {
        json_path.clone() // Create steve.json if nothing exists
    };

    // Load existing config as a serde_json::Value to preserve all fields
    let mut value: serde_json::Value = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)
            .with_context(|| format!("failed to read {}", config_path.display()))?;
        let parsed = jsonc_parser::parse_to_serde_value(&content, &Default::default())
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", config_path.display()))?;
        parsed.unwrap_or(serde_json::Value::Object(serde_json::Map::new()))
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };

    // Ensure allow_tools array exists and add the tool if not present
    let obj = value.as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("config root is not an object"))?;

    let allow_tools = obj.entry("allow_tools")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));

    if let serde_json::Value::Array(arr) = allow_tools {
        let tool_val = serde_json::Value::String(tool_name.to_string());
        if !arr.contains(&tool_val) {
            arr.push(tool_val);
        }
    }

    // Write back as formatted JSON
    // If the original was .jsonc, we write to .json instead (can't preserve comments)
    let write_path = if config_path.extension().is_some_and(|ext| ext == "jsonc") && !json_path.exists() {
        // Writing to .jsonc would lose comments; write to .json instead
        // The .jsonc still exists but .json takes priority on next load
        json_path
    } else {
        config_path
    };

    let json_str = serde_json::to_string_pretty(&value)
        .context("failed to serialize config")?;

    // Atomic write: write to tmp file then rename to avoid partial writes on crash
    let tmp_path = write_path.with_extension("json.tmp");
    std::fs::write(&tmp_path, json_str.as_bytes())
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &write_path)
        .with_context(|| format!("failed to rename {} → {}", tmp_path.display(), write_path.display()))?;

    tracing::info!(tool = tool_name, path = %write_path.display(), "persisted tool to allow_tools");
    Ok(())
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

    #[test]
    fn load_jsonc_file_parses_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.json");
        std::fs::write(&path, r#"{"model": "test/m"}"#).unwrap();
        let config = load_jsonc_file(&path).unwrap();
        assert_eq!(config.model, Some("test/m".into()));
    }

    #[test]
    fn load_jsonc_file_parses_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonc");
        std::fs::write(&path, "{\n  // comment\n  \"model\": \"test/m\"\n}").unwrap();
        let config = load_jsonc_file(&path).unwrap();
        assert_eq!(config.model, Some("test/m".into()));
    }

    #[test]
    fn load_jsonc_file_missing_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        assert!(load_jsonc_file(&path).is_err());
    }

    #[test]
    fn load_project_no_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let config = load_project(dir.path()).unwrap();
        assert_eq!(config.model, None);
        assert!(config.providers.is_empty());
    }

    #[test]
    fn global_config_dir_returns_some() {
        // On any platform with a home directory, this should return Some
        let dir = global_config_dir();
        assert!(dir.is_some(), "should resolve a config directory");
    }

    // -- persist_allow_tool tests --

    #[test]
    fn persist_allow_tool_creates_config_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        persist_allow_tool(dir.path(), "edit").unwrap();

        let config = load(dir.path()).unwrap();
        assert!(config.allow_tools.contains(&"edit".to_string()));
    }

    #[test]
    fn persist_allow_tool_appends_to_existing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("steve.json"),
            r#"{"model": "openai/gpt-4o", "allow_tools": ["bash"]}"#,
        ).unwrap();

        persist_allow_tool(dir.path(), "edit").unwrap();

        let config = load(dir.path()).unwrap();
        assert!(config.allow_tools.contains(&"bash".to_string()), "existing tool preserved");
        assert!(config.allow_tools.contains(&"edit".to_string()), "new tool added");
        assert_eq!(config.model, Some("openai/gpt-4o".into()), "other fields preserved");
    }

    #[test]
    fn persist_allow_tool_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("steve.json"),
            r#"{"allow_tools": ["edit"]}"#,
        ).unwrap();

        persist_allow_tool(dir.path(), "edit").unwrap();

        let content = std::fs::read_to_string(dir.path().join("steve.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        let arr = value["allow_tools"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "should not duplicate");
    }

    #[test]
    fn persist_allow_tool_with_jsonc_creates_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("steve.jsonc"),
            "{\n  // comment\n  \"model\": \"test/m\"\n}",
        ).unwrap();

        persist_allow_tool(dir.path(), "bash").unwrap();

        // Should create steve.json (since .jsonc comments can't be preserved)
        assert!(dir.path().join("steve.json").exists());
        let config = load(dir.path()).unwrap();
        assert!(config.allow_tools.contains(&"bash".to_string()));
    }
}
