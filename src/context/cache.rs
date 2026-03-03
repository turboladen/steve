//! Tool result caching for avoiding redundant operations within a session.
//!
//! Caches results of read-only tools (read, grep, glob, list) and automatically
//! invalidates entries when write operations (edit, write, patch) modify files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::tool::{ToolName, ToolOutput};

/// Cached result from a tool execution.
#[derive(Clone)]
struct CachedResult {
    /// The original full tool output.
    output: ToolOutput,
}

/// Cache for tool results within a session.
///
/// Session-scoped: lives in `App` behind `Arc<Mutex>`, shared across all
/// `spawn_stream` calls within a session. Reset on `/new`.
/// Automatically invalidated when write operations touch cached file paths.
pub struct ToolResultCache {
    /// Map from cache key to cached result.
    entries: HashMap<String, CachedResult>,
    /// Map from normalized file path to set of cache keys that reference it.
    /// Used for invalidation when a file is modified.
    path_index: HashMap<PathBuf, Vec<String>>,
    /// Project root for normalizing paths.
    project_root: PathBuf,
    /// Stats
    hits: u32,
    misses: u32,
}

impl ToolResultCache {
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            entries: HashMap::new(),
            path_index: HashMap::new(),
            project_root,
            hits: 0,
            misses: 0,
        }
    }

    /// Try to get a cached result. Returns the full cached output if available.
    ///
    /// Returns the original tool output (not a compact reference) so the LLM
    /// can use the content directly. The compressor handles token optimization
    /// separately — it's the right layer for deciding when to summarize.
    pub fn get(&mut self, tool_name: ToolName, args: &Value) -> Option<ToolOutput> {
        let key = self.cache_key(tool_name, args)?;

        if let Some(cached) = self.entries.get(&key) {
            self.hits += 1;
            tracing::info!(
                tool = %tool_name,
                key = %key,
                hits = self.hits,
                "cache hit"
            );

            Some(cached.output.clone())
        } else {
            self.misses += 1;
            None
        }
    }

    /// Store a tool result in the cache.
    pub fn put(
        &mut self,
        tool_name: ToolName,
        args: &Value,
        output: &ToolOutput,
    ) {
        // Don't cache errors
        if output.is_error {
            return;
        }

        let Some(key) = self.cache_key(tool_name, args) else {
            return;
        };

        // Track which file paths this cache entry references (for invalidation)
        if let Some(path) = self.extract_path(tool_name, args) {
            self.path_index
                .entry(path)
                .or_default()
                .push(key.clone());
        }

        self.entries.insert(
            key,
            CachedResult {
                output: output.clone(),
            },
        );
    }

    /// Invalidate all cache entries that reference the given file path.
    /// Also invalidates all grep and glob entries since they may have matched
    /// content in the modified file.
    /// Call this after edit/write/patch operations.
    pub fn invalidate_path(&mut self, path: &str) {
        let normalized = self.normalize_path(path);
        let mut invalidated = 0;

        // Invalidate exact path matches (read, list)
        if let Some(keys) = self.path_index.remove(&normalized) {
            invalidated += keys.len();
            for key in keys {
                self.entries.remove(&key);
            }
        }

        // Invalidate all grep and glob entries — they may reference the
        // modified file and we can't cheaply determine which ones do.
        let grep_glob_keys: Vec<String> = self
            .entries
            .keys()
            .filter(|k| k.starts_with("grep:") || k.starts_with("glob:"))
            .cloned()
            .collect();
        invalidated += grep_glob_keys.len();
        for key in grep_glob_keys {
            self.entries.remove(&key);
        }

        if invalidated > 0 {
            tracing::info!(
                path = %normalized.display(),
                invalidated,
                "cache entries invalidated"
            );
        }
    }

    /// Build a cache key for a tool invocation.
    /// Returns None for tools that should not be cached (bash, write tools).
    fn cache_key(&self, tool_name: ToolName, args: &Value) -> Option<String> {
        match tool_name {
            ToolName::Read => {
                let path = args.get("path")?.as_str()?;
                let normalized = self.normalize_path(path);
                let offset = args
                    .get("offset")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1);
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "all".to_string());
                let max_lines = args
                    .get("max_lines")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(2000);
                Some(format!(
                    "read:{}:{}:{}:{}",
                    normalized.display(),
                    offset,
                    limit,
                    max_lines
                ))
            }
            ToolName::Grep => {
                let pattern = args.get("pattern")?.as_str()?;
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".");
                let normalized = self.normalize_path(path);
                let include = args
                    .get("include")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                Some(format!("grep:{}:{}:{}", pattern, normalized.display(), include))
            }
            ToolName::Glob => {
                let pattern = args.get("pattern")?.as_str()?;
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".");
                let normalized = self.normalize_path(path);
                Some(format!("glob:{}:{}", pattern, normalized.display()))
            }
            ToolName::List => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".");
                let normalized = self.normalize_path(path);
                let depth = args
                    .get("depth")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1);
                Some(format!("list:{}:{}", normalized.display(), depth))
            }
            // Don't cache tools with side effects or dynamic content
            ToolName::Bash | ToolName::Edit | ToolName::Write | ToolName::Patch
            | ToolName::Question | ToolName::Todo | ToolName::Webfetch | ToolName::Memory => None,
        }
    }

    /// Extract the primary file path referenced by a tool invocation.
    fn extract_path(&self, tool_name: ToolName, args: &Value) -> Option<PathBuf> {
        match tool_name {
            ToolName::Read | ToolName::List => {
                let path = args.get("path")?.as_str()?;
                Some(self.normalize_path(path))
            }
            ToolName::Grep | ToolName::Glob | ToolName::Edit | ToolName::Write
            | ToolName::Patch | ToolName::Bash | ToolName::Question | ToolName::Todo
            | ToolName::Webfetch | ToolName::Memory => None,
        }
    }

    /// Normalize a file path relative to the project root.
    fn normalize_path(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.project_root.join(p)
        }
    }

    /// Log cache statistics (called at end of session/stream).
    pub fn log_stats(&self) {
        if self.hits > 0 || self.misses > 0 {
            tracing::info!(
                hits = self.hits,
                misses = self.misses,
                entries = self.entries.len(),
                "tool result cache stats"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolName;
    use serde_json::json;

    fn test_cache() -> ToolResultCache {
        ToolResultCache::new(PathBuf::from("/project"))
    }

    fn test_output(text: &str) -> ToolOutput {
        ToolOutput {
            title: "test".to_string(),
            output: text.to_string(),
            is_error: false,
        }
    }

    #[test]
    fn test_cache_miss_then_hit() {
        let mut cache = test_cache();
        let args = json!({"path": "src/main.rs"});

        // Miss
        assert!(cache.get(ToolName::Read, &args).is_none());

        // Put
        cache.put(ToolName::Read, &args, &test_output("file content"));

        // Hit — returns the original content, not a compact reference
        let result = cache.get(ToolName::Read, &args);
        assert!(result.is_some());
        assert_eq!(result.unwrap().output, "file content");
    }

    #[test]
    fn test_cache_invalidation() {
        let mut cache = test_cache();
        let args = json!({"path": "src/main.rs"});

        cache.put(ToolName::Read, &args, &test_output("content"));
        assert!(cache.get(ToolName::Read, &args).is_some());

        // Invalidate
        cache.invalidate_path("src/main.rs");
        assert!(cache.get(ToolName::Read, &args).is_none());
    }

    #[test]
    fn test_no_cache_bash() {
        let mut cache = test_cache();
        let args = json!({"command": "ls"});

        // bash should not be cacheable
        assert!(cache.get(ToolName::Bash, &args).is_none());
        cache.put(ToolName::Bash, &args, &test_output("output"));
        assert!(cache.get(ToolName::Bash, &args).is_none());
    }

    #[test]
    fn test_grep_glob_invalidated_on_file_edit() {
        let mut cache = test_cache();

        // Cache a grep result
        let grep_args = json!({"pattern": "fn main", "path": "src/"});
        cache.put(ToolName::Grep, &grep_args, &test_output("src/main.rs:1: fn main()"));
        assert!(cache.get(ToolName::Grep, &grep_args).is_some());

        // Cache a glob result
        let glob_args = json!({"pattern": "**/*.rs"});
        cache.put(ToolName::Glob, &glob_args, &test_output("src/main.rs\nsrc/lib.rs"));
        assert!(cache.get(ToolName::Glob, &glob_args).is_some());

        // Editing any file should invalidate grep and glob entries
        cache.invalidate_path("src/other.rs");
        assert!(cache.get(ToolName::Grep, &grep_args).is_none());
        assert!(cache.get(ToolName::Glob, &glob_args).is_none());
    }

    #[test]
    fn test_no_cache_errors() {
        let mut cache = test_cache();
        let args = json!({"path": "missing.rs"});

        let error_output = ToolOutput {
            title: "read".to_string(),
            output: "Error: file not found".to_string(),
            is_error: true,
        };

        cache.put(ToolName::Read, &args, &error_output);
        assert!(cache.get(ToolName::Read, &args).is_none());
    }
}
