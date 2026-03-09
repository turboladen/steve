pub mod types;

use std::collections::HashSet;

use crate::tool::ToolName;
use types::{PermissionAction, PermissionRule};
use types::ToolMatcher;

/// Evaluates permission rules for tool calls.
pub struct PermissionEngine {
    /// Static rules (from agent mode configuration).
    rules: Vec<PermissionRule>,
    /// Tools that have been granted "always allow" for this session.
    session_grants: HashSet<ToolName>,
}

impl PermissionEngine {
    pub fn new(rules: Vec<PermissionRule>) -> Self {
        Self {
            rules,
            session_grants: HashSet::new(),
        }
    }

    /// Check whether a tool call should be allowed, denied, or needs user approval.
    pub fn check(&self, tool_name: ToolName, _path_hint: Option<&str>) -> PermissionAction {
        // If there's a session-level grant, allow immediately
        if self.session_grants.contains(&tool_name) {
            return PermissionAction::Allow;
        }

        // Find the first matching rule for this tool
        for rule in &self.rules {
            if rule.tool.matches(tool_name) {
                return rule.action.clone().into();
            }
        }

        // Default: ask for permission
        PermissionAction::Ask
    }

    /// Grant "always allow" for a specific tool for the rest of this session.
    pub fn grant_session(&mut self, tool_name: ToolName) {
        self.session_grants.insert(tool_name);
    }

    /// Check if a tool should be completely excluded from the LLM's available tools.
    /// Tools with Deny on all patterns should not be sent to the LLM at all.
    pub fn is_tool_denied(&self, tool_name: ToolName) -> bool {
        for rule in &self.rules {
            if rule.tool.matches(tool_name) {
                return matches!(
                    PermissionAction::from(rule.action.clone()),
                    PermissionAction::Deny
                );
            }
        }
        false
    }

    /// Update the ruleset (e.g., when switching agent modes).
    pub fn set_rules(&mut self, rules: Vec<PermissionRule>) {
        self.rules = rules;
        // Don't clear session grants — they persist across mode changes
    }
}

/// Named permission profiles that define base tool permission behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PermissionProfile {
    /// All tools auto-allowed. Lean on git for recovery.
    Trust,
    /// Reads auto-allowed, writes/bash require permission. (Default)
    #[default]
    Standard,
    /// All tools require permission.
    Cautious,
}

impl std::str::FromStr for PermissionProfile {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "trust" => Ok(PermissionProfile::Trust),
            "standard" => Ok(PermissionProfile::Standard),
            "cautious" => Ok(PermissionProfile::Cautious),
            _ => Err(format!("unknown permission profile: '{s}' (expected: trust, standard, cautious)")),
        }
    }
}

impl std::fmt::Display for PermissionProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PermissionProfile::Trust => write!(f, "trust"),
            PermissionProfile::Standard => write!(f, "standard"),
            PermissionProfile::Cautious => write!(f, "cautious"),
        }
    }
}

impl serde::Serialize for PermissionProfile {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for PermissionProfile {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Build Build-mode permission rules from a profile, with optional per-tool overrides.
///
/// `allow_overrides` are tool names that should be auto-allowed regardless of profile.
/// These are prepended as `Allow` rules (first-match wins).
pub fn profile_build_rules(profile: PermissionProfile, allow_overrides: &[ToolName]) -> Vec<PermissionRule> {
    let mut rules = Vec::new();

    // Per-tool overrides come first (highest priority)
    for &tool in allow_overrides {
        rules.push(PermissionRule {
            tool: ToolMatcher::Specific(tool),
            pattern: "*".into(),
            action: types::PermissionActionSerde::Allow,
        });
    }

    // Then the profile's base rules
    rules.extend(match profile {
        PermissionProfile::Trust => trust_build_rules(),
        PermissionProfile::Standard => build_mode_rules(),
        PermissionProfile::Cautious => cautious_build_rules(),
    });

    rules
}

/// Build Plan-mode permission rules from a profile, with optional per-tool overrides.
///
/// Plan mode always denies write tools regardless of profile or overrides.
pub fn profile_plan_rules(profile: PermissionProfile, allow_overrides: &[ToolName]) -> Vec<PermissionRule> {
    let mut rules = Vec::new();

    // Per-tool overrides, but only for non-write tools in Plan mode
    // (write tools are always denied in Plan mode)
    for &tool in allow_overrides {
        if !tool.is_write_tool() {
            rules.push(PermissionRule {
                tool: ToolMatcher::Specific(tool),
                pattern: "*".into(),
                action: types::PermissionActionSerde::Allow,
            });
        }
    }

    // Plan mode rules are always the same base (writes denied)
    rules.extend(plan_mode_rules());

    rules
}

/// Trust profile: all tools auto-allowed.
fn trust_build_rules() -> Vec<PermissionRule> {
    use types::PermissionActionSerde::*;
    vec![
        PermissionRule { tool: ToolMatcher::All, pattern: "*".into(), action: Allow },
    ]
}

/// Cautious profile: everything requires permission except question/todo.
fn cautious_build_rules() -> Vec<PermissionRule> {
    use crate::tool::ToolName;
    use types::PermissionActionSerde::*;

    vec![
        // Only question and todo are auto-allowed (no filesystem effects)
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Question), pattern: "*".into(), action: Allow },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Todo), pattern: "*".into(), action: Allow },
        // Everything else requires permission
        PermissionRule { tool: ToolMatcher::All, pattern: "*".into(), action: Ask },
    ]
}

/// Build the default Build mode permission rules (Standard profile).
pub fn build_mode_rules() -> Vec<PermissionRule> {
    use crate::tool::ToolName;
    use types::PermissionActionSerde::*;

    vec![
        // Read-only tools: always allowed
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Read), pattern: "*".into(), action: Allow },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Grep), pattern: "*".into(), action: Allow },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Glob), pattern: "*".into(), action: Allow },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::List), pattern: "*".into(), action: Allow },
        // Utility tools: always allowed (no filesystem side effects)
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Memory), pattern: "*".into(), action: Allow },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Todo), pattern: "*".into(), action: Allow },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Question), pattern: "*".into(), action: Allow },
        // Write/execute tools: require permission
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Edit), pattern: "*".into(), action: Ask },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Write), pattern: "*".into(), action: Ask },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Patch), pattern: "*".into(), action: Ask },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Move), pattern: "*".into(), action: Ask },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Copy), pattern: "*".into(), action: Ask },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Delete), pattern: "*".into(), action: Ask },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Mkdir), pattern: "*".into(), action: Ask },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Bash), pattern: "*".into(), action: Ask },
    ]
}

/// Build the Plan mode permission rules (read-only, no writes).
pub fn plan_mode_rules() -> Vec<PermissionRule> {
    use crate::tool::ToolName;
    use types::PermissionActionSerde::*;

    vec![
        // Read-only tools: always allowed
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Read), pattern: "*".into(), action: Allow },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Grep), pattern: "*".into(), action: Allow },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Glob), pattern: "*".into(), action: Allow },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::List), pattern: "*".into(), action: Allow },
        // Utility tools: always allowed (even in Plan mode)
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Memory), pattern: "*".into(), action: Allow },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Todo), pattern: "*".into(), action: Allow },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Question), pattern: "*".into(), action: Allow },
        // Write/execute tools: denied in Plan mode
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Edit), pattern: "*".into(), action: Deny },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Write), pattern: "*".into(), action: Deny },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Patch), pattern: "*".into(), action: Deny },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Move), pattern: "*".into(), action: Deny },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Copy), pattern: "*".into(), action: Deny },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Delete), pattern: "*".into(), action: Deny },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Mkdir), pattern: "*".into(), action: Deny },
        PermissionRule { tool: ToolMatcher::Specific(ToolName::Bash), pattern: "*".into(), action: Ask },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolName;

    #[test]
    fn build_mode_allows_read_tools() {
        let engine = PermissionEngine::new(build_mode_rules());
        assert_eq!(engine.check(ToolName::Read, None), PermissionAction::Allow);
        assert_eq!(engine.check(ToolName::Grep, None), PermissionAction::Allow);
        assert_eq!(engine.check(ToolName::Glob, None), PermissionAction::Allow);
        assert_eq!(engine.check(ToolName::List, None), PermissionAction::Allow);
        // Utility tools also auto-allowed
        assert_eq!(engine.check(ToolName::Memory, None), PermissionAction::Allow);
        assert_eq!(engine.check(ToolName::Todo, None), PermissionAction::Allow);
        assert_eq!(engine.check(ToolName::Question, None), PermissionAction::Allow);
    }

    #[test]
    fn build_mode_asks_for_write_tools() {
        let engine = PermissionEngine::new(build_mode_rules());
        assert_eq!(engine.check(ToolName::Edit, None), PermissionAction::Ask);
        assert_eq!(engine.check(ToolName::Write, None), PermissionAction::Ask);
        assert_eq!(engine.check(ToolName::Patch, None), PermissionAction::Ask);
        assert_eq!(engine.check(ToolName::Bash, None), PermissionAction::Ask);
    }

    #[test]
    fn plan_mode_denies_write_tools() {
        let engine = PermissionEngine::new(plan_mode_rules());
        assert_eq!(engine.check(ToolName::Edit, None), PermissionAction::Deny);
        assert_eq!(engine.check(ToolName::Write, None), PermissionAction::Deny);
        assert_eq!(engine.check(ToolName::Patch, None), PermissionAction::Deny);
    }

    #[test]
    fn plan_mode_allows_read_tools() {
        let engine = PermissionEngine::new(plan_mode_rules());
        assert_eq!(engine.check(ToolName::Read, None), PermissionAction::Allow);
        assert_eq!(engine.check(ToolName::Grep, None), PermissionAction::Allow);
        // Utility tools also auto-allowed in plan mode
        assert_eq!(engine.check(ToolName::Todo, None), PermissionAction::Allow);
        assert_eq!(engine.check(ToolName::Question, None), PermissionAction::Allow);
    }

    #[test]
    fn plan_mode_asks_for_bash() {
        let engine = PermissionEngine::new(plan_mode_rules());
        assert_eq!(engine.check(ToolName::Bash, None), PermissionAction::Ask);
    }

    #[test]
    fn session_grant_overrides_ask() {
        let mut engine = PermissionEngine::new(build_mode_rules());
        assert_eq!(engine.check(ToolName::Bash, None), PermissionAction::Ask);
        engine.grant_session(ToolName::Bash);
        assert_eq!(engine.check(ToolName::Bash, None), PermissionAction::Allow);
    }

    #[test]
    fn session_grants_persist_across_mode_change() {
        let mut engine = PermissionEngine::new(build_mode_rules());
        engine.grant_session(ToolName::Bash);
        engine.set_rules(plan_mode_rules());
        // Session grant should still override
        assert_eq!(engine.check(ToolName::Bash, None), PermissionAction::Allow);
    }

    #[test]
    fn unmatched_tool_defaults_to_ask() {
        let engine = PermissionEngine::new(build_mode_rules());
        assert_eq!(engine.check(ToolName::Webfetch, None), PermissionAction::Ask);
    }

    #[test]
    fn is_tool_denied_in_plan_mode() {
        let engine = PermissionEngine::new(plan_mode_rules());
        assert!(engine.is_tool_denied(ToolName::Edit));
        assert!(engine.is_tool_denied(ToolName::Write));
        assert!(engine.is_tool_denied(ToolName::Patch));
        assert!(!engine.is_tool_denied(ToolName::Read));
        assert!(!engine.is_tool_denied(ToolName::Bash));
    }

    #[test]
    fn is_tool_denied_in_build_mode() {
        let engine = PermissionEngine::new(build_mode_rules());
        assert!(!engine.is_tool_denied(ToolName::Edit));
        assert!(!engine.is_tool_denied(ToolName::Bash));
        assert!(!engine.is_tool_denied(ToolName::Read));
    }

    // -- Permission Profile tests --

    #[test]
    fn trust_profile_allows_everything() {
        let engine = PermissionEngine::new(profile_build_rules(PermissionProfile::Trust, &[]));
        assert_eq!(engine.check(ToolName::Edit, None), PermissionAction::Allow);
        assert_eq!(engine.check(ToolName::Bash, None), PermissionAction::Allow);
        assert_eq!(engine.check(ToolName::Read, None), PermissionAction::Allow);
        assert_eq!(engine.check(ToolName::Delete, None), PermissionAction::Allow);
    }

    #[test]
    fn standard_profile_matches_build_mode() {
        let standard = PermissionEngine::new(profile_build_rules(PermissionProfile::Standard, &[]));
        let build = PermissionEngine::new(build_mode_rules());
        assert_eq!(standard.check(ToolName::Read, None), build.check(ToolName::Read, None));
        assert_eq!(standard.check(ToolName::Edit, None), build.check(ToolName::Edit, None));
        assert_eq!(standard.check(ToolName::Bash, None), build.check(ToolName::Bash, None));
    }

    #[test]
    fn cautious_profile_asks_for_reads() {
        let engine = PermissionEngine::new(profile_build_rules(PermissionProfile::Cautious, &[]));
        assert_eq!(engine.check(ToolName::Read, None), PermissionAction::Ask);
        assert_eq!(engine.check(ToolName::Grep, None), PermissionAction::Ask);
        assert_eq!(engine.check(ToolName::Edit, None), PermissionAction::Ask);
        // Only question/todo are auto-allowed
        assert_eq!(engine.check(ToolName::Question, None), PermissionAction::Allow);
        assert_eq!(engine.check(ToolName::Todo, None), PermissionAction::Allow);
    }

    #[test]
    fn allow_overrides_prepend_to_profile() {
        let engine = PermissionEngine::new(
            profile_build_rules(PermissionProfile::Standard, &[ToolName::Edit, ToolName::Bash]),
        );
        // Overridden tools should be auto-allowed
        assert_eq!(engine.check(ToolName::Edit, None), PermissionAction::Allow);
        assert_eq!(engine.check(ToolName::Bash, None), PermissionAction::Allow);
        // Non-overridden write tools still ask
        assert_eq!(engine.check(ToolName::Write, None), PermissionAction::Ask);
    }

    #[test]
    fn plan_mode_ignores_write_overrides() {
        let engine = PermissionEngine::new(
            profile_plan_rules(PermissionProfile::Standard, &[ToolName::Edit, ToolName::Bash]),
        );
        // Write tool override is stripped in Plan mode
        assert_eq!(engine.check(ToolName::Edit, None), PermissionAction::Deny);
        // Bash override IS applied (bash isn't a write tool, it's Ask in plan mode)
        assert_eq!(engine.check(ToolName::Bash, None), PermissionAction::Allow);
    }

    #[test]
    fn permission_profile_from_str() {
        assert_eq!("trust".parse::<PermissionProfile>().unwrap(), PermissionProfile::Trust);
        assert_eq!("standard".parse::<PermissionProfile>().unwrap(), PermissionProfile::Standard);
        assert_eq!("cautious".parse::<PermissionProfile>().unwrap(), PermissionProfile::Cautious);
        assert!("unknown".parse::<PermissionProfile>().is_err());
    }

    #[test]
    fn permission_profile_display_roundtrip() {
        for profile in [PermissionProfile::Trust, PermissionProfile::Standard, PermissionProfile::Cautious] {
            let s = profile.to_string();
            let parsed: PermissionProfile = s.parse().unwrap();
            assert_eq!(parsed, profile);
        }
    }

    #[test]
    fn permission_profile_serde_roundtrip() {
        let profile = PermissionProfile::Trust;
        let json = serde_json::to_string(&profile).unwrap();
        assert_eq!(json, "\"trust\"");
        let parsed: PermissionProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, profile);
    }
}
