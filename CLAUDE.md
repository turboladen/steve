# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is Steve?

Steve is a Rust TUI AI coding agent — a simplified [opencode](https://opencode.ai) clone built with ratatui. It connects to any OpenAI-compatible LLM API, streams responses token-by-token, and provides a tool-calling loop that lets the LLM read, search, edit, and execute code within the user's project.

## Build & Run

```bash
cargo build            # Build (debug)
cargo build --release  # Build (release)
cargo run              # Run the TUI (requires steve.json config in project root)
cargo check            # Type-check without building
RUST_LOG=steve=debug cargo run  # Override log level (default: steve=info)
```

The project uses Rust edition 2024.

```bash
cargo test              # Run all tests
```

Logs are written to `~/.local/share/steve/logs/steve.log` (daily rolling via `tracing-appender`).

## Configuration

Steve reads `steve.json` or `steve.jsonc` from the project root. Config is always parsed through the JSONC parser regardless of extension. The config defines providers (OpenAI-compatible API endpoints), models, and which environment variable holds each provider's API key.

Model references use `"provider_id/model_id"` format throughout (config, commands, internal types).

Optional top-level fields: `small_model` (used for compaction/summarization, falls back to `model`), `auto_compact` (default `true` — auto-compacts at 80% context window usage).

Example `steve.json`:
```jsonc
{
  "model": "openai/gpt-4o",
  // "small_model": "openai/gpt-4o-mini",  // optional: used for /compact
  // "auto_compact": true,                  // optional: default true
  "providers": {
    "openai": {
      "base_url": "https://api.openai.com/v1",
      "api_key_env": "OPENAI_API_KEY",
      "models": {
        "gpt-4o": {
          "id": "gpt-4o",
          "name": "GPT-4o",
          "context_window": 128000,
          "capabilities": { "tool_call": true, "reasoning": false }
        }
      }
    }
  }
}
```

## Commands & Keys

| Command | Action |
|---------|--------|
| `/new` | Start a new session |
| `/rename <title>` | Rename current session |
| `/models` | List available models |
| `/model <ref>` | Switch to a model (e.g., `/model openai/gpt-4o`) |
| `/compact` | Compact conversation into a summary (frees context window) |
| `/init` | Create AGENTS.md in project root |
| `/help` | Show help |
| `/exit` | Quit |

| Key | Action |
|-----|--------|
| Tab | Toggle Build/Plan mode |
| Ctrl+C | Cancel current stream (first press) / quit (second press) |
| Enter | Send message |
| Mouse wheel | Scroll messages |

## Architecture

Single binary crate (33 source files), no workspace. All modules share core types.

### Event-Driven Architecture

Everything funnels into a single `AppEvent` enum through one `mpsc::UnboundedSender`:

1. **Terminal input** — crossterm `EventStream`
2. **LLM streaming** — tokio task sends deltas, tool calls, finish events
3. **Tool execution** — synchronous tool handlers called from the stream task
4. **Tick timer** — 100ms interval for UI refresh

The main loop in `app.rs` uses `tokio::select!` across these sources, then re-renders after every event.

### LLM Stream + Tool Call Loop (`stream.rs`)

A spawned tokio task opens an SSE stream via async-openai, processes chunks (sending `AppEvent::LlmDelta` to the UI), and accumulates tool call fragments. When the stream finishes with `FinishReason::ToolCalls`, it executes each tool (with permission checks), appends results as `ChatCompletionRequestToolMessage`s, and loops back to the LLM. This continues until the LLM produces a response with no tool calls.

Cancellation uses `tokio_util::sync::CancellationToken` with `select!` — the token is checked before each LLM call, during chunk processing, and between tool executions.

### Permission Handshake

When a tool needs user permission, the stream task sends a `PermissionRequest` containing a `oneshot::Sender`, then awaits the `oneshot::Receiver`. The main event loop shows the prompt; when the user responds (y/n/a), it sends the reply through the stored sender. This suspends the stream task at exactly the right point without polling.

### Agent Modes

- **Build mode** (default): read tools auto-allowed, write/execute tools require permission (Ask)
- **Plan mode**: read tools auto-allowed, write tools denied entirely (excluded from LLM tool list), bash requires permission

Tab toggles between modes. Mode rules live in `permission/mod.rs` as `build_mode_rules()` / `plan_mode_rules()`.

### Compaction (`/compact`)

Summarizes the conversation into a single message to reclaim context window space. Uses a non-streaming `LlmClient::simple_chat()` call in a background tokio task, communicating results back via `AppEvent::CompactFinish` / `AppEvent::CompactError`. Uses `small_model` if configured, otherwise falls back to the main model. On completion, old messages are deleted from storage and replaced with a single assistant message containing the summary. Auto-compact triggers after `LlmFinish` when `session.token_usage.total_tokens >= context_window * 0.80` (controlled by `auto_compact` config).

### Context Management (`context/`)

Reduces LLM API token usage via two subsystems in `src/context/`:
- **Compressor** (`compressor.rs`): Before each LLM API call in the tool loop, replaces already-seen tool results with compact heuristic summaries (e.g., `"[Previously read: src/main.rs, 150 lines, Rust]"`). Preserves `tool_call_id` for valid conversation structure.
- **Cache** (`cache.rs`): Session-scoped `ToolResultCache` maps `(tool_name, canonical_args)` → cached output. On cache hit, returns a compact reference instead of re-executing. Invalidated when write ops modify files — grep/glob entries are wholesale-invalidated since they can't be tracked by individual path.

**Critical invariant**: Write tools (`edit`, `write`, `patch`) must never run in the parallel execution phase — they must go through the sequential phase for proper cache invalidation, even if they have `AllowAlways` permission.

### Parallel Tool Execution (`stream.rs`)

The tool call loop partitions pending tool calls into two phases:
1. **Phase 2 (parallel)**: Read-only tools with `Allow` permission run via `spawn_blocking`. Write tools are excluded even with `AllowAlways`.
2. **Phase 3 (sequential)**: Permission-required tools (Ask/Deny) and write tools. Handles permission handshake and cache invalidation.

Every `tool_call_id` in an assistant message **must** have a corresponding tool result message — missing one causes an API error. The parallel phase uses `unwrap_or_else` with an error fallback to guarantee this.

### Tool System (`tool/mod.rs`)

Tools are registered in `ToolRegistry` as `ToolEntry` structs containing a `ToolDef` (name, description, JSON schema) and a handler closure `Fn(Value, ToolContext) -> Result<ToolOutput>`. Tools are synchronous (not async) — they run inside the stream task's spawned tokio task.

Available tools: `read`, `grep`, `glob`, `list`, `edit`, `write`, `patch`, `bash`, `question`, `todo`, `webfetch`.

### Storage (`storage/mod.rs`)

Flat JSON files under `~/.local/share/steve/storage/{project_id}/`. Key paths map to filesystem paths: `["sessions", "abc123"]` → `sessions/abc123.json`. Uses `fs2` file locking (shared for reads, exclusive for writes) and atomic writes via tmp+rename.

Project ID is derived from the git root commit hash (deterministic across clones). Falls back to a hash of CWD for non-git directories.

Messages are stored one-per-file under `messages/{session_id}/{message_id}.json` to avoid read-modify-write races during streaming.

### UI (`ui/`)

Built with ratatui 0.29 + crossterm 0.28 + tui-textarea 0.7 (version-pinned for compatibility). Sidebar appears at terminal width >= 120. The TUI owns stdout, so all logging goes to a file via `tracing-appender`.

Messages render with role-based styling via `DisplayRole` enum: `User`, `Assistant`, `Tool`, `ToolResult`, `Error`, `System`, `Permission` — each mapped to distinct theme colors in `message_area.rs`.

Auto-scroll calculates content height using wrapped line widths (not `lines.len()`) since `Paragraph` uses `Wrap { trim: false }`. This is critical — using unwrapped line count causes scroll to undershoot on long messages, hiding new content below the visible area.

## Key Dependency Notes

- **tui-textarea 0.7** requires ratatui 0.29 and crossterm 0.28 — do not upgrade independently
- **async-openai 0.32** requires `features = ["chat-completion"]`; types live under `async_openai::types::chat::`, not `async_openai::types::`
- **async-openai 0.32 tool types**: `ChatCompletionTools` (plural enum with `Function` variant), `ChatCompletionMessageToolCalls` (plural enum with `Function` variant). `ChatCompletionRequestAssistantMessage` requires `audio: None` and `function_call: None` fields. `ChatCompletionStreamOptions` has `include_usage` and `include_obfuscation` fields
- **stream_options is required**: `CreateChatCompletionRequest` must include `stream_options: Some(ChatCompletionStreamOptions { include_usage: Some(true), .. })` — without it, token usage is never reported and auto-compact cannot trigger
- **jsonc-parser** requires `features = ["serde"]` for `parse_to_serde_value`
- **html2text v0.14** `from_read()` returns `Result<String, Error>`, not `String`
- **tracing** outputs to file appender, never stdout (TUI owns stdout)
- **No `unreachable!()` in stream tasks** — panics in the stream tokio task crash silently. Use graceful error handling with `tracing::error!` instead
- `AGENTS.md` in the project root is optional — if present, it's loaded at startup and injected as part of the system prompt. Create one with `/init`

## Data Locations

- **Storage**: `~/.local/share/steve/storage/{project_id}/` — sessions, messages, project metadata
- **Logs**: `~/.local/share/steve/logs/steve.log` — daily rolling tracing output
