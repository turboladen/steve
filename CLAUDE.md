# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this
repository.

## What is Steve?

Steve is a Rust TUI AI coding agent — a simplified [opencode](https://opencode.ai) clone built with
ratatui. It connects to any OpenAI-compatible LLM API, streams responses token-by-token, and
provides a tool-calling loop that lets the LLM read, search, edit, and execute code within the
user's project.

## Build & Run

```bash
cargo build            # Build (debug)
cargo build --release  # Build (release)
cargo run              # Run the TUI (requires config — see Configuration)
cargo check            # Type-check without building
cargo test             # Run all tests
RUST_LOG=steve=debug cargo run  # Override log level (default: steve=info)
```

Rust edition 2024. `build.rs` injects git short rev as `STEVE_GIT_REV` for clap `--version` output —
must be `&'static str` (use `concat!(env!(...))`, not `format!()`).

### Testing Policy

Every change that introduces new types, trait impls, or behavior must include unit tests:

- **Match arms**: Prefer explicit variant lists over `_ =>` wildcards — exhaustive matching is a
  primary safety mechanism
- **Exhaustive test loops**: Use `ToolName::iter()` (not hard-coded variant arrays). Branch on
  predicates with `if/else if/else` so every variant hits at least one assertion
- **Helper methods** (e.g., `is_write_tool()`): Exhaustive assertions covering every variant, not
  just spot checks
- **New enums**: `FromStr`/`Display` round-trip, serde round-trip, rejection of invalid input.
  Strum-derived enums just need the variant added — existing tests validate
- **Refactors**: Existing tests passing is necessary but not sufficient — new logic paths need
  dedicated tests

Run `cargo test` after every change.

**Test infrastructure**:

- **UI rendering**: `render_to_buffer(width, height, draw_fn)` in `ui/mod.rs` creates a headless
  `TestBackend`. `make_test_app()` in `app.rs` for rendering tests.
  `ProjectInfo` requires `cwd: PathBuf` field (typically `root.clone()` in tests).
  `App::new()` takes `agents_files: Vec<config::AgentsFile>` (use `Vec::new()` in tests)
- **Storage**: `Storage::with_base(path)` for temp-dir-based tests. For UI tests:
  `Storage::new("test-name").expect("test storage")`
- **Stream**: `MockChatStream` in `stream.rs` — canned SSE responses. Use
  `with_test_manager(|mgr| { ... })` for `SessionManager` tests
- **Integration tests** (`tests/`): `permission_integration`, `config_integration`,
  `tool_integration`. Use `ToolRegistry::new(root)` +
  `ToolContext { project_root, storage_dir: None, task_store: None, lsp_manager: None }`. LSP
  integration tests use `#[ignore]` for tests requiring language servers on PATH
- **Assertions**: Never use trivially-true assertions — verify the specific behavior under test

Logs: `{data_dir}/logs/steve.log.YYYY-MM-DD` (daily rolling via `tracing-appender`).

## Configuration

Two layers merged at startup (`config/mod.rs`):

1. **Global**: `~/.config/steve/config.jsonc`
2. **Project**: `.steve.jsonc` in project root (dotfile, not committed)

Project values override global; providers deep-merge by provider ID, then model ID. Model references
use `"provider_id/model_id"` format throughout.

Optional top-level fields: `small_model` (title gen + compaction), `auto_compact` (default `true`,
triggers at 80% context), `permission_profile` (`"trust"`/`"standard"`/`"cautious"`), `allow_tools`
(per-tool auto-allow list).

**Config gotchas**:

- `Config::default()` gives `auto_compact=false` (Rust bool); serde gives `true` via
  `#[serde(default = "default_auto_compact")]`. `merge()` detects empty project configs to avoid
  clobbering
- `config::load()` returns `Result<(Config, Vec<String>)>` — second element is non-fatal warnings
- `ModelCost` uses `#[serde(alias)]` for `input`/`input_per_million` dual naming

## Commands & Keys

| Command            | Action                                      |
| ------------------ | ------------------------------------------- |
| `/new`             | Start a new session                         |
| `/rename <title>`  | Rename current session                      |
| `/models`          | List available models                       |
| `/model <ref>`     | Switch model (e.g., `/model openai/gpt-4o`) |
| `/compact`         | Compact conversation (frees context window) |
| `/export-debug`    | Export session as markdown for debugging    |
| `/init`            | Create AGENTS.md at CWD (not necessarily project root) |
| `/agents-update`   | Update AGENTS.md via LLM analysis            |
| `/help`            | Show help                                   |
| `/quit` or `/exit` | Quit                                        |

| Key             | Action                                       |
| --------------- | -------------------------------------------- |
| Enter           | Send message                                 |
| Shift+Enter     | Insert newline                               |
| Tab             | Accept autocomplete / toggle Build–Plan mode |
| Up/Down         | Navigate autocomplete / scroll messages      |
| PageUp/PageDown | Scroll messages one page                     |
| Ctrl+C          | Cancel stream (first) / quit (second)        |
| Ctrl+B          | Toggle sidebar (auto → hide → show → auto)   |
| Mouse wheel     | Scroll messages                              |
| Click+drag      | Select text (auto-copies to clipboard)       |

### CLI Subcommands (`src/cli/`, `src/main.rs`)

Subcommands short-circuit before TUI setup via `Commands` enum. `steve task` manages tasks/epics
from terminal — auto-detects entity type from kind char in ID. Formatting functions return `String`
for testability.

## Architecture

`lib.rs` (public modules) + `main.rs` (binary). Integration tests in `tests/` access modules via
`use steve::*`. ~40 source files, no workspace.

### Event-Driven Architecture

Single `AppEvent` enum through one `mpsc::UnboundedSender`. Main loop in `app.rs` uses
`tokio::select!` across terminal input, LLM streaming, tool execution, and tick timer (100ms).

**System prompt** (`build_system_prompt()` in `app.rs`): Steve identity, environment context,
permission model, AGENTS.md chain (labeled sections, root-first), and `TOOL_GUIDANCE`.

### LLM Stream + Tool Call Loop (`stream.rs`)

Spawned tokio task streams via async-openai, accumulates tool call fragments, executes tools in a
loop until none remain. Safety limits: `MAX_TOOL_ITERATIONS = 75` (build),
`MAX_PLAN_ITERATIONS = 55` (plan). Counter resets on user permission grants/interjections.

**Tool stripping**: At ~73% of max iterations (`WARN_CRITICAL_PCT`), tool definitions are removed
from the API request (`tools = None`) so the LLM structurally cannot make tool calls. At the hard
limit, one final tool-free API call is made before termination. Both `tools_stripped` and
`final_chance_taken` flags reset on user interaction (permission grant or interjection).

**`finish_reason=Length` recovery**: Two distinct causes — context pressure vs output truncation.
`classify_length_cause()` checks `prompt_tokens > CONTEXT_PRESSURE_PCT (85%) of context_window`.
Context-pressured: compress all tool results + strip tools + retry. Output-truncated: strip tools
only (no compression — context is fine) + retry. Message strings centralized in
`length_recovery_no_tools()` and `length_recovery_truncated_tools()`. Both paths preserve partial
assistant text. Falls back to error after retry.

**Deferred compression**: Normal compression only triggers when estimated context > 40% of window.
Keeps last 2 iterations of tool results uncompressed (`prev_iteration_tool_count + current`).
Aggressive pruning at 60% still fires as safety valve.

**Critical gotchas**:

- Detect tool calls by checking for valid data (non-empty `id` + `function_name`), NOT
  `finish_reason` — providers vary
- Filter out tool calls with invalid JSON args (truncated by `finish_reason=Length`)
- Use `last_assistant_mut()` during streaming, NOT `messages.last_mut()` — Permission/System blocks
  interleave
- No `unreachable!()` in stream tasks — panics crash silently. Use `tracing::error!`

Stream decoupled via `ChatStreamProvider` trait (returns `Pin<Box<dyn Future>>`). Tests use `MockChatStream`.

### Permission System

Stream task sends `PermissionRequest` with `oneshot::Sender`, awaits reply. Permission handler
matches `(key.code, key.modifiers)` tuples — new `Ctrl+<key>` bindings must not conflict with
`y`/`n`/`a` handlers.

**Modes** (Tab toggles):

- **Build** (default): permissions determined by profile (see below)
- **Plan**: read tools + LSP/Memory/Task/Question auto-allowed, Bash/Webfetch require Ask, write
  tools + Agent denied. Plan mode ignores the profile — rules are always the same

**Profiles** (Build mode only):

- **Trust**: all tools auto-allowed
- **Standard** (default): read tools + LSP/Memory/Task/Question auto-allowed; write tools + Bash +
  Webfetch + Agent require Ask
- **Cautious**: only Question/Task auto-allowed; everything else (including reads) requires Ask

Rule composition via `profile_build_rules(profile, overrides, path_rules)` and
`profile_plan_rules(profile, overrides, path_rules)` (3-arg). `profile_plan_rules()` ignores the
profile param.
`allow_tools` overrides insert rules before profile defaults (first-match-wins). Plan mode strips
write overrides. Both `build_mode_rules()` and `plan_mode_rules()` explicitly list every `ToolName`
variant.

**Path rules**: `permission_rules` array in `.steve.jsonc` with `{"tool", "pattern", "action"}`.
Priority: path rules > allow_tools > profile defaults. Special sentinel `"!project"` matches paths
outside the project root (e.g., `{"tool": "*", "pattern": "!project", "action": "deny"}`).

**Path normalization**: `normalize_tool_path(raw, project_root)` in `permission/mod.rs` resolves
raw LLM paths to project-relative form before glob matching. Returns `(normalized_path, inside_project)`.
Called at the `check()` call site in `stream.rs`. `check()` takes an `inside_project: Option<bool>`
third parameter — `None` for tools without paths.

**Persistent grants**: `a` (AllowAlways) persists to `.steve.jsonc` via
`config::persist_allow_tool()` (atomic tmp+rename, background thread).

**Permission diff preview**: `MessageBlock::Permission` has `diff_content: Option<DiffContent>`.
`PermissionRequest` carries `tool_args: Value` for inline diff rendering.

### Context Management (`context/`)

- **Compressor** (`compressor.rs`): Replaces already-seen tool results with compact summaries.
  Deferred until >40% context window; aggressive pruning at 60%. `compress_read()` and
  `compress_grep()` accept optional `tool_args` for richer summaries (file paths, line ranges,
  search patterns, top match lines). `build_tool_args_map()` extracts args from assistant messages.
  Summaries must NOT invite re-reading
- **Cache** (`cache.rs`): Session-scoped `ToolResultCache` behind `Arc<Mutex>`. Path-normalized
  keys. Auto-invalidates on mtime changes. After `REPEAT_THRESHOLD` (2) hits returns short summary
  to break feedback loops

**Critical invariant**: Write tools (`edit`, `write`, `patch`, `move`, `copy`, `delete`, `mkdir`),
`memory`, `lsp`, `question`, and `agent` must never run in parallel execution phase — sequential
only. Write/memory for cache invalidation; LSP because it holds a `std::sync::Mutex` across
blocking I/O; question/agent because they are intercepted stubs requiring async handling.

### Token Pipeline

Two metrics — do not confuse:

- **`last_prompt_tokens`** (per-call): Context window pressure. Used by input bar,
  `check_context_warning()` (60%), `should_auto_compact()` (80%)
- **`total_tokens`** (cumulative): Used by sidebar cost display

`LlmFinish` handler must NOT overwrite `last_prompt_tokens`. `sync_sidebar_tokens()` reconciles at
discrete sync points (`LlmFinish`, `/new`, `switch_to_session`, `CompactFinish`).

### Parallel Tool Execution (`stream.rs`)

Two phases: (1) parallel — read-only `Allow` tools via `spawn_blocking`, (2) sequential —
permission-required + write tools. Every `tool_call_id` must have a corresponding result message.
Results emitted in original index order (not completion order) — `complete_tool_call()` depends on
this.

### Tool System (`tool/mod.rs`)

Tools: `read`, `grep`, `glob`, `list`, `symbols`, `lsp`, `edit`, `write`, `patch`, `move`, `copy`,
`delete`, `mkdir`, `bash`, `question`, `task`, `webfetch`, `memory`, `agent`. Synchronous handlers
via `Fn(Value, ToolContext) -> Result<ToolOutput>`.

**Intercepted tools**: `question` and `agent` have stub `execute()` handlers that return errors.
`stream.rs` intercepts these by `ToolName` before calling `execute()`, handling them with full async
capabilities (permission prompts, sub-agent spawning). New intercepted tools must be excluded from
the parallel execution phase alongside `Question`, `Lsp`, and `Agent`.

**Tool argument names vary**: `read`/`list`/`grep`/`glob`/`delete`/`mkdir` use `"path"`.
`edit`/`write`/`patch` use `"file_path"`. `move`/`copy` use `"from_path"`/`"to_path"`. `read` also
accepts `"paths"` (array, multi-file), `"tail"` (last N lines), `"count"` (line count only), and
`"max_lines"` (cap output, default 2000). `edit` ops: `find_replace` (default),
`multi_find_replace`, `insert_lines`, `delete_lines`, `replace_range`.

**Ropey gotcha**: `Rope::from_str("").len_lines()` returns 1. `total_lines()` helper checks
`len_chars() == 0` first and subtracts 1 for trailing `\n`.

**Bash interception**: `check_native_tool_redirect()` rejects `cat`→read, `ls`→list, `find`→glob,
`grep`→grep, `sed`→edit, `wc -l`→read(count). Compound commands (including newlines) pass through.

**Webfetch security**: `execute()` rejects non-HTTP(S) URL schemes before issuing requests.
Custom redirect policy blocks scheme-changing redirects (e.g., http→file:// SSRF).

**Bash process cleanup**: `run_command()` spawns bash in its own process group
(`process_group(0)`) and uses `libc::killpg` on timeout to kill the entire tree.

**Exhaustive `ToolName` match locations** (all must update when adding variants):
`extract_args_summary()` and `extract_diff_content()` in `app.rs`, `extract_tool_summary()` in
`export.rs`, `cache_key()` and `extract_path()` in `context/cache.rs`, `compress_tool_output()` in
`context/compressor.rs`, `build_permission_summary()` and `extract_tool_path()` in `stream.rs`,
`is_write_tool()`/`intent_category()`/`tool_marker()`/`visual_category()`/`gutter_char()`/`path_arg_keys()` in
`tool/mod.rs`, `build_mode_rules()` and `plan_mode_rules()` in `permission/mod.rs`. Inner operation
dispatches (e.g., edit `operation`) must also list all values explicitly.

`path_arg_keys()` in `tool/mod.rs` is the single source of truth for tool→path-arg-key mapping.
`invalidate_write_tool_cache()` and `extract_tool_path()` in `stream.rs` delegate to it.

When adding edit operations: update `extract_diff_content()` in `app.rs` and
`build_permission_summary()` in `stream.rs`.

### Task System (`task/`, `tool/task.rs`, `cli/mod.rs`)

IDs: `{project_name}-{kind_char}{4_hex}` (e.g., `steve-ta3f0`). Kind chars: `t` (task), `b` (bug),
`e` (epic). Legacy IDs (`task-*`/`bug-*`/`epic-*`) still recognized. Three interfaces must stay in
sync: TUI tool handler (`tool/task.rs`), CLI (`cli/mod.rs`), and `app.rs` (`Command::TaskNew`).

### MCP Client Integration (`mcp/`)

`McpManager` behind `Arc<tokio::sync::Mutex>` manages connections to MCP servers. Background init
at app startup via `tokio::spawn`. Uses the `rmcp` crate (v1.2) for transport, handshake, and RPC.

**Architecture**: MCP tools bypass `ToolName` entirely — they have their own registry and execution
path. Three surgical integration points in `stream.rs`:
1. `build_tools()` appends MCP tool defs alongside native ones
2. When `ToolName::from_str()` fails, falls back to `McpManager::has_tool()`
3. MCP calls execute sequentially after native Phase 3 (external IPC)

**Tool naming**: `mcp__{server_id}__{tool_name}` prefix prevents collisions.
`parse_prefixed_tool_name()` / `prefixed_tool_name()` in `mcp/types.rs`.

**Config**: `mcp_servers` HashMap in `Config`. Supports `${VAR}` env expansion.
```jsonc
{ "mcp_servers": { "github": { "command": "npx", "args": ["-y", "@mcp/server-github"],
  "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}" } } } }
```

**Permissions**: `check_mcp()` on `PermissionEngine` — Trust=Allow, Standard/Cautious=Ask,
Plan=Ask. Session grants via `grant_mcp_session()`. `allow_tools` supports MCP prefixed names.

**Resources**: Cached at server init. Listed in system prompt under `## MCP Context`.

**Sub-agents**: General agents inherit MCP tools; Explore/Plan agents do not.

**Tool stripping**: MCP tools stripped alongside native tools at critical iteration threshold.

### LSP Integration (`lsp/`)

`LspManager` behind `Arc<std::sync::Mutex>` manages per-language `LspServer` instances. Background
init at app startup via `spawn_blocking`. Custom JSON-RPC transport over stdio with Content-Length
framing (no heavy client framework). Five languages: Rust, Python, TypeScript, JSON, Ruby — detected
from project marker files, servers resolved from PATH.

**Tool operations**: `diagnostics`, `definition`, `references`, `rename` (read-only plan). Single
`ToolName::Lsp` variant, not cached, not read-only (spawns external process), categorized as
`Exploring`.

**Key types**: `lsp-types` 0.97 uses `Uri` (not `Url`). `path_to_uri()` and `uri_to_path()` (pub) in
`lsp/mod.rs` handle conversion. `root_uri` field is `#[deprecated]` but still widely supported —
suppress with `#[allow(deprecated)]`.

**Adding languages**: Add variant to `Language` enum in `lsp/types.rs`, implement
`from_extension()`, `detect_from_project()`, `server_candidates()`. Existing tests use
`Language::iter()` so compiler catches missing arms.

### Agent System (`tool/agent.rs`, `stream.rs`)

`AgentSpawner` struct on `StreamRequest` captures shared resources for spawning child agents.
Three types: Explore (read-only, `small_model`), Plan (read-only + LSP, primary model), General
(full tools, inherits permissions). `AgentType::allowed_tools()` defines tool sets per type.

Sub-agents reuse `run_stream()` with `ToolRegistry::filtered()` and `agent_spawner: None` (prevents
recursive spawning). Recursive async requires `Box::pin(run_stream(...)).await`. Sub-agent events
flow through a private channel — `LlmUsageUpdate` forwarded to parent for live display,
`LlmFinish` captured for usage accumulation. `run_sub_agent()` returns `(String, StreamUsage)` —
callers must add the returned usage to parent `total_usage` for correct cost/token accounting.

Permission: `Ask` in Build mode, `Deny` in Plan mode. General agents forward `PermissionRequest`
to parent for user approval of writes.

### Storage (`storage/mod.rs`)

Flat JSON files under `{data_dir}/storage/{project_id}/`. Key paths → filesystem paths. Native file
locking + atomic tmp+rename writes. Project ID from git root commit hash (fallback: CWD hash).
Messages stored one-per-file under `messages/{session_id}/`.

### UI (`ui/`)

ratatui 0.30 + crossterm 0.29 + ratatui-textarea 0.8. Sidebar at width >= 120. TUI owns stdout — all
logging to file.

**Key patterns**:

- `MessageBlock` variants: `User`, `Assistant` (with `parts: Vec<AssistantPart>`), `System`,
  `Error`, `Permission`
- `complete_tool_call()` uses **forward** search — stream emits events in original order
- Tool colors: `tool_read` (read-only + webfetch + lsp), `tool_write` (write tools + memory),
  `accent` (bash/question/todo/agent). Markers: `·`/`✎`/`$`/`⚡`/`>`
- Intent labels (`exploring`/`editing`/`executing`/`delegating`) derived at render time via
  `infer_group_intent()`. Priority: editing > executing > delegating > exploring. Consecutive
  same-category groups deduplicated
- Code blocks rendered via `render_text_with_code_blocks()` using `CodeFence::classify()` from
  `message_block.rs`
- Auto-scroll uses wrapped line widths (not `lines.len()`) — critical for `Wrap { trim: false }`
- Scroll: Map `ScrollDown`→`scroll_down()` directly — do NOT invert (macOS already applies natural
  scrolling)
- `/new` resets ALL session state: messages, tool cache, changeset, todos, tokens, context warning,
  auto-compact flag, `pending_agents_update`, `stream_start_time`, `frozen_elapsed`. When adding
  session-scoped state, add its reset here

**Sidebar**: Changes (file diffs), Session (model/tokens/cost), Todos. Changeset recorded at
`ToolResult` time, gated on `!output.is_error`. `strip_project_root()` takes `&str` not `&Path`.

**Input area**: `INPUT_HEIGHT = 5` (1 border + 1 context + 3 textarea). Context line shows
`[Mode] ~/path prompt_tokens/ctx (%)`. During/after streaming, elapsed timer prepends:
`[Mode] ~/path elapsed · prompt_tokens/ctx (%)`.

**Elapsed timers**: `stream_start_time: Option<Instant>` + `frozen_elapsed: Option<Duration>` on
`App`. Rendering computes elapsed at render time:
`frozen_elapsed.or_else(|| stream_start_time.map(|t| t.elapsed()))`. Per-activity timer:
`activity_start: Option<Instant>` on `StatusLineState`, managed by `set_activity()` — the
`activity` field is private, all changes must go through `set_activity()`.

**Ambient context pressure**: `Theme::border_color(context_pct)` shifts borders through
gray→amber→yellow→red at 40/60/80% thresholds.

## Key Dependency Gotchas

- **strum 0.28**: Use `IntoStaticStr` (not `AsRefStr`) for `&'static str`. `ToolName::iter()` for
  exhaustive coverage
- **async-openai 0.33**: Types under `async_openai::types::chat::`, not `async_openai::types::`.
  `ChatCompletionRequestAssistantMessage` requires `audio: None` and `function_call: None`. Must set
  `stream_options` with `include_usage: Some(true)` or token usage never reports
- **mpatch 1.3**: Always appends trailing newline — `apply_unified_diff()` post-processes to
  preserve original behavior
- **html2text v0.16**: `from_read()` returns `Result<String, Error>`, not `String`
- **`move` is a Rust keyword**: Module is `move_`, variant uses `#[strum(serialize = "move")]` +
  `#[serde(rename = "move")]`
- **Unicode width**: Box-drawing `─` is 3 bytes UTF-8 but 1 display char — use `.chars().count()`
  not `.len()`
- **`all_commands()` ordering** affects autocomplete prefix matching — update autocomplete tests
  when adding commands
- **`ToolContext` fields**: `project_root: PathBuf`, `storage_dir: Option<PathBuf>`,
  `task_store: Option<Arc<TaskStore>>`, `lsp_manager: Option<Arc<std::sync::Mutex<LspManager>>>`.
  Use `None` for optional fields in tests unless testing that specific feature
- No `dirs` crate — use `std::env::var("HOME")` or `directories::ProjectDirs`. Global config uses
  `$HOME/.config/steve/` directly
- **AGENTS.md chain**: Walk-up discovery from CWD to project root collects all `AGENTS.md` files.
  `load_agents_md_chain()` returns `Vec<AgentsFile>` (root-first). `App.agents_files` replaces old
  `agents_md: Option<String>`. `combined_agents_content()` helper for contexts needing a single string

## Provider Compatibility

Steve targets any OpenAI-compatible API. Known quirks:

- `finish_reason` may not be `ToolCalls` even with tool calls present — detect by data, not reason
- `finish_reason=Length` truncates JSON args — validate with `serde_json::from_str`, drop invalid
- `stream_options` with `include_usage: Some(true)` required for token reporting

## Data Locations

- **Data dir**: macOS `~/Library/Application Support/steve/`, Linux `~/.local/share/steve/`
- **Storage**: `{data_dir}/storage/{project_id}/`
- **Logs**: `{data_dir}/logs/steve.log.YYYY-MM-DD`
