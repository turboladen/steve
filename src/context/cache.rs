//! Tool result caching for avoiding redundant operations within a session.
//!
//! Caches results of read-only tools (read, grep, glob, list) and automatically
//! invalidates entries when write operations (edit, write, patch) modify files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::tool::{ToolName, ToolOutput};

/// Cached result from a tool execution.
#[derive(Clone)]
struct CachedResult {
    /// The original full tool output.
    output: ToolOutput,
    /// mtime of the file when cached (None for non-file tools like grep/glob).
    mtime: Option<SystemTime>,
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
    /// Per-key hit counter. After `REPEAT_THRESHOLD` hits on the same key,
    /// `get()` returns a short summary instead of full content to break
    /// compressor/cache feedback loops where the LLM re-reads the same file
    /// indefinitely.
    hit_counts: HashMap<String, u32>,
    /// Project root for normalizing paths.
    project_root: PathBuf,
    /// Stats
    hits: u32,
    misses: u32,
}

impl ToolResultCache {
    /// Number of cache hits on the same key before `get()` returns a summary
    /// instead of the full cached content.
    const REPEAT_THRESHOLD: u32 = 2;

    pub fn new(project_root: PathBuf) -> Self {
        Self {
            entries: HashMap::new(),
            path_index: HashMap::new(),
            hit_counts: HashMap::new(),
            project_root,
            hits: 0,
            misses: 0,
        }
    }

    /// Try to get a cached result.
    ///
    /// For the first `REPEAT_THRESHOLD - 1` hits, returns the full cached
    /// output so the LLM can use the content directly. After that, returns
    /// a short summary to break compressor/cache feedback loops where the
    /// LLM re-reads the same file indefinitely.
    pub fn get(&mut self, tool_name: ToolName, args: &Value) -> Option<ToolOutput> {
        let key = self.cache_key(tool_name, args)?;

        // Check if the file has been modified externally since we cached it.
        if let Some(cached) = self.entries.get(&key) {
            if let Some(cached_mtime) = cached.mtime {
                let stale = self
                    .extract_path(tool_name, args)
                    .and_then(|path| std::fs::metadata(&path).ok())
                    .and_then(|meta| meta.modified().ok())
                    .is_some_and(|current_mtime| current_mtime != cached_mtime);
                if stale {
                    tracing::info!(
                        tool = %tool_name,
                        key = %key,
                        "cache miss — file modified externally"
                    );
                    self.entries.remove(&key);
                    self.hit_counts.remove(&key);
                    self.misses += 1;
                    return None;
                }
            }
        }

        if let Some(cached) = self.entries.get(&key) {
            self.hits += 1;
            let count = self.hit_counts.entry(key.clone()).or_insert(0);
            *count += 1;

            tracing::info!(
                tool = %tool_name,
                key = %key,
                hits = self.hits,
                repeat_count = *count,
                "cache hit"
            );

            if *count >= Self::REPEAT_THRESHOLD {
                // Break the compressor/cache feedback loop: after repeated
                // hits the LLM is clearly stuck re-reading the same content.
                let prior_hits = *count - 1;
                let time_word = if prior_hits == 1 { "time" } else { "times" };
                Some(ToolOutput {
                    title: cached.output.title.clone(),
                    output: format!(
                        "[This content was already provided {prior_hits} {time_word}. It has not changed.]"
                    ),
                    is_error: false,
                })
            } else {
                Some(cached.output.clone())
            }
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

        // Reset hit count so re-cached content gets full delivery again
        self.hit_counts.remove(&key);

        // Track which file paths this cache entry references (for invalidation)
        // and capture the file's mtime for external-change detection.
        let mtime = self
            .extract_path(tool_name, args)
            .and_then(|path| {
                self.path_index
                    .entry(path.clone())
                    .or_default()
                    .push(key.clone());
                std::fs::metadata(&path)
                    .ok()
                    .and_then(|m| m.modified().ok())
            });

        self.entries.insert(
            key,
            CachedResult {
                output: output.clone(),
                mtime,
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
                self.hit_counts.remove(&key);
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
            self.hit_counts.remove(&key);
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
            ToolName::Symbols => {
                let path = args.get("path")?.as_str()?;
                let normalized = self.normalize_path(path);
                let op = args.get("operation").and_then(|v| v.as_str()).unwrap_or("list_symbols");
                let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let line = args.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                Some(format!("symbols:{}:{}:{}:{}", normalized.display(), op, name, line))
            }
            // Don't cache tools with side effects or dynamic content
            ToolName::Bash | ToolName::Edit | ToolName::Write | ToolName::Patch
            | ToolName::Move | ToolName::Copy | ToolName::Delete | ToolName::Mkdir
            | ToolName::Question | ToolName::Task | ToolName::Webfetch | ToolName::Memory
            | ToolName::Lsp | ToolName::Agent => None,
        }
    }

    /// Extract the primary file path referenced by a tool invocation.
    fn extract_path(&self, tool_name: ToolName, args: &Value) -> Option<PathBuf> {
        match tool_name {
            ToolName::Read | ToolName::List | ToolName::Symbols => {
                let path = args.get("path")?.as_str()?;
                Some(self.normalize_path(path))
            }
            ToolName::Grep | ToolName::Glob | ToolName::Edit | ToolName::Write
            | ToolName::Patch | ToolName::Move | ToolName::Copy | ToolName::Delete
            | ToolName::Mkdir | ToolName::Bash | ToolName::Question | ToolName::Task
            | ToolName::Webfetch | ToolName::Memory | ToolName::Lsp
            | ToolName::Agent => None,
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

    /// Return `(hits, misses)` counters for diagnostics.
    pub fn cache_stats(&self) -> (u32, u32) {
        (self.hits, self.misses)
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
    fn test_cache_hits_below_threshold_return_full_content() {
        let mut cache = test_cache();
        let args = json!({"path": "src/main.rs"});

        cache.put(ToolName::Read, &args, &test_output("full file content"));

        // Hits below the threshold should return the original content
        for i in 0..ToolResultCache::REPEAT_THRESHOLD - 1 {
            let r = cache.get(ToolName::Read, &args).unwrap();
            assert_eq!(r.output, "full file content", "hit {} should return full content", i + 1);
        }
    }

    #[test]
    fn test_cache_repeat_returns_summary_after_threshold() {
        let mut cache = test_cache();
        let args = json!({"path": "src/main.rs"});

        cache.put(ToolName::Read, &args, &test_output("full file content"));

        // Exhaust the threshold
        for _ in 0..ToolResultCache::REPEAT_THRESHOLD - 1 {
            let r = cache.get(ToolName::Read, &args).unwrap();
            assert_eq!(r.output, "full file content");
        }

        // Next hit should return a summary, not the full content
        let r = cache.get(ToolName::Read, &args).unwrap();
        assert!(
            r.output.contains("already provided"),
            "expected summary after threshold, got: {}",
            r.output
        );
        assert!(!r.is_error);
    }

    #[test]
    fn test_cache_repeat_counter_resets_on_invalidation() {
        let mut cache = test_cache();
        let args = json!({"path": "src/main.rs"});

        cache.put(ToolName::Read, &args, &test_output("original content"));

        // Hit once (just under threshold)
        let r = cache.get(ToolName::Read, &args).unwrap();
        assert_eq!(r.output, "original content");

        // Invalidate (simulating a write to the file)
        cache.invalidate_path("src/main.rs");

        // Re-populate cache with new content
        cache.put(ToolName::Read, &args, &test_output("updated content"));

        // Counter should be reset — hits below threshold return full content
        for i in 0..ToolResultCache::REPEAT_THRESHOLD - 1 {
            let r = cache.get(ToolName::Read, &args).unwrap();
            assert_eq!(r.output, "updated content", "post-invalidation hit {} should return full content", i + 1);
        }
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

    #[test]
    fn test_cache_stats_returns_hits_misses() {
        let mut cache = test_cache();
        let args = json!({"path": "src/main.rs"});

        // Initial state: 0/0
        assert_eq!(cache.cache_stats(), (0, 0));

        // Miss
        cache.get(ToolName::Read, &args);
        assert_eq!(cache.cache_stats(), (0, 1));

        // Put + Hit
        cache.put(ToolName::Read, &args, &test_output("content"));
        cache.get(ToolName::Read, &args);
        assert_eq!(cache.cache_stats(), (1, 1));
    }

    #[test]
    fn test_cache_invalidates_on_external_mtime_change() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.rs");
        std::fs::write(&file_path, "original").unwrap();

        let mut cache = ToolResultCache::new(dir.path().to_path_buf());
        let path_str = file_path.to_string_lossy().to_string();
        let args = json!({"path": path_str});

        cache.put(ToolName::Read, &args, &test_output("original"));

        // First hit — file unchanged, should return cached content
        let r = cache.get(ToolName::Read, &args);
        assert!(r.is_some(), "should hit cache when file unchanged");
        assert_eq!(r.unwrap().output, "original");

        // Modify the file externally (simulate git merge, editor save, etc.)
        // Sleep to ensure mtime differs (HFS+ on macOS has 1-second granularity)
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&file_path, "modified externally").unwrap();

        // Next hit — file mtime changed, should be a cache miss
        let r = cache.get(ToolName::Read, &args);
        assert!(r.is_none(), "should miss cache when file modified externally");
    }
}
