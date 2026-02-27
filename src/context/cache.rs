//! Tool result caching for avoiding redundant operations within a session.
//!
//! Caches results of read-only tools (read, grep, glob, list) and automatically
//! invalidates entries when write operations (edit, write, patch) modify files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::tool::ToolOutput;

/// Cached result from a tool execution.
#[derive(Clone)]
struct CachedResult {
    /// The original full tool output.
    output: ToolOutput,
    /// The tool_call_id from when this was first cached.
    first_call_id: String,
}

/// Cache for tool results within a single streaming session.
///
/// Session-scoped: created fresh for each `spawn_stream` call.
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

    /// Try to get a cached result. Returns the compact reference string if cached.
    pub fn get(&mut self, tool_name: &str, args: &Value) -> Option<ToolOutput> {
        let key = self.cache_key(tool_name, args)?;

        if let Some(cached) = self.entries.get(&key) {
            self.hits += 1;
            tracing::debug!(
                tool = tool_name,
                key = %key,
                hits = self.hits,
                "cache hit"
            );

            Some(ToolOutput {
                title: cached.output.title.clone(),
                output: format!(
                    "[Cached: same content as tool_call {}. File unchanged.]",
                    cached.first_call_id
                ),
                is_error: false,
            })
        } else {
            self.misses += 1;
            None
        }
    }

    /// Store a tool result in the cache.
    pub fn put(
        &mut self,
        tool_name: &str,
        args: &Value,
        tool_call_id: &str,
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
                first_call_id: tool_call_id.to_string(),
            },
        );
    }

    /// Invalidate all cache entries that reference the given file path.
    /// Call this after edit/write/patch operations.
    pub fn invalidate_path(&mut self, path: &str) {
        let normalized = self.normalize_path(path);

        if let Some(keys) = self.path_index.remove(&normalized) {
            let count = keys.len();
            for key in keys {
                self.entries.remove(&key);
            }
            tracing::debug!(
                path = %normalized.display(),
                invalidated = count,
                "cache entries invalidated"
            );
        }
    }

    /// Build a cache key for a tool invocation.
    /// Returns None for tools that should not be cached (bash, write tools).
    fn cache_key(&self, tool_name: &str, args: &Value) -> Option<String> {
        match tool_name {
            "read" => {
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
                Some(format!("read:{}:{}:{}", normalized.display(), offset, limit))
            }
            "grep" => {
                let pattern = args.get("pattern")?.as_str()?;
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".");
                let include = args
                    .get("include")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                Some(format!("grep:{}:{}:{}", pattern, path, include))
            }
            "glob" => {
                let pattern = args.get("pattern")?.as_str()?;
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".");
                Some(format!("glob:{}:{}", pattern, path))
            }
            "list" => {
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
            // Don't cache tools with side effects
            "bash" | "edit" | "write" | "patch" | "question" | "todo" | "webfetch" => None,
            _ => None,
        }
    }

    /// Extract the primary file path referenced by a tool invocation.
    fn extract_path(&self, tool_name: &str, args: &Value) -> Option<PathBuf> {
        match tool_name {
            "read" | "list" => {
                let path = args.get("path")?.as_str()?;
                Some(self.normalize_path(path))
            }
            // grep and glob operate on directories, so we don't track them
            // for path-based invalidation (they're invalidated when any file
            // in their scope changes, which is too broad to be useful).
            _ => None,
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
        assert!(cache.get("read", &args).is_none());

        // Put
        cache.put("read", &args, "call_1", &test_output("file content"));

        // Hit
        let result = cache.get("read", &args);
        assert!(result.is_some());
        assert!(result.unwrap().output.contains("Cached"));
    }

    #[test]
    fn test_cache_invalidation() {
        let mut cache = test_cache();
        let args = json!({"path": "src/main.rs"});

        cache.put("read", &args, "call_1", &test_output("content"));
        assert!(cache.get("read", &args).is_some());

        // Invalidate
        cache.invalidate_path("src/main.rs");
        assert!(cache.get("read", &args).is_none());
    }

    #[test]
    fn test_no_cache_bash() {
        let mut cache = test_cache();
        let args = json!({"command": "ls"});

        // bash should not be cacheable
        assert!(cache.get("bash", &args).is_none());
        cache.put("bash", &args, "call_1", &test_output("output"));
        assert!(cache.get("bash", &args).is_none());
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

        cache.put("read", &args, "call_1", &error_output);
        assert!(cache.get("read", &args).is_none());
    }
}
