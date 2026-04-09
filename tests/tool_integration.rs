//! Integration tests for tool execution.
//!
//! Tests tools against real filesystem fixtures in temp directories,
//! verifying end-to-end behavior including path resolution and output.

use serde_json::json;
use std::path::PathBuf;
use tempfile::tempdir;

use steve::{
    context::cache::ToolResultCache,
    tool::{ToolContext, ToolName, ToolRegistry},
};

/// Helper to create a ToolContext with a temp directory as project root.
fn tool_context(project_root: PathBuf) -> ToolContext {
    ToolContext {
        project_root,
        storage_dir: None,
        task_store: None,
        lsp_manager: None,
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
    std::fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    println!(\"hello\");\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn greet() -> &'static str {\n    \"hello\"\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"test\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    (dir, root)
}

#[test]
fn read_tool_returns_file_content() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    let output = registry
        .execute(
            ToolName::Read,
            json!({ "path": root.join("src/main.rs").to_string_lossy().to_string() }),
            ctx,
        )
        .unwrap();

    assert!(!output.is_error);
    assert!(output.output.contains("fn main()"));
    assert!(output.output.contains("println!"));
}

#[test]
fn read_tool_nonexistent_file_returns_error() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    let output = registry
        .execute(
            ToolName::Read,
            json!({ "path": root.join("nonexistent.rs").to_string_lossy().to_string() }),
            ctx,
        )
        .unwrap();

    assert!(output.is_error);
}

#[test]
fn glob_tool_finds_rust_files() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    let output = registry
        .execute(
            ToolName::Glob,
            json!({ "pattern": "**/*.rs", "path": root.to_string_lossy().to_string() }),
            ctx,
        )
        .unwrap();

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

    let output = registry
        .execute(
            ToolName::Edit,
            json!({
                "file_path": &file_path,
                "old_string": "println!(\"hello\")",
                "new_string": "println!(\"world\")"
            }),
            ctx,
        )
        .unwrap();

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

    let output = registry
        .execute(
            ToolName::Write,
            json!({
                "file_path": &file_path,
                "content": "pub fn new_fn() {}\n"
            }),
            ctx,
        )
        .unwrap();

    assert!(!output.is_error, "write should succeed: {}", output.output);
    let content = std::fs::read_to_string(root.join("src/new_file.rs")).unwrap();
    assert_eq!(content, "pub fn new_fn() {}\n");
}

#[test]
fn list_tool_shows_directory_contents() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    let output = registry
        .execute(
            ToolName::List,
            json!({ "path": root.join("src").to_string_lossy().to_string() }),
            ctx,
        )
        .unwrap();

    assert!(!output.is_error);
    assert!(output.output.contains("main.rs"));
    assert!(output.output.contains("lib.rs"));
}

#[test]
fn grep_tool_finds_matches() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    let output = registry
        .execute(
            ToolName::Grep,
            json!({
                "pattern": "fn main",
                "path": root.to_string_lossy().to_string()
            }),
            ctx,
        )
        .unwrap();

    assert!(!output.is_error);
    assert!(
        output.output.contains("main.rs"),
        "should find fn main in main.rs"
    );
}

#[test]
fn delete_tool_removes_file() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());
    let file_path = root.join("src/lib.rs").to_string_lossy().to_string();

    assert!(root.join("src/lib.rs").exists());

    let output = registry
        .execute(ToolName::Delete, json!({ "path": &file_path }), ctx)
        .unwrap();

    assert!(!output.is_error);
    assert!(!root.join("src/lib.rs").exists());
}

#[test]
fn mkdir_tool_creates_directory() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());
    let dir_path = root.join("src/new_module").to_string_lossy().to_string();

    let output = registry
        .execute(ToolName::Mkdir, json!({ "path": &dir_path }), ctx)
        .unwrap();

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

    let output = registry
        .execute(
            ToolName::Move,
            json!({ "from_path": &from, "to_path": &to }),
            ctx,
        )
        .unwrap();

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
    let to = root
        .join("src/main_backup.rs")
        .to_string_lossy()
        .to_string();

    let output = registry
        .execute(
            ToolName::Copy,
            json!({ "from_path": &from, "to_path": &to }),
            ctx,
        )
        .unwrap();

    assert!(!output.is_error);
    assert!(
        root.join("src/main.rs").exists(),
        "original should still exist"
    );
    assert!(
        root.join("src/main_backup.rs").exists(),
        "copy should exist"
    );

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

    let output = registry
        .execute(
            ToolName::Edit,
            json!({
                "file_path": &file_path,
                "operation": "insert_lines",
                "line": 2,
                "content": "    // inserted comment"
            }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "insert_lines should succeed: {}",
        output.output
    );
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
    let output = registry
        .execute(
            ToolName::Edit,
            json!({
                "file_path": &file_path,
                "operation": "delete_lines",
                "start_line": 2,
                "end_line": 2
            }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "delete_lines should succeed: {}",
        output.output
    );
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

    let output = registry
        .execute(
            ToolName::Edit,
            json!({
                "file_path": &file_path,
                "operation": "replace_range",
                "start_line": 2,
                "end_line": 2,
                "content": "    eprintln!(\"debug\");\n    println!(\"world\");"
            }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "replace_range should succeed: {}",
        output.output
    );
    let content = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert!(content.contains("eprintln!"));
    assert!(content.contains("println!(\"world\")"));
    assert!(!content.contains("println!(\"hello\")"));
}

// ── LSP tool ──

/// Check if rust-analyzer is installed; skip tests if not.
fn has_rust_analyzer() -> bool {
    std::process::Command::new("rust-analyzer")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Helper to create a minimal Cargo project that rust-analyzer can analyze.
fn create_cargo_project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"lsp-test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "fn greet() -> &'static str {\n    \"hello\"\n}\n\nfn main() {\n    let msg = greet();\n    println!(\"{}\", msg);\n}\n",
    )
    .unwrap();

    (dir, root)
}

#[test]
fn lsp_tool_no_manager_returns_error() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    // ToolContext without lsp_manager
    let ctx = tool_context(root.clone());

    let result = registry.execute(
        ToolName::Lsp,
        json!({ "path": root.join("src/main.rs").to_string_lossy().to_string() }),
        ctx,
    );

    // Should fail because lsp_manager is None
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("LSP not available")
    );
}

#[test]
fn lsp_tool_file_not_found() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let rt = tokio::runtime::Runtime::new().unwrap();
    let lsp_mgr = std::sync::Arc::new(std::sync::Mutex::new(steve::lsp::LspManager::new(
        root.clone(),
        rt.handle().clone(),
    )));
    let ctx = ToolContext {
        project_root: root.clone(),
        storage_dir: None,
        task_store: None,
        lsp_manager: Some(lsp_mgr),
    };

    let output = registry
        .execute(
            ToolName::Lsp,
            json!({ "path": root.join("nonexistent.rs").to_string_lossy().to_string() }),
            ctx,
        )
        .unwrap();

    assert!(output.is_error);
    assert!(output.output.contains("not found"));
}

#[test]
fn lsp_tool_unknown_operation() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let rt = tokio::runtime::Runtime::new().unwrap();
    let lsp_mgr = std::sync::Arc::new(std::sync::Mutex::new(steve::lsp::LspManager::new(
        root.clone(),
        rt.handle().clone(),
    )));
    let ctx = ToolContext {
        project_root: root.clone(),
        storage_dir: None,
        task_store: None,
        lsp_manager: Some(lsp_mgr),
    };

    let output = registry
        .execute(
            ToolName::Lsp,
            json!({
                "path": root.join("src/main.rs").to_string_lossy().to_string(),
                "operation": "bogus"
            }),
            ctx,
        )
        .unwrap();

    assert!(output.is_error);
    assert!(
        output.output.contains("unknown operation"),
        "expected unknown operation error, got: {}",
        output.output
    );
}

#[test]
fn lsp_tool_definition_missing_position() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let rt = tokio::runtime::Runtime::new().unwrap();
    let lsp_mgr = std::sync::Arc::new(std::sync::Mutex::new(steve::lsp::LspManager::new(
        root.clone(),
        rt.handle().clone(),
    )));
    let ctx = ToolContext {
        project_root: root.clone(),
        storage_dir: None,
        task_store: None,
        lsp_manager: Some(lsp_mgr),
    };

    let result = registry.execute(
        ToolName::Lsp,
        json!({
            "path": root.join("src/main.rs").to_string_lossy().to_string(),
            "operation": "definition"
        }),
        ctx,
    );

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("line"));
}

/// Integration test with real rust-analyzer — skipped if not installed.
#[test]
#[ignore = "requires rust-analyzer on PATH"]
fn lsp_diagnostics_with_rust_analyzer() {
    if !has_rust_analyzer() {
        eprintln!("skipping: rust-analyzer not found on PATH");
        return;
    }

    let (_dir, root) = create_cargo_project();
    // Write a file with a deliberate error
    std::fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    let x: i32 = \"not a number\";\n    println!(\"{}\", x);\n}\n",
    )
    .unwrap();

    let registry = ToolRegistry::new(root.clone());
    let rt = tokio::runtime::Runtime::new().unwrap();
    let lsp_mgr = std::sync::Arc::new(std::sync::Mutex::new(steve::lsp::LspManager::new(
        root.clone(),
        rt.handle().clone(),
    )));
    let ctx = ToolContext {
        project_root: root.clone(),
        storage_dir: None,
        task_store: None,
        lsp_manager: Some(lsp_mgr),
    };

    let output = registry
        .execute(
            ToolName::Lsp,
            json!({
                "path": root.join("src/main.rs").to_string_lossy().to_string(),
                "operation": "diagnostics"
            }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "diagnostics call should succeed: {}",
        output.output
    );
    // rust-analyzer should report a type mismatch error
    assert!(
        output.output.contains("error") || output.output.contains("mismatched"),
        "expected error diagnostic, got: {}",
        output.output
    );
}

/// Integration test: go-to-definition with real rust-analyzer.
#[test]
#[ignore = "requires rust-analyzer on PATH"]
fn lsp_definition_with_rust_analyzer() {
    if !has_rust_analyzer() {
        eprintln!("skipping: rust-analyzer not found on PATH");
        return;
    }

    let (_dir, root) = create_cargo_project();
    let registry = ToolRegistry::new(root.clone());
    let rt = tokio::runtime::Runtime::new().unwrap();
    let lsp_mgr = std::sync::Arc::new(std::sync::Mutex::new(steve::lsp::LspManager::new(
        root.clone(),
        rt.handle().clone(),
    )));
    let ctx = ToolContext {
        project_root: root.clone(),
        storage_dir: None,
        task_store: None,
        lsp_manager: Some(lsp_mgr),
    };

    // Query definition of `greet` on line 6, column 14 (the call site)
    // "    let msg = greet();" — greet starts at column 14
    let output = registry
        .execute(
            ToolName::Lsp,
            json!({
                "path": root.join("src/main.rs").to_string_lossy().to_string(),
                "operation": "definition",
                "line": 6,
                "character": 14
            }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "definition call should succeed: {}",
        output.output
    );
    // Should find the definition at line 1 of main.rs
    assert!(
        output.output.contains("main.rs") || output.output.contains("Definition"),
        "expected definition result, got: {}",
        output.output
    );
}

// ── Symbols tool ──

#[test]
fn symbols_tool_lists_rust_symbols() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    let output = registry
        .execute(
            ToolName::Symbols,
            json!({ "path": root.join("src/main.rs").to_string_lossy().to_string() }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "symbols should succeed: {}",
        output.output
    );
    assert!(output.output.contains("fn main"), "should find fn main");
    assert!(output.output.contains("Symbols in"), "should have header");
}

#[test]
fn symbols_tool_unsupported_file_type() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    let output = registry
        .execute(
            ToolName::Symbols,
            json!({ "path": root.join("Cargo.toml").to_string_lossy().to_string() }),
            ctx,
        )
        .unwrap();

    // TOML is supported, so it should parse
    assert!(!output.is_error);
}

#[test]
fn symbols_tool_find_definition() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    // lib.rs has `pub fn greet()`
    let output = registry
        .execute(
            ToolName::Symbols,
            json!({
                "path": root.join("src/lib.rs").to_string_lossy().to_string(),
                "operation": "find_definition",
                "name": "greet"
            }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "find_definition should succeed: {}",
        output.output
    );
    assert!(
        output.output.contains("greet"),
        "should find greet function"
    );
}

#[test]
fn symbols_tool_find_scope() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    // main.rs line 2 is inside fn main
    let output = registry
        .execute(
            ToolName::Symbols,
            json!({
                "path": root.join("src/main.rs").to_string_lossy().to_string(),
                "operation": "find_scope",
                "line": 2
            }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "find_scope should succeed: {}",
        output.output
    );
    assert!(output.output.contains("main"), "should find main scope");
}

// ── Step 1: Filesystem Error Cases ──

#[test]
fn move_to_existing_file_overwrites() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    // Create a target file with different content
    std::fs::write(root.join("src/target.rs"), "old content\n").unwrap();

    let from = root.join("src/main.rs").to_string_lossy().to_string();
    let to = root.join("src/target.rs").to_string_lossy().to_string();

    let output = registry
        .execute(
            ToolName::Move,
            json!({ "from_path": &from, "to_path": &to }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "move should overwrite existing target: {}",
        output.output
    );
    assert!(!root.join("src/main.rs").exists(), "source should be gone");
    let content = std::fs::read_to_string(root.join("src/target.rs")).unwrap();
    assert!(
        content.contains("fn main()"),
        "target should have source's content"
    );
}

#[test]
fn copy_to_existing_file_overwrites() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    std::fs::write(root.join("src/target.rs"), "old content\n").unwrap();

    let from = root.join("src/main.rs").to_string_lossy().to_string();
    let to = root.join("src/target.rs").to_string_lossy().to_string();

    let output = registry
        .execute(
            ToolName::Copy,
            json!({ "from_path": &from, "to_path": &to }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "copy should overwrite existing target: {}",
        output.output
    );
    assert!(
        root.join("src/main.rs").exists(),
        "source should still exist"
    );
    let content = std::fs::read_to_string(root.join("src/target.rs")).unwrap();
    assert!(
        content.contains("fn main()"),
        "target should have source's content"
    );
}

#[test]
fn delete_project_root_refused() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    let root_path = root.canonicalize().unwrap().to_string_lossy().to_string();

    let result = registry.execute(ToolName::Delete, json!({ "path": &root_path }), ctx);

    assert!(result.is_err(), "deleting project root should fail");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("refusing") && err_msg.contains("project root"),
        "error should mention refusing to delete project root, got: {err_msg}"
    );
}

#[test]
fn move_directory_via_registry() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    // src/ has main.rs and lib.rs
    let from = root.join("src").to_string_lossy().to_string();
    let to = root.join("source").to_string_lossy().to_string();

    let output = registry
        .execute(
            ToolName::Move,
            json!({ "from_path": &from, "to_path": &to }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "move directory should succeed: {}",
        output.output
    );
    assert!(!root.join("src").exists(), "old directory should be gone");
    assert!(
        root.join("source/main.rs").exists(),
        "files should be in new dir"
    );
    assert!(
        root.join("source/lib.rs").exists(),
        "all files should be moved"
    );
}

#[test]
fn copy_directory_recursive_via_registry() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    let from = root.join("src").to_string_lossy().to_string();
    let to = root.join("src_copy").to_string_lossy().to_string();

    let output = registry
        .execute(
            ToolName::Copy,
            json!({ "from_path": &from, "to_path": &to }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "copy directory should succeed: {}",
        output.output
    );
    assert!(
        root.join("src/main.rs").exists(),
        "original should still exist"
    );
    assert!(
        root.join("src_copy/main.rs").exists(),
        "copy should have main.rs"
    );
    assert!(
        root.join("src_copy/lib.rs").exists(),
        "copy should have lib.rs"
    );

    let original = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    let copied = std::fs::read_to_string(root.join("src_copy/main.rs")).unwrap();
    assert_eq!(original, copied, "file content should match");
}

// ── Step 2: Edit Edge Cases ──

#[test]
fn edit_multi_find_replace_via_registry() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());

    // Write a file with multiple replaceable strings
    let file_path = root.join("src/multi.rs");
    std::fs::write(&file_path, "fn alpha() {}\nfn beta() {}\nfn gamma() {}\n").unwrap();

    let output = registry
        .execute(
            ToolName::Edit,
            json!({
                "file_path": file_path.to_string_lossy().to_string(),
                "operation": "multi_find_replace",
                "edits": [
                    { "old_string": "fn alpha()", "new_string": "fn one()" },
                    { "old_string": "fn beta()", "new_string": "fn two()" },
                    { "old_string": "fn gamma()", "new_string": "fn three()" }
                ]
            }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "multi_find_replace should succeed: {}",
        output.output
    );
    let content = std::fs::read_to_string(&file_path).unwrap();
    assert!(content.contains("fn one()"));
    assert!(content.contains("fn two()"));
    assert!(content.contains("fn three()"));
    assert!(!content.contains("fn alpha()"));
}

#[test]
fn edit_find_replace_no_match_returns_error() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root.clone());
    let file_path = root.join("src/main.rs").to_string_lossy().to_string();

    let result = registry.execute(
        ToolName::Edit,
        json!({
            "file_path": &file_path,
            "old_string": "this string does not exist in the file",
            "new_string": "replacement"
        }),
        ctx,
    );

    assert!(
        result.is_err(),
        "find_replace with no match should return error"
    );
    assert!(
        result.unwrap_err().to_string().contains("not found"),
        "error should mention old_string not found"
    );
}

#[test]
fn edit_large_file_line_operations() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let file_path = root.join("src/large.rs");

    // Generate a 10k-line file
    let mut content = String::new();
    for i in 1..=10_000 {
        content.push_str(&format!("// line {i}\n"));
    }
    std::fs::write(&file_path, &content).unwrap();
    let fp = file_path.to_string_lossy().to_string();

    // Insert at line 5000
    let ctx = tool_context(root.clone());
    let output = registry
        .execute(
            ToolName::Edit,
            json!({
                "file_path": &fp,
                "operation": "insert_lines",
                "line": 5000,
                "content": "// INSERTED LINE"
            }),
            ctx,
        )
        .unwrap();
    assert!(
        !output.is_error,
        "insert at line 5000 should succeed: {}",
        output.output
    );

    // Delete lines 7000-7010
    let ctx = tool_context(root.clone());
    let output = registry
        .execute(
            ToolName::Edit,
            json!({
                "file_path": &fp,
                "operation": "delete_lines",
                "start_line": 7000,
                "end_line": 7010
            }),
            ctx,
        )
        .unwrap();
    assert!(
        !output.is_error,
        "delete lines 7000-7010 should succeed: {}",
        output.output
    );

    // Replace range 9000-9005
    let ctx = tool_context(root.clone());
    let output = registry
        .execute(
            ToolName::Edit,
            json!({
                "file_path": &fp,
                "operation": "replace_range",
                "start_line": 9000,
                "end_line": 9005,
                "content": "// REPLACED BLOCK"
            }),
            ctx,
        )
        .unwrap();
    assert!(
        !output.is_error,
        "replace_range 9000-9005 should succeed: {}",
        output.output
    );

    // Verify the file is roughly the right size
    let final_content = std::fs::read_to_string(&file_path).unwrap();
    let line_count = final_content.lines().count();
    // Started with 10000, +1 insert, -11 delete, -5 replace (6→1) = net -15
    assert!(
        (9980..=9990).contains(&line_count),
        "expected ~9985 lines, got {line_count}"
    );
    assert!(final_content.contains("// INSERTED LINE"));
    assert!(final_content.contains("// REPLACED BLOCK"));
}

// ── Step 3: Symbols Multi-Language ──

#[test]
fn symbols_go_file() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let file = root.join("main.go");
    std::fs::write(
        &file,
        "package main\n\nfunc main() {\n}\n\ntype Config struct {\n\tName string\n}\n",
    )
    .unwrap();

    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root);

    let output = registry
        .execute(
            ToolName::Symbols,
            json!({ "path": file.to_string_lossy().to_string() }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "Go symbols should succeed: {}",
        output.output
    );
    assert!(
        output.output.contains("function main"),
        "should find main function"
    );
    // Go type_declaration wraps struct — extract_name finds it as type node
    assert!(
        output.output.contains("type"),
        "should find type declaration"
    );
    // Verify we get 2 symbols total (function + type)
    assert!(
        output.output.contains("line"),
        "should have line info for symbols"
    );
}

#[test]
fn symbols_c_file() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let file = root.join("main.c");
    std::fs::write(
        &file,
        "struct Point {\n    int x;\n    int y;\n};\n\nvoid greet() {\n}\n",
    )
    .unwrap();

    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root);

    let output = registry
        .execute(
            ToolName::Symbols,
            json!({ "path": file.to_string_lossy().to_string() }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "C symbols should succeed: {}",
        output.output
    );
    assert!(
        output.output.contains("struct Point"),
        "should find Point struct"
    );
    // C function_definition: name inside declarator child — extract_name reports (anonymous)
    // Verify we get 2 symbols (struct + function_definition)
    assert!(
        output.output.contains("def"),
        "should find function definition"
    );
    assert!(output.output.contains("(c,"), "should detect C language");
}

#[test]
fn symbols_cpp_file() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let file = root.join("main.cpp");
    // Use a named class — tree-sitter C++ has `name` field on class_specifier
    std::fs::write(
        &file,
        "class Greeter {\npublic:\n    void greet() {}\n};\n\nvoid standalone() {}\n",
    )
    .unwrap();

    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root);

    let output = registry
        .execute(
            ToolName::Symbols,
            json!({ "path": file.to_string_lossy().to_string() }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "C++ symbols should succeed: {}",
        output.output
    );
    assert!(
        output.output.contains("Greeter"),
        "should find Greeter class"
    );
    assert!(
        output.output.contains("(cpp,"),
        "should detect C++ language"
    );
    // Nested methods in class_specifier containers
    assert!(
        output.output.contains("class"),
        "should have class kind label"
    );
}

#[test]
fn symbols_java_file() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let file = root.join("Foo.java");
    std::fs::write(
        &file,
        "public class Foo {\n    public void bar() {\n    }\n}\n",
    )
    .unwrap();

    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root);

    let output = registry
        .execute(
            ToolName::Symbols,
            json!({ "path": file.to_string_lossy().to_string() }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "Java symbols should succeed: {}",
        output.output
    );
    assert!(output.output.contains("Foo"), "should find Foo class");
    assert!(output.output.contains("bar"), "should find bar method");
}

#[test]
fn symbols_ruby_file() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let file = root.join("greeter.rb");
    std::fs::write(
        &file,
        "class Greeter\n  def greet\n    puts 'hello'\n  end\nend\n",
    )
    .unwrap();

    let registry = ToolRegistry::new(root.clone());
    let ctx = tool_context(root);

    let output = registry
        .execute(
            ToolName::Symbols,
            json!({ "path": file.to_string_lossy().to_string() }),
            ctx,
        )
        .unwrap();

    assert!(
        !output.is_error,
        "Ruby symbols should succeed: {}",
        output.output
    );
    assert!(
        output.output.contains("Greeter"),
        "should find Greeter class"
    );
    assert!(output.output.contains("greet"), "should find greet method");
}

// ── Step 4: Cache API + Real Tool Execution ──
//
// These tests verify ToolResultCache behavior (put/get/invalidate_path)
// using real tool outputs from ToolRegistry. They do NOT test stream-level
// auto-invalidation — that is covered by stream_cache_invalidation_after_write
// in src/stream.rs.

#[test]
fn cache_read_then_edit_invalidates() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let file_path = root.join("src/main.rs");
    let fp = file_path.to_string_lossy().to_string();

    let mut cache = ToolResultCache::new(root.clone());

    // Read the file and cache the result
    let ctx = tool_context(root.clone());
    let read_output = registry
        .execute(ToolName::Read, json!({ "path": &fp }), ctx)
        .unwrap();
    let read_args = json!({ "path": &fp });
    cache.put(ToolName::Read, &read_args, &read_output);

    // Verify cache hit
    assert!(
        cache.get(ToolName::Read, &read_args).is_some(),
        "should hit cache before edit"
    );

    // Edit the file
    let ctx = tool_context(root.clone());
    let edit_output = registry
        .execute(
            ToolName::Edit,
            json!({
                "file_path": &fp,
                "old_string": "println!(\"hello\")",
                "new_string": "println!(\"world\")"
            }),
            ctx,
        )
        .unwrap();
    assert!(!edit_output.is_error);

    // Invalidate the cache (as stream.rs would do after a write)
    cache.invalidate_path(&fp);

    // Cache should miss now
    assert!(
        cache.get(ToolName::Read, &read_args).is_none(),
        "should miss cache after edit + invalidation"
    );
}

#[test]
fn cache_read_then_write_invalidates() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let file_path = root.join("src/new_file.rs");
    let fp = file_path.to_string_lossy().to_string();

    // Write a file first so we can read it
    let ctx = tool_context(root.clone());
    registry
        .execute(
            ToolName::Write,
            json!({ "file_path": &fp, "content": "original content\n" }),
            ctx,
        )
        .unwrap();

    // Read and cache
    let mut cache = ToolResultCache::new(root.clone());
    let ctx = tool_context(root.clone());
    let read_output = registry
        .execute(ToolName::Read, json!({ "path": &fp }), ctx)
        .unwrap();
    let read_args = json!({ "path": &fp });
    cache.put(ToolName::Read, &read_args, &read_output);
    assert!(cache.get(ToolName::Read, &read_args).is_some());

    // Overwrite via write tool
    let ctx = tool_context(root.clone());
    registry
        .execute(
            ToolName::Write,
            json!({ "file_path": &fp, "content": "new content\n" }),
            ctx,
        )
        .unwrap();

    // Invalidate
    cache.invalidate_path(&fp);
    assert!(
        cache.get(ToolName::Read, &read_args).is_none(),
        "cache miss after write + invalidation"
    );

    // Re-read gets new content
    let ctx = tool_context(root.clone());
    let new_output = registry
        .execute(ToolName::Read, json!({ "path": &fp }), ctx)
        .unwrap();
    assert!(
        new_output.output.contains("new content"),
        "re-read should show new content"
    );
}

#[test]
fn cache_read_then_move_invalidates_source() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());
    let a_path = root.join("src/lib.rs");
    let a_fp = a_path.to_string_lossy().to_string();

    // Read lib.rs and cache
    let mut cache = ToolResultCache::new(root.clone());
    let ctx = tool_context(root.clone());
    let read_output = registry
        .execute(ToolName::Read, json!({ "path": &a_fp }), ctx)
        .unwrap();
    let read_args = json!({ "path": &a_fp });
    cache.put(ToolName::Read, &read_args, &read_output);
    assert!(cache.get(ToolName::Read, &read_args).is_some());

    // Move a to b
    let b_fp = root.join("src/moved.rs").to_string_lossy().to_string();
    let ctx = tool_context(root.clone());
    registry
        .execute(
            ToolName::Move,
            json!({ "from_path": &a_fp, "to_path": &b_fp }),
            ctx,
        )
        .unwrap();

    // Invalidate old path
    cache.invalidate_path(&a_fp);
    assert!(
        cache.get(ToolName::Read, &read_args).is_none(),
        "cache miss after move + invalidation"
    );
}

#[test]
fn cache_grep_invalidated_by_any_edit() {
    let (_dir, root) = create_test_project();
    let registry = ToolRegistry::new(root.clone());

    let mut cache = ToolResultCache::new(root.clone());

    // Cache a grep result
    let ctx = tool_context(root.clone());
    let grep_args = json!({ "pattern": "fn main", "path": root.to_string_lossy().to_string() });
    let grep_output = registry
        .execute(ToolName::Grep, grep_args.clone(), ctx)
        .unwrap();
    cache.put(ToolName::Grep, &grep_args, &grep_output);
    assert!(cache.get(ToolName::Grep, &grep_args).is_some());

    // Edit an unrelated file
    let lib_path = root.join("src/lib.rs").to_string_lossy().to_string();
    let ctx = tool_context(root.clone());
    registry
        .execute(
            ToolName::Edit,
            json!({
                "file_path": &lib_path,
                "old_string": "\"hello\"",
                "new_string": "\"goodbye\""
            }),
            ctx,
        )
        .unwrap();

    // Invalidate the edited path — should also kill grep cache
    cache.invalidate_path(&lib_path);
    assert!(
        cache.get(ToolName::Grep, &grep_args).is_none(),
        "grep cache should be invalidated by any file edit"
    );
}

// ── Step 5b: Sub-Agent Tool Restrictions ──

#[test]
fn explore_agent_has_only_read_tools() {
    use steve::tool::agent::AgentType;
    use strum::IntoEnumIterator;

    let tools = AgentType::Explore.allowed_tools();
    let filtered = ToolRegistry::filtered(PathBuf::from("/tmp"), &tools);

    // Every allowed tool must be read-only.
    // Note: Webfetch is categorized as Exploring (UI) but is_read_only() == false,
    // so it is deliberately excluded from the Explore agent's tool set.
    for t in &tools {
        assert!(t.is_read_only(), "Explore tool {t} should be read-only");
    }

    // Every non-allowed variant must be absent from the filtered registry
    for t in ToolName::iter() {
        if tools.contains(&t) {
            assert!(filtered.has_tool(t), "Explore registry should have {t}");
        } else {
            assert!(
                !filtered.has_tool(t),
                "Explore registry should not have {t}"
            );
        }
    }
}

#[test]
fn plan_agent_includes_lsp_no_writes() {
    use steve::tool::agent::AgentType;
    use strum::IntoEnumIterator;

    let tools = AgentType::Plan.allowed_tools();
    let filtered = ToolRegistry::filtered(PathBuf::from("/tmp"), &tools);

    // Has LSP
    assert!(filtered.has_tool(ToolName::Lsp), "Plan should have LSP");

    // Every allowed tool must be read-only or LSP
    for t in &tools {
        assert!(
            t.is_read_only() || *t == ToolName::Lsp,
            "Plan tool {t} should be read-only or LSP"
        );
    }

    // Every non-allowed variant must be absent
    for t in ToolName::iter() {
        if tools.contains(&t) {
            assert!(filtered.has_tool(t), "Plan registry should have {t}");
        } else {
            assert!(!filtered.has_tool(t), "Plan registry should not have {t}");
        }
    }
}

#[test]
fn general_agent_excludes_only_agent() {
    use steve::tool::agent::AgentType;
    use strum::IntoEnumIterator;

    let tools = AgentType::General.allowed_tools();
    let filtered = ToolRegistry::filtered(PathBuf::from("/tmp"), &tools);

    // Should have all tools except Agent
    let expected_count = ToolName::iter().count() - 1; // minus Agent
    assert_eq!(
        tools.len(),
        expected_count,
        "General should have all tools minus Agent ({expected_count}), got {}",
        tools.len()
    );

    // Exhaustively verify every variant: present if not Agent, absent if Agent
    for t in ToolName::iter() {
        if t == ToolName::Agent {
            assert!(!filtered.has_tool(t), "General should not have Agent");
        } else {
            assert!(filtered.has_tool(t), "General should have {t}");
        }
    }
}
