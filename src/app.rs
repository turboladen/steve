use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use tokio_stream::StreamExt;
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
use crate::usage::UsageWriter;
use crate::usage::types::SessionRecord;
use crate::ui;
use crate::ui::autocomplete::{AutocompleteMode, AutocompleteState, apply_file_completion};
use crate::ui::input::InputState;
use crate::ui::message_area::MessageAreaState;
use crate::ui::model_picker::ModelPickerState;
use crate::ui::session_picker::SessionPickerState;
use crate::ui::message_block::{
    AssistantPart, DiffContent, DiffLine, MessageBlock, ToolCall,
};
use crate::ui::selection::SelectionState;
use crate::task::types::{Priority, TaskKind, TaskStatus};
use crate::ui::sidebar::{SidebarLsp, SidebarState, SidebarTask, count_diff_lines, MAX_SIDEBAR_TASKS};
use crate::ui::status_line::{Activity, StatusLineState};
use crate::ui::theme::Theme;

/// System prompt for conversation compaction/summarization.
const COMPACT_SYSTEM_PROMPT: &str = "Provide a detailed but concise summary of the conversation below. \
Focus on information that would be helpful for continuing the conversation, including: \
what was done, what is currently being worked on, which files are being modified, \
decisions that were made, and what the next steps are. \
Preserve specific technical details, file paths, and code patterns.";

/// System prompt for AGENTS.md generation/update.
const AGENTS_UPDATE_SYSTEM_PROMPT: &str = "You are analyzing a software project to produce an AGENTS.md file — \
project-specific instructions for AI coding assistants working in this codebase.\n\n\
Output raw markdown only — no code fences wrapping the entire response.\n\n\
Focus on:\n\
- Build, test, and run commands\n\
- Project architecture and key abstractions\n\
- Coding conventions and patterns specific to this project\n\
- Common gotchas and things an AI assistant should know\n\
- Important file paths and their purposes\n\n\
If an existing AGENTS.md is provided, preserve valuable existing content while improving \
and extending it with new insights from the project context. Do not remove useful information \
that was already there.\n\n\
Keep the document concise and actionable — focus on what an AI assistant needs to know \
to be effective in this codebase.";

/// System prompt for LLM-generated session titles.
const TITLE_SYSTEM_PROMPT: &str = "Generate a concise 3-7 word title for this conversation \
based on the user's first message. Output only the title — no punctuation at the end, \
no quotes, no explanation. The title should capture the user's intent in plain English.";

/// Guidance for efficient tool usage, injected into the system prompt.
const TOOL_GUIDANCE: &str = "\n\n## Task Planning\n\n\
When the user gives you a task with multiple sequential steps (e.g. \"do X, then Y, then Z\") or any task that will require 3+ distinct actions, \
you MUST use the `task` tool FIRST to create your plan before doing any other work. \
Create one task per step, then work through them one at a time — complete each task before starting the next. \
Tasks persist across sessions, so you can plan in one session and execute across multiple. \
Use epics to group related tasks under a larger work item (e.g., a Jira ticket).\n\n\
## Tool Call Budget\n\n\
You have a limited number of tool calls per response. Plan your exploration efficiently:\n\
- **Aim for 10-20 tool calls** per response. Beyond that, you are likely over-exploring.\n\
- After gathering key files, synthesize your findings and respond. Do not keep exploring.\n\
- The system will warn you as you approach the limit. At ~73% of the budget, **tool access is revoked** — \
you will not be able to make any more tool calls. Plan accordingly and start synthesizing early.\n\
- If you hit the hard limit, you get one final chance to respond before the stream is terminated.\n\n\
## IMPORTANT: Use Native Tools, Not Bash\n\n\
Do NOT use `bash` for simple file operations — use the dedicated tool instead:\n\
| Instead of... | Use |\n\
|---|---|\n\
| `cat`, `head`, `tail`, `wc -l` | `read` (with `offset`/`limit`, `tail`, or `count`) |\n\
| `ls`, `find` | `list`, `glob` |\n\
| `grep`, `rg` | `grep` |\n\
| `sed`, `awk` | `edit`, `patch` |\n\n\
Simple bash commands like `cat file` or `ls dir` will be REJECTED — use the native tool.\n\
Piped/compound commands (e.g., `cat file | wc -l`) are allowed since they go beyond native tool capabilities.\n\n\
## Tool Usage Guidelines\n\n\
- **Verify CLI tools before recommending**: When suggesting an external CLI tool (e.g., `pdftotext`, `jq`, `ffmpeg`), first check if it's installed by running `command -v <tool>` via `bash`. If it's not available, say so explicitly and suggest how to install it (e.g., `brew install poppler` on macOS). Never assume a tool is on the user's PATH.\n\
- **Line-based edits**: The `edit` tool supports `insert_lines`, `delete_lines`, and `replace_range` operations with 1-indexed line numbers matching `read` output. Use these when you know the exact line numbers instead of find_replace.\n\
- **Search before reading**: Use `grep` to find relevant code, then `read` with specific line ranges. Avoid reading entire large files.\n\
- **Use line ranges**: The `read` tool supports `offset`/`limit` for ranges, `tail` for last N lines, and `count` for line counts without content. For files over 200 lines, read only the relevant section.\n\
- **Read multiple files**: Use `read` with `paths` (array) to read several files in one call instead of separate reads.\n\
- **Be context-efficient**: Each tool result consumes context window space. Prefer targeted searches over broad reads.\n\
- **Glob for discovery**: Use `glob` to find files by pattern before reading them.\n\
- **Batch related reads**: If you need multiple files, request them in a single response to enable parallel execution.\n\
- **Respond literally**: When the user asks to see, show, or display content, output the actual content in a fenced code block — do not summarize or paraphrase. In general, follow the user's request directly rather than reinterpreting what they want.\n\
- **Avoid re-reading**: Files you've already read are cached. If a tool returns a message saying content is unchanged, \
that means you already have this information in your conversation context. Do NOT try to work around it — \
proceed with the information you already have and answer the user's question.\n\
- **Code structure**: Use the `symbols` tool to list functions/structs/classes in a file, find what scope contains a line, or locate a symbol definition. Faster and more accurate than grepping for structural queries.\n\
- **Record discoveries**: Use the `memory` tool to save important project context (architecture, patterns, key files) that persists across sessions. \
Your project memory is automatically loaded into context — you don't need to read it manually. \
When memory gets long, use 'replace' to consolidate into a curated summary. Worth remembering: \
architecture decisions, key file locations, recurring patterns, user preferences, gotchas encountered.\n\
- **Language intelligence**: Use the `lsp` tool for compiler-grade diagnostics, go-to-definition, find-references, and rename planning. \
It connects to real language servers (rust-analyzer, pyright, typescript-language-server, etc.) for accurate, cross-file analysis. \
Use `diagnostics` to check for compile errors after edits, `definition` to jump to a symbol's source, \
`references` to find all usages, and `rename` to get a safe rename plan (then apply with `edit`). \
Prefer `lsp` over `grep` when you need semantic accuracy (e.g., distinguishing a type from a variable with the same name).\n\
- **Delegate to sub-agents**: Use the `agent` tool to spawn child agents with their own context windows. \
Choose `explore` for fast read-only searches (uses smaller model), `plan` for architecture analysis (read + LSP), \
or `general` for full tool access including writes. Sub-agents run autonomously and return a summary. \
Use agents to protect your context from large exploration results, parallelize independent searches, \
or isolate complex subtasks.";

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

/// Extract a compact argument summary for display in tool call lines.
/// Build a compact argument summary for a tool call (e.g., path for read, pattern for grep).
/// Public so `stream.rs` can use it for sub-agent progress updates.
pub fn extract_args_summary(tool_name: ToolName, args: &Value) -> String {
    match tool_name {
        ToolName::Read => {
            if let Some(paths) = args.get("paths").and_then(|v| v.as_array()) {
                format!("{} files", paths.len())
            } else {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let is_count = args.get("count").and_then(|v| v.as_bool()).unwrap_or(false);
                let tail_n = args.get("tail").and_then(|v| v.as_u64());
                if is_count {
                    format!("{path} (count)")
                } else if let Some(n) = tail_n {
                    format!("{path} (tail {n})")
                } else {
                    path.to_string()
                }
            }
        }
        ToolName::List => args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Symbols => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let op = args.get("operation").and_then(|v| v.as_str()).unwrap_or("list_symbols");
            match op {
                "find_scope" => {
                    let line = args.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                    format!("{path} scope@{line}")
                }
                "find_definition" => {
                    let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    format!("{path} def:{name}")
                }
                _ => path.to_string(),
            }
        }
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
            .get("question")
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
        ToolName::Task => args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
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
        ToolName::Lsp => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let op = args.get("operation").and_then(|v| v.as_str()).unwrap_or("diagnostics");
            match op {
                "diagnostics" => format!("{path} diagnostics"),
                _ => {
                    let line = args.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                    format!("{path} {op}@{line}")
                }
            }
        }
        ToolName::Agent => {
            let agent_type = args.get("agent_type").and_then(|v| v.as_str()).unwrap_or("explore");
            let task = args.get("task").and_then(|v| v.as_str()).unwrap_or("");
            let truncated = if task.chars().count() > 30 {
                let t: String = task.chars().take(27).collect();
                format!("{t}...")
            } else {
                task.to_string()
            };
            format!("{agent_type}: {truncated}")
        }
    }
}

/// Build a compact result summary for a tool output (truncated to 80 chars).
/// Public so `stream.rs` can use it for sub-agent progress updates.
pub fn extract_result_summary(tool_name: ToolName, output: &crate::tool::ToolOutput) -> String {
    let _ = tool_name; // All tools use the same truncation logic for now
    if output.output.chars().count() > 80 {
        let truncated: String = output.output.chars().take(77).collect();
        format!("{truncated}...")
    } else {
        output.output.clone()
    }
}

/// Extract inline diff content from tool call arguments for UI rendering.
/// Returns `None` for tools that don't produce diffs (read, grep, bash, etc.).
fn extract_diff_content(tool_name: ToolName, args: &Value) -> Option<DiffContent> {
    match tool_name {
        ToolName::Edit => {
            let operation = args
                .get("operation")
                .and_then(|v| v.as_str())
                .unwrap_or("find_replace");
            match operation {
                "find_replace" => {
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
                "insert_lines" => {
                    let line_num = args.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    if content.is_empty() {
                        return None;
                    }
                    let mut lines = vec![DiffLine::HunkHeader(format!("@@ +{line_num} @@"))];
                    for line in content.lines() {
                        lines.push(DiffLine::Addition(line.to_string()));
                    }
                    Some(DiffContent::EditDiff { lines })
                }
                "delete_lines" => {
                    let start = args.get("start_line").and_then(|v| v.as_u64()).unwrap_or(0);
                    let end = args.get("end_line").and_then(|v| v.as_u64()).unwrap_or(0);
                    let count = end.saturating_sub(start) + 1;
                    let lines = vec![
                        DiffLine::HunkHeader(format!("@@ -{start},{count} @@")),
                        DiffLine::Removal(format!("({count} line(s) deleted)")),
                    ];
                    Some(DiffContent::EditDiff { lines })
                }
                "replace_range" => {
                    let start = args.get("start_line").and_then(|v| v.as_u64()).unwrap_or(0);
                    let end = args.get("end_line").and_then(|v| v.as_u64()).unwrap_or(0);
                    let old_count = end.saturating_sub(start) + 1;
                    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    let mut lines = vec![
                        DiffLine::HunkHeader(format!("@@ -{start},{old_count} @@")),
                        DiffLine::Removal(format!("({old_count} line(s) replaced)")),
                    ];
                    for line in content.lines() {
                        lines.push(DiffLine::Addition(line.to_string()));
                    }
                    Some(DiffContent::EditDiff { lines })
                }
                "multi_find_replace" => {
                    let edits = args.get("edits").and_then(|v| v.as_array());
                    let mut lines = Vec::new();
                    if let Some(edits) = edits {
                        for edit in edits {
                            let old =
                                edit.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
                            let new =
                                edit.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
                            for line in old.lines() {
                                lines.push(DiffLine::Removal(line.to_string()));
                            }
                            for line in new.lines() {
                                lines.push(DiffLine::Addition(line.to_string()));
                            }
                        }
                    }
                    if lines.is_empty() {
                        None
                    } else {
                        Some(DiffContent::EditDiff { lines })
                    }
                }
                other => {
                    tracing::warn!("unhandled edit operation for diff extraction: {other}");
                    None
                }
            }
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
        | ToolName::Task
        | ToolName::Webfetch
        | ToolName::Memory
        | ToolName::Move
        | ToolName::Copy
        | ToolName::Delete
        | ToolName::Mkdir
        | ToolName::Symbols
        | ToolName::Lsp
        | ToolName::Agent => None,
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
        let profile = config.permission_profile.unwrap_or(crate::permission::PermissionProfile::Standard);
        let allow_overrides: Vec<ToolName> = config.allow_tools.iter()
            .filter_map(|s| s.parse::<ToolName>().ok())
            .collect();
        let permission_engine = Arc::new(tokio::sync::Mutex::new(PermissionEngine::new(
            crate::permission::profile_build_rules(profile, &allow_overrides, &config.permission_rules),
        )));

        // Build tool result cache (session-scoped, shared across stream tasks)
        let tool_cache = Arc::new(std::sync::Mutex::new(ToolResultCache::new(
            project.root.clone(),
        )));

        // Build task store (persistent across sessions)
        let repo_name = crate::project::git_repo_name(&project.root)
            .unwrap_or_else(|| "proj".to_string());
        let task_store = crate::task::TaskStore::new(storage.clone(), repo_name);

        // Build LSP manager (servers started in background after app init)
        let lsp_manager = Arc::new(std::sync::Mutex::new(
            crate::lsp::LspManager::new(project.root.clone()),
        ));

        // Build MCP manager (servers started in background after app init)
        let mcp_manager = Arc::new(tokio::sync::Mutex::new(
            crate::mcp::McpManager::new(),
        ));

        // Configure MCP-related permission state on the engine
        {
            // Safe: engine was just created, no contention possible
            if let Ok(mut engine) = permission_engine.try_lock() {
                engine.set_profile(profile);

                let mcp_overrides: std::collections::HashSet<String> = config.allow_tools.iter()
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

    pub async fn run(&mut self) -> Result<()> {
        // Try to resume the last session
        self.resume_or_new_session();

        let (mut terminal, detected) = ui::detect_and_setup_terminal()?;
        self.theme = ui::terminal_detect::resolve_theme(self.config.theme, detected);
        let mut crossterm_events = crossterm::event::EventStream::new();
        let mut tick_interval = tokio::time::interval(Duration::from_millis(100));

        // Start LSP servers in background (non-blocking)
        {
            let lsp = self.lsp_manager.clone();
            let tx = self.event_tx.clone();
            tokio::task::spawn_blocking(move || {
                if let Ok(mut mgr) = lsp.lock() {
                    mgr.start_servers();
                    let status = mgr.language_status();
                    if !status.is_empty() {
                        let running: Vec<&str> = status
                            .iter()
                            .filter(|(_, r)| *r)
                            .map(|(l, _)| l.as_str())
                            .collect();
                        if !running.is_empty() {
                            let _ = tx.send(AppEvent::StreamNotice {
                                text: format!(
                                    "LSP servers started: {}",
                                    running.join(", ")
                                ),
                            });
                        }
                        let _ = tx.send(AppEvent::LspStatus { servers: status });
                    }
                }
            });
        }

        // Start MCP servers in background (non-blocking)
        if !self.config.mcp_servers.is_empty() {
            let mcp = self.mcp_manager.clone();
            let tx = self.event_tx.clone();
            let configs = self.config.mcp_servers.clone();
            tokio::spawn(async move {
                let mut mgr = mcp.lock().await;
                mgr.start_servers(&configs).await;
                let summary = mgr.server_summary();
                if !summary.is_empty() {
                    let _ = tx.send(AppEvent::StreamNotice {
                        text: format!("MCP servers started: {}", summary.join(", ")),
                    });
                }
            });
        }

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
            self.usage_writer.upsert_session(SessionRecord {
                session_id: session.id.clone(),
                project_id: self.project.id.clone(),
                title: session.title.clone(),
                model_ref: session.model_ref.clone(),
                created_at: session.created_at,
            });
            self.current_session = Some(session);
        }

        self.refresh_git_info();
        self.sync_sidebar_tokens();
        self.sync_diagnostics();
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
        self.stream_start_time = None;
        self.frozen_elapsed = None;
        self.is_loading = false;
        self.auto_compact_failed = false;
        self.context_warned = false;
        self.last_prompt_tokens = 0;
        self.exchange_count = 0;
        self.pending_permission = None;
        self.pending_question = None;
        self.pending_agents_update = None;
        self.model_picker.close();
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
        self.sync_context_window();
        self.usage_writer.upsert_session(SessionRecord {
            session_id: session.id.clone(),
            project_id: self.project.id.clone(),
            title: session.title.clone(),
            model_ref: session.model_ref.clone(),
            created_at: session.created_at,
        });
        self.current_session = Some(session.clone());
        self.sidebar_state.changes.clear();
        self.sidebar_state.session_closed_task_ids.clear();
        self.model_picker.close();
        self.session_picker.close();
        self.diagnostics_overlay.close();
        self.compaction_count = 0;
        self.refresh_git_info();
        self.sync_sidebar_tokens();
        self.sync_diagnostics();
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
                // Block mouse events in the message area when an overlay is active
                if self.model_picker.visible || self.session_picker.visible || self.diagnostics_overlay.visible || self.pending_question.is_some() {
                    return Ok(());
                }
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
                if self.pending_permission.is_none() && self.pending_question.is_none() {
                    self.input.collapse_paste(&text);
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
                self.status_line_state.set_activity(Activity::RunningTool {
                    tool_name,
                    args_summary: String::new(),
                });
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
                self.status_line_state.set_activity(Activity::RunningTool {
                    tool_name,
                    args_summary,
                });
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

                // Track task completions for sidebar display
                if tool_name == ToolName::Task && !output.is_error {
                    if let Some(call) = self.find_last_completed_call(tool_name) {
                        if call.args_summary == "complete" {
                            // Parse task ID from output: "Completed task {id}: {title}"
                            // Safe to split on ':' — task IDs are hex-only (task-XXXXXXXX)
                            if let Some(rest) = output.output.strip_prefix("Completed task ") {
                                if let Some(id) = rest.split(':').next() {
                                    self.sidebar_state
                                        .record_task_closed(id.trim().to_string());
                                }
                            }
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
                if let Some(start) = self.stream_start_time {
                    self.frozen_elapsed = Some(start.elapsed());
                }
                self.is_loading = false;
                self.streaming_active = false;
                self.stream_cancel = None;
                self.interjection_tx = None;
                self.status_line_state.set_activity(Activity::Idle);

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
                    self.sync_diagnostics();
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
                if let Some(start) = self.stream_start_time {
                    self.frozen_elapsed = Some(start.elapsed());
                }
                self.is_loading = false;
                self.streaming_active = false;
                self.stream_cancel = None;
                self.interjection_tx = None;
                self.streaming_message = None;
                self.messages.push(MessageBlock::Error { text: error });
                self.status_line_state.set_activity(Activity::Idle);
                self.message_area_state.scroll_to_bottom();
            }
            AppEvent::StreamNotice { text } => {
                self.messages.push(MessageBlock::System { text });
                self.message_area_state.scroll_to_bottom();
            }
            AppEvent::AgentProgress { call_id: _, tool_name, args_summary, result_summary } => {
                // Update the agent tool call's inline progress — no new MessageBlock,
                // so the assistant block stays at the bottom and follow-up text is visible.
                if let Some(last) = self.last_assistant_mut() {
                    if result_summary.is_some() {
                        // ToolResult: just update the result on the existing progress
                        last.update_agent_progress_result(result_summary);
                    } else {
                        // LlmToolCall: new tool call, update with tool_name + args
                        last.update_agent_progress(tool_name, args_summary);
                    }
                }
            }
            AppEvent::LspStatus { servers } => {
                self.sidebar_state.lsp_servers = servers
                    .into_iter()
                    .map(|(binary, running)| SidebarLsp { binary, running })
                    .collect();
            }
            AppEvent::PermissionRequest(req) => {
                // Show permission prompt to user, with diff preview if available
                let diff_content = extract_diff_content(req.tool_name, &req.tool_args);
                self.messages.push(MessageBlock::Permission {
                    tool_name: req.tool_name.to_string(),
                    args_summary: req.arguments_summary.clone(),
                    diff_content,
                });
                self.status_line_state.set_activity(Activity::WaitingForPermission);
                self.message_area_state.scroll_to_bottom();
                self.pending_permission = Some(PendingPermission {
                    tool_name: req.tool_name,
                    summary: req.arguments_summary,
                    response_tx: req.response_tx,
                });
            }
            AppEvent::QuestionRequest(req) => {
                let has_options = !req.options.is_empty();
                self.messages.push(MessageBlock::Question {
                    question: req.question.clone(),
                    options: req.options.clone(),
                    selected: if has_options { Some(0) } else { None },
                    free_text: String::new(),
                    answered: None,
                });
                self.status_line_state.set_activity(Activity::WaitingForQuestion);
                self.message_area_state.scroll_to_bottom();
                self.pending_question = Some(PendingQuestion {
                    call_id: req.call_id,
                    question: req.question,
                    options: req.options,
                    selected: if has_options { Some(0) } else { None },
                    free_text: String::new(),
                    response_tx: req.response_tx,
                });
            }
            AppEvent::CompactFinish { summary } => {
                self.is_loading = false;
                self.compaction_count += 1;

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
                self.sync_diagnostics();
                self.update_sidebar();

                tracing::info!("conversation compacted successfully");
            }
            AppEvent::CompactError { error } => {
                self.is_loading = false;
                self.auto_compact_failed = true;
                self.messages.push(MessageBlock::Error { text: error });
                self.status_line_state.set_activity(Activity::Idle);
                self.message_area_state.scroll_to_bottom();
                tracing::error!("compaction failed, auto-compact disabled for this session");
            }
            AppEvent::AgentsUpdateFinish { proposed_content } => {
                // Guard: if cancelled (is_loading already cleared), discard the late result
                if !self.is_loading {
                    tracing::info!("AGENTS.md update result arrived after cancellation, discarding");
                } else {
                    self.is_loading = false;
                    self.status_line_state.set_activity(Activity::Idle);
                    self.pending_agents_update = Some(proposed_content.clone());
                    self.messages.push(MessageBlock::System {
                        text: "Proposed AGENTS.md update \u{2014} press **y** to apply, **n** to discard:".to_string(),
                    });
                    self.messages.push(MessageBlock::Assistant {
                        thinking: None,
                        parts: vec![AssistantPart::Text(proposed_content)],
                    });
                    self.message_area_state.scroll_to_bottom();
                    tracing::info!("AGENTS.md update proposed, awaiting user approval");
                }
            }
            AppEvent::AgentsUpdateError { error } => {
                if !self.is_loading {
                    tracing::info!("AGENTS.md update error arrived after cancellation, discarding");
                } else {
                    self.is_loading = false;
                    self.status_line_state.set_activity(Activity::Idle);
                    self.messages.push(MessageBlock::Error { text: error });
                    self.message_area_state.scroll_to_bottom();
                    tracing::error!("AGENTS.md update failed");
                }
            }
            AppEvent::TitleGenerated { session_id, title } => {
                self.apply_title_if_current(&session_id, &title);
            }
            AppEvent::TitleError {
                session_id,
                fallback_title,
            } => {
                self.apply_title_if_current(&session_id, &fallback_title);
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
        // Only process key presses — ignore Release/Repeat events from enhanced keyboard protocol
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        // If there's a pending permission prompt, intercept keystrokes
        if self.pending_permission.is_some() {
            match (key.code, key.modifiers) {
                (KeyCode::Char('y'), _) | (KeyCode::Char('Y'), _) => {
                    if let Some(perm) = self.pending_permission.take() {
                        let _ = perm.response_tx.send(PermissionReply::AllowOnce);
                        self.remove_last_permission_block();
                        self.status_line_state.set_activity(Activity::Thinking);
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
                        self.status_line_state.set_activity(Activity::Thinking);
                        self.message_area_state.scroll_to_bottom();
                    }
                    return Ok(());
                }
                (KeyCode::Char('a'), _) | (KeyCode::Char('A'), _) => {
                    if let Some(perm) = self.pending_permission.take() {
                        let tool_str = perm.tool_name.as_str().to_string();
                        let _ = perm.response_tx.send(PermissionReply::AllowAlways);
                        self.remove_last_permission_block();
                        self.status_line_state.set_activity(Activity::Thinking);
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

        // If there's a pending question prompt, intercept keystrokes
        if self.pending_question.is_some() {
            match (key.code, key.modifiers) {
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                    // Cancel the stream entirely
                    if let Some(q) = self.pending_question.take() {
                        let _ = q.response_tx.send("User cancelled.".to_string());
                    }
                    self.cancel_stream();
                    return Ok(());
                }
                (KeyCode::Enter, _) => {
                    if let Some(q) = self.pending_question.take() {
                        let answer = if let Some(idx) = q.selected {
                            q.options.get(idx).cloned().unwrap_or_default()
                        } else if q.free_text.is_empty() {
                            "User declined to answer.".to_string()
                        } else {
                            q.free_text.clone()
                        };
                        let display_answer = answer.clone();
                        let _ = q.response_tx.send(answer);
                        // Mark the question block as answered
                        self.mark_question_answered(&display_answer);
                        self.status_line_state.set_activity(Activity::Thinking);
                        self.message_area_state.scroll_to_bottom();
                    }
                    return Ok(());
                }
                (KeyCode::Esc, _) => {
                    if let Some(q) = self.pending_question.take() {
                        let _ = q.response_tx.send("User declined to answer.".to_string());
                        self.mark_question_answered("(skipped)");
                        self.status_line_state.set_activity(Activity::Thinking);
                        self.message_area_state.scroll_to_bottom();
                    }
                    return Ok(());
                }
                (KeyCode::Char(c @ '1'..='9'), _) => {
                    if let Some(q) = self.pending_question.as_mut() {
                        let idx = (c as usize) - ('1' as usize);
                        if idx < q.options.len() {
                            q.selected = Some(idx);
                            self.sync_question_block();
                        }
                    }
                    return Ok(());
                }
                (KeyCode::Up, _) => {
                    if let Some(q) = self.pending_question.as_mut() {
                        if let Some(sel) = q.selected {
                            if sel > 0 {
                                q.selected = Some(sel - 1);
                                self.sync_question_block();
                            }
                        }
                    }
                    return Ok(());
                }
                (KeyCode::Down, _) => {
                    if let Some(q) = self.pending_question.as_mut() {
                        if let Some(sel) = q.selected {
                            if sel + 1 < q.options.len() {
                                q.selected = Some(sel + 1);
                                self.sync_question_block();
                            }
                        }
                    }
                    return Ok(());
                }
                (KeyCode::Tab, _) => {
                    if let Some(q) = self.pending_question.as_mut() {
                        if q.options.is_empty() {
                            // No options — already in free-text mode, ignore
                        } else if q.selected.is_some() {
                            // Switch to free-text mode
                            q.selected = None;
                            self.sync_question_block();
                        } else {
                            // Switch back to options mode
                            q.selected = Some(0);
                            self.sync_question_block();
                        }
                    }
                    return Ok(());
                }
                (KeyCode::Char(c), _) => {
                    if let Some(q) = self.pending_question.as_mut() {
                        if q.selected.is_none() {
                            // Free-text mode
                            q.free_text.push(c);
                            self.sync_question_block();
                        }
                    }
                    return Ok(());
                }
                (KeyCode::Backspace, _) => {
                    if let Some(q) = self.pending_question.as_mut() {
                        if q.selected.is_none() {
                            q.free_text.pop();
                            self.sync_question_block();
                        }
                    }
                    return Ok(());
                }
                _ => {
                    // Swallow other keys
                    return Ok(());
                }
            }
        }

        // If there's a pending AGENTS.md update awaiting approval, intercept y/n
        if self.pending_agents_update.is_some() {
            match (key.code, key.modifiers) {
                (KeyCode::Char('y'), _) | (KeyCode::Char('Y'), _) => {
                    let content = self.pending_agents_update.take().unwrap();
                    let agents_path = self.project.root.join("AGENTS.md");
                    match std::fs::write(&agents_path, &content) {
                        Ok(_) => {
                            // Update or insert root-level entry in the chain
                            if let Some(existing) = self.agents_files.iter_mut().find(|f| f.path == agents_path) {
                                existing.content = content;
                            } else {
                                self.agents_files.insert(0, crate::config::AgentsFile {
                                    path: agents_path.clone(),
                                    content,
                                });
                            }
                            self.messages.push(MessageBlock::System {
                                text: format!("AGENTS.md updated at {}", agents_path.display()),
                            });
                        }
                        Err(e) => {
                            self.messages.push(MessageBlock::Error {
                                text: format!("Failed to write AGENTS.md: {e}"),
                            });
                        }
                    }
                    self.message_area_state.scroll_to_bottom();
                }
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                    self.discard_pending_agents_update();
                }
                (KeyCode::Char('n'), _) | (KeyCode::Char('N'), _) | (KeyCode::Esc, _) => {
                    self.discard_pending_agents_update();
                }
                _ => {
                    // Ignore other keys
                }
            }
            return Ok(());
        }

        // If the model picker overlay is open, intercept keystrokes
        if self.model_picker.visible {
            match (key.code, key.modifiers) {
                (KeyCode::Esc, _) => {
                    self.model_picker.close();
                }
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                    self.model_picker.close();
                    // Also cancel any active stream (unlikely but possible)
                    if self.is_loading || self.streaming_active {
                        self.cancel_stream();
                    }
                }
                (KeyCode::Up, _) => {
                    self.model_picker.prev();
                }
                (KeyCode::Down, _) => {
                    self.model_picker.next();
                }
                (KeyCode::Enter, _) => {
                    if let Some(model_ref) = self.model_picker.selected_ref().map(|s| s.to_string()) {
                        self.model_picker.close();
                        self.handle_input(format!("/model {model_ref}")).await?;
                    }
                }
                (KeyCode::Backspace, _) => {
                    self.model_picker.backspace();
                }
                (KeyCode::Char(c), _) => {
                    self.model_picker.type_char(c);
                }
                _ => {}
            }
            return Ok(());
        }

        // If the session picker overlay is open, intercept keystrokes
        if self.session_picker.visible {
            match (key.code, key.modifiers) {
                (KeyCode::Esc, _) => {
                    self.session_picker.close();
                }
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                    self.session_picker.close();
                    if self.is_loading || self.streaming_active {
                        self.cancel_stream();
                    }
                }
                (KeyCode::Up, _) => {
                    self.session_picker.prev();
                }
                (KeyCode::Down, _) => {
                    self.session_picker.next();
                }
                (KeyCode::Enter, _) => {
                    if let Some(session) = self.session_picker.selected_session() {
                        self.session_picker.close();
                        self.switch_to_session(session).await?;
                    }
                }
                (KeyCode::Backspace, _) => {
                    self.session_picker.backspace();
                }
                (KeyCode::Char(c), _) => {
                    self.session_picker.type_char(c);
                }
                _ => {}
            }
            return Ok(());
        }

        // If the diagnostics overlay is open, intercept keystrokes
        if self.diagnostics_overlay.visible {
            match (key.code, key.modifiers) {
                (KeyCode::Esc, _) => {
                    self.diagnostics_overlay.close();
                }
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                    self.diagnostics_overlay.close();
                    if self.is_loading || self.streaming_active {
                        self.cancel_stream();
                    }
                }
                (KeyCode::Up, _) => {
                    self.diagnostics_overlay.scroll_up(1);
                }
                (KeyCode::Down, _) => {
                    self.diagnostics_overlay.scroll_down(1);
                }
                _ => {}
            }
            return Ok(());
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
            (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                // Toggle paste preview overlay (only meaningful when a paste is collapsed)
                if self.input.collapsed_paste.is_some() {
                    self.input.paste_preview_visible = !self.input.paste_preview_visible;
                }
            }
            (KeyCode::Enter, KeyModifiers::SHIFT) => {
                // Shift+Enter: insert newline in textarea (forward as plain Enter)
                self.input.expand_paste();
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
                // Only expand collapsed paste for keys that modify text content,
                // not navigation keys (arrows, Home, End, F-keys, etc.)
                let is_editing_key = matches!(
                    key.code,
                    KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete
                );
                if is_editing_key {
                    self.input.expand_paste();
                }
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

    /// Update the last Question block to reflect current PendingQuestion state.
    fn sync_question_block(&mut self) {
        if let Some(q) = &self.pending_question {
            if let Some(block) = self.messages.iter_mut().rev().find(|m| matches!(m, MessageBlock::Question { answered: None, .. })) {
                if let MessageBlock::Question { selected, free_text, .. } = block {
                    *selected = q.selected;
                    *free_text = q.free_text.clone();
                }
            }
        }
    }

    /// Mark the last unanswered Question block as answered.
    fn mark_question_answered(&mut self, answer: &str) {
        if let Some(block) = self.messages.iter_mut().rev().find(|m| matches!(m, MessageBlock::Question { answered: None, .. })) {
            if let MessageBlock::Question { answered, .. } = block {
                *answered = Some(answer.to_string());
            }
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

    /// Discard a pending AGENTS.md update and notify the user.
    fn discard_pending_agents_update(&mut self) {
        self.pending_agents_update = None;
        self.messages.push(MessageBlock::System {
            text: "AGENTS.md update discarded.".to_string(),
        });
        self.message_area_state.scroll_to_bottom();
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

        // Also dismiss any pending question prompt
        if let Some(q) = self.pending_question.take() {
            let _ = q.response_tx.send("User cancelled.".to_string());
        }

        if let Some(start) = self.stream_start_time {
            self.frozen_elapsed = Some(start.elapsed());
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
        self.status_line_state.set_activity(Activity::Idle);
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
        if text.starts_with('/') {
            return self.handle_command(&text).await;
        }

        // Ensure we have a session
        self.ensure_session();

        // Clear elapsed timer state from the previous response
        self.stream_start_time = None;
        self.frozen_elapsed = None;

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
        self.frozen_elapsed = None;
        self.stream_start_time = Some(Instant::now());
        self.status_line_state.set_activity(Activity::Thinking);
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
                task_store: Some(Arc::new(self.task_store.clone())),
                lsp_manager: Some(self.lsp_manager.clone()),
            }),
            permission_engine: Some(self.permission_engine.clone()),
            tool_cache: self.tool_cache.clone(),
            cancel_token: cancel_token.clone(),
            context_window: if self.status_line_state.context_window > 0 {
                Some(self.status_line_state.context_window)
            } else {
                None
            },
            interjection_rx,
            usage_writer: self.usage_writer.clone(),
            usage_project_id: self.project.id.clone(),
            usage_session_id: session_id.clone(),
            usage_model_cost: resolved.config.cost.clone(),
            is_plan_mode: {
                use crate::ui::input::AgentMode;
                self.input.mode == AgentMode::Plan
            },
            agent_spawner: Some(stream::AgentSpawner {
                stream_provider: std::sync::Arc::new(stream::OpenAIChatStream::new(
                    client.inner().clone(),
                )),
                primary_model: resolved.api_model_id().to_string(),
                small_model: self.config.small_model.as_ref().and_then(|model_ref| {
                    self.provider_registry.as_ref()?.resolve_model(model_ref).ok().map(|r| r.api_model_id().to_string())
                }),
                project_root: self.project.root.clone(),
                tool_context: ToolContext {
                    project_root: self.project.root.clone(),
                    storage_dir: Some(self.storage.base_dir().clone()),
                    task_store: Some(Arc::new(self.task_store.clone())),
                    lsp_manager: Some(self.lsp_manager.clone()),
                },
                permission_engine: Some(self.permission_engine.clone()),
                context_window: if self.status_line_state.context_window > 0 {
                    Some(self.status_line_state.context_window)
                } else {
                    None
                },
                usage_writer: self.usage_writer.clone(),
                usage_project_id: self.project.id.clone(),
                usage_session_id: session_id.clone(),
                cancel_token: cancel_token.clone(),
                mcp_manager: Some(self.mcp_manager.clone()),
            }),
            mcp_manager: Some(self.mcp_manager.clone()),
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
                self.usage_writer.upsert_session(SessionRecord {
                    session_id: session.id.clone(),
                    project_id: self.project.id.clone(),
                    title: session.title.clone(),
                    model_ref: session.model_ref.clone(),
                    created_at: session.created_at,
                });
                self.current_session = Some(session);
            }
            Err(_) => {
                // Silently continue without persistence if storage fails
            }
        }
    }

    /// Try to auto-generate a title for the session after the first exchange.
    /// Uses `small_model` for async LLM title generation when configured,
    /// otherwise falls back to truncating the first user message.
    fn maybe_generate_title(&mut self) {
        let Some(session) = &self.current_session else {
            return;
        };

        // Don't re-title sessions that already have a non-default title
        // (e.g., after /rename, or after compaction resets exchange_count).
        if session.title != "New session" {
            return;
        }

        let first_user_msg = self.messages.iter().find_map(|m| match m {
            MessageBlock::User { text } => Some(text.clone()),
            _ => None,
        });

        let Some(first_text) = first_user_msg else {
            return;
        };

        let fallback = title_fallback(&first_text);

        // Only use async LLM title gen when small_model is explicitly configured.
        // Unlike compact_model_ref(), we don't fall back to the main model —
        // title gen shouldn't add latency/cost to the primary model.
        let Some(model_ref) = self.config.small_model.clone() else {
            self.apply_session_title(&fallback);
            return;
        };

        let Some(registry) = &self.provider_registry else {
            self.apply_session_title(&fallback);
            return;
        };

        let resolved = match registry.resolve_model(&model_ref) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "failed to resolve title model, using fallback");
                self.apply_session_title(&fallback);
                return;
            }
        };

        let client = match registry.client(&resolved.provider_id) {
            Ok(c) => c.clone(),
            Err(e) => {
                tracing::warn!(error = %e, "failed to get title model client, using fallback");
                self.apply_session_title(&fallback);
                return;
            }
        };

        let session_id = session.id.clone();
        let api_model_id = resolved.api_model_id().to_string();
        let event_tx = self.event_tx.clone();

        tokio::spawn(async move {
            match client
                .simple_chat(&api_model_id, Some(TITLE_SYSTEM_PROMPT), &first_text)
                .await
            {
                Ok(raw) => {
                    let title = sanitize_title(&raw);
                    if title.is_empty() {
                        let _ = event_tx.send(AppEvent::TitleError {
                            session_id,
                            fallback_title: fallback,
                        });
                    } else {
                        let _ = event_tx.send(AppEvent::TitleGenerated { session_id, title });
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "LLM title generation failed, using fallback");
                    let _ = event_tx.send(AppEvent::TitleError {
                        session_id,
                        fallback_title: fallback,
                    });
                }
            }
        });
    }

    /// Apply a title only if `session_id` matches the current session and
    /// the title is still the default "New session" (guards against stale
    /// events after /new or /rename).
    fn apply_title_if_current(&mut self, session_id: &str, title: &str) {
        let should_apply = self
            .current_session
            .as_ref()
            .is_some_and(|s| s.id == session_id && s.title == "New session");
        if should_apply {
            self.apply_session_title(title);
        }
    }

    /// Apply a generated title to the current session, persisting to storage.
    fn apply_session_title(&mut self, title: &str) {
        if title.is_empty() {
            return;
        }
        let Some(session) = &self.current_session else {
            return;
        };
        let mgr = SessionManager::new(&self.storage, &self.project.id);
        let mut session = session.clone();
        if let Err(e) = mgr.rename_session(&mut session, title) {
            tracing::error!(error = %e, "failed to rename session");
        }
        self.usage_writer.update_session_title(&session.id, title);
        self.current_session = Some(session);
        self.update_sidebar();
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
        /// Map priority to a sort key (lower = higher priority).
        fn priority_sort_key(p: Priority) -> u8 {
            match p {
                Priority::High => 0,
                Priority::Medium => 1,
                Priority::Low => 2,
            }
        }

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
        // Sync task list for sidebar: open/in_progress tasks + session-closed tasks
        self.sidebar_state.tasks = self.task_store
            .list_tasks()
            .unwrap_or_default()
            .into_iter()
            .filter(|t| {
                t.status != TaskStatus::Done
                    || self.sidebar_state.session_closed_task_ids.contains(&t.id)
            })
            .map(|t| SidebarTask::from(t))
            .collect();
        // Sort: open/in_progress first (by priority High→Low), then done at bottom
        self.sidebar_state.tasks.sort_by(|a, b| {
            let a_done = a.status == TaskStatus::Done;
            let b_done = b.status == TaskStatus::Done;
            a_done.cmp(&b_done).then_with(|| {
                // Within same done/not-done group, sort by priority (High < Medium < Low)
                priority_sort_key(a.priority).cmp(&priority_sort_key(b.priority))
            })
        });
        self.sidebar_state.tasks.truncate(MAX_SIDEBAR_TASKS);

        // Sync status line state
        if let Some(model) = &self.current_model {
            self.status_line_state.model_name = model.clone();
        }
        if let Some(session) = &self.current_session {
            self.status_line_state.total_tokens = session.token_usage.total_tokens;
        }
        self.status_line_state.last_prompt_tokens = self.last_prompt_tokens;
    }

    /// Eagerly resolve the current model's context window for border color display.
    /// Call after any change to `current_model` (startup, `/model`, session switch).
    fn sync_context_window(&mut self) {
        if let (Some(model_ref), Some(registry)) = (&self.current_model, &self.provider_registry) {
            if let Ok(resolved) = registry.resolve_model(model_ref) {
                self.status_line_state.context_window = resolved.config.context_window as u64;
            }
        }
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
    }

    /// Run all diagnostic checks against current app state.
    /// Called at discrete sync points (LlmFinish, CompactFinish, /new, switch_to_session)
    /// and when the /diagnostics overlay is opened — not per-frame.
    fn collect_diagnostics(&self) -> Vec<crate::diagnostics::DiagnosticCheck> {
        let (cache_hits, cache_misses) = self.tool_cache.lock().unwrap().cache_stats();
        let lsp_servers: Vec<(&str, bool)> =
            self.sidebar_state.lsp_servers.iter()
                .map(|s| (s.binary.as_str(), s.running))
                .collect();
        let system_prompt_len = self.build_system_prompt()
            .map(|s| s.len())
            .unwrap_or(0);
        let total_tokens = self.current_session
            .as_ref()
            .map(|s| s.token_usage.total_tokens)
            .unwrap_or(0);
        let combined_agents = self.combined_agents_content();
        let input = crate::diagnostics::DiagnosticInput {
            agents_md: combined_agents.as_deref(),
            system_prompt_len,
            config: &self.config,
            lsp_servers: &lsp_servers,
            total_tokens,
            exchange_count: self.exchange_count,
            cache_hits,
            cache_misses,
            compaction_count: self.compaction_count,
            session_cost: self.sidebar_state.session_cost,
        };
        crate::diagnostics::run_diagnostics(&input)
    }

    /// Refresh diagnostics summary for the sidebar indicator.
    fn sync_diagnostics(&mut self) {
        let checks = self.collect_diagnostics();
        self.sidebar_state.diagnostics_summary = crate::diagnostics::summarize(&checks);
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
        let is_plan = self.input.mode == AgentMode::Plan;
        tokio::spawn(async move {
            let mut engine = engine.lock().await;
            engine.set_rules(rules);
            engine.set_plan_mode(is_plan);
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

    /// Combine all loaded AGENTS.md files into a single string (for diagnostics).
    fn combined_agents_content(&self) -> Option<String> {
        if self.agents_files.is_empty() {
            None
        } else {
            Some(self.agents_files.iter().map(|f| f.content.as_str()).collect::<Vec<_>>().join("\n\n"))
        }
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

        // Inject open tasks summary
        let session_id = self.current_session.as_ref()
            .map(|s| s.id.as_str())
            .unwrap_or("");
        let task_summary = self.task_store.summary_for_prompt(session_id);
        if !task_summary.is_empty() {
            parts.push(format!("\n## Active Tasks\n\n{task_summary}"));
        }

        if !self.agents_files.is_empty() {
            let mut section = String::from("\n---\n\n## Project Instructions (AGENTS.md)\n");
            for file in &self.agents_files {
                let label = file.path.strip_prefix(&self.project.root)
                    .unwrap_or(&file.path)
                    .display();
                section.push_str(&format!("\n### {label}\n\n{}\n", file.content));
            }
            parts.push(section);
        }

        // Inject MCP resource context (if any servers provide resources)
        // Note: This is a sync context, so we use try_lock + cached resources only
        if let Ok(mgr) = self.mcp_manager.try_lock() {
            if mgr.has_servers() {
                let resources = mgr.all_resources();
                if !resources.is_empty() {
                    let mut section = String::from("\n## MCP Context\n");
                    let mut total_len = 0;
                    for (server_id, resource) in &resources {
                        let name = &resource.name;
                        let desc = resource.description.as_deref().unwrap_or("");
                        let entry = format!("\n- **{server_id}/{name}**: {desc}\n");
                        total_len += entry.len();
                        if total_len > 2000 {
                            section.push_str("\n(additional resources omitted)\n");
                            break;
                        }
                        section.push_str(&entry);
                    }
                    parts.push(section);
                }

                // Add MCP tool guidance
                let tool_defs = mgr.all_tool_defs();
                if !tool_defs.is_empty() {
                    let mut guidance = String::from("\n## MCP Tools\n\nExternal tools provided by MCP servers. \
                        These tools use prefixed names (`mcp__{server}__{tool}`). Use them when native tools \
                        don't cover the task.\n");
                    for (server_id, tool) in &tool_defs {
                        let desc = tool.description.as_deref().unwrap_or("(no description)");
                        let prefixed = crate::mcp::types::prefixed_tool_name(server_id, &tool.name);
                        guidance.push_str(&format!("\n- `{prefixed}`: {desc}"));
                    }
                    guidance.push('\n');
                    parts.push(guidance);
                }
            }
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

    /// Gather project context for AGENTS.md generation.
    ///
    /// Collects file tree, key config files, current AGENTS.md, and recent
    /// conversation messages to give the LLM enough context to produce a
    /// useful AGENTS.md.
    fn gather_project_context(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        // 1. File tree (max_depth 4 = root + 3 subdirectory levels, max 200 entries)
        let mut entries: Vec<String> = Vec::new();
        if let Ok(walker) = ignore::WalkBuilder::new(&self.project.root)
            .hidden(true)
            .git_ignore(true)
            .max_depth(Some(4))
            .build()
            .take(200)
            .collect::<Result<Vec<_>, _>>()
        {
            for entry in walker {
                if let Some(path) = entry.path().strip_prefix(&self.project.root).ok() {
                    entries.push(path.display().to_string());
                }
            }
        }
        if !entries.is_empty() {
            parts.push(format!("## File Tree\n\n```\n{}\n```", entries.join("\n")));
        }

        // 2. Key config files (first 100 lines each)
        let config_files = [
            "Cargo.toml", "package.json", "pyproject.toml", "go.mod",
            "Makefile", "Dockerfile", ".gitignore",
        ];
        for name in &config_files {
            let path = self.project.root.join(name);
            if let Ok(content) = std::fs::read_to_string(&path) {
                let truncated: String = content.lines().take(100).collect::<Vec<_>>().join("\n");
                parts.push(format!("## {name}\n\n```\n{truncated}\n```"));
            }
        }

        // 3. Current AGENTS.md
        if !self.agents_files.is_empty() {
            for file in &self.agents_files {
                parts.push(format!("## Current AGENTS.md ({})\n\n{}", file.path.display(), file.content));
            }
        } else {
            parts.push("## Current AGENTS.md\n\n(No AGENTS.md exists yet)".to_string());
        }

        // 4. Recent conversation (last 5 user messages)
        let user_msgs: Vec<&Message> = self.stored_messages.iter()
            .filter(|m| m.role == Role::User)
            .collect();
        let recent: Vec<&Message> = user_msgs.iter().rev().take(5).rev().copied().collect();
        if !recent.is_empty() {
            let mut convo = String::from("## Recent Conversation\n\n");
            for msg in recent {
                let text = msg.text_content();
                if !text.is_empty() {
                    convo.push_str(&format!("[User]: {text}\n\n"));
                }
            }
            parts.push(convo);
        }

        // Cap total output at ~10K chars
        let mut result = parts.join("\n\n");
        if result.len() > 10_000 {
            let mut end = 10_000;
            while end > 0 && !result.is_char_boundary(end) {
                end -= 1;
            }
            result.truncate(end);
            result.push_str("\n\n(truncated)");
        }
        result
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
                self.stream_start_time = None;
                self.frozen_elapsed = None;
                self.is_loading = false;
                self.exchange_count = 0;
                self.auto_compact_failed = false;
                self.context_warned = false;
                self.last_prompt_tokens = 0;
                self.current_session = None;
                self.session_picker.close();
                // Reset tool result cache for the new session
                *self.tool_cache.lock().unwrap() = ToolResultCache::new(self.project.root.clone());
                // Clear changeset tracking, session-closed tasks, selection, and reset token counters
                // Note: tasks persist across sessions (not cleared on /new)
                self.sidebar_state.changes.clear();
                self.sidebar_state.session_closed_task_ids.clear();
                self.selection_state.clear();
                self.pending_question = None;
                self.pending_agents_update = None;
                self.model_picker.close();
                self.diagnostics_overlay.close();
                self.compaction_count = 0;
                self.autocomplete_state.hide();
                self.ensure_session();
                self.refresh_git_info();
                self.sync_sidebar_tokens();
                self.sync_diagnostics();
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
                    if let Err(e) = mgr.rename_session(&mut session, &title) {
                        tracing::error!(error = %e, "failed to rename session");
                    }
                    self.usage_writer.update_session_title(&session.id, &title);
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
                            self.sync_context_window();
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
                self.diagnostics_overlay.close();
                self.session_picker.close();
                if let Some(registry) = &self.provider_registry {
                    let models = registry.list_models();
                    if models.is_empty() {
                        self.messages.push(MessageBlock::System {
                            text: "No models configured.".to_string(),
                        });
                    } else {
                        let picker_models: Vec<(String, String)> = models
                            .iter()
                            .map(|m| (m.display_ref(), m.config.name.clone()))
                            .collect();
                        let current = self.current_model.as_deref();
                        self.model_picker.open(&picker_models, current);
                    }
                } else {
                    self.messages.push(MessageBlock::Error {
                        text: "No providers configured.".to_string(),
                    });
                }
            }
            Command::Diagnostics => {
                // Close other overlays (mutual exclusivity)
                self.model_picker.close();
                self.session_picker.close();
                // Run diagnostics and open the overlay
                let checks = self.collect_diagnostics();
                self.diagnostics_overlay.open(checks);
            }
            Command::Init => {
                let agents_path = self.project.cwd.join("AGENTS.md");
                if agents_path.exists() {
                    self.messages.push(MessageBlock::System {
                        text: format!("AGENTS.md already exists at {}", agents_path.display()),
                    });
                } else {
                    let default_content = "# AGENTS.md\n\nProject-specific instructions for AI coding assistants.\n\n## Guidelines\n\n- Follow existing code style and conventions.\n- Write clear, concise commit messages.\n- Add tests for new functionality.\n";
                    match std::fs::write(&agents_path, default_content) {
                        Ok(_) => {
                            let new_entry = crate::config::AgentsFile {
                                path: agents_path.clone(),
                                content: default_content.to_string(),
                            };
                            // Maintain root-first ordering: root-level inserts at front
                            if self.project.cwd == self.project.root {
                                self.agents_files.insert(0, new_entry);
                            } else {
                                self.agents_files.push(new_entry);
                            }
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
            Command::AgentsUpdate => {
                // Guard: must not already be streaming/loading
                if self.is_loading || self.streaming_active {
                    self.messages.push(MessageBlock::Error {
                        text: "Cannot update AGENTS.md while streaming.".to_string(),
                    });
                    return Ok(());
                }

                // Guard: must not already have a pending update
                if self.pending_agents_update.is_some() {
                    self.messages.push(MessageBlock::Error {
                        text: "An AGENTS.md update is already pending approval.".to_string(),
                    });
                    return Ok(());
                }

                // Use primary model (not compact/small model — this is analytical work)
                let model_ref = match &self.current_model {
                    Some(r) => r.clone(),
                    None => {
                        self.messages.push(MessageBlock::Error {
                            text: "No model available.".to_string(),
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
                            text: format!("Failed to resolve model: {e}"),
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

                // Gather project context
                let context = self.gather_project_context();

                // Show feedback
                self.messages.push(MessageBlock::System {
                    text: "Analyzing project...".to_string(),
                });
                self.message_area_state.scroll_to_bottom();
                self.is_loading = true;
                self.status_line_state.set_activity(Activity::UpdatingAgents);

                let api_model_id = resolved.api_model_id().to_string();
                let event_tx = self.event_tx.clone();

                tracing::info!(
                    model = %api_model_id,
                    context_len = context.len(),
                    "starting AGENTS.md update"
                );

                // Spawn background LLM task
                tokio::spawn(async move {
                    match client
                        .simple_chat(&api_model_id, Some(AGENTS_UPDATE_SYSTEM_PROMPT), &context)
                        .await
                    {
                        Ok(proposed_content) => {
                            let _ = event_tx.send(AppEvent::AgentsUpdateFinish { proposed_content });
                        }
                        Err(e) => {
                            let _ = event_tx.send(AppEvent::AgentsUpdateError {
                                error: format!("AGENTS.md update failed: {e}"),
                            });
                        }
                    }
                });
            }
            Command::Sessions => {
                if self.is_loading || self.streaming_active {
                    self.messages.push(MessageBlock::Error {
                        text: "Cannot browse sessions while streaming.".to_string(),
                    });
                    return Ok(());
                }
                // Close other overlays (mutual exclusivity)
                self.model_picker.close();
                self.diagnostics_overlay.close();
                let mgr = SessionManager::new(&self.storage, &self.project.id);
                match mgr.list_sessions() {
                    Ok(sessions) if sessions.is_empty() => {
                        self.messages.push(MessageBlock::System {
                            text: "No sessions found.".to_string(),
                        });
                    }
                    Ok(sessions) => {
                        let current_id = self.current_session.as_ref().map(|s| s.id.as_str());
                        self.session_picker.open(&sessions, current_id);
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
                self.status_line_state.set_activity(Activity::Compacting);

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
                    text: "Commands:\n  /new            \u{2014} Start a new session\n  /rename <t>     \u{2014} Rename current session\n  /models         \u{2014} List available models\n  /model <r>      \u{2014} Switch to a model\n  /compact        \u{2014} Compact conversation into a summary\n  /sessions       \u{2014} Browse sessions\n  /tasks          \u{2014} List all tasks\n  /task-new <t>   \u{2014} Create a task\n  /task-done <id> \u{2014} Complete a task\n  /task-show <id> \u{2014} Show task details\n  /task-edit <id> \u{2014} Edit a task (field=value)\n  /epics          \u{2014} List epics\n  /epic-new <t>   \u{2014} Create an epic\n  /export-debug   \u{2014} Export session with logs\n  /init           \u{2014} Create AGENTS.md in project root\n  /agents-update  \u{2014} Update AGENTS.md with LLM analysis\n  /help           \u{2014} Show this help\n  /exit           \u{2014} Quit\n\nKeys:\n  Enter       \u{2014} Send message\n  Shift+Enter \u{2014} Insert newline\n  Tab         \u{2014} Accept autocomplete / toggle Build\u{2013}Plan mode\n  Up/Down     \u{2014} Navigate autocomplete list\n  Ctrl+C      \u{2014} Cancel stream / quit\n  Ctrl+B      \u{2014} Toggle sidebar\n  Mouse wheel \u{2014} Scroll messages\n  Click+drag  \u{2014} Select text (auto-copies to clipboard)".to_string(),
                });
            }
            // -- Task management commands --
            Command::Tasks => {
                let tasks = self.task_store.list_tasks().unwrap_or_default();
                let epics = self.task_store.list_epics().unwrap_or_default();
                if tasks.is_empty() {
                    self.messages.push(MessageBlock::System {
                        text: "No tasks. Use /task-new <title> to create one.".to_string(),
                    });
                } else {
                    let mut output = String::new();
                    // Group tasks by epic
                    for epic in &epics {
                        let epic_tasks: Vec<_> = tasks.iter().filter(|t| t.epic_id.as_deref() == Some(&epic.id)).collect();
                        if !epic_tasks.is_empty() {
                            output.push_str(&format!("## {} ({})\n", epic.title, epic.id));
                            for t in &epic_tasks {
                                let marker = if t.status == crate::task::types::TaskStatus::Done { "x" } else { " " };
                                let bug_label = if t.kind == TaskKind::Bug { " [bug]" } else { "" };
                                output.push_str(&format!("  - [{marker}] {}: {}{bug_label} [{}]\n", t.id, t.title, t.priority));
                            }
                        }
                    }
                    // Standalone tasks (no epic)
                    let standalone: Vec<_> = tasks.iter().filter(|t| t.epic_id.is_none()).collect();
                    if !standalone.is_empty() {
                        if !output.is_empty() { output.push('\n'); }
                        output.push_str("## Standalone Tasks\n");
                        for t in &standalone {
                            let marker = if t.status == crate::task::types::TaskStatus::Done { "x" } else { " " };
                            output.push_str(&format!("  - [{marker}] {}: {} [{}]\n", t.id, t.title, t.priority));
                        }
                    }
                    self.messages.push(MessageBlock::System { text: output.trim_end().to_string() });
                }
                self.update_sidebar();
            }
            Command::TaskNew(title) => {
                match self.task_store.create_task(&title, None, None, None, Priority::default(), TaskKind::Task) {
                    Ok(task) => {
                        self.messages.push(MessageBlock::System {
                            text: format!("Created task: {} \u{2014} {}", task.id, task.title),
                        });
                        self.update_sidebar();
                    }
                    Err(e) => {
                        self.messages.push(MessageBlock::Error {
                            text: format!("Failed to create task: {e}"),
                        });
                    }
                }
            }
            Command::TaskDone(id) => {
                match self.task_store.complete_task(&id) {
                    Ok(task) => {
                        self.messages.push(MessageBlock::System {
                            text: format!("Completed: {} \u{2014} {}", task.id, task.title),
                        });
                        self.update_sidebar();
                    }
                    Err(e) => {
                        self.messages.push(MessageBlock::Error {
                            text: format!("Failed to complete task: {e}"),
                        });
                    }
                }
            }
            Command::TaskShow(id) => {
                match self.task_store.get_task(&id) {
                    Ok(task) => {
                        let epic_info = task.epic_id.as_ref()
                            .and_then(|eid| self.task_store.get_epic(eid).ok())
                            .map(|e| format!("{} ({})", e.title, e.id))
                            .unwrap_or_else(|| "(none)".to_string());
                        let text = format!(
                            "ID: {}\nType: {}\nTitle: {}\nStatus: {}\nPriority: {}\nEpic: {}\nDescription: {}\nCreated: {}",
                            task.id, task.kind, task.title, task.status, task.priority,
                            epic_info,
                            task.description.as_deref().unwrap_or("(none)"),
                            task.created_at.format("%Y-%m-%d %H:%M"),
                        );
                        self.messages.push(MessageBlock::System { text });
                    }
                    Err(e) => {
                        self.messages.push(MessageBlock::Error {
                            text: format!("Task not found: {e}"),
                        });
                    }
                }
            }
            Command::TaskEdit(args_str) => {
                // Parse: "<task-id> field=value field=value ..."
                let parts: Vec<&str> = args_str.splitn(2, ' ').collect();
                let id = parts[0];
                match self.task_store.get_task(id) {
                    Ok(mut task) => {
                        let mut changed = Vec::new();
                        if let Some(kv_str) = parts.get(1) {
                            for pair in kv_str.split_whitespace() {
                                if let Some((key, val)) = pair.split_once('=') {
                                    match key {
                                        "title" => { task.title = val.to_string(); changed.push("title"); }
                                        "priority" => {
                                            match val {
                                                "high" => { task.priority = crate::task::types::Priority::High; changed.push("priority"); }
                                                "medium" => { task.priority = crate::task::types::Priority::Medium; changed.push("priority"); }
                                                "low" => { task.priority = crate::task::types::Priority::Low; changed.push("priority"); }
                                                _ => {
                                                    self.messages.push(MessageBlock::Error {
                                                        text: format!("Invalid priority '{val}'. Use high, medium, or low."),
                                                    });
                                                }
                                            }
                                        }
                                        "status" => {
                                            match val {
                                                "open" => { task.status = crate::task::types::TaskStatus::Open; changed.push("status"); }
                                                "in_progress" | "inprogress" => { task.status = crate::task::types::TaskStatus::InProgress; changed.push("status"); }
                                                "done" => { task.status = crate::task::types::TaskStatus::Done; changed.push("status"); }
                                                _ => {
                                                    self.messages.push(MessageBlock::Error {
                                                        text: format!("Invalid status '{val}'. Use open, in_progress, or done."),
                                                    });
                                                }
                                            }
                                        }
                                        _ => {
                                            self.messages.push(MessageBlock::Error {
                                                text: format!("Unknown field '{key}'. Use title, priority, or status."),
                                            });
                                        }
                                    }
                                }
                            }
                        }
                        if changed.is_empty() {
                            self.messages.push(MessageBlock::Error {
                                text: "No valid fields to update. Usage: /task-edit <id> title=... priority=... status=...".to_string(),
                            });
                        } else if let Err(e) = self.task_store.update_task(&mut task) {
                            self.messages.push(MessageBlock::Error {
                                text: format!("Failed to update task: {e}"),
                            });
                        } else {
                            self.messages.push(MessageBlock::System {
                                text: format!("Updated task {id}: changed {}.", changed.join(", ")),
                            });
                        }
                        self.update_sidebar();
                    }
                    Err(e) => {
                        self.messages.push(MessageBlock::Error {
                            text: format!("Task not found: {e}"),
                        });
                    }
                }
            }
            Command::Epics => {
                let epics = self.task_store.list_epics().unwrap_or_default();
                if epics.is_empty() {
                    self.messages.push(MessageBlock::System {
                        text: "No epics. Use /epic-new <title> to create one.".to_string(),
                    });
                } else {
                    let lines: Vec<String> = epics.iter().map(|e| {
                        let ref_str = e.external_ref.as_deref().unwrap_or("");
                        let ref_part = if ref_str.is_empty() { String::new() } else { format!(" ({ref_str})") };
                        format!("  {} \u{2014} {} [{}]{ref_part}", e.id, e.title, e.status)
                    }).collect();
                    self.messages.push(MessageBlock::System {
                        text: format!("## Epics\n{}", lines.join("\n")),
                    });
                }
            }
            Command::EpicNew(title) => {
                match self.task_store.create_epic(&title, "", None, crate::task::types::Priority::default()) {
                    Ok(epic) => {
                        self.messages.push(MessageBlock::System {
                            text: format!("Created epic: {} \u{2014} {}", epic.id, epic.title),
                        });
                    }
                    Err(e) => {
                        self.messages.push(MessageBlock::Error {
                            text: format!("Failed to create epic: {e}"),
                        });
                    }
                }
            }
        }

        Ok(())
    }
}

/// Enforce a 60-char cap on a title, appending "..." if truncated.
fn truncate_title(text: &str) -> String {
    if text.chars().count() > 60 {
        let truncated: String = text.chars().take(57).collect();
        format!("{truncated}...")
    } else {
        text.to_string()
    }
}

/// Truncate the first user message to produce a sync fallback session title.
/// Takes only the first non-empty line (handles Shift+Enter newlines).
fn title_fallback(text: &str) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(text)
        .trim();
    truncate_title(line)
}

/// Clean up an LLM-generated title: trim whitespace, strip surrounding quotes,
/// remove common preamble prefixes, and enforce a 60-char cap.
fn sanitize_title(raw: &str) -> String {
    // Take only the first non-empty line (LLMs sometimes add explanation after).
    let line = raw
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(raw)
        .trim();

    // Strip one layer of matching surrounding quotes (must be at least 2 chars).
    let stripped = if line.len() >= 2
        && ((line.starts_with('"') && line.ends_with('"'))
            || (line.starts_with('\'') && line.ends_with('\'')))
    {
        &line[1..line.len() - 1]
    } else {
        line
    };

    // Strip common preamble prefix (case-insensitive, ASCII-safe guard).
    let cleaned = if stripped.len() >= 6
        && stripped.is_char_boundary(6)
        && stripped[..6].eq_ignore_ascii_case("title:")
    {
        stripped[6..].trim()
    } else {
        stripped
    };

    truncate_title(cleaned)
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
        let args = json!({"question": "What is this?"});
        assert_eq!(
            extract_args_summary(ToolName::Question, &args),
            "What is this?"
        );
    }

    #[test]
    fn extract_args_summary_question_long_truncates() {
        let long_text = "a".repeat(40);
        let args = json!({"question": long_text});
        let result = extract_args_summary(ToolName::Question, &args);
        assert_eq!(result.chars().count(), 30); // 27 + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn extract_args_summary_task_returns_action() {
        let args = json!({"action": "create", "title": "something"});
        assert_eq!(extract_args_summary(ToolName::Task, &args), "create");
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
            ToolName::Task,
            ToolName::Webfetch,
            ToolName::Memory,
            ToolName::Symbols,
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
    fn diff_content_edit_insert_lines() {
        let args = json!({
            "file_path": "f.rs",
            "operation": "insert_lines",
            "line": 5,
            "content": "new line 1\nnew line 2"
        });
        match extract_diff_content(ToolName::Edit, &args) {
            Some(DiffContent::EditDiff { lines }) => {
                assert_eq!(lines.len(), 3);
                assert_eq!(lines[0], DiffLine::HunkHeader("@@ +5 @@".into()));
                assert_eq!(lines[1], DiffLine::Addition("new line 1".into()));
                assert_eq!(lines[2], DiffLine::Addition("new line 2".into()));
            }
            other => panic!("expected EditDiff, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_edit_insert_lines_empty_content_returns_none() {
        let args = json!({
            "file_path": "f.rs",
            "operation": "insert_lines",
            "line": 1,
            "content": ""
        });
        assert!(extract_diff_content(ToolName::Edit, &args).is_none());
    }

    #[test]
    fn diff_content_edit_delete_lines() {
        let args = json!({
            "file_path": "f.rs",
            "operation": "delete_lines",
            "start_line": 3,
            "end_line": 7
        });
        match extract_diff_content(ToolName::Edit, &args) {
            Some(DiffContent::EditDiff { lines }) => {
                assert_eq!(lines.len(), 2);
                assert_eq!(lines[0], DiffLine::HunkHeader("@@ -3,5 @@".into()));
                assert_eq!(lines[1], DiffLine::Removal("(5 line(s) deleted)".into()));
            }
            other => panic!("expected EditDiff, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_edit_replace_range() {
        let args = json!({
            "file_path": "f.rs",
            "operation": "replace_range",
            "start_line": 2,
            "end_line": 4,
            "content": "replaced1\nreplaced2"
        });
        match extract_diff_content(ToolName::Edit, &args) {
            Some(DiffContent::EditDiff { lines }) => {
                assert_eq!(lines.len(), 4);
                assert_eq!(lines[0], DiffLine::HunkHeader("@@ -2,3 @@".into()));
                assert_eq!(lines[1], DiffLine::Removal("(3 line(s) replaced)".into()));
                assert_eq!(lines[2], DiffLine::Addition("replaced1".into()));
                assert_eq!(lines[3], DiffLine::Addition("replaced2".into()));
            }
            other => panic!("expected EditDiff, got {other:?}"),
        }
    }

    #[test]
    fn diff_content_edit_unknown_operation_returns_none() {
        let args = json!({
            "file_path": "f.rs",
            "operation": "teleport"
        });
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
            ToolName::Task,
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
            ToolName::Task,
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

        let root = PathBuf::from("/tmp/test");
        let project = ProjectInfo {
            root: root.clone(),
            id: "test".to_string(),
            cwd: root,
        };
        let config = Config::default();
        let storage = Storage::new("test-sidebar").expect("test storage");
        let usage_writer = crate::usage::test_usage_writer();
        App::new(project, config, storage, Vec::new(), None, None, Vec::new(), usage_writer)
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

    // -- title_fallback tests --

    #[test]
    fn title_fallback_short_text() {
        assert_eq!(title_fallback("Fix the login bug"), "Fix the login bug");
    }

    #[test]
    fn title_fallback_exactly_60_chars() {
        let text = "a".repeat(60);
        assert_eq!(title_fallback(&text), text);
    }

    #[test]
    fn title_fallback_over_60_chars_truncates() {
        let text = "a".repeat(80);
        let result = title_fallback(&text);
        assert_eq!(result.chars().count(), 60);
        assert!(result.ends_with("..."));
        assert_eq!(&result[..57], "a".repeat(57));
    }

    #[test]
    fn title_fallback_unicode_truncation() {
        // 70 emoji characters — should truncate to 57 + "..."
        let text = "🦀".repeat(70);
        let result = title_fallback(&text);
        assert_eq!(result.chars().count(), 60);
        assert!(result.ends_with("..."));
    }

    // -- sanitize_title tests --

    #[test]
    fn sanitize_title_clean_response() {
        assert_eq!(sanitize_title("Fix login redirect"), "Fix login redirect");
    }

    #[test]
    fn sanitize_title_strips_double_quotes() {
        assert_eq!(sanitize_title("\"Fix login redirect\""), "Fix login redirect");
    }

    #[test]
    fn sanitize_title_strips_single_quotes() {
        assert_eq!(sanitize_title("'Fix login redirect'"), "Fix login redirect");
    }

    #[test]
    fn sanitize_title_strips_title_prefix() {
        assert_eq!(sanitize_title("Title: Fix login redirect"), "Fix login redirect");
    }

    #[test]
    fn sanitize_title_strips_title_prefix_case_insensitive() {
        assert_eq!(sanitize_title("TITLE: Fix login redirect"), "Fix login redirect");
    }

    #[test]
    fn sanitize_title_takes_first_line() {
        assert_eq!(
            sanitize_title("Fix login redirect\nHere is some explanation"),
            "Fix login redirect"
        );
    }

    #[test]
    fn sanitize_title_skips_empty_first_line() {
        assert_eq!(
            sanitize_title("\n  \nFix login redirect\n"),
            "Fix login redirect"
        );
    }

    #[test]
    fn sanitize_title_enforces_60_char_cap() {
        let long = "a".repeat(80);
        let result = sanitize_title(&long);
        assert_eq!(result.chars().count(), 60);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn sanitize_title_trims_whitespace() {
        assert_eq!(sanitize_title("  Fix login redirect  \n"), "Fix login redirect");
    }

    #[test]
    fn sanitize_title_empty_returns_empty() {
        assert_eq!(sanitize_title(""), "");
    }

    #[test]
    fn sanitize_title_combined_quote_and_prefix() {
        // Quotes stripped first, then prefix stripped too
        assert_eq!(sanitize_title("\"Title: Fix login\""), "Fix login");
    }

    #[test]
    fn sanitize_title_single_quote_char_no_panic() {
        assert_eq!(sanitize_title("\""), "\"");
    }

    #[test]
    fn sanitize_title_single_apostrophe_no_panic() {
        assert_eq!(sanitize_title("'"), "'");
    }

    #[test]
    fn sanitize_title_non_ascii_prefix_no_panic() {
        // "Título:" starts with a multibyte char — must not panic on byte-index
        assert_eq!(sanitize_title("Título: Fix"), "Título: Fix");
    }

    // -- title_fallback edge cases --

    #[test]
    fn title_fallback_strips_newlines() {
        assert_eq!(
            title_fallback("Fix bug\nin login.rs"),
            "Fix bug"
        );
    }

    #[test]
    fn title_fallback_empty_returns_empty() {
        assert_eq!(title_fallback(""), "");
    }

    // -- apply_session_title edge cases --

    #[test]
    fn apply_session_title_rejects_empty_string() {
        let (mut app, _dir) = make_test_app_with_storage();
        let session = create_test_session(&app);
        app.current_session = Some(session);

        app.apply_session_title("");

        // Title should remain unchanged
        assert_eq!(app.current_session.as_ref().unwrap().title, "New session");
    }

    // -- maybe_generate_title / apply_session_title tests --

    /// Create a test app backed by an isolated temp directory for storage tests.
    fn make_test_app_with_storage() -> (App, tempfile::TempDir) {
        use crate::config::types::Config;
        use crate::project::ProjectInfo;
        use crate::storage::Storage;
        use std::path::PathBuf;

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
        let app = App::new(project, config, storage, Vec::new(), None, None, Vec::new(), usage_writer);
        (app, dir)
    }

    /// Helper: create a session via SessionManager and return the SessionInfo.
    fn create_test_session(app: &App) -> SessionInfo {
        let mgr = SessionManager::new(&app.storage, &app.project.id);
        mgr.create_session("test/model").expect("create test session")
    }

    #[test]
    fn maybe_generate_title_skips_non_default_title() {
        let (mut app, _dir) = make_test_app_with_storage();
        let session = create_test_session(&app);

        // Rename via manager before handing to app
        {
            let mgr = SessionManager::new(&app.storage, &app.project.id);
            let mut s = session;
            mgr.rename_session(&mut s, "My Custom Title").unwrap();
            app.current_session = Some(s);
        }

        app.messages.push(MessageBlock::User {
            text: "Hello world".to_string(),
        });

        app.maybe_generate_title();

        assert_eq!(app.current_session.as_ref().unwrap().title, "My Custom Title");
    }

    #[test]
    fn maybe_generate_title_sync_fallback_without_small_model() {
        let (mut app, _dir) = make_test_app_with_storage();
        let session = create_test_session(&app);
        app.current_session = Some(session);
        assert!(app.config.small_model.is_none());

        app.messages.push(MessageBlock::User {
            text: "Fix the authentication bug in login.rs".to_string(),
        });

        app.maybe_generate_title();

        assert_eq!(
            app.current_session.as_ref().unwrap().title,
            "Fix the authentication bug in login.rs"
        );
    }

    #[test]
    fn maybe_generate_title_sync_fallback_truncates_long_message() {
        let (mut app, _dir) = make_test_app_with_storage();
        let session = create_test_session(&app);
        app.current_session = Some(session);

        app.messages.push(MessageBlock::User {
            text: "a".repeat(80),
        });

        app.maybe_generate_title();

        let title = &app.current_session.as_ref().unwrap().title;
        assert_eq!(title.chars().count(), 60);
        assert!(title.ends_with("..."));
    }

    #[test]
    fn apply_session_title_updates_sidebar_and_persists() {
        let (mut app, _dir) = make_test_app_with_storage();
        let session = create_test_session(&app);
        let session_id = session.id.clone();
        app.current_session = Some(session);

        app.apply_session_title("My New Title");

        assert_eq!(app.current_session.as_ref().unwrap().title, "My New Title");
        assert_eq!(app.sidebar_state.session_title, "My New Title");

        // Verify persisted to storage
        {
            let mgr = SessionManager::new(&app.storage, &app.project.id);
            let reloaded = mgr.load_session(&session_id).expect("load");
            assert_eq!(reloaded.title, "My New Title");
        }
    }

    #[test]
    fn title_event_guards_stale_session_id() {
        let (mut app, _dir) = make_test_app_with_storage();
        let session = create_test_session(&app);
        app.current_session = Some(session);

        // Stale session ID — apply_title_if_current should be a no-op
        app.apply_title_if_current("stale-id-does-not-match", "LLM Generated Title");

        assert_eq!(app.current_session.as_ref().unwrap().title, "New session");
    }

    #[test]
    fn title_event_guards_renamed_session() {
        let (mut app, _dir) = make_test_app_with_storage();
        let session = create_test_session(&app);
        let session_id = session.id.clone();
        app.current_session = Some(session);

        // User renames the session before async title arrives
        app.apply_session_title("User Chose This");

        // apply_title_if_current with matching session_id should still not overwrite
        app.apply_title_if_current(&session_id, "LLM Generated Title");

        assert_eq!(
            app.current_session.as_ref().unwrap().title,
            "User Chose This"
        );
    }

    // ─── Model picker integration tests ───

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

    // -- diagnostics overlay tests --

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

    // -- sync_context_window tests --

    /// Helper: build a ProviderRegistry with a single test model.
    fn make_test_registry(context_window: u32) -> crate::provider::ProviderRegistry {
        use crate::config::types::{ModelCapabilities, ModelConfig, ProviderConfig};
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
        crate::provider::ProviderRegistry::from_entries(vec![
            ("test".to_string(), provider_config, client),
        ])
    }

    #[test]
    fn sync_context_window_sets_from_registry() {
        let mut app = make_test_app();
        assert_eq!(app.status_line_state.context_window, 0);

        app.provider_registry = Some(make_test_registry(128_000));
        app.current_model = Some("test/test-model".to_string());
        app.sync_context_window();

        assert_eq!(app.status_line_state.context_window, 128_000);
    }

    #[test]
    fn sync_context_window_noop_without_registry() {
        let mut app = make_test_app();
        app.current_model = Some("test/test-model".to_string());
        app.sync_context_window();
        assert_eq!(app.status_line_state.context_window, 0);
    }

    #[test]
    fn sync_context_window_noop_without_model() {
        let mut app = make_test_app();
        app.provider_registry = Some(make_test_registry(128_000));
        app.current_model = None;
        app.sync_context_window();
        assert_eq!(app.status_line_state.context_window, 0);
    }

    #[test]
    fn sync_context_window_invalid_model_preserves_previous() {
        let mut app = make_test_app();
        app.provider_registry = Some(make_test_registry(128_000));
        app.current_model = Some("test/test-model".to_string());
        app.sync_context_window();
        assert_eq!(app.status_line_state.context_window, 128_000);

        // Switch to an invalid model — previous value should be preserved
        app.current_model = Some("nonexistent/model".to_string());
        app.sync_context_window();
        assert_eq!(app.status_line_state.context_window, 128_000);
    }

    #[test]
    fn sync_context_window_updates_on_model_change() {
        let mut app = make_test_app();
        let mut models = std::collections::HashMap::new();
        models.insert(
            "small".to_string(),
            crate::config::types::ModelConfig {
                id: "small".to_string(),
                name: "Small".to_string(),
                context_window: 32_000,
                max_output_tokens: None,
                cost: None,
                capabilities: crate::config::types::ModelCapabilities {
                    tool_call: true,
                    reasoning: false,
                },
            },
        );
        models.insert(
            "large".to_string(),
            crate::config::types::ModelConfig {
                id: "large".to_string(),
                name: "Large".to_string(),
                context_window: 200_000,
                max_output_tokens: None,
                cost: None,
                capabilities: crate::config::types::ModelCapabilities {
                    tool_call: true,
                    reasoning: false,
                },
            },
        );
        let provider_config = crate::config::types::ProviderConfig {
            base_url: "https://api.test.com/v1".to_string(),
            api_key_env: "TEST_KEY".to_string(),
            models,
        };
        let client = crate::provider::client::LlmClient::new("https://api.test.com/v1", "fake");
        app.provider_registry = Some(crate::provider::ProviderRegistry::from_entries(vec![
            ("test".to_string(), provider_config, client),
        ]));

        app.current_model = Some("test/small".to_string());
        app.sync_context_window();
        assert_eq!(app.status_line_state.context_window, 32_000);

        app.current_model = Some("test/large".to_string());
        app.sync_context_window();
        assert_eq!(app.status_line_state.context_window, 200_000);
    }
}
