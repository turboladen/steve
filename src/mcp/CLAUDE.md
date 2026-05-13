# MCP Client Integration

MCP tools bypass `ToolName` entirely — own registry with `McpToolSnapshot` (lock-free `Arc`) for
lookups. Three integration points in `stream/phases.rs`: tool defs, name resolution fallback,
Phase 4 sequential execution. Server IDs must not contain `__` (the separator).
Submodules: `server.rs` (McpServer connection), `manager.rs` (McpManager orchestration),
`transport.rs` (rmcp transport setup), `oauth/` (OAuth flow).

`AllowAlways` for MCP tools is session-only (not persisted) — MCP tool names are runtime-dynamic.
