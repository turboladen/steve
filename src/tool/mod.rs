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
use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

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
    pub name: String,
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
    tools: HashMap<String, ToolEntry>,
    /// Ordered list of tool names (for consistent ordering in schemas).
    order: Vec<String>,
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
        let name = entry.def.name.clone();
        self.order.push(name.clone());
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
                        "name": entry.def.name,
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
        name: &str,
        args: Value,
        ctx: ToolContext,
    ) -> Result<ToolOutput> {
        let entry = self
            .tools
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {name}"))?;
        (entry.handler)(args, ctx)
    }

    /// Check if a tool exists.
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }
}
