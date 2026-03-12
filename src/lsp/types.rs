//! Language enum and server configuration for LSP integration.

use std::path::Path;

use strum::{Display, EnumIter, EnumString, IntoStaticStr};

/// Languages supported by the LSP integration.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, EnumIter, EnumString, Display, IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    Json,
    Ruby,
}

/// A candidate language server binary with its command-line arguments.
#[derive(Debug, Clone)]
pub struct ServerCandidate {
    pub binary: &'static str,
    pub args: &'static [&'static str],
}

impl Language {
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
    pub fn detect_from_project(root: &Path) -> Vec<Language> {
        let mut langs = Vec::new();

        if root.join("Cargo.toml").exists() {
            langs.push(Language::Rust);
        }
        if root.join("pyproject.toml").exists()
            || root.join("setup.py").exists()
            || root.join("requirements.txt").exists()
        {
            langs.push(Language::Python);
        }
        if root.join("package.json").exists() || root.join("tsconfig.json").exists() {
            langs.push(Language::TypeScript);
        }
        // JSON is always available (lightweight, common)
        langs.push(Language::Json);
        if root.join("Gemfile").exists()
            || std::fs::read_dir(root)
                .ok()
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .any(|e| {
                            e.path()
                                .extension()
                                .is_some_and(|ext| ext == "gemspec")
                        })
                })
                .unwrap_or(false)
        {
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
                binary: "vscode-json-languageserver",
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
            if which(candidate.binary) {
                return Some((
                    candidate.binary.to_string(),
                    candidate.args.iter().map(|s| s.to_string()).collect(),
                ));
            }
        }
        None
    }
}

/// Check if a binary is available on PATH.
fn which(binary: &str) -> bool {
    std::process::Command::new("which")
        .arg(binary)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
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
        assert!(langs.contains(&Language::Json)); // always included
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
        // Only JSON should be present (always included)
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
                assert!(
                    !candidate.binary.is_empty(),
                    "{lang} has empty binary name"
                );
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
}
