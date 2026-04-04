//! Question tool — asks the user a question and waits for their response.
//!
//! This tool is special because it needs to communicate back to the UI
//! to show a prompt and collect input. It sends an event through the
//! app event channel and awaits a response via a oneshot channel.
//!
//! Since our tool handlers are synchronous, this tool stores the question
//! and returns a message telling the LLM to wait. The actual question flow
//! is handled asynchronously by the stream task.

use serde_json::Value;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

pub fn tool() -> ToolEntry {
    let def_json = definition();
    let func = def_json.get("function").unwrap();
    ToolEntry {
        def: ToolDef {
            name: ToolName::Question,
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
            "name": "question",
            "description": "Ask the user a question and wait for their response. Use this when you need clarification or want to give the user a choice between options. The user's answer will be returned as the tool result.",
            "parameters": {
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question to ask the user."
                    },
                    "options": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of choices for the user."
                    }
                },
                "required": ["question"]
            }
        }
    })
}

fn execute(args: Value, _ctx: ToolContext) -> anyhow::Result<ToolOutput> {
    // For now, the question tool returns a placeholder. The actual question
    // flow will be implemented in the stream task, which intercepts "question"
    // tool calls before they reach this handler and uses the event channel.
    let question = args
        .get("question")
        .and_then(|v| v.as_str())
        .unwrap_or("(no question provided)");

    // This handler is a fallback — ideally the stream task handles question
    // tool calls specially before reaching execute().
    Ok(ToolOutput {
        title: "Question".to_string(),
        output: format!(
            "Question for user: {question}\n(Question tool not yet fully implemented — user interaction pending)"
        ),
        is_error: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_handler_returns_not_error() {
        let result = execute(
            serde_json::json!({"question": "Pick a color?"}),
            crate::tool::tests::test_tool_context(std::path::PathBuf::from("/tmp")),
        )
        .unwrap();
        assert!(!result.is_error, "question stub should not be an error");
    }

    #[test]
    fn stub_handler_output_contains_question_text() {
        let result = execute(
            serde_json::json!({"question": "What is your name?"}),
            crate::tool::tests::test_tool_context(std::path::PathBuf::from("/tmp")),
        )
        .unwrap();
        assert!(
            result.output.contains("What is your name?"),
            "output should contain the question: {}",
            result.output
        );
    }

    #[test]
    fn stub_handler_missing_question_uses_fallback() {
        let result = execute(
            serde_json::json!({}),
            crate::tool::tests::test_tool_context(std::path::PathBuf::from("/tmp")),
        )
        .unwrap();
        assert!(
            result.output.contains("(no question provided)"),
            "output should contain fallback text: {}",
            result.output
        );
        assert!(!result.is_error);
    }

    #[test]
    fn tool_definition_parses() {
        let entry = tool();
        assert_eq!(entry.def.name, ToolName::Question);
        assert!(!entry.def.description.is_empty());
    }
}
