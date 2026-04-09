use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    process::Stdio,
};

use anyhow::{Context, Result};

use super::{
    Language,
    server::{LspServer, path_to_uri},
};

pub struct LspManager {
    servers: HashMap<Language, LspServer>,
    detected_languages: Vec<Language>,
    project_root: PathBuf,
}

impl LspManager {
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            servers: HashMap::new(),
            detected_languages: Vec::new(),
            project_root,
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
                    tracing::debug!("LSP: {lang} server not available: {e}");
                }
            }
        }
    }

    fn start_server(&self, lang: Language) -> Result<LspServer> {
        let (binary, args) = lang
            .resolve_server()
            .ok_or_else(|| anyhow::anyhow!("no {lang} language server found on PATH"))?;

        tracing::debug!(binary = %binary, ?args, "spawning LSP server for {lang}");

        let mut child = std::process::Command::new(&binary)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to spawn {binary}"))?;

        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let mut transport = super::client::JsonRpcTransport::new(stdin, stdout);

        let root_uri = path_to_uri(&self.project_root)?;
        let init_params = async_lsp::lsp_types::InitializeParams {
            process_id: Some(std::process::id()),
            workspace_folders: Some(vec![async_lsp::lsp_types::WorkspaceFolder {
                uri: root_uri,
                name: self
                    .project_root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("project")
                    .to_string(),
            }]),
            capabilities: async_lsp::lsp_types::ClientCapabilities {
                text_document: Some(async_lsp::lsp_types::TextDocumentClientCapabilities {
                    publish_diagnostics: Some(async_lsp::lsp_types::PublishDiagnosticsClientCapabilities {
                        related_information: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let (result, _notifications) = transport
            .send_request("initialize", &init_params)
            .context("initialize request failed")?;

        let init_result: async_lsp::lsp_types::InitializeResult =
            serde_json::from_value(result).context("failed to parse InitializeResult")?;

        transport.send_notification("initialized", serde_json::json!({}))?;

        Ok(LspServer {
            _process: child,
            transport,
            language: lang,
            binary: binary.clone(),
            capabilities: init_result.capabilities,
            open_files: HashSet::new(),
            diagnostics: HashMap::new(),
        })
    }

    pub fn server_for_file(&mut self, path: &std::path::Path) -> Result<&mut LspServer> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| anyhow::anyhow!("file has no extension"))?;

        let lang = Language::from_extension(ext)
            .ok_or_else(|| anyhow::anyhow!("unsupported language for .{ext}"))?;

        self.ensure_server(lang)
    }

    fn ensure_server(&mut self, lang: Language) -> Result<&mut LspServer> {
        if !self.servers.contains_key(&lang) {
            let server = self.start_server(lang)?;
            self.servers.insert(lang, server);
        }
        Ok(self.servers.get_mut(&lang).expect("just inserted"))
    }

    pub fn shutdown(&mut self) {
        for (lang, server) in self.servers.drain() {
            tracing::debug!("LSP: shutting down {lang} server");
            if let Err(e) = server.transport_shutdown() {
                tracing::debug!("LSP: {lang} shutdown error: {e}");
            }
        }
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

    #[test]
    fn lsp_manager_new() {
        let dir = tempdir().unwrap();
        let mgr = LspManager::new(dir.path().to_path_buf());
        assert!(!mgr.has_servers());
        assert!(mgr.running_languages().is_empty());
        assert!(mgr.language_status().is_empty());
    }

    #[test]
    fn language_status_detected_but_not_running() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\n",
        )
        .unwrap();
        let mut mgr = LspManager::new(dir.path().to_path_buf());
        mgr.detected_languages = Language::detect_from_project(dir.path());
        let status = mgr.language_status();
        assert!(status.len() >= 2, "expected at least 2 detected languages");
        assert!(
            status.iter().all(|(_, r)| !r),
            "no servers should be running"
        );
    }
}
