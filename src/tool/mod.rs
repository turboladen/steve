pub mod bash;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod list;
pub mod patch;
pub mod question;
pub mod read;
pub mod todo;
pub mod webfetch;
pub mod write;

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Names of all registered tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolName {
    Read,
    Grep,
    Glob,
    List,
    Edit,
    Write,
    Patch,
    Bash,
    Question,
    Todo,
    Webfetch,
}

impl ToolName {
    /// Return the lowercase string representation.
    pub fn as_str(self) -> &'static str {
        match self {
            ToolName::Read => "read",
            ToolName::Grep => "grep",
            ToolName::Glob => "glob",
            ToolName::List => "list",
            ToolName::Edit => "edit",
            ToolName::Write => "write",
            ToolName::Patch => "patch",
            ToolName::Bash => "bash",
            ToolName::Question => "question",
            ToolName::Todo => "todo",
            ToolName::Webfetch => "webfetch",
        }
    }

    /// Whether this is a write tool that modifies files (edit, write, patch).
    pub fn is_write_tool(self) -> bool {
        matches!(self, ToolName::Edit | ToolName::Write | ToolName::Patch)
    }

    /// Whether this is a read-only tool (read, grep, glob, list).
    pub fn is_read_only(self) -> bool {
        matches!(
            self,
            ToolName::Read | ToolName::Grep | ToolName::Glob | ToolName::List
        )
    }

    /// Whether this tool's results can be cached.
    pub fn is_cacheable(self) -> bool {
        matches!(
            self,
            ToolName::Read | ToolName::Grep | ToolName::Glob | ToolName::List
        )
    }
}

impl fmt::Display for ToolName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ToolName {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read" => Ok(ToolName::Read),
            "grep" => Ok(ToolName::Grep),
            "glob" => Ok(ToolName::Glob),
            "list" => Ok(ToolName::List),
            "edit" => Ok(ToolName::Edit),
            "write" => Ok(ToolName::Write),
            "patch" => Ok(ToolName::Patch),
            "bash" => Ok(ToolName::Bash),
            "question" => Ok(ToolName::Question),
            "todo" => Ok(ToolName::Todo),
            "webfetch" => Ok(ToolName::Webfetch),
            _ => Err(anyhow::anyhow!("unknown tool name: {s}")),
        }
    }
}

impl AsRef<str> for ToolName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Output from a tool execution.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// Short description for rendering in the UI (e.g., "Read src/main.rs").
    pub title: String,
    /// The tool output text.
    pub output: String,
    /// Whether the tool encountered an error.
    pub is_error: bool,
}

/// Context passed to tool execution.
#[derive(Clone)]
pub struct ToolContext {
    /// The project root directory.
    pub project_root: PathBuf,
}

/// Definition of a tool (for sending to the LLM as a function schema).
pub struct ToolDef {
    pub name: ToolName,
    pub description: String,
    pub parameters: Value,
}

/// A registered tool that can be dispatched.
pub struct ToolEntry {
    pub def: ToolDef,
    pub handler: Box<dyn Fn(Value, ToolContext) -> Result<ToolOutput> + Send + Sync>,
}

/// Registry of all available tools.
pub struct ToolRegistry {
    tools: HashMap<ToolName, ToolEntry>,
    /// Ordered list of tool names (for consistent ordering in schemas).
    order: Vec<ToolName>,
}

impl ToolRegistry {
    /// Build the registry with all available tools.
    pub fn new(project_root: PathBuf) -> Self {
        let mut registry = Self {
            tools: HashMap::new(),
            order: Vec::new(),
        };

        // Register read-only tools
        registry.register(read::tool());
        registry.register(grep::tool());
        registry.register(glob::tool());
        registry.register(list::tool());

        // Register write/execute tools
        registry.register(edit::tool());
        registry.register(write::tool());
        registry.register(patch::tool());
        registry.register(bash::tool());

        // Register utility tools
        registry.register(question::tool());
        registry.register(todo::tool());
        registry.register(webfetch::tool());

        let _ = project_root; // Will be used by tools that need it

        registry
    }

    fn register(&mut self, entry: ToolEntry) {
        let name = entry.def.name;
        self.order.push(name);
        self.tools.insert(name, entry);
    }

    /// Get the OpenAI function tool definitions for sending to the API.
    pub fn tool_definitions(&self) -> Vec<Value> {
        self.order
            .iter()
            .filter_map(|name| self.tools.get(name))
            .map(|entry| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": entry.def.name.as_str(),
                        "description": entry.def.description,
                        "parameters": entry.def.parameters,
                    }
                })
            })
            .collect()
    }

    /// Execute a tool by name.
    pub fn execute(
        &self,
        name: ToolName,
        args: Value,
        ctx: ToolContext,
    ) -> Result<ToolOutput> {
        let entry = self
            .tools
            .get(&name)
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {name}"))?;
        (entry.handler)(args, ctx)
    }

    /// Check if a tool exists.
    pub fn has_tool(&self, name: ToolName) -> bool {
        self.tools.contains_key(&name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant round-trips through as_str -> FromStr.
    #[test]
    fn tool_name_round_trip_all_variants() {
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
        for name in all {
            let s = name.as_str();
            let parsed: ToolName = s.parse().unwrap();
            assert_eq!(parsed, name, "round-trip failed for {s}");
            // Display also matches
            assert_eq!(name.to_string(), s);
            // AsRef<str> also matches
            assert_eq!(name.as_ref(), s);
        }
    }

    #[test]
    fn tool_name_from_str_unknown() {
        assert!("unknown".parse::<ToolName>().is_err());
        assert!("READ".parse::<ToolName>().is_err()); // case-sensitive
        assert!("".parse::<ToolName>().is_err());
    }

    #[test]
    fn tool_name_serde_round_trip() {
        // Serialize produces lowercase strings
        let json = serde_json::to_string(&ToolName::Webfetch).unwrap();
        assert_eq!(json, "\"webfetch\"");

        // Deserialize from lowercase string
        let parsed: ToolName = serde_json::from_str("\"read\"").unwrap();
        assert_eq!(parsed, ToolName::Read);

        // Deserialize rejects unknown
        assert!(serde_json::from_str::<ToolName>("\"unknown\"").is_err());
    }

    #[test]
    fn is_write_tool_correct() {
        let write_tools = [ToolName::Edit, ToolName::Write, ToolName::Patch];
        let non_write = [
            ToolName::Read,
            ToolName::Grep,
            ToolName::Glob,
            ToolName::List,
            ToolName::Bash,
            ToolName::Question,
            ToolName::Todo,
            ToolName::Webfetch,
        ];
        for t in write_tools {
            assert!(t.is_write_tool(), "{t} should be a write tool");
        }
        for t in non_write {
            assert!(!t.is_write_tool(), "{t} should not be a write tool");
        }
    }

    #[test]
    fn is_read_only_correct() {
        let read_only = [ToolName::Read, ToolName::Grep, ToolName::Glob, ToolName::List];
        let not_read_only = [
            ToolName::Edit,
            ToolName::Write,
            ToolName::Patch,
            ToolName::Bash,
            ToolName::Question,
            ToolName::Todo,
            ToolName::Webfetch,
        ];
        for t in read_only {
            assert!(t.is_read_only(), "{t} should be read-only");
        }
        for t in not_read_only {
            assert!(!t.is_read_only(), "{t} should not be read-only");
        }
    }

    #[test]
    fn is_cacheable_correct() {
        let cacheable = [ToolName::Read, ToolName::Grep, ToolName::Glob, ToolName::List];
        let not_cacheable = [
            ToolName::Edit,
            ToolName::Write,
            ToolName::Patch,
            ToolName::Bash,
            ToolName::Question,
            ToolName::Todo,
            ToolName::Webfetch,
        ];
        for t in cacheable {
            assert!(t.is_cacheable(), "{t} should be cacheable");
        }
        for t in not_cacheable {
            assert!(!t.is_cacheable(), "{t} should not be cacheable");
        }
    }

    /// Write tools and read-only tools must be disjoint sets, and together
    /// with the remaining tools must cover all variants.
    #[test]
    fn write_and_read_only_are_disjoint() {
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
            assert!(
                !(t.is_write_tool() && t.is_read_only()),
                "{t} is both write and read-only"
            );
        }
    }
}
