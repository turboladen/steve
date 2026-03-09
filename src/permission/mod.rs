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

/// Build the default Build mode permission rules.
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
}
