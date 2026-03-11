//! Integration tests for tool execution.
//!
//! Tests tools against real filesystem fixtures in temp directories,
//! verifying end-to-end behavior including path resolution and output.

use std::path::PathBuf;
use serde_json::json;
use tempfile::tempdir;

use steve::tool::{ToolName, ToolContext, ToolRegistry};

/// Helper to create a ToolContext with a temp directory as project root.
fn tool_context(project_root: PathBuf) -> ToolContext {
    ToolContext {
        project_root,
        storage_dir: None,
        task_store: None,
    }
}

/// Helper to create a temp project with some files.
fn create_test_project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();

    // Create directory structure
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();

    // Create files
    std::fs::write(root.join("src/main.rs"), "fn main() {\n    println!(\"hello\");\n}\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn greet() -> &'static str {\n    \"hello\"\n}\n").unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"test\"\nversion = \"0.1.0\"\n").unwrap();

    (dir, root)
}

#[test]
fn read_tool_returns_file_content() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    let output = registry.execute(
        ToolName::Read,
        json!({ "path": root.join("src/main.rs").to_string_lossy().to_string() }),
        ctx,
    ).unwrap();

    assert!(!output.is_error);
    assert!(output.output.contains("fn main()"));
    assert!(output.output.contains("println!"));
}

#[test]
fn read_tool_nonexistent_file_returns_error() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    let output = registry.execute(
        ToolName::Read,
        json!({ "path": root.join("nonexistent.rs").to_string_lossy().to_string() }),
        ctx,
    ).unwrap();

    assert!(output.is_error);
}

#[test]
fn glob_tool_finds_rust_files() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    let output = registry.execute(
        ToolName::Glob,
        json!({ "pattern": "**/*.rs", "path": root.to_string_lossy().to_string() }),
        ctx,
    ).unwrap();

    assert!(!output.is_error);
    assert!(output.output.contains("main.rs"));
    assert!(output.output.contains("lib.rs"));
}

#[test]
fn edit_tool_replaces_content() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());
    let file_path = root.join("src/main.rs").to_string_lossy().to_string();

    let output = registry.execute(
        ToolName::Edit,
        json!({
            "file_path": &file_path,
            "old_string": "println!(\"hello\")",
            "new_string": "println!(\"world\")"
        }),
        ctx,
    ).unwrap();

    assert!(!output.is_error, "edit should succeed: {}", output.output);

    // Verify the file was actually changed
    let content = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert!(content.contains("println!(\"world\")"));
    assert!(!content.contains("println!(\"hello\")"));
}

#[test]
fn write_tool_creates_new_file() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());
    let file_path = root.join("src/new_file.rs").to_string_lossy().to_string();

    let output = registry.execute(
        ToolName::Write,
        json!({
            "file_path": &file_path,
            "content": "pub fn new_fn() {}\n"
        }),
        ctx,
    ).unwrap();

    assert!(!output.is_error, "write should succeed: {}", output.output);
    let content = std::fs::read_to_string(root.join("src/new_file.rs")).unwrap();
    assert_eq!(content, "pub fn new_fn() {}\n");
}

#[test]
fn list_tool_shows_directory_contents() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    let output = registry.execute(
        ToolName::List,
        json!({ "path": root.join("src").to_string_lossy().to_string() }),
        ctx,
    ).unwrap();

    assert!(!output.is_error);
    assert!(output.output.contains("main.rs"));
    assert!(output.output.contains("lib.rs"));
}

#[test]
fn grep_tool_finds_matches() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    let output = registry.execute(
        ToolName::Grep,
        json!({
            "pattern": "fn main",
            "path": root.to_string_lossy().to_string()
        }),
        ctx,
    ).unwrap();

    assert!(!output.is_error);
    assert!(output.output.contains("main.rs"), "should find fn main in main.rs");
}

#[test]
fn delete_tool_removes_file() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());
    let file_path = root.join("src/lib.rs").to_string_lossy().to_string();

    assert!(root.join("src/lib.rs").exists());

    let output = registry.execute(
        ToolName::Delete,
        json!({ "path": &file_path }),
        ctx,
    ).unwrap();

    assert!(!output.is_error);
    assert!(!root.join("src/lib.rs").exists());
}

#[test]
fn mkdir_tool_creates_directory() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());
    let dir_path = root.join("src/new_module").to_string_lossy().to_string();

    let output = registry.execute(
        ToolName::Mkdir,
        json!({ "path": &dir_path }),
        ctx,
    ).unwrap();

    assert!(!output.is_error);
    assert!(root.join("src/new_module").is_dir());
}

#[test]
fn move_tool_renames_file() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());
    let from = root.join("src/lib.rs").to_string_lossy().to_string();
    let to = root.join("src/library.rs").to_string_lossy().to_string();

    let output = registry.execute(
        ToolName::Move,
        json!({ "from_path": &from, "to_path": &to }),
        ctx,
    ).unwrap();

    assert!(!output.is_error);
    assert!(!root.join("src/lib.rs").exists());
    assert!(root.join("src/library.rs").exists());
}

#[test]
fn copy_tool_duplicates_file() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());
    let from = root.join("src/main.rs").to_string_lossy().to_string();
    let to = root.join("src/main_backup.rs").to_string_lossy().to_string();

    let output = registry.execute(
        ToolName::Copy,
        json!({ "from_path": &from, "to_path": &to }),
        ctx,
    ).unwrap();

    assert!(!output.is_error);
    assert!(root.join("src/main.rs").exists(), "original should still exist");
    assert!(root.join("src/main_backup.rs").exists(), "copy should exist");

    let original = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    let copy = std::fs::read_to_string(root.join("src/main_backup.rs")).unwrap();
    assert_eq!(original, copy);
}

// ── Edit tool: line-based operations ──

#[test]
fn edit_insert_lines_through_registry() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());
    let file_path = root.join("src/main.rs").to_string_lossy().to_string();

    let output = registry.execute(
        ToolName::Edit,
        json!({
            "file_path": &file_path,
            "operation": "insert_lines",
            "line": 2,
            "content": "    // inserted comment"
        }),
        ctx,
    ).unwrap();

    assert!(!output.is_error, "insert_lines should succeed: {}", output.output);
    let content = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert!(content.contains("// inserted comment"));
    assert!(content.contains("fn main()"));
}

#[test]
fn edit_delete_lines_through_registry() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());
    let file_path = root.join("src/main.rs").to_string_lossy().to_string();

    // main.rs is: "fn main() {\n    println!(\"hello\");\n}\n" (3 lines)
    let output = registry.execute(
        ToolName::Edit,
        json!({
            "file_path": &file_path,
            "operation": "delete_lines",
            "start_line": 2,
            "end_line": 2
        }),
        ctx,
    ).unwrap();

    assert!(!output.is_error, "delete_lines should succeed: {}", output.output);
    let content = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert!(!content.contains("println!"));
    assert_eq!(content, "fn main() {\n}\n");
}

#[test]
fn edit_replace_range_through_registry() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());
    let file_path = root.join("src/main.rs").to_string_lossy().to_string();

    let output = registry.execute(
        ToolName::Edit,
        json!({
            "file_path": &file_path,
            "operation": "replace_range",
            "start_line": 2,
            "end_line": 2,
            "content": "    eprintln!(\"debug\");\n    println!(\"world\");"
        }),
        ctx,
    ).unwrap();

    assert!(!output.is_error, "replace_range should succeed: {}", output.output);
    let content = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert!(content.contains("eprintln!"));
    assert!(content.contains("println!(\"world\")"));
    assert!(!content.contains("println!(\"hello\")"));
}

// ── Symbols tool ──

#[test]
fn symbols_tool_lists_rust_symbols() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    let output = registry.execute(
        ToolName::Symbols,
        json!({ "path": root.join("src/main.rs").to_string_lossy().to_string() }),
        ctx,
    ).unwrap();

    assert!(!output.is_error, "symbols should succeed: {}", output.output);
    assert!(output.output.contains("fn main"), "should find fn main");
    assert!(output.output.contains("Symbols in"), "should have header");
}

#[test]
fn symbols_tool_unsupported_file_type() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    let output = registry.execute(
        ToolName::Symbols,
        json!({ "path": root.join("Cargo.toml").to_string_lossy().to_string() }),
        ctx,
    ).unwrap();

    // TOML is supported, so it should parse
    assert!(!output.is_error);
}

#[test]
fn symbols_tool_find_definition() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    // lib.rs has `pub fn greet()`
    let output = registry.execute(
        ToolName::Symbols,
        json!({
            "path": root.join("src/lib.rs").to_string_lossy().to_string(),
            "operation": "find_definition",
            "name": "greet"
        }),
        ctx,
    ).unwrap();

    assert!(!output.is_error, "find_definition should succeed: {}", output.output);
    assert!(output.output.contains("greet"), "should find greet function");
}

#[test]
fn symbols_tool_find_scope() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    // main.rs line 2 is inside fn main
    let output = registry.execute(
        ToolName::Symbols,
        json!({
            "path": root.join("src/main.rs").to_string_lossy().to_string(),
            "operation": "find_scope",
            "line": 2
        }),
        ctx,
    ).unwrap();

    assert!(!output.is_error, "find_scope should succeed: {}", output.output);
    assert!(output.output.contains("main"), "should find main scope");
}
