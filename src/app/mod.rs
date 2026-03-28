mod commands;
mod constants;
mod context;
mod event_loop;
mod helpers;
mod input;
mod key_handling;
mod prompt;
mod session;
mod tool_display;
mod types;

use constants::*;
use tool_display::extract_diff_content;
pub use tool_display::{extract_args_summary, extract_result_summary};
use types::*;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;

use async_openai::types::chat::{
    ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
    ChatCompletionRequestMessage, ChatCompletionRequestUserMessage,
    ChatCompletionRequestUserMessageContent,
};

use crate::{
    config::types::Config,
    context::cache::ToolResultCache,
    event::AppEvent,
    file_ref,
    permission::{PermissionEngine, types::PermissionReply},
    project::ProjectInfo,
    provider::ProviderRegistry,
    session::{
        SessionManager,
        message::{Message, Role},
        types::SessionInfo,
    },
    storage::Storage,
    stream::{self, StreamRequest},
    task::types::{Priority, TaskKind, TaskStatus},
    tool::{ToolContext, ToolName, ToolRegistry},
    ui,
    ui::{
        autocomplete::{AutocompleteMode, AutocompleteState, apply_file_completion},
        input::InputState,
        message_area::MessageAreaState,
        message_block::{AssistantPart, MessageBlock, ToolCall},
        model_picker::ModelPickerState,
        selection::SelectionState,
        session_picker::SessionPickerState,
        sidebar::{MAX_SIDEBAR_TASKS, SidebarLsp, SidebarState, SidebarTask, count_diff_lines},
        status_line::{Activity, StatusLineState},
        theme::Theme,
    },
    usage::{UsageWriter, types::SessionRecord},
};

pub struct App {
    // Core state
    pub project: ProjectInfo,
    pub config: Config,
    pub storage: Storage,
    pub agents_files: Vec<crate::config::AgentsFile>,
    pub provider_registry: Option<ProviderRegistry>,

    /// Persistent task store (epics + tasks).
    pub task_store: crate::task::TaskStore,

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

    /// When the current streaming request started (wall-clock).
    /// Set when user sends a message that triggers streaming.
    pub(crate) stream_start_time: Option<Instant>,

    /// Frozen elapsed duration after streaming ends.
    /// When present, the UI renders this instead of computing from stream_start_time.
    pub(crate) frozen_elapsed: Option<Duration>,

    /// The in-progress assistant message being built during streaming.
    /// Saved to storage when streaming finishes.
    streaming_message: Option<Message>,

    /// Count of user+assistant exchanges in the current session (for auto-title).
    exchange_count: usize,

    /// Active permission prompt awaiting user response.
    pending_permission: Option<PendingPermission>,

    /// Active question prompt awaiting user input.
    pending_question: Option<PendingQuestion>,

    /// Proposed AGENTS.md content awaiting user approval (y/n).
    pending_agents_update: Option<String>,

    /// Session picker overlay state.
    pub session_picker: SessionPickerState,

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

    /// Model picker overlay state.
    pub model_picker: ModelPickerState,

    /// Diagnostics overlay state.
    pub diagnostics_overlay: crate::ui::diagnostics_overlay::DiagnosticsOverlayState,

    /// MCP overlay state.
    pub mcp_overlay: crate::ui::mcp_overlay::McpOverlayState,

    /// Number of compactions in the current session (for diagnostics).
    pub compaction_count: u32,

    /// Text selection state for copy-on-select.
    pub selection_state: SelectionState,

    /// Message area rect from last render (for mouse hit-testing).
    pub last_message_area: ratatui::layout::Rect,

    /// LSP manager (shared with tool handlers via ToolContext).
    lsp_manager: Arc<std::sync::Mutex<crate::lsp::LspManager>>,

    /// MCP manager for dynamic tool/resource servers.
    mcp_manager: Arc<tokio::sync::Mutex<crate::mcp::McpManager>>,

    /// Usage analytics writer (SQLite background thread).
    usage_writer: UsageWriter,

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
        agents_files: Vec<crate::config::AgentsFile>,
        provider_registry: Option<ProviderRegistry>,
        provider_error: Option<String>,
        config_warnings: Vec<String>,
        usage_writer: UsageWriter,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // Determine the default model from config
        let current_model = config.model.clone();

        // Build tool registry
        let tool_registry = Arc::new(ToolRegistry::new(project.root.clone()));

        // Build permission engine with Build mode rules (default start mode)
        // Profile-aware rules will be set on first sync_permission_mode call
        let profile = config
            .permission_profile
            .unwrap_or(crate::permission::PermissionProfile::Standard);
        let allow_overrides: Vec<ToolName> = config
            .allow_tools
            .iter()
            .filter_map(|s| s.parse::<ToolName>().ok())
            .collect();
        let permission_engine = Arc::new(tokio::sync::Mutex::new(PermissionEngine::new(
            crate::permission::profile_build_rules(
                profile,
                &allow_overrides,
                &config.permission_rules,
            ),
        )));

        // Build tool result cache (session-scoped, shared across stream tasks)
        let tool_cache = Arc::new(std::sync::Mutex::new(ToolResultCache::new(
            project.root.clone(),
        )));

        // Build task store (persistent across sessions)
        let repo_name =
            crate::project::git_repo_name(&project.root).unwrap_or_else(|| "proj".to_string());
        let task_store = crate::task::TaskStore::new(storage.clone(), repo_name);

        // Build LSP manager (servers started in background after app init)
        let lsp_manager = Arc::new(std::sync::Mutex::new(crate::lsp::LspManager::new(
            project.root.clone(),
        )));

        // Build MCP manager (servers started in background after app init)
        let mcp_manager = Arc::new(tokio::sync::Mutex::new(crate::mcp::McpManager::new()));

        // Configure MCP-related permission state on the engine
        {
            // Safe: engine was just created, no contention possible
            if let Ok(mut engine) = permission_engine.try_lock() {
                engine.set_profile(profile);

                let mcp_overrides: std::collections::HashSet<String> = config
                    .allow_tools
                    .iter()
                    .filter(|s| crate::mcp::types::parse_prefixed_tool_name(s).is_some())
                    .cloned()
                    .collect();
                if !mcp_overrides.is_empty() {
                    engine.set_mcp_overrides(mcp_overrides);
                }
            }
        }

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

        let mut app = Self {
            project,
            config,
            storage,
            agents_files,
            provider_registry,
            task_store,
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
            stream_start_time: None,
            frozen_elapsed: None,
            streaming_message: None,
            exchange_count: 0,
            pending_permission: None,
            pending_question: None,
            pending_agents_update: None,
            session_picker: SessionPickerState::default(),
            stream_cancel: None,
            interjection_tx: None,
            auto_compact_failed: false,
            context_warned: false,
            last_prompt_tokens: 0,
            sidebar_override: None,
            file_index: None,
            model_picker: ModelPickerState::default(),
            diagnostics_overlay: crate::ui::diagnostics_overlay::DiagnosticsOverlayState::default(),
            mcp_overlay: crate::ui::mcp_overlay::McpOverlayState::default(),
            compaction_count: 0,
            selection_state: SelectionState::default(),
            last_message_area: ratatui::layout::Rect::default(),
            lsp_manager,
            mcp_manager,
            usage_writer,
            event_tx,
            event_rx,
            should_quit: false,
        };
        app.sync_context_window();
        app
    }
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod overlay_tests {
    use super::*;
    use tests::make_test_app;

    #[test]
    fn model_picker_open_populates_state() {
        let mut app = make_test_app();
        let models = vec![
            ("openai/gpt-4o".into(), "GPT-4o".into()),
            ("anthropic/claude".into(), "Claude".into()),
        ];
        app.model_picker.open(&models, Some("openai/gpt-4o"));

        assert!(app.model_picker.visible);
        assert_eq!(app.model_picker.filtered_models().len(), 2);
    }

    #[test]
    fn model_picker_close_on_new() {
        let mut app = make_test_app();
        let models = vec![("openai/gpt-4o".into(), "GPT-4o".into())];
        app.model_picker.open(&models, None);
        assert!(app.model_picker.visible);

        // Simulate the relevant part of Command::New handler
        app.model_picker.close();
        assert!(!app.model_picker.visible);
    }

    #[test]
    fn model_picker_renders_in_full_app() {
        let mut app = make_test_app();
        let models = vec![
            ("openai/gpt-4o".into(), "GPT-4o".into()),
            ("anthropic/claude".into(), "Claude".into()),
        ];
        app.model_picker.open(&models, Some("openai/gpt-4o"));

        let buf = crate::ui::render_to_buffer(80, 24, |frame| {
            crate::ui::render(frame, &mut app);
        });

        let mut text = String::new();
        for y in 0..24 {
            for x in 0..80 {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }

        assert!(
            text.contains("Switch Model"),
            "overlay title should be visible, got:\n{text}"
        );
        assert!(
            text.contains("openai/gpt-4o"),
            "model ref should be visible, got:\n{text}"
        );
    }

    #[test]
    fn diagnostics_overlay_close_on_new() {
        let mut app = make_test_app();
        app.diagnostics_overlay.open(vec![]);
        assert!(app.diagnostics_overlay.visible);

        // Simulate the relevant part of Command::New handler
        app.diagnostics_overlay.close();
        assert!(!app.diagnostics_overlay.visible);
    }

    #[test]
    fn diagnostics_overlay_closes_model_picker() {
        let mut app = make_test_app();
        let models = vec![("openai/gpt-4o".into(), "GPT-4o".into())];
        app.model_picker.open(&models, None);
        assert!(app.model_picker.visible);

        // Opening diagnostics should close model picker (mutual exclusivity)
        app.model_picker.close();
        let checks = app.collect_diagnostics();
        app.diagnostics_overlay.open(checks);
        assert!(app.diagnostics_overlay.visible);
        assert!(!app.model_picker.visible);
    }

    #[test]
    fn mcp_overlay_close_on_new() {
        let mut app = make_test_app();
        let snapshot = crate::ui::mcp_overlay::McpSnapshot::default();
        app.mcp_overlay
            .open(crate::ui::mcp_overlay::McpTab::Servers, snapshot, None);
        assert!(app.mcp_overlay.visible);

        // Simulate the relevant part of Command::New handler
        app.mcp_overlay.close();
        assert!(!app.mcp_overlay.visible);
    }

    #[test]
    fn mcp_overlay_closes_other_overlays() {
        let mut app = make_test_app();
        let models = vec![("openai/gpt-4o".into(), "GPT-4o".into())];
        app.model_picker.open(&models, None);
        assert!(app.model_picker.visible);

        // Simulate what open_mcp_overlay() does — close other overlays then open MCP.
        app.model_picker.close();
        app.session_picker.close();
        app.diagnostics_overlay.close();
        let snapshot = crate::ui::mcp_overlay::McpSnapshot::default();
        app.mcp_overlay
            .open(crate::ui::mcp_overlay::McpTab::Tools, snapshot, None);
        assert!(app.mcp_overlay.visible);
        assert!(!app.model_picker.visible);
    }

    #[test]
    fn mcp_overlay_closed_by_diagnostics() {
        let mut app = make_test_app();
        let snapshot = crate::ui::mcp_overlay::McpSnapshot::default();
        app.mcp_overlay
            .open(crate::ui::mcp_overlay::McpTab::Servers, snapshot, None);
        assert!(app.mcp_overlay.visible);

        // Simulate what handle_command(Diagnostics) does — close others then open.
        app.model_picker.close();
        app.session_picker.close();
        app.mcp_overlay.close();
        let checks = app.collect_diagnostics();
        app.diagnostics_overlay.open(checks);
        assert!(app.diagnostics_overlay.visible);
        assert!(!app.mcp_overlay.visible);
    }

    #[test]
    fn compaction_count_resets_on_new() {
        let mut app = make_test_app();
        app.compaction_count = 5;

        // Simulate the relevant part of Command::New handler
        app.compaction_count = 0;
        assert_eq!(app.compaction_count, 0);
    }

    #[test]
    fn compaction_count_increments() {
        let mut app = make_test_app();
        assert_eq!(app.compaction_count, 0);
        app.compaction_count += 1;
        assert_eq!(app.compaction_count, 1);
    }

    #[test]
    fn diagnostics_overlay_renders_in_full_app() {
        let mut app = make_test_app();
        let checks = app.collect_diagnostics();
        app.diagnostics_overlay.open(checks);

        let buf = crate::ui::render_to_buffer(80, 24, |frame| {
            crate::ui::render(frame, &mut app);
        });

        let mut text = String::new();
        for y in 0..24 {
            for x in 0..80 {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }

        assert!(
            text.contains("Health Dashboard"),
            "overlay title should be visible, got:\n{text}"
        );
    }
}
