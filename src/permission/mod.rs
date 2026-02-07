pub mod types;

use std::collections::HashSet;

use types::{PermissionAction, PermissionRule};

/// Evaluates permission rules for tool calls.
pub struct PermissionEngine {
    /// Static rules (from agent mode configuration).
    rules: Vec<PermissionRule>,
    /// Tools that have been granted "always allow" for this session.
    session_grants: HashSet<String>,
}

impl PermissionEngine {
    pub fn new(rules: Vec<PermissionRule>) -> Self {
        Self {
            rules,
            session_grants: HashSet::new(),
        }
    }

    /// Check whether a tool call should be allowed, denied, or needs user approval.
    pub fn check(&self, tool_name: &str, _path_hint: Option<&str>) -> PermissionAction {
        // If there's a session-level grant, allow immediately
        if self.session_grants.contains(tool_name) {
            return PermissionAction::Allow;
        }

        // Find the first matching rule for this tool
        for rule in &self.rules {
            if rule.tool == tool_name || rule.tool == "*" {
                return rule.action.clone().into();
            }
        }

        // Default: ask for permission
        PermissionAction::Ask
    }

    /// Grant "always allow" for a specific tool for the rest of this session.
    pub fn grant_session(&mut self, tool_name: &str) {
        self.session_grants.insert(tool_name.to_string());
    }

    /// Check if a tool should be completely excluded from the LLM's available tools.
    /// Tools with Deny on all patterns should not be sent to the LLM at all.
    pub fn is_tool_denied(&self, tool_name: &str) -> bool {
        for rule in &self.rules {
            if rule.tool == tool_name || rule.tool == "*" {
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
    use types::PermissionActionSerde::*;

    vec![
        // Read-only tools: always allowed
        PermissionRule { tool: "read".into(), pattern: "*".into(), action: Allow },
        PermissionRule { tool: "grep".into(), pattern: "*".into(), action: Allow },
        PermissionRule { tool: "glob".into(), pattern: "*".into(), action: Allow },
        PermissionRule { tool: "list".into(), pattern: "*".into(), action: Allow },
        // Write/execute tools: require permission
        PermissionRule { tool: "edit".into(), pattern: "*".into(), action: Ask },
        PermissionRule { tool: "write".into(), pattern: "*".into(), action: Ask },
        PermissionRule { tool: "patch".into(), pattern: "*".into(), action: Ask },
        PermissionRule { tool: "bash".into(), pattern: "*".into(), action: Ask },
    ]
}

/// Build the Plan mode permission rules (read-only, no writes).
pub fn plan_mode_rules() -> Vec<PermissionRule> {
    use types::PermissionActionSerde::*;

    vec![
        // Read-only tools: always allowed
        PermissionRule { tool: "read".into(), pattern: "*".into(), action: Allow },
        PermissionRule { tool: "grep".into(), pattern: "*".into(), action: Allow },
        PermissionRule { tool: "glob".into(), pattern: "*".into(), action: Allow },
        PermissionRule { tool: "list".into(), pattern: "*".into(), action: Allow },
        // Write/execute tools: denied in Plan mode
        PermissionRule { tool: "edit".into(), pattern: "*".into(), action: Deny },
        PermissionRule { tool: "write".into(), pattern: "*".into(), action: Deny },
        PermissionRule { tool: "patch".into(), pattern: "*".into(), action: Deny },
        PermissionRule { tool: "bash".into(), pattern: "*".into(), action: Ask },
    ]
}
