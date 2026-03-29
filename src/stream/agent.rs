//! Sub-agent spawning and prompt construction.

use std::sync::Arc;

use tokio::sync::mpsc;

use super::{AgentSpawner, StreamRequest};
use crate::{
    event::{AppEvent, StreamUsage},
    tool::{ToolRegistry, agent::AgentType},
};

/// Build a focused system prompt for a sub-agent.
pub(super) fn build_sub_agent_prompt(
    agent_type: AgentType,
    task: &str,
    context: Option<&str>,
) -> String {
    let type_label = match agent_type {
        AgentType::Explore => "exploration",
        AgentType::Plan => "architecture analysis",
        AgentType::General => "implementation",
    };

    let tool_guidance = match agent_type {
        AgentType::Explore => {
            "\
You have read-only tools: read, grep, glob, list, symbols. \
Search efficiently — use grep to find relevant code, then read specific sections. \
Use glob for file discovery. Use symbols for structural queries."
        }
        AgentType::Plan => {
            "\
You have read-only tools plus LSP for semantic analysis: read, grep, glob, list, symbols, lsp. \
Use LSP diagnostics, go-to-definition, and find-references for accurate cross-file analysis. \
Focus on architecture, design, and feasibility."
        }
        AgentType::General => {
            "\
You have full tool access (read, write, edit, bash, etc.). \
Follow the same safety practices as the parent agent. \
Write operations may require user permission."
        }
    };

    let ctx_section = context
        .map(|c| format!("\n\nAdditional context:\n{c}"))
        .unwrap_or_default();

    format!(
        "You are a focused {type_label} sub-agent. Your task:\n\n\
         {task}{ctx_section}\n\n\
         {tool_guidance}\n\n\
         Be concise and thorough. When done, provide a clear summary of your findings or work."
    )
}

/// Run a sub-agent stream, collecting its final text response.
///
/// Creates a fresh conversation with a focused system prompt and restricted tools.
/// Sub-agent events are monitored on a private channel — only usage updates and
/// permission requests are forwarded to the parent.
pub(super) async fn run_sub_agent(
    spawner: &AgentSpawner,
    agent_type: AgentType,
    task: &str,
    context: Option<&str>,
    parent_event_tx: &mpsc::UnboundedSender<AppEvent>,
    call_id: &str,
) -> (Result<String, String>, StreamUsage) {
    let model = match agent_type {
        AgentType::Explore => spawner
            .small_model
            .clone()
            .unwrap_or_else(|| spawner.primary_model.clone()),
        AgentType::Plan | AgentType::General => spawner.primary_model.clone(),
    };

    let allowed = agent_type.allowed_tools();
    let registry = ToolRegistry::filtered(spawner.project_root.clone(), &allowed);
    let child_cancel = spawner.cancel_token.child_token();

    let system_prompt = build_sub_agent_prompt(agent_type, task, context);

    // Private event channel for the sub-agent
    let (sub_tx, mut sub_rx) = mpsc::unbounded_channel::<AppEvent>();
    let (_interjection_tx, interjection_rx) = mpsc::unbounded_channel();

    let sub_request = StreamRequest {
        stream_provider: spawner.stream_provider.clone(),
        model: model.clone(),
        system_prompt: Some(system_prompt),
        history: vec![],
        user_message: task.to_string(),
        event_tx: sub_tx,
        tool_registry: Some(Arc::new(registry)),
        tool_context: Some(spawner.tool_context.clone()),
        permission_engine: if agent_type == AgentType::General {
            spawner.permission_engine.clone()
        } else {
            // Explore/Plan: auto-allow everything (all tools are read-only)
            Some(Arc::new(tokio::sync::Mutex::new(
                crate::permission::PermissionEngine::new(vec![
                    crate::permission::types::PermissionRule {
                        tool: crate::permission::types::ToolMatcher::All,
                        pattern: "*".into(),
                        action: crate::permission::types::PermissionActionSerde::Allow,
                    },
                ]),
            )))
        },
        tool_cache: Arc::new(std::sync::Mutex::new(
            crate::context::cache::ToolResultCache::new(spawner.project_root.clone()),
        )),
        cancel_token: child_cancel.clone(),
        context_window: spawner.context_window,
        interjection_rx,
        usage_writer: spawner.usage_writer.clone(),
        usage_project_id: spawner.usage_project_id.clone(),
        usage_session_id: spawner.usage_session_id.clone(),
        usage_model_cost: None,
        is_plan_mode: matches!(agent_type, AgentType::Plan),
        agent_spawner: None, // No recursion
        // General agents inherit MCP tools; Explore/Plan do not
        mcp_manager: if agent_type == AgentType::General {
            spawner.mcp_manager.clone()
        } else {
            None
        },
    };

    // Run the sub-agent stream concurrently with event processing.
    // Events must be drained during execution (not after) because
    // PermissionRequest events contain oneshot senders that the UI must
    // reply to — draining post-hoc would deadlock on Ask permissions.
    // Box::pin breaks the recursive async type (run_stream → run_sub_agent → run_stream).
    let mut final_text = String::new();
    let mut tool_count = 0u32;
    let mut last_error: Option<String> = None;
    let mut stream_done = false;
    let mut sub_agent_usage = StreamUsage::default();

    let mut stream_future = Box::pin(sub_request.run());

    loop {
        tokio::select! {
            result = &mut stream_future, if !stream_done => {
                let _ = result;
                stream_done = true;
            }
            event = sub_rx.recv() => {
                match event {
                    Some(AppEvent::LlmDelta { text }) => {
                        final_text.push_str(&text);
                    }
                    Some(AppEvent::LlmUsageUpdate { usage }) => {
                        let _ = parent_event_tx.send(AppEvent::LlmUsageUpdate { usage });
                    }
                    Some(AppEvent::LlmToolCall { tool_name, arguments, .. }) => {
                        tool_count += 1;
                        let args_summary = crate::app::extract_args_summary(tool_name, &arguments);
                        let _ = parent_event_tx.send(AppEvent::AgentProgress {
                            call_id: call_id.to_string(),
                            tool_name,
                            args_summary,
                            result_summary: None,
                        });
                    }
                    Some(AppEvent::ToolResult { tool_name, output, .. }) => {
                        // Update progress with the result summary
                        let summary = crate::app::extract_result_summary(tool_name, &output);
                        let _ = parent_event_tx.send(AppEvent::AgentProgress {
                            call_id: call_id.to_string(),
                            tool_name,
                            args_summary: String::new(),
                            result_summary: Some(summary),
                        });
                    }
                    Some(AppEvent::LlmError { error }) => {
                        tracing::error!(call_id, %error, "sub-agent error");
                        last_error = Some(error);
                    }
                    Some(AppEvent::PermissionRequest(req)) => {
                        // Forward permission requests to parent so the UI can prompt the user.
                        let _ = parent_event_tx.send(AppEvent::PermissionRequest(req));
                    }
                    Some(AppEvent::LlmFinish { usage }) => {
                        if let Some(u) = usage {
                            sub_agent_usage += u;
                        }
                    }
                    // Other events (Tick, StreamNotice, etc.) are discarded.
                    Some(_) => {}
                    None => {
                        // Channel closed — sub-agent dropped its sender.
                        break;
                    }
                }
            }
        }

        // Once stream is done and channel is drained, exit.
        if stream_done && sub_rx.is_empty() {
            break;
        }
    }

    let result = if let Some(error) = last_error {
        if !final_text.is_empty() {
            // Prefer returning text even if there was a transient error.
            Ok(format!(
                "{final_text}\n\n[Agent encountered an error: {error}]"
            ))
        } else {
            Err(error)
        }
    } else if final_text.is_empty() {
        Ok(format!(
            "Sub-agent ({agent_type}) completed with {tool_count} tool calls but produced no text response."
        ))
    } else {
        Ok(final_text)
    };
    (result, sub_agent_usage)
}
