//! Glob tool — find files matching a glob pattern.

use serde_json::Value;

use ignore::WalkBuilder;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

pub fn tool() -> ToolEntry {
    ToolEntry {
        def: ToolDef {
            name: ToolName::Glob,
            description: "Find files matching a glob pattern in the project. Returns relative paths. Respects .gitignore.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match files (e.g., '**/*.rs', 'src/**/*.ts')"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory to search in (relative to project root). Defaults to project root."
                    }
                },
                "required": ["pattern"]
            }),
        },
        handler: Box::new(|args, ctx| execute(args, ctx)),
    }
}

fn execute(args: Value, ctx: ToolContext) -> anyhow::Result<ToolOutput> {
    let pattern_str = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing 'pattern' argument"))?;

    let search_path = args
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

    let glob_pattern = match ::glob::Pattern::new(pattern_str) {
        Ok(p) => p,
        Err(e) => {
            return Ok(ToolOutput {
                title: format!("glob '{pattern_str}'"),
                output: format!("Error: invalid glob pattern: {e}"),
                is_error: true,
            });
        }
    };

    let walker = WalkBuilder::new(&search_path)
        .hidden(true)
        .git_ignore(true)
        .build();

    let mut matches: Vec<String> = Vec::new();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Skip directories
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        let file_path = entry.path();
        let relative = file_path
            .strip_prefix(&ctx.project_root)
            .unwrap_or(file_path);

        let relative_str = relative.to_string_lossy();

        if glob_pattern.matches(&relative_str) {
            matches.push(relative_str.to_string());
        }

        if matches.len() >= 200 {
            break;
        }
    }

    matches.sort();

    let output = if matches.is_empty() {
        format!("No files found matching: {pattern_str}")
    } else {
        let count = matches.len();
        let truncated = if count >= 200 {
            "\n\n(showing first 200 results)".to_string()
        } else {
            String::new()
        };
        format!("{}{truncated}", matches.join("\n"))
    };

    Ok(ToolOutput {
        title: format!("glob '{pattern_str}'"),
        output,
        is_error: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::tempdir;

    fn make_ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            project_root: dir.to_path_buf(),
            storage_dir: None,
        }
    }

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
    fn matches_rs_files() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        fs::write(dir.path().join("test.rs"), "fn main() {}").unwrap();
        fs::write(dir.path().join("test.txt"), "hello").unwrap();

        let args = json!({ "pattern": "*.rs" });
        let result = execute(args, make_ctx(dir.path())).unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("test.rs"), "output should contain test.rs: {}", result.output);
        assert!(!result.output.contains("test.txt"), "output should not contain test.txt: {}", result.output);
    }

    #[test]
    fn results_are_sorted() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        fs::write(dir.path().join("c.rs"), "").unwrap();
        fs::write(dir.path().join("a.rs"), "").unwrap();
        fs::write(dir.path().join("b.rs"), "").unwrap();

        let args = json!({ "pattern": "*.rs" });
        let result = execute(args, make_ctx(dir.path())).unwrap();
        assert!(!result.is_error);
        let lines: Vec<&str> = result.output.lines().collect();
        assert_eq!(lines, vec!["a.rs", "b.rs", "c.rs"]);
    }

    #[test]
    fn invalid_pattern_returns_error() {
        let dir = tempdir().unwrap();
        let args = json!({ "pattern": "[invalid" });
        let result = execute(args, make_ctx(dir.path())).unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("invalid glob pattern"), "output: {}", result.output);
    }

    #[test]
    fn no_matches_descriptive_message() {
        let dir = tempdir().unwrap();
        init_git(dir.path());

        let args = json!({ "pattern": "*.xyz" });
        let result = execute(args, make_ctx(dir.path())).unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("No files found"), "output: {}", result.output);
    }

    #[test]
    fn path_parameter_scopes_search() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        fs::create_dir_all(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("root.rs"), "").unwrap();
        fs::write(dir.path().join("sub/nested.rs"), "").unwrap();

        let args = json!({ "pattern": "*.rs", "path": "sub" });
        let result = execute(args, make_ctx(dir.path())).unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("nested.rs"), "output should contain nested.rs: {}", result.output);
        assert!(!result.output.contains("root.rs"), "output should not contain root.rs: {}", result.output);
    }
}
