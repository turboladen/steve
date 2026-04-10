pub mod types;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use crate::tool::ToolName;
use types::{PermissionAction, PermissionActionSerde, PermissionRule, ToolMatcher};

/// Normalize a raw tool path relative to the project root.
///
/// Returns `(normalized_path, inside_project)`:
/// - Relative paths are joined with project_root, then stripped back to relative
/// - Absolute paths inside the project are stripped to relative
/// - Absolute paths outside return as-is with `inside_project = false`
/// - `..` traversals that escape the project return as-is with `inside_project = false`
pub fn normalize_tool_path(raw_path: &str, project_root: &Path) -> (String, bool) {
    let resolved = if Path::new(raw_path).is_absolute() {
        PathBuf::from(raw_path)
    } else {
        project_root.join(raw_path)
    };

    // Clean path (collapse . and ..) without filesystem access
    let cleaned = clean_path(&resolved);

    if let Ok(relative) = cleaned.strip_prefix(project_root) {
        (relative.to_string_lossy().into_owned(), true)
    } else {
        (cleaned.to_string_lossy().into_owned(), false)
    }
}

/// Collapse `.` and `..` segments in a path without hitting the filesystem.
///
/// Guards against `..` underflow: won't pop past `RootDir` or `Prefix` components,
/// so absolute paths stay absolute even with excessive `..` traversals.
fn clean_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                // Only pop Normal components — never pop RootDir or Prefix
                if components
                    .last()
                    .is_some_and(|c| matches!(c, std::path::Component::Normal(_)))
                {
                    components.pop();
                }
            }
            std::path::Component::CurDir => {}
            c => components.push(c),
        }
    }
    components.iter().collect()
}

/// Evaluates permission rules for tool calls.
pub struct PermissionEngine {
    /// Static rules (from agent mode configuration).
    rules: Vec<PermissionRule>,
    /// Tools that have been granted "always allow" for this session.
    session_grants: HashSet<ToolName>,
    /// MCP tools granted "always allow" for this session (keyed by prefixed name).
    mcp_session_grants: HashSet<String>,
    /// MCP tool names that are always allowed (from config allow_tools).
    mcp_allow_overrides: HashSet<String>,
    /// Current permission profile (needed for MCP permission checks).
    profile: PermissionProfile,
    /// Whether currently in Plan mode.
    is_plan_mode: bool,
}

impl PermissionEngine {
    pub fn new(rules: Vec<PermissionRule>) -> Self {
        Self {
            rules,
            session_grants: HashSet::new(),
            mcp_session_grants: HashSet::new(),
            mcp_allow_overrides: HashSet::new(),
            profile: PermissionProfile::Standard,
            is_plan_mode: false,
        }
    }

    /// Check whether a tool call should be allowed, denied, or needs user approval.
    ///
    /// `path_hint` is the primary file path from the tool arguments (if applicable).
    /// Path-specific rules use glob matching against this hint. Tools without paths
    /// (bash, question, task) pass `None` and skip path-specific rules.
    ///
    /// `inside_project` indicates whether the path is inside the project root:
    /// - `Some(true)` — path resolves inside the project
    /// - `Some(false)` — path resolves outside the project
    /// - `None` — tool has no path (bash, question, etc.)
    ///
    /// The special pattern `!project` matches when `inside_project == Some(false)`.
    pub fn check(
        &self,
        tool_name: ToolName,
        path_hint: Option<&str>,
        inside_project: Option<bool>,
    ) -> PermissionAction {
        // If there's a session-level grant, allow immediately
        if self.session_grants.contains(&tool_name) {
            return PermissionAction::Allow;
        }

        // Find the first matching rule for this tool
        for rule in &self.rules {
            if rule.tool.matches(tool_name) {
                // If the rule has a path pattern (not "*"), only match when we have a path
                if rule.pattern != "*" {
                    // Special sentinel: !project matches paths outside the project root
                    if rule.pattern == "!project" {
                        if inside_project == Some(false) {
                            return rule.action.clone().into();
                        }
                        // Inside project or no path — skip this rule
                        continue;
                    }

                    if let Some(path) = path_hint {
                        match glob::Pattern::new(&rule.pattern) {
                            Ok(pat) if pat.matches(path) => {
                                return rule.action.clone().into();
                            }
                            Err(e) => {
                                tracing::warn!(
                                    pattern = %rule.pattern,
                                    error = %e,
                                    "invalid glob pattern in permission rule — skipping"
                                );
                            }
                            _ => {} // valid pattern but didn't match
                        }
                        // Pattern didn't match — keep looking for another rule
                        continue;
                    } else {
                        // Rule has a path pattern but tool has no path — skip this rule
                        continue;
                    }
                }
                // Wildcard "*" pattern always matches
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

    /// Check whether an MCP tool call should be allowed, denied, or needs user approval.
    ///
    /// MCP tools bypass `ToolName` entirely — permission is based on:
    /// - Session grants (MCP-specific, keyed by prefixed name)
    /// - Config `allow_tools` overrides (supports prefixed MCP names)
    /// - Profile-based defaults: Trust=Allow, Standard/Cautious=Ask
    /// - Plan mode: always Ask (MCP tools may have side effects)
    pub fn check_mcp(&self, prefixed_name: &str) -> PermissionAction {
        if self.mcp_session_grants.contains(prefixed_name) {
            return PermissionAction::Allow;
        }
        if self.mcp_allow_overrides.contains(prefixed_name) && !self.is_plan_mode {
            return PermissionAction::Allow;
        }
        match self.profile {
            PermissionProfile::Trust if !self.is_plan_mode => PermissionAction::Allow,
            _ => PermissionAction::Ask,
        }
    }

    /// Grant "always allow" for an MCP tool for the rest of this session.
    pub fn grant_mcp_session(&mut self, prefixed_name: String) {
        self.mcp_session_grants.insert(prefixed_name);
    }

    /// Set MCP allow overrides from config `allow_tools` list.
    pub fn set_mcp_overrides(&mut self, overrides: HashSet<String>) {
        self.mcp_allow_overrides = overrides;
    }

    /// Update the permission profile (used when switching modes).
    pub fn set_profile(&mut self, profile: PermissionProfile) {
        self.profile = profile;
    }

    /// Update the plan mode flag.
    pub fn set_plan_mode(&mut self, is_plan: bool) {
        self.is_plan_mode = is_plan;
    }

    /// Check if a tool should be completely excluded from the LLM's available tools.
    /// Only excludes tools that have a wildcard Deny rule (all paths denied).
    /// Path-specific deny rules don't exclude the tool — the LLM still needs
    /// access, and individual calls are denied at check() time.
    pub fn is_tool_denied(&self, tool_name: ToolName) -> bool {
        for rule in &self.rules {
            if rule.tool.matches(tool_name) && rule.pattern == "*" {
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
            _ => Err(format!(
                "unknown permission profile: '{s}' (expected: trust, standard, cautious)"
            )),
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
/// `path_rules` are user-configured path-based rules from config.
/// Priority order: path-based rules > per-tool overrides > profile defaults (first-match wins).
pub fn profile_build_rules(
    profile: PermissionProfile,
    allow_overrides: &[ToolName],
    path_rules: &[PermissionRule],
) -> Vec<PermissionRule> {
    let mut rules = Vec::new();

    // Path-based rules come first (highest priority — most specific)
    rules.extend(path_rules.iter().cloned());

    // Per-tool overrides come next
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
/// Path-based rules that allow writes are also stripped in Plan mode.
pub fn profile_plan_rules(
    _profile: PermissionProfile,
    allow_overrides: &[ToolName],
    path_rules: &[PermissionRule],
) -> Vec<PermissionRule> {
    let mut rules = Vec::new();

    // Path-based rules, but strip write-tool allow rules in Plan mode
    for rule in path_rules {
        let strip = rule.tool.could_match_write()
            && matches!(rule.action, types::PermissionActionSerde::Allow);
        if strip {
            continue; // Don't allow writes in Plan mode
        }
        rules.push(rule.clone());
    }

    // Per-tool overrides, but only for non-write, non-Agent tools in Plan mode
    // (write tools and Agent are always denied in Plan mode)
    for &tool in allow_overrides {
        if !tool.is_write_tool() && tool != ToolName::Agent {
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
    vec![PermissionRule {
        tool: ToolMatcher::All,
        pattern: "*".into(),
        action: Allow,
    }]
}

/// Cautious profile: everything requires permission except question/task.
fn cautious_build_rules() -> Vec<PermissionRule> {
    use crate::tool::ToolName;
    use types::PermissionActionSerde::*;

    vec![
        // Only question and task are auto-allowed (no filesystem effects)
        PermissionRule {
            tool: ToolMatcher::Specific(ToolName::Question),
            pattern: "*".into(),
            action: Allow,
        },
        PermissionRule {
            tool: ToolMatcher::Specific(ToolName::Task),
            pattern: "*".into(),
            action: Allow,
        },
        // Everything else requires permission
        PermissionRule {
            tool: ToolMatcher::All,
            pattern: "*".into(),
            action: Ask,
        },
    ]
}

/// Build the default Build mode permission rules (Standard profile).
/// Shorthand for building a permission rule for a specific tool.
fn rule(tool: ToolName, action: PermissionActionSerde) -> PermissionRule {
    PermissionRule {
        tool: ToolMatcher::Specific(tool),
        pattern: "*".into(),
        action,
    }
}

/// Read-only and utility tools that are always auto-allowed in both modes.
fn always_allowed_rules() -> Vec<PermissionRule> {
    use crate::tool::ToolName;
    use types::PermissionActionSerde::Allow;

    vec![
        // Read-only tools
        rule(ToolName::Read, Allow),
        rule(ToolName::Grep, Allow),
        rule(ToolName::Glob, Allow),
        rule(ToolName::List, Allow),
        rule(ToolName::Symbols, Allow),
        rule(ToolName::Lsp, Allow),
        rule(ToolName::FindSymbol, Allow),
        // Utility tools (no filesystem side effects)
        rule(ToolName::Memory, Allow),
        rule(ToolName::Task, Allow),
        rule(ToolName::Question, Allow),
    ]
}

/// Build the Build mode permission rules (read=Allow, write/execute=Ask).
pub fn build_mode_rules() -> Vec<PermissionRule> {
    use crate::tool::ToolName;
    use types::PermissionActionSerde::Ask;

    let mut rules = always_allowed_rules();
    rules.extend([
        // Write/execute tools: require permission
        rule(ToolName::Edit, Ask),
        rule(ToolName::Write, Ask),
        rule(ToolName::Patch, Ask),
        rule(ToolName::Move, Ask),
        rule(ToolName::Copy, Ask),
        rule(ToolName::Delete, Ask),
        rule(ToolName::Mkdir, Ask),
        rule(ToolName::Bash, Ask),
        // Network tools: require permission (external side effects)
        rule(ToolName::Webfetch, Ask),
        // Agent tool: requires permission (spawns sub-agents with their own tool loops)
        rule(ToolName::Agent, Ask),
    ]);
    rules
}

/// Build the Plan mode permission rules (read=Allow, most writes=Deny).
pub fn plan_mode_rules() -> Vec<PermissionRule> {
    use crate::tool::ToolName;
    use types::PermissionActionSerde::{Ask, Deny};

    let mut rules = always_allowed_rules();
    rules.extend([
        // Write tools: denied in Plan mode
        rule(ToolName::Edit, Deny),
        rule(ToolName::Write, Deny),
        rule(ToolName::Patch, Deny),
        rule(ToolName::Move, Deny),
        rule(ToolName::Copy, Deny),
        rule(ToolName::Delete, Deny),
        rule(ToolName::Mkdir, Deny),
        // Bash: Ask (may be needed for read-only commands)
        rule(ToolName::Bash, Ask),
        // Network tools: require permission
        rule(ToolName::Webfetch, Ask),
        // Agent tool: denied (could spawn General agents that write)
        rule(ToolName::Agent, Deny),
    ]);
    rules
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolName;

    #[test]
    fn build_mode_allows_read_tools() {
        let engine = PermissionEngine::new(build_mode_rules());
        assert_eq!(
            engine.check(ToolName::Read, None, None),
            PermissionAction::Allow
        );
        assert_eq!(
            engine.check(ToolName::Grep, None, None),
            PermissionAction::Allow
        );
        assert_eq!(
            engine.check(ToolName::Glob, None, None),
            PermissionAction::Allow
        );
        assert_eq!(
            engine.check(ToolName::List, None, None),
            PermissionAction::Allow
        );
        // Utility tools also auto-allowed
        assert_eq!(
            engine.check(ToolName::Memory, None, None),
            PermissionAction::Allow
        );
        assert_eq!(
            engine.check(ToolName::Task, None, None),
            PermissionAction::Allow
        );
        assert_eq!(
            engine.check(ToolName::Question, None, None),
            PermissionAction::Allow
        );
    }

    #[test]
    fn build_mode_asks_for_write_tools() {
        let engine = PermissionEngine::new(build_mode_rules());
        assert_eq!(
            engine.check(ToolName::Edit, None, None),
            PermissionAction::Ask
        );
        assert_eq!(
            engine.check(ToolName::Write, None, None),
            PermissionAction::Ask
        );
        assert_eq!(
            engine.check(ToolName::Patch, None, None),
            PermissionAction::Ask
        );
        assert_eq!(
            engine.check(ToolName::Bash, None, None),
            PermissionAction::Ask
        );
    }

    #[test]
    fn plan_mode_denies_write_tools() {
        let engine = PermissionEngine::new(plan_mode_rules());
        assert_eq!(
            engine.check(ToolName::Edit, None, None),
            PermissionAction::Deny
        );
        assert_eq!(
            engine.check(ToolName::Write, None, None),
            PermissionAction::Deny
        );
        assert_eq!(
            engine.check(ToolName::Patch, None, None),
            PermissionAction::Deny
        );
    }

    #[test]
    fn plan_mode_allows_read_tools() {
        let engine = PermissionEngine::new(plan_mode_rules());
        assert_eq!(
            engine.check(ToolName::Read, None, None),
            PermissionAction::Allow
        );
        assert_eq!(
            engine.check(ToolName::Grep, None, None),
            PermissionAction::Allow
        );
        // Utility tools also auto-allowed in plan mode
        assert_eq!(
            engine.check(ToolName::Task, None, None),
            PermissionAction::Allow
        );
        assert_eq!(
            engine.check(ToolName::Question, None, None),
            PermissionAction::Allow
        );
    }

    #[test]
    fn plan_mode_asks_for_bash() {
        let engine = PermissionEngine::new(plan_mode_rules());
        assert_eq!(
            engine.check(ToolName::Bash, None, None),
            PermissionAction::Ask
        );
    }

    #[test]
    fn session_grant_overrides_ask() {
        let mut engine = PermissionEngine::new(build_mode_rules());
        assert_eq!(
            engine.check(ToolName::Bash, None, None),
            PermissionAction::Ask
        );
        engine.grant_session(ToolName::Bash);
        assert_eq!(
            engine.check(ToolName::Bash, None, None),
            PermissionAction::Allow
        );
    }

    #[test]
    fn session_grants_persist_across_mode_change() {
        let mut engine = PermissionEngine::new(build_mode_rules());
        engine.grant_session(ToolName::Bash);
        engine.set_rules(plan_mode_rules());
        // Session grant should still override
        assert_eq!(
            engine.check(ToolName::Bash, None, None),
            PermissionAction::Allow
        );
    }

    #[test]
    fn webfetch_requires_permission() {
        let engine = PermissionEngine::new(build_mode_rules());
        assert_eq!(
            engine.check(ToolName::Webfetch, None, None),
            PermissionAction::Ask
        );
    }

    #[test]
    fn all_tools_have_explicit_rule_in_build_mode() {
        use strum::IntoEnumIterator;
        let rules = build_mode_rules();
        for tool in ToolName::iter() {
            let has_rule = rules.iter().any(|r| r.tool.matches(tool));
            assert!(
                has_rule,
                "{tool} should have an explicit rule in build_mode_rules"
            );
        }
    }

    #[test]
    fn all_tools_have_explicit_rule_in_plan_mode() {
        use strum::IntoEnumIterator;
        let rules = plan_mode_rules();
        for tool in ToolName::iter() {
            let has_rule = rules.iter().any(|r| r.tool.matches(tool));
            assert!(
                has_rule,
                "{tool} should have an explicit rule in plan_mode_rules"
            );
        }
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
        let engine = PermissionEngine::new(profile_build_rules(PermissionProfile::Trust, &[], &[]));
        assert_eq!(
            engine.check(ToolName::Edit, None, None),
            PermissionAction::Allow
        );
        assert_eq!(
            engine.check(ToolName::Bash, None, None),
            PermissionAction::Allow
        );
        assert_eq!(
            engine.check(ToolName::Read, None, None),
            PermissionAction::Allow
        );
        assert_eq!(
            engine.check(ToolName::Delete, None, None),
            PermissionAction::Allow
        );
    }

    #[test]
    fn standard_profile_matches_build_mode() {
        let standard =
            PermissionEngine::new(profile_build_rules(PermissionProfile::Standard, &[], &[]));
        let build = PermissionEngine::new(build_mode_rules());
        assert_eq!(
            standard.check(ToolName::Read, None, None),
            build.check(ToolName::Read, None, None)
        );
        assert_eq!(
            standard.check(ToolName::Edit, None, None),
            build.check(ToolName::Edit, None, None)
        );
        assert_eq!(
            standard.check(ToolName::Bash, None, None),
            build.check(ToolName::Bash, None, None)
        );
    }

    #[test]
    fn cautious_profile_asks_for_reads() {
        let engine =
            PermissionEngine::new(profile_build_rules(PermissionProfile::Cautious, &[], &[]));
        assert_eq!(
            engine.check(ToolName::Read, None, None),
            PermissionAction::Ask
        );
        assert_eq!(
            engine.check(ToolName::Grep, None, None),
            PermissionAction::Ask
        );
        assert_eq!(
            engine.check(ToolName::Edit, None, None),
            PermissionAction::Ask
        );
        // Only question/task are auto-allowed
        assert_eq!(
            engine.check(ToolName::Question, None, None),
            PermissionAction::Allow
        );
        assert_eq!(
            engine.check(ToolName::Task, None, None),
            PermissionAction::Allow
        );
    }

    #[test]
    fn allow_overrides_prepend_to_profile() {
        let engine = PermissionEngine::new(profile_build_rules(
            PermissionProfile::Standard,
            &[ToolName::Edit, ToolName::Bash],
            &[],
        ));
        // Overridden tools should be auto-allowed
        assert_eq!(
            engine.check(ToolName::Edit, None, None),
            PermissionAction::Allow
        );
        assert_eq!(
            engine.check(ToolName::Bash, None, None),
            PermissionAction::Allow
        );
        // Non-overridden write tools still ask
        assert_eq!(
            engine.check(ToolName::Write, None, None),
            PermissionAction::Ask
        );
    }

    #[test]
    fn plan_mode_ignores_write_and_agent_overrides() {
        let engine = PermissionEngine::new(profile_plan_rules(
            PermissionProfile::Standard,
            &[ToolName::Edit, ToolName::Bash, ToolName::Agent],
            &[],
        ));
        // Write tool override is stripped in Plan mode
        assert_eq!(
            engine.check(ToolName::Edit, None, None),
            PermissionAction::Deny
        );
        // Agent override is also stripped in Plan mode
        assert_eq!(
            engine.check(ToolName::Agent, None, None),
            PermissionAction::Deny
        );
        // Bash override IS applied (bash isn't a write tool or agent, it's Ask in plan mode)
        assert_eq!(
            engine.check(ToolName::Bash, None, None),
            PermissionAction::Allow
        );
    }

    #[test]
    fn permission_profile_from_str() {
        assert_eq!(
            "trust".parse::<PermissionProfile>().unwrap(),
            PermissionProfile::Trust
        );
        assert_eq!(
            "standard".parse::<PermissionProfile>().unwrap(),
            PermissionProfile::Standard
        );
        assert_eq!(
            "cautious".parse::<PermissionProfile>().unwrap(),
            PermissionProfile::Cautious
        );
        assert!("unknown".parse::<PermissionProfile>().is_err());
    }

    #[test]
    fn permission_profile_display_roundtrip() {
        for profile in [
            PermissionProfile::Trust,
            PermissionProfile::Standard,
            PermissionProfile::Cautious,
        ] {
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

    // -- Path-based permission rule tests --

    #[test]
    fn path_rule_allows_edit_in_src() {
        let path_rules = vec![PermissionRule {
            tool: ToolMatcher::Specific(ToolName::Edit),
            pattern: "src/**".into(),
            action: types::PermissionActionSerde::Allow,
        }];
        let engine = PermissionEngine::new(profile_build_rules(
            PermissionProfile::Standard,
            &[],
            &path_rules,
        ));
        // Edit in src/ is allowed by path rule
        assert_eq!(
            engine.check(ToolName::Edit, Some("src/main.rs"), None),
            PermissionAction::Allow
        );
        // Edit outside src/ falls through to profile default (Ask)
        assert_eq!(
            engine.check(ToolName::Edit, Some("config/app.toml"), None),
            PermissionAction::Ask
        );
        // Edit with no path falls through to profile default (Ask)
        assert_eq!(
            engine.check(ToolName::Edit, None, None),
            PermissionAction::Ask
        );
    }

    #[test]
    fn path_rule_denies_edit_outside_project() {
        let path_rules = vec![PermissionRule {
            tool: ToolMatcher::Specific(ToolName::Edit),
            pattern: "/etc/**".into(),
            action: types::PermissionActionSerde::Deny,
        }];
        let engine = PermissionEngine::new(profile_build_rules(
            PermissionProfile::Standard,
            &[],
            &path_rules,
        ));
        assert_eq!(
            engine.check(ToolName::Edit, Some("/etc/passwd"), None),
            PermissionAction::Deny
        );
        // Normal edits still go through standard rules (Ask)
        assert_eq!(
            engine.check(ToolName::Edit, Some("src/main.rs"), None),
            PermissionAction::Ask
        );
    }

    #[test]
    fn path_rules_take_priority_over_allow_overrides() {
        let path_rules = vec![PermissionRule {
            tool: ToolMatcher::Specific(ToolName::Edit),
            pattern: "Cargo.toml".into(),
            action: types::PermissionActionSerde::Ask,
        }];
        // allow_overrides says "always allow edit", but path rule says "ask for Cargo.toml"
        let engine = PermissionEngine::new(profile_build_rules(
            PermissionProfile::Standard,
            &[ToolName::Edit],
            &path_rules,
        ));
        // Cargo.toml matches the path rule (Ask), which is higher priority
        assert_eq!(
            engine.check(ToolName::Edit, Some("Cargo.toml"), None),
            PermissionAction::Ask
        );
        // Other files fall through to the allow_override (Allow)
        assert_eq!(
            engine.check(ToolName::Edit, Some("src/main.rs"), None),
            PermissionAction::Allow
        );
    }

    #[test]
    fn path_rules_dont_affect_unrelated_tools() {
        let path_rules = vec![PermissionRule {
            tool: ToolMatcher::Specific(ToolName::Edit),
            pattern: "src/**".into(),
            action: types::PermissionActionSerde::Allow,
        }];
        let engine = PermissionEngine::new(profile_build_rules(
            PermissionProfile::Standard,
            &[],
            &path_rules,
        ));
        // Read tool unaffected (already allowed by profile)
        assert_eq!(
            engine.check(ToolName::Read, Some("src/main.rs"), None),
            PermissionAction::Allow
        );
        // Bash unaffected (no path rule for it)
        assert_eq!(
            engine.check(ToolName::Bash, None, None),
            PermissionAction::Ask
        );
    }

    #[test]
    fn plan_mode_strips_write_path_rules() {
        let path_rules = vec![PermissionRule {
            tool: ToolMatcher::Specific(ToolName::Edit),
            pattern: "src/**".into(),
            action: types::PermissionActionSerde::Allow,
        }];
        let engine = PermissionEngine::new(profile_plan_rules(
            PermissionProfile::Standard,
            &[],
            &path_rules,
        ));
        // Path rule for edit is stripped in Plan mode — edit is denied
        assert_eq!(
            engine.check(ToolName::Edit, Some("src/main.rs"), None),
            PermissionAction::Deny
        );
    }

    #[test]
    fn plan_mode_strips_wildcard_allow_path_rules() {
        let path_rules = vec![PermissionRule {
            tool: ToolMatcher::All,
            pattern: "**".into(),
            action: types::PermissionActionSerde::Allow,
        }];
        let engine = PermissionEngine::new(profile_plan_rules(
            PermissionProfile::Standard,
            &[],
            &path_rules,
        ));
        // Wildcard Allow path rule is stripped in Plan mode — write tools are still denied
        assert_eq!(
            engine.check(ToolName::Edit, Some("src/main.rs"), None),
            PermissionAction::Deny
        );
        assert_eq!(
            engine.check(ToolName::Write, Some("foo.txt"), None),
            PermissionAction::Deny
        );
        // Read tools should still be allowed (from plan_mode_rules defaults)
        assert_eq!(
            engine.check(ToolName::Read, Some("src/main.rs"), None),
            PermissionAction::Allow
        );
    }

    #[test]
    fn plan_mode_keeps_wildcard_deny_path_rules() {
        let path_rules = vec![PermissionRule {
            tool: ToolMatcher::All,
            pattern: "secret/**".into(),
            action: types::PermissionActionSerde::Deny,
        }];
        let engine = PermissionEngine::new(profile_plan_rules(
            PermissionProfile::Standard,
            &[],
            &path_rules,
        ));
        // Wildcard Deny is preserved — blocks reads too
        assert_eq!(
            engine.check(ToolName::Read, Some("secret/key.txt"), None),
            PermissionAction::Deny
        );
    }

    #[test]
    fn plan_mode_keeps_wildcard_ask_path_rules() {
        let path_rules = vec![PermissionRule {
            tool: ToolMatcher::All,
            pattern: "!project".into(),
            action: types::PermissionActionSerde::Ask,
        }];
        let engine = PermissionEngine::new(profile_plan_rules(
            PermissionProfile::Standard,
            &[],
            &path_rules,
        ));
        // Wildcard Ask rule is preserved — outside-project paths still require approval
        assert_eq!(
            engine.check(ToolName::Read, Some("/etc/passwd"), Some(false)),
            PermissionAction::Ask,
        );
    }

    #[test]
    fn session_grant_overrides_path_deny() {
        let path_rules = vec![PermissionRule {
            tool: ToolMatcher::Specific(ToolName::Edit),
            pattern: "/etc/**".into(),
            action: types::PermissionActionSerde::Deny,
        }];
        let mut engine = PermissionEngine::new(profile_build_rules(
            PermissionProfile::Standard,
            &[],
            &path_rules,
        ));
        engine.grant_session(ToolName::Edit);
        // Session grant overrides everything (including path deny)
        assert_eq!(
            engine.check(ToolName::Edit, Some("/etc/passwd"), None),
            PermissionAction::Allow
        );
    }

    #[test]
    fn is_tool_denied_only_for_wildcard_deny() {
        let path_rules = vec![PermissionRule {
            tool: ToolMatcher::Specific(ToolName::Edit),
            pattern: "/etc/**".into(),
            action: types::PermissionActionSerde::Deny,
        }];
        let engine = PermissionEngine::new(profile_build_rules(
            PermissionProfile::Standard,
            &[],
            &path_rules,
        ));
        // Path-specific deny doesn't exclude the tool from LLM
        assert!(!engine.is_tool_denied(ToolName::Edit));
    }

    #[test]
    fn path_rule_config_serde_roundtrip() {
        let rule = PermissionRule {
            tool: ToolMatcher::Specific(ToolName::Edit),
            pattern: "src/**".into(),
            action: types::PermissionActionSerde::Allow,
        };
        let json = serde_json::to_string(&rule).unwrap();
        let parsed: PermissionRule = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tool, rule.tool);
        assert_eq!(parsed.pattern, rule.pattern);
    }

    // -- Path normalization tests --

    #[test]
    fn normalize_tool_path_relative_inside() {
        let root = PathBuf::from("/project");
        let (normalized, inside) = normalize_tool_path("src/main.rs", &root);
        assert_eq!(normalized, "src/main.rs");
        assert!(inside);
    }

    #[test]
    fn normalize_tool_path_absolute_inside() {
        let root = PathBuf::from("/project");
        let (normalized, inside) = normalize_tool_path("/project/src/main.rs", &root);
        assert_eq!(normalized, "src/main.rs");
        assert!(inside);
    }

    #[test]
    fn normalize_tool_path_absolute_outside() {
        let root = PathBuf::from("/project");
        let (normalized, inside) = normalize_tool_path("/etc/passwd", &root);
        assert_eq!(normalized, "/etc/passwd");
        assert!(!inside);
    }

    #[test]
    fn normalize_tool_path_dotdot_escape() {
        let root = PathBuf::from("/project/sub");
        let (normalized, inside) = normalize_tool_path("../../etc/passwd", &root);
        assert_eq!(normalized, "/etc/passwd");
        assert!(!inside);
    }

    #[test]
    fn normalize_tool_path_dotdot_stays_inside() {
        let root = PathBuf::from("/project");
        let (normalized, inside) = normalize_tool_path("src/../lib/foo.rs", &root);
        assert_eq!(normalized, "lib/foo.rs");
        assert!(inside);
    }

    #[test]
    fn clean_path_collapses_dots() {
        assert_eq!(clean_path(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
        assert_eq!(clean_path(Path::new("/a/./b/c")), PathBuf::from("/a/b/c"));
        assert_eq!(clean_path(Path::new("/a/b/../../c")), PathBuf::from("/c"));
    }

    #[test]
    fn clean_path_guards_against_underflow() {
        // Excessive .. should not pop past root
        assert_eq!(clean_path(Path::new("/a/../../b")), PathBuf::from("/b"));
        assert_eq!(
            clean_path(Path::new("/a/../../../etc/passwd")),
            PathBuf::from("/etc/passwd")
        );
    }

    #[test]
    fn normalize_tool_path_project_root_itself() {
        let root = PathBuf::from("/project");
        let (normalized, inside) = normalize_tool_path(".", &root);
        // "." resolves to the project root — strip_prefix yields ""
        assert_eq!(normalized, "");
        assert!(inside);
    }

    // -- !project sentinel pattern tests --

    #[test]
    fn check_not_project_pattern_denies_outside() {
        let path_rules = vec![PermissionRule {
            tool: ToolMatcher::All,
            pattern: "!project".into(),
            action: types::PermissionActionSerde::Deny,
        }];
        let engine = PermissionEngine::new(profile_build_rules(
            PermissionProfile::Standard,
            &[],
            &path_rules,
        ));
        // Outside project → !project matches → Deny
        assert_eq!(
            engine.check(ToolName::Edit, Some("/etc/passwd"), Some(false)),
            PermissionAction::Deny,
        );
    }

    #[test]
    fn check_not_project_pattern_skips_inside() {
        let path_rules = vec![PermissionRule {
            tool: ToolMatcher::All,
            pattern: "!project".into(),
            action: types::PermissionActionSerde::Deny,
        }];
        let engine = PermissionEngine::new(profile_build_rules(
            PermissionProfile::Standard,
            &[],
            &path_rules,
        ));
        // Inside project → !project does NOT match → falls through to profile
        assert_eq!(
            engine.check(ToolName::Edit, Some("src/main.rs"), Some(true)),
            PermissionAction::Ask,
        );
    }

    #[test]
    fn check_not_project_pattern_preserved_in_plan_mode() {
        let path_rules = vec![PermissionRule {
            tool: ToolMatcher::All,
            pattern: "!project".into(),
            action: types::PermissionActionSerde::Deny,
        }];
        let engine = PermissionEngine::new(profile_plan_rules(
            PermissionProfile::Standard,
            &[],
            &path_rules,
        ));
        // !project deny rule should NOT be stripped in Plan mode
        assert_eq!(
            engine.check(ToolName::Read, Some("/etc/passwd"), Some(false)),
            PermissionAction::Deny,
        );
    }

    #[test]
    fn check_not_project_pattern_skips_no_path() {
        let path_rules = vec![PermissionRule {
            tool: ToolMatcher::All,
            pattern: "!project".into(),
            action: types::PermissionActionSerde::Deny,
        }];
        let engine = PermissionEngine::new(profile_build_rules(
            PermissionProfile::Standard,
            &[],
            &path_rules,
        ));
        // No path → !project does NOT match → falls through to profile
        assert_eq!(
            engine.check(ToolName::Bash, None, None),
            PermissionAction::Ask,
        );
    }

    // -- MCP permission tests --

    #[test]
    fn check_mcp_standard_profile_asks() {
        let engine = PermissionEngine::new(build_mode_rules());
        assert_eq!(
            engine.check_mcp("mcp__github__search"),
            PermissionAction::Ask
        );
    }

    #[test]
    fn check_mcp_trust_profile_allows() {
        let mut engine = PermissionEngine::new(build_mode_rules());
        engine.set_profile(PermissionProfile::Trust);
        assert_eq!(
            engine.check_mcp("mcp__github__search"),
            PermissionAction::Allow
        );
    }

    #[test]
    fn check_mcp_trust_plan_mode_asks() {
        let mut engine = PermissionEngine::new(build_mode_rules());
        engine.set_profile(PermissionProfile::Trust);
        engine.set_plan_mode(true);
        assert_eq!(
            engine.check_mcp("mcp__github__search"),
            PermissionAction::Ask
        );
    }

    #[test]
    fn check_mcp_session_grant_overrides() {
        let mut engine = PermissionEngine::new(build_mode_rules());
        assert_eq!(
            engine.check_mcp("mcp__github__search"),
            PermissionAction::Ask
        );
        engine.grant_mcp_session("mcp__github__search".into());
        assert_eq!(
            engine.check_mcp("mcp__github__search"),
            PermissionAction::Allow
        );
    }

    #[test]
    fn check_mcp_allow_override() {
        let mut engine = PermissionEngine::new(build_mode_rules());
        engine.set_mcp_overrides(["mcp__github__search".to_string()].into());
        assert_eq!(
            engine.check_mcp("mcp__github__search"),
            PermissionAction::Allow
        );
        // Other MCP tools still ask
        assert_eq!(engine.check_mcp("mcp__github__push"), PermissionAction::Ask);
    }

    #[test]
    fn check_mcp_allow_override_stripped_in_plan_mode() {
        let mut engine = PermissionEngine::new(build_mode_rules());
        engine.set_mcp_overrides(["mcp__github__search".to_string()].into());
        engine.set_plan_mode(true);
        // Override is stripped in Plan mode
        assert_eq!(
            engine.check_mcp("mcp__github__search"),
            PermissionAction::Ask
        );
    }
}
