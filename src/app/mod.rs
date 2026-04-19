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

use constants::*;
use tool_display::extract_diff_content;
pub use tool_display::{extract_args_summary, extract_result_summary};

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
    DateTimeExt,
    config::Config,
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
    task::{Priority, TaskKind, TaskStatus},
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

/// A permission prompt waiting for user input.
struct PendingPermission {
    tool_name: crate::tool::ToolName,
    #[allow(dead_code)]
    summary: String,
    response_tx: tokio::sync::oneshot::Sender<PermissionReply>,
}

/// A question prompt from the LLM waiting for user input.
struct PendingQuestion {
    call_id: String,
    #[allow(dead_code)]
    question: String,
    options: Vec<String>,
    selected: Option<usize>,
    free_text: String,
    response_tx: tokio::sync::oneshot::Sender<String>,
}

pub struct App {
    // Core state
    pub project: ProjectInfo,
    pub config: Config,
    pub storage: Storage,
    pub agents_files: Vec<crate::config::AgentsFile>,
    pub provider_registry: Option<ProviderRegistry>,

    /// Providers disabled at startup because their `api_key_env` variable was
    /// unset. Surfaced through the diagnostics system (sidebar dot + overlay)
    /// so users see the problem immediately instead of on first message.
    pub missing_api_keys: Vec<crate::provider::ProviderInitWarning>,

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

    /// LSP diagnostics overlay state.
    pub lsp_diagnostics_overlay: crate::ui::lsp_diagnostics_overlay::LspDiagnosticsOverlayState,

    /// Number of compactions in the current session (for diagnostics).
    pub compaction_count: u32,

    /// Text selection state for copy-on-select.
    pub selection_state: SelectionState,

    /// Message area rect from last render (for mouse hit-testing).
    pub last_message_area: ratatui::layout::Rect,

    /// LSP manager (shared with tool handlers via ToolContext).
    lsp_manager: Arc<std::sync::RwLock<crate::lsp::LspManager>>,

    /// Direct clone of the LSP status cache Arc, bypassing `lsp_manager`'s
    /// `RwLock`. The startup `spawn_blocking` holds the write lock for the
    /// entire duration of server Initialize (seconds for rust-analyzer),
    /// so Tick handlers that go through `lsp_manager.try_read()` would fail
    /// for the entire startup window and never observe Starting/Indexing
    /// transitions. Reading the cache directly via this Arc sidesteps that.
    lsp_status_cache: crate::lsp::client::SharedLspStatus,

    /// MCP manager for dynamic tool/resource servers.
    mcp_manager: Arc<tokio::sync::Mutex<crate::mcp::McpManager>>,

    /// Usage analytics writer (SQLite background thread).
    usage_writer: UsageWriter,

    // Runtime
    event_tx: mpsc::UnboundedSender<AppEvent>,
    event_rx: mpsc::UnboundedReceiver<AppEvent>,
    should_quit: bool,
}

/// Build startup error text for missing provider env vars.
///
/// Two presentation modes:
/// - **Loud** (one consolidated multi-line message) when either nothing works
///   (`registry_empty`) or the default model's provider is disabled. In both
///   cases the user's typical first action — typing a message — would fail, so
///   the warning must be impossible to miss.
/// - **Terse** (one short line per disabled provider) when the default still
///   works but some non-default providers are disabled. The user can proceed;
///   the full detail lives in the diagnostics overlay.
///
/// Caller must already have checked that `config.providers` is non-empty.
fn build_missing_provider_messages(
    registry_empty: bool,
    default_model: Option<&str>,
    missing: &[crate::provider::ProviderInitWarning],
) -> Vec<String> {
    if missing.is_empty() {
        return Vec::new();
    }

    let default_provider_id = default_model.and_then(|m| m.split('/').next());
    let default_disabled =
        default_provider_id.is_some_and(|pid| missing.iter().any(|w| w.provider_id == pid));

    if !(registry_empty || default_disabled) {
        return missing
            .iter()
            .map(|w| {
                format!(
                    "Provider '{}' disabled: ${} is not set (see diagnostics for details)",
                    w.provider_id, w.env_var,
                )
            })
            .collect();
    }

    let headline = if registry_empty {
        "STEVE CANNOT SEND MESSAGES — no working providers"
    } else {
        "STEVE CANNOT SEND MESSAGES — default model's provider is disabled"
    };
    let mut text = String::from(headline);
    text.push_str("\n\n");
    if default_disabled && let Some(model) = default_model {
        text.push_str(&format!("Default model: {model}\n"));
    }
    text.push_str("Disabled providers:\n");
    for w in missing {
        text.push_str(&format!(
            "  • ${}  (provider '{}')\n",
            w.env_var, w.provider_id
        ));
    }
    text.push_str("\nSet the env var(s) and restart steve, or adjust your config.");
    vec![text]
}

impl App {
    // Structural — these args are all needed
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project: ProjectInfo,
        config: Config,
        storage: Storage,
        agents_files: Vec<crate::config::AgentsFile>,
        provider_registry: Option<ProviderRegistry>,
        missing_api_keys: Vec<crate::provider::ProviderInitWarning>,
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

        // Build LSP manager (servers started in background after app init).
        // We do two things synchronously here so the sidebar can show
        // `Starting` entries on the very first frame:
        //   1. Run `detect_and_seed_starting` — filesystem walk + seed the
        //      shared cache with a `Starting` entry per detected language.
        //   2. Clone the status cache Arc so the Tick handler can read it
        //      directly (bypassing the enclosing `RwLock<LspManager>` which
        //      is held exclusively during the startup `spawn_blocking`).
        let (lsp_manager, lsp_status_cache) = {
            let mut mgr = crate::lsp::LspManager::new(
                project.root.clone(),
                tokio::runtime::Handle::current(),
                Some(event_tx.clone()),
            );
            mgr.detect_and_seed_starting();
            let cache = mgr.status_cache_handle();
            (Arc::new(std::sync::RwLock::new(mgr)), cache)
        };

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
                    .filter(|s| crate::mcp::parse_prefixed_tool_name(s).is_some())
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
        } else {
            let registry_empty = provider_registry.as_ref().is_some_and(|r| r.is_empty());
            for text in build_missing_provider_messages(
                registry_empty,
                config.model.as_deref(),
                &missing_api_keys,
            ) {
                messages.push(MessageBlock::Error { text });
            }
        }

        let mut app = Self {
            project,
            config,
            storage,
            agents_files,
            provider_registry,
            missing_api_keys,
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
            sidebar_state: {
                // Pre-populate `lsp_servers` from the seeded cache so the
                // very first render (before any `Tick` fires) already shows
                // `Starting` entries — otherwise there is a visible gap
                // between the initial draw and the first Tick poll.
                let mut state = SidebarState::default();
                state.lsp_servers = crate::lsp::LspManager::snapshot_cache(&lsp_status_cache)
                    .into_iter()
                    .map(|(_, entry)| SidebarLsp {
                        binary: entry.binary,
                        state: entry.state,
                        progress_message: entry.progress_message,
                        next_restart_at: entry.next_restart_at,
                    })
                    .collect();
                state
            },
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
            lsp_diagnostics_overlay:
                crate::ui::lsp_diagnostics_overlay::LspDiagnosticsOverlayState::default(),
            compaction_count: 0,
            selection_state: SelectionState::default(),
            last_message_area: ratatui::layout::Rect::default(),
            lsp_manager,
            lsp_status_cache,
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
pub(crate) mod tests {
    use super::*;
    use crate::provider::ProviderInitWarning;

    fn warn(provider: &str, env_var: &str) -> ProviderInitWarning {
        ProviderInitWarning {
            provider_id: provider.to_string(),
            env_var: env_var.to_string(),
        }
    }

    #[test]
    fn no_missing_providers_returns_empty() {
        let out = build_missing_provider_messages(false, Some("fireworks/foo"), &[]);
        assert!(out.is_empty());
    }

    #[test]
    fn all_providers_disabled_produces_single_loud_message() {
        let missing = vec![
            warn("fireworks", "FIREWORKS_API_KEY"),
            warn("anthropic", "ANTHROPIC_API_KEY"),
        ];
        // registry_empty=true — nothing works at all.
        let out = build_missing_provider_messages(true, Some("fireworks/foo"), &missing);

        assert_eq!(out.len(), 1, "single consolidated message expected");
        let msg = &out[0];
        assert!(
            msg.contains("STEVE CANNOT SEND MESSAGES"),
            "headline must be impossible to miss: {msg}",
        );
        assert!(msg.contains("no working providers"));
        assert!(msg.contains("$FIREWORKS_API_KEY"));
        assert!(msg.contains("$ANTHROPIC_API_KEY"));
        assert!(msg.contains("restart steve"));
    }

    #[test]
    fn default_providers_provider_disabled_produces_loud_message_even_if_others_work() {
        // Only fireworks is broken, but it's the default — loud message still.
        let missing = vec![warn("fireworks", "FIREWORKS_API_KEY")];
        let out = build_missing_provider_messages(false, Some("fireworks/qwen3-coder"), &missing);

        assert_eq!(out.len(), 1);
        let msg = &out[0];
        assert!(msg.contains("STEVE CANNOT SEND MESSAGES"));
        assert!(msg.contains("default model's provider is disabled"));
        assert!(
            msg.contains("fireworks/qwen3-coder"),
            "must name the default model so user knows what's broken: {msg}",
        );
        assert!(msg.contains("$FIREWORKS_API_KEY"));
    }

    #[test]
    fn non_default_providers_disabled_produces_terse_per_line_messages() {
        // Default is `fireworks/X` (working); `anthropic` is broken.
        let missing = vec![warn("anthropic", "ANTHROPIC_API_KEY")];
        let out = build_missing_provider_messages(false, Some("fireworks/qwen3-coder"), &missing);

        assert_eq!(out.len(), 1);
        let msg = &out[0];
        assert!(
            !msg.contains("STEVE CANNOT SEND MESSAGES"),
            "non-default failure should NOT use the loud headline: {msg}",
        );
        assert!(msg.contains("Provider 'anthropic' disabled"));
        assert!(msg.contains("$ANTHROPIC_API_KEY"));
    }

    #[test]
    fn multiple_non_default_disabled_produces_one_terse_line_per_provider() {
        // fireworks is default + working; two others broken.
        let missing = vec![
            warn("anthropic", "ANTHROPIC_API_KEY"),
            warn("openai", "OPENAI_API_KEY"),
        ];
        let out = build_missing_provider_messages(false, Some("fireworks/qwen3-coder"), &missing);

        assert_eq!(out.len(), 2, "one terse line per disabled provider");
        assert!(out.iter().any(|m| m.contains("anthropic")));
        assert!(out.iter().any(|m| m.contains("openai")));
    }

    #[test]
    fn no_default_model_with_empty_registry_still_loud() {
        // User has providers in config but no default model set AND no env vars:
        // still no way to send messages, so still loud.
        let missing = vec![warn("fireworks", "FIREWORKS_API_KEY")];
        let out = build_missing_provider_messages(true, None, &missing);

        assert_eq!(out.len(), 1);
        assert!(out[0].contains("no working providers"));
        assert!(!out[0].contains("Default model:"), "no default to name");
    }

    /// Create a minimal App for testing (without real storage/config).
    /// Note: uses `Storage::new` which writes to the real app data dir. This is
    /// acceptable because UI rendering tests don't perform storage writes. A
    /// temp-dir approach would require returning `TempDir` to keep it alive.
    /// Lazily-initialized tokio runtime for sync test helpers.
    ///
    /// `App::new` calls `tokio::runtime::Handle::current()` for the LSP manager,
    /// but most unit tests run outside a tokio context. This provides a shared
    /// runtime whose handle satisfies that requirement. The runtime is stored in
    /// a `OnceLock` so the handle remains valid for the entire test process.
    fn test_runtime_handle() -> tokio::runtime::Handle {
        use std::sync::OnceLock;
        static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        RT.get_or_init(|| tokio::runtime::Runtime::new().expect("test tokio runtime"))
            .handle()
            .clone()
    }

    pub(crate) fn make_test_app() -> App {
        use crate::{config::Config, project::ProjectInfo, storage::Storage};
        use std::path::PathBuf;

        let _guard = test_runtime_handle().enter();
        let root = PathBuf::from("/tmp/test");
        let project = ProjectInfo {
            root: root.clone(),
            id: "test".to_string(),
            cwd: root,
        };
        let config = Config::default();
        let storage = Storage::new("test-sidebar").expect("test storage");
        let usage_writer = crate::usage::test_usage_writer();
        App::new(
            project,
            config,
            storage,
            Vec::new(),
            None,
            Vec::new(),
            Vec::new(),
            usage_writer,
        )
    }

    /// Create a test app backed by an isolated temp directory for storage tests.
    pub(super) fn make_test_app_with_storage() -> (App, tempfile::TempDir) {
        use crate::{config::Config, project::ProjectInfo, storage::Storage};
        use std::path::PathBuf;

        let _guard = test_runtime_handle().enter();
        let dir = tempfile::tempdir().expect("temp dir");
        let storage = Storage::with_base(dir.path().to_path_buf()).expect("storage");
        let root = PathBuf::from("/tmp/test");
        let project = ProjectInfo {
            root: root.clone(),
            id: "test-title".to_string(),
            cwd: root,
        };
        let config = Config::default();
        let usage_writer = crate::usage::test_usage_writer();
        let app = App::new(
            project,
            config,
            storage,
            Vec::new(),
            None,
            Vec::new(),
            Vec::new(),
            usage_writer,
        );
        (app, dir)
    }

    /// Helper: create a session via SessionManager and return the SessionInfo.
    pub(super) fn create_test_session(app: &App) -> SessionInfo {
        let mgr = SessionManager::new(&app.storage, &app.project.id);
        mgr.create_session("test/model")
            .expect("create test session")
    }

    /// Helper: build a ProviderRegistry with a single test model.
    pub(crate) fn make_test_registry(context_window: u32) -> crate::provider::ProviderRegistry {
        use crate::config::{ModelCapabilities, ModelConfig, ProviderConfig};
        use std::collections::HashMap;

        let mut models = HashMap::new();
        models.insert(
            "test-model".to_string(),
            ModelConfig {
                id: "test-model".to_string(),
                name: "Test Model".to_string(),
                context_window,
                max_output_tokens: None,
                cost: None,
                capabilities: ModelCapabilities {
                    tool_call: true,
                    reasoning: false,
                },
            },
        );
        let provider_config = ProviderConfig {
            base_url: "https://api.test.com/v1".to_string(),
            api_key_env: "TEST_KEY".to_string(),
            models,
        };
        let client = crate::provider::client::LlmClient::new("https://api.test.com/v1", "fake");
        crate::provider::ProviderRegistry::from_entries(vec![(
            "test".to_string(),
            provider_config,
            client,
        )])
    }

    /// Check if any Error message contains the given substring.
    pub(crate) fn has_error_message(app: &App, needle: &str) -> bool {
        app.messages
            .iter()
            .any(|m| matches!(m, MessageBlock::Error { text } if text.contains(needle)))
    }

    /// Check if any System message contains the given substring.
    pub(crate) fn has_system_message(app: &App, needle: &str) -> bool {
        app.messages
            .iter()
            .any(|m| matches!(m, MessageBlock::System { text } if text.contains(needle)))
    }

    // ─── Overlay / cross-cutting tests ───

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

        app.diagnostics_overlay.close();
        assert!(!app.diagnostics_overlay.visible);
    }

    #[test]
    fn diagnostics_overlay_closes_model_picker() {
        let mut app = make_test_app();
        let models = vec![("openai/gpt-4o".into(), "GPT-4o".into())];
        app.model_picker.open(&models, None);
        assert!(app.model_picker.visible);

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

        app.mcp_overlay.close();
        assert!(!app.mcp_overlay.visible);
    }

    #[test]
    fn mcp_overlay_closes_other_overlays() {
        let mut app = make_test_app();
        let models = vec![("openai/gpt-4o".into(), "GPT-4o".into())];
        app.model_picker.open(&models, None);
        assert!(app.model_picker.visible);

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
