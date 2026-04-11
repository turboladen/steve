//! find_symbol tool — orchestrates grep + tree-sitter + LSP for symbol navigation.
//!
//! The LLM rarely chains grep→lsp manually. This tool does it in one call:
//! 1. Grep for the symbol name across the project
//! 2. Tree-sitter to classify matches as definitions vs references
//! 3. LSP enrichment (when available) for authoritative results
//!
//! Falls back gracefully at each layer.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Result;
use grep::{
    regex::RegexMatcher,
    searcher::{Searcher, sinks::UTF8},
};
use ignore::WalkBuilder;
use serde_json::Value;

use crate::lsp::uri_to_path;

use super::{
    FindSymbolOperation, ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput,
    symbols::{self, DefinitionInfo},
};

pub fn tool() -> ToolEntry {
    ToolEntry {
        def: ToolDef {
            name: ToolName::FindSymbol,
            description: "Find where a symbol is defined, all its usages, or both. Combines \
                grep, tree-sitter, and LSP in a single call for accurate results. Use this \
                instead of manually chaining grep → lsp. Supports functions, types, traits, \
                variables, and methods."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Name of the function, type, variable, or method to find"
                    },
                    "operation": {
                        "type": "string",
                        "enum": ["definition", "references", "overview"],
                        "description": "What to find: 'definition' (where defined), 'references' (all usages), 'overview' (both). Defaults to 'overview'."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Directory or file to search within (relative to project root). Defaults to project root."
                    }
                },
                "required": ["symbol"]
            }),
        },
        handler: Box::new(execute),
    }
}

// ── Types ───────────────────────────────────────────────────────────────────

/// A grep match with location info.
struct GrepMatch {
    path: PathBuf,
    line: u32,
    content: String,
}

/// Classification of a grep match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MatchKind {
    Definition,
    Reference,
}

/// A classified match ready for output.
struct ClassifiedMatch {
    path: PathBuf,
    line: u32,
    content: String,
    kind: MatchKind,
}

// ── Handler ─────────────────────────────────────────────────────────────────

fn execute(args: Value, ctx: ToolContext) -> Result<ToolOutput> {
    // Phase A: Parse arguments
    let symbol = args
        .get("symbol")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing 'symbol' argument"))?;

    let operation: FindSymbolOperation = match args.get("operation") {
        Some(v) => {
            let s = v
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("'operation' must be a string"))?;
            s.parse().map_err(|_| {
                anyhow::anyhow!(
                    "unknown operation '{s}'. Expected: definition, references, overview"
                )
            })?
        }
        None => FindSymbolOperation::Overview,
    };

    let scope_path = args
        .get("scope")
        .and_then(|v| v.as_str())
        .map(|p| super::resolve_path(p, &ctx.project_root))
        .unwrap_or_else(|| ctx.project_root.clone());

    // Phase B: Grep scan
    let grep_matches = grep_for_symbol(symbol, &scope_path, &ctx.project_root)?;

    if grep_matches.is_empty() {
        return Ok(ToolOutput {
            title: format!("find_symbol {operation}:{symbol}"),
            output: format!("No matches found for symbol '{symbol}'."),
            is_error: false,
        });
    }

    // Phase C: Tree-sitter classification
    let (definitions, classified) = classify_matches(symbol, &grep_matches, &ctx.project_root);

    // Phase D: LSP enrichment
    let (lsp_definitions, lsp_references) =
        try_lsp_enrichment(symbol, operation, &definitions, &ctx);

    // Phase E: Format output
    let output = format_output(
        symbol,
        operation,
        &definitions,
        &classified,
        &lsp_definitions,
        &lsp_references,
        &ctx.project_root,
    );

    Ok(ToolOutput {
        title: format!("find_symbol {operation}:{symbol}"),
        output,
        is_error: false,
    })
}

// ── Phase B: Grep scan ──────────────────────────────────────────────────────

const MAX_GREP_RESULTS: usize = 200;

/// Whether a symbol name consists entirely of identifier characters (letters, digits, underscore).
/// Word-boundary `\b` anchors only work reliably for such symbols.
fn is_identifier(symbol: &str) -> bool {
    let mut chars = symbol.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn grep_for_symbol(symbol: &str, scope: &Path, project_root: &Path) -> Result<Vec<GrepMatch>> {
    let escaped = regex_syntax::escape(symbol);
    let pattern = if is_identifier(symbol) {
        format!(r"\b{escaped}\b")
    } else {
        escaped
    };
    let matcher = RegexMatcher::new(&pattern)
        .map_err(|e| anyhow::anyhow!("invalid symbol name for search: {e}"))?;

    let mut results = Vec::new();
    let mut walker_builder = WalkBuilder::new(scope);
    walker_builder.hidden(true);
    walker_builder.git_ignore(true);

    for entry in walker_builder.build() {
        if results.len() >= MAX_GREP_RESULTS {
            break;
        }

        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        let file_path = entry.path().to_path_buf();
        let mut searcher = Searcher::new();

        let _ = searcher.search_path(
            &matcher,
            &file_path,
            UTF8(|line_num, line| {
                if results.len() >= MAX_GREP_RESULTS {
                    return Ok(false);
                }

                let trimmed = line.trim_end();
                let display_line = if trimmed.len() > 200 {
                    let end = trimmed.floor_char_boundary(197);
                    format!("{}...", &trimmed[..end])
                } else {
                    trimmed.to_string()
                };

                results.push(GrepMatch {
                    path: file_path
                        .strip_prefix(project_root)
                        .unwrap_or(&file_path)
                        .to_path_buf(),
                    line: line_num as u32,
                    content: display_line,
                });
                Ok(true)
            }),
        );
    }

    Ok(results)
}

// ── Phase C: Tree-sitter classification ─────────────────────────────────────

/// Classify grep matches as definitions or references using tree-sitter.
/// Returns (definition_infos, all_classified_matches).
fn classify_matches(
    symbol: &str,
    matches: &[GrepMatch],
    project_root: &Path,
) -> (Vec<(PathBuf, DefinitionInfo)>, Vec<ClassifiedMatch>) {
    // Group matches by file to avoid re-parsing
    let mut by_file: HashMap<&Path, Vec<&GrepMatch>> = HashMap::new();
    for m in matches {
        by_file.entry(m.path.as_path()).or_default().push(m);
    }

    let mut definitions: Vec<(PathBuf, DefinitionInfo)> = Vec::new();
    let mut classified: Vec<ClassifiedMatch> = Vec::new();

    // Files where tree-sitter found a definition
    let mut def_files: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    for (rel_path, file_matches) in &by_file {
        let abs_path = project_root.join(rel_path);

        // Try tree-sitter definition detection
        if let Some(lang_info) = symbols::detect_language(&abs_path)
            && let Ok(source) = std::fs::read(&abs_path)
            && let Some(tree) = symbols::parse_file(&source, lang_info.language)
        {
            let root = tree.root_node();
            if let Some(def_info) =
                symbols::find_symbol_by_name(root, &source, symbol, lang_info.lang)
            {
                def_files.insert(rel_path.to_path_buf());
                definitions.push((rel_path.to_path_buf(), def_info));
            }
        }

        // Classify each match — only the declaration line itself is a definition,
        // not the entire function/struct body (avoids mislabeling recursive calls).
        for m in file_matches {
            let kind = if def_files.contains(*rel_path) {
                if let Some((_, def_info)) = definitions.iter().find(|(p, _)| p == *rel_path) {
                    if m.line as usize == def_info.start_line {
                        MatchKind::Definition
                    } else {
                        MatchKind::Reference
                    }
                } else {
                    MatchKind::Reference
                }
            } else {
                MatchKind::Reference
            };

            classified.push(ClassifiedMatch {
                path: m.path.clone(),
                line: m.line,
                content: m.content.clone(),
                kind,
            });
        }
    }

    // Sort: definitions first, then references, each sorted by path then line
    classified.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });

    (definitions, classified)
}

// ── Phase D: LSP enrichment ─────────────────────────────────────────────────

/// A list of (relative_path, 1-indexed line) pairs from LSP.
type LspLocations = Vec<(PathBuf, u32)>;

/// Try LSP for authoritative definition/references results.
/// Returns (lsp_definitions, lsp_references) — empty vecs if LSP unavailable.
fn try_lsp_enrichment(
    symbol: &str,
    operation: FindSymbolOperation,
    definitions: &[(PathBuf, DefinitionInfo)],
    ctx: &ToolContext,
) -> (LspLocations, LspLocations) {
    let lsp_manager = match &ctx.lsp_manager {
        Some(mgr) => mgr,
        None => return (Vec::new(), Vec::new()),
    };

    // Need a definition site for LSP calls
    let (def_path, def_line, def_col) = match definitions.first() {
        Some((rel_path, _)) => {
            let abs_path = ctx.project_root.join(rel_path);
            match symbols::resolve_symbol_position(&abs_path, symbol) {
                Ok((row, col)) => (abs_path, row, col),
                Err(_) => return (Vec::new(), Vec::new()),
            }
        }
        None => return (Vec::new(), Vec::new()),
    };

    let wants_def = matches!(
        operation,
        FindSymbolOperation::Definition | FindSymbolOperation::Overview
    );
    let wants_refs = matches!(
        operation,
        FindSymbolOperation::References | FindSymbolOperation::Overview
    );

    let mut lsp_defs = Vec::new();
    let mut lsp_refs = Vec::new();

    // Try read lock first (common path — server already running), fall back to
    // write lock to start server. Perform all LSP calls inside the lock scope
    // since we can't return &LspServer through the guard.
    let lsp_result: Option<(Vec<_>, Vec<_>)> = (|| {
        // Try read lock
        let mgr = lsp_manager.read().ok()?;
        match mgr.server_for_file(&def_path) {
            Ok(server) => {
                let defs = if wants_def {
                    server.definition(&def_path, def_line, def_col).ok()
                } else {
                    None
                };
                let refs = if wants_refs {
                    server.references(&def_path, def_line, def_col).ok()
                } else {
                    None
                };
                Some((defs.unwrap_or_default(), refs.unwrap_or_default()))
            }
            Err(_) => {
                // Server not running — need write lock to start it
                drop(mgr);
                let mut mgr = lsp_manager.write().ok()?;
                let server = mgr.server_for_file_or_start(&def_path).ok()?;
                let defs = if wants_def {
                    server.definition(&def_path, def_line, def_col).ok()
                } else {
                    None
                };
                let refs = if wants_refs {
                    server.references(&def_path, def_line, def_col).ok()
                } else {
                    None
                };
                Some((defs.unwrap_or_default(), refs.unwrap_or_default()))
            }
        }
    })();

    if let Some((def_locations, ref_locations)) = lsp_result {
        for loc in &def_locations {
            if let Some(path) = uri_to_path(loc.uri.as_str()) {
                let rel = path
                    .strip_prefix(&ctx.project_root)
                    .unwrap_or(&path)
                    .to_path_buf();
                lsp_defs.push((rel, loc.range.start.line + 1));
            }
        }
        for loc in &ref_locations {
            if let Some(path) = uri_to_path(loc.uri.as_str()) {
                let rel = path
                    .strip_prefix(&ctx.project_root)
                    .unwrap_or(&path)
                    .to_path_buf();
                lsp_refs.push((rel, loc.range.start.line + 1));
            }
        }
    }

    (lsp_defs, lsp_refs)
}

// ── Phase E: Format output ──────────────────────────────────────────────────

fn format_output(
    symbol: &str,
    operation: FindSymbolOperation,
    definitions: &[(PathBuf, DefinitionInfo)],
    classified: &[ClassifiedMatch],
    lsp_definitions: &[(PathBuf, u32)],
    lsp_references: &[(PathBuf, u32)],
    project_root: &Path,
) -> String {
    let mut output = String::new();

    let wants_def = matches!(
        operation,
        FindSymbolOperation::Definition | FindSymbolOperation::Overview
    );
    let wants_refs = matches!(
        operation,
        FindSymbolOperation::References | FindSymbolOperation::Overview
    );

    // Definition section
    if wants_def {
        output.push_str("## Definition\n\n");

        if !lsp_definitions.is_empty() {
            // LSP authoritative definitions
            for (path, line) in lsp_definitions {
                output.push_str(&format!("{}:{}", path.display(), line));
                let abs = project_root.join(path);
                let context = super::lsp::read_context(&abs, (*line as usize).saturating_sub(1), 3);
                if !context.is_empty() {
                    output.push('\n');
                    output.push_str(&context);
                }
                output.push('\n');
            }
        } else if !definitions.is_empty() {
            // Tree-sitter definitions
            for (path, def_info) in definitions {
                output.push_str(&format!(
                    "{}:{} [{}] {}\n",
                    path.display(),
                    def_info.start_line,
                    def_info.kind,
                    def_info.name
                ));
                output.push_str(&def_info.source_preview);
                output.push('\n');
            }
        } else {
            output.push_str(&format!("No definition found for '{symbol}'.\n\n"));
        }
    }

    // References section
    if wants_refs {
        if wants_def && !output.is_empty() {
            output.push('\n');
        }

        let refs: Vec<String> = if !lsp_references.is_empty() {
            // LSP authoritative references — batch reads by file to avoid re-reading
            let mut file_cache: HashMap<&Path, Vec<String>> = HashMap::new();
            lsp_references
                .iter()
                .map(|(path, line)| {
                    let lines = file_cache.entry(path.as_path()).or_insert_with(|| {
                        let abs = project_root.join(path);
                        std::fs::read_to_string(&abs)
                            .unwrap_or_default()
                            .lines()
                            .map(String::from)
                            .collect()
                    });
                    let content = lines
                        .get((*line as usize).saturating_sub(1))
                        .map(|s| s.trim())
                        .unwrap_or("");
                    format!("{}:{}: {}", path.display(), line, content)
                })
                .collect()
        } else {
            // Fall back to grep-classified references
            classified
                .iter()
                .filter(|m| m.kind == MatchKind::Reference)
                .map(|m| format!("{}:{}: {}", m.path.display(), m.line, m.content))
                .collect()
        };

        output.push_str(&format!("## References ({} found)\n\n", refs.len()));
        for r in &refs {
            output.push_str(r);
            output.push('\n');
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::test_tool_context;
    use tempfile::tempdir;

    #[test]
    fn find_symbol_operation_round_trip() {
        for (s, expected) in [
            ("definition", FindSymbolOperation::Definition),
            ("references", FindSymbolOperation::References),
            ("overview", FindSymbolOperation::Overview),
        ] {
            let parsed: FindSymbolOperation = s.parse().unwrap();
            assert_eq!(parsed, expected);
            assert_eq!(parsed.to_string(), s);
        }
    }

    #[test]
    fn find_symbol_operation_serde_round_trip() {
        for op in [
            FindSymbolOperation::Definition,
            FindSymbolOperation::References,
            FindSymbolOperation::Overview,
        ] {
            let json = serde_json::to_string(&op).unwrap();
            let parsed: FindSymbolOperation = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, op, "serde round-trip failed for {op}");
        }
    }

    #[test]
    fn grep_scan_escapes_regex_metacharacters() {
        let dir = tempdir().unwrap();
        // File containing a C++ operator+ symbol (has regex metachar)
        std::fs::write(
            dir.path().join("ops.cpp"),
            "int operator+(int a, int b) { return a + b; }\n",
        )
        .unwrap();

        // Should not panic or produce regex error
        let result = grep_for_symbol("operator+", dir.path(), dir.path());
        assert!(result.is_ok(), "regex metacharacters should be escaped");
    }

    #[test]
    fn grep_scan_finds_symbol() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.rs"),
            "fn hello_world() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("other.rs"),
            "fn caller() {\n    hello_world();\n}\n",
        )
        .unwrap();

        let matches = grep_for_symbol("hello_world", dir.path(), dir.path()).unwrap();
        assert!(
            matches.len() >= 2,
            "expected at least 2 matches, got {}",
            matches.len()
        );
    }

    #[test]
    fn tree_sitter_classifies_definition_vs_reference() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "fn my_func() {\n    println!(\"inside\");\n}\n\nfn caller() {\n    my_func();\n}\n",
        )
        .unwrap();

        let matches = grep_for_symbol("my_func", dir.path(), dir.path()).unwrap();
        let (definitions, classified) = classify_matches("my_func", &matches, dir.path());

        assert!(
            !definitions.is_empty(),
            "should find at least one definition"
        );
        assert_eq!(definitions[0].1.kind, "fn");

        let def_matches: Vec<_> = classified
            .iter()
            .filter(|m| m.kind == MatchKind::Definition)
            .collect();
        let ref_matches: Vec<_> = classified
            .iter()
            .filter(|m| m.kind == MatchKind::Reference)
            .collect();

        assert!(!def_matches.is_empty(), "should have definition matches");
        assert!(!ref_matches.is_empty(), "should have reference matches");
    }

    #[test]
    fn recursive_call_classified_as_reference() {
        let dir = tempdir().unwrap();
        // Function with a recursive call inside its own body
        std::fs::write(
            dir.path().join("lib.rs"),
            "fn recurse(n: u32) {\n    if n > 0 {\n        recurse(n - 1);\n    }\n}\n",
        )
        .unwrap();

        let matches = grep_for_symbol("recurse", dir.path(), dir.path()).unwrap();
        let (_, classified) = classify_matches("recurse", &matches, dir.path());

        let def_count = classified
            .iter()
            .filter(|m| m.kind == MatchKind::Definition)
            .count();
        let ref_count = classified
            .iter()
            .filter(|m| m.kind == MatchKind::Reference)
            .count();

        assert_eq!(
            def_count, 1,
            "only the declaration line should be Definition"
        );
        assert!(
            ref_count >= 1,
            "recursive call should be classified as Reference"
        );
    }

    #[test]
    fn is_identifier_classification() {
        assert!(is_identifier("foo_bar"));
        assert!(is_identifier("_private"));
        assert!(is_identifier("MyType123"));
        assert!(!is_identifier("operator+"));
        assert!(!is_identifier("foo.bar"));
        assert!(!is_identifier("Type::method"));
        assert!(!is_identifier(""));
        assert!(!is_identifier("123abc"));
    }

    #[test]
    fn overview_has_both_sections() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "fn target_fn() {}\nfn other() { target_fn(); }\n",
        )
        .unwrap();

        let ctx = test_tool_context(dir.path().to_path_buf());
        let args = serde_json::json!({"symbol": "target_fn"});
        let result = execute(args, ctx).unwrap();

        assert!(
            result.output.contains("## Definition"),
            "overview should contain Definition section"
        );
        assert!(
            result.output.contains("## References"),
            "overview should contain References section"
        );
    }

    #[test]
    fn definition_only() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "fn target_fn() {}\nfn other() { target_fn(); }\n",
        )
        .unwrap();

        let ctx = test_tool_context(dir.path().to_path_buf());
        let args = serde_json::json!({"symbol": "target_fn", "operation": "definition"});
        let result = execute(args, ctx).unwrap();

        assert!(result.output.contains("## Definition"));
        assert!(!result.output.contains("## References"));
    }

    #[test]
    fn references_only() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "fn target_fn() {}\nfn other() { target_fn(); }\n",
        )
        .unwrap();

        let ctx = test_tool_context(dir.path().to_path_buf());
        let args = serde_json::json!({"symbol": "target_fn", "operation": "references"});
        let result = execute(args, ctx).unwrap();

        assert!(!result.output.contains("## Definition"));
        assert!(result.output.contains("## References"));
    }

    #[test]
    fn no_matches_returns_helpful_message() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn unrelated() {}\n").unwrap();

        let ctx = test_tool_context(dir.path().to_path_buf());
        let args = serde_json::json!({"symbol": "nonexistent_symbol"});
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("No matches found"));
    }

    #[test]
    fn scope_restricts_search() {
        let dir = tempdir().unwrap();
        let subdir = dir.path().join("src");
        std::fs::create_dir(&subdir).unwrap();
        std::fs::write(dir.path().join("root.rs"), "fn scoped_fn() {}\n").unwrap();
        std::fs::write(subdir.join("inner.rs"), "fn scoped_fn() {}\n").unwrap();

        let ctx = test_tool_context(dir.path().to_path_buf());
        let args = serde_json::json!({"symbol": "scoped_fn", "scope": "src"});
        let result = execute(args, ctx).unwrap();

        // Should only find the one in src/, not root.rs
        assert!(
            !result.output.contains("root.rs"),
            "scope should exclude root.rs"
        );
        assert!(
            result.output.contains("inner.rs"),
            "scope should include src/inner.rs"
        );
    }

    #[test]
    fn no_lsp_falls_back_gracefully() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "fn fallback_fn() {}\nfn caller() { fallback_fn(); }\n",
        )
        .unwrap();

        // test_tool_context has lsp_manager: None
        let ctx = test_tool_context(dir.path().to_path_buf());
        let args = serde_json::json!({"symbol": "fallback_fn"});
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("## Definition"));
        assert!(result.output.contains("fallback_fn"));
    }
}
