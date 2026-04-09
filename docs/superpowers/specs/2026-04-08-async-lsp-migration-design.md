# async-lsp Migration Design

## Problem

The LSP client's hand-rolled JSON-RPC transport in `lsp/client.rs` has a message
classification bug: `#[serde(untagged)]` deserialization can't distinguish
server-initiated requests (with both `id` and `method`) from responses (with `id`
only). Server requests like `workspace/configuration` get misclassified as
responses with non-matching IDs, causing them to be discarded and potentially
timing out the actual response.

Rather than patching the custom transport, replace it with `async-lsp` — a
production-grade LSP framework that handles JSON-RPC dispatch, message framing,
and server request routing correctly.

## Design

### Sync/Async Boundary

async-lsp is fully async (tower-based). Steve's tool handlers are sync
(`Fn(Value, ToolContext) -> Result<ToolOutput>`) and run via `spawn_blocking`.

**Strategy**: `Handle::block_on` bridge. LspServer methods remain sync. Internally
they use `tokio::runtime::Handle::block_on(server_socket.request(...))`. This is
safe because `spawn_blocking` threads are not async executor threads — calling
`block_on` from them does not deadlock.

The async-lsp `MainLoop` runs as a background tokio task, processing incoming
messages. The `ServerSocket` (cloneable) is used to send requests/notifications.

### Components

#### `SteveLspService` (new, in `lsp/client.rs`)

The client-side service that handles server-initiated messages. Implements
async-lsp's router or `LanguageClient` trait.

Handles:
- `textDocument/publishDiagnostics` notification: writes to shared diagnostics
  cache (`Arc<Mutex<HashMap<Uri, Vec<Diagnostic>>>>`)
- `workspace/configuration` request: responds with empty config objects
- `client/registerCapability` request: acknowledges
- `window/workDoneProgress/create` request: acknowledges
- Unknown server requests: default error response (async-lsp handles this)

#### `LspServer` (updated, in `lsp/server.rs`)

```rust
pub struct LspServer {
    _process: Child,
    _mainloop_handle: tokio::task::JoinHandle<()>,
    server_socket: ServerSocket,
    handle: tokio::runtime::Handle,
    language: Language,
    pub binary: String,
    capabilities: ServerCapabilities,
    open_files: HashSet<Uri>,
    diagnostics: Arc<Mutex<HashMap<Uri, Vec<Diagnostic>>>>,
}
```

Public methods (`diagnostics`, `definition`, `references`, `rename`) remain sync.
Internally, they bridge to async via:
```rust
self.handle.block_on(self.server_socket.request::<GotoDefinition>(params))?
```

`process_notifications()` is removed — diagnostics are automatically routed to the
shared cache by the service handler.

`ensure_open()` sends `textDocument/didOpen` via `server_socket.notify()` (sync).

`transport_shutdown()` sends `shutdown` request + `exit` notification via
ServerSocket, then aborts the MainLoop task and kills the child process.

#### `LspManager` (updated, in `lsp/manager.rs`)

```rust
pub struct LspManager {
    servers: HashMap<Language, LspServer>,
    detected_languages: Vec<Language>,
    project_root: PathBuf,
    handle: tokio::runtime::Handle,
}
```

Constructor takes `tokio::runtime::Handle`. `start_server()` creates the
MainLoop + ServerSocket pair, spawns the MainLoop as a background task, and
performs the initialize handshake via `handle.block_on()`.

#### `lsp/client.rs` (rewrite)

Current contents (JsonRpcTransport, JsonRpcMessage, JsonRpcResponse,
JsonRpcNotification, JsonRpcError) are entirely replaced with:
- `SteveLspService` struct and its handler impls
- Shared diagnostics type: `type SharedDiagnostics = Arc<Mutex<HashMap<Uri, Vec<Diagnostic>>>>`
- Factory function to create (MainLoop, ServerSocket) pair

### Changes by File

| File | Scope | Description |
|------|-------|-------------|
| `Cargo.toml` | Add dep | `async-lsp` with `stdio` feature |
| `lsp/client.rs` | Rewrite | SteveLspService, shared diagnostics, remove all JsonRpc types |
| `lsp/server.rs` | Moderate | ServerSocket instead of transport, shared diag cache, remove process_notifications |
| `lsp/manager.rs` | Small | Accept Handle, pass to start_server, spawn MainLoop |
| `lsp/mod.rs` | Minimal | Update exports if types changed |
| `app/mod.rs` | One line | Pass `Handle::current()` to `LspManager::new()` |
| `app/event_loop.rs` | One line | Handle already available in async context |
| `tool/lsp.rs` | None | Sync API preserved |

### Notification Flow (Before → After)

**Before**: `send_request()` manually collects notifications in a Vec, returns them
alongside the response. Callers pass them to `process_notifications()`.

**After**: The MainLoop dispatches notifications automatically to the service
handler. `publishDiagnostics` writes directly to the shared diagnostics cache.
No manual notification passing. Callers just read from the shared cache.

### Shutdown Sequence

1. `LspServer::transport_shutdown()` (or Drop):
   - `handle.block_on(server_socket.request::<Shutdown>(()))` — sends shutdown
   - `server_socket.notify::<Exit>(())` — sends exit
   - `_mainloop_handle.abort()` — stops the background task
   - Kill child process if still running (existing logic)

### Risks and Mitigations

1. **lsp-types version**: async-lsp re-exports lsp-types. Verify version
   compatibility with Steve's lsp-types 0.97. If mismatched, update Steve's
   version (lsp-types is semver-stable at 0.9x).

2. **Diagnostics race**: The shared `Arc<Mutex<HashMap>>` is written by the
   MainLoop task and read by LspServer methods on blocking threads. Mutex
   contention is negligible — diagnostics arrive infrequently and reads are fast.

3. **`try_lock()` in prompt.rs**: `app/prompt.rs` calls `lsp_manager.try_lock()`
   on the event loop thread. This only reads `language_status()` (no requests,
   no `block_on`), so it remains safe.

### Testing

- **Unit tests**: SteveLspService handles publishDiagnostics, workspace/configuration correctly
- **Existing tests**: `tool/lsp.rs` tests pass unchanged (they test arg parsing, not transport)
- **Build**: `cargo test`, `cargo clippy`, `cargo build`
- **Manual**: Run steve, trigger LSP operations, confirm no "unexpected response id" warnings

### Verification

```bash
cargo test
cargo clippy
RUST_LOG=steve=debug cargo run  # verify LSP operations work, no response ID warnings
```
