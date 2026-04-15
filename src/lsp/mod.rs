//! LSP integration — manages language server processes for code intelligence.
//!
//! Provides diagnostics, go-to-definition, find-references, and rename operations
//! by communicating with language servers over JSON-RPC stdio transport.

pub mod client;
mod manager;
mod server;

pub use manager::LspManager;
pub use server::{LspServer, WorkspaceSymbolResult, uri_to_path};

use std::{path::Path, time::Instant};

use strum::{Display, EnumIter, EnumString, IntoStaticStr};

/// Lifecycle state of a single language server as tracked by `LspManager`.
///
/// Crash detection writes `Error`, then transitions to `Restarting` if
/// retry budget remains. After `MAX_RESTART_ATTEMPTS` failures, `Error`
/// is terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspServerState {
    /// Process spawned; Initialize request in flight or queued.
    Starting,
    /// Initialize returned; at least one active `$/progress` work-done token.
    Indexing,
    /// Initialize returned; no active progress tokens.
    Ready,
    /// Crash detected; waiting for backoff timer before re-spawning.
    Restarting,
    /// Initialize failed, or the mainloop exited unexpectedly.
    Error { reason: String },
}

impl LspServerState {
    /// Human-readable label shown next to the binary name in the sidebar.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Starting => "Starting",
            Self::Indexing => "Indexing",
            Self::Ready => "Ready",
            Self::Restarting => "Restarting",
            Self::Error { .. } => "Error",
        }
    }

    /// Whether this state should render an animated spinner (vs a static glyph).
    pub fn is_animated(&self) -> bool {
        match self {
            Self::Starting | Self::Indexing | Self::Restarting => true,
            Self::Ready | Self::Error { .. } => false,
        }
    }
}

/// A single entry in the shared LSP status cache, keyed by `Language`.
///
/// Written by `LspManager::start_server` (Starting → Ready/Indexing/Error),
/// the `$/progress` notification handler in `client::create_client`, and the
/// per-server crash watcher task. Read by `LspManager::snapshot_cache` (for
/// the sidebar, via a direct Arc clone that bypasses the manager RwLock) and
/// by `LspManager::language_status` (for the LLM system prompt, read from
/// inside the manager at a non-startup moment).
#[derive(Debug, Clone)]
pub struct LspStatusEntry {
    /// The resolved binary name (e.g., `"rust-analyzer"`). May start as a
    /// best-guess candidate and get overwritten once `resolve_server` picks
    /// the actual binary.
    pub binary: String,
    /// Current lifecycle state.
    pub state: LspServerState,
    /// Number of outstanding `$/progress` work-done tokens. State is
    /// `Indexing` iff this is nonzero (and Initialize has returned).
    pub active_progress: usize,
    /// Latest `$/progress` title or Report message. Rendered as a dimmed
    /// indented line in the sidebar during active states (Starting/Indexing).
    pub progress_message: Option<String>,
    /// When the entry was last mutated. Not currently rendered; enables
    /// future "indexing for Ns" UX without a schema change.
    pub updated_at: Instant,
    /// Number of restart attempts since the last successful Ready state.
    pub restart_attempts: u8,
    /// When the next restart attempt should fire (backoff timer).
    pub next_restart_at: Option<Instant>,
}

/// Maximum number of restart attempts before `Error` becomes terminal.
pub const MAX_RESTART_ATTEMPTS: u8 = 3;

/// Backoff duration before the Nth restart attempt.
pub fn restart_backoff(attempt: u8) -> std::time::Duration {
    match attempt {
        0 => std::time::Duration::ZERO,
        1 => std::time::Duration::from_secs(1),
        _ => std::time::Duration::from_secs(5),
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    Hash,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    EnumIter,
    EnumString,
    Display,
    IntoStaticStr,
)]
#[strum(serialize_all = "lowercase")]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    Json,
    Ruby,
}

#[derive(Debug, Clone)]
pub struct ServerCandidate {
    pub binary: &'static str,
    pub args: &'static [&'static str],
}

impl Language {
    /// The LSP language identifier string (used in `textDocument/didOpen`).
    pub fn language_id(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::TypeScript => "typescript",
            Language::Json => "json",
            Language::Ruby => "ruby",
        }
    }

    /// Determine the language from a file extension.
    pub fn from_extension(ext: &str) -> Option<Language> {
        match ext {
            "rs" => Some(Language::Rust),
            "py" | "pyi" => Some(Language::Python),
            "ts" | "tsx" | "js" | "jsx" => Some(Language::TypeScript),
            "json" | "jsonc" => Some(Language::Json),
            "rb" => Some(Language::Ruby),
            _ => None,
        }
    }

    /// Detect which languages are used in a project by scanning for marker files.
    ///
    /// Phase 1 checks the project root (zero overhead for single-project repos).
    /// Phase 2 walks subdirectories for any languages not yet found (monorepo support).
    /// Files up to 3 directories deep are detected (`max_depth(4)`, root = depth 0).
    /// The walk respects `.gitignore` via `WalkBuilder`.
    pub fn detect_from_project(root: &Path) -> Vec<Language> {
        // Phase 1: fast root-level checks
        let mut found_rust = root.join("Cargo.toml").exists();
        let mut found_python = root.join("pyproject.toml").exists()
            || root.join("setup.py").exists()
            || root.join("requirements.txt").exists();
        let mut found_ts =
            root.join("package.json").exists() || root.join("tsconfig.json").exists();
        let mut found_ruby = root.join("Gemfile").exists()
            || std::fs::read_dir(root)
                .ok()
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .any(|e| e.path().extension().is_some_and(|ext| ext == "gemspec"))
                })
                .unwrap_or(false);

        // Phase 2: subdirectory walk for any languages still undetected
        if !found_rust || !found_python || !found_ts || !found_ruby {
            let walker = ignore::WalkBuilder::new(root)
                .hidden(true)
                .git_ignore(true)
                .max_depth(Some(4))
                .build();

            for entry in walker.flatten() {
                let Some(name) = entry.file_name().to_str() else {
                    continue;
                };
                match name {
                    "Cargo.toml" if !found_rust => found_rust = true,
                    "pyproject.toml" | "setup.py" | "requirements.txt" if !found_python => {
                        found_python = true;
                    }
                    "package.json" | "tsconfig.json" if !found_ts => found_ts = true,
                    "Gemfile" if !found_ruby => found_ruby = true,
                    _ if !found_ruby && name.ends_with(".gemspec") => found_ruby = true,
                    _ => {}
                }
                if found_rust && found_python && found_ts && found_ruby {
                    break;
                }
            }
        }

        let mut langs = Vec::new();
        if found_rust {
            langs.push(Language::Rust);
        }
        if found_python {
            langs.push(Language::Python);
        }
        if found_ts {
            langs.push(Language::TypeScript);
        }
        // JSON is always available (lightweight, common)
        langs.push(Language::Json);
        if found_ruby {
            langs.push(Language::Ruby);
        }
        langs
    }

    /// Ordered list of server candidates to try for this language.
    pub fn server_candidates(self) -> &'static [ServerCandidate] {
        match self {
            Language::Rust => &[ServerCandidate {
                binary: "rust-analyzer",
                args: &[],
            }],
            Language::Python => &[
                ServerCandidate {
                    binary: "basedpyright-langserver",
                    args: &["--stdio"],
                },
                ServerCandidate {
                    binary: "pyright-langserver",
                    args: &["--stdio"],
                },
                ServerCandidate {
                    binary: "ty",
                    args: &["server"],
                },
                ServerCandidate {
                    binary: "ruff",
                    args: &["server"],
                },
            ],
            Language::TypeScript => &[ServerCandidate {
                binary: "typescript-language-server",
                args: &["--stdio"],
            }],
            Language::Json => &[ServerCandidate {
                binary: "vscode-json-language-server",
                args: &["--stdio"],
            }],
            Language::Ruby => &[
                ServerCandidate {
                    binary: "ruby-lsp",
                    args: &[],
                },
                ServerCandidate {
                    binary: "solargraph",
                    args: &["stdio"],
                },
            ],
        }
    }

    /// Find the first available server binary on PATH for this language.
    pub fn resolve_server(self) -> Option<(String, Vec<String>)> {
        for candidate in self.server_candidates() {
            if which::which(candidate.binary).is_ok() {
                return Some((
                    candidate.binary.to_string(),
                    candidate.args.iter().map(|s| s.to_string()).collect(),
                ));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;
    use tempfile::tempdir;

    #[test]
    fn from_extension_all_supported() {
        let cases = [
            ("rs", Language::Rust),
            ("py", Language::Python),
            ("pyi", Language::Python),
            ("ts", Language::TypeScript),
            ("tsx", Language::TypeScript),
            ("js", Language::TypeScript),
            ("jsx", Language::TypeScript),
            ("json", Language::Json),
            ("jsonc", Language::Json),
            ("rb", Language::Ruby),
        ];
        for (ext, expected) in cases {
            assert_eq!(
                Language::from_extension(ext),
                Some(expected),
                "wrong language for .{ext}"
            );
        }
    }

    #[test]
    fn from_extension_unsupported() {
        assert!(Language::from_extension("md").is_none());
        assert!(Language::from_extension("txt").is_none());
        assert!(Language::from_extension("go").is_none());
        assert!(Language::from_extension("").is_none());
    }

    #[test]
    fn detect_from_project_rust() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        let langs = Language::detect_from_project(dir.path());
        assert!(langs.contains(&Language::Rust));
        assert!(langs.contains(&Language::Json));
    }

    #[test]
    fn detect_from_project_python() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "").unwrap();
        let langs = Language::detect_from_project(dir.path());
        assert!(langs.contains(&Language::Python));
    }

    #[test]
    fn detect_from_project_typescript() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let langs = Language::detect_from_project(dir.path());
        assert!(langs.contains(&Language::TypeScript));
    }

    #[test]
    fn detect_from_project_empty() {
        let dir = tempdir().unwrap();
        let langs = Language::detect_from_project(dir.path());
        assert_eq!(langs, vec![Language::Json]);
    }

    #[test]
    fn every_language_has_server_candidates() {
        for lang in Language::iter() {
            assert!(
                !lang.server_candidates().is_empty(),
                "{lang} should have at least one server candidate"
            );
        }
    }

    #[test]
    fn server_candidates_have_valid_binary_names() {
        for lang in Language::iter() {
            for candidate in lang.server_candidates() {
                assert!(!candidate.binary.is_empty(), "{lang} has empty binary name");
                assert!(
                    !candidate.binary.contains('/'),
                    "{lang} binary should be a bare name, not a path"
                );
            }
        }
    }

    #[test]
    fn language_round_trip() {
        for lang in Language::iter() {
            let s: &'static str = lang.into();
            let parsed: Language = s.parse().unwrap();
            assert_eq!(parsed, lang, "round-trip failed for {s}");
        }
    }

    #[test]
    fn detect_from_project_subdir_python() {
        let dir = tempdir().unwrap();
        let subdir = dir.path().join("services").join("api");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("pyproject.toml"), "").unwrap();
        let langs = Language::detect_from_project(dir.path());
        assert!(
            langs.contains(&Language::Python),
            "Python not detected in subdir"
        );
        assert!(langs.contains(&Language::Json));
    }

    #[test]
    fn detect_from_project_monorepo_multiple() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        let py_dir = dir.path().join("svc").join("api");
        std::fs::create_dir_all(&py_dir).unwrap();
        std::fs::write(py_dir.join("pyproject.toml"), "").unwrap();
        let ts_dir = dir.path().join("pkg").join("ui");
        std::fs::create_dir_all(&ts_dir).unwrap();
        std::fs::write(ts_dir.join("package.json"), "{}").unwrap();

        let langs = Language::detect_from_project(dir.path());
        assert!(langs.contains(&Language::Rust), "Rust not detected at root");
        assert!(
            langs.contains(&Language::Python),
            "Python not detected in svc/api"
        );
        assert!(
            langs.contains(&Language::TypeScript),
            "TS not detected in pkg/ui"
        );
        assert!(langs.contains(&Language::Json), "JSON always included");
    }

    #[test]
    fn detect_from_project_at_max_depth() {
        let dir = tempdir().unwrap();
        let at_boundary = dir.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&at_boundary).unwrap();
        std::fs::write(at_boundary.join("pyproject.toml"), "").unwrap();
        let langs = Language::detect_from_project(dir.path());
        assert!(
            langs.contains(&Language::Python),
            "Python should be detected at max_depth boundary"
        );
    }

    #[test]
    fn detect_from_project_beyond_max_depth() {
        let dir = tempdir().unwrap();
        let beyond = dir.path().join("a").join("b").join("c").join("d");
        std::fs::create_dir_all(&beyond).unwrap();
        std::fs::write(beyond.join("pyproject.toml"), "").unwrap();
        let langs = Language::detect_from_project(dir.path());
        assert!(
            !langs.contains(&Language::Python),
            "Python should NOT be detected beyond max_depth"
        );
    }

    #[test]
    fn language_id_all_non_empty() {
        for lang in Language::iter() {
            let id = lang.language_id();
            assert!(!id.is_empty(), "{lang} should have a non-empty language_id");
            assert_eq!(
                id,
                id.to_lowercase(),
                "{lang} language_id should be lowercase"
            );
        }
    }

    #[test]
    fn lsp_server_state_label_all_variants() {
        assert_eq!(LspServerState::Starting.label(), "Starting");
        assert_eq!(LspServerState::Indexing.label(), "Indexing");
        assert_eq!(LspServerState::Ready.label(), "Ready");
        assert_eq!(LspServerState::Restarting.label(), "Restarting");
        assert_eq!(
            LspServerState::Error {
                reason: "boom".into()
            }
            .label(),
            "Error"
        );
    }

    #[test]
    fn lsp_server_state_is_animated_matrix() {
        assert!(LspServerState::Starting.is_animated());
        assert!(LspServerState::Indexing.is_animated());
        assert!(LspServerState::Restarting.is_animated());
        assert!(!LspServerState::Ready.is_animated());
        assert!(
            !LspServerState::Error {
                reason: "nope".into()
            }
            .is_animated()
        );
    }

    #[test]
    fn lsp_status_entry_clone_preserves_fields() {
        let original = LspStatusEntry {
            binary: "rust-analyzer".into(),
            state: LspServerState::Indexing,
            active_progress: 3,
            progress_message: Some("Building crate graph".into()),
            updated_at: std::time::Instant::now(),
            restart_attempts: 2,
            next_restart_at: Some(std::time::Instant::now()),
        };
        let cloned = original.clone();
        assert_eq!(cloned.binary, original.binary);
        assert_eq!(cloned.state, original.state);
        assert_eq!(cloned.active_progress, original.active_progress);
        assert_eq!(cloned.progress_message, original.progress_message);
        assert_eq!(cloned.updated_at, original.updated_at);
        assert_eq!(cloned.restart_attempts, original.restart_attempts);
        assert_eq!(cloned.next_restart_at, original.next_restart_at);
    }

    #[test]
    fn restart_backoff_delays() {
        assert_eq!(restart_backoff(0), std::time::Duration::ZERO);
        assert_eq!(restart_backoff(1), std::time::Duration::from_secs(1));
        assert_eq!(restart_backoff(2), std::time::Duration::from_secs(5));
        assert_eq!(restart_backoff(3), std::time::Duration::from_secs(5));
    }

    #[test]
    fn max_restart_attempts_is_three() {
        assert_eq!(MAX_RESTART_ATTEMPTS, 3);
    }
}
