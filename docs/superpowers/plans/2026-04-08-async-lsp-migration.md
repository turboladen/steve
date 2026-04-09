# async-lsp Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace Steve's hand-rolled JSON-RPC transport with async-lsp to fix the server-initiated request misclassification bug and prevent future JSON-RPC dispatch issues.

**Architecture:** async-lsp's `MainLoop` runs as a background tokio task per language server, handling all incoming message dispatch. `ServerSocket` (cloneable) sends requests/notifications. LspServer methods remain sync, bridging via `Handle::block_on()` from `spawn_blocking` threads. Diagnostics are shared between the MainLoop service handler and LspServer via `Arc<Mutex<HashMap>>`.

**Tech Stack:** `async-lsp` 0.2.3 (with `omni-trait` feature for typed LSP traits), `tokio-util` 0.7 (with `compat` feature for IO bridging), `lsp-types` 0.95 (via async-lsp re-export, downgrade from 0.97)

**Spec:** `docs/superpowers/specs/2026-04-08-async-lsp-migration-design.md`

---

### File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `Cargo.toml` | Modify | Add async-lsp, add compat feature to tokio-util, remove direct lsp-types dep |
| `src/lsp/client.rs` | Rewrite | `SteveLspService` (Router-based), `SharedDiagnostics` type, `create_client()` factory |
| `src/lsp/server.rs` | Modify | Replace `JsonRpcTransport` with `ServerSocket`, shared diag cache, remove `process_notifications()` |
| `src/lsp/manager.rs` | Modify | Accept `Handle`, use `block_on` for init, spawn MainLoop task |
| `src/lsp/mod.rs` | Modify | Update re-exports, switch `lsp_types` import to async-lsp's re-export |
| `src/app/mod.rs` | Modify (1 line) | Pass `Handle::current()` to `LspManager::new()` |
| `src/app/event_loop.rs` | No change | Handle already captured in LspManager at construction |

**Files NOT changed:** `src/tool/lsp.rs`, `src/stream/phases.rs`, `src/app/prompt.rs` — sync API preserved.

---

### Task 1: Update Dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Update Cargo.toml**

Replace the `lsp-types` and `tokio-util` lines and add `async-lsp`:

In `Cargo.toml`, change:
```toml
tokio-util = "0.7"
```
to:
```toml
tokio-util = { version = "0.7", features = ["compat"] }
```

Remove:
```toml
lsp-types = "0.97"
```

Add (in the LSP section, where `lsp-types` was):
```toml
# LSP client framework (re-exports lsp-types 0.95)
async-lsp = { version = "0.2.3", default-features = false, features = ["omni-trait"] }
```

Note: We disable default features (which include `client-monitor`, `stdio`, `tracing`) and only enable `omni-trait` for the typed request/notification traits. We don't need `stdio` (that's for servers exposing their own stdin/stdout) or `client-monitor` (Linux-only process monitoring).

- [ ] **Step 2: Verify it compiles (expect errors)**

Run: `cargo check 2>&1 | head -5`
Expected: Errors about `lsp_types` not found — this is correct, we'll fix imports next.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: add async-lsp, remove direct lsp-types dep"
```

---

### Task 2: Update lsp-types Imports Crate-Wide

async-lsp 0.2.3 re-exports lsp-types 0.95, which uses `url::Url` instead of lsp-types 0.97's `Uri` type. Steve only uses `Uri` in `src/lsp/server.rs` (7 occurrences). All other files use string URIs.

**Files:**
- Modify: `src/lsp/mod.rs`
- Modify: `src/lsp/server.rs`
- Modify: `src/lsp/manager.rs`
- Modify: `src/tool/lsp.rs`
- Modify: `src/context/cache.rs` (if it imports lsp_types)

- [ ] **Step 1: Grep for all lsp_types and lsp-types usage**

Run: `grep -rn 'lsp_types\|lsp-types\|use lsp_types' src/`

This finds every import site that needs updating.

- [ ] **Step 2: Update `src/lsp/mod.rs`**

No changes needed to the Language enum or detection logic — these don't use lsp_types.

- [ ] **Step 3: Update `src/lsp/server.rs` imports and Uri→Url**

Change the imports at the top from:
```rust
use lsp_types::*;
```
to:
```rust
use async_lsp::lsp_types::*;
```

Replace all `Uri` usages with `Url` (7 occurrences):
- Line 22: `pub(super) open_files: HashSet<Uri>,` → `pub(super) open_files: HashSet<Url>,`
- Line 23: `pub(super) diagnostics: HashMap<Uri, Vec<Diagnostic>>,` → `pub(super) diagnostics: HashMap<Url, Vec<Diagnostic>>,`
- Line 27: `fn ensure_open(&mut self, path: &Path) -> Result<Uri> {` → `fn ensure_open(&mut self, path: &Path) -> Result<Url> {`
- Line 198: `pub(crate) fn path_to_uri(path: &Path) -> Result<Uri> {` → `pub(crate) fn path_to_uri(path: &Path) -> Result<Url> {`

In `path_to_uri()` (line 198-211), change the parsing at the end from:
```rust
    file_url
        .as_str()
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid URI for path {}: {e}", canonical.display()))
```
to just:
```rust
    Ok(file_url)
```
Because `Url::from_file_path` already returns a `Url` — no need to stringify then parse back.

In the test module, change:
- Line 370: `let mut diagnostics: HashMap<Uri, Vec<Diagnostic>>` → `let mut diagnostics: HashMap<Url, Vec<Diagnostic>>`
- Line 379: `let uri: Uri = "file:///test.rs".parse().unwrap();` → `let uri: Url = Url::parse("file:///test.rs").unwrap();`

Remove the `#[allow(clippy::mutable_key_type)]` comment on line 368-369 — `Url` doesn't have the interior mutability issue that `Uri` has.

- [ ] **Step 4: Update `src/lsp/manager.rs` imports**

Change:
```rust
use lsp_types::*;
```
to:
```rust
use async_lsp::lsp_types::*;
```

(If it uses `use lsp_types::*;` — verify by checking the actual import.)

- [ ] **Step 5: Update `src/tool/lsp.rs` imports**

Change:
```rust
use lsp_types::*; // or whatever form is used
```

The tool file imports `lsp_types` indirectly via the `crate::lsp` module. Check the actual imports — if it uses `lsp_types::DiagnosticSeverity` directly, update to `async_lsp::lsp_types::DiagnosticSeverity`.

- [ ] **Step 6: Update any other files that import lsp_types**

Run `grep -rn 'use lsp_types' src/` and update each occurrence to `use async_lsp::lsp_types`.

Also check for `lsp_types::` qualified paths: `grep -rn 'lsp_types::' src/`

- [ ] **Step 7: Verify it compiles**

Run: `cargo check`
Expected: Clean compile (lsp/client.rs may still have issues since we haven't rewritten it yet — that's fine, it compiles independently).

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor: switch lsp-types imports to async-lsp re-export (0.97→0.95)"
```

---

### Task 3: Rewrite `src/lsp/client.rs` — SteveLspService

This is the core change. Replace the hand-rolled JSON-RPC transport with async-lsp's Router-based service.

**Files:**
- Rewrite: `src/lsp/client.rs`

- [ ] **Step 1: Write the test for SteveLspService diagnostics handling**

Replace the entire contents of `src/lsp/client.rs` with:

```rust
//! LSP client service for async-lsp integration.
//!
//! Provides `SteveLspService` — a Router-based handler for server-initiated
//! messages (notifications like `publishDiagnostics`, requests like
//! `workspace/configuration`). The `MainLoop` drives the service automatically;
//! `ServerSocket` is used by `LspServer` to send requests/notifications.

use std::{
    collections::HashMap,
    ops::ControlFlow,
    sync::{Arc, Mutex},
};

use async_lsp::{
    MainLoop, ServerSocket,
    lsp_types::{Diagnostic, Url, notification, request},
    router::Router,
};

/// Shared diagnostics cache — written by the MainLoop service, read by LspServer.
pub type SharedDiagnostics = Arc<Mutex<HashMap<Url, Vec<Diagnostic>>>>;

/// State held by the Router service.
pub(crate) struct ClientState {
    pub diagnostics: SharedDiagnostics,
}

/// Create an async-lsp client MainLoop + ServerSocket pair.
///
/// The MainLoop should be spawned as a background tokio task via
/// `mainloop.run_buffered(stdout, stdin)`. The ServerSocket is used
/// to send requests and notifications to the language server.
pub(crate) fn create_client(
    diagnostics: SharedDiagnostics,
) -> (MainLoop<Router<ClientState>>, ServerSocket) {
    MainLoop::new_client(|_server_socket| {
        let mut router = Router::new(ClientState { diagnostics });

        // Handle textDocument/publishDiagnostics — buffer into shared cache
        router.notification::<notification::PublishDiagnostics>(|state, params| {
            if let Ok(mut diags) = state.diagnostics.lock() {
                diags.insert(params.uri, params.diagnostics);
            }
            ControlFlow::Continue(())
        });

        // Handle workspace/configuration — respond with empty config per item
        router.request::<request::WorkspaceConfiguration, _>(|_state, params| {
            let items: Vec<serde_json::Value> = params
                .items
                .iter()
                .map(|_| serde_json::json!({}))
                .collect();
            futures_util::future::ready(Ok(items))
        });

        // Handle client/registerCapability — acknowledge
        router.request::<request::RegisterCapability, _>(|_state, _params| {
            futures_util::future::ready(Ok(()))
        });

        // Handle window/workDoneProgress/create — acknowledge
        router.request::<request::WorkDoneProgressCreate, _>(|_state, _params| {
            futures_util::future::ready(Ok(()))
        });

        router
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_lsp::lsp_types::{
        Diagnostic, DiagnosticSeverity, PublishDiagnosticsParams, Range, Position,
    };

    #[test]
    fn shared_diagnostics_insert_and_read() {
        let diags: SharedDiagnostics = Arc::new(Mutex::new(HashMap::new()));
        let uri = Url::parse("file:///test.rs").unwrap();
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 5)),
            severity: Some(DiagnosticSeverity::ERROR),
            message: "test error".to_string(),
            ..Default::default()
        };

        diags.lock().unwrap().insert(uri.clone(), vec![diagnostic]);

        let locked = diags.lock().unwrap();
        let result = locked.get(&uri).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].message, "test error");
    }

    #[test]
    fn create_client_returns_socket_and_mainloop() {
        let diags: SharedDiagnostics = Arc::new(Mutex::new(HashMap::new()));
        let (_mainloop, _server_socket) = create_client(diags);
        // Just verify construction doesn't panic
    }

    #[test]
    fn shared_diagnostics_overwrite() {
        let diags: SharedDiagnostics = Arc::new(Mutex::new(HashMap::new()));
        let uri = Url::parse("file:///test.rs").unwrap();

        let d1 = Diagnostic {
            message: "first".to_string(),
            ..Default::default()
        };
        let d2 = Diagnostic {
            message: "second".to_string(),
            ..Default::default()
        };

        diags.lock().unwrap().insert(uri.clone(), vec![d1]);
        diags.lock().unwrap().insert(uri.clone(), vec![d2]);

        let locked = diags.lock().unwrap();
        let result = locked.get(&uri).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].message, "second");
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib lsp::client`
Expected: All 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/lsp/client.rs
git commit -m "feat: rewrite lsp/client.rs with async-lsp SteveLspService"
```

---

### Task 4: Update `src/lsp/server.rs` — ServerSocket + Shared Diagnostics

Replace `JsonRpcTransport` usage with `ServerSocket` and shared diagnostics.

**Files:**
- Modify: `src/lsp/server.rs`

- [ ] **Step 1: Update the struct and imports**

Replace the imports at the top:
```rust
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Child,
};

use anyhow::{Context, Result};
use async_lsp::lsp_types::*;
use async_lsp::ServerSocket;
use serde_json::Value;
use tokio::task::JoinHandle;

use super::Language;
use crate::lsp::client::SharedDiagnostics;
```

Replace the `LspServer` struct:
```rust
pub struct LspServer {
    pub(super) _process: Child,
    pub(super) _mainloop_handle: JoinHandle<()>,
    pub(super) server_socket: ServerSocket,
    pub(super) handle: tokio::runtime::Handle,
    pub(super) language: Language,
    pub binary: String,
    pub(super) capabilities: ServerCapabilities,
    pub(super) open_files: HashSet<Url>,
    pub(super) diagnostics: SharedDiagnostics,
}
```

- [ ] **Step 2: Update `ensure_open()` to use ServerSocket**

Replace:
```rust
fn ensure_open(&mut self, path: &Path) -> Result<Url> {
    let uri = path_to_uri(path)?;

    if !self.open_files.contains(&uri) {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        let params = DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: self.language.language_id().to_string(),
                version: 0,
                text: content,
            },
        };
        self.server_socket
            .notify::<notification::DidOpenTextDocument>(params)
            .map_err(|e| anyhow::anyhow!("failed to send didOpen: {e}"))?;
        self.open_files.insert(uri.clone());
    }

    Ok(uri)
}
```

- [ ] **Step 3: Remove `process_notifications()` entirely**

Delete the `process_notifications` method. Diagnostics are now handled automatically by the MainLoop service.

- [ ] **Step 4: Update `diagnostics()` method**

Replace:
```rust
pub fn diagnostics(&mut self, path: &Path) -> Result<Vec<Diagnostic>> {
    let uri = self.ensure_open(path)?;

    // Trigger a documentSymbol request to prompt the server to send diagnostics
    let params = DocumentSymbolParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    match self.handle.block_on(
        self.server_socket.request::<request::DocumentSymbolRequest>(params),
    ) {
        Ok(_result) => {}
        Err(e) => {
            tracing::debug!("documentSymbol request failed (ok, just for diagnostics): {e}");
        }
    }

    let locked = self.diagnostics.lock().map_err(|_| anyhow::anyhow!("diagnostics lock poisoned"))?;
    Ok(locked.get(&uri).cloned().unwrap_or_default())
}
```

- [ ] **Step 5: Update `definition()` method**

Replace the transport call:
```rust
pub fn definition(&mut self, path: &Path, line: u32, character: u32) -> Result<Vec<Location>> {
    let uri = self.ensure_open(path)?;

    if !self
        .capabilities
        .definition_provider
        .as_ref()
        .is_some_and(|p| match p {
            OneOf::Left(b) => *b,
            OneOf::Right(_) => true,
        })
    {
        return Err(anyhow::anyhow!("server does not support go-to-definition"));
    }

    let params = GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position { line, character },
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let result = self
        .handle
        .block_on(self.server_socket.request::<request::GotoDefinition>(params))
        .map_err(|e| anyhow::anyhow!("definition request failed: {e}"))?;

    parse_goto_definition_response(result)
}
```

Note: `request::<GotoDefinition>` returns `Option<GotoDefinitionResponse>`, not `Value`. Update `parse_locations` to accept this typed response. Replace `parse_locations(Value)` with:

```rust
fn parse_goto_definition_response(result: Option<GotoDefinitionResponse>) -> Result<Vec<Location>> {
    match result {
        None => Ok(Vec::new()),
        Some(GotoDefinitionResponse::Scalar(loc)) => Ok(vec![loc]),
        Some(GotoDefinitionResponse::Array(locs)) => Ok(locs),
        Some(GotoDefinitionResponse::Link(links)) => Ok(links
            .into_iter()
            .map(|link| Location {
                uri: link.target_uri,
                range: link.target_selection_range,
            })
            .collect()),
    }
}
```

- [ ] **Step 6: Update `references()` method**

```rust
pub fn references(&mut self, path: &Path, line: u32, character: u32) -> Result<Vec<Location>> {
    let uri = self.ensure_open(path)?;

    if self.capabilities.references_provider.is_none() {
        return Err(anyhow::anyhow!("server does not support find-references"));
    }

    let params = ReferenceParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position { line, character },
        },
        context: ReferenceContext {
            include_declaration: true,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let result = self
        .handle
        .block_on(self.server_socket.request::<request::References>(params))
        .map_err(|e| anyhow::anyhow!("references request failed: {e}"))?;

    Ok(result.unwrap_or_default())
}
```

Note: `request::<References>` returns `Option<Vec<Location>>` directly — no JSON parsing needed.

- [ ] **Step 7: Update `rename()` method**

```rust
pub fn rename(
    &mut self,
    path: &Path,
    line: u32,
    character: u32,
    new_name: &str,
) -> Result<WorkspaceEdit> {
    let uri = self.ensure_open(path)?;

    if self.capabilities.rename_provider.is_none() {
        return Err(anyhow::anyhow!("server does not support rename"));
    }

    let params = RenameParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position { line, character },
        },
        new_name: new_name.to_string(),
        work_done_progress_params: Default::default(),
    };

    let result = self
        .handle
        .block_on(self.server_socket.request::<request::Rename>(params))
        .map_err(|e| anyhow::anyhow!("rename request failed: {e}"))?;

    Ok(result.unwrap_or_default())
}
```

Note: `request::<Rename>` returns `Option<WorkspaceEdit>` directly.

- [ ] **Step 8: Update `transport_shutdown()`**

```rust
pub(super) fn transport_shutdown(self) -> Result<()> {
    // Send shutdown request + exit notification
    let _ = self.handle.block_on(
        self.server_socket.request::<request::Shutdown>(()),
    );
    let _ = self.server_socket.notify::<notification::Exit>(());

    // Stop the MainLoop background task
    self._mainloop_handle.abort();

    // Clean up child process
    let mut process = self._process;
    match process.try_wait() {
        Ok(Some(_status)) => {}
        _ => {
            std::thread::sleep(std::time::Duration::from_millis(500));
            match process.try_wait() {
                Ok(Some(_)) => {}
                _ => {
                    let _ = process.kill();
                    let _ = process.wait();
                }
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 9: Update the old `parse_locations` helper**

Remove the old `parse_locations(value: Value) -> Result<Vec<Location>>` function and its tests. It's replaced by `parse_goto_definition_response` and the typed responses from async-lsp.

- [ ] **Step 10: Update tests**

Update the `process_notifications_buffers_diagnostics` test. Since `process_notifications` is removed, replace it with a test that verifies the shared diagnostics pattern:

```rust
#[test]
fn shared_diagnostics_from_publish_params() {
    use crate::lsp::client::SharedDiagnostics;

    let diags: SharedDiagnostics = Arc::new(Mutex::new(HashMap::new()));

    // Simulate what the MainLoop service does
    let params = PublishDiagnosticsParams {
        uri: Url::parse("file:///test.rs").unwrap(),
        diagnostics: vec![Diagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 5)),
            severity: Some(DiagnosticSeverity::ERROR),
            message: "test error".to_string(),
            ..Default::default()
        }],
        version: None,
    };

    diags
        .lock()
        .unwrap()
        .insert(params.uri.clone(), params.diagnostics);

    let locked = diags.lock().unwrap();
    let result = locked.get(&Url::parse("file:///test.rs").unwrap()).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].message, "test error");
}
```

Update the `parse_locations_*` tests to test `parse_goto_definition_response` instead:

```rust
#[test]
fn parse_goto_definition_response_none() {
    let result = parse_goto_definition_response(None).unwrap();
    assert!(result.is_empty());
}

#[test]
fn parse_goto_definition_response_scalar() {
    let loc = Location {
        uri: Url::parse("file:///test.rs").unwrap(),
        range: Range::new(Position::new(10, 5), Position::new(10, 15)),
    };
    let result =
        parse_goto_definition_response(Some(GotoDefinitionResponse::Scalar(loc))).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].range.start.line, 10);
}

#[test]
fn parse_goto_definition_response_array() {
    let locs = vec![
        Location {
            uri: Url::parse("file:///a.rs").unwrap(),
            range: Range::new(Position::new(1, 0), Position::new(1, 5)),
        },
        Location {
            uri: Url::parse("file:///b.rs").unwrap(),
            range: Range::new(Position::new(2, 0), Position::new(2, 5)),
        },
    ];
    let result =
        parse_goto_definition_response(Some(GotoDefinitionResponse::Array(locs))).unwrap();
    assert_eq!(result.len(), 2);
}

#[test]
fn parse_goto_definition_response_links() {
    let links = vec![LocationLink {
        origin_selection_range: None,
        target_uri: Url::parse("file:///target.rs").unwrap(),
        target_range: Range::new(Position::new(5, 0), Position::new(10, 0)),
        target_selection_range: Range::new(Position::new(5, 4), Position::new(5, 15)),
    }];
    let result =
        parse_goto_definition_response(Some(GotoDefinitionResponse::Link(links))).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].range.start.line, 5);
    assert_eq!(result[0].range.start.character, 4);
}
```

- [ ] **Step 11: Run tests**

Run: `cargo test --lib lsp::server`
Expected: All tests pass.

Run: `cargo test --lib tool::lsp`
Expected: All tests pass (tool layer unchanged).

- [ ] **Step 12: Commit**

```bash
git add src/lsp/server.rs
git commit -m "feat: update LspServer to use async-lsp ServerSocket"
```

---

### Task 5: Update `src/lsp/manager.rs` — Async Init + MainLoop Spawn

**Files:**
- Modify: `src/lsp/manager.rs`

- [ ] **Step 1: Update imports and struct**

Replace imports:
```rust
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    process::Stdio,
};

use anyhow::{Context, Result};
use async_lsp::lsp_types::*;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::{
    Language,
    client::{SharedDiagnostics, create_client},
    server::{LspServer, path_to_uri},
};
```

Update struct:
```rust
pub struct LspManager {
    servers: HashMap<Language, LspServer>,
    detected_languages: Vec<Language>,
    project_root: PathBuf,
    handle: tokio::runtime::Handle,
}
```

- [ ] **Step 2: Update constructor**

```rust
impl LspManager {
    pub fn new(project_root: PathBuf, handle: tokio::runtime::Handle) -> Self {
        Self {
            servers: HashMap::new(),
            detected_languages: Vec::new(),
            project_root,
            handle,
        }
    }
```

- [ ] **Step 3: Rewrite `start_server()`**

```rust
fn start_server(&self, lang: Language) -> Result<LspServer> {
    let (binary, args) = lang
        .resolve_server()
        .ok_or_else(|| anyhow::anyhow!("no {lang} language server found on PATH"))?;

    tracing::debug!(binary = %binary, ?args, "spawning LSP server for {lang}");

    // Use tokio::process for async IO handles
    let mut child_tokio = self.handle.block_on(async {
        tokio::process::Command::new(&binary)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
    })
    .with_context(|| format!("failed to spawn {binary}"))?;

    let stdin = child_tokio.stdin.take().context("no stdin")?;
    let stdout = child_tokio.stdout.take().context("no stdout")?;

    // Create async-lsp client
    let diagnostics: SharedDiagnostics =
        std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
    let (mainloop, server_socket) = create_client(diagnostics.clone());

    // Spawn MainLoop as background task (bridges tokio IO → futures IO)
    let mainloop_handle = self.handle.spawn(async move {
        if let Err(e) = mainloop
            .run_buffered(stdout.compat(), stdin.compat_write())
            .await
        {
            tracing::debug!("LSP MainLoop exited: {e}");
        }
    });

    // Initialize handshake
    let root_uri = path_to_uri(&self.project_root)?;
    let init_params = InitializeParams {
        process_id: Some(std::process::id()),
        workspace_folders: Some(vec![WorkspaceFolder {
            uri: root_uri,
            name: self
                .project_root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("project")
                .to_string(),
        }]),
        capabilities: ClientCapabilities {
            text_document: Some(TextDocumentClientCapabilities {
                publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                    related_information: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let init_result: InitializeResult = self
        .handle
        .block_on(server_socket.request::<request::Initialize>(init_params))
        .map_err(|e| anyhow::anyhow!("initialize request failed: {e}"))?;

    server_socket
        .notify::<notification::Initialized>(InitializedParams {})
        .map_err(|e| anyhow::anyhow!("initialized notification failed: {e}"))?;

    // Get the std::process::Child for process management
    // We need it for shutdown/kill. tokio::process::Child has an id() method.
    // For the kill/wait logic in transport_shutdown, we need a handle.
    // Store the tokio child directly.
    let child_id = child_tokio.id();

    Ok(LspServer {
        _process: child_tokio,
        _mainloop_handle: mainloop_handle,
        server_socket,
        handle: self.handle.clone(),
        language: lang,
        binary: binary.clone(),
        capabilities: init_result.capabilities,
        open_files: HashSet::new(),
        diagnostics,
    })
}
```

Note: This changes `_process` from `std::process::Child` to `tokio::process::Child`. Update `LspServer._process` type in `server.rs` accordingly:

```rust
pub(super) _process: tokio::process::Child,
```

And update `transport_shutdown()` in `server.rs` to use tokio's `Child`:

```rust
pub(super) fn transport_shutdown(mut self) -> Result<()> {
    let _ = self.handle.block_on(
        self.server_socket.request::<request::Shutdown>(()),
    );
    let _ = self.server_socket.notify::<notification::Exit>(());
    self._mainloop_handle.abort();

    // tokio::process::Child requires async for wait, use block_on
    match self.handle.block_on(self._process.try_wait()) {
        Ok(Some(_status)) => {}
        _ => {
            std::thread::sleep(std::time::Duration::from_millis(500));
            match self.handle.block_on(self._process.try_wait()) {
                Ok(Some(_)) => {}
                _ => {
                    let _ = self._process.start_kill();
                    let _ = self.handle.block_on(self._process.wait());
                }
            }
        }
    }
    Ok(())
}
```

Wait — `tokio::process::Child::try_wait()` is actually sync (returns `io::Result<Option<ExitStatus>>`), not async. And `kill()` is `start_kill()` + `wait()`. Let me correct:

```rust
pub(super) fn transport_shutdown(mut self) -> Result<()> {
    let _ = self.handle.block_on(
        self.server_socket.request::<request::Shutdown>(()),
    );
    let _ = self.server_socket.notify::<notification::Exit>(());
    self._mainloop_handle.abort();

    match self._process.try_wait() {
        Ok(Some(_status)) => {}
        _ => {
            std::thread::sleep(std::time::Duration::from_millis(500));
            match self._process.try_wait() {
                Ok(Some(_)) => {}
                _ => {
                    let _ = self._process.start_kill();
                    let _ = self.handle.block_on(self._process.wait());
                }
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Update tests**

Update the `lsp_manager_new` test to pass a handle:
```rust
#[tokio::test]
async fn lsp_manager_new() {
    let dir = tempdir().unwrap();
    let mgr = LspManager::new(dir.path().to_path_buf(), tokio::runtime::Handle::current());
    assert!(!mgr.has_servers());
    assert!(mgr.running_languages().is_empty());
    assert!(mgr.language_status().is_empty());
}

#[tokio::test]
async fn language_status_detected_but_not_running() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"test\"\n",
    )
    .unwrap();
    let mut mgr = LspManager::new(dir.path().to_path_buf(), tokio::runtime::Handle::current());
    mgr.detected_languages = Language::detect_from_project(dir.path());
    let status = mgr.language_status();
    assert!(status.len() >= 2, "expected at least 2 detected languages");
    assert!(
        status.iter().all(|(_, r)| !r),
        "no servers should be running"
    );
}
```

- [ ] **Step 5: Verify tests pass**

Run: `cargo test --lib lsp::manager`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/lsp/server.rs src/lsp/manager.rs
git commit -m "feat: update LspManager to spawn async-lsp MainLoop per server"
```

---

### Task 6: Update App Layer — Pass Handle to LspManager

**Files:**
- Modify: `src/app/mod.rs:259`
- Modify: `src/app/event_loop.rs:15-25`

- [ ] **Step 1: Update `src/app/mod.rs`**

Find the LspManager construction (around line 259):
```rust
let lsp_manager = Arc::new(std::sync::Mutex::new(crate::lsp::LspManager::new(
    project.root.clone(),
)));
```

Change to:
```rust
let lsp_manager = Arc::new(std::sync::Mutex::new(crate::lsp::LspManager::new(
    project.root.clone(),
    tokio::runtime::Handle::current(),
)));
```

- [ ] **Step 2: Verify event_loop.rs doesn't need changes**

The `event_loop.rs` code at line 15-25 does:
```rust
let lsp = self.lsp_manager.clone();
let tx = self.event_tx.clone();
tokio::task::spawn_blocking(move || {
    if let Ok(mut mgr) = lsp.lock() {
        mgr.start_servers();
        ...
    }
});
```

`start_servers()` internally calls `start_server()` which uses `self.handle.block_on()` and `self.handle.spawn()`. This works from `spawn_blocking` because the handle was captured at construction time. **No changes needed to event_loop.rs.**

- [ ] **Step 3: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

Run: `cargo clippy`
Expected: No warnings.

- [ ] **Step 4: Commit**

```bash
git add src/app/mod.rs
git commit -m "feat: pass tokio Handle to LspManager for async-lsp bridge"
```

---

### Task 7: Update `src/lsp/mod.rs` Exports

**Files:**
- Modify: `src/lsp/mod.rs`

- [ ] **Step 1: Update exports**

The current `mod.rs` has `pub mod client;` which exposes the old `JsonRpcTransport` etc. Check if anything outside `lsp/` imports from `lsp::client`. 

Run: `grep -rn 'lsp::client\|lsp::client::' src/ --include='*.rs'`

If only `lsp/server.rs` and `lsp/manager.rs` use it (via `super::client`), no export changes needed — `pub mod client;` stays as is for internal visibility.

If `src/lsp/server.rs`'s test module imports `crate::lsp::client::JsonRpcNotification`, that import needs to be removed (the test was already rewritten in Task 4).

- [ ] **Step 2: Verify no stale references**

Run: `grep -rn 'JsonRpcTransport\|JsonRpcMessage\|JsonRpcResponse\|JsonRpcNotification\|JsonRpcError' src/`
Expected: No results — all old types have been removed.

- [ ] **Step 3: Run full build**

Run: `cargo build`
Expected: Clean build.

- [ ] **Step 4: Commit (if any changes)**

```bash
git add src/lsp/mod.rs
git commit -m "chore: clean up lsp module exports after async-lsp migration"
```

---

### Task 8: Final Verification

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy`
Expected: No warnings.

- [ ] **Step 3: Run formatting**

Run: `cargo +nightly fmt`

- [ ] **Step 4: Manual smoke test**

Run: `RUST_LOG=steve=debug cargo run`

In the TUI:
1. Open a Rust project
2. Verify LSP servers start (check debug log for "LSP: started rust server")
3. Trigger a diagnostics check
4. Verify no "unexpected response id" warnings in logs
5. Verify no panics or timeouts

- [ ] **Step 5: Final commit**

```bash
git add -A
git commit -m "chore: formatting after async-lsp migration"
```

- [ ] **Step 6: Close the beads issue**

```bash
bd close steve-nmvt
```
