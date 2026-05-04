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
- **Assertions**: Never use trivially-true assertions — verify the specific behavior under test
- **Anyhow chain assertions**: When asserting on errors wrapped via `with_context` / `Context`,
  use `format!("{err:#}")` (alternate format) — `err.to_string()` shows only the outermost
  context and silently masks the inner cause

Run `cargo test` after every change. Run `cargo clippy` and fix all warnings — only use
`#[allow(clippy::...)]` in rare cases with a justifying comment. The crate enables
`#![warn(clippy::cargo)]` in both `lib.rs` and `main.rs`.

## Configuration

Two layers merged at startup (`config/mod.rs`):

1. **Global**: `~/.config/steve/config.jsonc`
2. **Project**: `.steve.jsonc` in project root (dotfile, not committed)

Project values override global; providers deep-merge by provider ID, then model ID. Model references
use `"provider_id/model_id"` format throughout. MCP servers merge by server ID (project wins).

**Gotchas**:

- `Config::default()` gives `auto_compact=false` (Rust bool); serde gives `true` via
  `#[serde(default = "default_auto_compact")]`. `merge()` detects empty project configs to avoid
  clobbering
- `config::load()` returns `Result<(Config, Vec<String>)>` — second element is non-fatal warnings

## Architecture

`lib.rs` (public modules) + `main.rs` (binary). Integration tests in `tests/` access modules via
`use steve::*`. No workspace — single crate.

### App Module (`app/`)

The `App` struct (coordination point) lives in `app/mod.rs`. Submodules split by concern:
`event_loop.rs` (run/handle_event), `key_handling.rs`, `input.rs`, `commands.rs`,
`session.rs`, `prompt.rs`, `context.rs` (diagnostics/sidebar/tokens), `helpers.rs`,
`tool_display.rs`, `constants.rs`. Each submodule defines its own `impl App {}` block — Rust
allows multiple impl blocks across child modules. Submodules use `use super::*;` to inherit
mod.rs imports. Use `pub(super)` for cross-submodule methods, `pub` only for external API
(`extract_args_summary`, `extract_result_summary`, `should_show_sidebar`). Use
`close_all_overlays()`, `resolve_client()`, and `resolve_file_refs()` helpers to avoid
duplication. Use `r#""#` raw strings for multi-line system prompts in `constants.rs`.

### Module Structure Convention

Each major module follows a consistent pattern: `mod.rs` owns types and public API,
submodules split by concern (e.g., `server.rs`/`manager.rs` for LSP and MCP). No `types.rs`
files — types belong in `mod.rs` alongside the code that uses them. Tests live in each
submodule's `#[cfg(test)] mod tests {}` block, not in a separate `tests.rs`. Shared test
helpers go in `mod.rs` with `#[cfg(test)] pub(crate)` at module level (not
inside `mod tests`, which is private and inaccessible from other modules).

### Enums over Strings

Tool operations use typed enums (`EditOperation`, `SymbolsOperation`, `LspOperation`,
`FindSymbolOperation`, `TaskAction`) instead of string matching. Tree-sitter languages use
`TreeSitterLang` enum. Parse from JSON args with `.parse()`, match exhaustively — adding
a variant produces compiler errors at every unhandled site.

### Stream Module (`stream/`)

`StreamRequest::spawn()` launches the stream task; `StreamRequest::run()` is the main loop.
Submodules: `agent.rs` (sub-agent spawning), `tools.rs` (tool call helpers), `recovery.rs`
(length/iteration recovery), `phases.rs` (4 tool execution phases extracted from the loop).
Sub-agents use `sub_request.spawn()` (not `Box::pin(run())`) to preserve the Send bound —
`Box::pin` erases Send, preventing `tokio::spawn` for parallel execution.

Both Phase 2 (parallel) and Phase 3 (sequential) use `spawn_blocking` for
tool execution, so `block_on` is safe in any tool handler. Phase 3 also
handles `JoinError` (task panics) gracefully in the UI.

### Crate-Level Utilities (`lib.rs`)

`DateTimeExt` trait — `display_short()`, `display_date()`,
`display_full_utc()` for consistent date formatting. UTF-8-safe truncation uses the stdlib
`str::floor_char_boundary()`. `truncate_chars(s, max)` — char-aware truncation with "..."
suffix, used across tool display, export, and session modules.

### Critical Invariants

- **Tool call detection**: Check for valid data (non-empty `id` + `function_name`), NOT
  `finish_reason` — providers vary
- **Sequential-only tools**: Write tools, `lsp` rename (read ops like diagnostics/definition/references stay parallel), `question`, and MCP tools must
  never run in parallel. `agent` calls for Explore/Plan types are pre-spawned in parallel
  via `tokio::spawn`; General agents needing permission remain sequential
- **No `unreachable!()` in stream tasks** — panics crash silently. Use `tracing::error!`
- **`last_assistant_mut()` during streaming**, NOT `messages.last_mut()` — Permission/System blocks
  interleave
- **`LlmResponseStart` event**: Emitted by stream when resuming after interjections — UI must
  push a fresh `Assistant` block and `streaming_message`. Without this, response text appends
  to the previous assistant block
- **Token metrics**: `last_prompt_tokens` (per-call, context pressure) vs `total_tokens`
  (cumulative, cost display) — do not confuse. `LlmFinish` must NOT overwrite `last_prompt_tokens`
- **`/new` resets ALL session state** — when adding session-scoped state, add its reset in
  `commands.rs` Command::New. When adding overlays, update `close_all_overlays()` in `helpers.rs`
- **Scroll**: Map `ScrollDown`→`scroll_down()` directly — do NOT invert (macOS natural scrolling)

### Exhaustive `ToolName` Match Locations

All must update when adding a `ToolName` variant (also update `allowed_tools()` in
`tool/agent.rs`, `always_allowed_rules()` in `permission/mod.rs` if read-only).
Integration tests in `tests/permission_integration.rs` and `tests/tool_integration.rs`
iterate `ToolName::iter()` with `if/else if/else` chains that must account for new variants:

`extract_args_summary()` and `extract_diff_content()` in `app/tool_display.rs`, `extract_tool_summary()` in
`export.rs`, `cache_key()` and `extract_path()` in `context/cache.rs`, `compress_tool_output()` in
`context/compressor.rs`, `build_permission_summary()` and `extract_tool_path()` in `stream/tools.rs`,
`is_write_tool()`/`intent_category()`/`tool_marker()`/`visual_category()`/`gutter_char()`/`path_arg_keys()` in
`tool/mod.rs`, `build_mode_rules()` and `plan_mode_rules()` in `permission/mod.rs`.

`path_arg_keys()` in `tool/mod.rs` is the single source of truth for tool→path-arg-key mapping.
`path_arg_keys()` returns `&[]` for tools without file-path args (Bash, Question, FindSymbol,
etc.) — do not add directory/scope params here since the permission system expects file paths.

`resolve_path()` in `tool/mod.rs` is the single path resolution helper — do not add private
copies in individual tool modules. `test_tool_context()` in `tool/mod.rs`'s test block is the
shared test helper for `ToolContext` construction.

`normalize_tool_path()` in `permission/mod.rs` is the canonical lexical-path helper — collapses
`..`/`.`, returns `(normalized: String, inside_project: bool)`. Reuse it any time you need to
compare LLM-emitted paths against scenario/config paths.

When adding edit operations: update `EditOperation` enum in `tool/mod.rs`,
`extract_diff_content()` in `app/tool_display.rs`, and `build_permission_summary()` in
`stream/tools.rs`.

When adding tree-sitter languages: update `TreeSitterLang` enum and `detect_language()` in
`tool/symbols.rs`, add `symbol_node_types()` and `container_node_types()` entries (exhaustive
match — compiler enforces), and add `kind_label()` mappings.

`extract_name_node()` in `tool/symbols.rs` is the single source of truth for finding a
declaration's name node — `extract_name()` delegates to it (with import-specific display
handled before delegation). `find_symbol_node_recursive()` is the shared recursive walk
returning `(decl_node, name_node)` — used by both `find_symbol_by_name()` and
`resolve_symbol_position()`.

`DefinitionInfo.start_line`/`end_line` span the entire declaration body (opening to
closing brace). For classification, use `start_line` only — `end_line` includes the
body interior where references (e.g., recursive calls) live.

### Find Symbol Tool (`tool/find_symbol.rs`)

`find_symbol` orchestrates workspace/symbol → grep → tree-sitter → LSP in a single tool call.
Classified as `is_read_only()` (no side effects, always-allowed, parallel-eligible).
When LSP servers are running, Phase B tries `workspace/symbol` first for semantic results
(exact name match filtering), falling back to grep when unavailable or empty.
`LspManager::running_servers()` iterates all active servers for workspace-level queries.
`WorkspaceSymbolResult` in `lsp/server.rs` normalizes both `Flat` (`SymbolInformation`)
and `Nested` (`WorkspaceSymbol`) LSP response formats. `convert_workspace_results()`
bridges workspace/symbol output into the `(definitions, classified)` types used by
Phases D and E.
`is_identifier()` gates `\b` word-boundary wrapping — non-identifier symbols
(e.g., `operator+`) use plain escaped matching. LSP enrichment requires a
tree-sitter definition site; falls back to grep+tree-sitter when LSP unavailable.
`regex_syntax::escape()` (transitive dep via grep/ignore) for regex escaping —
do not add `regex` crate as a direct dependency.

### MCP Client Integration (`mcp/`)

MCP tools bypass `ToolName` entirely — own registry with `McpToolSnapshot` (lock-free `Arc`) for
lookups. Three integration points in `stream/phases.rs`: tool defs, name resolution fallback,
Phase 4 sequential execution. Server IDs must not contain `__` (the separator).
Submodules: `server.rs` (McpServer connection), `manager.rs` (McpManager orchestration),
`transport.rs` (rmcp transport setup), `oauth/` (OAuth flow).

`AllowAlways` for MCP tools is session-only (not persisted) — MCP tool names are runtime-dynamic.

### LSP Integration (`lsp/`)

The LSP tool accepts `symbol_name` as an alternative to `line`/`character` for
position-based operations — `resolve_symbol_position()` in `tool/symbols.rs`
bridges tree-sitter symbol lookup to LSP positions. Column values are byte
offsets (tree-sitter convention), not UTF-16 code units (LSP spec default).

Submodules: `server.rs` (LspServer + URI helpers), `manager.rs` (LspManager lifecycle),
`client.rs` (JSON-RPC transport). Uses `workspace_folders` (not deprecated `root_uri`) for
LSP init. URI encoding via `url::Url::from_file_path`/`to_file_path`. Binary discovery via
`which` crate (no shell-out).

`notify_did_change`/`notify_did_save` send file changes after write tools.
`cached_diagnostics` reads the `SharedDiagnostics` cache (no `block_on`).
`diagnostics()` uses `block_on` for a `documentSymbol` round-trip — only safe
from `spawn_blocking`. Narrow mutex scope in `ensure_open`/`notify_did_change`:
check state under lock, drop before I/O or notifications, re-acquire to commit.
`publishDiagnostics` is async — stale results can arrive after `didChange`.
Compare pre/post-notification snapshots to filter stale errors.
`lsp` tool validates `path.is_file()` — directories are rejected early with
a message redirecting to `grep`.

## Formatting

`rustfmt.toml` configures `imports_granularity = "Crate"` (nightly-only). Run
`cargo +nightly fmt` to apply. Imports group by crate with nested paths. The
PostToolUse hook runs `rustfmt +nightly` — must be nightly to match CI, since
stable silently ignores nightly-only config options.

A checked-in pre-commit hook in `.githooks/pre-commit` runs `cargo +nightly fmt --check`
before every commit. First-time setup: `git config core.hooksPath .githooks`.

## CI Configuration

CI runs `cargo clippy --all-targets -- -D warnings` — all warnings are errors.
Deprecated field usage (e.g., `SymbolInformation.deprecated`) must use
`#[allow(deprecated)]` in tests. Local `cargo clippy` without `-D warnings`
won't catch these.

## System Prompt Sensitivity (`app/constants.rs`)

Smaller models (e.g., qwen3-coder) are extremely sensitive to system prompt
wording. Adding emphasis (CRITICAL, ALWAYS, REQUIRED) or changing phrasing
can break tool-calling entirely — the model emits tool calls as text instead
of structured calls. When modifying prompt text: keep original wording where
possible, prefer reordering over rewriting, and test with local models.

## Key Dependency Gotchas

- **strum 0.28**: Use `IntoStaticStr` (not `AsRefStr`) for `&'static str`
- **async-openai 0.36**: Types under `async_openai::types::chat::`, not `async_openai::types::`.
  `ChatCompletionRequestAssistantMessage` requires `audio: None` and `function_call: None`. Must set
  `stream_options` with `include_usage: Some(true)` or token usage never reports.
  `reasoning_content` still NOT exposed on `ChatCompletionStreamResponseDelta` — TODO in stream/mod.rs
- **mpatch 1.3**: Always appends trailing newline — `apply_unified_diff()` post-processes
- **html2text v0.16**: `from_read()` returns `Result<String, Error>`, not `String`
- **`move` is a Rust keyword**: Module is `move_`, variant uses `#[strum(serialize = "move")]` +
  `#[serde(rename = "move")]`
- **Unicode width**: Box-drawing `─` is 3 bytes UTF-8 but 1 display char — use `.chars().count()`
  not `.len()`
- **rmcp 1.3**: Many structs are `#[non_exhaustive]` — use builder methods (e.g.,
  `CallToolRequestParams::new().with_arguments()`). `Content = Annotated<RawContent>`
- No `dirs` crate — use `std::env::var("HOME")` or `directories::ProjectDirs`

## Provider Compatibility

Steve targets any OpenAI-compatible API. Known quirks:

- `finish_reason` may not be `ToolCalls` even with tool calls present — detect by data, not reason
- `finish_reason=Length` truncates JSON args — validate with `serde_json::from_str`, drop invalid
- `stream_options` with `include_usage: Some(true)` required for token reporting

## Data Locations

- **Data dir**: macOS `~/Library/Application Support/steve/`, Linux `~/.local/share/steve/`
- **Storage**: `{data_dir}/storage/{project_id}/`
- **Logs**: `{data_dir}/logs/steve.log.YYYY-MM-DD`


<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:ca08a54f -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

## Session Completion

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd dolt push
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds
<!-- END BEADS INTEGRATION -->
