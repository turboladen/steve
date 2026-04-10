//! Grep tool — regex-based content search using the ripgrep ecosystem.

use serde_json::Value;

use grep::{
    regex::RegexMatcher,
    searcher::{Searcher, sinks::UTF8},
};
use ignore::WalkBuilder;

use super::{ToolContext, ToolDef, ToolEntry, ToolName, ToolOutput};

pub fn tool() -> ToolEntry {
    ToolEntry {
        def: ToolDef {
            name: ToolName::Grep,
            description: "Search for a text/regex pattern across files. Returns matching lines \
                with file paths and line numbers. For finding where a symbol is defined or \
                finding all usages, use `lsp` instead — it gives exact results without false \
                positives."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regex pattern to search for"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search in (relative to project root). Defaults to project root."
                    },
                    "include": {
                        "type": "string",
                        "description": "Glob pattern to filter files (e.g., '*.rs', '*.ts')"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of results to return. Defaults to 50."
                    }
                },
                "required": ["pattern"]
            }),
        },
        handler: Box::new(execute),
    }
}

fn execute(args: Value, ctx: ToolContext) -> anyhow::Result<ToolOutput> {
    let pattern = args
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

    let include = args.get("include").and_then(|v| v.as_str());

    let max_results = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(50);

    let matcher = match RegexMatcher::new(pattern) {
        Ok(m) => m,
        Err(e) => {
            return Ok(ToolOutput {
                title: format!("grep '{pattern}'"),
                output: format!("Error: invalid regex pattern: {e}"),
                is_error: true,
            });
        }
    };

    let mut results: Vec<String> = Vec::new();
    let mut walker_builder = WalkBuilder::new(&search_path);
    walker_builder.hidden(true); // skip hidden files
    walker_builder.git_ignore(true); // respect .gitignore

    // Add file type filter if specified
    if let Some(glob_pattern) = include {
        let mut types_builder = ignore::types::TypesBuilder::new();
        types_builder.add("custom", glob_pattern).ok();
        types_builder.select("custom");
        if let Ok(types) = types_builder.build() {
            walker_builder.types(types);
        }
    }

    let walker = walker_builder.build();

    for entry in walker {
        if results.len() >= max_results {
            break;
        }

        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Skip directories
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        let file_path = entry.path();
        let mut searcher = Searcher::new();

        let result = searcher.search_path(
            &matcher,
            file_path,
            UTF8(|line_num, line| {
                if results.len() >= max_results {
                    return Ok(false); // Stop searching
                }

                let relative = file_path
                    .strip_prefix(&ctx.project_root)
                    .unwrap_or(file_path);

                let trimmed = line.trim_end();
                let display_line = if trimmed.len() > 200 {
                    let end = trimmed.floor_char_boundary(197);
                    format!("{}...", &trimmed[..end])
                } else {
                    trimmed.to_string()
                };
                results.push(format!(
                    "{}:{}: {}",
                    relative.display(),
                    line_num,
                    display_line
                ));
                Ok(true)
            }),
        );

        // Silently skip files that can't be searched (binary files, etc.)
        if result.is_err() {
            continue;
        }
    }

    let output = if results.is_empty() {
        format!("No matches found for pattern: {pattern}")
    } else {
        let count = results.len();
        let truncated = if count >= max_results {
            format!("\n\n(showing first {max_results} results)")
        } else {
            String::new()
        };
        format!("{}{truncated}", results.join("\n"))
    };

    Ok(ToolOutput {
        title: format!("grep '{pattern}'"),
        output,
        is_error: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            project_root: dir.to_path_buf(),
            storage_dir: None,
            task_store: None,
            lsp_manager: None,
        }
    }

    #[test]
    fn grep_truncates_long_match_lines() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("long.txt");
        // Create a file with a very long line (300+ chars)
        let long_line = format!("MATCH{}", "x".repeat(300));
        std::fs::write(&file, &long_line).unwrap();

        let args = serde_json::json!({"pattern": "MATCH", "path": file.to_str().unwrap()});
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        // The match line should be truncated with "..."
        assert!(result.output.contains("..."));
        // Should not contain the full 300 x's
        assert!(result.output.len() < 300);
    }

    #[test]
    fn grep_short_lines_not_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("short.txt");
        std::fs::write(&file, "hello world\n").unwrap();

        let args = serde_json::json!({"pattern": "hello", "path": file.to_str().unwrap()});
        let ctx = test_ctx(dir.path());
        let result = execute(args, ctx).unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("hello world"));
        assert!(!result.output.contains("..."));
    }
}
