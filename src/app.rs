use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use async_openai::types::chat::{
    ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
    ChatCompletionRequestMessage, ChatCompletionRequestUserMessage,
    ChatCompletionRequestUserMessageContent,
};

use crate::config::types::Config;
use crate::context::cache::ToolResultCache;
use crate::event::AppEvent;
use crate::file_ref;
use crate::permission::PermissionEngine;
use crate::permission::types::PermissionReply;
use crate::project::ProjectInfo;
use crate::provider::ProviderRegistry;
use crate::session::SessionManager;
use crate::session::message::{Message, Role};
use crate::session::types::SessionInfo;
use crate::storage::Storage;
use crate::stream::{self, StreamRequest};
use crate::tool::{ToolContext, ToolName, ToolRegistry};
use crate::ui;
use crate::ui::autocomplete::{AutocompleteMode, AutocompleteState, apply_file_completion};
use crate::ui::input::InputState;
use crate::ui::message_area::MessageAreaState;
use crate::ui::message_block::{
    AssistantPart, DiffContent, DiffLine, MessageBlock, ToolCall,
};
use crate::ui::selection::SelectionState;
use crate::ui::sidebar::{SidebarState, count_diff_lines};
use crate::ui::status_line::{Activity, StatusLineState};
use crate::ui::theme::Theme;

/// System prompt for conversation compaction/summarization.
const COMPACT_SYSTEM_PROMPT: &str = "Provide a detailed but concise summary of the conversation below. \
Focus on information that would be helpful for continuing the conversation, including: \
what was done, what is currently being worked on, which files are being modified, \
decisions that were made, and what the next steps are. \
Preserve specific technical details, file paths, and code patterns.";

/// Guidance for efficient tool usage, injected into the system prompt.
const TOOL_GUIDANCE: &str = "\n\n## Task Planning\n\n\
When the user gives you a task with multiple sequential steps (e.g. \"do X, then Y, then Z\") or any task that will require 3+ distinct actions, \
you MUST use the `todo` tool FIRST to create your plan before doing any other work. \
Add one todo item per step, then work through them one at a time — complete each item before starting the next. \
This keeps you focused and shows the user your progress in the sidebar.\n\n\
## Tool Usage Guidelines\n\n\
- **Use native tools, not bash**: NEVER use `bash` to read files (`cat`, `head`, `tail`), search content (`grep`, `rg`), find files (`find`, `ls`), or write files (`sed`, `awk`, `tee`). Use the dedicated `read`, `grep`, `glob`, `list`, `edit`, `write`, and `patch` tools instead — they are faster, cached, and context-efficient.\n\
- **Verify CLI tools before recommending**: When suggesting an external CLI tool (e.g., `pdftotext`, `jq`, `ffmpeg`), first check if it's installed by running `command -v <tool>` via `bash`. If it's not available, say so explicitly and suggest how to install it (e.g., `brew install poppler` on macOS). Never assume a tool is on the user's PATH.\n\
- **Search before reading**: Use `grep` to find relevant code, then `read` with specific line ranges. Avoid reading entire large files.\n\
- **Use line ranges**: The `read` tool supports `offset` and `limit` parameters. For files over 200 lines, read only the relevant section.\n\
- **Be context-efficient**: Each tool result consumes context window space. Prefer targeted searches over broad reads.\n\
- **Glob for discovery**: Use `glob` to find files by pattern before reading them.\n\
- **Batch related reads**: If you need multiple files, request them in a single response to enable parallel execution.\n\
- **Respond literally**: When the user asks to see, show, or display content, output the actual content in a fenced code block — do not summarize or paraphrase. In general, follow the user's request directly rather than reinterpreting what they want.\n\
- **Avoid re-reading**: Files you've already read are cached. The system will tell you if content is unchanged.\n\
- **Record discoveries**: Use the `memory` tool to save important project context (architecture, patterns, key files) that persists across sessions. \
Your project memory is automatically loaded into context — you don't need to read it manually. \
When memory gets long, use 'replace' to consolidate into a curated summary. Worth remembering: \
architecture decisions, key file locations, recurring patterns, user preferences, gotchas encountered.";

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
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Move | ToolName::Copy => {
            let from = args.get("from_path").and_then(|v| v.as_str()).unwrap_or("");
            let to = args.get("to_path").and_then(|v| v.as_str()).unwrap_or("");
            format!("{from} \u{2192} {to}")
        }
        ToolName::Delete | ToolName::Mkdir => args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Bash => {
            let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
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
        ToolName::Memory => args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    }
}

/// Extract inline diff content from tool call arguments for UI rendering.
/// Returns `None` for tools that don't produce diffs (read, grep, bash, etc.).
fn extract_diff_content(tool_name: ToolName, args: &Value) -> Option<DiffContent> {
    match tool_name {
        ToolName::Edit => {
            let old = args
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new = args
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if old.is_empty() && new.is_empty() {
                return None;
            }
            let mut lines = Vec::new();
            for line in old.lines() {
                lines.push(DiffLine::Removal(line.to_string()));
            }
            for line in new.lines() {
                lines.push(DiffLine::Addition(line.to_string()));
            }
            Some(DiffContent::EditDiff { lines })
        }
        ToolName::Write => {
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let line_count = if content.is_empty() {
                0
            } else {
                content.lines().count()
            };
            Some(DiffContent::WriteSummary { line_count })
        }
        ToolName::Patch => {
            let diff = args.get("diff").and_then(|v| v.as_str()).unwrap_or("");
            if diff.is_empty() {
                return None;
            }
            Some(DiffContent::PatchDiff {
                lines: parse_unified_diff_lines(diff),
            })
        }
        ToolName::Read
        | ToolName::Grep
        | ToolName::Glob
        | ToolName::List
        | ToolName::Bash
        | ToolName::Question
        | ToolName::Todo
        | ToolName::Webfetch
        | ToolName::Memory
        | ToolName::Move
        | ToolName::Copy
        | ToolName::Delete
        | ToolName::Mkdir => None,
    }
}

/// Parse a unified diff string into structured `DiffLine` entries.
/// Skips `---`/`+++` file headers, keeps `@@` hunk headers.
fn parse_unified_diff_lines(patch: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    for line in patch.lines() {
        if line.starts_with("---") || line.starts_with("+++") {
            // Skip file headers
            continue;
        } else if line.starts_with("@@") {
            lines.push(DiffLine::HunkHeader(line.to_string()));
        } else if let Some(rest) = line.strip_prefix('-') {
            lines.push(DiffLine::Removal(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix('+') {
            lines.push(DiffLine::Addition(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix(' ') {
            lines.push(DiffLine::Context(rest.to_string()));
        } else {
            // Lines without a prefix (e.g., "No newline at end of file") → context
            lines.push(DiffLine::Context(line.to_string()));
        }
    }
    lines
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
    pub autocomplete_state: AutocompleteState,
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

    /// Active session browser list (populated by /sessions, cleared on selection or dismiss).
    session_browse_list: Option<Vec<SessionInfo>>,

    /// Cancellation token for the current stream task.
    stream_cancel: Option<CancellationToken>,

    /// Channel for sending user interjections to the active stream task.
    interjection_tx: Option<mpsc::UnboundedSender<String>>,

    /// Whether auto-compact has failed in this session (suppresses retries).
    auto_compact_failed: bool,

    /// Whether the 60% context warning has been shown this session.
    pub context_warned: bool,

    /// Last prompt_tokens reported by the API (current context window usage).
    pub last_prompt_tokens: u64,

    /// User override for sidebar visibility: None = auto, Some(true) = show, Some(false) = hide.
    pub sidebar_override: Option<bool>,

    /// Lazily populated file index for `@` autocomplete.
    file_index: Option<Vec<String>>,

    /// Text selection state for copy-on-select.
    pub selection_state: SelectionState,

    /// Message area rect from last render (for mouse hit-testing).
    pub last_message_area: ratatui::layout::Rect,

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
        config_warnings: Vec<String>,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // Determine the default model from config
        let current_model = config.model.clone();

        // Build tool registry
        let tool_registry = Arc::new(ToolRegistry::new(project.root.clone()));

        // Build permission engine with Plan mode rules (default start mode)
        // Profile-aware rules will be set on first sync_permission_mode call
        let profile = config.permission_profile.unwrap_or(crate::permission::PermissionProfile::Standard);
        let allow_overrides: Vec<ToolName> = config.allow_tools.iter()
            .filter_map(|s| s.parse::<ToolName>().ok())
            .collect();
        let permission_engine = Arc::new(tokio::sync::Mutex::new(PermissionEngine::new(
            crate::permission::profile_plan_rules(profile, &allow_overrides, &config.permission_rules),
        )));

        // Build tool result cache (session-scoped, shared across stream tasks)
        let tool_cache = Arc::new(std::sync::Mutex::new(ToolResultCache::new(
            project.root.clone(),
        )));

        // Build startup messages
        let mut messages = Vec::new();

        // Show config parse warnings first so the user knows what went wrong
        for warning in &config_warnings {
            messages.push(MessageBlock::Error {
                text: warning.clone(),
            });
        }

        if config.providers.is_empty() {
            messages.push(MessageBlock::Assistant {
                thinking: None,
                parts: vec![AssistantPart::Text("No providers configured. Create a .steve.jsonc config file or a global ~/.config/steve/config.jsonc to get started.".to_string())],
            });
        } else if let Some(err) = provider_error {
            messages.push(MessageBlock::Assistant {
                thinking: None,
                parts: vec![AssistantPart::Text(format!("Provider setup failed: {err}"))],
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
            autocomplete_state: AutocompleteState::default(),
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
            session_browse_list: None,
            stream_cancel: None,
            interjection_tx: None,
            auto_compact_failed: false,
            context_warned: false,
            last_prompt_tokens: 0,
            sidebar_override: None,
            file_index: None,
            selection_state: SelectionState::default(),
            last_message_area: ratatui::layout::Rect::default(),
            event_tx,
            event_rx,
            should_quit: false,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        // Try to resume the last session
        self.resume_or_new_session();

        let (mut terminal, detected) = ui::detect_and_setup_terminal()?;
        self.theme = ui::terminal_detect::resolve_theme(self.config.theme, detected);
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
                _ = tick_interval.tick() => {
                    self.handle_event(AppEvent::Tick).await?;
                }
            }

            terminal.draw(|frame| ui::render(frame, self))?;

            if self.should_quit {
                break;
            }
        }

        // Prune the session if the user never sent a message
        self.prune_empty_session();

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
                                parts: vec![AssistantPart::Text(msg.text_content())],
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

            // Restore model from session, falling back to config if the saved
            // model_ref is no longer valid (e.g. config was updated after save).
            self.current_model = Some(self.validated_model_ref(&session.model_ref));
            self.current_session = Some(session);
        }

        self.refresh_git_info();
        self.sync_sidebar_tokens();
        self.update_sidebar();
    }

    /// Switch to a different session (used by session browser).
    async fn switch_to_session(&mut self, session: SessionInfo) -> Result<()> {
        let mgr = SessionManager::new(&self.storage, &self.project.id);

        // Cancel any active stream
        if let Some(token) = self.stream_cancel.take() {
            token.cancel();
        }

        // Prune the old session if empty before switching away
        self.prune_empty_session();

        // Clear current state
        self.messages.clear();
        self.stored_messages.clear();
        self.streaming_message = None;
        self.streaming_active = false;
        self.is_loading = false;
        self.auto_compact_failed = false;
        self.context_warned = false;
        self.last_prompt_tokens = 0;
        self.exchange_count = 0;
        self.pending_permission = None;
        *self.tool_cache.lock().unwrap() = ToolResultCache::new(self.project.root.clone());

        // Load messages
        if let Ok(loaded_messages) = mgr.load_messages(&session.id) {
            self.exchange_count = loaded_messages
                .iter()
                .filter(|m| m.role == Role::User)
                .count();
            for msg in &loaded_messages {
                match msg.role {
                    Role::User => self.messages.push(MessageBlock::User {
                        text: msg.text_content(),
                    }),
                    Role::Assistant => self.messages.push(MessageBlock::Assistant {
                        thinking: None,
                        parts: vec![AssistantPart::Text(msg.text_content())],
                    }),
                    Role::System => continue,
                }
            }
            self.stored_messages = loaded_messages;
        }

        // Update tracking
        let mut meta = mgr.load_project_meta();
        meta.last_session_id = Some(session.id.clone());
        meta.last_model = Some(session.model_ref.clone());
        let _ = mgr.save_project_meta(&meta);

        self.current_model = Some(self.validated_model_ref(&session.model_ref));
        self.current_session = Some(session.clone());
        self.sidebar_state.changes.clear();
        self.refresh_git_info();
        self.sync_sidebar_tokens();
        self.messages.push(MessageBlock::System {
            text: format!("Switched to: {}", session.title),
        });
        self.message_area_state.scroll_to_bottom();
        self.update_sidebar();
        Ok(())
    }

    async fn handle_event(&mut self, event: AppEvent) -> Result<()> {
        match event {
            AppEvent::Input(Event::Key(key)) => self.handle_key(key).await?,
            AppEvent::Input(Event::Mouse(mouse)) => {
                use crossterm::event::MouseButton;
                match mouse.kind {
                    MouseEventKind::ScrollDown => self.message_area_state.scroll_down(3),
                    MouseEventKind::ScrollUp => self.message_area_state.scroll_up(3),
                    MouseEventKind::Down(MouseButton::Left) => {
                        let area = self.last_message_area;
                        if mouse.row >= area.y
                            && mouse.row < area.y + area.height
                            && mouse.column >= area.x
                            && mouse.column < area.x + area.width
                        {
                            // Clear any previous selection and start a new drag
                            self.selection_state.clear();
                            if let Some(map) = &self.message_area_state.content_map {
                                if let Some(pos) = map.screen_to_content(
                                    mouse.row,
                                    mouse.column,
                                    self.message_area_state.scroll_offset,
                                    area.y,
                                    area.x,
                                ) {
                                    self.selection_state.anchor = Some(pos);
                                    self.selection_state.cursor = Some(pos);
                                    self.selection_state.dragging = true;
                                }
                            }
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if self.selection_state.dragging {
                            let area = self.last_message_area;
                            // Scroll-to-select: scroll when dragging past edges
                            if mouse.row < area.y {
                                self.message_area_state.scroll_up(1);
                            } else if mouse.row >= area.y + area.height {
                                self.message_area_state.scroll_down(1);
                            }
                            if let Some(map) = &self.message_area_state.content_map {
                                if let Some(pos) = map.screen_to_content(
                                    mouse.row,
                                    mouse.column,
                                    self.message_area_state.scroll_offset,
                                    area.y,
                                    area.x,
                                ) {
                                    self.selection_state.cursor = Some(pos);
                                }
                            }
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        if self.selection_state.dragging {
                            self.selection_state.dragging = false;
                            // If we have a valid selection range, copy to clipboard
                            if let Some((start, end)) = self.selection_state.ordered_range() {
                                if let Some(map) = &self.message_area_state.content_map {
                                    let text = map.extract_text(&start, &end);
                                    if !text.is_empty() {
                                        self.copy_to_clipboard(&text);
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            AppEvent::Input(Event::Paste(text)) => {
                if self.pending_permission.is_none() {
                    self.input.textarea.insert_str(&text);
                    let current_text = self.input.textarea.lines().join("\n");
                    self.autocomplete_state.update(&current_text);
                }
            }
            AppEvent::Input(Event::Resize(_, _)) => {}
            AppEvent::Tick => {
                self.status_line_state.tick();
                // Clear expired "Copied!" flash
                if let Some(t) = self.selection_state.copied_flash {
                    if t.elapsed().as_secs() >= 1 {
                        self.selection_state.copied_flash = None;
                    }
                }
            }

            // -- Streaming events --
            AppEvent::LlmDelta { text } => {
                if self.streaming_active {
                    // Append to the display message
                    if let Some(last) = self.last_assistant_mut() {
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
                    if let Some(last) = self.last_assistant_mut() {
                        last.append_thinking(&text);
                    }
                    self.message_area_state.scroll_to_bottom();
                }
            }

            // -- Tool events --
            AppEvent::LlmToolCallStreaming {
                count: _,
                tool_name,
            } => {
                if let Some(last) = self.last_assistant_mut() {
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
                let diff_content = extract_diff_content(tool_name, &arguments);
                if let Some(last) = self.last_assistant_mut() {
                    last.add_tool_call(tool_name, args_summary.clone(), diff_content);
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

                if let Some(last) = self.last_assistant_mut() {
                    last.complete_tool_call(
                        tool_name,
                        summary,
                        output.output.clone(),
                        output.is_error,
                    );
                }

                // Invalidate file index on successful write tool completion
                if tool_name.is_write_tool() && !output.is_error {
                    self.invalidate_file_index();
                }

                // Record changeset on successful write tool completion
                if tool_name.is_write_tool() && !output.is_error {
                    if let Some(call) = self.find_last_completed_call(tool_name) {
                        if let Some(diff) = &call.diff_content {
                            let (additions, removals) = count_diff_lines(diff);
                            let display_path = self.strip_project_root(&call.args_summary);
                            self.sidebar_state.record_file_change(
                                display_path,
                                additions,
                                removals,
                            );
                        }
                    }
                }

                // Refresh git dirty status after write tools or bash (may have committed, etc.)
                if (tool_name.is_write_tool() || tool_name == ToolName::Bash) && !output.is_error {
                    self.refresh_git_info();
                }

                self.update_sidebar();
                self.message_area_state.scroll_to_bottom();
            }

            AppEvent::LlmFinish { usage } => {
                self.is_loading = false;
                self.streaming_active = false;
                self.stream_cancel = None;
                self.interjection_tx = None;
                self.status_line_state.activity = Activity::Idle;

                // Remove trailing empty assistant message if present
                if let Some(last) = self.messages.last()
                    && last.is_empty_assistant()
                {
                    self.messages.pop();
                }

                // Save the completed assistant message to storage
                if let Some(msg) = self.streaming_message.take() {
                    let mgr = SessionManager::new(&self.storage, &self.project.id);
                    let _ = mgr.save_message(&msg);
                    self.stored_messages.push(msg.clone());

                    // Update session usage (cumulative for storage).
                    // Note: last_prompt_tokens is NOT set here — LlmUsageUpdate
                    // already set the correct per-call value. The usage here is
                    // accumulated across all loop iterations, which is wrong for
                    // context pressure display but correct for cumulative cost.
                    if let (Some(u), Some(session)) = (usage, &mut self.current_session) {
                        let _ = mgr.add_usage(session, u.prompt_tokens, u.completion_tokens);
                    } else if let Some(session) = &mut self.current_session {
                        let _ = mgr.touch_session(session);
                    }

                    // Auto-generate title after first user+assistant exchange
                    self.exchange_count += 1;
                    if self.exchange_count == 1 {
                        self.maybe_generate_title();
                    }

                    self.sync_sidebar_tokens();
                    self.update_sidebar();

                    // Check context usage and warn if approaching limits
                    self.check_context_warning();

                    // Check if auto-compact should trigger
                    if self.should_auto_compact() {
                        tracing::info!("auto-compact threshold reached, triggering compaction");
                        let _ = self.handle_command("/compact").await;
                    }
                }
            }
            AppEvent::LlmUsageUpdate { usage } => {
                // Update token counters mid-loop without saving to storage.
                // usage.prompt_tokens is the current call's prompt tokens (context pressure).
                self.last_prompt_tokens = usage.prompt_tokens as u64;
                self.status_line_state.last_prompt_tokens = usage.prompt_tokens as u64;
                // Accumulate into sidebar display counters for live updates.
                // session.token_usage is NOT updated here (happens on LlmFinish to
                // avoid intermediate disk writes). update_sidebar() will overwrite
                // these with authoritative session values after LlmFinish.
                self.sidebar_state.prompt_tokens += usage.prompt_tokens as u64;
                self.sidebar_state.completion_tokens += usage.completion_tokens as u64;
                self.sidebar_state.total_tokens +=
                    (usage.prompt_tokens + usage.completion_tokens) as u64;
                // Sync context pressure to sidebar for live Ctx: display
                self.sidebar_state.context_window = self.status_line_state.context_window;
                self.sidebar_state.last_prompt_tokens = self.status_line_state.last_prompt_tokens;
                self.check_context_warning();
            }
            AppEvent::LlmRetry {
                attempt,
                max_attempts,
                error,
            } => {
                self.messages.push(MessageBlock::System {
                    text: format!(
                        "Connection error: {error}\nRetrying ({attempt}/{max_attempts})..."
                    ),
                });
                self.message_area_state.scroll_to_bottom();
            }
            AppEvent::LlmError { error } => {
                self.is_loading = false;
                self.streaming_active = false;
                self.stream_cancel = None;
                self.interjection_tx = None;
                self.streaming_message = None;
                self.messages.push(MessageBlock::Error { text: error });
                self.status_line_state.activity = Activity::Idle;
                self.message_area_state.scroll_to_bottom();
            }
            AppEvent::PermissionRequest(req) => {
                // Show permission prompt to user, with diff preview if available
                let diff_content = extract_diff_content(req.tool_name, &req.tool_args);
                self.messages.push(MessageBlock::Permission {
                    tool_name: req.tool_name.to_string(),
                    args_summary: req.arguments_summary.clone(),
                    diff_content,
                });
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
                    parts: vec![AssistantPart::Text(summary)],
                });
                self.message_area_state.scroll_to_bottom();
                // Reset context warning and prompt tokens since conversation is fresh
                self.context_warned = false;
                self.last_prompt_tokens = 0;
                self.sync_sidebar_tokens();
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

    /// Copy text to the system clipboard.
    /// Tries pbcopy (macOS), xclip (Linux), then falls back to OSC 52.
    fn copy_to_clipboard(&mut self, text: &str) {
        use std::io::Write as _;
        use std::process::{Command, Stdio};

        // Try pbcopy (macOS)
        let ok = Command::new("pbcopy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .and_then(|mut child| {
                if let Some(stdin) = child.stdin.as_mut() {
                    stdin.write_all(text.as_bytes())?;
                }
                child.wait()
            })
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            self.selection_state.copied_flash = Some(std::time::Instant::now());
            return;
        }

        // Try xclip (Linux)
        let ok = Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .and_then(|mut child| {
                if let Some(stdin) = child.stdin.as_mut() {
                    stdin.write_all(text.as_bytes())?;
                }
                child.wait()
            })
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            self.selection_state.copied_flash = Some(std::time::Instant::now());
            return;
        }

        // Fallback: OSC 52
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(text);
        let mut stdout = std::io::stdout();
        let write_result = std::io::Write::write_fmt(
            &mut stdout,
            format_args!("\x1b]52;c;{encoded}\x07"),
        );
        let result = write_result.and_then(|_| std::io::Write::flush(&mut stdout));
        if result.is_ok() {
            self.selection_state.copied_flash = Some(std::time::Instant::now());
        } else {
            tracing::error!("failed to copy to clipboard via any method");
        }
    }

    async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        // If there's a pending permission prompt, intercept keystrokes
        if self.pending_permission.is_some() {
            match (key.code, key.modifiers) {
                (KeyCode::Char('y'), _) | (KeyCode::Char('Y'), _) => {
                    if let Some(perm) = self.pending_permission.take() {
                        let _ = perm.response_tx.send(PermissionReply::AllowOnce);
                        self.remove_last_permission_block();
                        self.status_line_state.activity = Activity::Thinking;
                        self.message_area_state.scroll_to_bottom();
                    }
                    return Ok(());
                }
                (KeyCode::Char('n'), _) | (KeyCode::Char('N'), _) | (KeyCode::Esc, _) => {
                    if let Some(perm) = self.pending_permission.take() {
                        let _ = perm.response_tx.send(PermissionReply::Deny);
                        self.remove_last_permission_block();
                        self.messages.push(MessageBlock::System {
                            text: format!("\u{2717} denied: {}", perm.tool_name),
                        });
                        self.status_line_state.activity = Activity::Thinking;
                        self.message_area_state.scroll_to_bottom();
                    }
                    return Ok(());
                }
                (KeyCode::Char('a'), _) | (KeyCode::Char('A'), _) => {
                    if let Some(perm) = self.pending_permission.take() {
                        let tool_str = perm.tool_name.as_str().to_string();
                        let _ = perm.response_tx.send(PermissionReply::AllowAlways);
                        self.remove_last_permission_block();
                        self.status_line_state.activity = Activity::Thinking;
                        self.message_area_state.scroll_to_bottom();

                        // Persist the grant to project config so it survives restarts
                        self.persist_tool_grant(&tool_str);
                    }
                    return Ok(());
                }
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
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
            (KeyCode::Enter, KeyModifiers::NONE) if self.autocomplete_state.visible => {
                match self.autocomplete_state.mode {
                    AutocompleteMode::Command => {
                        if let Some(cmd_name) = self.autocomplete_state.selected_command() {
                            let cmd_name = cmd_name.to_string();
                            self.autocomplete_state.hide();
                            self.input.take_text(); // clear input
                            if !self.is_loading {
                                self.handle_input(cmd_name).await?;
                            }
                        }
                    }
                    AutocompleteMode::FileRef => {
                        if let Some(path) = self.autocomplete_state.selected_file() {
                            let path = path.to_string();
                            let current = self.input.textarea.lines().join("\n");
                            let new_text = apply_file_completion(&current, &path);
                            self.input.set_text(&new_text);
                            self.autocomplete_state.hide();
                        }
                    }
                }
            }
            (KeyCode::Tab, KeyModifiers::NONE) if self.autocomplete_state.visible => {
                match self.autocomplete_state.mode {
                    AutocompleteMode::Command => {
                        if let Some(cmd_name) = self.autocomplete_state.selected_command() {
                            let cmd_name = cmd_name.to_string();
                            self.autocomplete_state.hide();
                            self.input.take_text(); // clear input
                            if !self.is_loading {
                                self.handle_input(cmd_name).await?;
                            }
                        }
                    }
                    AutocompleteMode::FileRef => {
                        if let Some(path) = self.autocomplete_state.selected_file() {
                            let path = path.to_string();
                            let current = self.input.textarea.lines().join("\n");
                            let new_text = apply_file_completion(&current, &path);
                            self.input.set_text(&new_text);
                            self.autocomplete_state.hide();
                        }
                    }
                }
            }
            (KeyCode::Up, KeyModifiers::NONE) if self.autocomplete_state.visible => {
                self.autocomplete_state.prev();
            }
            (KeyCode::Down, KeyModifiers::NONE) if self.autocomplete_state.visible => {
                self.autocomplete_state.next();
            }
            (KeyCode::Up, KeyModifiers::NONE) => {
                self.message_area_state.scroll_up(1);
            }
            (KeyCode::Down, KeyModifiers::NONE) => {
                self.message_area_state.scroll_down(1);
            }
            (KeyCode::PageUp, KeyModifiers::NONE) => {
                let page = self.message_area_state.visible_height().max(1);
                self.message_area_state.scroll_up(page);
            }
            (KeyCode::PageDown, KeyModifiers::NONE) => {
                let page = self.message_area_state.visible_height().max(1);
                self.message_area_state.scroll_down(page);
            }
            (KeyCode::Esc, KeyModifiers::NONE) if self.autocomplete_state.visible => {
                self.autocomplete_state.hide();
            }
            (KeyCode::Tab, KeyModifiers::NONE) => {
                let current_text = self.input.textarea.lines().join("\n");
                if current_text.starts_with('/') {
                    self.autocomplete_state.update(&current_text);
                } else {
                    self.input.mode = self.input.mode.toggle();
                    // Update permission rules for the new mode
                    self.sync_permission_mode();
                }
            }
            (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                self.sidebar_override = match self.sidebar_override {
                    None => Some(false),       // auto (likely visible) -> hide
                    Some(false) => Some(true), // hidden -> show
                    Some(true) => None,        // forced visible -> auto
                };
            }
            (KeyCode::Enter, KeyModifiers::SHIFT) => {
                // Shift+Enter: insert newline in textarea (forward as plain Enter)
                self.input
                    .textarea
                    .input(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            }
            (KeyCode::Enter, KeyModifiers::NONE) => {
                let text = self.input.take_text();
                let trimmed = text.trim().to_string();
                if !trimmed.is_empty() {
                    if self.is_loading {
                        self.handle_interjection(trimmed);
                    } else {
                        self.handle_input(trimmed).await?;
                    }
                }
            }
            _ => {
                self.input.textarea.input(key);
                let current_text = self.input.textarea.lines().join("\n");
                self.ensure_file_index();
                let file_index = self.file_index.clone().unwrap_or_default();
                self.autocomplete_state.update_with_files(&current_text, &file_index);
            }
        }
        Ok(())
    }

    /// Find the last Assistant block in messages.
    /// Permission/System blocks can be interleaved during streaming, so
    /// `messages.last_mut()` may not be the Assistant block we need.
    fn last_assistant_mut(&mut self) -> Option<&mut MessageBlock> {
        self.messages.iter_mut().rev().find(|m| m.is_assistant())
    }

    /// Remove the last Permission block from messages.
    /// Called after the user responds to a permission prompt so the ephemeral
    /// prompt doesn't appear out-of-order with tool call results.
    fn remove_last_permission_block(&mut self) {
        if let Some(pos) = self
            .messages
            .iter()
            .rposition(|m| matches!(m, MessageBlock::Permission { .. }))
        {
            self.messages.remove(pos);
        }
    }

    /// Lazily build the file index for `@` autocomplete.
    fn ensure_file_index(&mut self) -> &[String] {
        if self.file_index.is_none() {
            self.file_index = Some(file_ref::build_file_index(&self.project.root));
        }
        self.file_index.as_ref().unwrap()
    }

    /// Invalidate the file index (called after write tools complete).
    fn invalidate_file_index(&mut self) {
        self.file_index = None;
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
        self.interjection_tx = None;
        self.streaming_message = None;

        // Remove trailing empty assistant message
        if let Some(last) = self.messages.last()
            && last.is_empty_assistant()
        {
            self.messages.pop();
        }
        self.messages.push(MessageBlock::System {
            text: "cancelled".to_string(),
        });
        self.status_line_state.activity = Activity::Idle;
        self.message_area_state.scroll_to_bottom();
    }

    /// Inject a user message into the active tool loop without cancelling it.
    /// The message is sent via the interjection channel to the stream task,
    /// which drains it before the next LLM API call.
    fn handle_interjection(&mut self, text: String) {
        // Silently reject slash commands during interjection
        if text.starts_with('/') {
            return;
        }

        let Some(tx) = &self.interjection_tx else {
            return;
        };

        // Parse and resolve @ file references
        let refs = file_ref::parse_refs(&text);
        let resolved: Vec<_> = refs
            .iter()
            .filter_map(|r| file_ref::resolve_ref(r, &self.project.root))
            .collect();

        // Show errors for unresolved refs
        for r in &refs {
            if !resolved.iter().any(|rr| rr.file_ref.path == r.path) {
                self.messages.push(MessageBlock::System {
                    text: format!("Could not resolve file: {}", r.path),
                });
            }
        }

        let (display_text, api_text) = if resolved.is_empty() {
            (text.clone(), text.clone())
        } else {
            file_ref::augment_message(&text, &resolved)
        };

        // Send augmented text to stream task via interjection channel
        if tx.send(api_text.clone()).is_err() {
            // Channel closed — stream already finished
            return;
        }

        // Persist to storage
        if let Some(session) = &self.current_session {
            let user_msg = Message::user(&session.id, &api_text);
            let mgr = SessionManager::new(&self.storage, &self.project.id);
            let _ = mgr.save_message(&user_msg);
            self.stored_messages.push(user_msg);
        }

        // Add to display
        self.messages
            .push(MessageBlock::User { text: display_text });
        self.message_area_state.scroll_to_bottom();
    }

    /// Whether the sidebar should be shown, considering user override and terminal width.
    pub fn should_show_sidebar(&self, terminal_width: u16) -> bool {
        match self.sidebar_override {
            Some(forced) => forced,
            None => terminal_width >= 120,
        }
    }

    async fn handle_input(&mut self, text: String) -> Result<()> {
        // Handle session browser selection
        if let Some(ref browse_list) = self.session_browse_list {
            if let Ok(n) = text.parse::<usize>() {
                if n >= 1 && n <= browse_list.len() {
                    let selected = browse_list[n - 1].clone();
                    self.session_browse_list = None;
                    return self.switch_to_session(selected).await;
                } else {
                    self.messages.push(MessageBlock::Error {
                        text: format!("Enter 1-{}.", browse_list.len()),
                    });
                    self.message_area_state.scroll_to_bottom();
                    return Ok(());
                }
            }
            // Non-numeric input dismisses browser
            self.session_browse_list = None;
        }

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

        // Parse and resolve @ file references
        let refs = file_ref::parse_refs(&text);
        let resolved: Vec<_> = refs
            .iter()
            .filter_map(|r| file_ref::resolve_ref(r, &self.project.root))
            .collect();

        // Show errors for unresolved refs
        for r in &refs {
            if !resolved.iter().any(|rr| rr.file_ref.path == r.path) {
                self.messages.push(MessageBlock::System {
                    text: format!("Could not resolve file: {}", r.path),
                });
            }
        }

        let (display_text, api_text) = if resolved.is_empty() {
            (text.clone(), text.clone())
        } else {
            file_ref::augment_message(&text, &resolved)
        };

        // Create and save user message
        let user_msg = Message::user(&session_id, &api_text);
        let mgr = SessionManager::new(&self.storage, &self.project.id);
        let _ = mgr.save_message(&user_msg);
        self.stored_messages.push(user_msg);

        // Add user message to display
        self.messages
            .push(MessageBlock::User { text: display_text });
        self.message_area_state.scroll_to_bottom();

        // Try to send to LLM
        let Some(registry) = &self.provider_registry else {
            self.messages.push(MessageBlock::Error {
                text: "No provider configured. Add providers to .steve.jsonc or ~/.config/steve/config.jsonc.".to_string(),
            });
            return Ok(());
        };

        let Some(model_ref) = &self.current_model else {
            self.messages.push(MessageBlock::Error {
                text: "No model selected. Set 'model' in .steve.jsonc or ~/.config/steve/config.jsonc.".to_string(),
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
            parts: vec![],
        });

        let system_prompt = self.build_system_prompt();
        self.is_loading = true;
        self.streaming_active = true;
        self.status_line_state.activity = Activity::Thinking;
        self.status_line_state.context_window = resolved.config.context_window as u64;

        // Create a cancellation token for this stream
        let cancel_token = CancellationToken::new();
        self.stream_cancel = Some(cancel_token.clone());

        // Create interjection channel for mid-loop user messages
        let (interjection_tx, interjection_rx) = mpsc::unbounded_channel();
        self.interjection_tx = Some(interjection_tx);

        // Build conversation history from stored messages (all except the last user message,
        // which will be passed as user_message separately)
        let history = self.build_api_history();

        // Launch the streaming task with tool support
        stream::spawn_stream(StreamRequest {
            stream_provider: std::sync::Arc::new(stream::OpenAIChatStream::new(
                client.inner().clone(),
            )),
            model: resolved.api_model_id().to_string(),
            system_prompt,
            history,
            user_message: api_text,
            event_tx: self.event_tx.clone(),
            tool_registry: Some(self.tool_registry.clone()),
            tool_context: Some(ToolContext {
                project_root: self.project.root.clone(),
                storage_dir: Some(self.storage.base_dir().clone()),
            }),
            permission_engine: Some(self.permission_engine.clone()),
            tool_cache: self.tool_cache.clone(),
            cancel_token,
            context_window: if self.status_line_state.context_window > 0 {
                Some(self.status_line_state.context_window)
            } else {
                None
            },
            interjection_rx,
        });

        Ok(())
    }

    /// If the current session has zero user messages, delete it from storage.
    /// Called before `/new`, `switch_to_session`, and on exit to avoid
    /// accumulating empty sessions. Callers must cancel the active stream
    /// first (if any) and are responsible for clearing `self.current_session`
    /// afterward if continuing to a new session.
    fn prune_empty_session(&self) {
        if self.exchange_count == 0 && self.stored_messages.is_empty() {
            if let Some(session) = &self.current_session {
                let mgr = SessionManager::new(&self.storage, &self.project.id);
                if let Err(e) = mgr.delete_session(&session.id) {
                    tracing::warn!(error = %e, "failed to prune empty session");
                } else {
                    tracing::info!(session_id = %session.id, "pruned empty session");
                }
            }
        }
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
        let first_user_msg = self.messages.iter().find_map(|m| match m {
            MessageBlock::User { text } => Some(text.clone()),
            _ => None,
        });

        if let Some(text) = first_user_msg {
            let title = if text.chars().count() > 60 {
                let truncated: String = text.chars().take(57).collect();
                format!("{truncated}...")
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
    /// Note: token counters are NOT synced here — they are updated live by
    /// `LlmUsageUpdate` (accumulate per-call) and authoritatively by
    /// `sync_sidebar_tokens()` after `add_usage()` on `LlmFinish`.
    fn update_sidebar(&mut self) {
        if let Some(session) = &self.current_session {
            self.sidebar_state.session_title = session.title.clone();
        }
        if let Some(model) = &self.current_model {
            self.sidebar_state.model_name = model.clone();
        }
        // Calculate session cost if model has pricing
        self.sidebar_state.session_cost = None;
        if let (Some(model_ref), Some(registry), Some(session)) = (
            &self.current_model,
            &self.provider_registry,
            &self.current_session,
        ) {
            if let Ok(resolved) = registry.resolve_model(model_ref) {
                self.sidebar_state.session_cost = resolved.session_cost(
                    session.token_usage.prompt_tokens,
                    session.token_usage.completion_tokens,
                );
            }
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
        self.status_line_state.last_prompt_tokens = self.last_prompt_tokens;
    }

    /// Sync sidebar token counters from the authoritative session data.
    /// Call after `add_usage()` (LlmFinish) or session reset (/new).
    fn sync_sidebar_tokens(&mut self) {
        if let Some(session) = &self.current_session {
            self.sidebar_state.prompt_tokens = session.token_usage.prompt_tokens;
            self.sidebar_state.completion_tokens = session.token_usage.completion_tokens;
            self.sidebar_state.total_tokens = session.token_usage.total_tokens;
        } else {
            self.sidebar_state.prompt_tokens = 0;
            self.sidebar_state.completion_tokens = 0;
            self.sidebar_state.total_tokens = 0;
        }
        // Sync context pressure fields so sidebar can show Ctx: X/Y (Z%)
        self.sidebar_state.context_window = self.status_line_state.context_window;
        self.sidebar_state.last_prompt_tokens = self.status_line_state.last_prompt_tokens;
    }

    /// Refresh git information in the sidebar state.
    fn refresh_git_info(&mut self) {
        use crate::project::{git_branch, git_is_dirty, git_repo_name};
        self.sidebar_state.git_branch = git_branch(&self.project.root);
        self.sidebar_state.git_dirty = git_is_dirty(&self.project.root);
        self.sidebar_state.git_repo_name = git_repo_name(&self.project.root);
    }

    /// Find the most recently completed tool call with the given name in the last
    /// assistant message's tool groups. Returns a reference to the `ToolCall`.
    fn find_last_completed_call(&self, tool_name: ToolName) -> Option<&ToolCall> {
        let last = self.messages.iter().rev().find(|m| m.is_assistant())?;
        if let MessageBlock::Assistant { parts, .. } = last {
            for part in parts.iter().rev() {
                if let AssistantPart::ToolGroup(group) = part {
                    // Find the last call with this tool name that has a result
                    for call in group.calls.iter().rev() {
                        if call.tool_name == tool_name && call.result_summary.is_some() {
                            return Some(call);
                        }
                    }
                }
            }
        }
        None
    }

    /// Strip the project root prefix from an absolute path, returning a relative path.
    /// Returns the input unchanged if it doesn't start with the project root.
    /// Only strips at path boundaries (e.g., `/foo/bar` won't match `/foo/bar-baz`).
    fn strip_project_root(&self, path: &str) -> String {
        let root = self.project.root.to_string_lossy();
        let root_str = root.as_ref();
        if let Some(rest) = path.strip_prefix(root_str) {
            if let Some(relative) = rest.strip_prefix('/') {
                relative.to_string()
            } else if rest.is_empty() {
                String::new()
            } else {
                // Did not match at a path boundary (e.g., sibling directory)
                path.to_string()
            }
        } else {
            path.to_string()
        }
    }

    /// Sync the permission engine rules with the current agent mode.
    fn sync_permission_mode(&self) {
        use crate::ui::input::AgentMode;
        use crate::permission::{PermissionProfile, profile_build_rules, profile_plan_rules};

        let profile = self.config.permission_profile.unwrap_or(PermissionProfile::Standard);
        let allow_overrides: Vec<ToolName> = self.config.allow_tools.iter()
            .filter_map(|s| s.parse::<ToolName>().ok())
            .collect();

        let path_rules = &self.config.permission_rules;
        let rules = match self.input.mode {
            AgentMode::Build => profile_build_rules(profile, &allow_overrides, path_rules),
            AgentMode::Plan => profile_plan_rules(profile, &allow_overrides, path_rules),
        };

        // Spawn a task to update the engine since it requires async lock
        let engine = self.permission_engine.clone();
        tokio::spawn(async move {
            let mut engine = engine.lock().await;
            engine.set_rules(rules);
        });
    }

    /// Persist a tool grant to the project config and update in-memory config.
    fn persist_tool_grant(&mut self, tool_name: &str) {
        // Update in-memory config
        if !self.config.allow_tools.contains(&tool_name.to_string()) {
            self.config.allow_tools.push(tool_name.to_string());
            // Re-sync permission rules with updated config
            self.sync_permission_mode();
        }

        // Persist to disk (fire-and-forget — don't block the UI)
        let project_root = self.project.root.clone();
        let tool = tool_name.to_string();
        std::thread::spawn(move || {
            if let Err(e) = crate::config::persist_allow_tool(&project_root, &tool) {
                tracing::warn!("failed to persist tool grant: {e}");
            }
        });
    }

    fn build_system_prompt(&self) -> Option<String> {
        use crate::ui::input::AgentMode;

        let mut parts: Vec<String> = Vec::new();

        // Identity and environment context
        let model_name = self.current_model.as_deref().unwrap_or("unknown");
        let git_branch = self.sidebar_state.git_branch.clone();
        let mode_name = if self.input.mode == AgentMode::Plan { "Plan" } else { "Build" };

        let mut identity = format!(
            "You are Steve, a TUI AI coding agent. You help users understand, modify, and build software \
            by reading files, searching code, making edits, and running commands — all within this terminal interface.\n\n\
            ## Environment\n\
            - **Project root**: {}\n\
            - **Model**: {model_name}\n\
            - **Mode**: {mode_name}\n\
            - **Date**: {}",
            self.project.root.display(),
            chrono::Local::now().format("%A, %B %-d, %Y at %-I:%M %p")
        );
        if let Some(branch) = git_branch {
            identity.push_str(&format!("\n- **Git branch**: {branch}"));
        }

        identity.push_str("\n\n\
            ## How You Work\n\
            - You can only access files within the project root. All paths are resolved relative to it.\n\
            - **Build mode**: Read tools are auto-approved. Write tools (edit, write, patch) and bash require user permission.\n\
            - **Plan mode**: Read-only. Write tools are unavailable. Use this for analysis and planning.\n\
            - The user sees your tool calls and results in the TUI. Be concise — tool output consumes context window space.\n\
            - When context runs low, the conversation may be automatically compacted into a summary.\n\
            - Use the `memory` tool to persist important discoveries across sessions.");

        parts.push(identity);

        parts.push(TOOL_GUIDANCE.to_string());

        // Load project memory if it exists (with shared lock for safe concurrent access)
        let memory_path = self.storage.base_dir().join("memory.md");
        if let Ok(memory) = Self::read_memory_file(&memory_path) {
            if !memory.trim().is_empty() {
                let truncated = if memory.len() > 2000 {
                    let mut end = 2000;
                    while end > 0 && !memory.is_char_boundary(end) {
                        end -= 1;
                    }
                    format!(
                        "{}...\n(use memory tool to read full content)",
                        &memory[..end]
                    )
                } else {
                    memory
                };
                parts.push(format!("\n## Project Memory\n\n{truncated}"));
            }
        }

        if let Some(agents_md) = &self.agents_md {
            parts.push(format!("\n---\n\n{agents_md}"));
        }

        if self.input.mode == AgentMode::Plan {
            parts.push("\n---\n\nYou are currently in PLAN mode. You can read files and analyze the codebase, but you CANNOT write, edit, patch, or create files. Focus on planning, analysis, and providing recommendations. If the user asks you to make changes, explain what you would do but note that the user must switch to BUILD mode (via the Tab key) before changes can be applied.".to_string());
        }

        Some(parts.join("\n"))
    }

    /// Read the memory file with a shared lock for safe concurrent access.
    fn read_memory_file(path: &std::path::Path) -> Result<String, std::io::Error> {
        use std::io::Read;
        let file = std::fs::File::open(path)?;
        file.lock_shared()?;
        let mut content = String::new();
        (&file).read_to_string(&mut content)?;
        let _ = file.unlock();
        Ok(content)
    }

    /// Determine which model ref to use for compaction/summarization.
    /// Prefers small_model if configured, falls back to current_model.
    fn compact_model_ref(&self) -> Option<String> {
        self.config
            .small_model
            .clone()
            .or_else(|| self.current_model.clone())
    }

    /// Validate a model ref from a saved session against the current provider
    /// registry. If it is no longer valid (e.g. the config was updated after
    /// the session was saved), log a warning and fall back to the model
    /// specified in the current config.
    fn validated_model_ref(&self, model_ref: &str) -> String {
        let Some(registry) = self.provider_registry.as_ref() else {
            // No registry to validate against — keep the stored model_ref as-is.
            return model_ref.to_string();
        };
        if registry.resolve_model(model_ref).is_ok() {
            return model_ref.to_string();
        }
        let fallback = self
            .config
            .model
            .clone()
            .unwrap_or_else(|| model_ref.to_string());
        tracing::warn!(
            "session model_ref '{}' is no longer valid, falling back to '{}'",
            model_ref,
            fallback
        );
        fallback
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
    fn check_context_warning(&mut self) {
        if self.context_warned {
            return;
        }
        let context_window = self.status_line_state.context_window;
        let prompt_tokens = self.status_line_state.last_prompt_tokens;
        if context_window == 0 {
            return;
        }
        let threshold = (context_window as f64 * 0.60) as u64;
        if prompt_tokens >= threshold {
            self.context_warned = true;
            let pct = self.status_line_state.context_usage_pct();
            self.messages.push(MessageBlock::System {
                text: format!(
                    "Context window {}% full ({}/{}). Consider /compact to free space.",
                    pct,
                    crate::ui::status_line::format_tokens(prompt_tokens),
                    crate::ui::status_line::format_tokens(context_window),
                ),
            });
            self.message_area_state.scroll_to_bottom();
        }
    }

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

        if self.current_session.is_none() {
            return false;
        }

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

        self.last_prompt_tokens >= threshold
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
                // Cancel any active stream before pruning/resetting
                self.cancel_stream();
                // Prune the old session if it had no user messages
                self.prune_empty_session();
                // Create a fresh session
                self.messages.clear();
                self.stored_messages.clear();
                self.streaming_message = None;
                self.streaming_active = false;
                self.is_loading = false;
                self.exchange_count = 0;
                self.auto_compact_failed = false;
                self.context_warned = false;
                self.last_prompt_tokens = 0;
                self.current_session = None;
                self.session_browse_list = None;
                // Reset tool result cache for the new session
                *self.tool_cache.lock().unwrap() = ToolResultCache::new(self.project.root.clone());
                // Clear changeset tracking, todos, selection, and reset token counters
                self.sidebar_state.changes.clear();
                crate::tool::todo::clear_todos();
                self.selection_state.clear();
                self.ensure_session();
                self.refresh_git_info();
                self.sync_sidebar_tokens();
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
                                format!(
                                    "  {} \u{2014} {}{}",
                                    m.display_ref(),
                                    m.config.name,
                                    marker
                                )
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
            Command::Sessions => {
                if self.is_loading || self.streaming_active {
                    self.messages.push(MessageBlock::Error {
                        text: "Cannot browse sessions while streaming.".to_string(),
                    });
                    return Ok(());
                }
                let mgr = SessionManager::new(&self.storage, &self.project.id);
                match mgr.list_sessions() {
                    Ok(sessions) if sessions.is_empty() => {
                        self.messages.push(MessageBlock::System {
                            text: "No sessions found.".to_string(),
                        });
                    }
                    Ok(sessions) => {
                        let display_sessions: Vec<_> = sessions.into_iter().take(20).collect();
                        let mut list = String::from("Sessions (enter number to switch):\n");
                        for (i, s) in display_sessions.iter().enumerate() {
                            let current_marker =
                                self.current_session.as_ref().is_some_and(|c| c.id == s.id);
                            let marker = if current_marker { " *" } else { "" };
                            let date = s.updated_at.format("%m/%d %H:%M");
                            list.push_str(&format!(
                                "  {:>2}. {} \u{2014} {}{}\n",
                                i + 1,
                                date,
                                s.title,
                                marker
                            ));
                        }
                        self.messages.push(MessageBlock::System { text: list });
                        self.session_browse_list = Some(display_sessions);
                        self.message_area_state.scroll_to_bottom();
                    }
                    Err(e) => {
                        self.messages.push(MessageBlock::Error {
                            text: format!("Failed to list sessions: {e}"),
                        });
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
            Command::ExportDebug => {
                let include_logs = true;
                if self.current_session.is_none() || self.stored_messages.is_empty() {
                    self.messages.push(MessageBlock::Error {
                        text: "No active session to export.".to_string(),
                    });
                } else {
                    let session = self.current_session.as_ref().unwrap();
                    let system_prompt = self.build_system_prompt();
                    let model_ref = self.current_model.as_deref();
                    let params = crate::export::ExportParams {
                        session_id: &session.id,
                        session_title: &session.title,
                        session_created_at: session.created_at,
                        token_usage: &session.token_usage,
                        messages: &self.stored_messages,
                        system_prompt,
                        model_ref,
                        project_root: &self.project.root,
                        include_logs,
                    };
                    match crate::export::export_debug(&params) {
                        Ok(path) => {
                            let display = self.strip_project_root(&path.to_string_lossy());
                            self.messages.push(MessageBlock::System {
                                text: format!("Debug export written to: {display}"),
                            });
                        }
                        Err(e) => {
                            self.messages.push(MessageBlock::Error {
                                text: format!("Export failed: {e}"),
                            });
                        }
                    }
                }
            }
            Command::Help => {
                self.messages.push(MessageBlock::System {
                    text: "Commands:\n  /new            \u{2014} Start a new session\n  /rename <t>     \u{2014} Rename current session\n  /models         \u{2014} List available models\n  /model <r>      \u{2014} Switch to a model\n  /compact        \u{2014} Compact conversation into a summary\n  /sessions       \u{2014} Browse sessions\n  /export-debug   \u{2014} Export session with logs\n  /init           \u{2014} Create AGENTS.md in project root\n  /help           \u{2014} Show this help\n  /exit           \u{2014} Quit\n\nKeys:\n  Enter       \u{2014} Send message\n  Shift+Enter \u{2014} Insert newline\n  Tab         \u{2014} Accept autocomplete / toggle Build\u{2013}Plan mode\n  Up/Down     \u{2014} Navigate autocomplete list\n  Ctrl+C      \u{2014} Cancel stream / quit\n  Ctrl+B      \u{2014} Toggle sidebar\n  Mouse wheel \u{2014} Scroll messages\n  Click+drag  \u{2014} Select text (auto-copies to clipboard)".to_string(),
                });
            }
        }

        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
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
        let args = json!({"file_path": "src/lib.rs", "old_string": "x", "new_string": "y"});
        assert_eq!(extract_args_summary(ToolName::Edit, &args), "src/lib.rs");
    }

    #[test]
    fn extract_args_summary_write_path() {
        let args = json!({"file_path": "new_file.txt", "content": "hello"});
        assert_eq!(extract_args_summary(ToolName::Write, &args), "new_file.txt");
    }

    #[test]
    fn extract_args_summary_patch_path() {
        let args = json!({"file_path": "src/app.rs", "diff": "..."});
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
        assert_eq!(
            extract_args_summary(ToolName::Question, &args),
            "What is this?"
        );
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
        assert_eq!(
            extract_args_summary(ToolName::Webfetch, &args),
            "https://example.com"
        );
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
        assert_eq!(extract_args_summary(ToolName::Memory, &args), "");
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
            ToolName::Memory,
        ];
        for tool in all_tools {
            // Just ensure it doesn't panic
            let _ = extract_args_summary(tool, &args);
        }
    }

    // -- extract_diff_content tests --

    #[test]
    fn diff_content_edit_basic() {
        let args = json!({
            "file_path": "src/main.rs",
            "old_string": "use std::collections::HashMap;",
            "new_string": "use std::collections::BTreeMap;"
        });
        let result = extract_diff_content(ToolName::Edit, &args);
        match result {
            Some(DiffContent::EditDiff { lines }) => {
                assert_eq!(lines.len(), 2);
                assert_eq!(
                    lines[0],
                    DiffLine::Removal("use std::collections::HashMap;".into())
                );
                assert_eq!(
                    lines[1],
                    DiffLine::Addition("use std::collections::BTreeMap;".into())
                );
            }
            other => panic!("expected EditDiff, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_edit_multiline() {
        let args = json!({
            "file_path": "f.rs",
            "old_string": "line1\nline2",
            "new_string": "new1\nnew2\nnew3"
        });
        match extract_diff_content(ToolName::Edit, &args) {
            Some(DiffContent::EditDiff { lines }) => {
                assert_eq!(lines.len(), 5);
                assert_eq!(lines[0], DiffLine::Removal("line1".into()));
                assert_eq!(lines[1], DiffLine::Removal("line2".into()));
                assert_eq!(lines[2], DiffLine::Addition("new1".into()));
                assert_eq!(lines[3], DiffLine::Addition("new2".into()));
                assert_eq!(lines[4], DiffLine::Addition("new3".into()));
            }
            other => panic!("expected EditDiff, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_edit_empty_strings_returns_none() {
        let args = json!({"file_path": "f.rs", "old_string": "", "new_string": ""});
        assert!(extract_diff_content(ToolName::Edit, &args).is_none());
    }

    #[test]
    fn diff_content_edit_missing_args_returns_none() {
        let args = json!({"file_path": "f.rs"});
        assert!(extract_diff_content(ToolName::Edit, &args).is_none());
    }

    #[test]
    fn diff_content_write_basic() {
        let args = json!({"file_path": "new.txt", "content": "line1\nline2\nline3"});
        match extract_diff_content(ToolName::Write, &args) {
            Some(DiffContent::WriteSummary { line_count }) => {
                assert_eq!(line_count, 3);
            }
            other => panic!("expected WriteSummary, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_write_empty_content() {
        let args = json!({"file_path": "empty.txt", "content": ""});
        match extract_diff_content(ToolName::Write, &args) {
            Some(DiffContent::WriteSummary { line_count }) => {
                assert_eq!(line_count, 0);
            }
            other => panic!("expected WriteSummary, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_write_missing_content() {
        let args = json!({"file_path": "f.txt"});
        match extract_diff_content(ToolName::Write, &args) {
            Some(DiffContent::WriteSummary { line_count }) => {
                assert_eq!(line_count, 0);
            }
            other => panic!("expected WriteSummary, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_patch_basic() {
        let diff_str = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,3 @@\n context\n-old line\n+new line\n context2";
        let args = json!({"file_path": "src/main.rs", "diff": diff_str});
        match extract_diff_content(ToolName::Patch, &args) {
            Some(DiffContent::PatchDiff { lines }) => {
                assert_eq!(lines.len(), 5);
                assert_eq!(lines[0], DiffLine::HunkHeader("@@ -1,3 +1,3 @@".into()));
                assert_eq!(lines[1], DiffLine::Context("context".into()));
                assert_eq!(lines[2], DiffLine::Removal("old line".into()));
                assert_eq!(lines[3], DiffLine::Addition("new line".into()));
                assert_eq!(lines[4], DiffLine::Context("context2".into()));
            }
            other => panic!("expected PatchDiff, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_patch_empty_returns_none() {
        let args = json!({"file_path": "f.rs", "diff": ""});
        assert!(extract_diff_content(ToolName::Patch, &args).is_none());
    }

    #[test]
    fn diff_content_non_write_tools_return_none() {
        let args = json!({"path": "src/main.rs"});
        for tool in [
            ToolName::Read,
            ToolName::Grep,
            ToolName::Glob,
            ToolName::List,
            ToolName::Bash,
            ToolName::Question,
            ToolName::Todo,
            ToolName::Webfetch,
            ToolName::Memory,
        ] {
            assert!(
                extract_diff_content(tool, &args).is_none(),
                "{tool} should return None"
            );
        }
    }

    #[test]
    fn diff_content_all_variants_covered() {
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
            ToolName::Memory,
        ];
        for tool in all_tools {
            let result = extract_diff_content(tool, &args);
            // Write tools produce diff content; all others return None.
            // Empty args produce None for write tools too, but the exhaustive
            // match is the point — a new variant without a match arm won't compile.
            if matches!(tool, ToolName::Edit | ToolName::Write | ToolName::Patch) {
                // With empty args, write tools may return None (no old_string etc.)
                // — the key assertion is that this doesn't panic.
                let _ = result;
            } else {
                assert!(
                    result.is_none(),
                    "{tool} should return None for diff content"
                );
            }
        }
    }

    // -- parse_unified_diff_lines tests --

    #[test]
    fn parse_diff_skips_file_headers() {
        let patch = "--- a/file.rs\n+++ b/file.rs\n@@ -1 +1 @@\n-old\n+new";
        let lines = parse_unified_diff_lines(patch);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], DiffLine::HunkHeader("@@ -1 +1 @@".into()));
        assert_eq!(lines[1], DiffLine::Removal("old".into()));
        assert_eq!(lines[2], DiffLine::Addition("new".into()));
    }

    #[test]
    fn parse_diff_context_lines() {
        let patch = "@@ -1,3 +1,3 @@\n unchanged\n-removed\n+added\n still here";
        let lines = parse_unified_diff_lines(patch);
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0], DiffLine::HunkHeader("@@ -1,3 +1,3 @@".into()));
        assert_eq!(lines[1], DiffLine::Context("unchanged".into()));
        assert_eq!(lines[2], DiffLine::Removal("removed".into()));
        assert_eq!(lines[3], DiffLine::Addition("added".into()));
        assert_eq!(lines[4], DiffLine::Context("still here".into()));
    }

    #[test]
    fn parse_diff_empty_string() {
        let lines = parse_unified_diff_lines("");
        assert!(lines.is_empty());
    }

    #[test]
    fn system_prompt_includes_tool_guidance() {
        let app = make_test_app();
        let prompt = app.build_system_prompt().unwrap();
        assert!(
            prompt.contains("Search before reading"),
            "should contain search guidance"
        );
        assert!(prompt.contains("offset"), "should mention offset param");
        assert!(
            prompt.contains("context-efficient"),
            "should mention context efficiency"
        );
        assert!(
            prompt.contains("You are Steve"),
            "should contain Steve identity"
        );
        assert!(
            prompt.contains("Build mode"),
            "should explain permission model"
        );
        assert!(
            prompt.contains("Date"),
            "should contain current date"
        );
    }

    #[test]
    fn should_show_sidebar_auto_mode() {
        // In auto mode (None), sidebar shows at >= 120 width
        let app = make_test_app();
        assert!(app.should_show_sidebar(120));
        assert!(app.should_show_sidebar(200));
        assert!(!app.should_show_sidebar(119));
        assert!(!app.should_show_sidebar(80));
    }

    #[test]
    fn should_show_sidebar_forced_show() {
        let mut app = make_test_app();
        app.sidebar_override = Some(true);
        // Force show regardless of width
        assert!(app.should_show_sidebar(80));
        assert!(app.should_show_sidebar(120));
    }

    #[test]
    fn should_show_sidebar_forced_hide() {
        let mut app = make_test_app();
        app.sidebar_override = Some(false);
        // Force hide regardless of width
        assert!(!app.should_show_sidebar(120));
        assert!(!app.should_show_sidebar(200));
    }

    #[test]
    fn check_context_warning_fires_at_60_pct() {
        let mut app = make_test_app();
        app.context_warned = false;
        app.status_line_state.context_window = 128_000;
        app.last_prompt_tokens = 80_000; // ~62%
        app.status_line_state.last_prompt_tokens = 80_000;
        app.check_context_warning();
        assert!(app.context_warned);
        assert!(app.messages.iter().any(|m| {
            matches!(m, MessageBlock::System { text } if text.contains("Context window"))
        }));
    }

    #[test]
    fn check_context_warning_only_fires_once() {
        let mut app = make_test_app();
        app.context_warned = false;
        app.status_line_state.context_window = 128_000;
        app.last_prompt_tokens = 80_000;
        app.status_line_state.last_prompt_tokens = 80_000;
        app.check_context_warning();
        let msg_count = app.messages.len();
        app.check_context_warning(); // second call
        assert_eq!(app.messages.len(), msg_count); // no new message
    }

    #[test]
    fn check_context_warning_does_not_fire_below_threshold() {
        let mut app = make_test_app();
        app.status_line_state.context_window = 128_000;
        app.last_prompt_tokens = 50_000; // ~39%
        app.status_line_state.last_prompt_tokens = 50_000;
        app.check_context_warning();
        assert!(!app.context_warned);
    }

    #[test]
    fn llm_usage_update_sets_prompt_tokens_without_session_storage() {
        let mut app = make_test_app();
        app.status_line_state.context_window = 128_000;
        app.last_prompt_tokens = 0;
        app.status_line_state.last_prompt_tokens = 0;

        // Simulate the LlmUsageUpdate handler logic
        let usage = crate::event::StreamUsage {
            prompt_tokens: 60_000,
            completion_tokens: 500,
            total_tokens: 60_500,
        };
        app.last_prompt_tokens = usage.prompt_tokens as u64;
        app.status_line_state.last_prompt_tokens = usage.prompt_tokens as u64;
        app.check_context_warning();

        assert_eq!(app.last_prompt_tokens, 60_000);
        assert_eq!(app.status_line_state.last_prompt_tokens, 60_000);
        // Session storage should remain untouched
        assert!(app.current_session.is_none());
    }

    #[test]
    fn scroll_down_event_scrolls_down() {
        let mut state = crate::ui::message_area::MessageAreaState::default();
        state.update_dimensions(500, 100);
        state.scroll_to_bottom();
        state.scroll_up(10);
        let after_up = state.scroll_offset;
        state.scroll_down(3);
        assert!(
            state.scroll_offset > after_up,
            "scroll_down should increase offset"
        );
    }

    #[test]
    fn keyboard_scroll_up_down() {
        let mut state = crate::ui::message_area::MessageAreaState::default();
        state.update_dimensions(500, 100);
        state.scroll_to_bottom(); // offset = 400
        assert!(state.auto_scroll);
        state.scroll_up(1);
        assert_eq!(state.scroll_offset, 399);
        assert!(!state.auto_scroll, "scrolling up should disable auto_scroll");
        state.scroll_down(1);
        assert_eq!(state.scroll_offset, 400);
        assert!(state.auto_scroll, "returning to bottom should re-enable auto_scroll");
    }

    #[test]
    fn keyboard_page_scroll() {
        let mut state = crate::ui::message_area::MessageAreaState::default();
        state.update_dimensions(500, 100);
        state.scroll_to_bottom(); // offset = 400
        assert!(state.auto_scroll);
        let page = state.visible_height(); // 100
        state.scroll_up(page);
        assert_eq!(state.scroll_offset, 300);
        assert!(!state.auto_scroll, "page up should disable auto_scroll");
        state.scroll_down(page);
        assert_eq!(state.scroll_offset, 400);
        assert!(state.auto_scroll, "page down to bottom should re-enable auto_scroll");
    }

    // -- strip_project_root tests --

    #[test]
    fn strip_project_root_absolute_path() {
        let app = make_test_app();
        assert_eq!(
            app.strip_project_root("/tmp/test/src/main.rs"),
            "src/main.rs"
        );
    }

    #[test]
    fn strip_project_root_relative_path() {
        let app = make_test_app();
        assert_eq!(app.strip_project_root("src/main.rs"), "src/main.rs");
    }

    #[test]
    fn strip_project_root_no_match() {
        let app = make_test_app();
        assert_eq!(
            app.strip_project_root("/other/path/file.rs"),
            "/other/path/file.rs"
        );
    }

    #[test]
    fn strip_project_root_exact_root() {
        let app = make_test_app();
        // Edge case: path is exactly the root with trailing slash
        assert_eq!(app.strip_project_root("/tmp/test/"), "");
    }

    #[test]
    fn strip_project_root_sibling_directory() {
        let app = make_test_app();
        // Should NOT strip prefix from sibling directory (not a path boundary)
        assert_eq!(
            app.strip_project_root("/tmp/test-backup/file.rs"),
            "/tmp/test-backup/file.rs"
        );
    }

    /// Create a minimal App for testing (without real storage/config).
    /// Note: uses `Storage::new` which writes to the real app data dir. This is
    /// acceptable because UI rendering tests don't perform storage writes. A
    /// temp-dir approach would require returning `TempDir` to keep it alive.
    pub(crate) fn make_test_app() -> App {
        use crate::config::types::Config;
        use crate::project::ProjectInfo;
        use crate::storage::Storage;
        use std::path::PathBuf;

        let project = ProjectInfo {
            root: PathBuf::from("/tmp/test"),
            id: "test".to_string(),
        };
        let config = Config::default();
        let storage = Storage::new("test-sidebar").expect("test storage");
        App::new(project, config, storage, None, None, None, Vec::new())
    }

    // -- interjection tests --

    #[test]
    fn handle_interjection_adds_user_message() {
        let mut app = make_test_app();
        let (interjection_tx, _rx) = mpsc::unbounded_channel();
        app.interjection_tx = Some(interjection_tx);

        let initial_count = app.messages.len();
        app.handle_interjection("focus on tests".to_string());

        assert_eq!(app.messages.len(), initial_count + 1);
        match &app.messages[initial_count] {
            MessageBlock::User { text } => {
                assert_eq!(text, "focus on tests");
            }
            other => panic!("expected User message, got {:?}", other),
        }
    }

    #[test]
    fn handle_interjection_rejects_commands() {
        let mut app = make_test_app();
        let (interjection_tx, _rx) = mpsc::unbounded_channel();
        app.interjection_tx = Some(interjection_tx);

        let initial_count = app.messages.len();
        app.handle_interjection("/compact".to_string());

        // No message should be added
        assert_eq!(app.messages.len(), initial_count);
    }

    #[test]
    fn handle_interjection_noop_when_no_sender() {
        let mut app = make_test_app();
        assert!(app.interjection_tx.is_none());

        let initial_count = app.messages.len();
        app.handle_interjection("hello".to_string());

        // Should silently do nothing
        assert_eq!(app.messages.len(), initial_count);
    }
}
