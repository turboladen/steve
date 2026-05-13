# App Module

The `App` struct (coordination point) lives in `app/mod.rs`. Submodules split by concern:
`event_loop.rs` (run/handle_event), `key_handling.rs`, `input.rs`, `commands.rs`,
`session.rs`, `prompt.rs`, `context.rs` (diagnostics/sidebar/tokens), `helpers.rs`,
`tool_display.rs`, `constants.rs`. Each submodule defines its own `impl App {}` block — Rust
allows multiple impl blocks across child modules. Submodules use `use super::*;` to inherit
mod.rs imports. Use `pub(super)` for cross-submodule methods, `pub` only for external API
(`extract_args_summary`, `extract_result_summary`, `should_show_sidebar`). Use
`close_all_overlays()`, `resolve_client()`, and `resolve_file_refs()` helpers to avoid
duplication. Use `r#""#` raw strings for multi-line system prompts in `constants.rs`.
