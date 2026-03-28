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
`tool_display.rs`, `constants.rs`, `types.rs`, `tests.rs`. Each submodule defines its own
`impl App {}` block — Rust allows multiple impl blocks across child modules. Submodules use
`use super::*;` to inherit mod.rs imports. Use `pub(super)` for cross-submodule methods,
`pub` only for external API (`extract_args_summary`, `extract_result_summary`,
`should_show_sidebar`). Use `close_all_overlays()` and `resolve_client()` helpers to avoid
duplication. Use `r#""#` raw strings for multi-line system prompts in `constants.rs`.

### Critical Invariants

- **Tool call detection**: Check for valid data (non-empty `id` + `function_name`), NOT
  `finish_reason` — providers vary
- **Sequential-only tools**: Write tools, `memory`, `lsp`, `question`, `agent`, and MCP tools must
  never run in parallel — sequential only
- **No `unreachable!()` in stream tasks** — panics crash silently. Use `tracing::error!`
- **`last_assistant_mut()` during streaming**, NOT `messages.last_mut()` — Permission/System blocks
  interleave
- **Token metrics**: `last_prompt_tokens` (per-call, context pressure) vs `total_tokens`
  (cumulative, cost display) — do not confuse. `LlmFinish` must NOT overwrite `last_prompt_tokens`
- **`/new` resets ALL session state** — when adding session-scoped state, add its reset in
  `commands.rs` Command::New. When adding overlays, update `close_all_overlays()` in `helpers.rs`
- **Scroll**: Map `ScrollDown`→`scroll_down()` directly — do NOT invert (macOS natural scrolling)

### Exhaustive `ToolName` Match Locations

All must update when adding a `ToolName` variant:

`extract_args_summary()` and `extract_diff_content()` in `app/tool_display.rs`, `extract_tool_summary()` in
`export.rs`, `cache_key()` and `extract_path()` in `context/cache.rs`, `compress_tool_output()` in
`context/compressor.rs`, `build_permission_summary()` and `extract_tool_path()` in `stream.rs`,
`is_write_tool()`/`intent_category()`/`tool_marker()`/`visual_category()`/`gutter_char()`/`path_arg_keys()` in
`tool/mod.rs`, `build_mode_rules()` and `plan_mode_rules()` in `permission/mod.rs`.

`path_arg_keys()` in `tool/mod.rs` is the single source of truth for tool→path-arg-key mapping.

When adding edit operations: update `extract_diff_content()` in `app/tool_display.rs` and
`build_permission_summary()` in `stream.rs`.

### MCP Client Integration (`mcp/`)

MCP tools bypass `ToolName` entirely — own registry with `McpToolSnapshot` (lock-free `Arc`) for
lookups. Three integration points in `stream.rs`: tool defs, name resolution fallback, Phase 4
sequential execution. Server IDs must not contain `__` (the separator).

`AllowAlways` for MCP tools is session-only (not persisted) — MCP tool names are runtime-dynamic.

## Key Dependency Gotchas

- **strum 0.28**: Use `IntoStaticStr` (not `AsRefStr`) for `&'static str`
- **async-openai 0.33**: Types under `async_openai::types::chat::`, not `async_openai::types::`.
  `ChatCompletionRequestAssistantMessage` requires `audio: None` and `function_call: None`. Must set
  `stream_options` with `include_usage: Some(true)` or token usage never reports
- **mpatch 1.3**: Always appends trailing newline — `apply_unified_diff()` post-processes
- **html2text v0.16**: `from_read()` returns `Result<String, Error>`, not `String`
- **`move` is a Rust keyword**: Module is `move_`, variant uses `#[strum(serialize = "move")]` +
  `#[serde(rename = "move")]`
- **Unicode width**: Box-drawing `─` is 3 bytes UTF-8 but 1 display char — use `.chars().count()`
  not `.len()`
- **rmcp 1.2**: Many structs are `#[non_exhaustive]` — use builder methods (e.g.,
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
