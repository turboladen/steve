//! Integration tests for the permission system.
//!
//! Tests path-based rules, persistent grants, and profile behavior
//! working together across modules.

use std::str::FromStr;

use strum::IntoEnumIterator;
use tempfile::tempdir;

use steve::permission::types::{
    PermissionAction, PermissionActionSerde, PermissionRule, ToolMatcher,
};
use steve::permission::{PermissionEngine, PermissionProfile, profile_build_rules, profile_plan_rules};
use steve::tool::ToolName;

/// Verify that path-based rules, allow_tools overrides, and profile defaults
/// compose correctly in priority order.
#[test]
fn full_permission_stack_priority() {
    // Path rule: allow edit in src/
    // Override: allow bash everywhere
    // Profile: standard (edit=Ask, bash=Ask by default)
    let path_rules = vec![PermissionRule {
        tool: ToolMatcher::Specific(ToolName::Edit),
        pattern: "src/**".into(),
        action: PermissionActionSerde::Allow,
    }];

    let engine = PermissionEngine::new(profile_build_rules(
        PermissionProfile::Standard,
        &[ToolName::Bash],
        &path_rules,
    ));

    // Path rule: edit in src/ is allowed
    assert_eq!(
        engine.check(ToolName::Edit, Some("src/main.rs")),
        PermissionAction::Allow,
        "path rule should allow edit in src/"
    );

    // No path match: edit outside src/ falls through to profile default
    assert_eq!(
        engine.check(ToolName::Edit, Some("Cargo.toml")),
        PermissionAction::Ask,
        "edit outside src/ should require permission"
    );

    // Override: bash is allowed everywhere
    assert_eq!(
        engine.check(ToolName::Bash, None),
        PermissionAction::Allow,
        "bash override should auto-allow"
    );

    // Profile default: write tool with no override
    assert_eq!(
        engine.check(ToolName::Write, None),
        PermissionAction::Ask,
        "write should require permission from profile default"
    );

    // Profile default: read tools auto-allowed
    assert_eq!(
        engine.check(ToolName::Read, Some("anything")),
        PermissionAction::Allow,
        "read should be auto-allowed from profile"
    );
}

/// Verify that session grants override even path-based deny rules.
#[test]
fn session_grant_overrides_all_rules() {
    let path_rules = vec![PermissionRule {
        tool: ToolMatcher::Specific(ToolName::Edit),
        pattern: "/etc/**".into(),
        action: PermissionActionSerde::Deny,
    }];

    let mut engine = PermissionEngine::new(profile_build_rules(
        PermissionProfile::Standard,
        &[],
        &path_rules,
    ));

    // Before grant: /etc/ edits are denied
    assert_eq!(
        engine.check(ToolName::Edit, Some("/etc/passwd")),
        PermissionAction::Deny,
    );

    // After session grant: overrides everything
    engine.grant_session(ToolName::Edit);
    assert_eq!(
        engine.check(ToolName::Edit, Some("/etc/passwd")),
        PermissionAction::Allow,
    );

    // Grant persists across mode switch
    engine.set_rules(profile_plan_rules(
        PermissionProfile::Standard,
        &[],
        &path_rules,
    ));
    assert_eq!(
        engine.check(ToolName::Edit, Some("/etc/passwd")),
        PermissionAction::Allow,
        "session grant should persist across mode switch"
    );
}

/// Verify Plan mode correctly strips write-allow rules but keeps deny rules.
#[test]
fn plan_mode_path_rule_filtering() {
    let path_rules = vec![
        // This should be stripped (allow write in Plan mode)
        PermissionRule {
            tool: ToolMatcher::Specific(ToolName::Edit),
            pattern: "src/**".into(),
            action: PermissionActionSerde::Allow,
        },
        // This should be kept (deny is still useful in Plan mode)
        PermissionRule {
            tool: ToolMatcher::Specific(ToolName::Edit),
            pattern: "/etc/**".into(),
            action: PermissionActionSerde::Deny,
        },
    ];

    let engine = PermissionEngine::new(profile_plan_rules(
        PermissionProfile::Standard,
        &[],
        &path_rules,
    ));

    // Allow rule was stripped — edit in src/ is denied (plan mode default)
    assert_eq!(
        engine.check(ToolName::Edit, Some("src/main.rs")),
        PermissionAction::Deny,
        "write-allow path rules stripped in Plan mode"
    );

    // Deny rule kept — /etc/ edits are still explicitly denied
    assert_eq!(
        engine.check(ToolName::Edit, Some("/etc/passwd")),
        PermissionAction::Deny,
        "deny path rules should persist in Plan mode"
    );

    // Read tools still allowed in Plan mode
    assert_eq!(
        engine.check(ToolName::Read, Some("src/main.rs")),
        PermissionAction::Allow,
    );
}

/// Verify that tool name exhaustive coverage works with all variants.
#[test]
fn all_tool_names_have_defined_permission_behavior() {
    let engine = PermissionEngine::new(profile_build_rules(
        PermissionProfile::Standard,
        &[],
        &[],
    ));

    for tool in ToolName::iter() {
        let action = engine.check(tool, None);
        // Every tool should have a defined action (not panic)
        match action {
            PermissionAction::Allow | PermissionAction::Ask | PermissionAction::Deny => {}
        }
    }
}

/// Exhaustively verify every ToolName × Profile combination in Build mode.
///
/// Uses if/else if/else on tool predicates so that adding a new ToolName variant
/// will fail loudly (it won't match any predicate and hit the else branch with
/// a wrong assertion) rather than silently passing via a wildcard.
#[test]
fn all_profiles_exhaustive_build_mode() {
    let profiles = [
        PermissionProfile::Trust,
        PermissionProfile::Standard,
        PermissionProfile::Cautious,
    ];

    for profile in profiles {
        let engine = PermissionEngine::new(profile_build_rules(profile, &[], &[]));

        for tool in ToolName::iter() {
            let action = engine.check(tool, None);

            match profile {
                PermissionProfile::Trust => {
                    assert_eq!(
                        action,
                        PermissionAction::Allow,
                        "Trust profile: {tool} should be Allow"
                    );
                }
                PermissionProfile::Standard => {
                    if tool.is_read_only()
                        || tool == ToolName::Lsp
                        || tool == ToolName::Memory
                        || tool == ToolName::Task
                        || tool == ToolName::Question
                    {
                        assert_eq!(
                            action,
                            PermissionAction::Allow,
                            "Standard profile: {tool} should be Allow"
                        );
                    } else if tool.is_write_tool()
                        || tool == ToolName::Bash
                        || tool == ToolName::Webfetch
                        || tool == ToolName::Agent
                    {
                        assert_eq!(
                            action,
                            PermissionAction::Ask,
                            "Standard profile: {tool} should be Ask"
                        );
                    } else {
                        panic!("Standard profile: unclassified tool {tool} — update this test");
                    }
                }
                PermissionProfile::Cautious => {
                    if tool == ToolName::Question || tool == ToolName::Task {
                        assert_eq!(
                            action,
                            PermissionAction::Allow,
                            "Cautious profile: {tool} should be Allow"
                        );
                    } else {
                        assert_eq!(
                            action,
                            PermissionAction::Ask,
                            "Cautious profile: {tool} should be Ask"
                        );
                    }
                }
            }
        }
    }
}

/// Verify that Plan mode denies writes and Agent regardless of which profile is passed.
///
/// `profile_plan_rules` ignores the profile parameter — this test documents that contract.
/// All three profiles produce identical Plan mode behavior.
#[test]
fn plan_mode_denies_writes_regardless_of_profile() {
    let profiles = [
        PermissionProfile::Trust,
        PermissionProfile::Standard,
        PermissionProfile::Cautious,
    ];

    for profile in profiles {
        let engine = PermissionEngine::new(profile_plan_rules(profile, &[], &[]));

        for tool in ToolName::iter() {
            let action = engine.check(tool, None);

            if tool.is_write_tool() || tool == ToolName::Agent {
                assert_eq!(
                    action,
                    PermissionAction::Deny,
                    "Plan mode ({profile}): {tool} should be Deny"
                );
            } else if tool.is_read_only()
                || tool == ToolName::Lsp
                || tool == ToolName::Memory
                || tool == ToolName::Task
                || tool == ToolName::Question
            {
                assert_eq!(
                    action,
                    PermissionAction::Allow,
                    "Plan mode ({profile}): {tool} should be Allow"
                );
            } else if tool == ToolName::Bash || tool == ToolName::Webfetch {
                assert_eq!(
                    action,
                    PermissionAction::Ask,
                    "Plan mode ({profile}): {tool} should be Ask"
                );
            } else {
                panic!("Plan mode ({profile}): unclassified tool {tool} — update this test");
            }
        }
    }
}

/// Verify that write-tool and Agent overrides are stripped in Plan mode but other overrides are kept.
#[test]
fn allow_overrides_stripped_for_write_tools_in_plan_mode() {
    let engine = PermissionEngine::new(profile_plan_rules(
        PermissionProfile::Standard,
        &[ToolName::Edit, ToolName::Write, ToolName::Bash, ToolName::Agent],
        &[],
    ));

    // Write tool overrides are stripped — still denied
    assert_eq!(
        engine.check(ToolName::Edit, None),
        PermissionAction::Deny,
        "Edit override should be stripped in Plan mode"
    );
    assert_eq!(
        engine.check(ToolName::Write, None),
        PermissionAction::Deny,
        "Write override should be stripped in Plan mode"
    );

    // Agent override is also stripped — Agent is denied in Plan mode
    assert_eq!(
        engine.check(ToolName::Agent, None),
        PermissionAction::Deny,
        "Agent override should be stripped in Plan mode"
    );

    // Non-write, non-Agent override is kept — Bash goes from Ask to Allow
    assert_eq!(
        engine.check(ToolName::Bash, None),
        PermissionAction::Allow,
        "Bash override should be kept in Plan mode"
    );
}

/// End-to-end: persist a tool grant to config, reload, build a PermissionEngine, verify behavior.
#[test]
fn persisted_config_builds_working_engine() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(".steve.jsonc"), "{}").unwrap();

    // Persist "edit" as an allowed tool
    steve::config::persist_allow_tool(dir.path(), "edit").unwrap();

    // Reload config and parse allow_tools into ToolName values
    let (config, _warnings) = steve::config::load(dir.path()).unwrap();
    let overrides: Vec<ToolName> = config
        .allow_tools
        .iter()
        .filter_map(|s| ToolName::from_str(s).ok())
        .collect();

    // Build engine with Standard profile + the persisted overrides
    let engine = PermissionEngine::new(profile_build_rules(
        PermissionProfile::Standard,
        &overrides,
        &config.permission_rules,
    ));

    // Edit should be auto-allowed (from persisted grant)
    assert_eq!(
        engine.check(ToolName::Edit, None),
        PermissionAction::Allow,
        "persisted edit grant should auto-allow"
    );

    // Write has no grant — still requires permission
    assert_eq!(
        engine.check(ToolName::Write, None),
        PermissionAction::Ask,
        "write with no grant should still ask"
    );
}
