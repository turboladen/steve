# Tool Module

> See the root `CLAUDE.md` for the cross-cutting **Exhaustive `ToolName`
> Match Locations** list — every site that must update when adding a new
> tool variant.

## Find Symbol Tool (`tool/find_symbol.rs`)

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
