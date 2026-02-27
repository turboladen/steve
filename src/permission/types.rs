use serde::{Deserialize, Serialize};

use crate::tool::ToolName;

/// What the permission system decides for a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionAction {
    /// Tool call is allowed automatically (no prompt).
    Allow,
    /// Tool call is denied outright.
    Deny,
    /// User must be asked for permission.
    Ask,
}

/// Matches one specific tool or all tools (wildcard).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolMatcher {
    /// Match a specific tool.
    Specific(ToolName),
    /// Match all tools ("*").
    All,
}

impl ToolMatcher {
    pub fn matches(&self, tool: ToolName) -> bool {
        match self {
            ToolMatcher::Specific(t) => *t == tool,
            ToolMatcher::All => true,
        }
    }
}

impl Serialize for ToolMatcher {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ToolMatcher::Specific(name) => serializer.serialize_str(name.as_str()),
            ToolMatcher::All => serializer.serialize_str("*"),
        }
    }
}

impl<'de> Deserialize<'de> for ToolMatcher {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        if s == "*" {
            Ok(ToolMatcher::All)
        } else {
            s.parse::<ToolName>()
                .map(ToolMatcher::Specific)
                .map_err(serde::de::Error::custom)
        }
    }
}

/// A rule that maps a tool + path pattern to an action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    /// Tool matcher (e.g., specific tool or wildcard "*").
    pub tool: ToolMatcher,
    /// Glob pattern for matching the first argument / path (e.g., "src/**").
    /// Use "*" to match everything.
    pub pattern: String,
    /// The action to take when this rule matches.
    pub action: PermissionActionSerde,
}

/// Serializable version of PermissionAction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionActionSerde {
    Allow,
    Deny,
    Ask,
}

impl From<PermissionActionSerde> for PermissionAction {
    fn from(s: PermissionActionSerde) -> Self {
        match s {
            PermissionActionSerde::Allow => PermissionAction::Allow,
            PermissionActionSerde::Deny => PermissionAction::Deny,
            PermissionActionSerde::Ask => PermissionAction::Ask,
        }
    }
}

/// A permission request sent to the UI for user approval.
#[derive(Debug)]
pub struct PermissionRequest {
    pub call_id: String,
    pub tool_name: ToolName,
    pub arguments_summary: String,
    pub response_tx: tokio::sync::oneshot::Sender<PermissionReply>,
}

/// The user's reply to a permission prompt.
#[derive(Debug, Clone)]
pub enum PermissionReply {
    /// Allow this specific tool call.
    AllowOnce,
    /// Allow this tool for the rest of the session.
    AllowAlways,
    /// Deny this tool call.
    Deny,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_matcher_specific_matches_only_that_tool() {
        let matcher = ToolMatcher::Specific(ToolName::Bash);
        assert!(matcher.matches(ToolName::Bash));
        assert!(!matcher.matches(ToolName::Read));
        assert!(!matcher.matches(ToolName::Edit));
    }

    #[test]
    fn tool_matcher_all_matches_everything() {
        let matcher = ToolMatcher::All;
        let all = [
            ToolName::Read,
            ToolName::Grep,
            ToolName::Glob,
            ToolName::List,
            ToolName::Edit,
            ToolName::Write,
            ToolName::Patch,
            ToolName::Bash,
            ToolName::Question,
            ToolName::Todo,
            ToolName::Webfetch,
        ];
        for t in all {
            assert!(matcher.matches(t), "All should match {t}");
        }
    }

    #[test]
    fn tool_matcher_serde_specific_round_trip() {
        let matcher = ToolMatcher::Specific(ToolName::Edit);
        let json = serde_json::to_string(&matcher).unwrap();
        assert_eq!(json, "\"edit\"");

        let parsed: ToolMatcher = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, matcher);
    }

    #[test]
    fn tool_matcher_serde_all_round_trip() {
        let matcher = ToolMatcher::All;
        let json = serde_json::to_string(&matcher).unwrap();
        assert_eq!(json, "\"*\"");

        let parsed: ToolMatcher = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ToolMatcher::All);
    }

    #[test]
    fn tool_matcher_deserialize_invalid_tool() {
        let result = serde_json::from_str::<ToolMatcher>("\"nonexistent\"");
        assert!(result.is_err());
    }

    #[test]
    fn permission_rule_serde_round_trip() {
        let rule = PermissionRule {
            tool: ToolMatcher::Specific(ToolName::Bash),
            pattern: "*".to_string(),
            action: PermissionActionSerde::Ask,
        };
        let json = serde_json::to_string(&rule).unwrap();
        let parsed: PermissionRule = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tool, rule.tool);
        assert_eq!(parsed.pattern, rule.pattern);
    }
}
