//! List tool — directory listing, .gitignore-aware.

use serde_json::Value;

use ignore::WalkBuilder;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

pub fn tool() -> ToolEntry {
    ToolEntry {
        def: ToolDef {
            name: ToolName::List,
            description: "List files and directories at a given path. Respects .gitignore. Shows file types (file/dir) and sizes.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory to list (relative to project root). Defaults to project root."
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Maximum depth to recurse. Defaults to 1 (immediate children only)."
                    }
                },
                "required": []
            }),
        },
        handler: Box::new(execute),
    }
}

fn execute(args: Value, ctx: ToolContext) -> anyhow::Result<ToolOutput> {
    let list_path = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(|p| {
            if std::path::Path::new(p).is_absolute() {
                std::path::PathBuf::from(p)
            } else {
                ctx.project_root.join(p)
            }
        })
        .unwrap_or_else(|| ctx.project_root.clone());

    let depth = args
        .get("depth")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(1);

    if !list_path.exists() {
        return Ok(ToolOutput {
            title: "list".to_string(),
            output: format!("Error: path not found: {}", list_path.display()),
            is_error: true,
        });
    }

    let walker = WalkBuilder::new(&list_path)
        .max_depth(Some(depth + 1)) // +1 because the root itself is depth 0
        .hidden(true)
        .git_ignore(true)
        .sort_by_file_name(|a, b| a.cmp(b))
        .build();

    let mut entries: Vec<String> = Vec::new();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let file_path = entry.path();

        // Skip the root directory itself
        if file_path == list_path {
            continue;
        }

        let relative = file_path
            .strip_prefix(&ctx.project_root)
            .unwrap_or(file_path);

        let relative_str = relative.to_string_lossy();

        if let Some(ft) = entry.file_type() {
            if ft.is_dir() {
                entries.push(format!("{}/", relative_str));
            } else if ft.is_file() {
                // Show file size
                let size = entry
                    .metadata()
                    .ok()
                    .map(|m| format_size(m.len()))
                    .unwrap_or_default();
                entries.push(format!("{} {}", relative_str, size));
            }
        }

        if entries.len() >= 500 {
            entries.push("... (truncated)".to_string());
            break;
        }
    }

    let relative_display = list_path
        .strip_prefix(&ctx.project_root)
        .unwrap_or(&list_path)
        .display()
        .to_string();

    let display_path = if relative_display.is_empty() {
        ".".to_string()
    } else {
        relative_display
    };

    let output = if entries.is_empty() {
        format!("{display_path}/ (empty)")
    } else {
        entries.join("\n")
    };

    Ok(ToolOutput {
        title: format!("list {display_path}"),
        output,
        is_error: false,
    })
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("({bytes}B)")
    } else if bytes < 1024 * 1024 {
        format!("({:.1}KB)", bytes as f64 / 1024.0)
    } else {
        format!("({:.1}MB)", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::tempdir;

    /// Initialize a minimal git repo so the `ignore` crate's walker works
    /// without being confused by parent .gitignore files.
    fn init_git(dir: &std::path::Path) {
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(dir)
            .status()
            .expect("git init failed");
    }

    #[test]
    fn shows_files_with_sizes() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        fs::write(dir.path().join("small.txt"), "hi").unwrap();
        fs::write(dir.path().join("bigger.txt"), "a".repeat(2048)).unwrap();

        let args = json!({});
        let result = execute(
            args,
            crate::tool::tests::test_tool_context(dir.path().to_path_buf()),
        )
        .unwrap();
        assert!(!result.is_error);
        assert!(
            result.output.contains("small.txt"),
            "output should contain small.txt: {}",
            result.output
        );
        assert!(
            result.output.contains("bigger.txt"),
            "output should contain bigger.txt: {}",
            result.output
        );
        // small.txt is 2 bytes -> "(2B)", bigger.txt is 2048 bytes -> "(2.0KB)"
        assert!(
            result.output.contains("(2B)"),
            "output should contain size (2B): {}",
            result.output
        );
        assert!(
            result.output.contains("(2.0KB)"),
            "output should contain size (2.0KB): {}",
            result.output
        );
    }

    #[test]
    fn nonexistent_path_returns_error() {
        let dir = tempdir().unwrap();
        let args = json!({ "path": "does_not_exist" });
        let result = execute(
            args,
            crate::tool::tests::test_tool_context(dir.path().to_path_buf()),
        )
        .unwrap();
        assert!(result.is_error);
        assert!(
            result.output.contains("not found"),
            "output: {}",
            result.output
        );
    }

    #[test]
    fn empty_directory_shows_empty() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let sub = dir.path().join("empty_sub");
        fs::create_dir_all(&sub).unwrap();

        let args = json!({ "path": "empty_sub" });
        let result = execute(
            args,
            crate::tool::tests::test_tool_context(dir.path().to_path_buf()),
        )
        .unwrap();
        assert!(!result.is_error);
        assert!(
            result.output.contains("(empty)"),
            "output should indicate empty: {}",
            result.output
        );
    }

    #[test]
    fn depth_limits_nesting() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
        fs::write(dir.path().join("root.txt"), "root").unwrap();
        fs::write(dir.path().join("a/mid.txt"), "mid").unwrap();
        fs::write(dir.path().join("a/b/deep.txt"), "deep").unwrap();
        fs::write(dir.path().join("a/b/c/deeper.txt"), "deeper").unwrap();

        // Increasing depth should reveal progressively deeper files.
        // depth=1 shows immediate children + one level of nesting.
        let args1 = json!({ "depth": 1 });
        let result1 = execute(
            args1,
            crate::tool::tests::test_tool_context(dir.path().to_path_buf()),
        )
        .unwrap();
        assert!(!result1.is_error);
        assert!(
            result1.output.contains("root.txt"),
            "depth=1 should show root.txt: {}",
            result1.output
        );
        assert!(
            result1.output.contains("a/"),
            "depth=1 should show a/: {}",
            result1.output
        );
        // deeper.txt at 3 levels should not appear
        assert!(
            !result1.output.contains("deeper.txt"),
            "depth=1 should not show deeper.txt: {}",
            result1.output
        );

        // With a smaller depth, fewer entries should be shown.
        // depth=3 should reveal everything including deeper.txt.
        let args3 = json!({ "depth": 3 });
        let result3 = execute(
            args3,
            crate::tool::tests::test_tool_context(dir.path().to_path_buf()),
        )
        .unwrap();
        assert!(!result3.is_error);
        assert!(
            result3.output.contains("deeper.txt"),
            "depth=3 should show deeper.txt: {}",
            result3.output
        );
        assert!(
            result3.output.contains("root.txt"),
            "depth=3 should still show root.txt: {}",
            result3.output
        );

        // Verify that a lower depth produces fewer output lines than a higher depth.
        let lines1 = result1.output.lines().count();
        let lines3 = result3.output.lines().count();
        assert!(
            lines1 < lines3,
            "depth=1 ({lines1} lines) should have fewer entries than depth=3 ({lines3} lines)"
        );
    }

    #[test]
    fn format_size_helper() {
        assert_eq!(format_size(500), "(500B)");
        assert_eq!(format_size(0), "(0B)");
        assert_eq!(format_size(1023), "(1023B)");
        assert_eq!(format_size(1024), "(1.0KB)");
        assert_eq!(format_size(1536), "(1.5KB)");
        assert_eq!(format_size(1048576), "(1.0MB)");
        assert_eq!(format_size(2621440), "(2.5MB)");
    }
}
