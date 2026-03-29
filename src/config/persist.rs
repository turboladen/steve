use std::path::Path;

use anyhow::{Context, Result};

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

    if let Some(existing) = obj.get("allow_tools")
        && !existing.is_array()
    {
        anyhow::bail!(
            "config {} has non-array allow_tools; expected an array",
            config_path.display()
        );
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load;

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

    #[test]
    fn persist_allow_tool_errors_on_non_array_allow_tools() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".steve.jsonc"),
            r#"{"allow_tools": "edit"}"#,
        )
        .unwrap();

        let err = persist_allow_tool(dir.path(), "bash").unwrap_err();
        assert!(
            err.to_string().contains("non-array allow_tools"),
            "expected non-array error, got: {err}"
        );
    }
}
