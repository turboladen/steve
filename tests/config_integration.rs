//! Integration tests for the configuration system.
//!
//! Tests config loading, merging, and persistent permission grants
//! working end-to-end with the filesystem.

use steve::{
    config,
    config::types::Config,
    permission::{
        PermissionProfile,
        types::{PermissionActionSerde, PermissionRule, ToolMatcher},
    },
    tool::ToolName,
};
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
    std::fs::write(dir.path().join(".steve.jsonc"), config_json).unwrap();

    let (config, _warnings) = config::load(dir.path()).unwrap();
    assert_eq!(config.permission_rules.len(), 2);
    assert_eq!(config.permission_rules[0].pattern, "src/**");
    assert_eq!(config.permission_rules[1].pattern, "/etc/**");
}

/// Persist a tool grant, then reload and verify it's in allow_tools.
#[test]
fn persist_and_reload_tool_grant() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join(".steve.jsonc"),
        r#"{"model": "openai/gpt-4o", "providers": {}}"#,
    )
    .unwrap();

    // Persist a grant
    config::persist_allow_tool(dir.path(), "edit").unwrap();

    // Reload and verify
    let (config, _warnings) = config::load(dir.path()).unwrap();
    assert!(config.allow_tools.contains(&"edit".to_string()));
    assert_eq!(
        config.model,
        Some("openai/gpt-4o".into()),
        "model preserved"
    );
}

/// Persisting multiple grants accumulates in the array.
#[test]
fn persist_multiple_grants_accumulates() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(".steve.jsonc"), "{}").unwrap();

    config::persist_allow_tool(dir.path(), "edit").unwrap();
    config::persist_allow_tool(dir.path(), "bash").unwrap();
    config::persist_allow_tool(dir.path(), "write").unwrap();

    let (config, _warnings) = config::load(dir.path()).unwrap();
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
        dir.path().join(".steve.jsonc"),
        r#"{"model": "openai/gpt-4o"}"#,
    )
    .unwrap();

    let (config, _warnings) = config::load(dir.path()).unwrap();
    assert!(config.permission_rules.is_empty());
}

/// Persisting the same tool twice should not create duplicates.
#[test]
fn duplicate_persist_is_idempotent() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(".steve.jsonc"), "{}").unwrap();

    config::persist_allow_tool(dir.path(), "edit").unwrap();
    config::persist_allow_tool(dir.path(), "edit").unwrap();

    let (config, _warnings) = config::load(dir.path()).unwrap();
    assert_eq!(
        config.allow_tools.len(),
        1,
        "duplicate persist should be idempotent"
    );
}

/// Project permission_profile overrides global; None preserves global.
#[test]
fn merge_permission_profile_override() {
    let global = Config {
        permission_profile: Some(PermissionProfile::Trust),
        ..Default::default()
    };

    // Project with Cautious overrides global Trust
    let project = Config {
        permission_profile: Some(PermissionProfile::Cautious),
        ..Default::default()
    };
    let merged = global.clone().merge(project);
    assert_eq!(
        merged.permission_profile,
        Some(PermissionProfile::Cautious),
        "project Cautious should override global Trust"
    );

    // Project with None preserves global Trust
    let project_none = Config {
        permission_profile: None,
        ..Default::default()
    };
    let merged = global.merge(project_none);
    assert_eq!(
        merged.permission_profile,
        Some(PermissionProfile::Trust),
        "None project should preserve global Trust"
    );
}

/// Project allow_tools replaces (not appends to) global; empty preserves global.
#[test]
fn merge_allow_tools_project_replaces_global() {
    let global = Config {
        allow_tools: vec!["bash".to_string(), "write".to_string()],
        ..Default::default()
    };

    // Non-empty project replaces global entirely (not appended)
    let project = Config {
        allow_tools: vec!["edit".to_string()],
        ..Default::default()
    };
    let merged = global.clone().merge(project);
    assert_eq!(
        merged.allow_tools,
        vec!["edit".to_string()],
        "project should replace global (2 global entries reduced to 1)"
    );

    // Empty project preserves global
    let project_empty = Config {
        allow_tools: vec![],
        ..Default::default()
    };
    let merged = global.merge(project_empty);
    assert_eq!(
        merged.allow_tools,
        vec!["bash".to_string(), "write".to_string()],
        "empty project should preserve global"
    );
}
