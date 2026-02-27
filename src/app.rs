use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use serde_json::Value;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use async_openai::types::chat::{
    ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
    ChatCompletionRequestMessage, ChatCompletionRequestUserMessage,
    ChatCompletionRequestUserMessageContent,
};

use crate::config::types::Config;
use crate::event::AppEvent;
use crate::permission::PermissionEngine;
use crate::permission::types::PermissionReply;
use crate::project::ProjectInfo;
use crate::provider::ProviderRegistry;
use crate::session::message::{Message, Role};
use crate::session::types::SessionInfo;
use crate::session::SessionManager;
use crate::storage::Storage;
use crate::stream::{self, StreamRequest};
use crate::context::cache::ToolResultCache;
use crate::tool::{ToolContext, ToolName, ToolRegistry};
use crate::ui;
use crate::ui::input::InputState;
use crate::ui::message_area::MessageAreaState;
use crate::ui::message_block::MessageBlock;
use crate::ui::sidebar::SidebarState;
use crate::ui::status_line::{Activity, StatusLineState};
use crate::ui::theme::Theme;

/// System prompt for conversation compaction/summarization.
const COMPACT_SYSTEM_PROMPT: &str = "Provide a detailed but concise summary of the conversation below. \
Focus on information that would be helpful for continuing the conversation, including: \
what was done, what is currently being worked on, which files are being modified, \
decisions that were made, and what the next steps are. \
Preserve specific technical details, file paths, and code patterns.";

/// A permission prompt waiting for user input.
struct PendingPermission {
    tool_name: crate::tool::ToolName,
    #[allow(dead_code)]
    summary: String,
    response_tx: tokio::sync::oneshot::Sender<PermissionReply>,
}

/// Extract a compact argument summary for display in tool call lines.
fn extract_args_summary(tool_name: ToolName, args: &Value) -> String {
    match tool_name {
        ToolName::Read | ToolName::List => args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Grep | ToolName::Glob => args
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Edit | ToolName::Write | ToolName::Patch => args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Bash => {
            let cmd = args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if cmd.chars().count() > 40 {
                let truncated: String = cmd.chars().take(37).collect();
                format!("{truncated}...")
            } else {
                cmd.to_string()
            }
        }
        ToolName::Question => args
            .get("text")
            .and_then(|v| v.as_str())
            .map(|s| {
                if s.chars().count() > 30 {
                    let truncated: String = s.chars().take(27).collect();
                    format!("{truncated}...")
                } else {
                    s.to_string()
                }
            })
            .unwrap_or_default(),
        ToolName::Todo => String::new(),
        ToolName::Webfetch => args
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    }
}

pub struct App {
    // Core state
    pub project: ProjectInfo,
    pub config: Config,
    pub storage: Storage,
    pub agents_md: Option<String>,
    pub provider_registry: Option<ProviderRegistry>,

    /// Currently selected model ref ("provider/model").
    pub current_model: Option<String>,

    /// Current active session.
    pub current_session: Option<SessionInfo>,

    /// Tool registry (shared with stream tasks via Arc).
    tool_registry: Arc<ToolRegistry>,

    /// Permission engine (shared with stream tasks via Arc<Mutex>).
    permission_engine: Arc<tokio::sync::Mutex<PermissionEngine>>,

    /// Tool result cache (shared across stream tasks within a session).
    tool_cache: Arc<std::sync::Mutex<ToolResultCache>>,

    // UI state
    pub input: InputState,
    pub messages: Vec<MessageBlock>,
    pub message_area_state: MessageAreaState,
    pub sidebar_state: SidebarState,
    pub theme: Theme,
    pub status_line_state: StatusLineState,
    pub is_loading: bool,

    /// Stored messages for the current session (for building conversation history).
    stored_messages: Vec<Message>,

    /// Whether we are currently accumulating an assistant streaming response.
    streaming_active: bool,

    /// The in-progress assistant message being built during streaming.
    /// Saved to storage when streaming finishes.
    streaming_message: Option<Message>,

    /// Count of user+assistant exchanges in the current session (for auto-title).
    exchange_count: usize,

    /// Active permission prompt awaiting user response.
    pending_permission: Option<PendingPermission>,

    /// Cancellation token for the current stream task.
    stream_cancel: Option<CancellationToken>,

    /// Whether auto-compact has failed in this session (suppresses retries).
    auto_compact_failed: bool,

    // Runtime
    event_tx: mpsc::UnboundedSender<AppEvent>,
    event_rx: mpsc::UnboundedReceiver<AppEvent>,
    should_quit: bool,
}

impl App {
    pub fn new(
        project: ProjectInfo,
        config: Config,
        storage: Storage,
        agents_md: Option<String>,
        provider_registry: Option<ProviderRegistry>,
        provider_error: Option<String>,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // Determine the default model from config
        let current_model = config.model.clone();

        // Build tool registry
        let tool_registry = Arc::new(ToolRegistry::new(project.root.clone()));

        // Build permission engine with Build mode rules (default)
        let permission_engine = Arc::new(tokio::sync::Mutex::new(
            PermissionEngine::new(crate::permission::build_mode_rules()),
        ));

        // Build tool result cache (session-scoped, shared across stream tasks)
        let tool_cache = Arc::new(std::sync::Mutex::new(
            ToolResultCache::new(project.root.clone()),
        ));

        // Build startup messages
        let mut messages = Vec::new();
        if config.providers.is_empty() {
            messages.push(MessageBlock::Assistant {
                thinking: None,
                text: "No providers configured. Create a steve.json or steve.jsonc config file to get started.".to_string(),
                tool_groups: vec![],
            });
        } else if let Some(err) = provider_error {
            messages.push(MessageBlock::Assistant {
                thinking: None,
                text: format!("Provider setup failed: {err}"),
                tool_groups: vec![],
            });
        }

        Self {
            project,
            config,
            storage,
            agents_md,
            provider_registry,
            current_model,
            current_session: None,
            tool_registry,
            permission_engine,
            tool_cache,
            stored_messages: Vec::new(),
            input: InputState::default(),
            messages,
            message_area_state: MessageAreaState::default(),
            sidebar_state: SidebarState::default(),
            theme: Theme::default(),
            status_line_state: StatusLineState::default(),
            is_loading: false,
            streaming_active: false,
            streaming_message: None,
            exchange_count: 0,
            pending_permission: None,
            stream_cancel: None,
            auto_compact_failed: false,
            event_tx,
            event_rx,
            should_quit: false,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        // Try to resume the last session
        self.resume_or_new_session();

        let mut terminal = ui::setup_terminal()?;
        let mut crossterm_events = crossterm::event::EventStream::new();
        let mut tick_interval = tokio::time::interval(Duration::from_millis(100));

        // Initial render
        terminal.draw(|frame| ui::render(frame, self))?;

        loop {
            tokio::select! {
                maybe_event = crossterm_events.next() => {
                    if let Some(Ok(event)) = maybe_event {
                        self.handle_event(AppEvent::Input(event)).await?;
                    }
                }
                maybe_event = self.event_rx.recv() => {
                    if let Some(event) = maybe_event {
                        self.handle_event(event).await?;
                    }
                }
                _ = tick_interval.tick() => {}
            }

            terminal.draw(|frame| ui::render(frame, self))?;

            if self.should_quit {
                break;
            }
        }

        ui::restore_terminal(&mut terminal)?;
        Ok(())
    }

    /// Try to resume the last session, or silently start without one.
    fn resume_or_new_session(&mut self) {
        let mgr = SessionManager::new(&self.storage, &self.project.id);

        if let Some(session) = mgr.last_session() {
            tracing::info!(session_id = %session.id, title = %session.title, "resuming session");

            // Load messages from storage and populate the display
            if let Ok(loaded_messages) = mgr.load_messages(&session.id) {
                self.exchange_count = loaded_messages
                    .iter()
                    .filter(|m| m.role == Role::User)
                    .count();

                for msg in &loaded_messages {
                    match msg.role {
                        Role::User => {
                            self.messages.push(MessageBlock::User {
                                text: msg.text_content(),
                            });
                        }
                        Role::Assistant => {
                            self.messages.push(MessageBlock::Assistant {
                                thinking: None,
                                text: msg.text_content(),
                                tool_groups: vec![],
                            });
                        }
                        Role::System => continue, // Don't display system messages
                    }
                }

                self.stored_messages = loaded_messages;

                if !self.stored_messages.is_empty() {
                    self.message_area_state.scroll_to_bottom();
                }
            }

            // Restore model from session
            self.current_model = Some(session.model_ref.clone());
            self.current_session = Some(session);
        }

        self.update_sidebar();
    }

    async fn handle_event(&mut self, event: AppEvent) -> Result<()> {
        match event {
            AppEvent::Input(Event::Key(key)) => self.handle_key(key).await?,
            AppEvent::Input(Event::Mouse(mouse)) => match mouse.kind {
                // macOS natural scrolling: ScrollDown = swipe up = see older content
                MouseEventKind::ScrollDown => self.message_area_state.scroll_up(3),
                MouseEventKind::ScrollUp => self.message_area_state.scroll_down(3),
                _ => {}
            },
            AppEvent::Input(Event::Resize(_, _)) => {}
            AppEvent::Tick => {
                self.status_line_state.tick();
            }

            // -- Streaming events --
            AppEvent::LlmDelta { text } => {
                if self.streaming_active {
                    // Append to the display message
                    if let Some(last) = self.messages.last_mut() {
                        last.append_text(&text);
                    }
                    // Also append to the in-progress Message for persistence
                    if let Some(msg) = &mut self.streaming_message {
                        msg.append_text(&text);
                    }
                    self.message_area_state.scroll_to_bottom();
                }
            }

            AppEvent::LlmReasoning { text } => {
                if self.streaming_active {
                    if let Some(last) = self.messages.last_mut() {
                        last.append_thinking(&text);
                    }
                    self.message_area_state.scroll_to_bottom();
                }
            }

            // -- Tool events --
            AppEvent::LlmToolCallStreaming { count: _, tool_name } => {
                if let Some(last) = self.messages.last_mut() {
                    last.ensure_preparing_tool_group();
                }
                self.status_line_state.activity = Activity::RunningTool {
                    tool_name,
                    args_summary: String::new(),
                };
                self.message_area_state.scroll_to_bottom();
            }
            AppEvent::LlmToolCall {
                call_id: _,
                tool_name,
                arguments,
            } => {
                let args_summary = extract_args_summary(tool_name, &arguments);
                if let Some(last) = self.messages.last_mut() {
                    last.add_tool_call(tool_name, args_summary.clone());
                }
                self.status_line_state.activity = Activity::RunningTool {
                    tool_name,
                    args_summary,
                };
                self.message_area_state.scroll_to_bottom();
            }
            AppEvent::ToolResult {
                call_id: _,
                tool_name,
                output,
            } => {
                // UTF-8 safe truncation for summary
                let summary = if output.output.chars().count() > 80 {
                    let truncated: String = output.output.chars().take(77).collect();
                    format!("{truncated}...")
                } else {
                    output.output.clone()
                };

                if let Some(last) = self.messages.last_mut() {
                    last.complete_tool_call(
                        tool_name,
                        summary,
                        output.output.clone(),
                        output.is_error,
                    );
                }

                self.update_sidebar();
                self.message_area_state.scroll_to_bottom();
            }

            AppEvent::LlmFinish { usage } => {
                self.is_loading = false;
                self.streaming_active = false;
                self.stream_cancel = None;
                self.status_line_state.activity = Activity::Idle;

                // Remove trailing empty assistant message if present
                if let Some(last) = self.messages.last() {
                    if last.is_empty_assistant() {
                        self.messages.pop();
                    }
                }

                // Save the completed assistant message to storage
                if let Some(msg) = self.streaming_message.take() {
                    let mgr = SessionManager::new(&self.storage, &self.project.id);
                    let _ = mgr.save_message(&msg);
                    self.stored_messages.push(msg.clone());

                    // Update session usage
                    if let (Some(u), Some(session)) =
                        (usage, &mut self.current_session)
                    {
                        let _ = mgr.add_usage(
                            session,
                            u.prompt_tokens,
                            u.completion_tokens,
                        );
                    } else if let Some(session) = &mut self.current_session {
                        let _ = mgr.touch_session(session);
                    }

                    // Auto-generate title after first user+assistant exchange
                    self.exchange_count += 1;
                    if self.exchange_count == 1 {
                        self.maybe_generate_title();
                    }

                    self.update_sidebar();

                    // Check if auto-compact should trigger
                    if self.should_auto_compact() {
                        tracing::info!("auto-compact threshold reached, triggering compaction");
                        let _ = self.handle_command("/compact").await;
                    }
                }
            }
            AppEvent::LlmError { error } => {
                self.is_loading = false;
                self.streaming_active = false;
                self.stream_cancel = None;
                self.streaming_message = None;
                self.messages.push(MessageBlock::Error { text: error });
                self.status_line_state.activity = Activity::Idle;
                self.message_area_state.scroll_to_bottom();
            }
            AppEvent::PermissionRequest(req) => {
                // Show permission prompt to user
                let summary = format!(
                    "\u{26a0} {}: {} \u{2014} Allow? (y)es / (n)o / (a)lways",
                    req.tool_name, req.arguments_summary
                );
                self.messages.push(MessageBlock::System { text: summary });
                self.status_line_state.activity = Activity::WaitingForPermission;
                self.message_area_state.scroll_to_bottom();
                self.pending_permission = Some(PendingPermission {
                    tool_name: req.tool_name,
                    summary: req.arguments_summary,
                    response_tx: req.response_tx,
                });
            }
            AppEvent::CompactFinish { summary } => {
                self.is_loading = false;

                let session_id = self
                    .current_session
                    .as_ref()
                    .map(|s| s.id.clone())
                    .unwrap_or_default();

                let mgr = SessionManager::new(&self.storage, &self.project.id);

                // 1. Delete all old messages from storage
                if let Err(e) = mgr.delete_messages(&session_id) {
                    tracing::error!(error = %e, "failed to delete old messages during compact");
                }

                // 2. Create and save a summary assistant message
                let summary_msg = Message::assistant(&session_id, &summary);
                let _ = mgr.save_message(&summary_msg);

                // 3. Reset token usage on the session
                if let Some(session) = &mut self.current_session {
                    let _ = mgr.reset_usage(session);
                }

                // 4. Replace in-memory stored_messages
                self.stored_messages = vec![summary_msg];

                // 5. Replace display messages with a system notice + the summary
                self.messages.clear();
                self.messages.push(MessageBlock::System {
                    text: "Conversation compacted.".into(),
                });
                self.messages.push(MessageBlock::Assistant {
                    thinking: None,
                    text: summary,
                    tool_groups: vec![],
                });
                self.message_area_state.scroll_to_bottom();
                self.update_sidebar();

                tracing::info!("conversation compacted successfully");
            }
            AppEvent::CompactError { error } => {
                self.is_loading = false;
                self.auto_compact_failed = true;
                self.messages.push(MessageBlock::Error { text: error });
                self.status_line_state.activity = Activity::Idle;
                self.message_area_state.scroll_to_bottom();
                tracing::error!("compaction failed, auto-compact disabled for this session");
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        // If there's a pending permission prompt, intercept keystrokes
        if self.pending_permission.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Some(perm) = self.pending_permission.take() {
                        let _ = perm.response_tx.send(PermissionReply::AllowOnce);
                        self.messages.push(MessageBlock::System {
                            text: format!("\u{2713} allowed: {}", perm.tool_name),
                        });
                        self.status_line_state.activity = Activity::Thinking;
                        self.message_area_state.scroll_to_bottom();
                    }
                    return Ok(());
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    if let Some(perm) = self.pending_permission.take() {
                        let _ = perm.response_tx.send(PermissionReply::Deny);
                        self.messages.push(MessageBlock::System {
                            text: format!("\u{2717} denied: {}", perm.tool_name),
                        });
                        self.status_line_state.activity = Activity::Thinking;
                        self.message_area_state.scroll_to_bottom();
                    }
                    return Ok(());
                }
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    if let Some(perm) = self.pending_permission.take() {
                        let _ = perm.response_tx.send(PermissionReply::AllowAlways);
                        self.messages.push(MessageBlock::System {
                            text: format!("\u{2713} always allow: {}", perm.tool_name),
                        });
                        self.status_line_state.activity = Activity::Thinking;
                        self.message_area_state.scroll_to_bottom();
                    }
                    return Ok(());
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    // Cancel the stream (which will drop the permission request)
                    self.cancel_stream();
                    return Ok(());
                }
                _ => {
                    // Ignore other keys while permission prompt is active
                    return Ok(());
                }
            }
        }

        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                if self.is_loading || self.streaming_active {
                    self.cancel_stream();
                } else {
                    self.should_quit = true;
                }
            }
            (KeyCode::Tab, KeyModifiers::NONE) => {
                self.input.mode = self.input.mode.toggle();
                // Update permission rules for the new mode
                self.sync_permission_mode();
            }
            (KeyCode::Enter, KeyModifiers::SHIFT) => {
                // Shift+Enter: insert newline in textarea (forward as plain Enter)
                self.input.textarea.input(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                ));
            }
            (KeyCode::Enter, KeyModifiers::NONE) => {
                if !self.is_loading {
                    let text = self.input.take_text();
                    let trimmed = text.trim().to_string();
                    if !trimmed.is_empty() {
                        self.handle_input(trimmed).await?;
                    }
                }
            }
            _ => {
                self.input.textarea.input(key);
            }
        }
        Ok(())
    }

    /// Cancel the current streaming task.
    fn cancel_stream(&mut self) {
        tracing::info!("cancelling stream");

        // Signal the stream task to stop
        if let Some(cancel) = self.stream_cancel.take() {
            cancel.cancel();
        }

        // Also dismiss any pending permission prompt
        if let Some(perm) = self.pending_permission.take() {
            let _ = perm.response_tx.send(PermissionReply::Deny);
        }

        self.is_loading = false;
        self.streaming_active = false;
        self.streaming_message = None;

        // Remove trailing empty assistant message
        if let Some(last) = self.messages.last() {
            if last.is_empty_assistant() {
                self.messages.pop();
            }
        }
        self.messages.push(MessageBlock::System {
            text: "cancelled".to_string(),
        });
        self.status_line_state.activity = Activity::Idle;
        self.message_area_state.scroll_to_bottom();
    }

    async fn handle_input(&mut self, text: String) -> Result<()> {
        if text.starts_with('/') {
            return self.handle_command(&text).await;
        }

        // Ensure we have a session
        self.ensure_session();

        let session_id = self
            .current_session
            .as_ref()
            .map(|s| s.id.clone())
            .unwrap_or_default();

        // Create and save user message
        let user_msg = Message::user(&session_id, &text);
        let mgr = SessionManager::new(&self.storage, &self.project.id);
        let _ = mgr.save_message(&user_msg);
        self.stored_messages.push(user_msg);

        // Add user message to display
        self.messages.push(MessageBlock::User { text: text.clone() });
        self.message_area_state.scroll_to_bottom();

        // Try to send to LLM
        let Some(registry) = &self.provider_registry else {
            self.messages.push(MessageBlock::Error {
                text: "No provider configured. Add providers to steve.json.".to_string(),
            });
            return Ok(());
        };

        let Some(model_ref) = &self.current_model else {
            self.messages.push(MessageBlock::Error {
                text: "No model selected. Set 'model' in steve.json.".to_string(),
            });
            return Ok(());
        };

        let resolved = match registry.resolve_model(model_ref) {
            Ok(r) => r,
            Err(e) => {
                self.messages.push(MessageBlock::Error {
                    text: format!("{e}"),
                });
                return Ok(());
            }
        };

        let client = match registry.client(&resolved.provider_id) {
            Ok(c) => c,
            Err(e) => {
                self.messages.push(MessageBlock::Error {
                    text: format!("{e}"),
                });
                return Ok(());
            }
        };

        // Create the assistant message that will accumulate streaming deltas
        let assistant_msg = Message::assistant(&session_id, "");
        self.streaming_message = Some(assistant_msg);

        // Push an empty assistant message to the display
        self.messages.push(MessageBlock::Assistant {
            thinking: None,
            text: String::new(),
            tool_groups: vec![],
        });

        let system_prompt = self.build_system_prompt();
        self.is_loading = true;
        self.streaming_active = true;
        self.status_line_state.activity = Activity::Thinking;
        self.status_line_state.context_window = resolved.config.context_window as u64;

        // Create a cancellation token for this stream
        let cancel_token = CancellationToken::new();
        self.stream_cancel = Some(cancel_token.clone());

        // Build conversation history from stored messages (all except the last user message,
        // which will be passed as user_message separately)
        let history = self.build_api_history();

        // Launch the streaming task with tool support
        stream::spawn_stream(StreamRequest {
            client: client.inner().clone(),
            model: resolved.api_model_id().to_string(),
            system_prompt,
            history,
            user_message: text,
            event_tx: self.event_tx.clone(),
            tool_registry: Some(self.tool_registry.clone()),
            tool_context: Some(ToolContext {
                project_root: self.project.root.clone(),
            }),
            permission_engine: Some(self.permission_engine.clone()),
            tool_cache: self.tool_cache.clone(),
            cancel_token,
        });

        Ok(())
    }

    /// Ensure there's an active session. Creates one if needed.
    fn ensure_session(&mut self) {
        if self.current_session.is_some() {
            return;
        }

        let model_ref = self
            .current_model
            .clone()
            .unwrap_or_else(|| "unknown/unknown".to_string());

        let mgr = SessionManager::new(&self.storage, &self.project.id);
        match mgr.create_session(&model_ref) {
            Ok(session) => {
                tracing::info!(session_id = %session.id, "new session created");
                self.current_session = Some(session);
            }
            Err(_) => {
                // Silently continue without persistence if storage fails
            }
        }
    }

    /// Try to auto-generate a title for the session after the first exchange.
    fn maybe_generate_title(&mut self) {
        let Some(session) = &self.current_session else {
            return;
        };

        // Use the first user message as a simple title (truncated).
        // TODO: Upgrade to use small_model for LLM-generated titles.
        let first_user_msg = self
            .messages
            .iter()
            .find_map(|m| match m {
                MessageBlock::User { text } => Some(text.clone()),
                _ => None,
            });

        if let Some(text) = first_user_msg {
            let title = if text.len() > 60 {
                format!("{}...", &text[..57])
            } else {
                text
            };

            let mgr = SessionManager::new(&self.storage, &self.project.id);
            let mut session = session.clone();
            let _ = mgr.rename_session(&mut session, &title);
            self.current_session = Some(session);
        }
    }

    /// Build API-compatible conversation history from stored messages.
    /// Excludes the last message (the current user message, passed separately).
    #[allow(deprecated)]
    fn build_api_history(&self) -> Vec<ChatCompletionRequestMessage> {
        // All messages except the last one (which is the new user message)
        let history_messages = if self.stored_messages.len() > 1 {
            &self.stored_messages[..self.stored_messages.len() - 1]
        } else {
            return Vec::new();
        };

        history_messages
            .iter()
            .filter_map(|msg| match msg.role {
                Role::User => {
                    let text = msg.text_content();
                    if text.is_empty() {
                        return None;
                    }
                    Some(ChatCompletionRequestMessage::User(
                        ChatCompletionRequestUserMessage {
                            content: ChatCompletionRequestUserMessageContent::Text(text),
                            name: None,
                        },
                    ))
                }
                Role::Assistant => {
                    let text = msg.text_content();
                    if text.is_empty() {
                        return None;
                    }
                    Some(ChatCompletionRequestMessage::Assistant(
                        ChatCompletionRequestAssistantMessage {
                            content: Some(ChatCompletionRequestAssistantMessageContent::Text(text)),
                            name: None,
                            audio: None,
                            tool_calls: None,
                            function_call: None,
                            refusal: None,
                        },
                    ))
                }
                Role::System => None, // System messages are handled separately
            })
            .collect()
    }

    /// Update the sidebar state from current app state.
    fn update_sidebar(&mut self) {
        if let Some(session) = &self.current_session {
            self.sidebar_state.session_title = session.title.clone();
            self.sidebar_state.prompt_tokens = session.token_usage.prompt_tokens;
            self.sidebar_state.completion_tokens = session.token_usage.completion_tokens;
            self.sidebar_state.total_tokens = session.token_usage.total_tokens;
        }
        if let Some(model) = &self.current_model {
            self.sidebar_state.model_name = model.clone();
        }
        // Sync todos from the todo tool
        let tool_todos = crate::tool::todo::get_todos();
        self.sidebar_state.todos = tool_todos
            .into_iter()
            .map(|t| crate::ui::sidebar::TodoItem {
                text: t.text,
                done: t.done,
            })
            .collect();

        // Sync status line state
        if let Some(model) = &self.current_model {
            self.status_line_state.model_name = model.clone();
        }
        if let Some(session) = &self.current_session {
            self.status_line_state.total_tokens = session.token_usage.total_tokens;
        }
    }

    /// Sync the permission engine rules with the current agent mode.
    fn sync_permission_mode(&self) {
        use crate::ui::input::AgentMode;

        let rules = match self.input.mode {
            AgentMode::Build => crate::permission::build_mode_rules(),
            AgentMode::Plan => crate::permission::plan_mode_rules(),
        };

        // Spawn a task to update the engine since it requires async lock
        let engine = self.permission_engine.clone();
        tokio::spawn(async move {
            let mut engine = engine.lock().await;
            engine.set_rules(rules);
        });
    }

    fn build_system_prompt(&self) -> Option<String> {
        use crate::ui::input::AgentMode;

        let mut parts: Vec<String> = Vec::new();

        parts.push(format!(
            "You are a helpful AI coding assistant. You are working in the project at: {}",
            self.project.root.display()
        ));

        if let Some(agents_md) = &self.agents_md {
            parts.push(format!("\n---\n\n{agents_md}"));
        }

        if self.input.mode == AgentMode::Plan {
            parts.push("\n---\n\nYou are currently in PLAN mode. You can read files and analyze the codebase, but you CANNOT write, edit, patch, or create files. Focus on planning, analysis, and providing recommendations. If the user asks you to make changes, explain what you would do but do not attempt to use write tools.".to_string());
        }

        Some(parts.join("\n"))
    }

    /// Determine which model ref to use for compaction/summarization.
    /// Prefers small_model if configured, falls back to current_model.
    fn compact_model_ref(&self) -> Option<String> {
        self.config
            .small_model
            .clone()
            .or_else(|| self.current_model.clone())
    }

    /// Build a transcript of stored messages for the summarizer.
    fn build_compact_prompt(&self) -> String {
        let mut transcript = String::new();
        for msg in &self.stored_messages {
            let role_label = match msg.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::System => "System",
            };
            let text = msg.text_content();
            if !text.is_empty() {
                transcript.push_str(&format!("[{role_label}]: {text}\n\n"));
            }
        }
        transcript
    }

    /// Check if the session is approaching the context window limit and
    /// auto-compact should be triggered.
    fn should_auto_compact(&self) -> bool {
        if !self.config.auto_compact {
            return false;
        }

        if self.auto_compact_failed {
            return false;
        }

        // Need at least a few messages to make compaction worthwhile
        if self.stored_messages.len() < 4 {
            return false;
        }

        let Some(session) = &self.current_session else {
            return false;
        };

        let Some(model_ref) = &self.current_model else {
            return false;
        };

        let Some(registry) = &self.provider_registry else {
            return false;
        };

        let Ok(resolved) = registry.resolve_model(model_ref) else {
            return false;
        };

        let context_window = resolved.config.context_window as u64;
        let threshold = (context_window as f64 * 0.80) as u64;

        session.token_usage.total_tokens >= threshold
    }

    async fn handle_command(&mut self, text: &str) -> Result<()> {
        use crate::command::Command;

        let command = match Command::parse(text) {
            Ok(cmd) => cmd,
            Err(msg) => {
                self.messages.push(MessageBlock::Error { text: msg });
                return Ok(());
            }
        };

        match command {
            Command::Exit => {
                self.should_quit = true;
            }
            Command::New => {
                // Create a fresh session
                self.messages.clear();
                self.stored_messages.clear();
                self.streaming_message = None;
                self.streaming_active = false;
                self.is_loading = false;
                self.exchange_count = 0;
                self.auto_compact_failed = false;
                self.current_session = None;
                // Reset tool result cache for the new session
                *self.tool_cache.lock().unwrap() =
                    ToolResultCache::new(self.project.root.clone());
                self.ensure_session();
                self.message_area_state.scroll_to_bottom();
                self.messages.push(MessageBlock::System {
                    text: "New session started.".to_string(),
                });
                self.update_sidebar();
            }
            Command::Rename(title) => {
                if let Some(session) = &self.current_session {
                    let mgr = SessionManager::new(&self.storage, &self.project.id);
                    let mut session = session.clone();
                    let _ = mgr.rename_session(&mut session, &title);
                    self.current_session = Some(session);
                    self.messages.push(MessageBlock::System {
                        text: format!("Session renamed to: {title}"),
                    });
                    self.update_sidebar();
                }
            }
            Command::Model(model_ref) => {
                if let Some(registry) = &self.provider_registry {
                    match registry.resolve_model(&model_ref) {
                        Ok(_) => {
                            self.current_model = Some(model_ref.to_string());
                            self.messages.push(MessageBlock::System {
                                text: format!("Switched to model: {model_ref}"),
                            });
                            self.update_sidebar();
                        }
                        Err(e) => {
                            self.messages.push(MessageBlock::Error {
                                text: format!("{e}"),
                            });
                        }
                    }
                } else {
                    self.messages.push(MessageBlock::Error {
                        text: "No providers configured.".to_string(),
                    });
                }
            }
            Command::Models => {
                if let Some(registry) = &self.provider_registry {
                    let models = registry.list_models();
                    if models.is_empty() {
                        self.messages.push(MessageBlock::System {
                            text: "No models configured.".to_string(),
                        });
                    } else {
                        let list = models
                            .iter()
                            .map(|m| {
                                let current = self
                                    .current_model
                                    .as_ref()
                                    .is_some_and(|c| c == &m.display_ref());
                                let marker = if current { " \u{25cf}" } else { "" };
                                format!("  {} \u{2014} {}{}", m.display_ref(), m.config.name, marker)
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        self.messages.push(MessageBlock::System {
                            text: format!("Models (use /model <ref> to switch):\n{list}"),
                        });
                    }
                } else {
                    self.messages.push(MessageBlock::Error {
                        text: "No providers configured.".to_string(),
                    });
                }
            }
            Command::Init => {
                let agents_path = self.project.root.join("AGENTS.md");
                if agents_path.exists() {
                    self.messages.push(MessageBlock::System {
                        text: format!("AGENTS.md already exists at {}", agents_path.display()),
                    });
                } else {
                    let default_content = "# AGENTS.md\n\nProject-specific instructions for AI coding assistants.\n\n## Guidelines\n\n- Follow existing code style and conventions.\n- Write clear, concise commit messages.\n- Add tests for new functionality.\n";
                    match std::fs::write(&agents_path, default_content) {
                        Ok(_) => {
                            self.agents_md = Some(default_content.to_string());
                            self.messages.push(MessageBlock::System {
                                text: format!("Created AGENTS.md at {}", agents_path.display()),
                            });
                        }
                        Err(e) => {
                            self.messages.push(MessageBlock::Error {
                                text: format!("Failed to create AGENTS.md: {e}"),
                            });
                        }
                    }
                }
            }
            Command::Compact => {
                // Guard: must have a session with messages
                if self.current_session.is_none() || self.stored_messages.is_empty() {
                    self.messages.push(MessageBlock::System {
                        text: "Nothing to compact.".to_string(),
                    });
                    return Ok(());
                }

                // Guard: must not already be streaming/loading
                if self.is_loading || self.streaming_active {
                    self.messages.push(MessageBlock::Error {
                        text: "Cannot compact while streaming.".to_string(),
                    });
                    return Ok(());
                }

                // Resolve the model for summarization
                let model_ref = match self.compact_model_ref() {
                    Some(r) => r,
                    None => {
                        self.messages.push(MessageBlock::Error {
                            text: "No model available for compaction.".to_string(),
                        });
                        return Ok(());
                    }
                };

                let Some(registry) = &self.provider_registry else {
                    self.messages.push(MessageBlock::Error {
                        text: "No provider configured.".to_string(),
                    });
                    return Ok(());
                };

                let resolved = match registry.resolve_model(&model_ref) {
                    Ok(r) => r,
                    Err(e) => {
                        self.messages.push(MessageBlock::Error {
                            text: format!("Failed to resolve compact model: {e}"),
                        });
                        return Ok(());
                    }
                };

                let client = match registry.client(&resolved.provider_id) {
                    Ok(c) => c.clone(),
                    Err(e) => {
                        self.messages.push(MessageBlock::Error {
                            text: format!("{e}"),
                        });
                        return Ok(());
                    }
                };

                // Show feedback
                let msg_count = self.stored_messages.len();
                self.messages.push(MessageBlock::System {
                    text: format!("Compacting {msg_count} messages..."),
                });
                self.message_area_state.scroll_to_bottom();
                self.is_loading = true;
                self.status_line_state.activity = Activity::Compacting;

                // Build the transcript to summarize
                let transcript = self.build_compact_prompt();
                let api_model_id = resolved.api_model_id().to_string();
                let event_tx = self.event_tx.clone();

                tracing::info!(
                    model = %api_model_id,
                    messages = msg_count,
                    transcript_len = transcript.len(),
                    "starting conversation compaction"
                );

                // Spawn background summarization task
                tokio::spawn(async move {
                    match client
                        .simple_chat(&api_model_id, Some(COMPACT_SYSTEM_PROMPT), &transcript)
                        .await
                    {
                        Ok(summary) => {
                            let _ = event_tx.send(AppEvent::CompactFinish { summary });
                        }
                        Err(e) => {
                            let _ = event_tx.send(AppEvent::CompactError {
                                error: format!("Compaction failed: {e}"),
                            });
                        }
                    }
                });
            }
            Command::Help => {
                self.messages.push(MessageBlock::System {
                    text: "Commands:\n  /new        \u{2014} Start a new session\n  /rename <t> \u{2014} Rename current session\n  /models     \u{2014} List available models\n  /model <r>  \u{2014} Switch to a model\n  /compact    \u{2014} Compact conversation into a summary\n  /init       \u{2014} Create AGENTS.md in project root\n  /help       \u{2014} Show this help\n  /exit       \u{2014} Quit\n\nKeys:\n  Tab         \u{2014} Toggle Build/Plan mode\n  Ctrl+C      \u{2014} Cancel stream / quit\n  Mouse wheel \u{2014} Scroll messages".to_string(),
                });
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_args_summary_read_path() {
        let args = json!({"path": "src/main.rs"});
        assert_eq!(extract_args_summary(ToolName::Read, &args), "src/main.rs");
    }

    #[test]
    fn extract_args_summary_list_path() {
        let args = json!({"path": "/tmp/dir"});
        assert_eq!(extract_args_summary(ToolName::List, &args), "/tmp/dir");
    }

    #[test]
    fn extract_args_summary_grep_pattern() {
        let args = json!({"pattern": "fn main"});
        assert_eq!(extract_args_summary(ToolName::Grep, &args), "fn main");
    }

    #[test]
    fn extract_args_summary_glob_pattern() {
        let args = json!({"pattern": "**/*.rs"});
        assert_eq!(extract_args_summary(ToolName::Glob, &args), "**/*.rs");
    }

    #[test]
    fn extract_args_summary_edit_path() {
        let args = json!({"path": "src/lib.rs", "old_string": "x", "new_string": "y"});
        assert_eq!(extract_args_summary(ToolName::Edit, &args), "src/lib.rs");
    }

    #[test]
    fn extract_args_summary_write_path() {
        let args = json!({"path": "new_file.txt", "content": "hello"});
        assert_eq!(extract_args_summary(ToolName::Write, &args), "new_file.txt");
    }

    #[test]
    fn extract_args_summary_patch_path() {
        let args = json!({"path": "src/app.rs", "diff": "..."});
        assert_eq!(extract_args_summary(ToolName::Patch, &args), "src/app.rs");
    }

    #[test]
    fn extract_args_summary_bash_short_command() {
        let args = json!({"command": "ls -la"});
        assert_eq!(extract_args_summary(ToolName::Bash, &args), "ls -la");
    }

    #[test]
    fn extract_args_summary_bash_long_command_truncates() {
        let long_cmd = "a".repeat(50);
        let args = json!({"command": long_cmd});
        let result = extract_args_summary(ToolName::Bash, &args);
        assert_eq!(result.chars().count(), 40); // 37 + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn extract_args_summary_bash_exactly_40_chars() {
        let cmd = "a".repeat(40);
        let args = json!({"command": cmd});
        let result = extract_args_summary(ToolName::Bash, &args);
        assert_eq!(result.chars().count(), 40);
        assert!(!result.ends_with("..."));
    }

    #[test]
    fn extract_args_summary_question_short() {
        let args = json!({"text": "What is this?"});
        assert_eq!(extract_args_summary(ToolName::Question, &args), "What is this?");
    }

    #[test]
    fn extract_args_summary_question_long_truncates() {
        let long_text = "a".repeat(40);
        let args = json!({"text": long_text});
        let result = extract_args_summary(ToolName::Question, &args);
        assert_eq!(result.chars().count(), 30); // 27 + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn extract_args_summary_todo_always_empty() {
        let args = json!({"action": "add", "text": "something"});
        assert_eq!(extract_args_summary(ToolName::Todo, &args), "");
    }

    #[test]
    fn extract_args_summary_webfetch_url() {
        let args = json!({"url": "https://example.com"});
        assert_eq!(extract_args_summary(ToolName::Webfetch, &args), "https://example.com");
    }

    #[test]
    fn extract_args_summary_missing_field_returns_empty() {
        let args = json!({});
        assert_eq!(extract_args_summary(ToolName::Read, &args), "");
        assert_eq!(extract_args_summary(ToolName::Grep, &args), "");
        assert_eq!(extract_args_summary(ToolName::Edit, &args), "");
        assert_eq!(extract_args_summary(ToolName::Bash, &args), "");
        assert_eq!(extract_args_summary(ToolName::Question, &args), "");
        assert_eq!(extract_args_summary(ToolName::Webfetch, &args), "");
    }

    #[test]
    fn extract_args_summary_all_variants_covered() {
        // Ensure every ToolName variant is handled (exhaustive match).
        // This test will fail to compile if a new variant is added without
        // updating extract_args_summary.
        let args = json!({});
        let all_tools = [
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
        for tool in all_tools {
            // Just ensure it doesn't panic
            let _ = extract_args_summary(tool, &args);
        }
    }
}
