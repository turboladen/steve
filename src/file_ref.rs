//! Parse and resolve `@file` and `@!file` references in user messages.
//!
//! - `@path` (hint): injects a compact metadata note so the LLM can decide whether to read it
//! - `@!path` (inject): injects the full file contents into context immediately

use std::fs;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

/// A parsed file reference from user input.
#[derive(Debug, Clone, PartialEq)]
pub struct FileRef {
    /// The raw token as it appeared (e.g. `"@src/main.rs"` or `"@!lib.rs"`).
    pub raw: String,
    /// The resolved path portion (e.g. `"src/main.rs"` or `"lib.rs"`).
    pub path: String,
    /// `true` for inject mode (`@!`), `false` for hint mode (`@`).
    pub inject: bool,
    /// Byte offset of the start of this reference in the original text.
    pub start: usize,
    /// Byte offset of the end (exclusive) of this reference in the original text.
    pub end: usize,
}

/// A file reference after resolving against the filesystem.
#[derive(Debug, Clone)]
pub struct ResolvedFileRef {
    pub file_ref: FileRef,
    /// Path relative to the project root.
    pub rel_path: String,
    pub line_count: usize,
    pub language: Option<String>,
    /// File contents (only populated for inject mode).
    pub contents: Option<String>,
}

/// Maximum lines to inject for `@!` references.
const MAX_INJECT_LINES: usize = 2000;

/// Scan `text` for `@` and `@!` file reference tokens.
///
/// Rules:
/// - Token starts with `@` (or `@!`) and extends to the next whitespace or end of string.
/// - Skipped if preceded by an alphanumeric character (avoids `user@host`).
/// - Skipped if the path portion starts with a digit.
/// - The path portion must contain at least one `.` or `/` to look like a file path.
pub fn parse_refs(text: &str) -> Vec<FileRef> {
    let mut refs = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'@' {
            // Skip if preceded by alphanumeric
            if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
                i += 1;
                continue;
            }

            let token_start = i;
            i += 1; // skip '@'

            // Check for inject mode '!'
            let inject = i < bytes.len() && bytes[i] == b'!';
            if inject {
                i += 1;
            }

            // Path starts here
            let path_start = i;

            // Skip if path starts with a digit
            if i < bytes.len() && bytes[i].is_ascii_digit() {
                continue;
            }

            // Advance to end of token (next whitespace or end of string)
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }

            if i == path_start {
                // Empty path after @ — skip
                continue;
            }

            let raw = &text[token_start..i];
            let path = &text[path_start..i];

            // Must look like a file path (contain . or /)
            if !path.contains('.') && !path.contains('/') {
                continue;
            }

            refs.push(FileRef {
                raw: raw.to_string(),
                path: path.to_string(),
                inject,
                start: token_start,
                end: i,
            });
        } else {
            i += 1;
        }
    }

    refs
}

/// Try to resolve a `FileRef` against the project root.
///
/// Resolution strategy:
/// 1. Try exact relative path from project root
/// 2. If the path has no `/`, try basename match via `ignore::WalkBuilder` (only if unique)
pub fn resolve_ref(file_ref: &FileRef, project_root: &Path) -> Option<ResolvedFileRef> {
    // Try exact relative path first
    let candidate = project_root.join(&file_ref.path);
    if candidate.is_file() {
        return build_resolved(file_ref, &candidate, project_root);
    }

    // If no '/' in path, try basename match
    if !file_ref.path.contains('/') {
        if let Some(found) = find_unique_basename(&file_ref.path, project_root) {
            return build_resolved(file_ref, &found, project_root);
        }
    }

    None
}

/// Build a `ResolvedFileRef` from a verified file path.
fn build_resolved(
    file_ref: &FileRef,
    abs_path: &Path,
    project_root: &Path,
) -> Option<ResolvedFileRef> {
    let rel_path = abs_path
        .strip_prefix(project_root)
        .ok()?
        .to_string_lossy()
        .to_string();

    // Check for binary by reading first 8KB
    let mut is_binary = false;
    if let Ok(bytes) = fs::read(abs_path) {
        let check_len = bytes.len().min(8192);
        if bytes[..check_len].contains(&0) {
            is_binary = true;
        }
    }

    if is_binary {
        return Some(ResolvedFileRef {
            file_ref: file_ref.clone(),
            rel_path,
            line_count: 0,
            language: None,
            contents: None,
        });
    }

    let content = fs::read_to_string(abs_path).ok()?;
    let line_count = content.lines().count();
    let language = detect_language(abs_path);

    let contents = if file_ref.inject {
        // Cap at MAX_INJECT_LINES
        if line_count > MAX_INJECT_LINES {
            let truncated: String = content
                .lines()
                .take(MAX_INJECT_LINES)
                .collect::<Vec<_>>()
                .join("\n");
            Some(format!(
                "{truncated}\n\n[Truncated at {MAX_INJECT_LINES} of {line_count} lines]"
            ))
        } else {
            Some(content)
        }
    } else {
        None
    };

    Some(ResolvedFileRef {
        file_ref: file_ref.clone(),
        rel_path,
        line_count,
        language,
        contents,
    })
}

/// Search for a unique file matching `basename` in the project tree.
fn find_unique_basename(basename: &str, project_root: &Path) -> Option<PathBuf> {
    let walker = WalkBuilder::new(project_root)
        .hidden(false)
        .git_ignore(true)
        .build();

    let mut matches: Vec<PathBuf> = Vec::new();
    for entry in walker.flatten() {
        if entry.file_type().is_some_and(|ft| ft.is_file()) {
            if let Some(name) = entry.path().file_name() {
                if name == basename {
                    matches.push(entry.path().to_path_buf());
                    if matches.len() > 1 {
                        return None; // ambiguous
                    }
                }
            }
        }
    }

    matches.into_iter().next()
}

/// Detect programming language from file extension.
pub fn detect_language(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    let lang = match ext {
        "rs" => "Rust",
        "py" => "Python",
        "js" => "JavaScript",
        "ts" => "TypeScript",
        "tsx" => "TypeScript (TSX)",
        "jsx" => "JavaScript (JSX)",
        "go" => "Go",
        "java" => "Java",
        "c" => "C",
        "h" => "C Header",
        "cpp" | "cc" | "cxx" => "C++",
        "hpp" | "hxx" => "C++ Header",
        "rb" => "Ruby",
        "php" => "PHP",
        "swift" => "Swift",
        "kt" | "kts" => "Kotlin",
        "scala" => "Scala",
        "cs" => "C#",
        "sh" | "bash" | "zsh" | "fish" => "Shell",
        "lua" => "Lua",
        "zig" => "Zig",
        "toml" => "TOML",
        "yaml" | "yml" => "YAML",
        "json" | "jsonc" => "JSON",
        "xml" => "XML",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "scss" | "sass" => "SCSS",
        "sql" => "SQL",
        "md" | "markdown" => "Markdown",
        "txt" => "Text",
        "dockerfile" => "Dockerfile",
        "r" => "R",
        "ex" | "exs" => "Elixir",
        "erl" => "Erlang",
        "hs" => "Haskell",
        "ml" | "mli" => "OCaml",
        "v" | "sv" => "Verilog",
        "vhd" | "vhdl" => "VHDL",
        _ => return None,
    };
    Some(lang.to_string())
}

/// Produce display and API versions of the user message with resolved file references.
///
/// - `display_text`: original text unchanged (@ tokens stay visible)
/// - `api_text`: for hint refs, appends metadata; for inject refs, prepends file contents
pub fn augment_message(text: &str, resolved: &[ResolvedFileRef]) -> (String, String) {
    let display_text = text.to_string();

    let hints: Vec<&ResolvedFileRef> = resolved.iter().filter(|r| !r.file_ref.inject).collect();
    let injects: Vec<&ResolvedFileRef> = resolved.iter().filter(|r| r.file_ref.inject).collect();

    let mut api_text = String::new();

    // Prepend injected file contents
    for r in &injects {
        if let Some(contents) = &r.contents {
            let lang = r.language.as_deref().unwrap_or("text");
            api_text.push_str(&format!(
                "<file path=\"{}\" language=\"{}\" lines=\"{}\">\n{}\n</file>\n\n",
                r.rel_path, lang, r.line_count, contents
            ));
        }
    }

    api_text.push_str(text);

    // Append hint metadata
    if !hints.is_empty() {
        api_text.push_str("\n\n---\nReferenced files:\n");
        for r in &hints {
            let lang_info = r
                .language
                .as_ref()
                .map(|l| format!(", {l}"))
                .unwrap_or_default();
            api_text.push_str(&format!(
                "- {} ({} lines{})\n",
                r.rel_path, r.line_count, lang_info
            ));
        }
    }

    (display_text, api_text)
}

/// Build a file index for autocomplete by walking the project tree.
pub fn build_file_index(project_root: &Path) -> Vec<String> {
    let walker = WalkBuilder::new(project_root)
        .hidden(false)
        .git_ignore(true)
        .build();

    let mut files = Vec::new();
    for entry in walker.flatten() {
        if entry.file_type().is_some_and(|ft| ft.is_file()) {
            if let Ok(rel) = entry.path().strip_prefix(project_root) {
                files.push(rel.to_string_lossy().to_string());
            }
        }
    }
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // ─── parse_refs ───

    #[test]
    fn parse_basic_hint_ref() {
        let refs = parse_refs("look at @src/main.rs please");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "src/main.rs");
        assert!(!refs[0].inject);
        assert_eq!(refs[0].raw, "@src/main.rs");
    }

    #[test]
    fn parse_basic_inject_ref() {
        let refs = parse_refs("explain @!lib.rs");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "lib.rs");
        assert!(refs[0].inject);
        assert_eq!(refs[0].raw, "@!lib.rs");
    }

    #[test]
    fn parse_multiple_refs() {
        let refs = parse_refs("compare @foo.rs and @!bar.rs");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].path, "foo.rs");
        assert!(!refs[0].inject);
        assert_eq!(refs[1].path, "bar.rs");
        assert!(refs[1].inject);
    }

    #[test]
    fn parse_skips_email() {
        let refs = parse_refs("email user@example.com about it");
        assert!(refs.is_empty());
    }

    #[test]
    fn parse_skips_digit_path() {
        let refs = parse_refs("see @123 for details");
        assert!(refs.is_empty());
    }

    #[test]
    fn parse_skips_no_file_chars() {
        // No dot or slash — doesn't look like a file
        let refs = parse_refs("hey @someone do this");
        assert!(refs.is_empty());
    }

    #[test]
    fn parse_ref_at_end_of_string() {
        let refs = parse_refs("read @config.toml");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "config.toml");
    }

    #[test]
    fn parse_ref_at_start_of_string() {
        let refs = parse_refs("@main.rs what does this do?");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "main.rs");
    }

    #[test]
    fn parse_byte_offsets_correct() {
        let text = "look at @src/main.rs please";
        let refs = parse_refs(text);
        assert_eq!(&text[refs[0].start..refs[0].end], "@src/main.rs");
    }

    #[test]
    fn parse_underscore_preceded_skipped() {
        // variable_name@file.rs — preceded by underscore
        let refs = parse_refs("some_thing@file.rs");
        assert!(refs.is_empty());
    }

    #[test]
    fn parse_newline_preceded_ok() {
        let refs = parse_refs("first line\n@second.rs here");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "second.rs");
    }

    // ─── detect_language ───

    #[test]
    fn detect_common_languages() {
        assert_eq!(detect_language(Path::new("foo.rs")), Some("Rust".into()));
        assert_eq!(detect_language(Path::new("bar.py")), Some("Python".into()));
        assert_eq!(
            detect_language(Path::new("baz.ts")),
            Some("TypeScript".into())
        );
        assert_eq!(detect_language(Path::new("x.go")), Some("Go".into()));
        assert_eq!(detect_language(Path::new("y.toml")), Some("TOML".into()));
    }

    #[test]
    fn detect_unknown_extension() {
        assert_eq!(detect_language(Path::new("foo.xyz")), None);
    }

    #[test]
    fn detect_no_extension() {
        assert_eq!(detect_language(Path::new("Makefile")), None);
    }

    // ─── resolve_ref ───

    fn setup_test_project() -> TempDir {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {\n    println!(\"hello\");\n}\n").unwrap();
        fs::write(root.join("src/lib.rs"), "pub mod app;\n").unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();
        // Binary file
        fs::write(root.join("binary.bin"), b"\x00\x01\x02\x03").unwrap();
        // Dot-directory (e.g. .github/workflows)
        fs::create_dir_all(root.join(".github/workflows")).unwrap();
        fs::write(
            root.join(".github/workflows/deploy.yml"),
            "name: Deploy\non:\n  push:\n    branches: [main]\n",
        )
        .unwrap();

        dir
    }

    #[test]
    fn resolve_exact_path() {
        let dir = setup_test_project();
        let r = FileRef {
            raw: "@src/main.rs".into(),
            path: "src/main.rs".into(),
            inject: false,
            start: 0,
            end: 12,
        };
        let resolved = resolve_ref(&r, dir.path()).unwrap();
        assert_eq!(resolved.rel_path, "src/main.rs");
        assert_eq!(resolved.line_count, 3);
        assert_eq!(resolved.language, Some("Rust".into()));
        assert!(resolved.contents.is_none()); // hint mode
    }

    #[test]
    fn resolve_basename_unique() {
        let dir = setup_test_project();
        let r = FileRef {
            raw: "@Cargo.toml".into(),
            path: "Cargo.toml".into(),
            inject: false,
            start: 0,
            end: 11,
        };
        let resolved = resolve_ref(&r, dir.path()).unwrap();
        assert_eq!(resolved.rel_path, "Cargo.toml");
    }

    #[test]
    fn resolve_missing_file() {
        let dir = setup_test_project();
        let r = FileRef {
            raw: "@nonexistent.rs".into(),
            path: "nonexistent.rs".into(),
            inject: false,
            start: 0,
            end: 15,
        };
        assert!(resolve_ref(&r, dir.path()).is_none());
    }

    #[test]
    fn resolve_inject_mode_includes_contents() {
        let dir = setup_test_project();
        let r = FileRef {
            raw: "@!src/main.rs".into(),
            path: "src/main.rs".into(),
            inject: true,
            start: 0,
            end: 13,
        };
        let resolved = resolve_ref(&r, dir.path()).unwrap();
        assert!(resolved.contents.is_some());
        assert!(resolved.contents.as_ref().unwrap().contains("fn main()"));
    }

    #[test]
    fn resolve_binary_file() {
        let dir = setup_test_project();
        let r = FileRef {
            raw: "@binary.bin".into(),
            path: "binary.bin".into(),
            inject: true,
            start: 0,
            end: 11,
        };
        let resolved = resolve_ref(&r, dir.path()).unwrap();
        assert_eq!(resolved.line_count, 0);
        assert!(resolved.contents.is_none()); // binary skipped
    }

    // ─── augment_message ───

    #[test]
    fn augment_hint_only() {
        let resolved = vec![ResolvedFileRef {
            file_ref: FileRef {
                raw: "@src/main.rs".into(),
                path: "src/main.rs".into(),
                inject: false,
                start: 8,
                end: 20,
            },
            rel_path: "src/main.rs".into(),
            line_count: 42,
            language: Some("Rust".into()),
            contents: None,
        }];
        let (display, api) = augment_message("look at @src/main.rs", &resolved);
        assert_eq!(display, "look at @src/main.rs");
        assert!(api.contains("Referenced files:"));
        assert!(api.contains("src/main.rs (42 lines, Rust)"));
    }

    #[test]
    fn augment_inject_only() {
        let resolved = vec![ResolvedFileRef {
            file_ref: FileRef {
                raw: "@!lib.rs".into(),
                path: "lib.rs".into(),
                inject: true,
                start: 0,
                end: 8,
            },
            rel_path: "src/lib.rs".into(),
            line_count: 1,
            language: Some("Rust".into()),
            contents: Some("pub mod app;\n".into()),
        }];
        let (display, api) = augment_message("@!lib.rs explain", &resolved);
        assert_eq!(display, "@!lib.rs explain");
        assert!(api.contains("<file path=\"src/lib.rs\""));
        assert!(api.contains("pub mod app;"));
        assert!(api.contains("@!lib.rs explain")); // original text preserved
        assert!(!api.contains("Referenced files:")); // no hint section
    }

    #[test]
    fn augment_mixed() {
        let resolved = vec![
            ResolvedFileRef {
                file_ref: FileRef {
                    raw: "@main.rs".into(),
                    path: "main.rs".into(),
                    inject: false,
                    start: 0,
                    end: 8,
                },
                rel_path: "src/main.rs".into(),
                line_count: 10,
                language: Some("Rust".into()),
                contents: None,
            },
            ResolvedFileRef {
                file_ref: FileRef {
                    raw: "@!lib.rs".into(),
                    path: "lib.rs".into(),
                    inject: true,
                    start: 13,
                    end: 21,
                },
                rel_path: "src/lib.rs".into(),
                line_count: 1,
                language: Some("Rust".into()),
                contents: Some("pub mod app;\n".into()),
            },
        ];
        let (_, api) = augment_message("@main.rs and @!lib.rs", &resolved);
        // Inject block should come before the original text
        let file_pos = api.find("<file").unwrap();
        let text_pos = api.find("@main.rs and").unwrap();
        assert!(file_pos < text_pos);
        // Hint section at the end
        assert!(api.contains("Referenced files:"));
        assert!(api.contains("src/main.rs (10 lines, Rust)"));
    }

    #[test]
    fn augment_no_refs_passthrough() {
        let (display, api) = augment_message("just a normal message", &[]);
        assert_eq!(display, "just a normal message");
        assert_eq!(api, "just a normal message");
    }

    // ─── build_file_index ───

    #[test]
    fn build_file_index_finds_files() {
        let dir = setup_test_project();
        let index = build_file_index(dir.path());
        assert!(index.contains(&"src/main.rs".to_string()));
        assert!(index.contains(&"src/lib.rs".to_string()));
        assert!(index.contains(&"Cargo.toml".to_string()));
    }

    #[test]
    fn build_file_index_includes_dot_dirs() {
        let dir = setup_test_project();
        let index = build_file_index(dir.path());
        assert!(
            index.contains(&".github/workflows/deploy.yml".to_string()),
            "file index should include files in dot-directories; got: {index:?}"
        );
    }

    #[test]
    fn find_unique_basename_in_dot_dir() {
        let dir = setup_test_project();
        let r = FileRef {
            raw: "@deploy.yml".into(),
            path: "deploy.yml".into(),
            inject: false,
            start: 0,
            end: 11,
        };
        let resolved = resolve_ref(&r, dir.path()).unwrap();
        assert_eq!(resolved.rel_path, ".github/workflows/deploy.yml");
        assert_eq!(resolved.language, Some("YAML".into()));
    }

    #[test]
    fn build_file_index_sorted() {
        let dir = setup_test_project();
        let index = build_file_index(dir.path());
        let mut sorted = index.clone();
        sorted.sort();
        assert_eq!(index, sorted);
    }
}
