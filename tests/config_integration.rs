//! Integration tests for the configuration system.
//!
//! Tests config loading, merging, and persistent permission grants
//! working end-to-end with the filesystem.

use steve::config;
use steve::config::types::Config;
use steve::permission::types::{PermissionActionSerde, PermissionRule, ToolMatcher};
use steve::tool::ToolName;
use tempfile::tempdir;

/// Config with path-based permission rules round-trips through load/save.
#[test]
fn permission_rules_config_round_trip() {
    let dir = tempdir().unwrap();
    let config_json = r#"{
        "model": "openai/gpt-4o",
        "permission_rules": [
            {"tool": "edit", "pattern": "src/**", "action": "allow"},
            {"tool": "edit", "pattern": "/etc/**", "action": "deny"}
        ],
        "providers": {}
    }"#;
    std::fs::write(dir.path().join("steve.json"), config_json).unwrap();

    let config = config::load(dir.path()).unwrap();
    assert_eq!(config.permission_rules.len(), 2);
    assert_eq!(config.permission_rules[0].pattern, "src/**");
    assert_eq!(config.permission_rules[1].pattern, "/etc/**");
}

/// Persist a tool grant, then reload and verify it's in allow_tools.
#[test]
fn persist_and_reload_tool_grant() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("steve.json"),
        r#"{"model": "openai/gpt-4o", "providers": {}}"#,
    ).unwrap();

    // Persist a grant
    config::persist_allow_tool(dir.path(), "edit").unwrap();

    // Reload and verify
    let config = config::load(dir.path()).unwrap();
    assert!(config.allow_tools.contains(&"edit".to_string()));
    assert_eq!(config.model, Some("openai/gpt-4o".into()), "model preserved");
}

/// Persisting multiple grants accumulates in the array.
#[test]
fn persist_multiple_grants_accumulates() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("steve.json"), "{}").unwrap();

    config::persist_allow_tool(dir.path(), "edit").unwrap();
    config::persist_allow_tool(dir.path(), "bash").unwrap();
    config::persist_allow_tool(dir.path(), "write").unwrap();

    let config = config::load(dir.path()).unwrap();
    assert_eq!(config.allow_tools.len(), 3);
    assert!(config.allow_tools.contains(&"edit".to_string()));
    assert!(config.allow_tools.contains(&"bash".to_string()));
    assert!(config.allow_tools.contains(&"write".to_string()));
}

/// Merging global + project configs with permission rules.
#[test]
fn merge_preserves_path_rules() {
    let global = Config {
        permission_rules: vec![PermissionRule {
            tool: ToolMatcher::Specific(ToolName::Edit),
            pattern: "global/**".into(),
            action: PermissionActionSerde::Allow,
        }],
        ..Default::default()
    };

    // Project with no rules — should keep global rules
    let project_empty = Config::default();
    let merged = global.clone().merge(project_empty);
    assert_eq!(merged.permission_rules.len(), 1);
    assert_eq!(merged.permission_rules[0].pattern, "global/**");

    // Project with rules — should override global
    let project_with_rules = Config {
        permission_rules: vec![PermissionRule {
            tool: ToolMatcher::Specific(ToolName::Edit),
            pattern: "project/**".into(),
            action: PermissionActionSerde::Deny,
        }],
        ..Default::default()
    };
    let merged = global.merge(project_with_rules);
    assert_eq!(merged.permission_rules.len(), 1);
    assert_eq!(merged.permission_rules[0].pattern, "project/**");
}

/// Config without permission_rules field loads with empty vec (default).
#[test]
fn missing_permission_rules_defaults_to_empty() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("steve.json"),
        r#"{"model": "openai/gpt-4o"}"#,
    ).unwrap();

    let config = config::load(dir.path()).unwrap();
    assert!(config.permission_rules.is_empty());
}
