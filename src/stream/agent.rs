//! Sub-agent spawning and prompt construction.

use std::sync::Arc;

use tokio::sync::mpsc;

use super::{AgentSpawner, StreamRequest};
use crate::{
    event::{AppEvent, StreamUsage},
    tool::{ToolRegistry, agent::AgentType},
};

/// Run a sub-agent stream, collecting its final text response.
///
/// Creates a fresh conversation with a focused system prompt and restricted tools.
/// Sub-agent events are monitored on a private channel — only permission requests
/// and tool progress are forwarded to the parent. Usage updates are intentionally
/// suppressed to avoid overwriting the parent's context pressure metrics.
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

    let system_prompt = agent_type.build_prompt(task, context);

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
                    Some(AppEvent::LlmUsageUpdate { .. }) => {
                        // Don't forward sub-agent usage to parent — it would overwrite
                        // parent's last_prompt_tokens with the sub-agent's context size,
                        // breaking auto-compact thresholds. Sub-agent totals are tracked
                        // via LlmFinish → sub_agent_usage and returned to the caller.
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

        // Once stream is done, drain any remaining events before exiting.
        if stream_done {
            while let Ok(event) = sub_rx.try_recv() {
                match event {
                    AppEvent::LlmDelta { text } => final_text.push_str(&text),
                    AppEvent::LlmFinish { usage: Some(u) } => {
                        sub_agent_usage += u;
                    }
                    AppEvent::LlmFinish { usage: None } => {}
                    AppEvent::LlmError { error } => {
                        tracing::error!(call_id, %error, "sub-agent error (during drain)");
                        last_error = Some(error);
                    }
                    _ => {}
                }
            }
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
