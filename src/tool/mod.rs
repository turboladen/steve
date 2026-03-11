pub mod bash;
pub mod copy;
pub mod delete;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod list;
pub mod memory;
pub mod mkdir;
pub mod move_;
pub mod patch;
pub mod question;
pub mod read;
pub mod symbols;
pub mod task;
pub mod webfetch;
pub mod write;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use strum::{Display, EnumIter, EnumString, IntoStaticStr};

use crate::task::TaskStore;

/// High-level intent category for UI intent indicators.
///
/// Derived from the tool calls in an assistant turn to show what the agent
/// is *doing* (exploring, editing, executing). Used purely at render time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentCategory {
    /// Read-only observation (read, grep, glob, list, webfetch).
    Exploring,
    /// File mutations (edit, write, patch, memory).
    Editing,
    /// Shell commands (bash).
    Executing,
    /// Interactive/utility (question, task).
    Asking,
}

/// Names of all registered tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize,
         EnumString, Display, EnumIter, IntoStaticStr)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ToolName {
    Read,
    Grep,
    Glob,
    List,
    Edit,
    Write,
    Patch,
    #[strum(serialize = "move")]
    #[serde(rename = "move")]
    Move,
    Copy,
    Delete,
    Mkdir,
    Bash,
    Question,
    Task,
    Webfetch,
    Memory,
    Symbols,
}

impl ToolName {
    /// Return the lowercase string representation.
    pub fn as_str(self) -> &'static str {
        self.into()
    }

    /// Whether this is a write tool that modifies files.
    pub fn is_write_tool(self) -> bool {
        matches!(
            self,
            ToolName::Edit | ToolName::Write | ToolName::Patch
            | ToolName::Move | ToolName::Copy | ToolName::Delete | ToolName::Mkdir
        )
    }

    /// Whether this is a read-only tool (read, grep, glob, list, symbols).
    pub fn is_read_only(self) -> bool {
        matches!(
            self,
            ToolName::Read | ToolName::Grep | ToolName::Glob | ToolName::List
            | ToolName::Symbols
        )
    }

    /// Whether this tool's results can be cached.
    pub fn is_cacheable(self) -> bool {
        matches!(
            self,
            ToolName::Read | ToolName::Grep | ToolName::Glob | ToolName::List
            | ToolName::Symbols
        )
    }

    /// Whether this is the memory tool.
    pub fn is_memory(self) -> bool {
        matches!(self, ToolName::Memory)
    }

    /// Whether this is the task tool (writes to disk via storage).
    pub fn is_task(self) -> bool {
        matches!(self, ToolName::Task)
    }

    /// High-level intent category for UI intent indicators.
    ///
    /// Exhaustive match — adding a new variant forces updating this.
    pub fn intent_category(self) -> IntentCategory {
        match self {
            ToolName::Read | ToolName::Grep | ToolName::Glob
            | ToolName::List | ToolName::Webfetch | ToolName::Symbols => IntentCategory::Exploring,
            ToolName::Edit | ToolName::Write | ToolName::Patch
            | ToolName::Move | ToolName::Copy | ToolName::Delete | ToolName::Mkdir
            | ToolName::Memory => IntentCategory::Editing,
            ToolName::Bash => IntentCategory::Executing,
            ToolName::Question | ToolName::Task => IntentCategory::Asking,
        }
    }

    /// Marker symbol for display: read=·, write=✎, execute=$, interactive=⚡
    ///
    /// `Webfetch` gets the read marker despite `is_read_only()` being false —
    /// it's read-like from a UI perspective (fetches data, never writes) but
    /// isn't in the read-only permission/caching group.
    pub fn tool_marker(self) -> &'static str {
        match self {
            ToolName::Read | ToolName::Grep | ToolName::Glob
            | ToolName::List | ToolName::Webfetch | ToolName::Symbols => "\u{00b7}",       // ·
            ToolName::Edit | ToolName::Write | ToolName::Patch
            | ToolName::Move | ToolName::Copy | ToolName::Delete | ToolName::Mkdir
            | ToolName::Memory => "\u{270e}",                           // ✎
            ToolName::Bash => "$",
            ToolName::Question | ToolName::Task => "\u{26a1}",          // ⚡
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
    /// The storage directory for this project (for memory tool).
    pub storage_dir: Option<PathBuf>,
    /// The task store for persistent task management.
    pub task_store: Option<Arc<TaskStore>>,
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
        registry.register(symbols::tool());

        // Register write/execute tools
        registry.register(edit::tool());
        registry.register(write::tool());
        registry.register(patch::tool());
        registry.register(move_::tool());
        registry.register(copy::tool());
        registry.register(delete::tool());
        registry.register(mkdir::tool());
        registry.register(bash::tool());

        // Register utility tools
        registry.register(question::tool());
        registry.register(task::tool());
        registry.register(webfetch::tool());
        registry.register(memory::tool());

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
    use strum::IntoEnumIterator;

    /// Every variant round-trips through as_str -> FromStr.
    #[test]
    fn tool_name_round_trip_all_variants() {
        for name in ToolName::iter() {
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
        let write_tools = [
            ToolName::Edit, ToolName::Write, ToolName::Patch,
            ToolName::Move, ToolName::Copy, ToolName::Delete, ToolName::Mkdir,
        ];
        let non_write = [
            ToolName::Read,
            ToolName::Grep,
            ToolName::Glob,
            ToolName::List,
            ToolName::Bash,
            ToolName::Question,
            ToolName::Task,
            ToolName::Webfetch,
            ToolName::Memory,
            ToolName::Symbols,
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
        let read_only = [ToolName::Read, ToolName::Grep, ToolName::Glob, ToolName::List, ToolName::Symbols];
        let not_read_only = [
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
        let cacheable = [ToolName::Read, ToolName::Grep, ToolName::Glob, ToolName::List, ToolName::Symbols];
        let not_cacheable = [
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
        ];
        for t in cacheable {
            assert!(t.is_cacheable(), "{t} should be cacheable");
        }
        for t in not_cacheable {
            assert!(!t.is_cacheable(), "{t} should not be cacheable");
        }
    }

    /// Every variant returns a non-empty marker, and each category maps
    /// to the correct symbol.
    #[test]
    fn tool_marker_exhaustive() {
        for t in ToolName::iter() {
            assert!(!t.tool_marker().is_empty(), "{t} marker should be non-empty");
        }

        // Read tools get ·
        for t in [ToolName::Read, ToolName::Grep, ToolName::Glob, ToolName::List, ToolName::Webfetch, ToolName::Symbols] {
            assert_eq!(t.tool_marker(), "\u{00b7}", "{t} should have read marker ·");
        }

        // Write tools get ✎
        for t in [
            ToolName::Edit, ToolName::Write, ToolName::Patch,
            ToolName::Move, ToolName::Copy, ToolName::Delete, ToolName::Mkdir,
            ToolName::Memory,
        ] {
            assert_eq!(t.tool_marker(), "\u{270e}", "{t} should have write marker ✎");
        }

        // Bash gets $
        assert_eq!(ToolName::Bash.tool_marker(), "$");

        // Interactive tools get ⚡
        for t in [ToolName::Question, ToolName::Task] {
            assert_eq!(t.tool_marker(), "\u{26a1}", "{t} should have interactive marker ⚡");
        }
    }

    /// Read-only tools get the read marker, write tools + memory get the
    /// write marker, and remaining tools get execute/interactive markers.
    /// Webfetch gets the read marker despite is_read_only() == false (UI-only divergence).
    #[test]
    fn tool_marker_categories_consistent() {
        for t in ToolName::iter() {
            if t.is_read_only() {
                assert_eq!(t.tool_marker(), "\u{00b7}", "{t} is read-only but doesn't have read marker");
            } else if t.is_write_tool() || t.is_memory() {
                assert_eq!(t.tool_marker(), "\u{270e}", "{t} is write/memory but doesn't have write marker");
            } else {
                // Bash, Question, Task, Webfetch — not covered by predicates
                assert!(
                    ["\u{00b7}", "$", "\u{26a1}"].contains(&t.tool_marker()),
                    "{t} has unexpected marker '{}'", t.tool_marker()
                );
            }
        }

        // Webfetch specifically: read marker despite is_read_only() == false
        assert_eq!(ToolName::Webfetch.tool_marker(), "\u{00b7}",
            "Webfetch should have read marker (UI-only, not in is_read_only() group)");
    }

    /// Every variant maps to the expected intent category.
    /// Uses if/else if/else so every variant hits at least one assertion.
    #[test]
    fn intent_category_exhaustive() {
        for t in ToolName::iter() {
            let cat = t.intent_category();
            if t.is_read_only() || t == ToolName::Webfetch {
                assert_eq!(cat, IntentCategory::Exploring, "{t} should be Exploring");
            } else if t.is_write_tool() || t.is_memory() {
                assert_eq!(cat, IntentCategory::Editing, "{t} should be Editing");
            } else if t == ToolName::Bash {
                assert_eq!(cat, IntentCategory::Executing, "{t} should be Executing");
            } else {
                assert_eq!(cat, IntentCategory::Asking, "{t} should be Asking");
            }
        }
    }

    /// Write tools and read-only tools must be disjoint sets, and together
    /// with the remaining tools must cover all variants.
    #[test]
    fn write_and_read_only_are_disjoint() {
        for t in ToolName::iter() {
            assert!(
                !(t.is_write_tool() && t.is_read_only()),
                "{t} is both write and read-only"
            );
        }
    }
}
