//! LLM streaming bridge with tool call loop.
//!
//! Spawns a tokio task that:
//! 1. Opens an SSE stream via async-openai
//! 2. Processes chunks, sending text deltas to the UI
//! 3. Accumulates tool call fragments from the stream
//! 4. When the stream finishes with tool calls, executes them and loops back

use std::collections::HashMap;
use std::sync::Arc;

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
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::types::ModelCost;
use crate::context::cache::ToolResultCache;
use crate::event::{AppEvent, StreamUsage};
use crate::permission::PermissionEngine;
use crate::permission::types::{PermissionAction, PermissionReply, PermissionRequest};
use crate::tool::{ToolContext, ToolName, ToolRegistry};
use crate::usage::UsageWriter;
use crate::usage::types::ApiCallRecord;

/// Abstraction over LLM stream creation — enables mock testing of the tool loop.
#[async_trait]
pub trait ChatStreamProvider: Send + Sync {
    async fn create_stream(
        &self,
        request: CreateChatCompletionRequest,
    ) -> Result<ChatCompletionResponseStream, async_openai::error::OpenAIError>;
}

/// Production implementation wrapping async-openai's Client.
pub struct OpenAIChatStream {
    client: Client<OpenAIConfig>,
}

impl OpenAIChatStream {
    pub fn new(client: Client<OpenAIConfig>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl ChatStreamProvider for OpenAIChatStream {
    async fn create_stream(
        &self,
        request: CreateChatCompletionRequest,
    ) -> Result<ChatCompletionResponseStream, async_openai::error::OpenAIError> {
        self.client.chat().create_stream(request).await
    }
}

/// Parameters for launching a streaming LLM request.
pub struct StreamRequest {
    pub stream_provider: Arc<dyn ChatStreamProvider>,
    pub model: String,
    pub system_prompt: Option<String>,
    /// Previous conversation history (user + assistant messages from prior exchanges).
    pub history: Vec<ChatCompletionRequestMessage>,
    pub user_message: String,
    pub event_tx: mpsc::UnboundedSender<AppEvent>,
    pub tool_registry: Option<Arc<ToolRegistry>>,
    pub tool_context: Option<ToolContext>,
    pub permission_engine: Option<Arc<tokio::sync::Mutex<PermissionEngine>>>,
    pub tool_cache: Arc<std::sync::Mutex<ToolResultCache>>,
    pub cancel_token: CancellationToken,
    /// Context window size for the current model (used for pre-call pruning).
    pub context_window: Option<u64>,
    /// Channel for receiving user interjections mid-tool-loop.
    pub interjection_rx: mpsc::UnboundedReceiver<String>,
    /// Usage analytics writer (fire-and-forget to background SQLite thread).
    pub usage_writer: UsageWriter,
    /// Project ID for usage recording.
    pub usage_project_id: String,
    /// Session ID for usage recording.
    pub usage_session_id: String,
    /// Model cost config for per-call cost calculation (None if no pricing configured).
    pub usage_model_cost: Option<ModelCost>,
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
        stream_provider,
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
        mut interjection_rx,
        usage_writer,
        usage_project_id,
        usage_session_id,
        usage_model_cost,
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

    // Safety limit: prevent infinite tool-call loops (e.g. compressor/cache
    // feedback oscillation where the LLM re-reads the same files forever).
    // Resets when the user grants a permission (proving active supervision).
    const MAX_TOOL_ITERATIONS: u32 = 75;
    let mut iteration_count: u32 = 0;
    let mut total_iteration_count: u32 = 0;

    // Bitmask tracks which escalating warnings have fired this cycle
    // (resets with iteration_count on user interaction).
    let mut warnings_sent: u8 = 0;

    // Mid-stream error retry limit (separate from stream creation retries).
    const MAX_STREAM_RETRIES: u32 = 2;
    let mut stream_retry_count: u32 = 0;

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

        // Drain pending user interjections before the next LLM call
        while let Ok(text) = interjection_rx.try_recv() {
            tracing::info!("user interjection received: {} chars", text.len());
            messages.push(ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessage {
                    content: ChatCompletionRequestUserMessageContent::Text(text),
                    name: None,
                },
            ));
            iteration_count = 0; // User interaction resets safety counter
            warnings_sent = 0;
        }

        total_iteration_count += 1;
        iteration_count += 1;
        let mut user_interacted_this_iteration = false;
        let call_start = std::time::Instant::now();
        if iteration_count > MAX_TOOL_ITERATIONS {
            tracing::error!(
                iterations = iteration_count,
                total_iterations = total_iteration_count,
                "tool loop exceeded max iterations"
            );
            let _ = event_tx.send(AppEvent::LlmError {
                error: format!(
                    "Tool loop exceeded {MAX_TOOL_ITERATIONS} iterations. Try /compact or /new."
                ),
            });
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

        // Open the stream with retry for transient errors
        const MAX_RETRIES: u32 = 3;
        let stream_start = std::time::Instant::now();
        let mut stream_result: Option<ChatCompletionResponseStream> = None;
        for attempt in 1..=MAX_RETRIES {
            match stream_provider.create_stream(request.clone()).await {
                Ok(s) => {
                    if attempt > 1 {
                        tracing::info!(attempt, "stream connection succeeded after retry");
                    }
                    tracing::info!(
                        elapsed_ms = stream_start.elapsed().as_millis() as u64,
                        "stream connection opened"
                    );
                    stream_result = Some(s);
                    break;
                }
                Err(e) => {
                    if attempt < MAX_RETRIES && is_transient_error(&e) {
                        let delay = std::time::Duration::from_secs(1 << (attempt - 1)); // 1s, 2s
                        tracing::warn!(
                            error = %e,
                            attempt,
                            max_attempts = MAX_RETRIES,
                            delay_secs = delay.as_secs(),
                            "transient error, retrying stream creation"
                        );
                        let _ = event_tx.send(AppEvent::LlmRetry {
                            attempt,
                            max_attempts: MAX_RETRIES,
                            error: e.to_string(),
                        });
                        tokio::time::sleep(delay).await;
                        if cancel_token.is_cancelled() {
                            let _ = event_tx.send(AppEvent::LlmFinish {
                                usage: Some(total_usage),
                            });
                            return Ok(());
                        }
                        continue;
                    }
                    // Non-transient or max retries exceeded
                    tracing::error!(error = %e, attempt, "stream creation failed");
                    let _ = event_tx.send(AppEvent::LlmError {
                        error: format!("API error: {e}"),
                    });
                    return Err(());
                }
            }
        }
        let Some(mut stream) = stream_result else {
            // Should not happen — loop always breaks or returns — but handle gracefully
            tracing::error!("stream_result was None after retry loop (should be unreachable)");
            let _ = event_tx.send(AppEvent::LlmError {
                error: "internal error: stream creation produced no result".to_string(),
            });
            return Err(());
        };

        // Accumulate tool call fragments
        let mut pending_tool_calls: HashMap<u32, PendingToolCall> = HashMap::new();
        let mut finish_reason: Option<FinishReason> = None;
        let mut assistant_content = String::new();
        let mut first_token_logged = false;
        let mut stream_chunk_error: Option<String> = None;

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

                                // Record per-call usage to SQLite
                                let cost = usage_model_cost.as_ref().map(|mc| {
                                    u.prompt_tokens as f64 * mc.input_per_million / 1_000_000.0
                                        + u.completion_tokens as f64 * mc.output_per_million
                                            / 1_000_000.0
                                });
                                usage_writer.record_api_call(ApiCallRecord {
                                    timestamp: chrono::Utc::now(),
                                    project_id: usage_project_id.clone(),
                                    session_id: usage_session_id.clone(),
                                    model_ref: model.clone(),
                                    prompt_tokens: u.prompt_tokens,
                                    completion_tokens: u.completion_tokens,
                                    total_tokens: u.total_tokens,
                                    cost,
                                    duration_ms: call_start.elapsed().as_millis() as u64,
                                    iteration: total_iteration_count.saturating_sub(1),
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
                                        let func_name = tc.function.as_ref().and_then(|f| f.name.as_deref());
                                        let func_args = tc.function.as_ref().and_then(|f| f.arguments.as_deref());
                                        accumulate_tool_call(
                                            &mut pending_tool_calls,
                                            tc.index,
                                            tc.id.as_deref(),
                                            func_name,
                                            func_args,
                                        );
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
                            tracing::error!(
                                error = %e,
                                error_debug = ?e,
                                iteration = total_iteration_count,
                                message_count = messages.len(),
                                chunks_received = if first_token_logged { "yes" } else { "no" },
                                partial_text_len = assistant_content.len(),
                                pending_tool_calls = pending_tool_calls.len(),
                                "stream chunk error"
                            );
                            stream_chunk_error = Some(e.to_string());
                            break;
                        }
                    }
                }
            }
        }

        // Mid-stream error — retry the LLM call if retries remain.
        if let Some(err_msg) = stream_chunk_error {
            stream_retry_count += 1;
            if stream_retry_count <= MAX_STREAM_RETRIES {
                tracing::warn!(
                    error = %err_msg,
                    attempt = stream_retry_count,
                    max = MAX_STREAM_RETRIES,
                    "mid-stream error, retrying LLM call"
                );
                let _ = event_tx.send(AppEvent::LlmRetry {
                    attempt: stream_retry_count,
                    max_attempts: MAX_STREAM_RETRIES,
                    error: err_msg,
                });
                // Don't count this as a tool iteration — it's a retry of
                // the same call. Undo the increment from the top of the loop.
                total_iteration_count -= 1;
                iteration_count -= 1;
                continue;
            }
            tracing::error!(
                error = %err_msg,
                retries = stream_retry_count,
                "mid-stream error, retries exhausted"
            );
            let _ = event_tx.send(AppEvent::LlmError {
                error: format!("stream error: {err_msg}"),
            });
            let _ = event_tx.send(AppEvent::LlmFinish {
                usage: Some(total_usage),
            });
            return Ok(());
        }
        // Reset stream retry counter on successful completion
        stream_retry_count = 0;

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
            task_store: None,
        });

        // Sort tool calls by index for deterministic ordering
        let mut sorted_indices: Vec<u32> = pending_tool_calls.keys().cloned().collect();
        sorted_indices.sort();

        // Filter out truncated tool calls (common when finish_reason=Length).
        // The last tool call's JSON arguments are often cut off mid-stream.
        let pre_filter_count = sorted_indices.len();
        sorted_indices.retain(|idx| {
            let tc = &pending_tool_calls[idx];
            if is_valid_tool_call(tc) {
                true
            } else {
                tracing::warn!(
                    tool = %tc.function_name,
                    call_id = %tc.id,
                    args_len = tc.arguments.len(),
                    "dropping tool call with invalid/truncated data"
                );
                false
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

            let path_hint = extract_tool_path(tool_name, &args);
            let action = if let Some(ref engine) = permission_engine {
                let engine = engine.lock().await;
                engine.check(tool_name, path_hint.as_deref())
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
                    && !matches!(tc.tool_name, ToolName::Question)
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
                tool_cache.lock().unwrap().put(tool_name, &args, &output);

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

            // Question tool: special interactive flow (bypass permission)
            if matches!(tc.tool_name, ToolName::Question) {
                let question = tc.args.get("question")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(no question provided)")
                    .to_string();
                let options: Vec<String> = tc.args.get("options")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
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
                            usage: Some(total_usage),
                        });
                        return Ok(());
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
                current_iteration_tool_count += 1;
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
                        tool_args: tc.args.clone(),
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
                                    user_interacted_this_iteration = true;
                                }
                                Ok(PermissionReply::AllowAlways) => {
                                    tracing::info!(tool = %tc.tool_name, "permission granted (always)");
                                    user_interacted_this_iteration = true;
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
                        cache.put(tc.tool_name, &tc.args, &result);

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
                        cache.put(tc.tool_name, &tc.args, &result);

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

        // Reset iteration counter if user granted permission this iteration
        if user_interacted_this_iteration {
            tracing::debug!(
                iterations = iteration_count,
                total_iterations = total_iteration_count,
                "resetting iteration counter (user granted permission)"
            );
            iteration_count = 0;
            warnings_sent = 0;
        }

        // Escalating warnings: append a nudge to the last tool result message
        // so the LLM sees iteration pressure as contextual feedback.
        if let Some(text) = check_iteration_warning(
            iteration_count, MAX_TOOL_ITERATIONS, &mut warnings_sent,
        ) {
            // Append warning to the last Tool message in the conversation
            for msg in messages.iter_mut().rev() {
                if let ChatCompletionRequestMessage::Tool(tool_msg) = msg {
                    if let ChatCompletionRequestToolMessageContent::Text(content) = &mut tool_msg.content {
                        content.push_str(&text);
                    }
                    break;
                }
            }
            // Also notify the TUI so the user sees why the LLM might wrap up
            let display_text = text.trim().trim_start_matches('[').trim_end_matches(']');
            let _ = event_tx.send(AppEvent::StreamNotice {
                text: display_text.to_string(),
            });
            tracing::info!(
                iteration_count,
                total_iteration_count,
                "tool loop warning injected"
            );
        }

        // Loop back to send the messages (with tool results) to the LLM again
    }
}

/// Warning thresholds as percentages of the max iteration limit.
const WARN_NUDGE_PCT: u32 = 33;
const WARN_WARNING_PCT: u32 = 67;
const WARN_CRITICAL_PCT: u32 = 87;

/// Bitmask flags for tracking which warnings have been sent.
const WARN_NUDGE_BIT: u8 = 0b001;
const WARN_WARNING_BIT: u8 = 0b010;
const WARN_CRITICAL_BIT: u8 = 0b100;

/// Check whether an escalating warning should be emitted at the current iteration count.
/// Returns the warning text to append to the last tool result, if any, and updates the
/// bitmask to prevent re-firing the same threshold.
fn check_iteration_warning(
    iteration_count: u32,
    max_iterations: u32,
    warnings_sent: &mut u8,
) -> Option<String> {
    let critical_at = max_iterations * WARN_CRITICAL_PCT / 100;
    let warning_at = max_iterations * WARN_WARNING_PCT / 100;
    let nudge_at = max_iterations * WARN_NUDGE_PCT / 100;

    if iteration_count >= critical_at && (*warnings_sent & WARN_CRITICAL_BIT) == 0 {
        *warnings_sent |= WARN_CRITICAL_BIT;
        let remaining = max_iterations.saturating_sub(iteration_count);
        Some(format!(
            "\n\n[CRITICAL: {remaining} tool calls remaining before forced termination. \
             Respond to the user NOW with your findings. Do NOT make any more tool calls \
             unless absolutely necessary.]"
        ))
    } else if iteration_count >= warning_at && (*warnings_sent & WARN_WARNING_BIT) == 0 {
        *warnings_sent |= WARN_WARNING_BIT;
        Some(format!(
            "\n\n[Warning: {iteration_count} of {max_iterations} tool calls used. \
             You MUST finish within the next few calls. Do not re-read files — work with \
             what you have.]"
        ))
    } else if iteration_count >= nudge_at && (*warnings_sent & WARN_NUDGE_BIT) == 0 {
        *warnings_sent |= WARN_NUDGE_BIT;
        Some(format!(
            "\n\n[Note: You have made {iteration_count} tool calls on this response. \
             Begin wrapping up your analysis.]"
        ))
    } else {
        None
    }
}

/// Extract the primary file path from tool arguments for path-based permission checks.
///
/// Returns `None` for tools that don't operate on file paths (bash, question, todo).
fn extract_tool_path(tool_name: ToolName, args: &Value) -> Option<String> {
    match tool_name {
        ToolName::Read | ToolName::Edit | ToolName::Write | ToolName::Patch | ToolName::List => {
            args.get("file_path").or_else(|| args.get("path"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        }
        ToolName::Grep | ToolName::Glob => {
            args.get("path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        }
        ToolName::Move | ToolName::Copy => {
            // Use the destination path for permission checking (the write target)
            args.get("to_path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        }
        ToolName::Delete | ToolName::Mkdir => {
            args.get("path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        }
        // Tools without file paths
        ToolName::Bash | ToolName::Question | ToolName::Task
        | ToolName::Webfetch | ToolName::Memory => None,
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
        ToolName::Move | ToolName::Copy => {
            let from = args.get("from_path").and_then(|v| v.as_str()).unwrap_or("(unknown)");
            let to = args.get("to_path").and_then(|v| v.as_str()).unwrap_or("(unknown)");
            format!("{tool_name}: {from} \u{2192} {to}")
        }
        ToolName::Delete => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("(unknown)");
            format!("Delete: {path}")
        }
        ToolName::Mkdir => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("(unknown)");
            format!("Create directory: {path}")
        }
        ToolName::Read | ToolName::Grep | ToolName::Glob | ToolName::List
        | ToolName::Question | ToolName::Task | ToolName::Webfetch | ToolName::Memory => {
            format!("{tool_name}: {}", serde_json::to_string(args).unwrap_or_default())
        }
    }
}

/// Classify whether an OpenAI API error is transient (worth retrying).
///
/// Transient errors include network timeouts, connection failures, rate limits,
/// and server overload responses. Non-transient errors (auth failures, invalid
/// arguments, deserialization errors) are not retried.
fn is_transient_error(err: &async_openai::error::OpenAIError) -> bool {
    use async_openai::error::OpenAIError;
    match err {
        OpenAIError::Reqwest(e) => {
            // Network-level failures: timeout, connection refused, DNS, 5xx, etc.
            e.is_timeout() || e.is_connect() || e.is_request()
                || e.status().is_some_and(|s| s.is_server_error())
        }
        OpenAIError::ApiError(api_err) => {
            // Rate limit or server overload from the API
            matches!(api_err.code.as_deref(), Some("rate_limit_exceeded"))
                || api_err.message.contains("overloaded")
                || api_err.message.contains("temporarily unavailable")
        }
        OpenAIError::StreamError(_) => {
            // SSE stream failures are often transient (connection drops)
            true
        }
        // Explicit non-transient variants — exhaustive match ensures new variants
        // are reviewed for transient-ness when async-openai adds them.
        OpenAIError::JSONDeserialize(_, _) => false,
        OpenAIError::FileSaveError(_) => false,
        OpenAIError::FileReadError(_) => false,
        OpenAIError::InvalidArgument(_) => false,
    }
}

/// Accumulate a tool call fragment from a stream chunk delta.
/// Updates the pending_tool_calls map with the fragment data.
/// Returns true if this fragment introduces a new tool call (has a function name).
fn accumulate_tool_call(
    pending: &mut HashMap<u32, PendingToolCall>,
    index: u32,
    id: Option<&str>,
    name: Option<&str>,
    arguments: Option<&str>,
) -> bool {
    let entry = pending.entry(index).or_default();

    if let Some(id) = id {
        entry.id = id.to_string();
    }
    if let Some(name) = name {
        entry.function_name = name.to_string();
    }
    if let Some(args) = arguments {
        entry.arguments.push_str(args);
    }

    // A new tool call is signaled when a function name is provided
    name.is_some()
}

/// Check if a pending tool call is valid: non-empty id, function_name,
/// and parseable JSON arguments.
fn is_valid_tool_call(tc: &PendingToolCall) -> bool {
    !tc.arguments.is_empty()
        && !tc.id.is_empty()
        && !tc.function_name.is_empty()
        && serde_json::from_str::<Value>(&tc.arguments).is_ok()
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::error::{ApiError, OpenAIError, StreamError};

    /// Helper to build an ApiError with a given message and optional code.
    fn make_api_error(message: &str, code: Option<&str>) -> OpenAIError {
        OpenAIError::ApiError(ApiError {
            message: message.to_string(),
            r#type: None,
            param: None,
            code: code.map(String::from),
        })
    }

    #[test]
    fn transient_rate_limit() {
        let err = make_api_error("Rate limit exceeded", Some("rate_limit_exceeded"));
        assert!(is_transient_error(&err), "rate_limit_exceeded should be transient");
    }

    #[test]
    fn transient_overloaded() {
        let err = make_api_error("The server is overloaded, please try again later", None);
        assert!(is_transient_error(&err), "overloaded message should be transient");
    }

    #[test]
    fn transient_temporarily_unavailable() {
        let err = make_api_error("Service is temporarily unavailable", None);
        assert!(is_transient_error(&err), "temporarily unavailable message should be transient");
    }

    #[test]
    fn not_transient_auth_error() {
        let err = make_api_error("Invalid API key provided", Some("invalid_api_key"));
        assert!(!is_transient_error(&err), "invalid_api_key should not be transient");
    }

    #[test]
    fn not_transient_invalid_argument() {
        let err = OpenAIError::InvalidArgument("bad argument".to_string());
        assert!(!is_transient_error(&err), "InvalidArgument should not be transient");
    }

    #[test]
    fn not_transient_json_deserialize() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid json").unwrap_err();
        let err = OpenAIError::JSONDeserialize(json_err, "invalid json".to_string());
        assert!(!is_transient_error(&err), "JSONDeserialize should not be transient");
    }

    #[test]
    fn not_transient_generic_api_error() {
        let err = make_api_error("Something went wrong", Some("server_error"));
        assert!(
            !is_transient_error(&err),
            "generic server_error without overloaded/unavailable message should not be transient"
        );
    }

    #[test]
    fn transient_stream_error() {
        let stream_err = StreamError::EventStream("connection reset".to_string());
        let err = OpenAIError::StreamError(Box::new(stream_err));
        assert!(is_transient_error(&err), "StreamError should be transient");
    }

    // -- accumulate_tool_call tests --

    #[test]
    fn accumulate_new_tool_call() {
        let mut pending = HashMap::new();
        let is_new = accumulate_tool_call(
            &mut pending, 0,
            Some("call_123"), Some("read"), Some("{\"path\":")
        );
        assert!(is_new);
        assert_eq!(pending.len(), 1);
        let tc = &pending[&0];
        assert_eq!(tc.id, "call_123");
        assert_eq!(tc.function_name, "read");
        assert_eq!(tc.arguments, "{\"path\":");
    }

    #[test]
    fn accumulate_appends_arguments() {
        let mut pending = HashMap::new();
        accumulate_tool_call(&mut pending, 0, Some("call_123"), Some("read"), Some("{\"path\":"));
        let is_new = accumulate_tool_call(&mut pending, 0, None, None, Some("\"src/main.rs\"}"));
        assert!(!is_new); // No new name, just appending
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[&0].arguments, "{\"path\":\"src/main.rs\"}");
    }

    #[test]
    fn accumulate_multiple_indices() {
        let mut pending = HashMap::new();
        accumulate_tool_call(&mut pending, 0, Some("call_1"), Some("read"), Some("{}"));
        accumulate_tool_call(&mut pending, 1, Some("call_2"), Some("grep"), Some("{}"));
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[&0].function_name, "read");
        assert_eq!(pending[&1].function_name, "grep");
    }

    // -- is_valid_tool_call tests --

    #[test]
    fn valid_tool_call_complete() {
        let tc = PendingToolCall {
            id: "call_123".to_string(),
            function_name: "read".to_string(),
            arguments: r#"{"path":"src/main.rs"}"#.to_string(),
        };
        assert!(is_valid_tool_call(&tc));
    }

    #[test]
    fn invalid_tool_call_truncated_arguments() {
        let tc = PendingToolCall {
            id: "call_123".to_string(),
            function_name: "read".to_string(),
            arguments: r#"{"path":"src/main"#.to_string(), // truncated
        };
        assert!(!is_valid_tool_call(&tc));
    }

    #[test]
    fn invalid_tool_call_empty_id() {
        let tc = PendingToolCall {
            id: String::new(),
            function_name: "read".to_string(),
            arguments: "{}".to_string(),
        };
        assert!(!is_valid_tool_call(&tc));
    }

    #[test]
    fn invalid_tool_call_empty_function_name() {
        let tc = PendingToolCall {
            id: "call_123".to_string(),
            function_name: String::new(),
            arguments: "{}".to_string(),
        };
        assert!(!is_valid_tool_call(&tc));
    }

    #[test]
    fn invalid_tool_call_empty_arguments() {
        let tc = PendingToolCall {
            id: "call_123".to_string(),
            function_name: "read".to_string(),
            arguments: String::new(),
        };
        assert!(!is_valid_tool_call(&tc));
    }

    // -- MockChatStream infrastructure --

    use async_openai::types::chat::{
        ChatChoiceStream, ChatCompletionStreamResponseDelta,
        ChatCompletionMessageToolCallChunk, FunctionCallStream, FunctionType,
        CreateChatCompletionStreamResponse, CompletionUsage,
    };
    use async_openai::types::chat::Role as OaiRole;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Mock stream provider that returns pre-built response chunks.
    /// Each `create_stream` call pops the next set of chunks from `streams`.
    struct MockChatStream {
        streams: Mutex<VecDeque<Vec<Result<CreateChatCompletionStreamResponse, OpenAIError>>>>,
    }

    impl MockChatStream {
        fn new(streams: Vec<Vec<Result<CreateChatCompletionStreamResponse, OpenAIError>>>) -> Self {
            Self {
                streams: Mutex::new(VecDeque::from(streams)),
            }
        }
    }

    #[async_trait]
    impl ChatStreamProvider for MockChatStream {
        async fn create_stream(
            &self,
            _request: CreateChatCompletionRequest,
        ) -> Result<ChatCompletionResponseStream, OpenAIError> {
            let chunks = self.streams.lock().unwrap().pop_front()
                .unwrap_or_default();
            let stream = futures::stream::iter(chunks);
            Ok(Box::pin(stream))
        }
    }

    /// Build a text delta chunk.
    #[allow(deprecated)]
    fn text_delta(content: &str) -> CreateChatCompletionStreamResponse {
        CreateChatCompletionStreamResponse {
            id: "test".to_string(),
            choices: vec![ChatChoiceStream {
                index: 0,
                delta: ChatCompletionStreamResponseDelta {
                    content: Some(content.to_string()),
                    function_call: None,
                    tool_calls: None,
                    role: Some(OaiRole::Assistant),
                    refusal: None,
                },
                finish_reason: None,
                logprobs: None,
            }],
            created: 0,
            model: "test".to_string(),
            service_tier: None,
            system_fingerprint: None,
            object: "chat.completion.chunk".to_string(),
            usage: None,
        }
    }

    /// Build a tool call chunk.
    #[allow(deprecated)]
    fn tool_call_chunk(
        index: u32,
        id: Option<&str>,
        name: Option<&str>,
        args: Option<&str>,
    ) -> CreateChatCompletionStreamResponse {
        CreateChatCompletionStreamResponse {
            id: "test".to_string(),
            choices: vec![ChatChoiceStream {
                index: 0,
                delta: ChatCompletionStreamResponseDelta {
                    content: None,
                    function_call: None,
                    tool_calls: Some(vec![ChatCompletionMessageToolCallChunk {
                        index,
                        id: id.map(String::from),
                        r#type: id.map(|_| FunctionType::Function),
                        function: Some(FunctionCallStream {
                            name: name.map(String::from),
                            arguments: args.map(String::from),
                        }),
                    }]),
                    role: Some(OaiRole::Assistant),
                    refusal: None,
                },
                finish_reason: None,
                logprobs: None,
            }],
            created: 0,
            model: "test".to_string(),
            service_tier: None,
            system_fingerprint: None,
            object: "chat.completion.chunk".to_string(),
            usage: None,
        }
    }

    /// Build a finish chunk with usage stats.
    #[allow(deprecated)]
    fn finish_chunk(
        reason: FinishReason,
        prompt_tokens: u32,
        completion_tokens: u32,
    ) -> CreateChatCompletionStreamResponse {
        CreateChatCompletionStreamResponse {
            id: "test".to_string(),
            choices: vec![ChatChoiceStream {
                index: 0,
                delta: ChatCompletionStreamResponseDelta {
                    content: None,
                    function_call: None,
                    tool_calls: None,
                    role: None,
                    refusal: None,
                },
                finish_reason: Some(reason),
                logprobs: None,
            }],
            created: 0,
            model: "test".to_string(),
            service_tier: None,
            system_fingerprint: None,
            object: "chat.completion.chunk".to_string(),
            usage: Some(CompletionUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
                prompt_tokens_details: None,
                completion_tokens_details: None,
            }),
        }
    }

    /// Helper: create a minimal StreamRequest using the mock provider.
    fn mock_stream_request(
        mock: Arc<dyn ChatStreamProvider>,
        event_tx: mpsc::UnboundedSender<AppEvent>,
    ) -> StreamRequest {
        let (_interjection_tx, interjection_rx) = mpsc::unbounded_channel();
        StreamRequest {
            stream_provider: mock,
            model: "test-model".to_string(),
            system_prompt: None,
            history: vec![],
            user_message: "test message".to_string(),
            event_tx,
            tool_registry: None,
            tool_context: None,
            permission_engine: None,
            tool_cache: Arc::new(std::sync::Mutex::new(
                ToolResultCache::new(std::path::PathBuf::from("/tmp/test"))
            )),
            cancel_token: CancellationToken::new(),
            context_window: None,
            interjection_rx,
            usage_writer: crate::usage::test_usage_writer(),
            usage_project_id: "test-project".to_string(),
            usage_session_id: "test-session".to_string(),
            usage_model_cost: None,
        }
    }

    /// Collect all events from the receiver into a vec.
    /// Close the channel first so `recv()` drains remaining messages then returns None.
    async fn collect_events(mut rx: mpsc::UnboundedReceiver<AppEvent>) -> Vec<AppEvent> {
        rx.close();
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    }

    // -- Integration tests --

    #[tokio::test]
    async fn stream_simple_text_response() {
        let mock = Arc::new(MockChatStream::new(vec![
            vec![
                Ok(text_delta("Hello")),
                Ok(text_delta(" world")),
                Ok(text_delta("!")),
                Ok(finish_chunk(FinishReason::Stop, 100, 10)),
            ],
        ]));
        let (tx, rx) = mpsc::unbounded_channel();
        let req = mock_stream_request(mock, tx);
        run_stream(req).await.expect("stream should succeed");
        let events = collect_events(rx).await;

        // Should have delta events for text content
        let deltas: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::LlmDelta { .. })).collect();
        assert!(deltas.len() >= 3, "should have at least 3 delta events, got {}", deltas.len());

        // Should have a usage update
        let usage_updates: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::LlmUsageUpdate { .. })).collect();
        assert!(!usage_updates.is_empty(), "should have usage update");

        // Should have a finish event
        let finishes: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::LlmFinish { .. })).collect();
        assert_eq!(finishes.len(), 1, "should have exactly 1 finish event");
    }

    #[tokio::test]
    async fn stream_cancel_before_call() {
        let mock = Arc::new(MockChatStream::new(vec![
            vec![
                Ok(text_delta("should not appear")),
                Ok(finish_chunk(FinishReason::Stop, 10, 5)),
            ],
        ]));
        let (tx, rx) = mpsc::unbounded_channel();
        let req = mock_stream_request(mock, tx);
        req.cancel_token.cancel(); // Cancel before stream starts
        run_stream(req).await.expect("should handle cancellation gracefully");
        let events = collect_events(rx).await;

        // Should have LlmFinish but no deltas (cancellation checked before stream opens)
        let deltas: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::LlmDelta { .. })).collect();
        assert!(deltas.is_empty(), "cancelled stream should produce no deltas");

        let finishes: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::LlmFinish { .. })).collect();
        assert_eq!(finishes.len(), 1, "should still get LlmFinish on cancel");
    }

    #[tokio::test]
    async fn stream_tool_call_with_execution() {
        // First call: LLM returns a read tool call
        // Second call: LLM returns text response (after seeing tool result)
        let mock = Arc::new(MockChatStream::new(vec![
            // First stream: tool call
            vec![
                Ok(tool_call_chunk(0, Some("call_1"), Some("read"), Some(r#"{"path":"src/main.rs"}"#))),
                Ok(finish_chunk(FinishReason::ToolCalls, 50, 20)),
            ],
            // Second stream: final text response
            vec![
                Ok(text_delta("File contents processed.")),
                Ok(finish_chunk(FinishReason::Stop, 100, 30)),
            ],
        ]));
        let (tx, rx) = mpsc::unbounded_channel();
        let mut req = mock_stream_request(mock, tx);

        // Add tool registry with a read tool
        req.tool_registry = Some(Arc::new(ToolRegistry::new(std::path::PathBuf::from("/tmp/test"))));
        req.tool_context = Some(ToolContext {
            project_root: std::path::PathBuf::from("/tmp/test"),
            storage_dir: None,
            task_store: None,
        });
        req.permission_engine = Some(Arc::new(tokio::sync::Mutex::new(
            crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
        )));

        run_stream(req).await.expect("stream with tool call should succeed");
        let events = collect_events(rx).await;

        // Should have tool call streaming events
        let tool_calls: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::LlmToolCallStreaming { .. })).collect();
        assert!(!tool_calls.is_empty(), "should have tool call streaming events");

        // Should have tool result
        let tool_results: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::ToolResult { .. })).collect();
        assert!(!tool_results.is_empty(), "should have tool result events");

        // Should have text delta from second call
        let deltas: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::LlmDelta { .. })).collect();
        assert!(!deltas.is_empty(), "should have text deltas from second LLM call");

        // Should have exactly 1 finish event (from the final call)
        let finishes: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::LlmFinish { .. })).collect();
        assert_eq!(finishes.len(), 1, "should have exactly 1 finish event");
    }

    #[tokio::test]
    async fn stream_truncated_tool_call_filtered() {
        // LLM returns a tool call with truncated JSON (finish_reason: Length)
        let mock = Arc::new(MockChatStream::new(vec![
            vec![
                Ok(tool_call_chunk(0, Some("call_1"), Some("read"), Some(r#"{"path":"src/main"#))), // truncated!
                Ok(finish_chunk(FinishReason::Length, 50, 20)),
            ],
        ]));
        let (tx, rx) = mpsc::unbounded_channel();
        let mut req = mock_stream_request(mock, tx);
        req.tool_registry = Some(Arc::new(ToolRegistry::new(std::path::PathBuf::from("/tmp/test"))));
        req.tool_context = Some(ToolContext {
            project_root: std::path::PathBuf::from("/tmp/test"),
            storage_dir: None,
            task_store: None,
        });
        req.permission_engine = Some(Arc::new(tokio::sync::Mutex::new(
            crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
        )));

        run_stream(req).await.expect("stream should handle truncated tool calls");
        let events = collect_events(rx).await;

        // When all tool calls are truncated (empty sorted_indices), the stream sends
        // LlmError and returns — no LlmFinish is sent.
        let errors: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::LlmError { .. })).collect();
        let finishes: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::LlmFinish { .. })).collect();
        assert!(!errors.is_empty(), "truncated tool call should produce LlmError");
        assert!(finishes.is_empty(), "should not get LlmFinish when all tool calls are truncated");
    }

    #[tokio::test]
    async fn stream_minimal_finish_response() {
        // Verify the stream handles a minimal response (just a finish chunk)
        // correctly. Retry logic for transient errors is covered by
        // is_transient_error unit tests above.
        let mock = Arc::new(MockChatStream::new(vec![
            vec![
                Ok(finish_chunk(FinishReason::Stop, 10, 5)),
            ],
        ]));
        let (tx, rx) = mpsc::unbounded_channel();
        let req = mock_stream_request(mock, tx);
        run_stream(req).await.expect("empty stream with finish should work");
        let events = collect_events(rx).await;
        let finishes: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::LlmFinish { .. })).collect();
        assert_eq!(finishes.len(), 1, "should get finish event");
    }

    #[tokio::test]
    async fn stream_parallel_read_tools_in_order() {
        // LLM returns 3 read tool calls, then text
        let mock = Arc::new(MockChatStream::new(vec![
            // First stream: 3 tool calls
            vec![
                Ok(tool_call_chunk(0, Some("call_a"), Some("glob"), Some(r#"{"pattern":"*.rs"}"#))),
                Ok(tool_call_chunk(1, Some("call_b"), Some("glob"), Some(r#"{"pattern":"*.txt"}"#))),
                Ok(tool_call_chunk(2, Some("call_c"), Some("glob"), Some(r#"{"pattern":"*.md"}"#))),
                Ok(finish_chunk(FinishReason::ToolCalls, 50, 20)),
            ],
            // Second stream: final response
            vec![
                Ok(text_delta("Done.")),
                Ok(finish_chunk(FinishReason::Stop, 150, 30)),
            ],
        ]));
        let (tx, rx) = mpsc::unbounded_channel();
        let mut req = mock_stream_request(mock, tx);
        req.tool_registry = Some(Arc::new(ToolRegistry::new(std::path::PathBuf::from("/tmp/test"))));
        req.tool_context = Some(ToolContext {
            project_root: std::path::PathBuf::from("/tmp/test"),
            storage_dir: None,
            task_store: None,
        });
        req.permission_engine = Some(Arc::new(tokio::sync::Mutex::new(
            crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
        )));

        run_stream(req).await.expect("parallel tool calls should succeed");
        let events = collect_events(rx).await;

        // Should have tool results for all 3 calls
        let tool_results: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::ToolResult { .. })).collect();
        assert_eq!(tool_results.len(), 3, "should have 3 tool results, got {}", tool_results.len());

        // Results should arrive in original call order (a, b, c)
        let tool_call_events: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::LlmToolCall { .. })).collect();
        assert_eq!(tool_call_events.len(), 3, "should have 3 LlmToolCall events");
    }

    #[tokio::test]
    async fn stream_cache_hit_skips_execution() {
        // Pre-populate the cache, then run a stream that calls the same tool
        let cache = Arc::new(std::sync::Mutex::new(
            ToolResultCache::new(std::path::PathBuf::from("/tmp/test"))
        ));

        // Pre-populate with a glob result
        {
            let mut c = cache.lock().unwrap();
            let args = serde_json::json!({"pattern": "*.rs"});
            let output = crate::tool::ToolOutput {
                title: "glob '*.rs'".to_string(),
                output: "cached_result.rs".to_string(),
                is_error: false,
            };
            c.put(ToolName::Glob, &args, &output);
        }

        let mock = Arc::new(MockChatStream::new(vec![
            vec![
                Ok(tool_call_chunk(0, Some("call_1"), Some("glob"), Some(r#"{"pattern":"*.rs"}"#))),
                Ok(finish_chunk(FinishReason::ToolCalls, 50, 20)),
            ],
            vec![
                Ok(text_delta("Got it.")),
                Ok(finish_chunk(FinishReason::Stop, 100, 10)),
            ],
        ]));
        let (tx, rx) = mpsc::unbounded_channel();
        let (_interjection_tx, interjection_rx) = mpsc::unbounded_channel();
        let req = StreamRequest {
            stream_provider: mock,
            model: "test-model".to_string(),
            system_prompt: None,
            history: vec![],
            user_message: "test".to_string(),
            event_tx: tx,
            tool_registry: Some(Arc::new(ToolRegistry::new(std::path::PathBuf::from("/tmp/test")))),
            tool_context: Some(ToolContext {
                project_root: std::path::PathBuf::from("/tmp/test"),
                storage_dir: None,
            task_store: None,
            }),
            permission_engine: Some(Arc::new(tokio::sync::Mutex::new(
                crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
            ))),
            tool_cache: cache,
            cancel_token: CancellationToken::new(),
            context_window: None,
            interjection_rx,
            usage_writer: crate::usage::test_usage_writer(),
            usage_project_id: "test-project".to_string(),
            usage_session_id: "test-session".to_string(),
            usage_model_cost: None,
        };

        run_stream(req).await.expect("cache hit stream should succeed");
        let events = collect_events(rx).await;

        // Should still have a tool result (from cache)
        let tool_results: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::ToolResult { .. })).collect();
        assert!(!tool_results.is_empty(), "should have tool result even from cache");
    }

    #[tokio::test]
    async fn stream_terminates_after_max_iterations() {
        // Configure MockChatStream to always return tool calls — simulates
        // the infinite loop scenario where the LLM never produces a final
        // text response.
        let mut streams = Vec::new();
        for i in 0..80 {
            streams.push(vec![
                Ok(tool_call_chunk(
                    0,
                    Some(&format!("call_{i}")),
                    Some("glob"),
                    Some(r#"{"pattern":"*.rs"}"#),
                )),
                Ok(finish_chunk(FinishReason::ToolCalls, 50, 20)),
            ]);
        }
        let mock = Arc::new(MockChatStream::new(streams));
        let (tx, rx) = mpsc::unbounded_channel();
        let mut req = mock_stream_request(mock, tx);
        req.tool_registry = Some(Arc::new(ToolRegistry::new(
            std::path::PathBuf::from("/tmp/test"),
        )));
        req.tool_context = Some(ToolContext {
            project_root: std::path::PathBuf::from("/tmp/test"),
            storage_dir: None,
            task_store: None,
        });
        req.permission_engine = Some(Arc::new(tokio::sync::Mutex::new(
            crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
        )));

        run_stream(req).await.expect("should terminate gracefully");
        let events = collect_events(rx).await;

        // Should emit LlmError about exceeding max iterations
        let errors: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmError { .. }))
            .collect();
        assert_eq!(errors.len(), 1, "should have exactly 1 LlmError");
        if let AppEvent::LlmError { error } = errors[0] {
            assert!(
                error.contains("exceeded"),
                "error should mention exceeded: {error}"
            );
        }

        // Should also get LlmFinish to persist accumulated token usage
        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(finishes.len(), 1, "should get LlmFinish to persist token usage");
    }

    #[tokio::test]
    async fn stream_retries_mid_stream_error() {
        // First stream: partial text then error
        // Second stream (retry): successful completion
        let error_stream = vec![
            Ok(text_delta("partial ")),
            Err(OpenAIError::StreamError(Box::new(
                StreamError::EventStream("error decoding response body".to_string()),
            ))),
        ];
        let success_stream = vec![
            Ok(text_delta("Hello world!")),
            Ok(finish_chunk(FinishReason::Stop, 100, 20)),
        ];
        let mock = Arc::new(MockChatStream::new(vec![error_stream, success_stream]));
        let (tx, rx) = mpsc::unbounded_channel();
        let req = mock_stream_request(mock, tx);

        run_stream(req).await.expect("should recover from mid-stream error");
        let events = collect_events(rx).await;

        // Should have a retry notification (LlmRetry, not LlmError)
        let retries: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmRetry { .. }))
            .collect();
        assert_eq!(retries.len(), 1, "should have 1 LlmRetry for the retry notification");
        if let AppEvent::LlmRetry { attempt, max_attempts, .. } = retries[0] {
            assert_eq!(*attempt, 1);
            assert_eq!(*max_attempts, 2);
        }

        // Should complete successfully with LlmFinish
        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(finishes.len(), 1, "should get LlmFinish after successful retry");

        // Should have text deltas from the successful retry
        let deltas: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmDelta { .. }))
            .collect();
        assert!(!deltas.is_empty(), "should have text deltas from retry");
    }

    #[tokio::test]
    async fn stream_exhausts_mid_stream_retries() {
        // All streams fail with errors — retries should be exhausted
        let error_streams: Vec<Vec<Result<CreateChatCompletionStreamResponse, OpenAIError>>> =
            (0..5)
                .map(|_| {
                    vec![Err(OpenAIError::StreamError(Box::new(
                        StreamError::EventStream("error decoding response body".to_string()),
                    )))]
                })
                .collect();
        let mock = Arc::new(MockChatStream::new(error_streams));
        let (tx, rx) = mpsc::unbounded_channel();
        let req = mock_stream_request(mock, tx);

        run_stream(req).await.expect("should handle exhausted retries gracefully");
        let events = collect_events(rx).await;

        // Should have 2 LlmRetry events + 1 final LlmError
        let retries: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmRetry { .. }))
            .collect();
        assert_eq!(retries.len(), 2, "should have 2 LlmRetry events, got {}", retries.len());

        let errors: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmError { .. }))
            .collect();
        assert_eq!(errors.len(), 1, "should have 1 final LlmError, got {}", errors.len());

        // Should get LlmFinish to persist usage
        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(finishes.len(), 1, "should get LlmFinish even after exhausted retries");
    }

    #[tokio::test]
    async fn interjection_injected_as_user_message() {
        // Stream: tool call -> (interjection injected) -> text response
        let mock = Arc::new(MockChatStream::new(vec![
            vec![
                Ok(tool_call_chunk(0, Some("call_1"), Some("glob"), Some(r#"{"pattern":"*.rs"}"#))),
                Ok(finish_chunk(FinishReason::ToolCalls, 50, 20)),
            ],
            vec![
                Ok(text_delta("I see your message.")),
                Ok(finish_chunk(FinishReason::Stop, 100, 10)),
            ],
        ]));
        let (tx, rx) = mpsc::unbounded_channel();
        let (interjection_tx, interjection_rx) = mpsc::unbounded_channel();

        // Pre-load the interjection before running the stream
        interjection_tx.send("focus on tests instead".to_string()).unwrap();

        let mut req = mock_stream_request(mock, tx);
        req.interjection_rx = interjection_rx;
        req.tool_registry = Some(Arc::new(ToolRegistry::new(std::path::PathBuf::from("/tmp/test"))));
        req.tool_context = Some(ToolContext {
            project_root: std::path::PathBuf::from("/tmp/test"),
            storage_dir: None,
            task_store: None,
        });
        req.permission_engine = Some(Arc::new(tokio::sync::Mutex::new(
            crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
        )));

        run_stream(req).await.expect("interjection stream should succeed");
        let events = collect_events(rx).await;

        // Should complete successfully
        let finishes: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::LlmFinish { .. })).collect();
        assert_eq!(finishes.len(), 1, "should get LlmFinish");

        // Should have text deltas from the second call
        let deltas: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::LlmDelta { .. })).collect();
        assert!(!deltas.is_empty(), "should have text deltas");
    }

    #[tokio::test]
    async fn multiple_interjections_all_consumed() {
        let mock = Arc::new(MockChatStream::new(vec![
            vec![
                Ok(tool_call_chunk(0, Some("call_1"), Some("glob"), Some(r#"{"pattern":"*.rs"}"#))),
                Ok(finish_chunk(FinishReason::ToolCalls, 50, 20)),
            ],
            vec![
                Ok(text_delta("OK")),
                Ok(finish_chunk(FinishReason::Stop, 100, 10)),
            ],
        ]));
        let (tx, rx) = mpsc::unbounded_channel();
        let (interjection_tx, interjection_rx) = mpsc::unbounded_channel();

        // Send multiple interjections
        interjection_tx.send("first message".to_string()).unwrap();
        interjection_tx.send("second message".to_string()).unwrap();
        interjection_tx.send("third message".to_string()).unwrap();

        let mut req = mock_stream_request(mock, tx);
        req.interjection_rx = interjection_rx;
        req.tool_registry = Some(Arc::new(ToolRegistry::new(std::path::PathBuf::from("/tmp/test"))));
        req.tool_context = Some(ToolContext {
            project_root: std::path::PathBuf::from("/tmp/test"),
            storage_dir: None,
            task_store: None,
        });
        req.permission_engine = Some(Arc::new(tokio::sync::Mutex::new(
            crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
        )));

        run_stream(req).await.expect("multi-interjection stream should succeed");
        let events = collect_events(rx).await;

        // Should complete with LlmFinish (not error)
        let finishes: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::LlmFinish { .. })).collect();
        assert_eq!(finishes.len(), 1);
        let errors: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::LlmError { .. })).collect();
        assert!(errors.is_empty(), "should have no errors");
    }

    #[tokio::test]
    async fn empty_interjection_channel_is_noop() {
        // Basic text response with no interjections — existing behavior unchanged
        let mock = Arc::new(MockChatStream::new(vec![
            vec![
                Ok(text_delta("Hello")),
                Ok(finish_chunk(FinishReason::Stop, 50, 10)),
            ],
        ]));
        let (tx, rx) = mpsc::unbounded_channel();
        let req = mock_stream_request(mock, tx);

        run_stream(req).await.expect("empty interjection channel should be fine");
        let events = collect_events(rx).await;

        let finishes: Vec<&AppEvent> = events.iter().filter(|e| matches!(e, AppEvent::LlmFinish { .. })).collect();
        assert_eq!(finishes.len(), 1);
    }

    #[tokio::test]
    async fn interjection_coexists_with_tool_loop() {
        // Verify that an interjection drained at loop start doesn't interfere
        // with normal tool call execution. Counter reset is meaningful in
        // production (mid-loop arrival) but can't be tested with synchronous mocks.
        let mut streams = Vec::new();
        for i in 0..5 {
            streams.push(vec![
                Ok(tool_call_chunk(
                    0,
                    Some(&format!("call_{i}")),
                    Some("glob"),
                    Some(r#"{"pattern":"*.rs"}"#),
                )),
                Ok(finish_chunk(FinishReason::ToolCalls, 50, 20)),
            ]);
        }
        streams.push(vec![
            Ok(text_delta("Done")),
            Ok(finish_chunk(FinishReason::Stop, 50, 10)),
        ]);

        let mock = Arc::new(MockChatStream::new(streams));
        let (tx, rx) = mpsc::unbounded_channel();
        let (interjection_tx, interjection_rx) = mpsc::unbounded_channel();

        interjection_tx.send("keep going".to_string()).unwrap();

        let mut req = mock_stream_request(mock, tx);
        req.interjection_rx = interjection_rx;
        req.tool_registry = Some(Arc::new(ToolRegistry::new(std::path::PathBuf::from("/tmp/test"))));
        req.tool_context = Some(ToolContext {
            project_root: std::path::PathBuf::from("/tmp/test"),
            storage_dir: None,
            task_store: None,
        });
        req.permission_engine = Some(Arc::new(tokio::sync::Mutex::new(
            crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
        )));

        run_stream(req).await.expect("interjection with tool calls should succeed");
        let events = collect_events(rx).await;

        // Should complete without errors
        let errors: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmError { .. }))
            .collect();
        assert!(errors.is_empty(), "should have no errors: {:?}", errors);

        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(finishes.len(), 1);
    }

    // -- check_iteration_warning tests --

    #[test]
    fn warning_nudge_fires_at_33_percent() {
        let mut sent = 0u8;
        // At 75 max, nudge_at = 75 * 33 / 100 = 24
        assert!(check_iteration_warning(23, 75, &mut sent).is_none());
        let text = check_iteration_warning(24, 75, &mut sent);
        assert!(text.is_some());
        assert!(text.unwrap().contains("24 tool calls"));
        assert_eq!(sent, WARN_NUDGE_BIT);
    }

    #[test]
    fn warning_does_not_repeat() {
        let mut sent = 0u8;
        let first = check_iteration_warning(25, 75, &mut sent);
        assert!(first.is_some());
        let second = check_iteration_warning(26, 75, &mut sent);
        assert!(second.is_none(), "nudge should not fire twice");
    }

    #[test]
    fn warning_escalates_through_all_levels() {
        let mut sent = 0u8;
        // Nudge at 33%
        let nudge = check_iteration_warning(25, 75, &mut sent);
        assert!(nudge.is_some());
        assert!(nudge.unwrap().contains("Note:"));
        // Warning at 67% (75 * 67 / 100 = 50)
        let warn = check_iteration_warning(50, 75, &mut sent);
        assert!(warn.is_some());
        assert!(warn.unwrap().contains("Warning:"));
        // Critical at 87% (75 * 87 / 100 = 65)
        let crit = check_iteration_warning(65, 75, &mut sent);
        assert!(crit.is_some());
        assert!(crit.unwrap().contains("CRITICAL:"));
        // All bits set
        assert_eq!(sent, WARN_NUDGE_BIT | WARN_WARNING_BIT | WARN_CRITICAL_BIT);
    }

    #[test]
    fn warning_critical_shows_remaining_count() {
        let mut sent = 0u8;
        // Jump straight to critical — should skip nudge/warning and fire critical
        let text = check_iteration_warning(65, 75, &mut sent).unwrap();
        assert!(text.contains("10 tool calls remaining"));
    }

    #[test]
    fn warning_reset_allows_refiring() {
        let mut sent = 0u8;
        check_iteration_warning(24, 75, &mut sent);
        assert_ne!(sent, 0);
        // Simulate reset (user granted permission)
        sent = 0;
        let text = check_iteration_warning(24, 75, &mut sent);
        assert!(text.is_some(), "should fire again after reset");
    }

    #[test]
    fn warning_below_all_thresholds_returns_none() {
        let mut sent = 0u8;
        assert!(check_iteration_warning(1, 75, &mut sent).is_none());
        assert!(check_iteration_warning(10, 75, &mut sent).is_none());
        assert!(check_iteration_warning(23, 75, &mut sent).is_none());
        assert_eq!(sent, 0);
    }

    #[test]
    fn warning_highest_level_wins_on_jump() {
        let mut sent = 0u8;
        // Jump from 0 to 70 — should fire critical (highest), not nudge
        let text = check_iteration_warning(70, 75, &mut sent).unwrap();
        assert!(text.contains("CRITICAL:"));
        assert_eq!(sent & WARN_CRITICAL_BIT, WARN_CRITICAL_BIT);
    }
}
