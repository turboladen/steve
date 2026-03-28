pub mod types;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use types::Config;

/// Load configuration with global + project merge.
/// Global config at `~/.config/steve/config.jsonc` (XDG-style on all platforms) provides defaults;
/// project-level `.steve.jsonc` overlays on top.
///
/// Returns `(Config, Vec<String>)` — the merged config and any non-fatal warnings
/// (e.g., parse errors from config files that exist but couldn't be loaded).
pub fn load(project_root: &Path) -> Result<(Config, Vec<String>)> {
    let mut warnings = Vec::new();
    let global = load_global(&mut warnings);
    let project = load_project(project_root, &mut warnings)?;

    Ok((global.merge(project), warnings))
}

/// Load global config from `~/.config/steve/config.jsonc`.
/// Returns `Config::default()` if no global config exists.
/// Pushes a warning if the file exists but can't be parsed.
fn load_global(warnings: &mut Vec<String>) -> Config {
    let Some(path) = global_config_path() else {
        tracing::debug!(
            dir = ?global_config_dir(),
            "no global config.jsonc found"
        );
        return Config::default();
    };
    tracing::info!(path = %path.display(), "loading global config");
    match load_jsonc_file(&path) {
        Ok(config) => config,
        Err(e) => {
            let msg = format_config_error(&path, &e);
            tracing::warn!(path = %path.display(), error = ?e, "failed to load global config, using defaults");
            warnings.push(msg);
            Config::default()
        }
    }
}

/// Load project-level config from `.steve.jsonc` in the project root.
/// Pushes a warning if the file exists but can't be parsed.
fn load_project(project_root: &Path, warnings: &mut Vec<String>) -> Result<Config> {
    let path = project_root.join(".steve.jsonc");
    if path.exists() {
        match load_jsonc_file(&path) {
            Ok(config) => Ok(config),
            Err(e) => {
                let msg = format_config_error(&path, &e);
                warnings.push(msg);
                Ok(Config::default())
            }
        }
    } else {
        Ok(Config::default())
    }
}

/// Format a config error into a user-friendly message showing the file path
/// and the root cause (e.g., the specific serde field error).
fn format_config_error(path: &Path, error: &anyhow::Error) -> String {
    // Walk the error chain to find the most specific cause
    let root_cause = error.chain().last().unwrap_or(error.as_ref());
    format!("Config error in {}: {root_cause}", path.display())
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

/// Returns the path to the global `config.jsonc`, if it exists.
fn global_config_path() -> Option<PathBuf> {
    find_global_config_in(None)
}

/// Find the global `config.jsonc` file. Accepts an optional override directory
/// for testing; when `None`, uses `~/.config/steve/`.
fn find_global_config_in(dir_override: Option<&Path>) -> Option<PathBuf> {
    let config_dir = match dir_override {
        Some(d) => d.to_path_buf(),
        None => global_config_dir()?,
    };
    let path = config_dir.join("config.jsonc");
    if path.exists() { Some(path) } else { None }
}

/// Returns `~/.config/steve/` — the global config directory.
/// Uses `$HOME/.config/steve/` directly (XDG-style) on all platforms.
pub fn global_config_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".config").join("steve"))
}

/// Persist a tool name to the project's `.steve.jsonc` `allow_tools` list.
///
/// Reads the existing config (or creates a minimal one), adds the tool
/// to `allow_tools` if not already present, and writes back. Uses JSONC parser
/// for reading but writes clean JSON (comments are not preserved).
pub fn persist_allow_tool(project_root: &Path, tool_name: &str) -> Result<()> {
    let config_path = project_root.join(".steve.jsonc");

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
    let obj = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("config root is not an object"))?;

    let allow_tools = obj
        .entry("allow_tools")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));

    if let serde_json::Value::Array(arr) = allow_tools {
        let tool_val = serde_json::Value::String(tool_name.to_string());
        if !arr.contains(&tool_val) {
            arr.push(tool_val);
        }
    }

    let json_str = serde_json::to_string_pretty(&value).context("failed to serialize config")?;

    // Atomic write: write to uniquely-named tmp file then rename to avoid
    // partial writes on crash and races between concurrent persist calls.
    let tmp_path = config_path.with_extension(format!(
        "jsonc.{}-{:?}.tmp",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&tmp_path, json_str.as_bytes())
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &config_path).with_context(|| {
        format!(
            "failed to rename {} → {}",
            tmp_path.display(),
            config_path.display()
        )
    })?;

    tracing::info!(tool = tool_name, path = %config_path.display(), "persisted tool to allow_tools");
    Ok(())
}

/// Load the AGENTS.md file from the project root, if it exists.
pub fn load_agents_md(project_root: &Path) -> Option<String> {
    let path = project_root.join("AGENTS.md");
    std::fs::read_to_string(path).ok()
}

/// An AGENTS.md file discovered during walk-up discovery.
#[derive(Debug, Clone)]
pub struct AgentsFile {
    /// Absolute path to the AGENTS.md file.
    pub path: PathBuf,
    /// File content.
    pub content: String,
}

/// Walk from `cwd` up to `project_root` (inclusive), collecting AGENTS.md files.
/// Returns them root-first (outermost to innermost / highest to lowest priority).
pub fn load_agents_md_chain(project_root: &Path, cwd: &Path) -> Vec<AgentsFile> {
    let mut files = Vec::new();
    // Guard: if cwd is not under project_root, fall back to project_root
    let effective_cwd = if cwd.starts_with(project_root) {
        cwd
    } else {
        project_root
    };
    let mut dir = effective_cwd.to_path_buf();
    loop {
        let agents_path = dir.join("AGENTS.md");
        if let Ok(content) = std::fs::read_to_string(&agents_path) {
            files.push(AgentsFile {
                path: agents_path,
                content,
            });
        }
        if dir == project_root {
            break;
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
    files.reverse(); // root-first order
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_dotfile_jsonc() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".steve.jsonc"),
            r#"{"model": "openai/gpt-4o", "providers": {}}"#,
        )
        .unwrap();
        let (config, _warnings) = load(dir.path()).unwrap();
        assert_eq!(config.model, Some("openai/gpt-4o".into()));
    }

    #[test]
    fn load_dotfile_jsonc_with_comments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".steve.jsonc"),
            "{\n  // this is a comment\n  \"model\": \"openai/gpt-4o\",\n  \"providers\": {}\n}",
        )
        .unwrap();
        let (config, _warnings) = load(dir.path()).unwrap();
        assert_eq!(config.model, Some("openai/gpt-4o".into()));
    }

    #[test]
    fn no_config_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let (config, _warnings) = load(dir.path()).unwrap();
        let default = Config::default();
        assert_eq!(config.model, default.model);
        assert_eq!(config.auto_compact, default.auto_compact);
    }

    #[test]
    fn invalid_json_returns_warning() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".steve.jsonc"), "{{invalid").unwrap();
        let (config, warnings) = load(dir.path()).unwrap();
        assert!(!warnings.is_empty(), "should have a config warning");
        assert!(
            warnings[0].contains(".steve.jsonc"),
            "warning mentions the file"
        );
        assert_eq!(config.model, None, "falls back to default config");
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
        std::fs::write(dir.path().join(".steve.jsonc"), r#"{"model": "test/m"}"#).unwrap();
        let (config, _warnings) = load(dir.path()).unwrap();
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
        let mut warnings = Vec::new();
        let config = load_project(dir.path(), &mut warnings).unwrap();
        assert_eq!(config.model, None);
        assert!(config.providers.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn global_config_dir_returns_some() {
        // On any platform with a home directory, this should return Some
        let dir = global_config_dir();
        assert!(dir.is_some(), "should resolve a config directory");
    }

    #[test]
    fn global_config_finds_config_jsonc() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.jsonc"),
            r#"{"model": "openai/gpt-4o", "providers": {}}"#,
        )
        .unwrap();

        let found = find_global_config_in(Some(dir.path()));
        assert!(found.is_some(), "should find config.jsonc");
        assert!(found.unwrap().ends_with("config.jsonc"));
    }

    #[test]
    fn global_config_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let found = find_global_config_in(Some(dir.path()));
        assert!(
            found.is_none(),
            "should return None when no config file exists"
        );
    }

    #[test]
    fn global_config_ignores_old_config_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"model": "openai/gpt-4o"}"#,
        )
        .unwrap();
        let found = find_global_config_in(Some(dir.path()));
        assert!(found.is_none(), "config.json (old name) should be ignored");
    }

    #[test]
    fn old_project_filenames_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("steve.json"),
            r#"{"model": "openai/gpt-4o"}"#,
        )
        .unwrap();
        let (config, _warnings) = load(dir.path()).unwrap();
        assert_eq!(config.model, None, "steve.json should not be loaded");

        std::fs::write(
            dir.path().join("steve.jsonc"),
            r#"{"model": "openai/gpt-4o"}"#,
        )
        .unwrap();
        let (config, _warnings) = load(dir.path()).unwrap();
        assert_eq!(config.model, None, "steve.jsonc should not be loaded");
    }

    #[test]
    fn global_merge_with_empty_project() {
        // Simulate: global config has providers, empty project config
        let dir = tempfile::tempdir().unwrap();
        let global_dir = tempfile::tempdir().unwrap();

        // Write global config
        std::fs::write(
            global_dir.path().join("config.jsonc"),
            r#"{"model": "openai/gpt-4o", "providers": {"openai": {"base_url": "https://api.openai.com/v1", "api_key_env": "OPENAI_API_KEY", "models": {}}}}"#,
        ).unwrap();

        // Write empty project config
        std::fs::write(dir.path().join(".steve.jsonc"), "{}").unwrap();

        // Load and verify global providers are preserved
        let global = load_jsonc_file(&global_dir.path().join("config.jsonc")).unwrap();
        let mut warnings = Vec::new();
        let project = load_project(dir.path(), &mut warnings).unwrap();
        let merged = global.merge(project);

        assert_eq!(merged.model, Some("openai/gpt-4o".into()));
        assert!(
            merged.providers.contains_key("openai"),
            "global providers preserved"
        );
    }

    // -- persist_allow_tool tests --

    #[test]
    fn persist_allow_tool_creates_config_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        persist_allow_tool(dir.path(), "edit").unwrap();

        let (config, _warnings) = load(dir.path()).unwrap();
        assert!(config.allow_tools.contains(&"edit".to_string()));
    }

    #[test]
    fn persist_allow_tool_appends_to_existing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".steve.jsonc"),
            r#"{"model": "openai/gpt-4o", "allow_tools": ["bash"]}"#,
        )
        .unwrap();

        persist_allow_tool(dir.path(), "edit").unwrap();

        let (config, _warnings) = load(dir.path()).unwrap();
        assert!(
            config.allow_tools.contains(&"bash".to_string()),
            "existing tool preserved"
        );
        assert!(
            config.allow_tools.contains(&"edit".to_string()),
            "new tool added"
        );
        assert_eq!(
            config.model,
            Some("openai/gpt-4o".into()),
            "other fields preserved"
        );
    }

    #[test]
    fn persist_allow_tool_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".steve.jsonc"),
            r#"{"allow_tools": ["edit"]}"#,
        )
        .unwrap();

        persist_allow_tool(dir.path(), "edit").unwrap();

        let content = std::fs::read_to_string(dir.path().join(".steve.jsonc")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        let arr = value["allow_tools"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "should not duplicate");
    }

    #[test]
    fn persist_allow_tool_writes_to_dotfile() {
        let dir = tempfile::tempdir().unwrap();

        persist_allow_tool(dir.path(), "bash").unwrap();

        // Should create .steve.jsonc
        assert!(dir.path().join(".steve.jsonc").exists());
        let (config, _warnings) = load(dir.path()).unwrap();
        assert!(config.allow_tools.contains(&"bash".to_string()));
    }

    // -- load_agents_md_chain tests --

    #[test]
    fn agents_md_chain_single_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "# Root").unwrap();
        let chain = load_agents_md_chain(dir.path(), dir.path());
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].content, "# Root");
    }

    #[test]
    fn agents_md_chain_nested() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("AGENTS.md"), "# Root").unwrap();
        let sub = root.join("sub").join("dir");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("AGENTS.md"), "# Sub").unwrap();

        let chain = load_agents_md_chain(root, &sub);
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].content, "# Root", "root-first order");
        assert_eq!(chain[1].content, "# Sub", "subdirectory last");
    }

    #[test]
    fn agents_md_chain_middle_missing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("AGENTS.md"), "# Root").unwrap();
        let deep = root.join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("AGENTS.md"), "# Deep").unwrap();

        let chain = load_agents_md_chain(root, &deep);
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].content, "# Root");
        assert_eq!(chain[1].content, "# Deep");
    }

    #[test]
    fn agents_md_chain_none() {
        let dir = tempfile::tempdir().unwrap();
        let chain = load_agents_md_chain(dir.path(), dir.path());
        assert!(chain.is_empty());
    }

    #[test]
    fn agents_md_chain_cwd_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let sub = root.join("pkg");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("AGENTS.md"), "# Pkg only").unwrap();

        let chain = load_agents_md_chain(root, &sub);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].content, "# Pkg only");
    }

    #[test]
    fn agents_md_chain_cwd_outside_root_falls_back() {
        let root_dir = tempfile::tempdir().unwrap();
        let other_dir = tempfile::tempdir().unwrap();
        std::fs::write(root_dir.path().join("AGENTS.md"), "# Root").unwrap();
        std::fs::write(other_dir.path().join("AGENTS.md"), "# Other").unwrap();

        // cwd is not under project_root — should fall back to project_root
        let chain = load_agents_md_chain(root_dir.path(), other_dir.path());
        assert_eq!(chain.len(), 1);
        assert_eq!(
            chain[0].content, "# Root",
            "should only find root, not the unrelated dir"
        );
    }
}
