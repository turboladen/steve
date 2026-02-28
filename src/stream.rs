//! LLM streaming bridge with tool call loop.
//!
//! Spawns a tokio task that:
//! 1. Opens an SSE stream via async-openai
//! 2. Processes chunks, sending text deltas to the UI
//! 3. Accumulates tool call fragments from the stream
//! 4. When the stream finishes with tool calls, executes them and loops back

use std::collections::HashMap;

use async_openai::{
    config::OpenAIConfig,
    types::chat::{
        ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
        ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessage, ChatCompletionRequestSystemMessageContent,
        ChatCompletionRequestToolMessage, ChatCompletionRequestToolMessageContent,
        ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageContent,
        ChatCompletionResponseStream, ChatCompletionStreamOptions,
        ChatCompletionTool, ChatCompletionTools,
        CreateChatCompletionRequest, FunctionCall, FunctionObject, FinishReason,
    },
    Client,
};
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::context::cache::ToolResultCache;
use crate::event::{AppEvent, StreamUsage};
use crate::permission::PermissionEngine;
use crate::permission::types::{PermissionAction, PermissionReply, PermissionRequest};
use crate::tool::{ToolContext, ToolName, ToolRegistry};

/// Parameters for launching a streaming LLM request.
pub struct StreamRequest {
    pub client: Client<OpenAIConfig>,
    pub model: String,
    pub system_prompt: Option<String>,
    /// Previous conversation history (user + assistant messages from prior exchanges).
    pub history: Vec<ChatCompletionRequestMessage>,
    pub user_message: String,
    pub event_tx: mpsc::UnboundedSender<AppEvent>,
    pub tool_registry: Option<std::sync::Arc<ToolRegistry>>,
    pub tool_context: Option<ToolContext>,
    pub permission_engine: Option<std::sync::Arc<tokio::sync::Mutex<PermissionEngine>>>,
    pub tool_cache: std::sync::Arc<std::sync::Mutex<ToolResultCache>>,
    pub cancel_token: CancellationToken,
    /// Context window size for the current model (used for pre-call pruning).
    pub context_window: Option<u64>,
}

/// Spawn a tokio task that streams the LLM response and sends events.
/// Returns the JoinHandle so the caller can track task completion.
pub fn spawn_stream(req: StreamRequest) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(_) = run_stream(req).await {
            // Error already sent via channel in run_stream
        }
    })
}

/// Accumulated tool call from streaming fragments.
#[derive(Default)]
struct PendingToolCall {
    id: String,
    function_name: String,
    arguments: String,
}

async fn run_stream(req: StreamRequest) -> Result<(), ()> {
    let StreamRequest {
        client,
        model,
        system_prompt,
        history,
        user_message,
        event_tx,
        tool_registry,
        tool_context,
        permission_engine,
        tool_cache,
        cancel_token,
        context_window,
    } = req;

    tracing::info!(model = %model, "starting LLM stream");

    // Build initial messages: system prompt + history + new user message
    let mut messages: Vec<ChatCompletionRequestMessage> = Vec::new();

    if let Some(system) = system_prompt {
        messages.push(ChatCompletionRequestMessage::System(
            ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(system),
                name: None,
            },
        ));
    }

    // Append prior conversation history
    messages.extend(history);

    messages.push(ChatCompletionRequestMessage::User(
        ChatCompletionRequestUserMessage {
            content: ChatCompletionRequestUserMessageContent::Text(user_message),
            name: None,
        },
    ));

    // Build tool definitions for the API
    let tools: Option<Vec<ChatCompletionTools>> = tool_registry.as_ref().map(|registry| {
        registry
            .tool_definitions()
            .into_iter()
            .filter_map(|def| {
                let func = def.get("function")?;
                Some(ChatCompletionTools::Function(ChatCompletionTool {
                    function: FunctionObject {
                        name: func.get("name")?.as_str()?.to_string(),
                        description: func.get("description").and_then(|d| d.as_str()).map(String::from),
                        parameters: func.get("parameters").cloned(),
                        strict: None,
                    },
                }))
            })
            .collect()
    });

    let mut total_usage = StreamUsage::default();
    let mut current_iteration_tool_count: usize = 0;

    // Tool result cache — shared across stream tasks within a session.
    // Avoids re-executing identical read operations across messages.

    // Tool call loop — keep going until the LLM produces a response with no tool calls
    loop {
        // Check for cancellation before each LLM call
        if cancel_token.is_cancelled() {
            tracing::info!("stream cancelled before LLM call");
            let _ = event_tx.send(AppEvent::LlmFinish {
                usage: Some(total_usage),
            });
            return Ok(());
        }

        // Compress old tool results from prior iterations to reduce token usage.
        // Only tool results from the current iteration (which the LLM hasn't seen yet)
        // are kept uncompressed.
        if current_iteration_tool_count > 0 || messages.iter().any(|m| matches!(m, ChatCompletionRequestMessage::Tool(_))) {
            crate::context::compressor::compress_old_tool_results(
                &mut messages,
                current_iteration_tool_count,
            );
        }

        // Aggressive pruning: if conversation is still large after normal compression,
        // compress ALL tool results (including current iteration) to stay under budget.
        // Uses keep_recent=0 so even the latest tool results get compressed.
        if let Some(ctx_window) = context_window {
            let estimated_chars: usize = messages.iter().map(|m| estimate_message_chars(m)).sum();
            let estimated_tokens = estimated_chars / 4;
            if ctx_window > 0 && estimated_tokens as u64 > ctx_window * 60 / 100 {
                tracing::info!(
                    estimated_tokens,
                    context_window = ctx_window,
                    "aggressive pruning triggered — compressing all tool results"
                );
                crate::context::compressor::compress_old_tool_results(&mut messages, 0);
            }
        }

        // Reset counter for the next iteration
        current_iteration_tool_count = 0;

        // Estimate payload size for diagnostics
        let payload_chars: usize = messages.iter().map(|m| {
            match m {
                ChatCompletionRequestMessage::System(s) => match &s.content {
                    ChatCompletionRequestSystemMessageContent::Text(t) => t.len(),
                    _ => 0,
                },
                ChatCompletionRequestMessage::User(u) => match &u.content {
                    ChatCompletionRequestUserMessageContent::Text(t) => t.len(),
                    _ => 0,
                },
                ChatCompletionRequestMessage::Assistant(a) => match &a.content {
                    Some(ChatCompletionRequestAssistantMessageContent::Text(t)) => t.len(),
                    _ => 0,
                },
                ChatCompletionRequestMessage::Tool(t) => match &t.content {
                    ChatCompletionRequestToolMessageContent::Text(t) => t.len(),
                    _ => 0,
                },
                _ => 0,
            }
        }).sum();
        tracing::info!(
            message_count = messages.len(),
            payload_chars,
            estimated_tokens = payload_chars / 4,
            "sending request to LLM"
        );

        let mut request = CreateChatCompletionRequest {
            model: model.clone(),
            messages: messages.clone(),
            stream: Some(true),
            stream_options: Some(ChatCompletionStreamOptions {
                include_usage: Some(true),
                include_obfuscation: None,
            }),
            ..Default::default()
        };

        if let Some(ref t) = tools {
            if !t.is_empty() {
                request.tools = Some(t.clone());
            }
        }

        // Open the stream
        let stream_start = std::time::Instant::now();
        let mut stream: ChatCompletionResponseStream =
            match client.chat().create_stream(request).await {
                Ok(s) => {
                    tracing::info!(
                        elapsed_ms = stream_start.elapsed().as_millis() as u64,
                        "stream connection opened"
                    );
                    s
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to start stream");
                    let _ = event_tx.send(AppEvent::LlmError {
                        error: format!("failed to start stream: {e}"),
                    });
                    return Err(());
                }
            };

        // Accumulate tool call fragments
        let mut pending_tool_calls: HashMap<u32, PendingToolCall> = HashMap::new();
        let mut finish_reason: Option<FinishReason> = None;
        let mut assistant_content = String::new();
        let mut first_token_logged = false;

        // Process chunks — use select! to check cancellation while streaming
        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    tracing::info!("stream cancelled during processing");
                    let _ = event_tx.send(AppEvent::LlmFinish {
                        usage: Some(total_usage),
                    });
                    return Ok(());
                }
                maybe_chunk = stream.next() => {
                    let Some(result) = maybe_chunk else {
                        // Stream ended
                        break;
                    };

                    if !first_token_logged {
                        first_token_logged = true;
                        tracing::info!(
                            ttft_ms = stream_start.elapsed().as_millis() as u64,
                            "first token received"
                        );
                    }

                    match result {
                        Ok(response) => {
                            // Check for usage in the final chunk
                            if let Some(u) = &response.usage {
                                tracing::info!(
                                    prompt = u.prompt_tokens,
                                    completion = u.completion_tokens,
                                    total = u.total_tokens,
                                    "usage data received in stream chunk"
                                );
                                total_usage.prompt_tokens += u.prompt_tokens;
                                total_usage.completion_tokens += u.completion_tokens;
                                total_usage.total_tokens += u.total_tokens;

                                // Send current-call usage to UI for live token counter updates
                                let _ = event_tx.send(AppEvent::LlmUsageUpdate {
                                    usage: StreamUsage {
                                        prompt_tokens: u.prompt_tokens,
                                        completion_tokens: u.completion_tokens,
                                        total_tokens: u.total_tokens,
                                    },
                                });
                            }

                            // Process each choice's delta
                            for choice in &response.choices {
                                // TODO: Emit AppEvent::LlmReasoning for reasoning/thinking tokens.
                                // OpenAI o1/o3 models send reasoning content via a `reasoning_content`
                                // field on the stream delta, but async-openai 0.32 does not expose this
                                // field on ChatCompletionStreamResponseDelta. When async-openai adds
                                // support (or if we switch to raw JSON parsing), add:
                                //
                                //   if let Some(reasoning) = &choice.delta.reasoning_content {
                                //       if !reasoning.is_empty() {
                                //           let _ = event_tx.send(AppEvent::LlmReasoning {
                                //               text: reasoning.clone(),
                                //           });
                                //       }
                                //   }

                                // Text content delta
                                if let Some(content) = &choice.delta.content {
                                    if !content.is_empty() {
                                        assistant_content.push_str(content);
                                        let _ = event_tx.send(AppEvent::LlmDelta {
                                            text: content.clone(),
                                        });
                                    }
                                }

                                // Tool call fragments
                                if let Some(tool_calls) = &choice.delta.tool_calls {
                                    for tc in tool_calls {
                                        let entry = pending_tool_calls
                                            .entry(tc.index)
                                            .or_insert_with(PendingToolCall::default);

                                        if let Some(id) = &tc.id {
                                            entry.id = id.clone();
                                        }
                                        if let Some(func) = &tc.function {
                                            if let Some(name) = &func.name {
                                                entry.function_name = name.clone();
                                            }
                                            if let Some(args) = &func.arguments {
                                                entry.arguments.push_str(args);
                                            }
                                        }
                                    }

                                    // Notify UI when new tool calls appear
                                    // (done outside the entry borrow to satisfy the borrow checker)
                                    for tc in tool_calls {
                                        if tc.function.as_ref().and_then(|f| f.name.as_ref()).is_some() {
                                            if let Some(entry) = pending_tool_calls.get(&tc.index) {
                                                if let Ok(name) = entry.function_name.parse::<ToolName>() {
                                                    let _ = event_tx.send(AppEvent::LlmToolCallStreaming {
                                                        count: pending_tool_calls.len(),
                                                        tool_name: name,
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }

                                // Track finish reason
                                if let Some(reason) = &choice.finish_reason {
                                    finish_reason = Some(reason.clone());
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "stream chunk error");
                            let _ = event_tx.send(AppEvent::LlmError {
                                error: format!("stream error: {e}"),
                            });
                            return Err(());
                        }
                    }
                }
            }
        }

        // Stream ended — check if we have tool calls to execute.
        // Some providers don't set finish_reason=tool_calls reliably,
        // so we check for accumulated tool call fragments regardless.
        let has_valid_tool_calls = pending_tool_calls
            .values()
            .any(|tc| !tc.id.is_empty() && !tc.function_name.is_empty());

        if pending_tool_calls.is_empty() || !has_valid_tool_calls {
            tracing::info!(
                finish_reason = ?finish_reason,
                pending_tool_calls = pending_tool_calls.len(),
                "stream finished (no tool calls)"
            );
            tracing::info!(
                prompt = total_usage.prompt_tokens,
                completion = total_usage.completion_tokens,
                total = total_usage.total_tokens,
                "final usage totals"
            );
            // Warn user if the model stopped due to context exhaustion
            if matches!(finish_reason, Some(FinishReason::Length)) {
                let _ = event_tx.send(AppEvent::LlmError {
                    error: "Context window full — response was cut off. Run /compact to free space, or /new to start fresh.".to_string(),
                });
                return Ok(());
            }
            let _ = event_tx.send(AppEvent::LlmFinish {
                usage: Some(total_usage),
            });
            return Ok(());
        }

        if !matches!(finish_reason, Some(FinishReason::ToolCalls)) {
            tracing::warn!(
                finish_reason = ?finish_reason,
                tool_call_count = pending_tool_calls.len(),
                "finish_reason is not ToolCalls but tool calls were streamed — executing anyway"
            );
        }

        // We have tool calls — execute them and loop
        let Some(ref registry) = tool_registry else {
            let _ = event_tx.send(AppEvent::LlmFinish {
                usage: Some(total_usage),
            });
            return Ok(());
        };

        let ctx = tool_context.clone().unwrap_or_else(|| ToolContext {
            project_root: std::path::PathBuf::from("."),
            storage_dir: None,
        });

        // Sort tool calls by index for deterministic ordering
        let mut sorted_indices: Vec<u32> = pending_tool_calls.keys().cloned().collect();
        sorted_indices.sort();

        // Filter out truncated tool calls (common when finish_reason=Length).
        // The last tool call's JSON arguments are often cut off mid-stream.
        let pre_filter_count = sorted_indices.len();
        sorted_indices.retain(|idx| {
            let tc = &pending_tool_calls[idx];
            match serde_json::from_str::<Value>(&tc.arguments) {
                Ok(_) => true,
                Err(e) => {
                    tracing::warn!(
                        tool = %tc.function_name,
                        call_id = %tc.id,
                        error = %e,
                        "dropping tool call with truncated/invalid JSON arguments"
                    );
                    false
                }
            }
        });
        if sorted_indices.len() < pre_filter_count {
            tracing::info!(
                dropped = pre_filter_count - sorted_indices.len(),
                remaining = sorted_indices.len(),
                "filtered out truncated tool calls"
            );
        }

        if sorted_indices.is_empty() {
            tracing::info!("all tool calls were truncated — finishing stream");
            let _ = event_tx.send(AppEvent::LlmError {
                error: "Context window full — tool calls were truncated. Run /compact to free space, or /new to start fresh.".to_string(),
            });
            return Ok(());
        }

        tracing::info!(count = sorted_indices.len(), "executing tool calls");

        // Build the assistant message with tool calls for the conversation history
        let api_tool_calls: Vec<ChatCompletionMessageToolCalls> = sorted_indices
            .iter()
            .map(|idx| {
                let tc = &pending_tool_calls[idx];
                ChatCompletionMessageToolCalls::Function(ChatCompletionMessageToolCall {
                    id: tc.id.clone(),
                    function: FunctionCall {
                        name: tc.function_name.clone(),
                        arguments: tc.arguments.clone(),
                    },
                })
            })
            .collect();

        // Add assistant message with tool calls to conversation
        #[allow(deprecated)]
        messages.push(ChatCompletionRequestMessage::Assistant(
            ChatCompletionRequestAssistantMessage {
                content: if assistant_content.is_empty() {
                    None
                } else {
                    Some(
                        async_openai::types::chat::ChatCompletionRequestAssistantMessageContent::Text(
                            assistant_content.clone(),
                        ),
                    )
                },
                name: None,
                audio: None,
                tool_calls: Some(api_tool_calls),
                function_call: None,
                refusal: None,
            },
        ));

        // ── Phase 1: Pre-check permissions and partition tool calls ──
        // Check permissions for all tool calls up front, then partition into
        // auto-allowed (can run in parallel) vs needs-interaction (sequential).

        struct PreparedToolCall {
            idx: u32,
            id: String,
            tool_name: ToolName,
            args: Value,
            action: PermissionAction,
        }

        let mut prepared: Vec<PreparedToolCall> = Vec::new();

        for idx in &sorted_indices {
            let tc = &pending_tool_calls[idx];
            let args: Value = serde_json::from_str(&tc.arguments).unwrap_or(Value::Null);

            let tool_name: ToolName = match tc.function_name.parse() {
                Ok(name) => name,
                Err(_) => {
                    tracing::warn!(tool = %tc.function_name, "unknown tool name from LLM, returning error");
                    // Must provide a tool result for every tool_call_id in the assistant message
                    messages.push(ChatCompletionRequestMessage::Tool(
                        ChatCompletionRequestToolMessage {
                            content: ChatCompletionRequestToolMessageContent::Text(
                                format!("Error: unknown tool '{}'", tc.function_name),
                            ),
                            tool_call_id: tc.id.clone(),
                        },
                    ));
                    current_iteration_tool_count += 1;
                    continue;
                }
            };

            let action = if let Some(ref engine) = permission_engine {
                let engine = engine.lock().await;
                engine.check(tool_name, None)
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
        // Write tools (edit, write, patch) and memory tool (append action) always
        // go to sequential phase for cache invalidation and permission prompts.
        let (auto_allowed, needs_interaction): (Vec<_>, Vec<_>) = prepared
            .into_iter()
            .partition(|tc| {
                matches!(tc.action, PermissionAction::Allow)
                    && !tc.tool_name.is_write_tool()
                    && !tc.tool_name.is_memory()
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

        // ── Phase 2: Execute auto-allowed tools in parallel ──
        // These are read-only tools that don't need user permission.

        // First, check cache for each. Only spawn tasks for cache misses.
        struct ParallelTask {
            id: String,
            tool_name: ToolName,
            args: Value,
        }

        let mut parallel_results: HashMap<String, crate::tool::ToolOutput> = HashMap::new();
        let mut tasks_to_spawn: Vec<ParallelTask> = Vec::new();

        for tc in &auto_allowed {
            if let Some(cached) = tool_cache.lock().unwrap().get(tc.tool_name, &tc.args) {
                tracing::debug!(tool = %tc.tool_name, "using cached result (parallel)");
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
                cached = auto_allowed.len() - tasks_to_spawn.len(),
                "executing auto-allowed tools in parallel"
            );

            // Spawn all cache-miss tools in parallel via spawn_blocking
            let handles: Vec<(String, ToolName, Value, tokio::task::JoinHandle<anyhow::Result<crate::tool::ToolOutput>>)> =
                tasks_to_spawn
                    .into_iter()
                    .map(|task| {
                        let reg = registry.clone();
                        let c = ctx.clone();
                        let name = task.tool_name;
                        let a = task.args.clone();
                        let handle = tokio::task::spawn_blocking(move || {
                            reg.execute(name, a, c)
                        });
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
                tool_cache.lock().unwrap().put(tool_name, &args, &call_id, &output);

                parallel_results.insert(call_id, output);
            }
        }

        // Emit events and add results for auto-allowed tools (in original order)
        for tc in &auto_allowed {
            if cancel_token.is_cancelled() {
                tracing::info!("stream cancelled during tool result processing");
                let _ = event_tx.send(AppEvent::LlmFinish {
                    usage: Some(total_usage),
                });
                return Ok(());
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
            current_iteration_tool_count += 1;
        }

        // ── Phase 3: Execute permission-required tools sequentially ──
        // These need user interaction (Ask) or are denied by policy.

        for tc in &needs_interaction {
            if cancel_token.is_cancelled() {
                tracing::info!("stream cancelled during tool execution");
                let _ = event_tx.send(AppEvent::LlmFinish {
                    usage: Some(total_usage),
                });
                return Ok(());
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
                        output: format!("Permission denied: {} is not allowed in the current mode.", tc.tool_name),
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
                    current_iteration_tool_count += 1;
                }
                PermissionAction::Ask => {
                    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
                    let summary = build_permission_summary(tc.tool_name, &tc.args);

                    let _ = event_tx.send(AppEvent::PermissionRequest(PermissionRequest {
                        call_id: tc.id.clone(),
                        tool_name: tc.tool_name,
                        arguments_summary: summary,
                        response_tx,
                    }));

                    tokio::select! {
                        _ = cancel_token.cancelled() => {
                            tracing::info!("stream cancelled while waiting for permission");
                            let _ = event_tx.send(AppEvent::LlmFinish {
                                usage: Some(total_usage),
                            });
                            return Ok(());
                        }
                        reply = response_rx => {
                            match reply {
                                Ok(PermissionReply::AllowOnce) => {
                                    tracing::info!(tool = %tc.tool_name, "permission granted (once)");
                                }
                                Ok(PermissionReply::AllowAlways) => {
                                    tracing::info!(tool = %tc.tool_name, "permission granted (always)");
                                    if let Some(ref engine) = permission_engine {
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
                                    current_iteration_tool_count += 1;
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

                    let output = if let Some(cached) = tool_cache.lock().unwrap().get(tc.tool_name, &tc.args) {
                        cached
                    } else {
                        let result = match registry.execute(tc.tool_name, tc.args.clone(), ctx.clone()) {
                            Ok(output) => output,
                            Err(e) => {
                                tracing::error!(tool = %tc.tool_name, error = %e, "tool execution failed");
                                crate::tool::ToolOutput {
                                    title: tc.tool_name.to_string(),
                                    output: format!("Error: {e}"),
                                    is_error: true,
                                }
                            }
                        };

                        let mut cache = tool_cache.lock().unwrap();
                        cache.put(tc.tool_name, &tc.args, &tc.id, &result);

                        // Invalidate cache entries when write operations modify files
                        if tc.tool_name.is_write_tool() {
                            if let Some(path) = tc.args.get("file_path").and_then(|v| v.as_str()) {
                                cache.invalidate_path(path);
                            }
                        }

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
                    current_iteration_tool_count += 1;
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

                    let output = if let Some(cached) = tool_cache.lock().unwrap().get(tc.tool_name, &tc.args) {
                        cached
                    } else {
                        let result = match registry.execute(tc.tool_name, tc.args.clone(), ctx.clone()) {
                            Ok(output) => output,
                            Err(e) => {
                                tracing::error!(tool = %tc.tool_name, error = %e, "tool execution failed");
                                crate::tool::ToolOutput {
                                    title: tc.tool_name.to_string(),
                                    output: format!("Error: {e}"),
                                    is_error: true,
                                }
                            }
                        };

                        let mut cache = tool_cache.lock().unwrap();
                        cache.put(tc.tool_name, &tc.args, &tc.id, &result);

                        // Invalidate cache entries when write operations modify files
                        if tc.tool_name.is_write_tool() {
                            if let Some(path) = tc.args.get("file_path").and_then(|v| v.as_str()) {
                                cache.invalidate_path(path);
                            }
                        }

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
                    current_iteration_tool_count += 1;
                }
            }
        }

        // Loop back to send the messages (with tool results) to the LLM again
    }
}

/// Build a human-readable summary of what a tool call wants to do.
fn build_permission_summary(tool_name: ToolName, args: &Value) -> String {
    match tool_name {
        ToolName::Bash => {
            let cmd = args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown command)");
            format!("Run command: {cmd}")
        }
        ToolName::Edit => {
            let file = args
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown file)");
            format!("Edit file: {file}")
        }
        ToolName::Write => {
            let file = args
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown file)");
            format!("Write file: {file}")
        }
        ToolName::Patch => {
            let file = args
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown file)");
            format!("Patch file: {file}")
        }
        ToolName::Read | ToolName::Grep | ToolName::Glob | ToolName::List
        | ToolName::Question | ToolName::Todo | ToolName::Webfetch | ToolName::Memory => {
            format!("{tool_name}: {}", serde_json::to_string(args).unwrap_or_default())
        }
    }
}

/// Estimate the character count of a message for token approximation.
fn estimate_message_chars(msg: &ChatCompletionRequestMessage) -> usize {
    match msg {
        ChatCompletionRequestMessage::System(s) => match &s.content {
            ChatCompletionRequestSystemMessageContent::Text(t) => t.len(),
            _ => 0,
        },
        ChatCompletionRequestMessage::User(u) => match &u.content {
            ChatCompletionRequestUserMessageContent::Text(t) => t.len(),
            _ => 0,
        },
        ChatCompletionRequestMessage::Assistant(a) => {
            let content_len = match &a.content {
                Some(ChatCompletionRequestAssistantMessageContent::Text(t)) => t.len(),
                _ => 0,
            };
            let tool_calls_len = a
                .tool_calls
                .as_ref()
                .map(|tcs| {
                    tcs.iter()
                        .map(|tc| {
                            if let ChatCompletionMessageToolCalls::Function(f) = tc {
                                f.function.name.len() + f.function.arguments.len()
                            } else {
                                0
                            }
                        })
                        .sum::<usize>()
                })
                .unwrap_or(0);
            content_len + tool_calls_len
        }
        ChatCompletionRequestMessage::Tool(t) => match &t.content {
            ChatCompletionRequestToolMessageContent::Text(t) => t.len(),
            _ => 0,
        },
        _ => 0,
    }
}
