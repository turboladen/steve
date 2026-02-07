# Steve: Rust TUI AI Coding Agent — Implementation Plan

A simplified opencode clone built in Rust with ratatui.

---

## Progress

| Phase | Description | Status |
|-------|-------------|--------|
| 1 | TUI Skeleton | **Done** |
| 2 | Project Detection + Config + Storage | **Done** |
| 3 | LLM Provider + Non-Streaming Chat | **Done** |
| 4 | Streaming + Full Event Architecture | **Done** |
| 5 | Sessions | **Done** |
| 6 | Read-Only Tools (read, grep, glob, list) | **Done** |
| 7 | Permission System + Write Tools | **Done** |
| 8 | Build/Plan Modes | **Done** |
| 9 | Sidebar + Remaining Tools | **Done** |
| 10 | Commands + Model Picker + Polish | **Done** |

### Lessons Learned

- **tui-textarea 0.7** requires ratatui 0.29 and crossterm 0.28 — pinned to these versions.
- **async-openai 0.32** requires `features = ["chat-completion"]`; types live under `async_openai::types::chat::`, not `async_openai::types::`.
- **jsonc-parser** requires `features = ["serde"]` for `parse_to_serde_value`.
- Config loader should always parse through JSONC parser regardless of file extension (`.json` or `.jsonc`).
- Provider errors should be captured and surfaced to the user, not silently hidden behind "no providers configured".
- **async-openai 0.32 tool types**: `ChatCompletionTools` (plural, enum with `Function` variant wrapping `ChatCompletionTool`), `ChatCompletionMessageToolCalls` (plural, enum with `Function` variant wrapping `ChatCompletionMessageToolCall`). No `ChatCompletionToolType` enum — the type tag comes from the serde enum tag. `ChatCompletionRequestAssistantMessage` requires `audio: None` and `function_call: None` fields (deprecated but still present).
- **tokio_util::sync::CancellationToken** is the cleanest way to cancel spawned stream tasks — pass the token to the task, use `select!` on `cancel_token.cancelled()` alongside stream processing.
- **tracing** must output to a file appender (not stdout) when the TUI owns stdout. `tracing-appender::rolling::daily` + `tracing_appender::non_blocking` handles this cleanly.
- **html2text v0.14** `from_read()` returns `Result<String, Error>`, not `String` — changed from v0.12.

---

## Architecture Overview

Single binary crate with well-defined modules. No workspace — the project is ~30 source files and all modules share core types.

```
User Input → Command Parser → [slash cmd OR chat message]
                                      ↓
                              LLM Stream Task (tokio::spawn)
                                      ↓
                              SSE chunks via async-openai
                                      ↓
                              AppEvent channel (mpsc::unbounded)
                                      ↓
                              App event loop (tokio::select!)
                                      ↓
                              ratatui render
```

### Event Loop Design

All event sources funnel into a single `AppEvent` enum through one `mpsc::UnboundedSender`:

1. **Terminal input** — crossterm `EventStream` (with `event-stream` feature)
2. **LLM streaming** — tokio task sends deltas, tool calls, finish events
3. **Tool execution** — tokio tasks per tool call
4. **Tick timer** — 100ms interval for UI refresh (spinners, etc.)

The main loop uses `tokio::select!` across these sources, then re-renders after every event.

### Permission Handshake

When a tool call needs user permission, the LLM stream task sends a `PermissionRequest` containing a `oneshot::Sender`. The task then `await`s the `oneshot::Receiver`. The main event loop shows the prompt, and when the user responds, sends the reply through the stored sender. This suspends the LLM task at exactly the right point without polling or shared mutable state.

### Tool Call Loop

OpenAI streaming works in steps: each response can contain text and/or tool calls. When tool calls are present, steve executes them (with permission checks), appends results, and sends a new request. This loop runs inside the spawned LLM task:

```
while !done {
    stream = client.chat().create_stream(messages)
    process chunks → text deltas + tool calls
    if no tool_calls { done = true }
    else {
        for call in tool_calls {
            check permission → execute tool → collect result
            messages.push(tool_result)
        }
    }
}
```

---

## Directory Layout

```
src/
  main.rs                    -- Entry point, tokio runtime
  app.rs                     -- App struct, event loop, state management
  event.rs                   -- AppEvent enum (Input, LlmDelta, ToolResult, etc.)
  stream.rs                  -- LLM streaming bridge with tool call loop
  config/
    mod.rs                   -- Config loading (steve.json / steve.jsonc)
    types.rs                 -- Config, ProviderConfig, ModelConfig structs
  provider/
    mod.rs                   -- Provider registry, model resolution
    client.rs                -- async-openai wrapper (custom base URL)
  session/
    mod.rs                   -- Session CRUD, title generation
    message.rs               -- Message, MessagePart, Role types
    types.rs                 -- SessionInfo, TokenUsage
  storage/
    mod.rs                   -- JSON file read/write with fs2 file locking
  project/
    mod.rs                   -- Git root detection, project ID (root commit hash)
  tool/
    mod.rs                   -- Tool trait, ToolRegistry, dispatch
    bash.rs                  -- Shell command execution (tokio::process::Command)
    edit.rs                  -- String replacement in files
    write.rs                 -- File creation/overwrite
    read.rs                  -- File reading with optional line range
    grep.rs                  -- Ripgrep-based content search
    glob.rs                  -- Glob pattern file matching
    list.rs                  -- Directory listing (.gitignore-aware)
    patch.rs                 -- Unified diff application
    webfetch.rs              -- HTTP GET + HTML-to-markdown
    question.rs              -- Ask user questions (oneshot channel to UI)
    todo.rs                  -- Todo list management (rendered in sidebar)
  permission/
    mod.rs                   -- Permission evaluation, rule matching
    types.rs                 -- PermissionAction, Rule, Request, Reply
  agent/
    mod.rs                   -- Build/Plan mode definitions + permission rulesets
  ui/
    mod.rs                   -- Terminal setup/restore, top-level render fn
    layout.rs                -- Layout computation (message area, sidebar, input)
    message_area.rs          -- Scrollable message list with role-based styling
    sidebar.rs               -- Session title, tokens, todos, modified files
    input.rs                 -- Text input with mode indicator (Build/Plan)
    command.rs               -- Slash command parsing (/models, /rename, etc.)
    prompt.rs                -- Permission prompt + question prompt widgets
    theme.rs                 -- Terminal-adaptive ANSI color detection
```

---

## Dependencies (Cargo.toml)

Actual versions in use (adjusted for compatibility):

```toml
[package]
name = "steve"
version = "0.1.0"
edition = "2024"

[dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }
futures = "0.3"

# TUI (pinned to match tui-textarea 0.7's ratatui/crossterm deps)
ratatui = "0.29"
crossterm = { version = "0.28", features = ["event-stream"] }
tui-textarea = "0.7"

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
jsonc-parser = { version = "0.29", features = ["serde"] }

# LLM Client
async-openai = { version = "0.32", features = ["chat-completion"] }

# Error handling
anyhow = "1"

# Utilities
uuid = { version = "1", features = ["v4", "serde"] }
chrono = { version = "0.4", features = ["serde"] }
directories = "6"
fs2 = "0.4"
```

Future deps (added as needed per phase):
- `grep` + `ignore` — Phase 6 (ripgrep-based search tools)
- `diffy` — Phase 7 (unified diff for edit/patch tools)
- `thiserror` — Phase 7 (permission error types)
- `reqwest` + `html2text` — Phase 9 (webfetch tool)
- `tracing` + `tracing-subscriber` + `tracing-appender` — Phase 10 (file logging)

### Notes on Choices

- **`grep` + `ignore`**: Same crates used in init.rs ripgrep implementation. `grep` bundles `grep-regex`, `grep-searcher`, and `grep-matcher`. `ignore` provides `.gitignore`-aware `WalkBuilder`.
- **`async-openai`**: Supports custom base URLs via `OpenAIConfig::new().with_api_base(url)`. Handles SSE parsing, streaming types, tool call schemas.
- **`tui-textarea`**: Multi-line text input with cursor movement for the chat input area.
- **`fs2`**: `FileExt::lock_exclusive()` / `lock_shared()` for safe concurrent JSON file access.
- **`jsonc-parser`**: Handles JSON with comments for `steve.jsonc` config files.
- **`diffy`**: Generates unified diffs for the edit/patch tools and diff display in the UI.
- **No `ropey`**: Not needed for MVP. File edits are simple string find-and-replace operations.

---

## Key Types

### Config (`src/config/types.rs`)

```rust
pub struct Config {
    pub model: Option<String>,                        // "provider/model"
    pub small_model: Option<String>,                  // for title generation
    pub providers: HashMap<String, ProviderConfig>,
}

pub struct ProviderConfig {
    pub base_url: String,                             // "https://api.example.com/v1"
    pub api_key_env: String,                          // env var name for API key
    pub models: HashMap<String, ModelConfig>,
}

pub struct ModelConfig {
    pub id: String,                                   // API model ID
    pub name: String,                                 // display name
    pub context_window: u32,
    pub max_output_tokens: Option<u32>,
    pub cost: Option<ModelCost>,
    pub capabilities: ModelCapabilities,              // { tool_call, reasoning }
}
```

### Session (`src/session/types.rs`, `message.rs`)

```rust
pub struct SessionInfo {
    pub id: String,                                   // UUID v4
    pub project_id: String,                           // git root commit hash
    pub title: String,
    pub model_ref: String,                            // "provider/model"
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub token_usage: TokenUsage,                      // accumulated across steps
}

pub struct Message {
    pub id: String,
    pub session_id: String,
    pub role: Role,                                   // User | Assistant | System
    pub parts: Vec<MessagePart>,
    pub created_at: DateTime<Utc>,
}

pub enum MessagePart {
    Text { text: String },
    Reasoning { text: String },
    ToolCall { call_id, tool_name, input, state: ToolCallState },
    ToolResult { call_id, tool_name, output, title, is_error },
}

pub enum ToolCallState { Pending, Running, Completed { .. }, Error { .. }, Denied { .. } }
```

**Relationship to async-openai types**: For the wire format (API requests/responses), use `async-openai` types directly (`ChatCompletionRequestMessage`, `ChatCompletionMessageToolCall`, etc.). The `Message` / `MessagePart` types above are a thin persistence + UI wrapper — they carry fields the API types don't have (`session_id`, `created_at`, `ToolCallState` with pending/denied states). Conversion functions `Message::to_api_messages()` and `Message::from_stream_delta()` bridge the two.

### Tools (`src/tool/mod.rs`)

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;  // OpenAI function schema
    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput>;
}

pub struct ToolOutput { pub title: String, pub output: String, pub is_error: bool }
```

### Permissions (`src/permission/types.rs`)

```rust
pub enum PermissionAction { Allow, Deny, Ask }
pub struct PermissionRule { pub tool: String, pub pattern: String, pub action: PermissionAction }
pub enum PermissionReply { AllowOnce, AllowAlways { pattern: String }, Deny }
```

### Agent Modes (`src/agent/mod.rs`)

```rust
pub enum AgentMode { Build, Plan }
```

- **Build**: edit/write/patch/bash -> `Ask`, read/grep/glob/list/question/todo/webfetch -> `Allow`
- **Plan**: edit/write/patch -> `Deny`, bash -> `Ask`, read/grep/glob/list/question/todo/webfetch -> `Allow`
- Tab key toggles. Plan mode appends a system prompt suffix telling the LLM it's read-only.
- Tools with `Deny` on all patterns are excluded from the `tools` array sent to the LLM entirely.

### Events (`src/event.rs`)

```rust
pub enum AppEvent {
    Input(crossterm::event::Event),
    Tick,
    LlmDelta { text: String },
    LlmReasoningDelta { text: String },
    LlmToolCallReady { call_id, tool_name, arguments: Value },
    LlmFinish { usage: StepUsage },
    LlmError { error: String },
    ToolResult { call_id, output: ToolOutput },
    PermissionRequest(PermissionRequest),
    Question { question, options, response_tx: oneshot::Sender },
    TitleGenerated { session_id, title: String },
    TodoUpdated { todos: Vec<TodoItem> },
}
```

---

## Storage Layout

Flat JSON files under XDG data directory. Per-project via git root commit hash.

```
~/.local/share/steve/
  storage/
    {project_id}/
      project.json                  -- { last_model, last_session_id }
      permissions.json              -- runtime "always allow" grants
      sessions/
        {session_id}.json           -- SessionInfo
      messages/
        {session_id}/
          {message_id}.json         -- Message with parts
  logs/
    steve.log                       -- tracing output
```

File-per-message avoids read-modify-write races during streaming and limits data loss on crash to one message.

---

## Config File Schema

Located at project root as `steve.json` or `steve.jsonc`:

```jsonc
{
  "$schema": "...",
  // Default model: "provider_id/model_id"
  "model": "mycompany/gpt-4",
  // Small model for title generation
  "small_model": "mycompany/gpt-4o-mini",
  "providers": {
    "mycompany": {
      "base_url": "https://llm.mycompany.com/v1",
      "api_key_env": "MYCOMPANY_API_KEY",
      "models": {
        "gpt-4": {
          "id": "gpt-4",
          "name": "GPT-4",
          "context_window": 128000,
          "max_output_tokens": 4096,
          "capabilities": { "tool_call": true, "reasoning": false },
          "cost": { "input_per_million": 30.0, "output_per_million": 60.0 }
        }
      }
    }
  }
}
```

---

## UI Layout

```
Terminal width >= 120:
+---------------------------------+----------------------+
|  Messages (scrollable)          |  Sidebar (~40 chars)  |
|                                 |  +------------------+ |
|  > User message                 |  | Session: "Fix..." | |
|                                 |  | Model: gpt-4      | |
|  Assistant response...          |  | Tokens: 1.2k/500  | |
|  +- read src/main.rs -------+  |  | Cost: $0.04       | |
|  | (tool output preview)    |   |  |                    | |
|  +---------------------------+  |  | Todos:             | |
|                                 |  | * Read config      | |
|  (thinking...)                  |  | > Fix parser       | |
|                                 |  | o Write tests      | |
|                                 |  |                    | |
|                                 |  | Modified:          | |
|                                 |  | src/main.rs +5 -2  | |
|                                 |  +------------------+ |
+---------------------------------+----------------------+
| [Build] > Type a message...                             |
+---------------------------------------------------------+

Terminal width < 120: sidebar hidden (no overlay for MVP)
```

### Commands

| Command | Action |
|---------|--------|
| `/models` | Open model picker |
| `/rename <title>` | Rename current session |
| `/new` | Start new session |
| `/exit` | Quit |
| `/init` | Create/update AGENTS.md |

### Key Bindings

| Key | Action |
|-----|--------|
| Tab | Toggle Build / Plan mode |
| Enter | Send message |
| Shift+Enter | Newline in input |
| Ctrl+C | Cancel current LLM stream / quit if idle |
| Esc | Dismiss prompt |
| Up/Down | Scroll messages (when not in input) |
| PageUp/PageDown | Fast scroll |
| Mouse scroll | Scroll messages |

---

## Implementation Phases

Each phase produces something runnable.

### Phase 1: TUI Skeleton (Done)

Create the event loop, terminal setup, basic layout with input area. Type text, press Enter to echo it in the message area. Ctrl+C exits.

**Files**: `main.rs`, `app.rs`, `event.rs`, `ui/mod.rs`, `ui/layout.rs`, `ui/input.rs`, `ui/theme.rs`, `ui/message_area.rs`

### Phase 2: Project Detection + Config + Storage (Done)

Detect git root, load `steve.json`/`steve.jsonc`, initialize `~/.local/share/steve/storage/{project_id}/`.

**Files**: `project/mod.rs`, `config/mod.rs`, `config/types.rs`, `storage/mod.rs`

### Phase 3: LLM Provider + Non-Streaming Chat (Done)

Configure a provider from config, send a message, display the full response. Proves the API integration works end-to-end.

**Files**: `provider/mod.rs`, `provider/client.rs`

### Phase 4: Streaming + Full Event Architecture

Replace non-streaming call with SSE streaming. Text appears token-by-token. Implement `stream.rs` with the event channel architecture.

**Files**: `stream.rs` (new), modify `app.rs`, `event.rs`

### Phase 5: Sessions

Persist conversations. Session CRUD, message storage, auto-title generation after first exchange. `/new` command. Resume last session on restart.

**Files**: `session/mod.rs`, `session/types.rs`, `session/message.rs`, modify `app.rs`

### Phase 6: Read-Only Tools (read, grep, glob, list)

Implement the `Tool` trait and four read-only tools. LLM can call them. Tool calls and results render in the message area.

**Files**: `tool/mod.rs`, `tool/read.rs`, `tool/grep.rs`, `tool/glob.rs`, `tool/list.rs`, modify `stream.rs`, `ui/message_area.rs`

**Deps**: add `grep`, `ignore`

### Phase 7: Permission System + Write Tools (edit, write, patch, bash)

Permission evaluation engine. Inline permission prompts. The four write/execute tools.

**Files**: `permission/mod.rs`, `permission/types.rs`, `tool/bash.rs`, `tool/edit.rs`, `tool/write.rs`, `tool/patch.rs`, `ui/prompt.rs`, modify `stream.rs`, `app.rs`

**Deps**: add `diffy`, `thiserror`

### Phase 8: Build/Plan Modes

Agent mode definitions with different permission rulesets. Tab key toggles. Mode indicator in input area. Plan mode blocks write tools entirely.

**Files**: `agent/mod.rs`, modify `app.rs`, `ui/input.rs`

### Phase 9: Sidebar + Remaining Tools

Sidebar panel with session info, token usage, todo list, modified files. Question tool, todo tool, webfetch tool.

**Files**: `ui/sidebar.rs`, `tool/question.rs`, `tool/todo.rs`, `tool/webfetch.rs`, modify `ui/layout.rs`, `app.rs`

**Deps**: add `reqwest`, `html2text`

### Phase 10: Commands + Model Picker + AGENTS.md + Theme + Polish

`/models` picker overlay, `/rename`, `/init`, `/exit`. Terminal-adaptive theme via ANSI OSC 10/11. Load `AGENTS.md` from project root as system instructions. Reasoning sections render distinctly. Tracing to log file.

**Files**: `ui/command.rs`, `ui/theme.rs`, modify `app.rs`

**Deps**: add `tracing`, `tracing-subscriber`, `tracing-appender`

---

## Verification Plan

After each phase, verify:

1. **Phase 1**: `cargo run` -> TUI launches, input works, Ctrl+C exits
2. **Phase 2**: Launch in a git repo -> project ID detected, storage dir created
3. **Phase 3**: Configure `steve.json` with a provider -> send message -> see response
4. **Phase 4**: Send message -> text streams token by token
5. **Phase 5**: Send messages, quit, relaunch -> conversation persists. `/new` starts fresh
6. **Phase 6**: Ask LLM "read src/main.rs" -> tool call renders, result returns, LLM continues
7. **Phase 7**: Ask LLM to edit a file -> permission prompt appears -> allow -> file modified
8. **Phase 8**: Press Tab -> mode toggles, indicator changes. Plan mode denies edits
9. **Phase 9**: Sidebar shows session info. Ask question via tool -> prompt appears in UI
10. **Phase 10**: `/models` shows picker. AGENTS.md loaded. Theme adapts to terminal
