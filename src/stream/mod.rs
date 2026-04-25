//! LLM streaming bridge with tool call loop.
//!
//! Spawns a tokio task that:
//! 1. Opens an SSE stream via async-openai
//! 2. Processes chunks, sending text deltas to the UI
//! 3. Accumulates tool call fragments from the stream
//! 4. When the stream finishes with tool calls, executes them and loops back

mod agent;
mod phases;
mod recovery;
mod tools;

use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use async_openai::{
    Client,
    types::chat::{
        ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestSystemMessageContent, ChatCompletionRequestToolMessageContent,
        ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageContent,
        ChatCompletionResponseStream, ChatCompletionStreamOptions, ChatCompletionTool,
        ChatCompletionTools, CompletionUsage, CreateChatCompletionRequest, FinishReason,
        FunctionCall, FunctionObject,
    },
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::{
    config::ModelCost,
    context::cache::ToolResultCache,
    event::{AppEvent, StreamUsage},
    permission::PermissionEngine,
    provider::config::LlmEndpointConfig,
    tool::{ToolContext, ToolName, ToolRegistry},
    usage::{UsageWriter, types::ApiCallRecord},
};

use recovery::{
    LengthCause, WARN_CRITICAL_PCT, check_iteration_warning, classify_length_cause,
    is_transient_error, length_recovery_no_tools, length_recovery_truncated_tools,
};
use tools::{PendingToolCall, accumulate_tool_call, estimate_message_chars, is_valid_tool_call};

/// Abstraction over LLM stream creation — enables mock testing of the tool loop.
pub trait ChatStreamProvider: Send + Sync {
    fn create_stream(
        &self,
        request: CreateChatCompletionRequest,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<ChatCompletionResponseStream, async_openai::error::OpenAIError>,
                > + Send
                + '_,
        >,
    >;
}

/// Production implementation wrapping async-openai's Client.
pub struct OpenAIChatStream {
    client: Client<LlmEndpointConfig>,
}

impl OpenAIChatStream {
    pub fn new(client: Client<LlmEndpointConfig>) -> Self {
        Self { client }
    }
}

impl ChatStreamProvider for OpenAIChatStream {
    fn create_stream(
        &self,
        request: CreateChatCompletionRequest,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<ChatCompletionResponseStream, async_openai::error::OpenAIError>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move { self.client.chat().create_stream(request).await })
    }
}

/// Captures the shared resources needed to spawn sub-agent streams.
///
/// Constructed in `app.rs` alongside the parent `StreamRequest`, then passed
/// into `run_stream()`. When a sub-agent is spawned, a fresh `StreamRequest`
/// is built from these fields with `agent_spawner: None` to prevent recursion.
#[derive(Clone)]
pub struct AgentSpawner {
    pub stream_provider: Arc<dyn ChatStreamProvider>,
    pub primary_model: String,
    pub small_model: Option<String>,
    pub project_root: std::path::PathBuf,
    pub tool_context: ToolContext,
    pub permission_engine: Option<Arc<tokio::sync::Mutex<PermissionEngine>>>,
    pub context_window: Option<u64>,
    pub usage_writer: UsageWriter,
    pub usage_project_id: String,
    pub usage_session_id: String,
    pub cancel_token: CancellationToken,
    /// MCP manager for General agents (None for Explore/Plan).
    pub mcp_manager: Option<Arc<tokio::sync::Mutex<crate::mcp::McpManager>>>,
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
    /// Whether the agent is in Plan mode (read-only analysis) — uses a lower iteration limit.
    /// Snapshotted at stream launch; toggling mode mid-stream does not change the limit.
    pub is_plan_mode: bool,
    /// Resources for spawning sub-agents. `None` in sub-agent streams to prevent recursion.
    pub agent_spawner: Option<AgentSpawner>,
    /// MCP manager for dynamic tool execution. May have no servers (empty snapshot).
    /// `None` for sub-agent streams where MCP should be unavailable.
    pub mcp_manager: Option<Arc<tokio::sync::Mutex<crate::mcp::McpManager>>>,
}

impl StreamRequest {
    /// Spawn a tokio task that streams the LLM response and sends events.
    /// Returns the JoinHandle so the caller can track task completion.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if self.run().await.is_err() {
                // Error already sent via channel in run()
            }
        })
    }

    pub(super) async fn run(self) -> Result<(), ()> {
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
            is_plan_mode,
            agent_spawner,
            mcp_manager,
        } = self;

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

        // Build tool definitions for the API (mutable — stripped at CRITICAL threshold).
        // Helper: convert a JSON tool def to ChatCompletionTools.
        fn json_to_tool(def: &Value) -> Option<ChatCompletionTools> {
            let func = def.get("function")?;
            Some(ChatCompletionTools::Function(ChatCompletionTool {
                function: FunctionObject {
                    name: func.get("name")?.as_str()?.to_string(),
                    description: func
                        .get("description")
                        .and_then(|d| d.as_str())
                        .map(String::from),
                    parameters: func.get("parameters").cloned(),
                    strict: None,
                },
            }))
        }

        // Get a lock-free snapshot of MCP tool metadata. We briefly await the lock
        // only to clone the Arc, then drop it so streams always get a consistent
        // view of available MCP tools.
        let mcp_snapshot: Option<std::sync::Arc<crate::mcp::McpToolSnapshot>> =
            if let Some(ref mgr) = mcp_manager {
                let mgr = mgr.lock().await;
                let snap = mgr.tool_snapshot();
                if !snap.is_empty() { Some(snap) } else { None }
            } else {
                None
            };

        // Helper closure rebuilds tools from the registry + MCP snapshot (no mutex needed).
        let build_tools = |registry: &ToolRegistry,
                           snap: &Option<std::sync::Arc<crate::mcp::McpToolSnapshot>>|
         -> Vec<ChatCompletionTools> {
            let mut tools: Vec<ChatCompletionTools> = registry
                .tool_definitions()
                .into_iter()
                .filter_map(|def| json_to_tool(&def))
                .collect();

            // Append MCP tool definitions from the lock-free snapshot
            if let Some(snap) = snap {
                tools.extend(snap.tool_definitions().iter().filter_map(json_to_tool));
            }

            tools
        };
        let mut tools: Option<Vec<ChatCompletionTools>> = tool_registry
            .as_ref()
            .map(|r| build_tools(r, &mcp_snapshot));

        let mut total_usage = StreamUsage::default();
        let mut current_iteration_tool_count: usize = 0;
        let mut current_iteration_cache_repeats: usize;

        // Tool result cache — shared across stream tasks within a session.
        // Avoids re-executing identical read operations across messages.

        // Safety limit: prevent infinite tool-call loops (e.g. compressor/cache
        // feedback oscillation where the LLM re-reads the same files forever).
        // Resets when the user grants a permission (proving active supervision).
        // Plan mode uses a lower limit since it's read-only analysis.
        const MAX_TOOL_ITERATIONS: u32 = 75;
        const MAX_PLAN_ITERATIONS: u32 = 55;
        let effective_max = if is_plan_mode {
            MAX_PLAN_ITERATIONS
        } else {
            MAX_TOOL_ITERATIONS
        };
        let mut iteration_count: u32 = 0;
        let mut total_iteration_count: u32 = 0;

        // Bitmask tracks which escalating warnings have fired this cycle
        // (resets with iteration_count on user interaction).
        let mut warnings_sent: u8 = 0;

        // Tool stripping: at CRITICAL threshold, tools are removed from the API request
        // so the LLM structurally cannot make tool calls — it must produce text.
        let mut tools_stripped = false;

        // Final chance: at hard limit, one last tool-free API call is made before termination.
        let mut final_chance_taken = false;

        // Track previous iteration's tool count for keeping 2 iterations uncompressed.
        // Updated at bottom of loop before current_iteration_tool_count is reset to 0.
        let mut prev_iteration_tool_count: usize = 0;

        // Track consecutive iterations where ALL tool calls were cache-repeat hits.
        // When this reaches the threshold, inject a strong "stop looping" message.
        let mut consecutive_cached_iterations: u32 = 0;
        const MAX_CONSECUTIVE_CACHED: u32 = 2;

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
                // Restore tools if they were stripped — user interaction proves supervision
                if tools_stripped {
                    tools_stripped = false;
                    final_chance_taken = false;
                    tools = tool_registry
                        .as_ref()
                        .map(|r| build_tools(r, &mcp_snapshot));
                }
            }

            total_iteration_count += 1;
            iteration_count += 1;
            // NOTE: prev_iteration_tool_count is updated at the bottom of the loop,
            // right before current_iteration_tool_count is reset to 0, so it captures
            // the actual tool count from the previous iteration.
            let mut user_interacted_this_iteration = false;
            let call_start = std::time::Instant::now();
            if iteration_count > effective_max {
                if !final_chance_taken {
                    final_chance_taken = true;
                    tools = None;
                    tools_stripped = true;
                    messages.push(ChatCompletionRequestMessage::User(
                    ChatCompletionRequestUserMessage {
                        content: ChatCompletionRequestUserMessageContent::Text(
                            "[SYSTEM: Maximum tool calls exceeded. Provide your complete response \
                             NOW. Summarize everything you found and answer the user's question. \
                             This is your final opportunity to respond.]"
                                .to_string(),
                        ),
                        name: None,
                    },
                ));
                    let _ = event_tx.send(AppEvent::StreamNotice {
                        text: "⚙ Tool limit reached — requesting final response".to_string(),
                    });
                    tracing::warn!(
                        iterations = iteration_count,
                        total_iterations = total_iteration_count,
                        effective_max,
                        "tool loop hit max iterations — making final-chance API call"
                    );
                    continue; // One more API call with no tools
                } else {
                    // Final chance already taken — truly done
                    tracing::error!(
                        iterations = iteration_count,
                        total_iterations = total_iteration_count,
                        effective_max,
                        "tool loop exceeded max iterations (final chance exhausted)"
                    );
                    let _ = event_tx.send(AppEvent::LlmError {
                        error: format!(
                            "Tool loop exceeded {effective_max} iterations. Try /compact or /new."
                        ),
                    });
                    let _ = event_tx.send(AppEvent::LlmFinish {
                        usage: Some(total_usage),
                    });
                    return Ok(());
                }
            }

            // Compress old tool results from prior iterations to reduce token usage.
            // Deferred: only compress when context usage exceeds 40% of the window.
            // This prevents destroying tool results the LLM still needs early on.
            // Keep the last 2 iterations uncompressed so the LLM has recent context.
            let has_tool_messages = messages
                .iter()
                .any(|m| matches!(m, ChatCompletionRequestMessage::Tool(_)));
            let keep_recent = current_iteration_tool_count + prev_iteration_tool_count;
            if has_tool_messages || current_iteration_tool_count > 0 {
                let payload_estimate: usize = messages.iter().map(estimate_message_chars).sum();
                // Rough heuristic: ~4 chars/token. Underestimates for code-heavy content,
                // but the 60% aggressive pruning safety valve catches underestimates.
                let estimated_tokens = payload_estimate / 4;
                let should_compress = context_window
                    .map(|cw| cw > 0 && estimated_tokens as u64 > cw * 40 / 100)
                    .unwrap_or(true); // Compress if we don't know the window size

                if should_compress {
                    crate::context::compressor::compress_old_tool_results(
                        &mut messages,
                        keep_recent,
                    );
                }
            }

            // Aggressive pruning: if conversation is still large after normal compression,
            // compress ALL tool results (including current iteration) to stay under budget.
            // Uses keep_recent=0 so even the latest tool results get compressed.
            if let Some(ctx_window) = context_window {
                let estimated_chars: usize = messages.iter().map(estimate_message_chars).sum();
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

            // Capture this iteration's tool count before resetting, so keep_recent
            // at the top of the next iteration correctly reflects "last 2 iterations".
            prev_iteration_tool_count = current_iteration_tool_count;
            current_iteration_tool_count = 0;
            current_iteration_cache_repeats = 0;

            // Estimate payload size for diagnostics
            let payload_chars: usize = messages
                .iter()
                .map(|m| match m {
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
                })
                .sum();
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

            if let Some(ref t) = tools
                && !t.is_empty()
            {
                request.tools = Some(t.clone());
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
            let mut call_usage: Option<CompletionUsage> = None;

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
                                // Capture usage (last-wins — some providers emit multiple chunks)
                                if let Some(u) = response.usage {
                                    call_usage = Some(u);
                                }

                                // Process each choice's delta
                                for choice in &response.choices {
                                    // TODO: Emit AppEvent::LlmReasoning for reasoning/thinking tokens.
                                    // OpenAI o1/o3 models send reasoning content via a `reasoning_content`
                                    // field on the stream delta, but async-openai 0.34 does not expose this
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
                                    if let Some(content) = &choice.delta.content
                                        && !content.is_empty() {
                                            assistant_content.push_str(content);
                                            let _ = event_tx.send(AppEvent::LlmDelta {
                                                text: content.clone(),
                                            });
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
                                            if tc.function.as_ref().and_then(|f| f.name.as_ref()).is_some()
                                                && let Some(entry) = pending_tool_calls.get(&tc.index)
                                                    && let Ok(name) = entry.function_name.parse::<ToolName>() {
                                                        let _ = event_tx.send(AppEvent::LlmToolCallStreaming {
                                                            count: pending_tool_calls.len(),
                                                            tool_name: name,
                                                        });
                                                    }
                                        }
                                    }

                                    // Track finish reason
                                    if let Some(reason) = &choice.finish_reason {
                                        finish_reason = Some(*reason);
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

            // Process usage once after a successful stream ends (last-wins avoids
            // double-counting when providers emit usage in multiple chunks).
            // Skip on mid-stream errors — the call may be retried.
            if stream_chunk_error.is_none()
                && let Some(u) = &call_usage
            {
                tracing::info!(
                    prompt = u.prompt_tokens,
                    completion = u.completion_tokens,
                    total = u.total_tokens,
                    "usage data received"
                );
                total_usage.prompt_tokens += u.prompt_tokens;
                total_usage.completion_tokens += u.completion_tokens;
                total_usage.total_tokens += u.total_tokens;

                let _ = event_tx.send(AppEvent::LlmUsageUpdate {
                    usage: StreamUsage {
                        prompt_tokens: u.prompt_tokens,
                        completion_tokens: u.completion_tokens,
                        total_tokens: u.total_tokens,
                    },
                });

                let cost = usage_model_cost.as_ref().map(|mc| {
                    u.prompt_tokens as f64 * mc.input_per_million / 1_000_000.0
                        + u.completion_tokens as f64 * mc.output_per_million / 1_000_000.0
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
                // If the model stopped due to length, recover based on cause:
                // context pressure → compress + strip tools + retry
                // output truncation → strip tools only + retry
                if matches!(finish_reason, Some(FinishReason::Length)) {
                    let cause = classify_length_cause(total_usage.prompt_tokens, context_window);
                    let msgs = length_recovery_no_tools(&cause);

                    if !tools_stripped {
                        tools_stripped = true;
                        tools = None;

                        if matches!(cause, LengthCause::ContextPressure) {
                            tracing::warn!(
                                "finish_reason=Length with no tool calls (context pressured) — compressing and retrying without tools"
                            );
                            crate::context::compressor::compress_old_tool_results(&mut messages, 0);
                        } else {
                            tracing::info!(
                                prompt = total_usage.prompt_tokens,
                                completion = total_usage.completion_tokens,
                                context_window = ?context_window,
                                "output truncation recovery (context not pressured)"
                            );
                        }

                        // Add any partial assistant text before the retry
                        if !assistant_content.is_empty() {
                            messages.push(
                                ChatCompletionRequestAssistantMessage::from(
                                    assistant_content.as_str(),
                                )
                                .into(),
                            );
                        }

                        messages.push(ChatCompletionRequestMessage::User(
                            ChatCompletionRequestUserMessage {
                                content: ChatCompletionRequestUserMessageContent::Text(
                                    msgs.system_msg.to_string(),
                                ),
                                name: None,
                            },
                        ));
                        let _ = event_tx.send(AppEvent::StreamNotice {
                            text: msgs.notice.to_string(),
                        });
                        continue;
                    }
                    // Already retried
                    let _ = event_tx.send(AppEvent::LlmError {
                        error: msgs.error_msg.to_string(),
                    });
                    let _ = event_tx.send(AppEvent::LlmFinish {
                        usage: Some(total_usage),
                    });
                    return Ok(());
                }
                // Before finishing, check for user interjections that arrived
                // during streaming. If found, push the assistant's response and
                // the interjection(s) into the conversation and continue the loop
                // so the LLM can respond to them.
                let mut has_interjection = false;
                while let Ok(text) = interjection_rx.try_recv() {
                    if !has_interjection {
                        // Push the assistant's completed response first
                        if !assistant_content.is_empty() {
                            messages.push(
                                ChatCompletionRequestAssistantMessage::from(
                                    assistant_content.as_str(),
                                )
                                .into(),
                            );
                        }
                        has_interjection = true;
                    }
                    tracing::info!(
                        "user interjection received after response: {} chars",
                        text.len()
                    );
                    messages.push(ChatCompletionRequestMessage::User(
                        ChatCompletionRequestUserMessage {
                            content: ChatCompletionRequestUserMessageContent::Text(text),
                            name: None,
                        },
                    ));
                    iteration_count = 0;
                    warnings_sent = 0;
                    if tools_stripped {
                        tools_stripped = false;
                        final_chance_taken = false;
                        tools = tool_registry
                            .as_ref()
                            .map(|r| build_tools(r, &mcp_snapshot));
                    }
                }
                if has_interjection {
                    // Tell the UI to start a fresh Assistant block for the new response
                    let _ = event_tx.send(AppEvent::LlmResponseStart);
                    continue;
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
                lsp_manager: None,
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
                let cause = classify_length_cause(total_usage.prompt_tokens, context_window);
                let msgs = length_recovery_truncated_tools(&cause);

                if !tools_stripped {
                    tools_stripped = true;
                    tools = None;

                    if matches!(cause, LengthCause::ContextPressure) {
                        tracing::warn!(
                            "all tool calls truncated (context pressured) — compressing and retrying without tools"
                        );
                        crate::context::compressor::compress_old_tool_results(&mut messages, 0);
                    } else {
                        tracing::info!(
                            prompt = total_usage.prompt_tokens,
                            completion = total_usage.completion_tokens,
                            context_window = ?context_window,
                            "output truncation recovery (context not pressured)"
                        );
                    }

                    // Preserve any partial assistant text from this turn
                    if !assistant_content.is_empty() {
                        messages.push(
                            ChatCompletionRequestAssistantMessage::from(assistant_content.as_str())
                                .into(),
                        );
                    }

                    messages.push(ChatCompletionRequestMessage::User(
                        ChatCompletionRequestUserMessage {
                            content: ChatCompletionRequestUserMessageContent::Text(
                                msgs.system_msg.to_string(),
                            ),
                            name: None,
                        },
                    ));
                    let _ = event_tx.send(AppEvent::StreamNotice {
                        text: msgs.notice.to_string(),
                    });
                    continue;
                }
                // Already retried
                tracing::error!("all tool calls truncated after retry — giving up");
                let _ = event_tx.send(AppEvent::LlmError {
                    error: msgs.error_msg.to_string(),
                });
                let _ = event_tx.send(AppEvent::LlmFinish {
                    usage: Some(total_usage),
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

            // ── Phases 1–4: Partition and execute tool calls ──
            let mut counters = phases::IterationCounters {
                tool_count: current_iteration_tool_count,
                cache_repeats: current_iteration_cache_repeats,
                user_interacted: user_interacted_this_iteration,
                total_usage,
            };

            // Phase 1: Pre-check permissions and partition tool calls
            let partitioned = phases::partition_tool_calls(
                &pending_tool_calls,
                &sorted_indices,
                &mcp_snapshot,
                &permission_engine,
                &tool_context,
                &mut messages,
                &mut counters,
            )
            .await;

            // Phase 2: Execute auto-allowed tools in parallel
            if matches!(
                phases::execute_parallel_tools(
                    &partitioned.auto_allowed,
                    registry,
                    &ctx,
                    &tool_cache,
                    &cancel_token,
                    &event_tx,
                    &mut messages,
                    &mut counters,
                )
                .await,
                phases::PhaseOutcome::Cancelled
            ) {
                return Ok(());
            }

            // Phase 3: Execute permission-required tools sequentially
            if matches!(
                phases::execute_sequential_tools(
                    &partitioned.needs_interaction,
                    registry,
                    &ctx,
                    &tool_cache,
                    &cancel_token,
                    &event_tx,
                    &permission_engine,
                    &agent_spawner,
                    &mut messages,
                    &mut counters,
                )
                .await,
                phases::PhaseOutcome::Cancelled
            ) {
                return Ok(());
            }

            // Phase 4: Execute MCP tool calls sequentially
            if matches!(
                phases::execute_mcp_tools(
                    &partitioned.mcp_calls,
                    &mcp_manager,
                    &cancel_token,
                    &event_tx,
                    &permission_engine,
                    &mut messages,
                    &mut counters,
                )
                .await,
                phases::PhaseOutcome::Cancelled
            ) {
                return Ok(());
            }

            // Sync counters back
            current_iteration_tool_count = counters.tool_count;
            current_iteration_cache_repeats = counters.cache_repeats;
            user_interacted_this_iteration = counters.user_interacted;
            total_usage = counters.total_usage;

            // Reset iteration counter if user granted permission this iteration
            if user_interacted_this_iteration {
                tracing::debug!(
                    iterations = iteration_count,
                    total_iterations = total_iteration_count,
                    "resetting iteration counter (user granted permission)"
                );
                iteration_count = 0;
                warnings_sent = 0;
                consecutive_cached_iterations = 0;
                // Restore tools if they were stripped — permission grant proves supervision
                if tools_stripped {
                    tools_stripped = false;
                    final_chance_taken = false;
                    tools = tool_registry
                        .as_ref()
                        .map(|r| build_tools(r, &mcp_snapshot));
                }
            }

            // Detect "stuck in cache" loops: if ALL tool calls in this iteration
            // returned cache-repeat summaries, the LLM is re-reading content it
            // already has. After MAX_CONSECUTIVE_CACHED such iterations, inject
            // a strong directive to stop looping and respond.
            if current_iteration_tool_count > 0
                && current_iteration_cache_repeats == current_iteration_tool_count
            {
                consecutive_cached_iterations += 1;
                tracing::warn!(
                    consecutive = consecutive_cached_iterations,
                    tools = current_iteration_tool_count,
                    "all tool calls returned cache-repeat summaries"
                );
                if consecutive_cached_iterations >= MAX_CONSECUTIVE_CACHED {
                    let stop_msg = "\n\n[STOP: You are re-reading files you already have. All tool \
                                calls returned cached content. You MUST respond to the user NOW \
                                with the information already in your conversation. Do NOT make \
                                any more tool calls.]";
                    for msg in messages.iter_mut().rev() {
                        if let ChatCompletionRequestMessage::Tool(tool_msg) = msg {
                            if let ChatCompletionRequestToolMessageContent::Text(content) =
                                &mut tool_msg.content
                            {
                                content.push_str(stop_msg);
                            }
                            break;
                        }
                    }
                    let _ = event_tx.send(AppEvent::StreamNotice {
                        text: "⚙ Cache loop detected — forcing response".to_string(),
                    });
                    // Reset so the stop message doesn't fire every subsequent iteration
                    consecutive_cached_iterations = 0;
                }
            } else {
                consecutive_cached_iterations = 0;
            }

            // Escalating warnings: append a nudge to the last tool result message
            // so the LLM sees iteration pressure as contextual feedback.
            if let Some(text) =
                check_iteration_warning(iteration_count, effective_max, &mut warnings_sent)
            {
                // Append warning to the last Tool message in the conversation
                for msg in messages.iter_mut().rev() {
                    if let ChatCompletionRequestMessage::Tool(tool_msg) = msg {
                        if let ChatCompletionRequestToolMessageContent::Text(content) =
                            &mut tool_msg.content
                        {
                            content.push_str(&text);
                        }
                        break;
                    }
                }
                // Also notify the TUI so the user sees why the LLM might wrap up
                let stripped = text.trim().trim_start_matches('[').trim_end_matches(']');
                let display_text = format!("⚙ {stripped}");
                let _ = event_tx.send(AppEvent::StreamNotice { text: display_text });
                tracing::info!(
                    iteration_count,
                    total_iteration_count,
                    "tool loop warning injected"
                );
            }

            // Tool stripping: at CRITICAL threshold, remove tool definitions from the
            // API request so the LLM structurally cannot make tool calls.
            // The CRITICAL warning message is already injected by check_iteration_warning()
            // above — this block just does the structural enforcement.
            if !tools_stripped {
                let critical_at = effective_max * WARN_CRITICAL_PCT / 100;
                if iteration_count >= critical_at {
                    tools_stripped = true;
                    tools = None;
                    let _ = event_tx.send(AppEvent::StreamNotice {
                        text: "⚙ Tool access revoked — forcing response".to_string(),
                    });
                    tracing::warn!(
                        iteration_count,
                        total_iteration_count,
                        critical_at,
                        "tool definitions stripped from API request"
                    );
                }
            }

            // Loop back to send the messages (with tool results) to the LLM again
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::error::{OpenAIError, StreamError};

    use async_openai::types::chat::{
        ChatChoiceStream, ChatCompletionMessageToolCallChunk, ChatCompletionStreamResponseDelta,
        CompletionUsage, CreateChatCompletionStreamResponse, FunctionCallStream, FunctionType,
        Role as OaiRole,
    };
    use std::{collections::VecDeque, sync::Mutex};

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

    impl ChatStreamProvider for MockChatStream {
        fn create_stream(
            &self,
            _request: CreateChatCompletionRequest,
        ) -> Pin<
            Box<dyn Future<Output = Result<ChatCompletionResponseStream, OpenAIError>> + Send + '_>,
        > {
            Box::pin(async move {
                let chunks = self
                    .streams
                    .lock()
                    .expect("lock poisoned")
                    .pop_front()
                    .unwrap_or_default();
                let stream = tokio_stream::iter(chunks);
                Ok(Box::pin(stream) as ChatCompletionResponseStream)
            })
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

    /// Build a text content chunk (assistant text delta).
    #[allow(deprecated)]
    fn text_chunk(content: &str) -> CreateChatCompletionStreamResponse {
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
            tool_cache: Arc::new(std::sync::Mutex::new(ToolResultCache::new(
                std::path::PathBuf::from("/tmp/test"),
            ))),
            cancel_token: CancellationToken::new(),
            context_window: None,
            interjection_rx,
            usage_writer: crate::usage::test_usage_writer(),
            usage_project_id: "test-project".to_string(),
            usage_session_id: "test-session".to_string(),
            usage_model_cost: None,
            is_plan_mode: false,
            agent_spawner: None,
            mcp_manager: None,
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
        let mock = Arc::new(MockChatStream::new(vec![vec![
            Ok(text_delta("Hello")),
            Ok(text_delta(" world")),
            Ok(text_delta("!")),
            Ok(finish_chunk(FinishReason::Stop, 100, 10)),
        ]]));
        let (tx, rx) = mpsc::unbounded_channel();
        let req = mock_stream_request(mock, tx);
        req.run().await.expect("stream should succeed");
        let events = collect_events(rx).await;

        // Should have delta events for text content
        let deltas: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmDelta { .. }))
            .collect();
        assert!(
            deltas.len() >= 3,
            "should have at least 3 delta events, got {}",
            deltas.len()
        );

        // Should have a usage update
        let usage_updates: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmUsageUpdate { .. }))
            .collect();
        assert!(!usage_updates.is_empty(), "should have usage update");

        // Should have a finish event
        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(finishes.len(), 1, "should have exactly 1 finish event");
    }

    #[tokio::test]
    async fn stream_cancel_before_call() {
        let mock = Arc::new(MockChatStream::new(vec![vec![
            Ok(text_delta("should not appear")),
            Ok(finish_chunk(FinishReason::Stop, 10, 5)),
        ]]));
        let (tx, rx) = mpsc::unbounded_channel();
        let req = mock_stream_request(mock, tx);
        req.cancel_token.cancel(); // Cancel before stream starts
        req.run()
            .await
            .expect("should handle cancellation gracefully");
        let events = collect_events(rx).await;

        // Should have LlmFinish but no deltas (cancellation checked before stream opens)
        let deltas: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmDelta { .. }))
            .collect();
        assert!(
            deltas.is_empty(),
            "cancelled stream should produce no deltas"
        );

        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(finishes.len(), 1, "should still get LlmFinish on cancel");
    }

    #[tokio::test]
    async fn stream_tool_call_with_execution() {
        // First call: LLM returns a read tool call
        // Second call: LLM returns text response (after seeing tool result)
        let mock = Arc::new(MockChatStream::new(vec![
            // First stream: tool call
            vec![
                Ok(tool_call_chunk(
                    0,
                    Some("call_1"),
                    Some("read"),
                    Some(r#"{"path":"src/main.rs"}"#),
                )),
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
        req.tool_registry = Some(Arc::new(ToolRegistry::new(std::path::PathBuf::from(
            "/tmp/test",
        ))));
        req.tool_context = Some(ToolContext {
            project_root: std::path::PathBuf::from("/tmp/test"),
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        });
        req.permission_engine = Some(Arc::new(tokio::sync::Mutex::new(
            crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
        )));

        req.run()
            .await
            .expect("stream with tool call should succeed");
        let events = collect_events(rx).await;

        // Should have tool call streaming events
        let tool_calls: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmToolCallStreaming { .. }))
            .collect();
        assert!(
            !tool_calls.is_empty(),
            "should have tool call streaming events"
        );

        // Should have tool result
        let tool_results: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::ToolResult { .. }))
            .collect();
        assert!(!tool_results.is_empty(), "should have tool result events");

        // Should have text delta from second call
        let deltas: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmDelta { .. }))
            .collect();
        assert!(
            !deltas.is_empty(),
            "should have text deltas from second LLM call"
        );

        // Should have exactly 1 finish event (from the final call)
        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(finishes.len(), 1, "should have exactly 1 finish event");
    }

    #[tokio::test]
    async fn stream_truncated_tool_call_filtered() {
        // LLM returns a tool call with truncated JSON (finish_reason: Length).
        // After our fix, Steve compresses and retries without tools. The retry
        // produces a text response instead of an error.
        let mock = Arc::new(MockChatStream::new(vec![
            // First call: tool call with truncated args
            vec![
                Ok(tool_call_chunk(
                    0,
                    Some("call_1"),
                    Some("read"),
                    Some(r#"{"path":"src/main"#),
                )), // truncated!
                Ok(finish_chunk(FinishReason::Length, 50, 20)),
            ],
            // Retry: tool-free response after compression
            vec![
                Ok(text_chunk("Here is my response based on what I found.")),
                Ok(finish_chunk(FinishReason::Stop, 30, 10)),
            ],
        ]));
        let (tx, rx) = mpsc::unbounded_channel();
        let mut req = mock_stream_request(mock, tx);
        req.tool_registry = Some(Arc::new(ToolRegistry::new(std::path::PathBuf::from(
            "/tmp/test",
        ))));
        req.tool_context = Some(ToolContext {
            project_root: std::path::PathBuf::from("/tmp/test"),
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        });
        req.permission_engine = Some(Arc::new(tokio::sync::Mutex::new(
            crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
        )));

        req.run()
            .await
            .expect("stream should handle truncated tool calls");
        let events = collect_events(rx).await;

        // The retry should produce a text response and LlmFinish, not an error
        let notices: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::StreamNotice { .. }))
            .collect();
        assert!(
            !notices.is_empty(),
            "should get a StreamNotice about the retry"
        );
        let deltas: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmDelta { .. }))
            .collect();
        assert!(!deltas.is_empty(), "retry should produce text deltas");
        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(finishes.len(), 1, "should get LlmFinish from the retry");
    }

    #[tokio::test]
    async fn stream_minimal_finish_response() {
        // Verify the stream handles a minimal response (just a finish chunk)
        // correctly. Retry logic for transient errors is covered by
        // is_transient_error unit tests above.
        let mock = Arc::new(MockChatStream::new(vec![vec![Ok(finish_chunk(
            FinishReason::Stop,
            10,
            5,
        ))]]));
        let (tx, rx) = mpsc::unbounded_channel();
        let req = mock_stream_request(mock, tx);
        req.run()
            .await
            .expect("empty stream with finish should work");
        let events = collect_events(rx).await;
        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(finishes.len(), 1, "should get finish event");
    }

    #[tokio::test]
    async fn stream_parallel_read_tools_in_order() {
        // LLM returns 3 read tool calls, then text
        let mock = Arc::new(MockChatStream::new(vec![
            // First stream: 3 tool calls
            vec![
                Ok(tool_call_chunk(
                    0,
                    Some("call_a"),
                    Some("glob"),
                    Some(r#"{"pattern":"*.rs"}"#),
                )),
                Ok(tool_call_chunk(
                    1,
                    Some("call_b"),
                    Some("glob"),
                    Some(r#"{"pattern":"*.txt"}"#),
                )),
                Ok(tool_call_chunk(
                    2,
                    Some("call_c"),
                    Some("glob"),
                    Some(r#"{"pattern":"*.md"}"#),
                )),
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
        req.tool_registry = Some(Arc::new(ToolRegistry::new(std::path::PathBuf::from(
            "/tmp/test",
        ))));
        req.tool_context = Some(ToolContext {
            project_root: std::path::PathBuf::from("/tmp/test"),
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        });
        req.permission_engine = Some(Arc::new(tokio::sync::Mutex::new(
            crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
        )));

        req.run().await.expect("parallel tool calls should succeed");
        let events = collect_events(rx).await;

        // Should have tool results for all 3 calls
        let tool_results: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::ToolResult { .. }))
            .collect();
        assert_eq!(
            tool_results.len(),
            3,
            "should have 3 tool results, got {}",
            tool_results.len()
        );

        // Results should arrive in original call order (a, b, c)
        let tool_call_events: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmToolCall { .. }))
            .collect();
        assert_eq!(
            tool_call_events.len(),
            3,
            "should have 3 LlmToolCall events"
        );
    }

    #[tokio::test]
    async fn stream_cache_hit_skips_execution() {
        // Pre-populate the cache, then run a stream that calls the same tool
        let cache = Arc::new(std::sync::Mutex::new(ToolResultCache::new(
            std::path::PathBuf::from("/tmp/test"),
        )));

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
                Ok(tool_call_chunk(
                    0,
                    Some("call_1"),
                    Some("glob"),
                    Some(r#"{"pattern":"*.rs"}"#),
                )),
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
            tool_registry: Some(Arc::new(ToolRegistry::new(std::path::PathBuf::from(
                "/tmp/test",
            )))),
            tool_context: Some(ToolContext {
                project_root: std::path::PathBuf::from("/tmp/test"),
                storage_dir: None,
                task_store: None,
                lsp_manager: None,
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
            is_plan_mode: false,
            agent_spawner: None,
            mcp_manager: None,
        };

        req.run().await.expect("cache hit stream should succeed");
        let events = collect_events(rx).await;

        // Should still have a tool result (from cache)
        let tool_results: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::ToolResult { .. }))
            .collect();
        assert!(
            !tool_results.is_empty(),
            "should have tool result even from cache"
        );
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
        req.tool_registry = Some(Arc::new(ToolRegistry::new(std::path::PathBuf::from(
            "/tmp/test",
        ))));
        req.tool_context = Some(ToolContext {
            project_root: std::path::PathBuf::from("/tmp/test"),
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        });
        req.permission_engine = Some(Arc::new(tokio::sync::Mutex::new(
            crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
        )));

        req.run().await.expect("should terminate gracefully");
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
        assert_eq!(
            finishes.len(),
            1,
            "should get LlmFinish to persist token usage"
        );
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

        req.run()
            .await
            .expect("should recover from mid-stream error");
        let events = collect_events(rx).await;

        // Should have a retry notification (LlmRetry, not LlmError)
        let retries: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmRetry { .. }))
            .collect();
        assert_eq!(
            retries.len(),
            1,
            "should have 1 LlmRetry for the retry notification"
        );
        if let AppEvent::LlmRetry {
            attempt,
            max_attempts,
            ..
        } = retries[0]
        {
            assert_eq!(*attempt, 1);
            assert_eq!(*max_attempts, 2);
        }

        // Should complete successfully with LlmFinish
        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(
            finishes.len(),
            1,
            "should get LlmFinish after successful retry"
        );

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
        let error_streams: Vec<Vec<Result<CreateChatCompletionStreamResponse, OpenAIError>>> = (0
            ..5)
            .map(|_| {
                vec![Err(OpenAIError::StreamError(Box::new(
                    StreamError::EventStream("error decoding response body".to_string()),
                )))]
            })
            .collect();
        let mock = Arc::new(MockChatStream::new(error_streams));
        let (tx, rx) = mpsc::unbounded_channel();
        let req = mock_stream_request(mock, tx);

        req.run()
            .await
            .expect("should handle exhausted retries gracefully");
        let events = collect_events(rx).await;

        // Should have 2 LlmRetry events + 1 final LlmError
        let retries: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmRetry { .. }))
            .collect();
        assert_eq!(
            retries.len(),
            2,
            "should have 2 LlmRetry events, got {}",
            retries.len()
        );

        let errors: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmError { .. }))
            .collect();
        assert_eq!(
            errors.len(),
            1,
            "should have 1 final LlmError, got {}",
            errors.len()
        );

        // Should get LlmFinish to persist usage
        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(
            finishes.len(),
            1,
            "should get LlmFinish even after exhausted retries"
        );
    }

    #[tokio::test]
    async fn interjection_injected_as_user_message() {
        // Stream: tool call -> (interjection injected) -> text response
        let mock = Arc::new(MockChatStream::new(vec![
            vec![
                Ok(tool_call_chunk(
                    0,
                    Some("call_1"),
                    Some("glob"),
                    Some(r#"{"pattern":"*.rs"}"#),
                )),
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
        interjection_tx
            .send("focus on tests instead".to_string())
            .unwrap();

        let mut req = mock_stream_request(mock, tx);
        req.interjection_rx = interjection_rx;
        req.tool_registry = Some(Arc::new(ToolRegistry::new(std::path::PathBuf::from(
            "/tmp/test",
        ))));
        req.tool_context = Some(ToolContext {
            project_root: std::path::PathBuf::from("/tmp/test"),
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        });
        req.permission_engine = Some(Arc::new(tokio::sync::Mutex::new(
            crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
        )));

        req.run().await.expect("interjection stream should succeed");
        let events = collect_events(rx).await;

        // Should complete successfully
        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(finishes.len(), 1, "should get LlmFinish");

        // Should have text deltas from the second call
        let deltas: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmDelta { .. }))
            .collect();
        assert!(!deltas.is_empty(), "should have text deltas");
    }

    #[tokio::test]
    async fn multiple_interjections_all_consumed() {
        let mock = Arc::new(MockChatStream::new(vec![
            vec![
                Ok(tool_call_chunk(
                    0,
                    Some("call_1"),
                    Some("glob"),
                    Some(r#"{"pattern":"*.rs"}"#),
                )),
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
        req.tool_registry = Some(Arc::new(ToolRegistry::new(std::path::PathBuf::from(
            "/tmp/test",
        ))));
        req.tool_context = Some(ToolContext {
            project_root: std::path::PathBuf::from("/tmp/test"),
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        });
        req.permission_engine = Some(Arc::new(tokio::sync::Mutex::new(
            crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
        )));

        req.run()
            .await
            .expect("multi-interjection stream should succeed");
        let events = collect_events(rx).await;

        // Should complete with LlmFinish (not error)
        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(finishes.len(), 1);
        let errors: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmError { .. }))
            .collect();
        assert!(errors.is_empty(), "should have no errors");
    }

    #[tokio::test]
    async fn empty_interjection_channel_is_noop() {
        // Basic text response with no interjections — existing behavior unchanged
        let mock = Arc::new(MockChatStream::new(vec![vec![
            Ok(text_delta("Hello")),
            Ok(finish_chunk(FinishReason::Stop, 50, 10)),
        ]]));
        let (tx, rx) = mpsc::unbounded_channel();
        let req = mock_stream_request(mock, tx);

        req.run()
            .await
            .expect("empty interjection channel should be fine");
        let events = collect_events(rx).await;

        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(finishes.len(), 1);
    }

    /// A stream provider that injects a user interjection into the channel
    /// after the first chunk of the first API call, simulating a user typing
    /// while the LLM is streaming a text-only response. Captures requests
    /// sent to each `create_stream` call for assertion.
    struct InterjectionInjectorMock {
        inner: MockChatStream,
        inject_tx: Mutex<Option<mpsc::UnboundedSender<String>>>,
        message: String,
        captured_requests: Mutex<Vec<Vec<ChatCompletionRequestMessage>>>,
    }

    impl ChatStreamProvider for InterjectionInjectorMock {
        fn create_stream(
            &self,
            request: CreateChatCompletionRequest,
        ) -> Pin<
            Box<dyn Future<Output = Result<ChatCompletionResponseStream, OpenAIError>> + Send + '_>,
        > {
            Box::pin(async move {
                // Capture the messages for later assertion
                self.captured_requests
                    .lock()
                    .expect("lock poisoned")
                    .push(request.messages);

                let chunks = self
                    .inner
                    .streams
                    .lock()
                    .expect("lock poisoned")
                    .pop_front()
                    .unwrap_or_default();

                // If this is the first call, build a stream that injects the
                // interjection after the first chunk
                let mut inject_tx_guard = self.inject_tx.lock().expect("lock poisoned");
                if let Some(tx) = inject_tx_guard.take() {
                    let msg = self.message.clone();
                    // Feed chunks through a channel-based stream so we can
                    // inject the interjection mid-stream
                    let (chunk_tx, chunk_rx) = tokio::sync::mpsc::unbounded_channel();
                    tokio::spawn(async move {
                        let mut first = true;
                        for chunk in chunks {
                            let _ = chunk_tx.send(chunk);
                            if first {
                                // After first chunk, inject the interjection
                                let _ = tx.send(msg.clone());
                                first = false;
                            }
                            // Yield so the receiver can process
                            tokio::task::yield_now().await;
                        }
                    });
                    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(chunk_rx);
                    Ok(Box::pin(stream) as ChatCompletionResponseStream)
                } else {
                    let stream = tokio_stream::iter(chunks);
                    Ok(Box::pin(stream) as ChatCompletionResponseStream)
                }
            })
        }
    }

    #[tokio::test]
    async fn interjection_during_text_response_triggers_followup() {
        // When the LLM produces a text-only response (no tool calls) but the
        // user sends an interjection during streaming, the stream should
        // continue with another API call so the LLM can respond to it.
        let (interjection_tx, interjection_rx) = mpsc::unbounded_channel();

        let mock = Arc::new(InterjectionInjectorMock {
            inner: MockChatStream::new(vec![
                // First call: text-only response
                vec![
                    Ok(text_delta("Working on it...")),
                    Ok(finish_chunk(FinishReason::Stop, 50, 10)),
                ],
                // Second call: LLM responds to the interjection
                vec![
                    Ok(text_delta("Got your message!")),
                    Ok(finish_chunk(FinishReason::Stop, 100, 15)),
                ],
            ]),
            inject_tx: Mutex::new(Some(interjection_tx)),
            message: "actually focus on tests".to_string(),
            captured_requests: Mutex::new(Vec::new()),
        });

        let (tx, rx) = mpsc::unbounded_channel();
        let mut req = mock_stream_request(mock.clone(), tx);
        req.interjection_rx = interjection_rx;

        req.run()
            .await
            .expect("interjection during text response should trigger followup");
        let events = collect_events(rx).await;

        // Should have text deltas from BOTH API calls
        let deltas: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmDelta { .. }))
            .collect();
        assert!(
            deltas.len() >= 2,
            "should have deltas from both calls: {:?}",
            deltas
        );

        // Should complete successfully
        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(finishes.len(), 1);

        // Verify the second API call includes the assistant's response followed
        // by the user's interjection in the correct order
        let requests = mock.captured_requests.lock().expect("lock poisoned");
        assert_eq!(requests.len(), 2, "should have made 2 API calls");

        let second_call_msgs = &requests[1];
        // Last two messages should be: Assistant("Working on it..."), User("actually focus on tests")
        let len = second_call_msgs.len();
        assert!(len >= 2, "second call should have at least 2 messages");

        let penultimate = &second_call_msgs[len - 2];
        assert!(
            matches!(
                penultimate,
                ChatCompletionRequestMessage::Assistant(a)
                    if matches!(&a.content, Some(ChatCompletionRequestAssistantMessageContent::Text(t)) if t == "Working on it...")
            ),
            "penultimate message should be assistant with first response text, got: {:?}",
            penultimate
        );

        let last = &second_call_msgs[len - 1];
        assert!(
            matches!(
                last,
                ChatCompletionRequestMessage::User(u)
                    if matches!(&u.content, ChatCompletionRequestUserMessageContent::Text(t) if t == "actually focus on tests")
            ),
            "last message should be user interjection, got: {:?}",
            last
        );
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
        req.tool_registry = Some(Arc::new(ToolRegistry::new(std::path::PathBuf::from(
            "/tmp/test",
        ))));
        req.tool_context = Some(ToolContext {
            project_root: std::path::PathBuf::from("/tmp/test"),
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        });
        req.permission_engine = Some(Arc::new(tokio::sync::Mutex::new(
            crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
        )));

        req.run()
            .await
            .expect("interjection with tool calls should succeed");
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

    #[tokio::test]
    async fn stream_cache_loop_emits_stop_notice() {
        // Pre-populate the cache and exhaust the repeat threshold so all
        // subsequent gets return the cache-repeat summary.
        let cache = Arc::new(std::sync::Mutex::new(ToolResultCache::new(
            std::path::PathBuf::from("/tmp/test"),
        )));

        let glob_args = serde_json::json!({"pattern": "*.rs"});
        {
            let mut c = cache.lock().unwrap();
            let output = crate::tool::ToolOutput {
                title: "glob '*.rs'".to_string(),
                output: "src/main.rs".to_string(),
                is_error: false,
            };
            c.put(ToolName::Glob, &glob_args, &output);
            // Exhaust threshold (2 hits) so next gets return cache-repeat summary
            for _ in 0..2 {
                c.get(ToolName::Glob, &glob_args);
            }
        }

        // 3 iterations of the same cached tool call, then final text.
        // MAX_CONSECUTIVE_CACHED = 2, so the stop notice should fire on
        // the 2nd all-cached iteration.
        let mut streams = Vec::new();
        for i in 0..3 {
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
            Ok(text_delta("Here is my answer.")),
            Ok(finish_chunk(FinishReason::Stop, 100, 10)),
        ]);

        let mock = Arc::new(MockChatStream::new(streams));
        let (tx, rx) = mpsc::unbounded_channel();
        let mut req = mock_stream_request(mock, tx);
        req.tool_cache = cache;
        req.tool_registry = Some(Arc::new(ToolRegistry::new(std::path::PathBuf::from(
            "/tmp/test",
        ))));
        req.tool_context = Some(ToolContext {
            project_root: std::path::PathBuf::from("/tmp/test"),
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        });
        req.permission_engine = Some(Arc::new(tokio::sync::Mutex::new(
            crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
        )));

        req.run().await.expect("cache loop stream should succeed");
        let events = collect_events(rx).await;

        // Should have a StreamNotice about cache loop detection
        let notices: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::StreamNotice { text } if text.contains("Cache loop")))
            .collect();
        assert!(
            !notices.is_empty(),
            "should emit StreamNotice about cache loop, events: {:?}",
            events
                .iter()
                .filter(|e| matches!(e, AppEvent::StreamNotice { .. }))
                .collect::<Vec<_>>()
        );

        // Should still complete normally with LlmFinish
        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(finishes.len(), 1);
    }

    // -- Step 5: Stream tool-loop integration tests --

    #[tokio::test]
    async fn stream_multi_tool_chain_with_filesystem() {
        // Set up a real temp dir with files, then have the mock LLM call glob then read
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "fn main() { println!(\"hello\"); }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn greet() -> &'static str { \"hi\" }\n",
        )
        .unwrap();

        let mock = Arc::new(MockChatStream::new(vec![
            // First stream: glob to find .rs files
            vec![
                Ok(tool_call_chunk(
                    0,
                    Some("call_glob"),
                    Some("glob"),
                    Some(&format!(
                        r#"{{"pattern":"**/*.rs","path":"{}"}}"#,
                        root.to_string_lossy()
                    )),
                )),
                Ok(finish_chunk(FinishReason::ToolCalls, 50, 20)),
            ],
            // Second stream: read main.rs after seeing glob results
            vec![
                Ok(tool_call_chunk(
                    0,
                    Some("call_read"),
                    Some("read"),
                    Some(&format!(
                        r#"{{"path":"{}"}}"#,
                        root.join("src/main.rs").to_string_lossy()
                    )),
                )),
                Ok(finish_chunk(FinishReason::ToolCalls, 80, 30)),
            ],
            // Third stream: final text response
            vec![
                Ok(text_delta("Found and read the files.")),
                Ok(finish_chunk(FinishReason::Stop, 120, 15)),
            ],
        ]));

        let (tx, rx) = mpsc::unbounded_channel();
        let mut req = mock_stream_request(mock, tx);
        req.tool_registry = Some(Arc::new(ToolRegistry::new(root.clone())));
        req.tool_context = Some(ToolContext {
            project_root: root,
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        });
        req.permission_engine = Some(Arc::new(tokio::sync::Mutex::new(
            crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
        )));

        req.run().await.expect("multi-tool chain should succeed");
        let events = collect_events(rx).await;

        // Should have tool results for both glob and read
        let tool_results: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::ToolResult { .. }))
            .collect();
        assert_eq!(
            tool_results.len(),
            2,
            "should have 2 tool results (glob + read)"
        );

        // Verify glob result contains .rs files
        let AppEvent::ToolResult { output, .. } = tool_results[0] else {
            panic!("expected ToolResult, got {:?}", tool_results[0]);
        };
        assert!(
            output.output.contains("main.rs"),
            "glob result should contain main.rs, got: {}",
            output.output
        );

        // Verify read result contains file content
        let AppEvent::ToolResult { output, .. } = tool_results[1] else {
            panic!("expected ToolResult, got {:?}", tool_results[1]);
        };
        assert!(
            output.output.contains("fn main()"),
            "read result should contain fn main(), got: {}",
            output.output
        );

        // Should finish normally
        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(finishes.len(), 1);
    }

    #[tokio::test]
    async fn stream_write_tool_emits_permission_request() {
        // Mock LLM tries to call edit tool in standard mode — should trigger permission request.
        // We can't use collect_events() here because PermissionRequest carries a oneshot::Sender
        // that blocks run_stream until replied to. We must drain the channel from outside the
        // stream task, intercept the PermissionRequest, send AllowOnce, then let the stream
        // continue. A 5-second timeout guards against hangs.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let file_path = root.join("src/main.rs").to_string_lossy().to_string();

        let mock = Arc::new(MockChatStream::new(vec![
            vec![
                Ok(tool_call_chunk(
                    0,
                    Some("call_edit"),
                    Some("edit"),
                    Some(&format!(
                        r#"{{"file_path":"{}","old_string":"fn main() {{}}","new_string":"fn main() {{ println!(\"hi\"); }}"}}"#,
                        file_path
                    )),
                )),
                Ok(finish_chunk(FinishReason::ToolCalls, 50, 20)),
            ],
            // After permission granted, LLM sees tool result and responds
            vec![
                Ok(text_delta("Edit complete.")),
                Ok(finish_chunk(FinishReason::Stop, 100, 10)),
            ],
        ]));

        let (tx, rx) = mpsc::unbounded_channel();
        let mut req = mock_stream_request(mock, tx);
        req.tool_registry = Some(Arc::new(ToolRegistry::new(root.clone())));
        req.tool_context = Some(ToolContext {
            project_root: root,
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        });
        // Standard mode: writes require Ask
        req.permission_engine = Some(Arc::new(tokio::sync::Mutex::new(
            crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
        )));

        // Spawn the stream, then drain events auto-granting any permission requests
        let mut perm_rx = rx;
        let stream_handle = tokio::spawn(async move { req.run().await });

        let mut events = Vec::new();
        let mut saw_permission = false;
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), perm_rx.recv()).await {
                Ok(Some(event)) => {
                    // Must move out of event to access response_tx (oneshot::Sender)
                    if let AppEvent::PermissionRequest(pr) = event {
                        saw_permission = true;
                        let _ = pr
                            .response_tx
                            .send(crate::permission::types::PermissionReply::AllowOnce);
                        continue;
                    }
                    let is_finish = matches!(event, AppEvent::LlmFinish { .. });
                    events.push(event);
                    if is_finish {
                        break;
                    }
                }
                Ok(None) => break, // Channel closed — stream finished
                Err(_) => panic!(
                    "timed out waiting for stream events — stream likely hung on unanswered PermissionRequest"
                ),
            }
        }
        stream_handle.await.unwrap().expect("stream should succeed");

        assert!(
            saw_permission,
            "edit tool should trigger PermissionRequest in standard mode"
        );

        // Should have tool result (edit was allowed)
        let tool_results: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::ToolResult { .. }))
            .collect();
        assert!(
            !tool_results.is_empty(),
            "should have tool result after permission grant"
        );
    }

    #[tokio::test]
    async fn stream_plan_mode_denies_write_tools() {
        // In plan mode, write tools should be denied entirely
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let file_path = root.join("src/main.rs").to_string_lossy().to_string();

        let mock = Arc::new(MockChatStream::new(vec![
            // LLM tries to edit in plan mode
            vec![
                Ok(tool_call_chunk(
                    0,
                    Some("call_edit"),
                    Some("edit"),
                    Some(&format!(
                        r#"{{"file_path":"{}","old_string":"fn main() {{}}","new_string":"fn main() {{ changed(); }}"}}"#,
                        file_path
                    )),
                )),
                Ok(finish_chunk(FinishReason::ToolCalls, 50, 20)),
            ],
            // LLM responds after seeing denied tool result
            vec![
                Ok(text_delta("Edit denied in plan mode.")),
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
            tool_registry: Some(Arc::new(ToolRegistry::new(root.clone()))),
            tool_context: Some(ToolContext {
                project_root: root.clone(),
                storage_dir: None,
                task_store: None,
                lsp_manager: None,
            }),
            permission_engine: Some(Arc::new(tokio::sync::Mutex::new(
                crate::permission::PermissionEngine::new(crate::permission::plan_mode_rules()),
            ))),
            tool_cache: Arc::new(std::sync::Mutex::new(ToolResultCache::new(root))),
            cancel_token: CancellationToken::new(),
            context_window: None,
            interjection_rx,
            usage_writer: crate::usage::test_usage_writer(),
            usage_project_id: "test-project".to_string(),
            usage_session_id: "test-session".to_string(),
            usage_model_cost: None,
            is_plan_mode: true,
            agent_spawner: None,
            mcp_manager: None,
        };

        req.run().await.expect("plan mode stream should succeed");
        let events = collect_events(rx).await;

        // Should have a tool result that indicates denial
        let tool_results: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::ToolResult { .. }))
            .collect();
        assert!(
            !tool_results.is_empty(),
            "should have tool result for denied edit"
        );

        let AppEvent::ToolResult { output, .. } = tool_results[0] else {
            panic!("expected ToolResult, got {:?}", tool_results[0]);
        };
        assert!(output.is_error, "denied tool result should be an error");
        assert!(
            output.output.to_lowercase().contains("denied")
                || output.output.to_lowercase().contains("not allowed"),
            "tool result should mention denial, got: {}",
            output.output
        );

        // File should be unchanged
        let content = std::fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
        assert_eq!(
            content, "fn main() {}\n",
            "file should be unchanged in plan mode"
        );
    }

    #[tokio::test]
    async fn stream_cache_invalidation_after_write() {
        // Pre-populate cache, then run a stream where trust-mode allows a write,
        // and verify the cache entry is invalidated.
        use crate::permission::types::{PermissionActionSerde, ToolMatcher};

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "fn main() { println!(\"hello\"); }\n",
        )
        .unwrap();
        let file_path = root.join("src/main.rs").to_string_lossy().to_string();

        // Pre-populate cache with a read result
        let cache = Arc::new(std::sync::Mutex::new(ToolResultCache::new(root.clone())));
        {
            let mut c = cache.lock().unwrap();
            let args = serde_json::json!({"path": &file_path});
            let output = crate::tool::ToolOutput {
                title: "read".to_string(),
                output: "fn main() { println!(\"hello\"); }".to_string(),
                is_error: false,
            };
            c.put(ToolName::Read, &args, &output);
            // Verify it's cached
            assert!(c.get(ToolName::Read, &args).is_some());
        }

        // Trust mode: all tools allowed
        let trust_rules = vec![crate::permission::types::PermissionRule {
            tool: ToolMatcher::All,
            pattern: "*".into(),
            action: PermissionActionSerde::Allow,
        }];

        let mock = Arc::new(MockChatStream::new(vec![
            // Edit the file
            vec![
                Ok(tool_call_chunk(
                    0,
                    Some("call_edit"),
                    Some("edit"),
                    Some(&format!(
                        r#"{{"file_path":"{}","old_string":"println!(\"hello\")","new_string":"println!(\"world\")"}}"#,
                        file_path
                    )),
                )),
                Ok(finish_chunk(FinishReason::ToolCalls, 50, 20)),
            ],
            // Final response
            vec![
                Ok(text_delta("Done.")),
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
            tool_registry: Some(Arc::new(ToolRegistry::new(root.clone()))),
            tool_context: Some(ToolContext {
                project_root: root.clone(),
                storage_dir: None,
                task_store: None,
                lsp_manager: None,
            }),
            permission_engine: Some(Arc::new(tokio::sync::Mutex::new(
                crate::permission::PermissionEngine::new(trust_rules),
            ))),
            tool_cache: cache.clone(),
            cancel_token: CancellationToken::new(),
            context_window: None,
            interjection_rx,
            usage_writer: crate::usage::test_usage_writer(),
            usage_project_id: "test-project".to_string(),
            usage_session_id: "test-session".to_string(),
            usage_model_cost: None,
            is_plan_mode: false,
            agent_spawner: None,
            mcp_manager: None,
        };

        req.run()
            .await
            .expect("trust-mode edit stream should succeed");
        let events = collect_events(rx).await;

        // Edit should have succeeded
        let tool_results: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::ToolResult { .. }))
            .collect();
        assert!(!tool_results.is_empty(), "should have tool result for edit");
        let AppEvent::ToolResult { output, .. } = tool_results[0] else {
            panic!("expected ToolResult, got {:?}", tool_results[0]);
        };
        assert!(
            !output.is_error,
            "edit should succeed in trust mode: {}",
            output.output
        );

        // Cache should be invalidated for the edited file
        {
            let mut c = cache.lock().unwrap();
            let args = serde_json::json!({"path": &file_path});
            assert!(
                c.get(ToolName::Read, &args).is_none(),
                "cache entry for edited file should be invalidated"
            );
        }

        // File should actually be changed
        let content = std::fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
        assert!(
            content.contains("println!(\"world\")"),
            "file should be updated"
        );
    }

    #[tokio::test]
    async fn parallel_agents_all_execute_and_accumulate_usage() {
        // LLM returns 3 agent (explore) tool calls in a single response.
        // Each sub-agent gets its own mock response (simple text).
        // The final parent response is text.
        //
        // Mock stream queue: [parent call 1, sub-agent 1, sub-agent 2, sub-agent 3, parent call 2]
        // The sub-agents are spawned concurrently, so consumption order from the mock
        // is non-deterministic — but each pops from the same VecDeque.
        let mock = Arc::new(MockChatStream::new(vec![
            // Parent call 1: return 3 agent tool calls
            vec![
                Ok(tool_call_chunk(
                    0,
                    Some("call_agent_1"),
                    Some("agent"),
                    Some(r#"{"agent_type":"explore","task":"find permissions"}"#),
                )),
                Ok(tool_call_chunk(
                    1,
                    Some("call_agent_2"),
                    Some("agent"),
                    Some(r#"{"agent_type":"explore","task":"find caching"}"#),
                )),
                Ok(tool_call_chunk(
                    2,
                    Some("call_agent_3"),
                    Some("agent"),
                    Some(r#"{"agent_type":"explore","task":"find sessions"}"#),
                )),
                Ok(finish_chunk(FinishReason::ToolCalls, 200, 50)),
            ],
            // Sub-agent 1 response
            vec![
                Ok(text_delta("Agent 1 result.")),
                Ok(finish_chunk(FinishReason::Stop, 100, 20)),
            ],
            // Sub-agent 2 response
            vec![
                Ok(text_delta("Agent 2 result.")),
                Ok(finish_chunk(FinishReason::Stop, 100, 25)),
            ],
            // Sub-agent 3 response
            vec![
                Ok(text_delta("Agent 3 result.")),
                Ok(finish_chunk(FinishReason::Stop, 100, 30)),
            ],
            // Parent call 2: final text response
            vec![
                Ok(text_delta("All agents done.")),
                Ok(finish_chunk(FinishReason::Stop, 300, 15)),
            ],
        ]));

        let root = std::path::PathBuf::from("/tmp/test-parallel-agents");
        let cancel_token = CancellationToken::new();

        let (tx, rx) = mpsc::unbounded_channel();
        let (_interjection_tx, interjection_rx) = mpsc::unbounded_channel();

        let spawner = AgentSpawner {
            stream_provider: mock.clone(),
            primary_model: "test-model".to_string(),
            small_model: None,
            project_root: root.clone(),
            tool_context: ToolContext {
                project_root: root.clone(),
                storage_dir: None,
                task_store: None,
                lsp_manager: None,
            },
            permission_engine: None,
            context_window: None,
            usage_writer: crate::usage::test_usage_writer(),
            usage_project_id: "test-project".to_string(),
            usage_session_id: "test-session".to_string(),
            cancel_token: cancel_token.clone(),
            mcp_manager: None,
        };

        let req = StreamRequest {
            stream_provider: mock,
            model: "test-model".to_string(),
            system_prompt: None,
            history: vec![],
            user_message: "test parallel agents".to_string(),
            event_tx: tx,
            tool_registry: Some(Arc::new(ToolRegistry::new(root.clone()))),
            tool_context: Some(ToolContext {
                project_root: root.clone(),
                storage_dir: None,
                task_store: None,
                lsp_manager: None,
            }),
            permission_engine: Some(Arc::new(tokio::sync::Mutex::new(
                crate::permission::PermissionEngine::new(crate::permission::build_mode_rules()),
            ))),
            tool_cache: Arc::new(std::sync::Mutex::new(ToolResultCache::new(root))),
            cancel_token,
            context_window: None,
            interjection_rx,
            usage_writer: crate::usage::test_usage_writer(),
            usage_project_id: "test-project".to_string(),
            usage_session_id: "test-session".to_string(),
            usage_model_cost: None,
            is_plan_mode: false,
            agent_spawner: Some(spawner),
            mcp_manager: None,
        };

        req.run()
            .await
            .expect("parallel agent stream should succeed");
        let events = collect_events(rx).await;

        // Should complete with LlmFinish
        let finishes: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmFinish { .. }))
            .collect();
        assert_eq!(finishes.len(), 1, "should get exactly one LlmFinish");

        // Should have tool results for all 3 agents
        let tool_results: Vec<&AppEvent> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    AppEvent::ToolResult {
                        tool_name: ToolName::Agent,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(tool_results.len(), 3, "should have 3 agent tool results");

        // Verify none errored
        for result in &tool_results {
            let AppEvent::ToolResult { output, .. } = result else {
                unreachable!();
            };
            assert!(
                !output.is_error,
                "agent should not error: {}",
                output.output
            );
        }

        // Should have text deltas from the final parent response
        let deltas: Vec<&AppEvent> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::LlmDelta { .. }))
            .collect();
        assert!(
            !deltas.is_empty(),
            "should have text deltas from final response"
        );

        // Verify usage was accumulated from sub-agents
        let AppEvent::LlmFinish { usage: Some(usage) } = finishes[0] else {
            panic!("LlmFinish should have usage");
        };
        // Parent: 200+300=500 prompt, 50+15=65 completion
        // Sub-agents: 3×100=300 prompt, 20+25+30=75 completion
        // Total: 800 prompt, 140 completion
        assert!(
            usage.prompt_tokens >= 500,
            "usage should include sub-agent prompt tokens, got {}",
            usage.prompt_tokens
        );
        assert!(
            usage.completion_tokens >= 65,
            "usage should include sub-agent completion tokens, got {}",
            usage.completion_tokens
        );
    }
}
