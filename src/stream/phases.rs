//! Extracted tool execution phases from `run_stream()`.
//!
//! Four phases execute after streaming completes:
//! 1. Partition tool calls by permission/category
//! 2. Execute auto-allowed read-only tools in parallel
//! 3. Execute permission-required (and sequential-auto) tools sequentially
//! 4. Execute MCP tool calls sequentially

use std::{collections::HashMap, sync::Arc};

use async_openai::types::chat::{
    ChatCompletionRequestMessage, ChatCompletionRequestToolMessage,
    ChatCompletionRequestToolMessageContent,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    context::cache::{CACHE_REPEAT_PREFIX, ToolResultCache},
    event::{AppEvent, StreamUsage},
    permission::{
        PermissionEngine,
        types::{PermissionAction, PermissionReply, PermissionRequest},
    },
    tool::{LspOperation, ToolContext, ToolName, ToolRegistry, agent::AgentType},
};

use super::{
    AgentSpawner,
    agent::run_sub_agent,
    tools::{
        PendingToolCall, build_permission_summary, extract_tool_path, invalidate_write_tool_cache,
        notify_and_diagnose_write_tool,
    },
};

// ── Shared types ───────────────────────────────────────────────────────────

/// A tool call that has been parsed, permission-checked, and is ready for execution.
pub(super) struct PreparedToolCall {
    pub idx: u32,
    pub id: String,
    pub tool_name: ToolName,
    pub args: Value,
    pub action: PermissionAction,
}

/// An MCP tool call that has been parsed and permission-checked.
pub(super) struct McpPreparedToolCall {
    pub id: String,
    pub prefixed_name: String,
    pub args: Value,
    pub action: PermissionAction,
}

/// Result of Phase 1: tool calls partitioned by execution strategy.
pub(super) struct PartitionedCalls {
    pub auto_allowed: Vec<PreparedToolCall>,
    pub needs_interaction: Vec<PreparedToolCall>,
    pub mcp_calls: Vec<McpPreparedToolCall>,
}

/// Mutable counters shared across phases within a single loop iteration.
pub(super) struct IterationCounters {
    pub tool_count: usize,
    pub cache_repeats: usize,
    pub user_interacted: bool,
    pub total_usage: StreamUsage,
}

/// Outcome of a phase that may need to terminate the stream early.
pub(super) enum PhaseOutcome {
    /// Continue to the next phase / iteration.
    Continue,
    /// Stream was cancelled — caller should `return Ok(())`.
    Cancelled,
}

/// Whether an LSP tool call's `operation` arg is a read-only operation.
///
/// `diagnostics`, `definition`, and `references` are pure reads safe for
/// parallel execution. `rename` is heavier and stays sequential.
fn is_lsp_read_op(args: &Value) -> bool {
    let op: LspOperation = args
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("diagnostics")
        .parse()
        .unwrap_or(LspOperation::Diagnostics);
    matches!(
        op,
        LspOperation::Diagnostics | LspOperation::Definition | LspOperation::References
    )
}

// ── Phase 1: Partition tool calls ──────────────────────────────────────────

/// Pre-check permissions and partition tool calls into categories.
///
/// Unknown tool names get an error tool-result message pushed to `messages`.
/// Valid calls are split into auto-allowed (parallel-eligible), needs-interaction
/// (sequential), and MCP calls.
// Structural — all parameters are distinct coordination concerns (tools, permissions, state, events)
#[allow(clippy::too_many_arguments)]
pub(super) async fn partition_tool_calls(
    pending_tool_calls: &HashMap<u32, PendingToolCall>,
    sorted_indices: &[u32],
    mcp_snapshot: &Option<std::sync::Arc<crate::mcp::McpToolSnapshot>>,
    permission_engine: &Option<Arc<tokio::sync::Mutex<PermissionEngine>>>,
    tool_context: &Option<ToolContext>,
    messages: &mut Vec<ChatCompletionRequestMessage>,
    counters: &mut IterationCounters,
) -> PartitionedCalls {
    let mut prepared: Vec<PreparedToolCall> = Vec::new();
    let mut mcp_pending: Vec<McpPreparedToolCall> = Vec::new();

    for idx in sorted_indices {
        let tc = &pending_tool_calls[idx];
        let args: Value = serde_json::from_str(&tc.arguments).unwrap_or(Value::Null);

        let tool_name: ToolName = match tc.function_name.parse() {
            Ok(name) => name,
            Err(_) => {
                // Check MCP snapshot (lock-free) before returning error
                let is_mcp = mcp_snapshot
                    .as_ref()
                    .is_some_and(|snap| snap.has_tool(&tc.function_name));

                if is_mcp {
                    // Route to MCP execution
                    let action = if let Some(engine) = permission_engine {
                        let engine = engine.lock().await;
                        engine.check_mcp(&tc.function_name)
                    } else {
                        PermissionAction::Allow
                    };
                    mcp_pending.push(McpPreparedToolCall {
                        id: tc.id.clone(),
                        prefixed_name: tc.function_name.clone(),
                        args: args.clone(),
                        action,
                    });
                    continue;
                }

                tracing::warn!(tool = %tc.function_name, "unknown tool name from LLM, returning error");
                // Must provide a tool result for every tool_call_id in the assistant message
                messages.push(ChatCompletionRequestMessage::Tool(
                    ChatCompletionRequestToolMessage {
                        content: ChatCompletionRequestToolMessageContent::Text(format!(
                            "Error: unknown tool '{}'",
                            tc.function_name
                        )),
                        tool_call_id: tc.id.clone(),
                    },
                ));
                counters.tool_count += 1;
                continue;
            }
        };

        let raw_path = extract_tool_path(tool_name, &args);
        let (path_hint, inside_project) = match raw_path {
            Some(raw) => {
                if let Some(ctx) = tool_context {
                    let (normalized, inside) =
                        crate::permission::normalize_tool_path(&raw, &ctx.project_root);
                    (Some(normalized), Some(inside))
                } else {
                    (Some(raw), None)
                }
            }
            None => (None, None),
        };
        let action = if let Some(engine) = permission_engine {
            let engine = engine.lock().await;
            engine.check(tool_name, path_hint.as_deref(), inside_project)
        } else {
            PermissionAction::Allow
        };

        prepared.push(PreparedToolCall {
            idx: *idx,
            id: tc.id.clone(),
            tool_name,
            args,
            action,
        });
    }

    // Partition: auto-allowed read-only tools can run in parallel.
    // Write tools (edit, write, patch), memory tool (append action),
    // task tool (writes to storage), and LSP rename (heavier mutex hold)
    // always go to sequential phase. LSP read operations (diagnostics,
    // definition, references) are safe for parallel execution.
    let (auto_allowed, needs_interaction): (Vec<_>, Vec<_>) =
        prepared.into_iter().partition(|tc| {
            matches!(tc.action, PermissionAction::Allow)
                && !tc.tool_name.is_write_tool()
                && !tc.tool_name.is_memory()
                && !tc.tool_name.is_task()
                && !matches!(tc.tool_name, ToolName::Question | ToolName::Agent)
                && (tc.tool_name != ToolName::Lsp || is_lsp_read_op(&tc.args))
        });

    // Log partition results for diagnostics
    if !needs_interaction.is_empty() {
        let tool_names: Vec<ToolName> = needs_interaction.iter().map(|tc| tc.tool_name).collect();
        tracing::info!(
            count = needs_interaction.len(),
            tools = ?tool_names,
            "tools requiring sequential/permission handling"
        );
    }

    PartitionedCalls {
        auto_allowed,
        needs_interaction,
        mcp_calls: mcp_pending,
    }
}

// ── Phase 2: Execute auto-allowed tools in parallel ────────────────────────

/// Execute auto-allowed (read-only, pre-permitted) tools in parallel.
///
/// Returns `PhaseOutcome::Cancelled` if the cancel token fires during result processing.
// Structural — each parameter is a distinct resource (registry, cache, events, cancellation, counters)
#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_parallel_tools(
    tools: &[PreparedToolCall],
    registry: &Arc<ToolRegistry>,
    ctx: &ToolContext,
    tool_cache: &Arc<std::sync::Mutex<ToolResultCache>>,
    cancel_token: &CancellationToken,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    messages: &mut Vec<ChatCompletionRequestMessage>,
    counters: &mut IterationCounters,
) -> PhaseOutcome {
    // First, check cache for each. Only spawn tasks for cache misses.
    struct ParallelTask {
        id: String,
        tool_name: ToolName,
        args: Value,
    }

    let mut parallel_results: HashMap<String, crate::tool::ToolOutput> = HashMap::new();
    let mut tasks_to_spawn: Vec<ParallelTask> = Vec::new();

    for tc in tools {
        if let Some(cached) = tool_cache
            .lock()
            .expect("lock poisoned")
            .get(tc.tool_name, &tc.args)
        {
            tracing::debug!(tool = %tc.tool_name, "using cached result (parallel)");
            if cached.output.starts_with(CACHE_REPEAT_PREFIX) {
                counters.cache_repeats += 1;
            }
            parallel_results.insert(tc.id.clone(), cached);
        } else {
            tasks_to_spawn.push(ParallelTask {
                id: tc.id.clone(),
                tool_name: tc.tool_name,
                args: tc.args.clone(),
            });
        }
    }

    if !tasks_to_spawn.is_empty() {
        tracing::info!(
            count = tasks_to_spawn.len(),
            cached = tools.len() - tasks_to_spawn.len(),
            "executing auto-allowed tools in parallel"
        );

        // Spawn all cache-miss tools in parallel via spawn_blocking
        let handles: Vec<(
            String,
            ToolName,
            Value,
            tokio::task::JoinHandle<anyhow::Result<crate::tool::ToolOutput>>,
        )> = tasks_to_spawn
            .into_iter()
            .map(|task| {
                let reg = registry.clone();
                let c = ctx.clone();
                let name = task.tool_name;
                let a = task.args.clone();
                let handle = tokio::task::spawn_blocking(move || reg.execute(name, a, c));
                (task.id, task.tool_name, task.args, handle)
            })
            .collect();

        // Collect results
        for (call_id, tool_name, args, handle) in handles {
            let output = match handle.await {
                Ok(Ok(output)) => output,
                Ok(Err(e)) => {
                    tracing::error!(tool = %tool_name, error = %e, "tool execution failed");
                    crate::tool::ToolOutput {
                        title: tool_name.to_string(),
                        output: format!("Error: {e}"),
                        is_error: true,
                    }
                }
                Err(e) => {
                    tracing::error!(tool = %tool_name, error = %e, "task join failed");
                    crate::tool::ToolOutput {
                        title: tool_name.to_string(),
                        output: format!("Error: task panicked: {e}"),
                        is_error: true,
                    }
                }
            };

            // Cache the result
            tool_cache
                .lock()
                .expect("lock poisoned")
                .put(tool_name, &args, &output);

            parallel_results.insert(call_id, output);
        }
    }

    // Emit events and add results for auto-allowed tools (in original order)
    for tc in tools {
        if cancel_token.is_cancelled() {
            tracing::info!("stream cancelled during tool result processing");
            let _ = event_tx.send(AppEvent::LlmFinish {
                usage: Some(counters.total_usage.clone()),
            });
            return PhaseOutcome::Cancelled;
        }

        let _ = event_tx.send(AppEvent::LlmToolCall {
            call_id: tc.id.clone(),
            tool_name: tc.tool_name,
            arguments: tc.args.clone(),
        });

        let output = parallel_results.remove(&tc.id).unwrap_or_else(|| {
            tracing::error!(
                tool = %tc.tool_name,
                call_id = %tc.id,
                "missing parallel result — inserting error to preserve conversation structure"
            );
            crate::tool::ToolOutput {
                title: tc.tool_name.to_string(),
                output: "Error: tool execution result was lost".to_string(),
                is_error: true,
            }
        });

        let _ = event_tx.send(AppEvent::ToolResult {
            call_id: tc.id.clone(),
            tool_name: tc.tool_name,
            output: output.clone(),
        });

        messages.push(ChatCompletionRequestMessage::Tool(
            ChatCompletionRequestToolMessage {
                content: ChatCompletionRequestToolMessageContent::Text(output.output),
                tool_call_id: tc.id.clone(),
            },
        ));
        counters.tool_count += 1;
    }

    PhaseOutcome::Continue
}

// ── Phase 3: Execute permission-required tools sequentially ────────────────

/// Execute tools that need user permission or must run sequentially.
///
/// This handles Question tool, Agent tool, Deny/Ask/Allow permission actions,
/// cache checking, and cache invalidation for write tools.
///
/// Returns `PhaseOutcome::Cancelled` if the cancel token fires.
// Structural — sequential execution requires all coordination handles (registry, permissions, cache, events, agents)
#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_sequential_tools(
    tools: &[PreparedToolCall],
    registry: &Arc<ToolRegistry>,
    ctx: &ToolContext,
    tool_cache: &Arc<std::sync::Mutex<ToolResultCache>>,
    cancel_token: &CancellationToken,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    permission_engine: &Option<Arc<tokio::sync::Mutex<PermissionEngine>>>,
    agent_spawner: &Option<AgentSpawner>,
    messages: &mut Vec<ChatCompletionRequestMessage>,
    counters: &mut IterationCounters,
) -> PhaseOutcome {
    // Pre-spawn auto-allowed agents (Explore/Plan) in parallel, await all in
    // completion order, and collect results. ToolResult events are sent as each
    // agent finishes so the UI shows results immediately, not in call order.
    let mut agent_results: HashMap<String, crate::tool::ToolOutput> = HashMap::new();
    let mut agent_results_sent: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    {
        type AgentHandle =
            tokio::task::JoinHandle<(String, AgentType, Result<String, String>, StreamUsage)>;
        let mut handles: Vec<AgentHandle> = Vec::new();

        if let Some(spawner) = agent_spawner {
            for tc in tools {
                if !matches!(tc.tool_name, ToolName::Agent) {
                    continue;
                }
                let agent_type_str = tc
                    .args
                    .get("agent_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("explore");
                let agent_type: AgentType = agent_type_str.parse().unwrap_or(AgentType::Explore);

                // Only pre-spawn agents that don't need user permission
                if agent_type == AgentType::General && matches!(tc.action, PermissionAction::Ask) {
                    continue;
                }

                let _ = event_tx.send(AppEvent::LlmToolCall {
                    call_id: tc.id.clone(),
                    tool_name: tc.tool_name,
                    arguments: tc.args.clone(),
                });

                let task_str = tc
                    .args
                    .get("task")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(no task provided)")
                    .to_string();
                let context_str = tc
                    .args
                    .get("context")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let spawner = spawner.clone();
                let parent_tx = event_tx.clone();
                let call_id = tc.id.clone();

                let handle = tokio::spawn(async move {
                    let (result, usage) = run_sub_agent(
                        &spawner,
                        agent_type,
                        &task_str,
                        context_str.as_deref(),
                        &parent_tx,
                        &call_id,
                    )
                    .await;
                    (call_id, agent_type, result, usage)
                });
                handles.push(handle);
            }
        }

        if handles.len() > 1 {
            tracing::info!(count = handles.len(), "running agents in parallel");
        }

        // Await in completion order — send ToolResult to UI as each finishes.
        // Observe cancel_token so cancellation doesn't block on slow agents.
        let mut remaining = handles;
        while !remaining.is_empty() {
            let select_future = futures_util::future::select_all(remaining);
            let completed = tokio::select! {
                _ = cancel_token.cancelled() => {
                    // select_all consumed remaining; abort via the returned rest
                    break;
                }
                (result, _index, rest) = select_future => {
                    remaining = rest;
                    result
                }
            };

            match completed {
                Ok((call_id, agent_type, result, usage)) => {
                    counters.total_usage += usage;
                    let output = match result {
                        Ok(text) => crate::tool::ToolOutput {
                            title: format!("Agent ({agent_type})"),
                            output: text,
                            is_error: false,
                        },
                        Err(e) => crate::tool::ToolOutput {
                            title: format!("Agent ({agent_type})"),
                            output: format!("Agent error: {e}"),
                            is_error: true,
                        },
                    };
                    let _ = event_tx.send(AppEvent::ToolResult {
                        call_id: call_id.clone(),
                        tool_name: ToolName::Agent,
                        output: output.clone(),
                    });
                    agent_results_sent.insert(call_id.clone());
                    agent_results.insert(call_id, output);
                }
                Err(e) => {
                    tracing::error!(error = %e, "agent task panicked");
                }
            }
        }
    }

    for tc in tools {
        if cancel_token.is_cancelled() {
            tracing::info!("stream cancelled during tool execution");
            let _ = event_tx.send(AppEvent::LlmFinish {
                usage: Some(counters.total_usage.clone()),
            });
            return PhaseOutcome::Cancelled;
        }

        // Question tool: special interactive flow (bypass permission)
        if matches!(tc.tool_name, ToolName::Question) {
            let question = tc
                .args
                .get("question")
                .and_then(|v| v.as_str())
                .unwrap_or("(no question provided)")
                .to_string();
            let options: Vec<String> = tc
                .args
                .get("options")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            // Emit LlmToolCall so the UI shows this in the tool group
            let _ = event_tx.send(AppEvent::LlmToolCall {
                call_id: tc.id.clone(),
                tool_name: tc.tool_name,
                arguments: tc.args.clone(),
            });

            // Create oneshot channel for the user's response
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();

            let _ = event_tx.send(AppEvent::QuestionRequest(crate::event::QuestionRequest {
                call_id: tc.id.clone(),
                question,
                options,
                response_tx,
            }));

            // Wait for user response or cancellation
            let answer = tokio::select! {
                _ = cancel_token.cancelled() => {
                    tracing::info!("stream cancelled while waiting for question answer");
                    let _ = event_tx.send(AppEvent::LlmFinish {
                        usage: Some(counters.total_usage.clone()),
                    });
                    return PhaseOutcome::Cancelled;
                }
                reply = response_rx => {
                    match reply {
                        Ok(answer) => answer,
                        Err(_) => "User declined to answer.".to_string(),
                    }
                }
            };

            let output = crate::tool::ToolOutput {
                title: "Question".to_string(),
                output: answer,
                is_error: false,
            };

            let _ = event_tx.send(AppEvent::ToolResult {
                call_id: tc.id.clone(),
                tool_name: tc.tool_name,
                output: output.clone(),
            });

            messages.push(ChatCompletionRequestMessage::Tool(
                ChatCompletionRequestToolMessage {
                    content: ChatCompletionRequestToolMessageContent::Text(output.output),
                    tool_call_id: tc.id.clone(),
                },
            ));
            counters.tool_count += 1;
            continue;
        }

        // Agent tool: await pre-spawned handle or run sequentially (General with permission)
        if matches!(tc.tool_name, ToolName::Agent) {
            let agent_type_str = tc
                .args
                .get("agent_type")
                .and_then(|v| v.as_str())
                .unwrap_or("explore");

            let output = if let Some(output) = agent_results.remove(&tc.id) {
                // Already completed in parallel — ToolResult already sent to UI
                output
            } else if let Some(spawner) = agent_spawner {
                // General agent needing permission — runs sequentially
                let _ = event_tx.send(AppEvent::LlmToolCall {
                    call_id: tc.id.clone(),
                    tool_name: tc.tool_name,
                    arguments: tc.args.clone(),
                });

                let agent_type: AgentType = agent_type_str.parse().unwrap_or(AgentType::Explore);
                let task_str = tc
                    .args
                    .get("task")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(no task provided)")
                    .to_string();
                let context_str = tc
                    .args
                    .get("context")
                    .and_then(|v| v.as_str())
                    .map(String::from);

                // General agents in Ask mode need permission
                if agent_type == AgentType::General && matches!(tc.action, PermissionAction::Ask) {
                    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
                    let summary = build_permission_summary(tc.tool_name, &tc.args);

                    let _ = event_tx.send(AppEvent::PermissionRequest(PermissionRequest {
                        call_id: tc.id.clone(),
                        tool_name: tc.tool_name,
                        display_name: None,
                        arguments_summary: summary,
                        tool_args: tc.args.clone(),
                        response_tx,
                    }));

                    let permitted = tokio::select! {
                        _ = cancel_token.cancelled() => {
                            let _ = event_tx.send(AppEvent::LlmFinish { usage: Some(counters.total_usage.clone()) });
                            return PhaseOutcome::Cancelled;
                        }
                        reply = response_rx => {
                            match reply {
                                Ok(PermissionReply::AllowOnce) | Ok(PermissionReply::AllowAlways) => {
                                    counters.user_interacted = true;
                                    if let Ok(PermissionReply::AllowAlways) = reply.as_ref()
                                        && let Some(engine) = permission_engine {
                                            engine.lock().await.grant_session(tc.tool_name);
                                        }
                                    true
                                }
                                _ => false,
                            }
                        }
                    };

                    if !permitted {
                        crate::tool::ToolOutput {
                            title: "Agent".to_string(),
                            output: "Permission denied by user for agent tool.".to_string(),
                            is_error: true,
                        }
                    } else {
                        let (result, usage) = run_sub_agent(
                            spawner,
                            agent_type,
                            &task_str,
                            context_str.as_deref(),
                            event_tx,
                            &tc.id,
                        )
                        .await;
                        counters.total_usage += usage;
                        match result {
                            Ok(text) => crate::tool::ToolOutput {
                                title: format!("Agent ({agent_type})"),
                                output: text,
                                is_error: false,
                            },
                            Err(e) => crate::tool::ToolOutput {
                                title: format!("Agent ({agent_type})"),
                                output: format!("Agent error: {e}"),
                                is_error: true,
                            },
                        }
                    }
                } else {
                    // Explore/Plan agents or already-allowed General agents
                    let (result, usage) = run_sub_agent(
                        spawner,
                        agent_type,
                        &task_str,
                        context_str.as_deref(),
                        event_tx,
                        &tc.id,
                    )
                    .await;
                    counters.total_usage += usage;
                    match result {
                        Ok(text) => crate::tool::ToolOutput {
                            title: format!("Agent ({agent_type})"),
                            output: text,
                            is_error: false,
                        },
                        Err(e) => crate::tool::ToolOutput {
                            title: format!("Agent ({agent_type})"),
                            output: format!("Agent error: {e}"),
                            is_error: true,
                        },
                    }
                }
            } else {
                // No agent_spawner — we're already a sub-agent, can't recurse
                crate::tool::ToolOutput {
                    title: "Agent".to_string(),
                    output: "Error: agent tool is not available in sub-agent context.".to_string(),
                    is_error: true,
                }
            };

            // Pre-completed agents already had ToolResult sent in the
            // completion-order loop above; only send for sequential agents.
            if !agent_results_sent.contains(&tc.id) {
                let _ = event_tx.send(AppEvent::ToolResult {
                    call_id: tc.id.clone(),
                    tool_name: tc.tool_name,
                    output: output.clone(),
                });
            }

            messages.push(ChatCompletionRequestMessage::Tool(
                ChatCompletionRequestToolMessage {
                    content: ChatCompletionRequestToolMessageContent::Text(output.output),
                    tool_call_id: tc.id.clone(),
                },
            ));
            counters.tool_count += 1;
            continue;
        }

        match tc.action {
            PermissionAction::Deny => {
                tracing::info!(tool = %tc.tool_name, "tool call denied by policy");
                let _ = event_tx.send(AppEvent::LlmToolCall {
                    call_id: tc.id.clone(),
                    tool_name: tc.tool_name,
                    arguments: tc.args.clone(),
                });

                let output = crate::tool::ToolOutput {
                    title: tc.tool_name.to_string(),
                    output: format!(
                        "Permission denied: {} is not allowed in the current mode.",
                        tc.tool_name
                    ),
                    is_error: true,
                };

                let _ = event_tx.send(AppEvent::ToolResult {
                    call_id: tc.id.clone(),
                    tool_name: tc.tool_name,
                    output: output.clone(),
                });

                messages.push(ChatCompletionRequestMessage::Tool(
                    ChatCompletionRequestToolMessage {
                        content: ChatCompletionRequestToolMessageContent::Text(output.output),
                        tool_call_id: tc.id.clone(),
                    },
                ));
                counters.tool_count += 1;
            }
            PermissionAction::Ask => {
                let (response_tx, response_rx) = tokio::sync::oneshot::channel();
                let summary = build_permission_summary(tc.tool_name, &tc.args);

                let _ = event_tx.send(AppEvent::PermissionRequest(PermissionRequest {
                    call_id: tc.id.clone(),
                    tool_name: tc.tool_name,
                    display_name: None,
                    arguments_summary: summary,
                    tool_args: tc.args.clone(),
                    response_tx,
                }));

                tokio::select! {
                    _ = cancel_token.cancelled() => {
                        tracing::info!("stream cancelled while waiting for permission");
                        let _ = event_tx.send(AppEvent::LlmFinish {
                            usage: Some(counters.total_usage.clone()),
                        });
                        return PhaseOutcome::Cancelled;
                    }
                    reply = response_rx => {
                        match reply {
                            Ok(PermissionReply::AllowOnce) => {
                                tracing::info!(tool = %tc.tool_name, "permission granted (once)");
                                counters.user_interacted = true;
                            }
                            Ok(PermissionReply::AllowAlways) => {
                                tracing::info!(tool = %tc.tool_name, "permission granted (always)");
                                counters.user_interacted = true;
                                if let Some(engine) = permission_engine {
                                    let mut engine = engine.lock().await;
                                    engine.grant_session(tc.tool_name);
                                }
                            }
                            Ok(PermissionReply::Deny) | Err(_) => {
                                tracing::info!(tool = %tc.tool_name, "permission denied by user");
                                let output = crate::tool::ToolOutput {
                                    title: tc.tool_name.to_string(),
                                    output: format!("Permission denied by user for: {}", tc.tool_name),
                                    is_error: true,
                                };

                                let _ = event_tx.send(AppEvent::ToolResult {
                                    call_id: tc.id.clone(),
                                    tool_name: tc.tool_name,
                                    output: output.clone(),
                                });

                                messages.push(ChatCompletionRequestMessage::Tool(
                                    ChatCompletionRequestToolMessage {
                                        content: ChatCompletionRequestToolMessageContent::Text(output.output),
                                        tool_call_id: tc.id.clone(),
                                    },
                                ));
                                counters.tool_count += 1;
                                continue;
                            }
                        }
                    }
                }

                // Permission granted — execute the tool
                let _ = event_tx.send(AppEvent::LlmToolCall {
                    call_id: tc.id.clone(),
                    tool_name: tc.tool_name,
                    arguments: tc.args.clone(),
                });

                tracing::info!(tool = %tc.tool_name, "executing tool");

                let output = if let Some(cached) = tool_cache
                    .lock()
                    .expect("lock poisoned")
                    .get(tc.tool_name, &tc.args)
                {
                    if cached.output.starts_with(CACHE_REPEAT_PREFIX) {
                        counters.cache_repeats += 1;
                    }
                    cached
                } else {
                    // Use spawn_blocking so tools that call block_on() (e.g.
                    // LSP) don't panic from within the tokio runtime.
                    let reg = registry.clone();
                    let name = tc.tool_name;
                    let a = tc.args.clone();
                    let c = ctx.clone();
                    let mut result =
                        match tokio::task::spawn_blocking(move || reg.execute(name, a, c)).await {
                            Ok(Ok(output)) => output,
                            Ok(Err(e)) => {
                                tracing::error!(tool = %tc.tool_name, error = %e, "tool execution failed");
                                crate::tool::ToolOutput {
                                    title: tc.tool_name.to_string(),
                                    output: format!("Error: {e}"),
                                    is_error: true,
                                }
                            }
                            Err(e) => {
                                tracing::error!(tool = %tc.tool_name, error = %e, "tool task panicked");
                                crate::tool::ToolOutput {
                                    title: tc.tool_name.to_string(),
                                    output: format!("Error: tool task panicked: {e}"),
                                    is_error: true,
                                }
                            }
                        };

                    let mut cache = tool_cache.lock().expect("lock poisoned");
                    cache.put(tc.tool_name, &tc.args, &result);

                    // Invalidate cache entries when write operations modify files
                    invalidate_write_tool_cache(tc.tool_name, &tc.args, &mut cache);
                    drop(cache);

                    // Notify LSP and append diagnostics for write tools
                    append_lsp_diagnostics(&mut result, tc.tool_name, &tc.args, ctx, event_tx);

                    result
                };

                let _ = event_tx.send(AppEvent::ToolResult {
                    call_id: tc.id.clone(),
                    tool_name: tc.tool_name,
                    output: output.clone(),
                });

                messages.push(ChatCompletionRequestMessage::Tool(
                    ChatCompletionRequestToolMessage {
                        content: ChatCompletionRequestToolMessageContent::Text(output.output),
                        tool_call_id: tc.id.clone(),
                    },
                ));
                counters.tool_count += 1;
            }
            PermissionAction::Allow => {
                // Normally handled in Phase 2, but write tools with AllowAlways
                // are routed here for cache invalidation. Execute normally.
                let _ = event_tx.send(AppEvent::LlmToolCall {
                    call_id: tc.id.clone(),
                    tool_name: tc.tool_name,
                    arguments: tc.args.clone(),
                });

                tracing::info!(tool = %tc.tool_name, "executing allowed tool (sequential)");

                let output = if let Some(cached) = tool_cache
                    .lock()
                    .expect("lock poisoned")
                    .get(tc.tool_name, &tc.args)
                {
                    if cached.output.starts_with(CACHE_REPEAT_PREFIX) {
                        counters.cache_repeats += 1;
                    }
                    cached
                } else {
                    // Use spawn_blocking so tools that call block_on() (e.g.
                    // LSP) don't panic from within the tokio runtime.
                    let reg = registry.clone();
                    let name = tc.tool_name;
                    let a = tc.args.clone();
                    let c = ctx.clone();
                    let mut result =
                        match tokio::task::spawn_blocking(move || reg.execute(name, a, c)).await {
                            Ok(Ok(output)) => output,
                            Ok(Err(e)) => {
                                tracing::error!(tool = %tc.tool_name, error = %e, "tool execution failed");
                                crate::tool::ToolOutput {
                                    title: tc.tool_name.to_string(),
                                    output: format!("Error: {e}"),
                                    is_error: true,
                                }
                            }
                            Err(e) => {
                                tracing::error!(tool = %tc.tool_name, error = %e, "tool task panicked");
                                crate::tool::ToolOutput {
                                    title: tc.tool_name.to_string(),
                                    output: format!("Error: tool task panicked: {e}"),
                                    is_error: true,
                                }
                            }
                        };

                    let mut cache = tool_cache.lock().expect("lock poisoned");
                    cache.put(tc.tool_name, &tc.args, &result);

                    // Invalidate cache entries when write operations modify files
                    invalidate_write_tool_cache(tc.tool_name, &tc.args, &mut cache);
                    drop(cache);

                    // Notify LSP and append diagnostics for write tools
                    append_lsp_diagnostics(&mut result, tc.tool_name, &tc.args, ctx, event_tx);

                    result
                };

                let _ = event_tx.send(AppEvent::ToolResult {
                    call_id: tc.id.clone(),
                    tool_name: tc.tool_name,
                    output: output.clone(),
                });

                messages.push(ChatCompletionRequestMessage::Tool(
                    ChatCompletionRequestToolMessage {
                        content: ChatCompletionRequestToolMessageContent::Text(output.output),
                        tool_call_id: tc.id.clone(),
                    },
                ));
                counters.tool_count += 1;
            }
        }
    }

    PhaseOutcome::Continue
}

/// Notify LSP of file changes after a write tool and append diagnostics.
/// Skips when the tool itself errored (file may not have been written).
fn append_lsp_diagnostics(
    result: &mut crate::tool::ToolOutput,
    tool_name: ToolName,
    args: &Value,
    ctx: &crate::tool::ToolContext,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
) {
    if result.is_error {
        return;
    }
    if let Some((error_count, diag_summary)) = notify_and_diagnose_write_tool(tool_name, args, ctx)
    {
        result.output.push_str(&diag_summary);
        let path = args
            .get("file_path")
            .or_else(|| args.get("to_path"))
            .or_else(|| args.get("path"))
            .and_then(|v| v.as_str())
            .unwrap_or("file");
        let _ = event_tx.send(AppEvent::StreamNotice {
            text: format!("⚠ LSP: {error_count} new error(s) in {path}"),
        });
    }
}

// ── Phase 4: Execute MCP tool calls sequentially ───────────────────────────

/// Execute MCP tool calls sequentially (external IPC, always sequential).
///
/// Returns `PhaseOutcome::Cancelled` if the cancel token fires.
// Structural — MCP execution needs all coordination handles (manager, permissions, events, counters)
#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_mcp_tools(
    tools: &[McpPreparedToolCall],
    mcp_manager: &Option<Arc<tokio::sync::Mutex<crate::mcp::McpManager>>>,
    cancel_token: &CancellationToken,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    permission_engine: &Option<Arc<tokio::sync::Mutex<PermissionEngine>>>,
    messages: &mut Vec<ChatCompletionRequestMessage>,
    counters: &mut IterationCounters,
) -> PhaseOutcome {
    for mcp_tc in tools {
        if cancel_token.is_cancelled() {
            tracing::info!("stream cancelled during MCP tool execution");
            let _ = event_tx.send(AppEvent::LlmFinish {
                usage: Some(counters.total_usage.clone()),
            });
            return PhaseOutcome::Cancelled;
        }

        // Handle permission
        if matches!(mcp_tc.action, PermissionAction::Deny) {
            messages.push(ChatCompletionRequestMessage::Tool(
                ChatCompletionRequestToolMessage {
                    content: ChatCompletionRequestToolMessageContent::Text(
                        "Error: MCP tool call denied by permission policy.".into(),
                    ),
                    tool_call_id: mcp_tc.id.clone(),
                },
            ));
            counters.tool_count += 1;
            continue;
        }

        if matches!(mcp_tc.action, PermissionAction::Ask) {
            // Send permission request to UI
            let (display_name, args_preview) =
                crate::mcp::mcp_permission_parts(&mcp_tc.prefixed_name, &mcp_tc.args);
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            let _ = event_tx.send(AppEvent::PermissionRequest(PermissionRequest {
                call_id: mcp_tc.id.clone(),
                tool_name: ToolName::Bash, // placeholder — not displayed (display_name wins)
                display_name: Some(display_name),
                arguments_summary: args_preview,
                tool_args: mcp_tc.args.clone(),
                response_tx: reply_tx,
            }));

            match reply_rx.await {
                Ok(PermissionReply::AllowOnce) => {
                    counters.user_interacted = true;
                }
                Ok(PermissionReply::AllowAlways) => {
                    counters.user_interacted = true;
                    if let Some(engine) = permission_engine {
                        let mut engine = engine.lock().await;
                        engine.grant_mcp_session(mcp_tc.prefixed_name.clone());
                    }
                }
                Ok(PermissionReply::Deny) | Err(_) => {
                    messages.push(ChatCompletionRequestMessage::Tool(
                        ChatCompletionRequestToolMessage {
                            content: ChatCompletionRequestToolMessageContent::Text(
                                "Error: MCP tool call denied by user.".into(),
                            ),
                            tool_call_id: mcp_tc.id.clone(),
                        },
                    ));
                    counters.tool_count += 1;
                    continue;
                }
            }
        }

        // Execute the MCP tool call.
        // The tokio::sync::Mutex is held across the await — this is safe (it's
        // designed for this), and the lock is sequential with other MCP calls in
        // this iteration. The snapshot handles all read-only access without locking.
        let (result_text, is_error) = if let Some(mgr) = mcp_manager {
            let mgr = mgr.lock().await;
            match mgr
                .call_tool(&mcp_tc.prefixed_name, mcp_tc.args.clone())
                .await
            {
                Ok((text, is_err)) => (text, is_err),
                Err(e) => (format!("MCP tool error: {e}"), true),
            }
        } else {
            ("Error: MCP manager not available".into(), true)
        };

        // Emit a notice for UI feedback. We intentionally do NOT emit
        // AppEvent::LlmToolCall/ToolResult with a placeholder ToolName (e.g., Bash)
        // because that would misclassify MCP calls in the UI: wrong tool grouping,
        // wrong gutter markers, and app-level side effects (e.g., git refresh on
        // bash success) would trigger incorrectly.
        let mcp_summary = crate::mcp::mcp_permission_summary(&mcp_tc.prefixed_name, &mcp_tc.args);
        if is_error {
            let _ = event_tx.send(AppEvent::StreamNotice {
                text: format!("⚠ {mcp_summary} → error"),
            });
        } else {
            let _ = event_tx.send(AppEvent::StreamNotice {
                text: format!("⚙ {mcp_summary} → ok"),
            });
        }

        messages.push(ChatCompletionRequestMessage::Tool(
            ChatCompletionRequestToolMessage {
                content: ChatCompletionRequestToolMessageContent::Text(result_text),
                tool_call_id: mcp_tc.id.clone(),
            },
        ));
        counters.tool_count += 1;
    }

    PhaseOutcome::Continue
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pending(idx: u32, function_name: &str, args: &str) -> (u32, PendingToolCall) {
        (
            idx,
            PendingToolCall {
                id: format!("call_{idx}"),
                function_name: function_name.to_string(),
                arguments: args.to_string(),
            },
        )
    }

    fn make_counters() -> IterationCounters {
        IterationCounters {
            tool_count: 0,
            cache_repeats: 0,
            user_interacted: false,
            total_usage: StreamUsage::default(),
        }
    }

    #[tokio::test]
    async fn partition_read_tools_are_auto_allowed() {
        let calls: HashMap<u32, PendingToolCall> =
            [make_pending(0, "read", r#"{"path":"src/main.rs"}"#)].into();
        let indices = vec![0];
        let mut messages = Vec::new();
        let mut counters = make_counters();

        let result = partition_tool_calls(
            &calls,
            &indices,
            &None,
            &None,
            &None,
            &mut messages,
            &mut counters,
        )
        .await;

        assert_eq!(result.auto_allowed.len(), 1);
        assert!(result.needs_interaction.is_empty());
        assert!(result.mcp_calls.is_empty());
        assert_eq!(result.auto_allowed[0].tool_name, ToolName::Read);
    }

    #[tokio::test]
    async fn partition_write_tools_need_interaction() {
        let calls: HashMap<u32, PendingToolCall> = [
            make_pending(0, "edit", r#"{"file_path":"f.rs"}"#),
            make_pending(1, "write", r#"{"file_path":"f.rs"}"#),
            make_pending(2, "patch", r#"{"file_path":"f.rs"}"#),
        ]
        .into();
        let indices = vec![0, 1, 2];
        let mut messages = Vec::new();
        let mut counters = make_counters();

        let result = partition_tool_calls(
            &calls,
            &indices,
            &None,
            &None,
            &None,
            &mut messages,
            &mut counters,
        )
        .await;

        assert!(result.auto_allowed.is_empty());
        assert_eq!(result.needs_interaction.len(), 3);
    }

    #[tokio::test]
    async fn partition_sequential_tools_need_interaction() {
        // question, lsp rename, and agent should all go to sequential phase
        let calls: HashMap<u32, PendingToolCall> = [
            make_pending(0, "question", r#"{"question":"?"}"#),
            make_pending(
                1,
                "lsp",
                r#"{"path":"f.rs","operation":"rename","new_name":"foo"}"#,
            ),
            make_pending(2, "agent", r#"{"task":"explore"}"#),
        ]
        .into();
        let indices = vec![0, 1, 2];
        let mut messages = Vec::new();
        let mut counters = make_counters();

        let result = partition_tool_calls(
            &calls,
            &indices,
            &None,
            &None,
            &None,
            &mut messages,
            &mut counters,
        )
        .await;

        assert!(result.auto_allowed.is_empty());
        assert_eq!(result.needs_interaction.len(), 3);
    }

    #[tokio::test]
    async fn partition_lsp_read_ops_go_to_parallel() {
        // LSP diagnostics/definition/references are read-only and go to parallel
        let calls: HashMap<u32, PendingToolCall> = [
            make_pending(0, "lsp", r#"{"path":"f.rs"}"#), // defaults to diagnostics
            make_pending(
                1,
                "lsp",
                r#"{"path":"f.rs","operation":"definition","line":10,"character":5}"#,
            ),
            make_pending(
                2,
                "lsp",
                r#"{"path":"f.rs","operation":"references","line":10,"character":5}"#,
            ),
        ]
        .into();
        let indices = vec![0, 1, 2];
        let mut messages = Vec::new();
        let mut counters = make_counters();

        let result = partition_tool_calls(
            &calls,
            &indices,
            &None,
            &None,
            &None,
            &mut messages,
            &mut counters,
        )
        .await;

        assert_eq!(
            result.auto_allowed.len(),
            3,
            "all LSP read ops should go to parallel"
        );
        assert!(result.needs_interaction.is_empty());
    }

    #[test]
    fn is_lsp_read_op_exhaustive() {
        use crate::tool::LspOperation;

        // Explicit variant list — adding a variant forces a decision here
        let cases: &[(LspOperation, bool)] = &[
            (LspOperation::Diagnostics, true),
            (LspOperation::Definition, true),
            (LspOperation::References, true),
            (LspOperation::Rename, false),
        ];
        for (op, expected) in cases {
            let args = serde_json::json!({ "path": "f.rs", "operation": op.to_string() });
            assert_eq!(
                is_lsp_read_op(&args),
                *expected,
                "{op} read_op should be {expected}"
            );
        }

        // Missing operation defaults to diagnostics (read)
        let no_op = serde_json::json!({ "path": "f.rs" });
        assert!(
            is_lsp_read_op(&no_op),
            "missing operation should default to read"
        );
    }

    #[tokio::test]
    async fn partition_unknown_tool_pushes_error_message() {
        let calls: HashMap<u32, PendingToolCall> =
            [make_pending(0, "nonexistent_tool", "{}")].into();
        let indices = vec![0];
        let mut messages = Vec::new();
        let mut counters = make_counters();

        let result = partition_tool_calls(
            &calls,
            &indices,
            &None,
            &None,
            &None,
            &mut messages,
            &mut counters,
        )
        .await;

        assert!(result.auto_allowed.is_empty());
        assert!(result.needs_interaction.is_empty());
        assert_eq!(messages.len(), 1, "should push error tool result");
        assert_eq!(counters.tool_count, 1);
    }

    #[tokio::test]
    async fn partition_mixed_tools() {
        let calls: HashMap<u32, PendingToolCall> = [
            make_pending(0, "read", r#"{"path":"a.rs"}"#),
            make_pending(1, "grep", r#"{"pattern":"foo"}"#),
            make_pending(2, "edit", r#"{"file_path":"b.rs"}"#),
            make_pending(3, "glob", r#"{"pattern":"*.rs"}"#),
        ]
        .into();
        let indices = vec![0, 1, 2, 3];
        let mut messages = Vec::new();
        let mut counters = make_counters();

        let result = partition_tool_calls(
            &calls,
            &indices,
            &None,
            &None,
            &None,
            &mut messages,
            &mut counters,
        )
        .await;

        assert_eq!(result.auto_allowed.len(), 3);
        assert_eq!(result.needs_interaction.len(), 1);
        assert_eq!(result.needs_interaction[0].tool_name, ToolName::Edit);
    }

    #[tokio::test]
    async fn partition_memory_and_task_need_interaction() {
        let calls: HashMap<u32, PendingToolCall> = [
            make_pending(0, "memory", r#"{"action":"append","content":"x"}"#),
            make_pending(1, "task", r#"{"action":"create","title":"t"}"#),
        ]
        .into();
        let indices = vec![0, 1];
        let mut messages = Vec::new();
        let mut counters = make_counters();

        let result = partition_tool_calls(
            &calls,
            &indices,
            &None,
            &None,
            &None,
            &mut messages,
            &mut counters,
        )
        .await;

        assert!(result.auto_allowed.is_empty());
        assert_eq!(result.needs_interaction.len(), 2);
    }

    #[tokio::test]
    async fn partition_with_permission_engine() {
        let rules = crate::permission::profile_build_rules(
            crate::permission::PermissionProfile::Standard,
            &[],
            &[],
        );
        let engine = Arc::new(tokio::sync::Mutex::new(PermissionEngine::new(rules)));

        let calls: HashMap<u32, PendingToolCall> = [
            make_pending(0, "read", r#"{"path":"f.rs"}"#),
            make_pending(1, "bash", r#"{"command":"ls"}"#),
        ]
        .into();
        let indices = vec![0, 1];
        let mut messages = Vec::new();
        let mut counters = make_counters();

        let result = partition_tool_calls(
            &calls,
            &indices,
            &None,
            &Some(engine),
            &None,
            &mut messages,
            &mut counters,
        )
        .await;

        assert_eq!(result.auto_allowed.len(), 1);
        assert_eq!(result.auto_allowed[0].tool_name, ToolName::Read);
        assert_eq!(result.needs_interaction.len(), 1);
        assert_eq!(result.needs_interaction[0].tool_name, ToolName::Bash);
    }
}
