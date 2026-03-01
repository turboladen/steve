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
- **Exhaustive test loops**: When iterating all variants and branching on predicates, use `if/else if/else` (not independent `if` blocks) so every variant hits at least one assertion
- **Helper methods** (e.g., `is_write_tool()`): Exhaustive assertions covering every variant, not just spot checks
- **Parse functions**: Valid inputs, invalid inputs, edge cases (empty string, extra whitespace)
- **Refactors**: Existing tests passing is necessary but not sufficient — new logic paths need dedicated tests

Run `cargo test` after every change. Aim for tests that break if someone adds a new variant without updating all the relevant match arms.

**Test infrastructure**:
- **UI rendering**: `render_to_buffer(width, height, draw_fn)` in `ui/mod.rs` (`#[cfg(test)]`) creates a headless ratatui `TestBackend` for buffer assertions. `make_test_app()` in `app.rs` constructs a minimal `App` for rendering tests
- **Storage**: `Storage::with_base(path)` (`#[cfg(test)]`) bypasses `directories::ProjectDirs` for temp-dir-based tests
- **Stream**: `MockChatStream` in `stream.rs` (`#[cfg(test)]`) provides canned SSE responses for integration tests. Use `with_test_manager(|mgr| { ... })` callback pattern for `SessionManager` tests (avoids `Box::leak` lifetime hacks)
- **Assertions**: Never use trivially-true assertions like `!a.is_empty() || !b.is_empty()` — verify the specific behavior under test

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

The stream is decoupled from async-openai via the `ChatStreamProvider` trait (`#[async_trait]`). Production uses `OpenAIChatStream`; tests use `MockChatStream` which returns pre-built response chunks from a `VecDeque` (supports multi-call tool loop scenarios). Builder helpers: `text_delta()`, `tool_call_chunk()`, `finish_chunk()`.

### Permission Handshake

When a tool needs user permission, the stream task sends a `PermissionRequest` containing a `oneshot::Sender`, then awaits the `oneshot::Receiver`. The main event loop shows the prompt; when the user responds (y/n/a), it sends the reply through the stored sender. This suspends the stream task at exactly the right point without polling.

**Critical**: Permission and System blocks are interleaved into `self.messages` during the stream. Streaming event handlers (`LlmDelta`, `LlmReasoning`, `LlmToolCallStreaming`, `LlmToolCall`, `ToolResult`) must use `last_assistant_mut()` to find the correct Assistant block — **not** `messages.last_mut()`, which may return a Permission or System block after a permission prompt.

### Agent Modes

- **Build mode** (default): read tools auto-allowed, write/execute tools require permission (Ask)
- **Plan mode**: read tools auto-allowed, write tools denied entirely (excluded from LLM tool list), bash requires permission

Tab toggles between modes. Mode rules live in `permission/mod.rs` as `build_mode_rules()` / `plan_mode_rules()`. Auto-allowed in both modes: read/grep/glob/list (read-only) + memory/todo/question (utility). Unmatched tools default to `Ask` — new tools must be added to the rules or they'll silently require permission prompts.

### Compaction (`/compact`)

Summarizes the conversation into a single message to reclaim context window space. Uses a non-streaming `LlmClient::simple_chat()` call in a background tokio task, communicating results back via `AppEvent::CompactFinish` / `AppEvent::CompactError`. Uses `small_model` if configured, otherwise falls back to the main model. On completion, old messages are deleted from storage and replaced with a single assistant message containing the summary. Auto-compact triggers after `LlmFinish` when `last_prompt_tokens >= context_window * 0.80` (controlled by `auto_compact` config). If compaction fails (`CompactError`), `auto_compact_failed` is set to suppress retries for the rest of the session (reset on `/new`). Manual `/compact` still works.

### Context Management (`context/`)

Reduces LLM API token usage via two subsystems in `src/context/`:
- **Compressor** (`compressor.rs`): Before each LLM API call in the tool loop, replaces already-seen tool results with compact heuristic summaries (e.g., `"[Previously read: src/main.rs, 150 lines, Rust]"`). Preserves `tool_call_id` for valid conversation structure.
- **Cache** (`cache.rs`): Session-scoped `ToolResultCache` lives in `App` behind `Arc<std::sync::Mutex<ToolResultCache>>`, passed to the stream task via `StreamRequest`. Maps `(tool_name, canonical_args)` → cached output. All tool cache keys normalize paths via `normalize_path()` (resolves relative paths against project root) for consistent matching. On cache hit, returns a compact reference instead of re-executing. Invalidated when write ops modify files — grep/glob entries are wholesale-invalidated since they can't be tracked by individual path. Reset on `/new` session.

**Critical invariant**: Write tools (`edit`, `write`, `patch`) and the `memory` tool (which writes to disk) must never run in the parallel execution phase — they must go through the sequential phase for proper cache invalidation, even if they have `AllowAlways` permission.

The compressor also runs an aggressive pruning pass (compressing ALL tool results including current iteration) when estimated token usage exceeds 60% of the context window. The `read` tool enforces a 2000-line default cap (`max_lines` parameter), `bash` uses head+tail truncation at 20KB, and `grep` truncates individual match lines to 200 chars. A 60% context warning is shown to the user before auto-compact triggers at 80%.

### Token Pipeline

Two token metrics — do not confuse them:
- **`last_prompt_tokens`** (per-call): API's reported `prompt_tokens` from the most recent call. Represents actual context window pressure. Used by input bar display and `check_context_warning()` (60% threshold).
- **`total_tokens`** (cumulative): Sum across all API calls in the session. Used by sidebar cost display. Both `should_auto_compact()` (80%) and `check_context_warning()` (60%) use `last_prompt_tokens`.

`LlmUsageUpdate` events send per-call values during tool loops for live UI updates. `LlmFinish` sends accumulated `total_usage` for storage. The `LlmFinish` handler must NOT overwrite `last_prompt_tokens` — the last `LlmUsageUpdate` already set the correct value.

**Sidebar token display** uses a two-tier approach: `LlmUsageUpdate` accumulates (`+=`) into `sidebar_state.{prompt,completion,total}_tokens` for live feedback during tool loops. `sync_sidebar_tokens()` reconciles with authoritative `session.token_usage` at discrete sync points: `LlmFinish` (after `add_usage`), `/new`, `switch_to_session`, `resume_or_new_session`, `CompactFinish`. `update_sidebar()` does NOT sync tokens — it reads stale session data during tool loops.

### Parallel Tool Execution (`stream.rs`)

The tool call loop partitions pending tool calls into two phases:
1. **Phase 2 (parallel)**: Read-only tools with `Allow` permission run via `spawn_blocking`. Write tools are excluded even with `AllowAlways`.
2. **Phase 3 (sequential)**: Permission-required tools (Ask/Deny) and write tools. Handles permission handshake and cache invalidation.

Every `tool_call_id` in an assistant message **must** have a corresponding tool result message — missing one causes an API error. The parallel phase uses `unwrap_or_else` with an error fallback to guarantee this.

**Event ordering invariant**: Phase 2 results are emitted by iterating `auto_allowed` in original index order (not completion order), so `LlmToolCall`/`ToolResult` pairs arrive in the same order the calls were added. `complete_tool_call()` in `message_block.rs` relies on this — it uses forward search to match results to calls by `tool_name`.

### Tool System (`tool/mod.rs`)

Tools are registered in `ToolRegistry` as `ToolEntry` structs containing a `ToolDef` (name, description, JSON schema) and a handler closure `Fn(Value, ToolContext) -> Result<ToolOutput>`. Tools are synchronous (not async) — they run inside the stream task's spawned tokio task.

Available tools: `read`, `grep`, `glob`, `list`, `edit`, `write`, `patch`, `bash`, `question`, `todo`, `webfetch`, `memory`.

`TOOL_GUIDANCE` in `app.rs` appends two sections to the system prompt: `## Task Planning` (mandatory todo usage for multi-step tasks — must stay prominent/first) and `## Tool Usage Guidelines` (context-efficiency tips).

### Storage (`storage/mod.rs`)

Flat JSON files under `{data_dir}/storage/{project_id}/` (see Data Locations for platform paths). Key paths map to filesystem paths: `["sessions", "abc123"]` → `sessions/abc123.json`. Uses `fs2` file locking (shared for reads, exclusive for writes) and atomic writes via tmp+rename.

Project ID is derived from the git root commit hash (deterministic across clones). Falls back to a hash of CWD for non-git directories.

Messages are stored one-per-file under `messages/{session_id}/{message_id}.json` to avoid read-modify-write races during streaming.

### UI (`ui/`)

Built with ratatui 0.29 + crossterm 0.28 + tui-textarea 0.7 (version-pinned for compatibility). Sidebar appears at terminal width >= 120. The TUI owns stdout, so all logging goes to a file via `tracing-appender`.

Messages render as `MessageBlock` variants: `User`, `Assistant` (with thinking/parts), `System`, `Error`, `Permission`. Styled per-variant in `message_area.rs`. Permission prompts render as bold yellow blocks with highlighted key letters.

**Interleaved assistant parts**: `MessageBlock::Assistant` stores `parts: Vec<AssistantPart>` where `AssistantPart` is either `Text(String)` or `ToolGroup(ToolGroup)`. Parts are rendered in order, preserving the chronological interleaving from the LLM stream (text → tool calls → more text → more tool calls). The `thinking` field stays separate (always rendered first). `append_text()` appends to the last `Text` part or creates a new one. `ensure_preparing_tool_group()` checks the last part. `complete_tool_call()` uses **forward** search (not reverse) to match results to calls — stream.rs emits events in original order.

**Inline diff rendering**: Write tools (edit/write/patch) auto-expand (`expanded: true`) and show colored inline diffs extracted from tool call arguments at `LlmToolCall` time. Diff content is UI-only — it does not change tool output or what the LLM sees. Types: `DiffContent` (enum: `EditDiff`, `WriteSummary`, `PatchDiff`) and `DiffLine` (enum: `Removal`, `Addition`, `Context`, `HunkHeader`) live in `message_block.rs`. Extraction logic (`extract_diff_content`, `parse_unified_diff_lines`) lives in `app.rs` alongside `extract_args_summary`. Rendering (`render_diff_lines`) lives in `message_area.rs` — uses box-drawing frame (`┌─│└─`) with `theme.error` for removals, `theme.success` for additions, `theme.dim` for context. Both `extract_args_summary` and `extract_diff_content` use exhaustive `ToolName` matches (no wildcards).

**Warm Terminal palette** (`theme.rs`): Uses RGB colors for consistent appearance across terminals. `Theme` derives `Debug, PartialEq`. Tool calls use three color categories: `tool_read` (muted warm gray) for read-only tools + webfetch, `tool_write` (coral) for write tools + memory, and `accent` (amber) for bash/question/todo. The `memory` tool gets `tool_write` because it writes to disk (see critical invariant in Context Management). Each category also has a distinct marker symbol: `·` (read), `✎` (write), `$` (execute/bash), `⚡` (interactive/question/todo) — see `ToolName::tool_marker()`. `Webfetch` gets the read marker/color despite `is_read_only()` being false — it's read-like in the UI but not in the permission/caching domain. `reasoning` uses muted lavender to distinguish from `tool_read`.

The input area has a top border (`Borders::TOP`, `INPUT_HEIGHT = 5`: 1 border + 1 context + 3 textarea), then a starship-style prompt: context line (`[Mode] ~/path prompt_tokens/ctx (%)`) showing context pressure (per-call `last_prompt_tokens`, not cumulative). `render_input` takes an `InputContext` with `last_prompt_tokens` and `context_window`. The border uses `block.inner(area)` to get the inner rect — child widgets layout inside that, not the textarea's own block. No status bar — activity spinner displays inline in the message area. Sidebar visibility uses `sidebar_override: Option<bool>` (None=auto, Some=forced).

**Sidebar changeset panel** (`sidebar.rs`): Three sections top-to-bottom: Changes (files modified by write tools with `+N`/`-N` line counts), Session (model, cumulative `in:/out:/total:` tokens, cost), Todos. The sidebar shows *cumulative* token usage — complementary to the input bar's *per-call* context pressure. `FileChange` accumulates per-file additions/removals via `SidebarState::record_file_change()` (linear scan dedup by path, skips zero-change entries). `count_diff_lines()` extracts counts from `DiffContent` — counts `Addition`/`Removal` DiffLine variants for EditDiff/PatchDiff, treats `WriteSummary.line_count` as all additions. Changeset is recorded at `ToolResult` time (not `LlmToolCall`) gated on `!output.is_error`, so failed writes don't appear. `App::find_last_completed_call()` retrieves the tool call's stored `diff_content` and `args_summary` for this. `App::strip_project_root()` converts absolute paths to relative display paths — uses string prefix stripping with path-boundary guard (won't match sibling directories like `/foo/bar-baz` when root is `/foo/bar`). The `/new` handler resets all session state: messages, stored_messages, tool cache, changeset, todos (`clear_todos()`), token counters (`sync_sidebar_tokens()`), context warning, auto-compact flag. When adding new session-scoped state, add its reset to the `Command::New` handler. `switch_to_session` also clears changeset.

**Scroll direction**: Terminal emulators on macOS already apply natural scrolling. Map `ScrollDown` → `scroll_down()` and `ScrollUp` → `scroll_up()` directly — do NOT invert them at the application level.

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
- **async-trait 0.1** enables `#[async_trait]` for `ChatStreamProvider` trait in `stream.rs` (required for `async fn` in `dyn` trait objects)
- **`Storage::new(project_id: &str)`** returns `Result<Self>` — takes a project ID string (not a path). For isolated tests: `Storage::with_base(tempdir().path().to_path_buf())`. For UI tests where no writes happen: `Storage::new("test-name").expect("test storage")`
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
