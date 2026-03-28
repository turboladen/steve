//! Symbols tool — tree-sitter-based structural code analysis.
//!
//! Provides three operations:
//! - `list_symbols`: Walk AST, collect top-level + one-level-nested named nodes
//! - `find_scope`: Find the innermost named scope containing a given line
//! - `find_definition`: Find a symbol by name, show its source lines

use std::path::Path;

use serde_json::Value;
use tree_sitter::{Language, Node, Parser, Tree};

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

// ── Language detection ───────────────────────────────────────────────────

/// Supported language info for tree-sitter parsing.
pub(crate) struct LangInfo {
    pub(crate) language: Language,
    pub(crate) name: &'static str,
}

/// Detect the programming language from a file extension and return its grammar.
pub(crate) fn detect_language(path: &Path) -> Option<LangInfo> {
    let ext = path.extension()?.to_str()?;
    let (lang_fn, name): (tree_sitter_language::LanguageFn, &str) = match ext {
        "rs" => (tree_sitter_rust::LANGUAGE, "rust"),
        "py" | "pyi" => (tree_sitter_python::LANGUAGE, "python"),
        "js" | "mjs" | "cjs" => (tree_sitter_javascript::LANGUAGE, "javascript"),
        "ts" => (tree_sitter_typescript::LANGUAGE_TYPESCRIPT, "typescript"),
        "tsx" => (tree_sitter_typescript::LANGUAGE_TSX, "tsx"),
        "go" => (tree_sitter_go::LANGUAGE, "go"),
        "c" | "h" => (tree_sitter_c::LANGUAGE, "c"),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => (tree_sitter_cpp::LANGUAGE, "cpp"),
        "java" => (tree_sitter_java::LANGUAGE, "java"),
        "rb" => (tree_sitter_ruby::LANGUAGE, "ruby"),
        "toml" => (tree_sitter_toml_ng::LANGUAGE, "toml"),
        "json" => (tree_sitter_json::LANGUAGE, "json"),
        _ => return None,
    };
    Some(LangInfo {
        language: Language::from(lang_fn),
        name,
    })
}

// ── AST node type lists per language ─────────────────────────────────────

/// Return the set of AST node types that represent "symbols" for a given language.
fn symbol_node_types(lang_name: &str) -> &'static [&'static str] {
    match lang_name {
        "rust" => &[
            "function_item",
            "struct_item",
            "enum_item",
            "impl_item",
            "trait_item",
            "mod_item",
            "use_declaration",
            "type_item",
            "const_item",
            "static_item",
            "macro_definition",
        ],
        "python" => &[
            "function_definition",
            "class_definition",
            "import_statement",
            "import_from_statement",
        ],
        "javascript" => &[
            "function_declaration",
            "class_declaration",
            "variable_declaration",
            "import_statement",
            "export_statement",
        ],
        "typescript" | "tsx" => &[
            "function_declaration",
            "class_declaration",
            "variable_declaration",
            "import_statement",
            "export_statement",
            "interface_declaration",
            "type_alias_declaration",
            "enum_declaration",
        ],
        "go" => &[
            "function_declaration",
            "method_declaration",
            "type_declaration",
            "import_declaration",
        ],
        "c" => &[
            "function_definition",
            "struct_specifier",
            "enum_specifier",
            "type_definition",
            "preproc_include",
        ],
        "cpp" => &[
            "function_definition",
            "struct_specifier",
            "enum_specifier",
            "type_definition",
            "preproc_include",
            "class_specifier",
            "namespace_definition",
            "template_declaration",
        ],
        "java" => &[
            "class_declaration",
            "interface_declaration",
            "method_declaration",
            "enum_declaration",
            "import_declaration",
        ],
        "ruby" => &["method", "class", "module", "singleton_method"],
        "toml" => &["table", "table_array_element"],
        "json" => &["pair"],
        _ => &[],
    }
}

/// Node types that can contain nested symbols (classes, impls, modules, etc.).
fn container_node_types(lang_name: &str) -> &'static [&'static str] {
    match lang_name {
        "rust" => &["impl_item", "trait_item", "mod_item"],
        "python" => &["class_definition"],
        "javascript" | "typescript" | "tsx" => &["class_declaration", "class_body"],
        "go" => &[],
        "c" => &[],
        "cpp" => &["class_specifier", "namespace_definition"],
        "java" => &["class_declaration", "interface_declaration", "class_body"],
        "ruby" => &["class", "module"],
        _ => &[],
    }
}

// ── Symbol extraction ────────────────────────────────────────────────────

/// A discovered symbol in the AST.
#[derive(Debug, Clone)]
pub(crate) struct Symbol {
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) start_line: usize, // 1-indexed
    pub(crate) end_line: usize,   // 1-indexed
    pub(crate) children: Vec<Symbol>,
}

/// Extract the human-readable name from an AST node.
///
/// Different languages use different child node names for the symbol's identifier.
/// We try `name` first (most common), then fall back to searching for an
/// `identifier`/`type_identifier` child.
fn extract_name<'a>(node: Node<'a>, source: &'a [u8]) -> Option<String> {
    // Special case: impl blocks in Rust — look for a type child
    if node.kind() == "impl_item" {
        // Try to find the type being implemented
        // Pattern: impl [Trait for] Type { ... }
        if let Some(type_node) = node.child_by_field_name("type") {
            return Some(node_text(type_node, source));
        }
        // Fallback: find first type_identifier child
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "type_identifier" || child.kind() == "generic_type" {
                return Some(node_text(child, source));
            }
        }
    }

    // Special case: use declarations — show the whole text (trimmed)
    if node.kind() == "use_declaration"
        || node.kind() == "import_statement"
        || node.kind() == "import_from_statement"
        || node.kind() == "import_declaration"
        || node.kind() == "preproc_include"
    {
        let text = node_text(node, source);
        // Truncate long imports
        if text.chars().count() > 60 {
            let truncated: String = text.chars().take(57).collect();
            return Some(format!("{truncated}..."));
        }
        return Some(text);
    }

    // Try field `name` first (works for most languages)
    if let Some(name_node) = node.child_by_field_name("name") {
        return Some(node_text(name_node, source));
    }

    // Fallback: scan direct children for identifier-like nodes
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if kind == "identifier"
            || kind == "type_identifier"
            || kind == "property_identifier"
            || kind == "constant"
        {
            return Some(node_text(child, source));
        }
    }

    None
}

/// Get the text content of a node.
fn node_text(node: Node, source: &[u8]) -> String {
    node.utf8_text(source)
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

/// Simplify an AST node type name into a human-readable kind label.
pub(crate) fn kind_label(node_type: &str) -> &str {
    match node_type {
        // Rust
        "function_item" => "fn",
        "struct_item" => "struct",
        "enum_item" => "enum",
        "impl_item" => "impl",
        "trait_item" => "trait",
        "mod_item" => "mod",
        "use_declaration" => "use",
        "type_item" => "type",
        "const_item" => "const",
        "static_item" => "static",
        "macro_definition" => "macro",
        // Python
        "function_definition" => "def",
        "class_definition" => "class",
        "import_statement" | "import_from_statement" | "import_declaration" => "import",
        // JS/TS
        "function_declaration" => "function",
        "class_declaration" => "class",
        "variable_declaration" => "var",
        "export_statement" => "export",
        "interface_declaration" => "interface",
        "type_alias_declaration" => "type",
        "enum_declaration" => "enum",
        // Go (function_declaration already covered by JS, method_declaration below)
        "method_declaration" => "method",
        "type_declaration" => "type",
        // C/C++ (function_definition already covered by Python)
        "struct_specifier" => "struct",
        "enum_specifier" => "enum",
        "type_definition" => "typedef",
        "preproc_include" => "include",
        "class_specifier" => "class",
        "namespace_definition" => "namespace",
        "template_declaration" => "template",
        // Ruby (method_declaration already covered above)
        "method" | "singleton_method" => "def",
        "module" => "module",
        // TOML
        "table" | "table_array_element" => "section",
        // JSON
        "pair" => "key",
        // Fallback
        _ => node_type,
    }
}

/// Walk the AST and collect symbols up to `max_depth` levels deep.
pub(crate) fn walk_symbols(
    node: Node,
    source: &[u8],
    lang_name: &str,
    depth: usize,
) -> Vec<Symbol> {
    let symbol_types = symbol_node_types(lang_name);
    let container_types = container_node_types(lang_name);
    let mut symbols = Vec::new();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if symbol_types.contains(&child.kind()) {
            let name = extract_name(child, source).unwrap_or_else(|| "(anonymous)".to_string());
            let start_line = child.start_position().row + 1;
            let end_line = child.end_position().row + 1;

            // Collect nested symbols if this is a container (impl, class, etc.)
            let children = if depth < 1 && container_types.contains(&child.kind()) {
                walk_inner_symbols(child, source, lang_name)
            } else {
                Vec::new()
            };

            symbols.push(Symbol {
                kind: kind_label(child.kind()).to_string(),
                name,
                start_line,
                end_line,
                children,
            });
        }
    }

    symbols
}

/// Walk inside a container node to find nested symbols (methods, associated fns, etc.).
fn walk_inner_symbols(container: Node, source: &[u8], lang_name: &str) -> Vec<Symbol> {
    let symbol_types = symbol_node_types(lang_name);
    let mut symbols = Vec::new();

    // For some languages, nested symbols are inside a `body`/`declaration_list` child
    let body = container
        .child_by_field_name("body")
        .or_else(|| {
            // Rust impl/trait use `body` field, but let's also try scanning
            // for common body node types
            let mut cursor = container.walk();
            container.children(&mut cursor).find(|c| {
                matches!(
                    c.kind(),
                    "declaration_list" | "block" | "class_body" | "field_declaration_list"
                )
            })
        })
        .unwrap_or(container);

    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if symbol_types.contains(&child.kind()) {
            let name = extract_name(child, source).unwrap_or_else(|| "(anonymous)".to_string());
            let start_line = child.start_position().row + 1;
            let end_line = child.end_position().row + 1;

            symbols.push(Symbol {
                kind: kind_label(child.kind()).to_string(),
                name,
                start_line,
                end_line,
                children: Vec::new(),
            });
        }
    }

    symbols
}

// ── Scope finding ────────────────────────────────────────────────────────

/// Information about a scope containing a given line.
#[derive(Debug)]
struct ScopeInfo {
    kind: String,
    name: String,
    start_line: usize,
    end_line: usize,
    parent: Option<Box<ScopeInfo>>,
}

/// Find the innermost named scope containing `target_line` (1-indexed).
fn find_enclosing_scope(
    node: Node,
    source: &[u8],
    target_line: usize,
    lang_name: &str,
) -> Option<ScopeInfo> {
    let symbol_types = symbol_node_types(lang_name);
    let target_row = target_line - 1; // Convert to 0-indexed

    let mut best: Option<ScopeInfo> = None;
    let mut parent: Option<ScopeInfo> = None;

    find_scope_recursive(
        node,
        source,
        target_row,
        symbol_types,
        &mut best,
        &mut parent,
    );

    // Attach parent info if we found a nested scope
    if let Some(ref mut scope) = best {
        scope.parent = parent.map(Box::new);
    }

    best
}

fn find_scope_recursive(
    node: Node,
    source: &[u8],
    target_row: usize,
    symbol_types: &[&str],
    best: &mut Option<ScopeInfo>,
    parent: &mut Option<ScopeInfo>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let start = child.start_position().row;
        let end = child.end_position().row;

        if target_row < start || target_row > end {
            continue;
        }

        if symbol_types.contains(&child.kind()) {
            let name = extract_name(child, source).unwrap_or_else(|| "(anonymous)".to_string());
            let scope = ScopeInfo {
                kind: kind_label(child.kind()).to_string(),
                name,
                start_line: start + 1,
                end_line: end + 1,
                parent: None,
            };

            // If we already have a best, it becomes the parent
            if let Some(prev) = best.take() {
                *parent = Some(prev);
            }
            *best = Some(scope);
        }

        // Recurse into children
        find_scope_recursive(child, source, target_row, symbol_types, best, parent);
    }
}

// ── Definition finding ───────────────────────────────────────────────────

/// Information about a found definition.
#[derive(Debug)]
struct DefinitionInfo {
    kind: String,
    name: String,
    start_line: usize,
    end_line: usize,
    source_preview: String,
}

/// Find a symbol by name in the AST.
fn find_symbol_by_name(
    node: Node,
    source: &[u8],
    target_name: &str,
    lang_name: &str,
) -> Option<DefinitionInfo> {
    let symbol_types = symbol_node_types(lang_name);
    find_def_recursive(node, source, target_name, symbol_types)
}

fn find_def_recursive(
    node: Node,
    source: &[u8],
    target_name: &str,
    symbol_types: &[&str],
) -> Option<DefinitionInfo> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if symbol_types.contains(&child.kind()) {
            if let Some(name) = extract_name(child, source) {
                if name == target_name {
                    let start_line = child.start_position().row + 1;
                    let end_line = child.end_position().row + 1;

                    // Extract source preview (first ~20 lines)
                    let source_str = std::str::from_utf8(source).unwrap_or("");
                    let lines: Vec<&str> = source_str.lines().collect();
                    let preview_end = std::cmp::min(start_line - 1 + 20, end_line);
                    let preview_end = std::cmp::min(preview_end, lines.len());
                    let preview_start = start_line - 1;

                    let mut preview = String::new();
                    for (i, line) in lines[preview_start..preview_end].iter().enumerate() {
                        let line_num = preview_start + i + 1;
                        preview.push_str(&format!("{line_num:>4} | {line}\n"));
                    }
                    if preview_end < end_line {
                        preview.push_str(&format!(
                            "     ... ({} more lines)\n",
                            end_line - preview_end
                        ));
                    }

                    return Some(DefinitionInfo {
                        kind: kind_label(child.kind()).to_string(),
                        name,
                        start_line,
                        end_line,
                        source_preview: preview,
                    });
                }
            }
        }

        // Recurse
        if let Some(found) = find_def_recursive(child, source, target_name, symbol_types) {
            return Some(found);
        }
    }
    None
}

// ── Parsing helper ───────────────────────────────────────────────────────

fn parse_file(source: &[u8], language: Language) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    parser.parse(source, None)
}

// ── Tool entry point ─────────────────────────────────────────────────────

pub fn tool() -> ToolEntry {
    ToolEntry {
        def: ToolDef {
            name: ToolName::Symbols,
            description: "Analyze code structure using tree-sitter parsing. \
                Lists symbols (functions, classes, structs, etc.), finds the scope containing a line, \
                or finds a symbol definition by name."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path to analyze (relative to project root or absolute)"
                    },
                    "operation": {
                        "type": "string",
                        "enum": ["list_symbols", "find_scope", "find_definition"],
                        "description": "Operation to perform. Defaults to 'list_symbols'."
                    },
                    "name": {
                        "type": "string",
                        "description": "Symbol name to find (required for find_definition)"
                    },
                    "line": {
                        "type": "integer",
                        "description": "Line number (1-indexed) for find_scope"
                    }
                },
                "required": ["path"]
            }),
        },
        handler: Box::new(|args, ctx| execute(args, ctx)),
    }
}

fn execute(args: Value, ctx: ToolContext) -> anyhow::Result<ToolOutput> {
    let path_str = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing 'path' argument"))?;

    let operation: super::SymbolsOperation = args
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("list_symbols")
        .parse()
        .map_err(|_| {
            let raw = args.get("operation").and_then(|v| v.as_str()).unwrap_or("?");
            anyhow::anyhow!(
                "unknown symbols operation: '{raw}'. Expected one of: list_symbols, find_scope, find_definition"
            )
        })?;

    // Resolve path relative to project root
    let path = if Path::new(path_str).is_absolute() {
        std::path::PathBuf::from(path_str)
    } else {
        ctx.project_root.join(path_str)
    };

    if !path.exists() {
        return Ok(ToolOutput {
            title: format!("symbols {path_str}"),
            output: format!("Error: file not found: {}", path.display()),
            is_error: true,
        });
    }

    // Detect language
    let lang_info = match detect_language(&path) {
        Some(info) => info,
        None => {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("(none)");
            return Ok(ToolOutput {
                title: format!("symbols {path_str}"),
                output: format!(
                    "Unsupported file type: .{ext}. Supported: rs, py, js, ts, tsx, go, c, cpp, java, rb, toml, json"
                ),
                is_error: true,
            });
        }
    };

    // Read and parse
    let source = std::fs::read(&path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;

    if source.is_empty() {
        return Ok(ToolOutput {
            title: format!("symbols {path_str}"),
            output: format!("Empty file: {path_str}"),
            is_error: false,
        });
    }

    // Check for binary content
    if source[..std::cmp::min(source.len(), 8192)].contains(&0) {
        return Ok(ToolOutput {
            title: format!("symbols {path_str}"),
            output: format!("Binary file, cannot parse: {path_str}"),
            is_error: true,
        });
    }

    let tree = match parse_file(&source, lang_info.language) {
        Some(t) => t,
        None => {
            return Ok(ToolOutput {
                title: format!("symbols {path_str}"),
                output: format!("Failed to parse {path_str}"),
                is_error: true,
            });
        }
    };

    let root = tree.root_node();

    match operation {
        super::SymbolsOperation::ListSymbols => {
            let symbols = walk_symbols(root, &source, lang_info.name, 0);
            let total_lines = std::str::from_utf8(&source)
                .map(|s| s.lines().count())
                .unwrap_or(0);

            let mut output = format!(
                "Symbols in {path_str} ({}, {total_lines} lines):\n\n",
                lang_info.name
            );

            if symbols.is_empty() {
                output.push_str("  (no symbols found)\n");
            } else {
                for sym in &symbols {
                    output.push_str(&format!(
                        "  {} {} (line {}-{})\n",
                        sym.kind, sym.name, sym.start_line, sym.end_line
                    ));
                    for child in &sym.children {
                        output.push_str(&format!(
                            "    {} {} (line {}-{})\n",
                            child.kind, child.name, child.start_line, child.end_line
                        ));
                    }
                }
            }

            Ok(ToolOutput {
                title: format!("symbols {path_str}"),
                output,
                is_error: false,
            })
        }

        super::SymbolsOperation::FindScope => {
            let line = args
                .get("line")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow::anyhow!("find_scope requires 'line' argument"))?
                as usize;

            if line == 0 {
                return Ok(ToolOutput {
                    title: format!("symbols {path_str} scope@{line}"),
                    output: "Error: line must be >= 1 (1-indexed).".to_string(),
                    is_error: true,
                });
            }

            match find_enclosing_scope(root, &source, line, lang_info.name) {
                Some(scope) => {
                    let mut output = format!("Line {line} is inside:\n\n");
                    output.push_str(&format!(
                        "  {} {} (line {}-{})",
                        scope.kind, scope.name, scope.start_line, scope.end_line
                    ));
                    if let Some(parent) = &scope.parent {
                        output.push_str(&format!(
                            " in {} {} (line {}-{})",
                            parent.kind, parent.name, parent.start_line, parent.end_line
                        ));
                    }
                    output.push('\n');

                    Ok(ToolOutput {
                        title: format!("symbols {path_str} scope@{line}"),
                        output,
                        is_error: false,
                    })
                }
                None => Ok(ToolOutput {
                    title: format!("symbols {path_str} scope@{line}"),
                    output: format!("Line {line} is not inside any named scope."),
                    is_error: false,
                }),
            }
        }

        super::SymbolsOperation::FindDefinition => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("find_definition requires 'name' argument"))?;

            match find_symbol_by_name(root, &source, name, lang_info.name) {
                Some(def) => {
                    let mut output = format!("Definition of '{}':\n\n", def.name);
                    output.push_str(&format!(
                        "  {} {} (line {}-{})\n\n",
                        def.kind, def.name, def.start_line, def.end_line
                    ));
                    output.push_str(&def.source_preview);

                    Ok(ToolOutput {
                        title: format!("symbols {path_str} def:{name}"),
                        output,
                        is_error: false,
                    })
                }
                None => Ok(ToolOutput {
                    title: format!("symbols {path_str} def:{name}"),
                    output: format!("Symbol '{name}' not found in {path_str}."),
                    is_error: false,
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_ctx(dir: &Path) -> ToolContext {
        ToolContext {
            project_root: dir.to_path_buf(),
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        }
    }

    // ── Language detection ───────────────────────────────────────────────

    #[test]
    fn detect_language_all_supported_extensions() {
        let cases = [
            ("test.rs", "rust"),
            ("test.py", "python"),
            ("test.pyi", "python"),
            ("test.js", "javascript"),
            ("test.mjs", "javascript"),
            ("test.cjs", "javascript"),
            ("test.ts", "typescript"),
            ("test.tsx", "tsx"),
            ("test.go", "go"),
            ("test.c", "c"),
            ("test.h", "c"),
            ("test.cpp", "cpp"),
            ("test.cc", "cpp"),
            ("test.cxx", "cpp"),
            ("test.hpp", "cpp"),
            ("test.hxx", "cpp"),
            ("test.hh", "cpp"),
            ("test.java", "java"),
            ("test.rb", "ruby"),
            ("test.toml", "toml"),
            ("test.json", "json"),
        ];
        for (filename, expected_lang) in cases {
            let info = detect_language(Path::new(filename));
            assert!(info.is_some(), "should detect language for {filename}");
            assert_eq!(
                info.unwrap().name,
                expected_lang,
                "wrong language for {filename}"
            );
        }
    }

    #[test]
    fn detect_language_unsupported() {
        assert!(detect_language(Path::new("test.xyz")).is_none());
        assert!(detect_language(Path::new("test.md")).is_none());
        assert!(detect_language(Path::new("test.txt")).is_none());
        assert!(detect_language(Path::new("noext")).is_none());
    }

    // ── list_symbols ─────────────────────────────────────────────────────

    #[test]
    fn list_symbols_rust_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(
            &file,
            r#"
use std::io;

pub struct Foo {
    bar: i32,
}

impl Foo {
    pub fn new() -> Self {
        Foo { bar: 0 }
    }

    fn helper(&self) -> i32 {
        self.bar
    }
}

pub fn standalone() -> bool {
    true
}

enum Color {
    Red,
    Blue,
}
"#,
        )
        .unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap()});
        let result = execute(args, test_ctx(dir.path())).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("struct Foo"));
        assert!(result.output.contains("impl Foo"));
        assert!(result.output.contains("fn new"));
        assert!(result.output.contains("fn helper"));
        assert!(result.output.contains("fn standalone"));
        assert!(result.output.contains("enum Color"));
        assert!(result.output.contains("use"));
    }

    #[test]
    fn list_symbols_python_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.py");
        std::fs::write(
            &file,
            r#"
import os
from pathlib import Path

class MyClass:
    def __init__(self):
        pass

    def method(self):
        return 42

def standalone_func():
    pass
"#,
        )
        .unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap()});
        let result = execute(args, test_ctx(dir.path())).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("class MyClass"));
        assert!(result.output.contains("def __init__"));
        assert!(result.output.contains("def method"));
        assert!(result.output.contains("def standalone_func"));
        assert!(result.output.contains("import"));
    }

    #[test]
    fn list_symbols_javascript_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.js");
        std::fs::write(
            &file,
            r#"
import { foo } from './bar';

function greet(name) {
    return `Hello, ${name}`;
}

class Greeter {
    constructor(name) {
        this.name = name;
    }
}
"#,
        )
        .unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap()});
        let result = execute(args, test_ctx(dir.path())).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("function greet"));
        assert!(result.output.contains("class Greeter"));
    }

    #[test]
    fn list_symbols_typescript_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.ts");
        std::fs::write(
            &file,
            r#"
interface Config {
    host: string;
    port: number;
}

type Result<T> = { ok: true; value: T } | { ok: false; error: string };

enum Status {
    Active,
    Inactive,
}

function process(config: Config): void {}

class Processor {
    run(): void {}
}
"#,
        )
        .unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap()});
        let result = execute(args, test_ctx(dir.path())).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("interface Config"));
        assert!(result.output.contains("type Result"));
        assert!(result.output.contains("enum Status"));
        assert!(result.output.contains("function process"));
        assert!(result.output.contains("class Processor"));
    }

    // ── find_scope ───────────────────────────────────────────────────────

    #[test]
    fn find_scope_inside_function() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(
            &file,
            "fn main() {\n    let x = 5;\n    println!(\"{x}\");\n}\n",
        )
        .unwrap();

        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "operation": "find_scope",
            "line": 2
        });
        let result = execute(args, test_ctx(dir.path())).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("fn main"));
        assert!(result.output.contains("Line 2 is inside"));
    }

    #[test]
    fn find_scope_nested_method() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(
            &file,
            r#"
impl Foo {
    fn bar(&self) -> i32 {
        42
    }
}
"#,
        )
        .unwrap();

        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "operation": "find_scope",
            "line": 4
        });
        let result = execute(args, test_ctx(dir.path())).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("fn bar"));
        assert!(result.output.contains("impl Foo"));
    }

    #[test]
    fn find_scope_top_level() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "use std::io;\n\nfn main() {}\n").unwrap();

        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "operation": "find_scope",
            "line": 1
        });
        let result = execute(args, test_ctx(dir.path())).unwrap();

        assert!(!result.is_error);
        // Line 1 is `use std::io;` — it is inside a use_declaration
        assert!(result.output.contains("use") || result.output.contains("not inside"));
    }

    // ── find_definition ──────────────────────────────────────────────────

    #[test]
    fn find_definition_found() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(
            &file,
            r#"
struct Foo {
    x: i32,
    y: String,
}

fn bar() -> bool {
    true
}
"#,
        )
        .unwrap();

        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "operation": "find_definition",
            "name": "Foo"
        });
        let result = execute(args, test_ctx(dir.path())).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("struct Foo"));
        assert!(result.output.contains("x: i32"));
    }

    #[test]
    fn find_definition_not_found() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "operation": "find_definition",
            "name": "NonExistent"
        });
        let result = execute(args, test_ctx(dir.path())).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("not found"));
    }

    #[test]
    fn find_definition_includes_source() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        let mut content = String::from("pub struct BigStruct {\n");
        for i in 0..25 {
            content.push_str(&format!("    field_{i}: i32,\n"));
        }
        content.push_str("}\n");
        std::fs::write(&file, &content).unwrap();

        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "operation": "find_definition",
            "name": "BigStruct"
        });
        let result = execute(args, test_ctx(dir.path())).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("struct BigStruct"));
        // Should show first ~20 lines and a "more lines" note
        assert!(result.output.contains("more lines"));
    }

    // ── Edge cases ───────────────────────────────────────────────────────

    #[test]
    fn empty_file_handled() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("empty.rs");
        std::fs::write(&file, "").unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap()});
        let result = execute(args, test_ctx(dir.path())).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("Empty file"));
    }

    #[test]
    fn missing_file_returns_error() {
        let dir = tempdir().unwrap();
        let args = serde_json::json!({"path": dir.path().join("nope.rs").to_str().unwrap()});
        let result = execute(args, test_ctx(dir.path())).unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("not found"));
    }

    #[test]
    fn binary_file_returns_error() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("binary.rs");
        std::fs::write(&file, b"\x00\x01\x02\x03").unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap()});
        let result = execute(args, test_ctx(dir.path())).unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("Binary file"));
    }

    #[test]
    fn find_scope_line_zero_returns_error() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "operation": "find_scope",
            "line": 0
        });
        let result = execute(args, test_ctx(dir.path())).unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("line must be >= 1"));
    }

    #[test]
    fn unsupported_extension_returns_error() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.xyz");
        std::fs::write(&file, "hello").unwrap();

        let args = serde_json::json!({"path": file.to_str().unwrap()});
        let result = execute(args, test_ctx(dir.path())).unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("Unsupported"));
    }

    #[test]
    fn default_operation_is_list_symbols() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        // No "operation" key — should default to list_symbols
        let args = serde_json::json!({"path": file.to_str().unwrap()});
        let result = execute(args, test_ctx(dir.path())).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("Symbols in"));
    }

    #[test]
    fn find_scope_missing_line_arg_returns_error() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "operation": "find_scope"
        });
        let result = execute(args, test_ctx(dir.path()));

        assert!(result.is_err());
    }

    #[test]
    fn find_definition_missing_name_arg_returns_error() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "operation": "find_definition"
        });
        let result = execute(args, test_ctx(dir.path()));

        assert!(result.is_err());
    }

    #[test]
    fn unknown_operation_returns_error() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "operation": "unknown_op"
        });
        let err = execute(args, test_ctx(dir.path())).unwrap_err();

        assert!(
            err.to_string().contains("unknown symbols operation"),
            "expected parse error, got: {err}"
        );
    }

    #[test]
    fn relative_path_resolved_to_project_root() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("lib.rs");
        std::fs::write(&file, "pub fn greet() {}\n").unwrap();

        // Use relative path
        let args = serde_json::json!({"path": "src/lib.rs"});
        let result = execute(args, test_ctx(dir.path())).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("fn greet"));
    }
}
