use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, Result};
use async_lsp::lsp_types::*;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::{
    Language, LspServerState, LspStatusEntry,
    client::{SharedDiagnostics, SharedLspStatus, create_client},
    server::{LspServer, path_to_uri},
};

pub struct LspManager {
    servers: HashMap<Language, LspServer>,
    detected_languages: Vec<Language>,
    project_root: PathBuf,
    handle: tokio::runtime::Handle,
    /// Shared per-language status cache. Single source of truth for
    /// `status_snapshot` / `language_status`. Written by `start_server`,
    /// the `$/progress` notification handler in `client::create_client`,
    /// and per-server crash-watcher tasks.
    status: SharedLspStatus,
}

impl LspManager {
    pub fn new(project_root: PathBuf, handle: tokio::runtime::Handle) -> Self {
        Self {
            servers: HashMap::new(),
            detected_languages: Vec::new(),
            project_root,
            handle,
            status: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn start_servers(&mut self) {
        let languages = Language::detect_from_project(&self.project_root);
        self.detected_languages = languages.clone();

        // Seed the status cache with Starting entries for every detected
        // language so the sidebar renders them from the very first frame,
        // before any Initialize completes. We pick a best-guess binary name
        // (first candidate) which gets overwritten by `start_server` once
        // `resolve_server` picks the actual binary.
        {
            let mut map = self.status.lock().unwrap_or_else(|p| p.into_inner());
            for &lang in &languages {
                let binary = lang
                    .server_candidates()
                    .first()
                    .map(|c| c.binary.to_string())
                    .unwrap_or_else(|| format!("{lang}-lsp"));
                map.entry(lang).or_insert_with(|| LspStatusEntry {
                    binary,
                    state: LspServerState::Starting,
                    active_progress: 0,
                    progress_message: None,
                    updated_at: Instant::now(),
                });
            }
        }

        for lang in languages {
            if self.servers.contains_key(&lang) {
                continue;
            }
            match self.start_server(lang) {
                Ok(server) => {
                    tracing::info!("LSP: started {lang} server");
                    self.servers.insert(lang, server);
                }
                Err(e) => {
                    tracing::debug!("LSP: {lang} server not available: {e:#}");
                    // Flip the seeded Starting entry to Error so the sidebar
                    // surfaces the failure instead of silently dropping it.
                    let mut map = self.status.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(entry) = map.get_mut(&lang) {
                        entry.state = LspServerState::Error {
                            reason: format!("{e:#}"),
                        };
                        entry.updated_at = Instant::now();
                    }
                }
            }
        }
    }

    fn start_server(&self, lang: Language) -> Result<LspServer> {
        let (binary, args) = lang
            .resolve_server()
            .ok_or_else(|| anyhow::anyhow!("no {lang} language server found on PATH"))?;

        tracing::debug!(binary = %binary, ?args, "spawning LSP server for {lang}");

        let mut child = tokio::process::Command::new(&binary)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to spawn {binary}"))?;

        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;

        // Upsert the real binary name into the status cache now that
        // `resolve_server` has picked one. Keep state as Starting.
        {
            let mut map = self.status.lock().unwrap_or_else(|p| p.into_inner());
            let entry = map.entry(lang).or_insert_with(|| LspStatusEntry {
                binary: binary.clone(),
                state: LspServerState::Starting,
                active_progress: 0,
                progress_message: None,
                updated_at: Instant::now(),
            });
            entry.binary = binary.clone();
            entry.state = LspServerState::Starting;
            entry.updated_at = Instant::now();
        }

        let binary_for_log = binary.clone();
        let diagnostics: SharedDiagnostics = Arc::new(Mutex::new(HashMap::new()));
        let (mainloop, server_socket) =
            create_client(diagnostics.clone(), self.status.clone(), lang);

        let mainloop_handle = self.handle.spawn(async move {
            tracing::debug!("LSP MainLoop starting for {binary_for_log}");
            match mainloop
                .run_buffered(stdout.compat(), stdin.compat_write())
                .await
            {
                Ok(()) => tracing::debug!("LSP MainLoop exited cleanly"),
                Err(e) => tracing::debug!("LSP MainLoop exited with error: {e:#}"),
            }
        });

        let mainloop_abort = mainloop_handle.abort_handle();
        let shutdown_flag = Arc::new(AtomicBool::new(false));

        // Spawn a crash watcher that awaits mainloop completion. If the
        // shutdown flag is not set, the mainloop exited unexpectedly —
        // write Error to the status cache so the sidebar surfaces it.
        {
            let status = self.status.clone();
            let shutdown_flag_watch = shutdown_flag.clone();
            let binary_for_watch = binary.clone();
            self.handle.spawn(async move {
                let join_result = mainloop_handle.await;
                if shutdown_flag_watch.load(Ordering::SeqCst) {
                    return; // intentional shutdown
                }
                let reason = match join_result {
                    Ok(()) => "mainloop exited".to_string(),
                    Err(e) if e.is_cancelled() => "mainloop cancelled".to_string(),
                    Err(e) => format!("mainloop panicked: {e}"),
                };
                tracing::warn!("LSP {binary_for_watch} ({lang}) {reason}");
                let mut map = status.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(entry) = map.get_mut(&lang) {
                    entry.state = LspServerState::Error { reason };
                    entry.active_progress = 0;
                    entry.progress_message = None;
                    entry.updated_at = Instant::now();
                }
            });
        }

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
                    synchronization: Some(TextDocumentSyncClientCapabilities {
                        dynamic_registration: Some(false),
                        did_save: Some(true),
                        ..Default::default()
                    }),
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

        let init_result: InitializeResult = match self
            .handle
            .block_on(server_socket.request::<request::Initialize>(init_params))
        {
            Ok(result) => result,
            Err(e) => {
                // Record Error in the cache before returning — the caller
                // (`start_servers`) sees the Err and bails, but the cache
                // needs to reflect the specific initialize failure.
                let mut map = self.status.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(entry) = map.get_mut(&lang) {
                    entry.state = LspServerState::Error {
                        reason: format!("initialize failed: {e:?}"),
                    };
                    entry.updated_at = Instant::now();
                }
                return Err(anyhow::anyhow!("initialize request failed: {e:?}"));
            }
        };

        server_socket
            .notify::<notification::Initialized>(InitializedParams {})
            .map_err(|e| anyhow::anyhow!("initialized notification failed: {e}"))?;

        // Transition Starting → Ready (or Indexing if `$/progress` Begin
        // notifications arrived during init — rust-analyzer does this).
        {
            let mut map = self.status.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = map.get_mut(&lang) {
                entry.state = if entry.active_progress > 0 {
                    LspServerState::Indexing
                } else {
                    LspServerState::Ready
                };
                entry.updated_at = Instant::now();
            }
        }

        Ok(LspServer {
            process: child,
            mainloop_abort,
            shutdown_flag,
            server_socket,
            handle: self.handle.clone(),
            language: lang,
            binary: binary.clone(),
            capabilities: init_result.capabilities,
            open_files: std::sync::Mutex::new(std::collections::HashMap::new()),
            diagnostics,
        })
    }

    pub fn server_for_file(&self, path: &std::path::Path) -> Result<&LspServer> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| anyhow::anyhow!("file has no extension"))?;

        let lang = Language::from_extension(ext)
            .ok_or_else(|| anyhow::anyhow!("unsupported language for .{ext}"))?;

        self.servers
            .get(&lang)
            .ok_or_else(|| anyhow::anyhow!("no {lang} server running"))
    }

    /// Notify the appropriate LSP server that a file was modified and saved.
    /// Returns `Ok(())` if no server handles this file type (graceful skip).
    pub fn notify_file_changed(&self, path: &std::path::Path) -> Result<()> {
        match self.server_for_file(path) {
            Ok(server) => {
                let uri = server.notify_did_change(path)?;
                server.notify_did_save(&uri)?;
                Ok(())
            }
            Err(_) => Ok(()),
        }
    }

    pub fn server_for_file_or_start(&mut self, path: &std::path::Path) -> Result<&LspServer> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| anyhow::anyhow!("file has no extension"))?;

        let lang = Language::from_extension(ext)
            .ok_or_else(|| anyhow::anyhow!("unsupported language for .{ext}"))?;

        self.ensure_server(lang)
    }

    fn ensure_server(&mut self, lang: Language) -> Result<&LspServer> {
        if !self.servers.contains_key(&lang) {
            // On-demand starts for a language not in the initial detection
            // set should also appear in the sidebar — append to
            // `detected_languages` so `status_snapshot` includes it.
            if !self.detected_languages.contains(&lang) {
                self.detected_languages.push(lang);
            }
            let server = self.start_server(lang)?;
            self.servers.insert(lang, server);
        }
        Ok(self.servers.get(&lang).expect("just inserted"))
    }

    pub fn shutdown(&mut self) {
        for (lang, server) in self.servers.drain() {
            tracing::debug!("LSP: shutting down {lang} server");
            if let Err(e) = server.transport_shutdown() {
                tracing::debug!("LSP: {lang} shutdown error: {e}");
            }
        }
        // Drop all status entries — the sidebar should empty out on exit.
        self.status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    pub fn running_servers(&self) -> impl Iterator<Item = &LspServer> {
        self.servers.values()
    }

    pub fn has_servers(&self) -> bool {
        !self.servers.is_empty()
    }

    pub fn running_languages(&self) -> Vec<Language> {
        self.servers.keys().copied().collect()
    }

    /// Rich status snapshot for every detected language. Reads from the
    /// shared status cache — the single source of truth.
    pub fn status_snapshot(&self) -> Vec<(Language, LspStatusEntry)> {
        let map = self.status.lock().unwrap_or_else(|p| p.into_inner());
        self.detected_languages
            .iter()
            .filter_map(|&lang| map.get(&lang).map(|entry| (lang, entry.clone())))
            .collect()
    }

    /// Back-compat pair view used by `src/app/prompt.rs` when building the
    /// system prompt. `running` is true iff the server is usable (Ready or
    /// Indexing — both respond to requests).
    pub fn language_status(&self) -> Vec<(String, bool)> {
        self.status_snapshot()
            .into_iter()
            .map(|(_, entry)| {
                let running = matches!(
                    entry.state,
                    LspServerState::Ready | LspServerState::Indexing
                );
                (entry.binary, running)
            })
            .collect()
    }

    /// Test-only helper: insert a status entry directly into the shared cache.
    /// Used by tests in this module and by `app::event_loop` tick tests.
    #[cfg(test)]
    pub(crate) fn insert_status_for_test(&mut self, lang: Language, entry: LspStatusEntry) {
        if !self.detected_languages.contains(&lang) {
            self.detected_languages.push(lang);
        }
        self.status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(lang, entry);
    }
}

impl Drop for LspManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn lsp_manager_new() {
        let dir = tempdir().unwrap();
        let mgr = LspManager::new(dir.path().to_path_buf(), tokio::runtime::Handle::current());
        assert!(!mgr.has_servers());
        assert!(mgr.running_languages().is_empty());
        assert!(mgr.language_status().is_empty());
    }

    #[tokio::test]
    async fn running_servers_empty() {
        let dir = tempdir().unwrap();
        let mgr = LspManager::new(dir.path().to_path_buf(), tokio::runtime::Handle::current());
        assert_eq!(mgr.running_servers().count(), 0);
    }

    fn sample_entry(state: LspServerState, binary: &str) -> LspStatusEntry {
        LspStatusEntry {
            binary: binary.into(),
            state,
            active_progress: 0,
            progress_message: None,
            updated_at: Instant::now(),
        }
    }

    #[tokio::test]
    async fn language_status_reads_from_state_cache_starting_and_error_are_not_running() {
        let dir = tempdir().unwrap();
        let mut mgr = LspManager::new(dir.path().to_path_buf(), tokio::runtime::Handle::current());
        mgr.insert_status_for_test(
            Language::Rust,
            sample_entry(LspServerState::Starting, "rust-analyzer"),
        );
        mgr.insert_status_for_test(
            Language::Python,
            sample_entry(
                LspServerState::Error {
                    reason: "no binary".into(),
                },
                "pyright-langserver",
            ),
        );
        let status = mgr.language_status();
        assert_eq!(status.len(), 2);
        assert!(
            status.iter().all(|(_, running)| !*running),
            "Starting and Error should both report running=false"
        );
    }

    #[tokio::test]
    async fn language_status_ready_and_indexing_report_running_true() {
        let dir = tempdir().unwrap();
        let mut mgr = LspManager::new(dir.path().to_path_buf(), tokio::runtime::Handle::current());
        mgr.insert_status_for_test(
            Language::Rust,
            sample_entry(LspServerState::Ready, "rust-analyzer"),
        );
        mgr.insert_status_for_test(
            Language::Python,
            sample_entry(LspServerState::Indexing, "pyright-langserver"),
        );
        let status = mgr.language_status();
        assert_eq!(status.len(), 2);
        for (binary, running) in &status {
            assert!(running, "{binary} should report running=true");
        }
    }

    #[tokio::test]
    async fn status_snapshot_returns_entries_for_detected_languages_only() {
        let dir = tempdir().unwrap();
        let mut mgr = LspManager::new(dir.path().to_path_buf(), tokio::runtime::Handle::current());
        mgr.insert_status_for_test(
            Language::Rust,
            sample_entry(LspServerState::Ready, "rust-analyzer"),
        );
        // Directly inject into the shared map an entry for a language that
        // is NOT in `detected_languages` — snapshot must skip it.
        mgr.status.lock().unwrap().insert(
            Language::Json,
            sample_entry(LspServerState::Ready, "json-ls"),
        );
        let snap = mgr.status_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].0, Language::Rust);
    }

    #[tokio::test]
    async fn shutdown_clears_status_cache() {
        let dir = tempdir().unwrap();
        let mut mgr = LspManager::new(dir.path().to_path_buf(), tokio::runtime::Handle::current());
        mgr.insert_status_for_test(
            Language::Rust,
            sample_entry(LspServerState::Ready, "rust-analyzer"),
        );
        assert_eq!(mgr.status_snapshot().len(), 1);
        mgr.shutdown();
        assert!(mgr.status_snapshot().is_empty());
    }

    #[tokio::test]
    async fn start_servers_seeds_error_entry_when_binary_missing() {
        // In CI, rust-analyzer is not guaranteed to be on PATH. We detect
        // rust from Cargo.toml, seed a Starting entry, fail to spawn, and
        // flip to Error. Skip this test if rust-analyzer IS on PATH — the
        // assertion would fail because the server would actually start.
        if which::which("rust-analyzer").is_ok() {
            return;
        }
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let mut mgr = LspManager::new(dir.path().to_path_buf(), tokio::runtime::Handle::current());
        mgr.start_servers();
        let snap = mgr.status_snapshot();
        let rust_entry = snap
            .iter()
            .find(|(lang, _)| *lang == Language::Rust)
            .map(|(_, e)| e.clone())
            .expect("Rust should be detected from Cargo.toml");
        assert!(
            matches!(rust_entry.state, LspServerState::Error { .. }),
            "expected Error state when rust-analyzer is missing, got {:?}",
            rust_entry.state
        );
    }
}
