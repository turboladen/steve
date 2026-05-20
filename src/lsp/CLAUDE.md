# LSP Integration

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
