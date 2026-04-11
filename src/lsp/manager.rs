use std::{collections::HashMap, path::PathBuf, process::Stdio};

use anyhow::{Context, Result};
use async_lsp::lsp_types::*;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::{
    Language,
    client::{SharedDiagnostics, create_client},
    server::{LspServer, path_to_uri},
};

pub struct LspManager {
    servers: HashMap<Language, LspServer>,
    detected_languages: Vec<Language>,
    project_root: PathBuf,
    handle: tokio::runtime::Handle,
}

impl LspManager {
    pub fn new(project_root: PathBuf, handle: tokio::runtime::Handle) -> Self {
        Self {
            servers: HashMap::new(),
            detected_languages: Vec::new(),
            project_root,
            handle,
        }
    }

    pub fn start_servers(&mut self) {
        let languages = Language::detect_from_project(&self.project_root);
        self.detected_languages = languages.clone();
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

        let binary_for_log = binary.clone();
        let diagnostics: SharedDiagnostics =
            std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
        let (mainloop, server_socket) = create_client(diagnostics.clone());

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

        let init_result: InitializeResult = self
            .handle
            .block_on(server_socket.request::<request::Initialize>(init_params))
            .map_err(|e| anyhow::anyhow!("initialize request failed: {e:?}"))?;

        server_socket
            .notify::<notification::Initialized>(InitializedParams {})
            .map_err(|e| anyhow::anyhow!("initialized notification failed: {e}"))?;

        Ok(LspServer {
            process: child,
            mainloop_handle,
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

    pub fn language_status(&self) -> Vec<(String, bool)> {
        self.detected_languages
            .iter()
            .map(|&lang| {
                let binary = self
                    .servers
                    .get(&lang)
                    .map(|s| s.binary.clone())
                    .unwrap_or_else(|| lang.server_candidates()[0].binary.to_string());
                let running = self.servers.contains_key(&lang);
                (binary, running)
            })
            .collect()
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
}
