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

### Testing Policy

Every change that introduces new types, trait impls, or behavior must include unit tests. Specifically:

- **Strum-derived enums** (`ToolName`): `FromStr`/`Display` are auto-derived via strum. When adding new variants, just add to the enum — no manual match arms needed. Existing round-trip tests validate the derives.
- **New enums**: `FromStr`/`Display` round-trip for all variants, serde round-trip, rejection of invalid input
- **Match arms**: Prefer explicit variant lists over `_ =>` wildcards — exhaustive matching is a primary safety mechanism
- **Helper methods** (e.g., `is_write_tool()`): Exhaustive assertions covering every variant, not just spot checks
- **Parse functions**: Valid inputs, invalid inputs, edge cases (empty string, extra whitespace)
- **Refactors**: Existing tests passing is necessary but not sufficient — new logic paths need dedicated tests

Run `cargo test` after every change. Aim for tests that break if someone adds a new variant without updating all the relevant match arms.

Logs are written to `{data_dir}/logs/steve.log` (daily rolling via `tracing-appender`). Data dir is resolved via `directories::ProjectDirs` — on macOS: `~/Library/Application Support/steve/`, on Linux: `~/.local/share/steve/`.

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
| Enter | Send message |
| Shift+Enter | Insert newline |
| Tab | Cycle autocomplete / toggle Build–Plan mode |
| Ctrl+C | Cancel stream (first press) / quit (second press) |
| Ctrl+B | Toggle sidebar (auto → hide → show → auto) |
| Mouse wheel | Scroll messages |

## Architecture

Single binary crate (~40 source files), no workspace. All modules share core types.

### Event-Driven Architecture

Everything funnels into a single `AppEvent` enum through one `mpsc::UnboundedSender`:

1. **Terminal input** — crossterm `EventStream`
2. **LLM streaming** — tokio task sends deltas, tool calls, finish events
3. **Tool execution** — synchronous tool handlers called from the stream task
4. **Tick timer** — 100ms interval for UI refresh

The main loop in `app.rs` uses `tokio::select!` across these sources, then re-renders after every event.

### LLM Stream + Tool Call Loop (`stream.rs`)

A spawned tokio task opens an SSE stream via async-openai, processes chunks (sending `AppEvent::LlmDelta` to the UI), and accumulates tool call fragments. When the stream finishes, it checks for valid tool call data (non-empty `id` and `function_name`) regardless of `finish_reason` — some providers (e.g., Fuel iX/litellm) don't reliably set `FinishReason::ToolCalls`. Tool calls with invalid/truncated JSON arguments (common when `finish_reason=Length`) are filtered out before execution. Valid tool calls are executed (with permission checks), results appended as `ChatCompletionRequestToolMessage`s, and the loop continues until no valid tool calls remain.

Cancellation uses `tokio_util::sync::CancellationToken` with `select!` — the token is checked before each LLM call, during chunk processing, and between tool executions.

### Permission Handshake

When a tool needs user permission, the stream task sends a `PermissionRequest` containing a `oneshot::Sender`, then awaits the `oneshot::Receiver`. The main event loop shows the prompt; when the user responds (y/n/a), it sends the reply through the stored sender. This suspends the stream task at exactly the right point without polling.

### Agent Modes

- **Build mode** (default): read tools auto-allowed, write/execute tools require permission (Ask)
- **Plan mode**: read tools auto-allowed, write tools denied entirely (excluded from LLM tool list), bash requires permission

Tab toggles between modes. Mode rules live in `permission/mod.rs` as `build_mode_rules()` / `plan_mode_rules()`.

### Compaction (`/compact`)

Summarizes the conversation into a single message to reclaim context window space. Uses a non-streaming `LlmClient::simple_chat()` call in a background tokio task, communicating results back via `AppEvent::CompactFinish` / `AppEvent::CompactError`. Uses `small_model` if configured, otherwise falls back to the main model. On completion, old messages are deleted from storage and replaced with a single assistant message containing the summary. Auto-compact triggers after `LlmFinish` when `session.token_usage.total_tokens >= context_window * 0.80` (controlled by `auto_compact` config). If compaction fails (`CompactError`), `auto_compact_failed` is set to suppress retries for the rest of the session (reset on `/new`). Manual `/compact` still works.

### Context Management (`context/`)

Reduces LLM API token usage via two subsystems in `src/context/`:
- **Compressor** (`compressor.rs`): Before each LLM API call in the tool loop, replaces already-seen tool results with compact heuristic summaries (e.g., `"[Previously read: src/main.rs, 150 lines, Rust]"`). Preserves `tool_call_id` for valid conversation structure.
- **Cache** (`cache.rs`): Session-scoped `ToolResultCache` lives in `App` behind `Arc<std::sync::Mutex<ToolResultCache>>`, passed to the stream task via `StreamRequest`. Maps `(tool_name, canonical_args)` → cached output. All tool cache keys normalize paths via `normalize_path()` (resolves relative paths against project root) for consistent matching. On cache hit, returns a compact reference instead of re-executing. Invalidated when write ops modify files — grep/glob entries are wholesale-invalidated since they can't be tracked by individual path. Reset on `/new` session.

**Critical invariant**: Write tools (`edit`, `write`, `patch`) and the `memory` tool (which writes to disk) must never run in the parallel execution phase — they must go through the sequential phase for proper cache invalidation, even if they have `AllowAlways` permission.

The compressor also runs an aggressive pruning pass (compressing ALL tool results including current iteration) when estimated token usage exceeds 60% of the context window. The `read` tool enforces a 2000-line default cap (`max_lines` parameter), `bash` uses head+tail truncation at 20KB, and `grep` truncates individual match lines to 200 chars. A 60% context warning is shown to the user before auto-compact triggers at 80%.

### Parallel Tool Execution (`stream.rs`)

The tool call loop partitions pending tool calls into two phases:
1. **Phase 2 (parallel)**: Read-only tools with `Allow` permission run via `spawn_blocking`. Write tools are excluded even with `AllowAlways`.
2. **Phase 3 (sequential)**: Permission-required tools (Ask/Deny) and write tools. Handles permission handshake and cache invalidation.

Every `tool_call_id` in an assistant message **must** have a corresponding tool result message — missing one causes an API error. The parallel phase uses `unwrap_or_else` with an error fallback to guarantee this.

### Tool System (`tool/mod.rs`)

Tools are registered in `ToolRegistry` as `ToolEntry` structs containing a `ToolDef` (name, description, JSON schema) and a handler closure `Fn(Value, ToolContext) -> Result<ToolOutput>`. Tools are synchronous (not async) — they run inside the stream task's spawned tokio task.

Available tools: `read`, `grep`, `glob`, `list`, `edit`, `write`, `patch`, `bash`, `question`, `todo`, `webfetch`, `memory`.

### Storage (`storage/mod.rs`)

Flat JSON files under `{data_dir}/storage/{project_id}/` (see Data Locations for platform paths). Key paths map to filesystem paths: `["sessions", "abc123"]` → `sessions/abc123.json`. Uses `fs2` file locking (shared for reads, exclusive for writes) and atomic writes via tmp+rename.

Project ID is derived from the git root commit hash (deterministic across clones). Falls back to a hash of CWD for non-git directories.

Messages are stored one-per-file under `messages/{session_id}/{message_id}.json` to avoid read-modify-write races during streaming.

### UI (`ui/`)

Built with ratatui 0.29 + crossterm 0.28 + tui-textarea 0.7 (version-pinned for compatibility). Sidebar appears at terminal width >= 120. The TUI owns stdout, so all logging goes to a file via `tracing-appender`.

Messages render as `MessageBlock` variants: `User`, `Assistant` (with thinking/tool_groups), `System`, `Error`, `Permission`. Styled per-variant in `message_area.rs`. Permission prompts render as bold yellow blocks with highlighted key letters.

The input area is a 2-line starship-style prompt: context line (`[Mode] ~/path tokens/ctx (%)`) above a `> ` chevron input. No status bar — activity spinner displays inline in the message area via `activity: Option<(char, String)>` parameter. `render_input` takes an `InputContext` struct. Sidebar visibility uses `sidebar_override: Option<bool>` (None=auto, Some=forced).

Auto-scroll calculates content height using wrapped line widths (not `lines.len()`) since `Paragraph` uses `Wrap { trim: false }`. This is critical — using unwrapped line count causes scroll to undershoot on long messages, hiding new content below the visible area. The height sum uses `u32` internally, capped at `u16::MAX` to prevent overflow on very long conversations.

## Key Dependency Notes

- **strum 0.28** derives `EnumString`, `Display`, `IntoStaticStr` on `ToolName`. Use `IntoStaticStr` (not `AsRefStr`) when you need `&'static str` — `AsRefStr` returns `&str` tied to `&self` lifetime
- **wait-timeout 0.2** provides `ChildExt::wait_timeout()` for bash tool timeout enforcement. Requires `Stdio::piped()` + `spawn()` (not `output()`)
- **mpatch 1.3** applies unified diffs with fuzzy matching via `patch_content_str()`. Always appends trailing newline — `apply_unified_diff()` in `patch.rs` post-processes to preserve original newline behavior
- **tui-textarea 0.7** requires ratatui 0.29 and crossterm 0.28 — do not upgrade independently
- **async-openai 0.32** requires `features = ["chat-completion"]`; types live under `async_openai::types::chat::`, not `async_openai::types::`
- **async-openai 0.32 tool types**: `ChatCompletionTools` (plural enum with `Function` variant), `ChatCompletionMessageToolCalls` (plural enum with `Function` variant). `ChatCompletionRequestAssistantMessage` requires `audio: None` and `function_call: None` fields. `ChatCompletionStreamOptions` has `include_usage` and `include_obfuscation` fields
- **stream_options is required**: `CreateChatCompletionRequest` must include `stream_options: Some(ChatCompletionStreamOptions { include_usage: Some(true), .. })` — without it, token usage is never reported and auto-compact cannot trigger
- **jsonc-parser** requires `features = ["serde"]` for `parse_to_serde_value`
- **html2text v0.14** `from_read()` returns `Result<String, Error>`, not `String`
- **tracing** outputs to file appender, never stdout (TUI owns stdout)
- **No `unreachable!()` in stream tasks** — panics in the stream tokio task crash silently. Use graceful error handling with `tracing::error!` instead
- **No `dirs` crate** — use `std::env::var("HOME")` for home directory detection, or `directories::ProjectDirs` for app data paths
- **`Storage::new(project_id: &str)`** returns `Result<Self>` — takes a project ID string (not a path). For tests: `Storage::new("test-name").expect("test storage")`
- `AGENTS.md` in the project root is optional — if present, it's loaded at startup and injected as part of the system prompt. Create one with `/init`
- **File locking (Rust 2024)**: `std::fs::File` has native `lock()` (exclusive), `lock_shared()`, `unlock()` methods. `fs2::FileExt` still provides `lock_exclusive()` (no native equivalent with that name) but `lock_shared`/`unlock` shadow the trait — importing `fs2::FileExt` triggers unused-import warnings for those
- **`ToolContext` fields**: `project_root: PathBuf` and `storage_dir: Option<PathBuf>`. In tests, use `storage_dir: None` unless testing the memory tool

## Provider Compatibility

Steve targets any OpenAI-compatible API. Known quirks with non-OpenAI providers (e.g., Fuel iX/litellm):
- **`finish_reason`**: May not be `ToolCalls` even when tool calls are streamed — detect tool calls by checking for valid data, not finish reason
- **`finish_reason=Length`**: Truncates the last tool call's JSON arguments mid-stream — validate JSON with `serde_json::from_str` before execution, drop invalid entries
- **`stream_options`**: `include_usage: Some(true)` works with Fuel iX; without it, token usage is never reported

## Data Locations

- **Data dir**: macOS `~/Library/Application Support/steve/`, Linux `~/.local/share/steve/` (via `directories::ProjectDirs`)
- **Storage**: `{data_dir}/storage/{project_id}/` — sessions, messages, project metadata
- **Logs**: `{data_dir}/logs/steve.log.YYYY-MM-DD` — daily rolling tracing output (date-suffixed by `tracing-appender`)
