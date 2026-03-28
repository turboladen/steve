//! Agent tool — delegates subtasks to child agents with their own conversation
//! contexts and tool loops.
//!
//! Like the Question tool, this is an "intercepted stub": the synchronous handler
//! returns an error telling the caller it should have been intercepted by `stream.rs`.
//! The actual agent spawning happens in `run_sub_agent()` in stream.rs.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use strum::{Display, EnumIter, EnumString, IntoStaticStr};

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

/// Types of child agents with different tool access and model selection.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    EnumString,
    Display,
    EnumIter,
    IntoStaticStr,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum AgentType {
    /// Read-only exploration — uses smaller/faster model.
    Explore,
    /// Architecture/design analysis — read-only + LSP.
    Plan,
    /// Full tool access (except agent) — inherits parent permissions.
    General,
}

impl AgentType {
    /// Tools available to this agent type.
    ///
    /// `Agent` is always excluded to prevent recursive spawning.
    pub fn allowed_tools(self) -> Vec<ToolName> {
        match self {
            AgentType::Explore => vec![
                ToolName::Read,
                ToolName::Grep,
                ToolName::Glob,
                ToolName::List,
                ToolName::Symbols,
            ],
            AgentType::Plan => vec![
                ToolName::Read,
                ToolName::Grep,
                ToolName::Glob,
                ToolName::List,
                ToolName::Symbols,
                ToolName::Lsp,
            ],
            AgentType::General => vec![
                ToolName::Read,
                ToolName::Grep,
                ToolName::Glob,
                ToolName::List,
                ToolName::Symbols,
                ToolName::Lsp,
                ToolName::Edit,
                ToolName::Write,
                ToolName::Patch,
                ToolName::Move,
                ToolName::Copy,
                ToolName::Delete,
                ToolName::Mkdir,
                ToolName::Bash,
                ToolName::Question,
                ToolName::Task,
                ToolName::Webfetch,
                ToolName::Memory,
                // Agent is deliberately excluded — no recursive spawning.
            ],
        }
    }
}

pub fn tool() -> ToolEntry {
    let def_json = definition();
    let func = def_json.get("function").unwrap();
    ToolEntry {
        def: ToolDef {
            name: ToolName::Agent,
            description: func
                .get("description")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
            parameters: func.get("parameters").cloned().unwrap(),
        },
        handler: Box::new(execute),
    }
}

fn definition() -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "agent",
            "description": "Delegate a subtask to a child agent with its own conversation context and tool loop. Use to protect your context window from deep exploration, parallelize independent work, or isolate complex subtasks. The agent runs autonomously and returns a summary of its findings or work.",
            "parameters": {
                "type": "object",
                "properties": {
                    "agent_type": {
                        "type": "string",
                        "enum": ["explore", "plan", "general"],
                        "description": "Agent type: 'explore' (read-only, fast, uses smaller model), 'plan' (read-only + LSP, for architecture/design), 'general' (full tool access, inherits permissions)"
                    },
                    "task": {
                        "type": "string",
                        "description": "Clear, specific description of what the agent should accomplish"
                    },
                    "context": {
                        "type": "string",
                        "description": "Optional additional context: file paths, patterns, constraints"
                    }
                },
                "required": ["agent_type", "task"]
            }
        }
    })
}

fn execute(_args: Value, _ctx: ToolContext) -> anyhow::Result<ToolOutput> {
    // This handler is a fallback — the stream task should intercept agent
    // tool calls before they reach execute(), just like the Question tool.
    Ok(ToolOutput {
        title: "Agent".to_string(),
        output: "Error: agent tool must be intercepted by the stream task. \
                 This handler should not be called directly."
            .to_string(),
        is_error: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;

    #[test]
    fn agent_type_serde_round_trip() {
        for at in AgentType::iter() {
            let json = serde_json::to_string(&at).unwrap();
            let parsed: AgentType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, at);
        }
    }

    #[test]
    fn agent_type_from_str_round_trip() {
        for at in AgentType::iter() {
            let s = at.to_string();
            let parsed: AgentType = s.parse().unwrap();
            assert_eq!(parsed, at);
        }
    }

    #[test]
    fn agent_type_from_str_rejects_invalid() {
        assert!("unknown".parse::<AgentType>().is_err());
        assert!("EXPLORE".parse::<AgentType>().is_err());
        assert!("".parse::<AgentType>().is_err());
    }

    #[test]
    fn allowed_tools_never_includes_agent() {
        for at in AgentType::iter() {
            let tools = at.allowed_tools();
            assert!(
                !tools.contains(&ToolName::Agent),
                "{at} allowed_tools must not include Agent (prevents recursion)"
            );
        }
    }

    #[test]
    fn explore_has_only_read_tools() {
        let tools = AgentType::Explore.allowed_tools();
        for t in &tools {
            assert!(
                t.is_read_only(),
                "Explore agent should only have read-only tools, but has {t}"
            );
        }
    }

    #[test]
    fn plan_has_read_tools_plus_lsp() {
        let tools = AgentType::Plan.allowed_tools();
        for t in &tools {
            assert!(
                t.is_read_only() || *t == ToolName::Lsp,
                "Plan agent should only have read-only + LSP tools, but has {t}"
            );
        }
    }

    #[test]
    fn general_has_most_tools() {
        let tools = AgentType::General.allowed_tools();
        // General should have all tools except Agent
        let all_except_agent: Vec<ToolName> =
            ToolName::iter().filter(|t| *t != ToolName::Agent).collect();
        for t in &all_except_agent {
            assert!(tools.contains(t), "General agent should include {t}");
        }
    }

    #[test]
    fn tool_definition_parses() {
        let entry = tool();
        assert_eq!(entry.def.name, ToolName::Agent);
        assert!(!entry.def.description.is_empty());
    }

    #[test]
    fn stub_handler_returns_error() {
        let ctx = ToolContext {
            project_root: std::path::PathBuf::from("/tmp"),
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        };
        let output = (entry_for_test().handler)(serde_json::json!({}), ctx).unwrap();
        assert!(output.is_error);
        assert!(output.output.contains("intercepted"));
    }

    fn entry_for_test() -> ToolEntry {
        tool()
    }
}
