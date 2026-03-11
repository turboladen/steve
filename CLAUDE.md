# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is Steve?

Steve is a Rust TUI AI coding agent — a simplified [opencode](https://opencode.ai) clone built with ratatui. It connects to any OpenAI-compatible LLM API, streams responses token-by-token, and provides a tool-calling loop that lets the LLM read, search, edit, and execute code within the user's project.

## Build & Run

```bash
cargo build            # Build (debug)
cargo build --release  # Build (release)
cargo run              # Run the TUI (requires config — see Configuration)
cargo check            # Type-check without building
cargo test             # Run all tests
RUST_LOG=steve=debug cargo run  # Override log level (default: steve=info)
```

Rust edition 2024. `build.rs` injects git short rev as `STEVE_GIT_REV` for clap `--version` output — must be `&'static str` (use `concat!(env!(...))`, not `format!()`).

### Testing Policy

Every change that introduces new types, trait impls, or behavior must include unit tests:

- **Match arms**: Prefer explicit variant lists over `_ =>` wildcards — exhaustive matching is a primary safety mechanism
- **Exhaustive test loops**: Use `ToolName::iter()` (not hard-coded variant arrays). Branch on predicates with `if/else if/else` so every variant hits at least one assertion
- **Helper methods** (e.g., `is_write_tool()`): Exhaustive assertions covering every variant, not just spot checks
- **New enums**: `FromStr`/`Display` round-trip, serde round-trip, rejection of invalid input. Strum-derived enums just need the variant added — existing tests validate
- **Refactors**: Existing tests passing is necessary but not sufficient — new logic paths need dedicated tests

Run `cargo test` after every change.

**Test infrastructure**:
- **UI rendering**: `render_to_buffer(width, height, draw_fn)` in `ui/mod.rs` creates a headless `TestBackend`. `make_test_app()` in `app.rs` for rendering tests
- **Storage**: `Storage::with_base(path)` for temp-dir-based tests. For UI tests: `Storage::new("test-name").expect("test storage")`
- **Stream**: `MockChatStream` in `stream.rs` — canned SSE responses. Use `with_test_manager(|mgr| { ... })` for `SessionManager` tests
- **Integration tests** (`tests/`): `permission_integration`, `config_integration`, `tool_integration`. Use `ToolRegistry::new(root)` + `ToolContext { project_root, storage_dir: None }`
- **Assertions**: Never use trivially-true assertions — verify the specific behavior under test

Logs: `{data_dir}/logs/steve.log.YYYY-MM-DD` (daily rolling via `tracing-appender`).

## Configuration

Two layers merged at startup (`config/mod.rs`):
1. **Global**: `~/.config/steve/config.jsonc`
2. **Project**: `.steve.jsonc` in project root (dotfile, not committed)

Project values override global; providers deep-merge by provider ID, then model ID. Model references use `"provider_id/model_id"` format throughout.

Optional top-level fields: `small_model` (title gen + compaction), `auto_compact` (default `true`, triggers at 80% context), `permission_profile` (`"trust"`/`"standard"`/`"cautious"`), `allow_tools` (per-tool auto-allow list).

**Config gotchas**:
- `Config::default()` gives `auto_compact=false` (Rust bool); serde gives `true` via `#[serde(default = "default_auto_compact")]`. `merge()` detects empty project configs to avoid clobbering
- `config::load()` returns `Result<(Config, Vec<String>)>` — second element is non-fatal warnings
- `ModelCost` uses `#[serde(alias)]` for `input`/`input_per_million` dual naming

## Commands & Keys

| Command | Action |
|---------|--------|
| `/new` | Start a new session |
| `/rename <title>` | Rename current session |
| `/models` | List available models |
| `/model <ref>` | Switch model (e.g., `/model openai/gpt-4o`) |
| `/compact` | Compact conversation (frees context window) |
| `/export-debug` | Export session as markdown for debugging |
| `/init` | Create AGENTS.md in project root |
| `/help` | Show help |
| `/quit` or `/exit` | Quit |

| Key | Action |
|-----|--------|
| Enter | Send message |
| Shift+Enter | Insert newline |
| Tab | Accept autocomplete / toggle Build–Plan mode |
| Up/Down | Navigate autocomplete / scroll messages |
| PageUp/PageDown | Scroll messages one page |
| Ctrl+C | Cancel stream (first) / quit (second) |
| Ctrl+B | Toggle sidebar (auto → hide → show → auto) |
| Mouse wheel | Scroll messages |
| Click+drag | Select text (auto-copies to clipboard) |

### CLI Subcommands (`src/cli/`, `src/main.rs`)

Subcommands short-circuit before TUI setup via `Commands` enum. `steve task` manages tasks/epics from terminal — auto-detects entity type from kind char in ID. Formatting functions return `String` for testability.

## Architecture

`lib.rs` (public modules) + `main.rs` (binary). Integration tests in `tests/` access modules via `use steve::*`. ~40 source files, no workspace.

### Event-Driven Architecture

Single `AppEvent` enum through one `mpsc::UnboundedSender`. Main loop in `app.rs` uses `tokio::select!` across terminal input, LLM streaming, tool execution, and tick timer (100ms).

**System prompt** (`build_system_prompt()` in `app.rs`): Steve identity, environment context, permission model, AGENTS.md (if present), and `TOOL_GUIDANCE`.

### LLM Stream + Tool Call Loop (`stream.rs`)

Spawned tokio task streams via async-openai, accumulates tool call fragments, executes tools in a loop until none remain. Safety limits: `MAX_TOOL_ITERATIONS = 75` (build), `MAX_PLAN_ITERATIONS = 40` (plan). Counter resets on user permission grants.

**Critical gotchas**:
- Detect tool calls by checking for valid data (non-empty `id` + `function_name`), NOT `finish_reason` — providers vary
- Filter out tool calls with invalid JSON args (truncated by `finish_reason=Length`)
- Use `last_assistant_mut()` during streaming, NOT `messages.last_mut()` — Permission/System blocks interleave
- No `unreachable!()` in stream tasks — panics crash silently. Use `tracing::error!`

Stream decoupled via `ChatStreamProvider` trait (`#[async_trait]`). Tests use `MockChatStream`.

### Permission System

Stream task sends `PermissionRequest` with `oneshot::Sender`, awaits reply. Permission handler matches `(key.code, key.modifiers)` tuples — new `Ctrl+<key>` bindings must not conflict with `y`/`n`/`a` handlers.

**Modes** (Tab toggles):
- **Build** (default): read auto-allowed, write/execute require Ask
- **Plan**: read auto-allowed, writes denied entirely, bash requires Ask

**Profiles**: Trust (all allowed), Standard (default), Cautious (almost everything asks). `allow_tools` overrides insert rules before profile defaults (first-match-wins). Plan mode strips write overrides. Both `build_mode_rules()` and `plan_mode_rules()` explicitly list every `ToolName` variant.

**Path rules**: `permission_rules` array in `.steve.jsonc` with `{"tool", "pattern", "action"}`. Priority: path rules > allow_tools > profile defaults.

**Persistent grants**: `a` (AllowAlways) persists to `.steve.jsonc` via `config::persist_allow_tool()` (atomic tmp+rename, background thread).

**Permission diff preview**: `MessageBlock::Permission` has `diff_content: Option<DiffContent>`. `PermissionRequest` carries `tool_args: Value` for inline diff rendering.

### Context Management (`context/`)

- **Compressor** (`compressor.rs`): Replaces already-seen tool results with compact summaries. Aggressive pruning at 60% context. Summaries must NOT invite re-reading
- **Cache** (`cache.rs`): Session-scoped `ToolResultCache` behind `Arc<Mutex>`. Path-normalized keys. Auto-invalidates on mtime changes. After `REPEAT_THRESHOLD` (2) hits returns short summary to break feedback loops

**Critical invariant**: Write tools (`edit`, `write`, `patch`, `move`, `copy`, `delete`, `mkdir`) and `memory` must never run in parallel execution phase — sequential only for cache invalidation.

### Token Pipeline

Two metrics — do not confuse:
- **`last_prompt_tokens`** (per-call): Context window pressure. Used by input bar, `check_context_warning()` (60%), `should_auto_compact()` (80%)
- **`total_tokens`** (cumulative): Used by sidebar cost display

`LlmFinish` handler must NOT overwrite `last_prompt_tokens`. `sync_sidebar_tokens()` reconciles at discrete sync points (`LlmFinish`, `/new`, `switch_to_session`, `CompactFinish`).

### Parallel Tool Execution (`stream.rs`)

Two phases: (1) parallel — read-only `Allow` tools via `spawn_blocking`, (2) sequential — permission-required + write tools. Every `tool_call_id` must have a corresponding result message. Results emitted in original index order (not completion order) — `complete_tool_call()` depends on this.

### Tool System (`tool/mod.rs`)

Tools: `read`, `grep`, `glob`, `list`, `edit`, `write`, `patch`, `move`, `copy`, `delete`, `mkdir`, `bash`, `question`, `task`, `webfetch`, `memory`. Synchronous handlers via `Fn(Value, ToolContext) -> Result<ToolOutput>`.

**Tool argument names vary**: `read`/`list`/`grep`/`glob`/`delete`/`mkdir` use `"path"`. `edit`/`write`/`patch` use `"file_path"`. `move`/`copy` use `"from_path"`/`"to_path"`. `edit` ops: `find_replace` (default), `multi_find_replace`, `insert_lines`, `delete_lines`, `replace_range`.

**Ropey gotcha**: `Rope::from_str("").len_lines()` returns 1. `total_lines()` helper checks `len_chars() == 0` first and subtracts 1 for trailing `\n`.

**Bash interception**: `check_native_tool_redirect()` rejects `cat`→read, `ls`→list, `find`→glob, `grep`→grep, `sed`→edit. Compound commands pass through.

**Exhaustive `ToolName` match locations** (all must update when adding variants): `extract_args_summary()` and `extract_diff_content()` in `app.rs`, `extract_tool_summary()` in `export.rs`, `cache_key()` and `extract_path()` in `context/cache.rs`, `compress_tool_output()` in `context/compressor.rs`, `build_permission_summary()` and `extract_tool_path()` in `stream.rs`, `is_write_tool()`/`intent_category()`/`tool_marker()` in `tool/mod.rs`. Inner operation dispatches (e.g., edit `operation`) must also list all values explicitly.

When adding edit operations: update `extract_diff_content()` in `app.rs` and `build_permission_summary()` in `stream.rs`.

### Task System (`task/`, `tool/task.rs`, `cli/mod.rs`)

IDs: `{project_name}-{kind_char}{4_hex}` (e.g., `steve-ta3f0`). Kind chars: `t` (task), `b` (bug), `e` (epic). Legacy IDs (`task-*`/`bug-*`/`epic-*`) still recognized. Three interfaces must stay in sync: TUI tool handler (`tool/task.rs`), CLI (`cli/mod.rs`), and `app.rs` (`Command::TaskNew`).

### Storage (`storage/mod.rs`)

Flat JSON files under `{data_dir}/storage/{project_id}/`. Key paths → filesystem paths. `fs2` file locking + atomic tmp+rename writes. Project ID from git root commit hash (fallback: CWD hash). Messages stored one-per-file under `messages/{session_id}/`.

### UI (`ui/`)

ratatui 0.30 + crossterm 0.29 + ratatui-textarea 0.8. Sidebar at width >= 120. TUI owns stdout — all logging to file.

**Key patterns**:
- `MessageBlock` variants: `User`, `Assistant` (with `parts: Vec<AssistantPart>`), `System`, `Error`, `Permission`
- `complete_tool_call()` uses **forward** search — stream emits events in original order
- Tool colors: `tool_read` (read-only + webfetch), `tool_write` (write tools + memory), `accent` (bash/question/todo). Markers: `·`/`✎`/`$`/`⚡`
- Intent labels (`exploring`/`editing`/`executing`) derived at render time via `infer_group_intent()`. Consecutive same-category groups deduplicated
- Code blocks rendered via `render_text_with_code_blocks()` using `CodeFence::classify()` from `message_block.rs`
- Auto-scroll uses wrapped line widths (not `lines.len()`) — critical for `Wrap { trim: false }`
- Scroll: Map `ScrollDown`→`scroll_down()` directly — do NOT invert (macOS already applies natural scrolling)
- `/new` resets ALL session state: messages, tool cache, changeset, todos, tokens, context warning, auto-compact flag. When adding session-scoped state, add its reset here

**Sidebar**: Changes (file diffs), Session (model/tokens/cost), Todos. Changeset recorded at `ToolResult` time, gated on `!output.is_error`. `strip_project_root()` takes `&str` not `&Path`.

**Input area**: `INPUT_HEIGHT = 5` (1 border + 1 context + 3 textarea). Context line shows `[Mode] ~/path prompt_tokens/ctx (%)`.

**Ambient context pressure**: `Theme::border_color(context_pct)` shifts borders through gray→amber→yellow→red at 40/60/80% thresholds.

## Key Dependency Gotchas

- **strum 0.28**: Use `IntoStaticStr` (not `AsRefStr`) for `&'static str`. `ToolName::iter()` for exhaustive coverage
- **async-openai 0.33**: Types under `async_openai::types::chat::`, not `async_openai::types::`. `ChatCompletionRequestAssistantMessage` requires `audio: None` and `function_call: None`. Must set `stream_options` with `include_usage: Some(true)` or token usage never reports
- **mpatch 1.3**: Always appends trailing newline — `apply_unified_diff()` post-processes to preserve original behavior
- **html2text v0.16**: `from_read()` returns `Result<String, Error>`, not `String`
- **Rust 2024 file locking**: `std::fs::File` has native `lock()`/`lock_shared()`. Importing `fs2::FileExt` triggers warnings for shadowed methods
- **`move` is a Rust keyword**: Module is `move_`, variant uses `#[strum(serialize = "move")]` + `#[serde(rename = "move")]`
- **Unicode width**: Box-drawing `─` is 3 bytes UTF-8 but 1 display char — use `.chars().count()` not `.len()`
- **`all_commands()` ordering** affects autocomplete prefix matching — update autocomplete tests when adding commands
- **`ToolContext` fields**: `project_root: PathBuf`, `storage_dir: Option<PathBuf>`. Use `storage_dir: None` in tests unless testing memory tool
- No `dirs` crate — use `std::env::var("HOME")` or `directories::ProjectDirs`. Global config uses `$HOME/.config/steve/` directly
- `AGENTS.md` optional — loaded at startup if present, injected into system prompt

## Provider Compatibility

Steve targets any OpenAI-compatible API. Known quirks:
- `finish_reason` may not be `ToolCalls` even with tool calls present — detect by data, not reason
- `finish_reason=Length` truncates JSON args — validate with `serde_json::from_str`, drop invalid
- `stream_options` with `include_usage: Some(true)` required for token reporting

## Data Locations

- **Data dir**: macOS `~/Library/Application Support/steve/`, Linux `~/.local/share/steve/`
- **Storage**: `{data_dir}/storage/{project_id}/`
- **Logs**: `{data_dir}/logs/steve.log.YYYY-MM-DD`
