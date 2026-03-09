//! Integration tests for the permission system.
//!
//! Tests path-based rules, persistent grants, and profile behavior
//! working together across modules.

use steve::config::types::Config;
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
    use strum::IntoEnumIterator;

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
