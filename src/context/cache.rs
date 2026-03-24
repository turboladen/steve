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
    /// Cache generation when this entry was created.
    generation: u64,
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
    /// Monotonically increasing generation counter. Bumped before each
    /// `spawn_stream` (i.e., each user message). Entries without mtime from
    /// a previous generation are treated as stale in `get()`.
    generation: u64,
    /// Stats
    hits: u32,
    misses: u32,
}

/// Prefix of the cache-repeat summary message. Stream code uses this to detect
/// when the LLM is stuck re-reading cached content and should stop looping.
pub const CACHE_REPEAT_PREFIX: &str = "[Content unchanged";

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
            generation: 0,
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
                    // Also clean path_index to prevent duplicate keys on re-put
                    if let Some(path) = self.extract_path(tool_name, args) {
                        let mut remove_entry = false;
                        if let Some(keys) = self.path_index.get_mut(&path) {
                            keys.retain(|k| k != &key);
                            if keys.is_empty() {
                                remove_entry = true;
                            }
                        }
                        if remove_entry {
                            self.path_index.remove(&path);
                        }
                    }
                    self.misses += 1;
                    return None;
                }
            } else if cached.generation != self.generation {
                // No mtime to check (grep, glob, multi-file read) and entry is
                // from a previous generation — files may have changed externally
                // between user turns.
                tracing::info!(
                    tool = %tool_name,
                    key = %key,
                    entry_gen = cached.generation,
                    current_gen = self.generation,
                    "cache miss — stale generation (no mtime)"
                );
                self.entries.remove(&key);
                self.hit_counts.remove(&key);
                // No path_index cleanup needed: tools that reach this branch
                // (grep, glob, multi-file read) have extract_path() -> None,
                // so they were never added to path_index in put().
                self.misses += 1;
                return None;
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
                // Use a directive message that tells the LLM to proceed,
                // not just that content was "already provided".
                Some(ToolOutput {
                    title: cached.output.title.clone(),
                    output: "[Content unchanged — you already have this in your conversation. \
                             Do NOT re-read this file. Proceed with the information you have \
                             and answer the user's question.]"
                        .to_string(),
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
                generation: self.generation,
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
                let is_count = args.get("count").and_then(|v| v.as_bool()).unwrap_or(false);
                let tail_n = args.get("tail").and_then(|v| v.as_u64());

                // Multi-file mode
                if let Some(paths_arr) = args.get("paths").and_then(|v| v.as_array()) {
                    let mut sorted: Vec<String> = paths_arr
                        .iter()
                        .filter_map(|v| v.as_str())
                        .map(|p| self.normalize_path(p).display().to_string())
                        .collect();
                    sorted.sort();
                    return Some(format!(
                        "read-multi:{}:count={}",
                        sorted.join(","),
                        is_count
                    ));
                }

                let path = args.get("path")?.as_str()?;
                let normalized = self.normalize_path(path);

                if is_count {
                    Some(format!("read:{}:count", normalized.display()))
                } else if let Some(n) = tail_n {
                    let max_lines = args
                        .get("max_lines")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(2000);
                    Some(format!(
                        "read:{}:tail={}:{}",
                        normalized.display(),
                        n,
                        max_lines
                    ))
                } else {
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
            ToolName::Read => {
                // Multi-file: can't track mtime of multiple files with single path
                if args.get("paths").and_then(|v| v.as_array()).is_some() {
                    return None;
                }
                let path = args.get("path")?.as_str()?;
                Some(self.normalize_path(path))
            }
            ToolName::List | ToolName::Symbols => {
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

    /// Advance the cache generation. Call before each `spawn_stream` so that
    /// mtime-less entries (grep, glob, multi-file reads) from a previous user
    /// turn are treated as stale.
    pub fn bump_generation(&mut self) {
        self.generation += 1;
        tracing::debug!(generation = self.generation, "cache generation bumped");
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
            r.output.starts_with(CACHE_REPEAT_PREFIX),
            "expected cache-repeat summary after threshold, got: {}",
            r.output
        );
        assert!(!r.is_error);
        // Verify the message is directive (tells LLM what to DO)
        assert!(
            r.output.contains("Do NOT re-read"),
            "cache repeat message should be directive, got: {}",
            r.output
        );
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

    #[test]
    fn test_cache_repeat_prefix_matches_actual_message() {
        // Ensure the public CACHE_REPEAT_PREFIX matches what get() actually returns.
        let mut cache = test_cache();
        let args = json!({"path": "src/foo.rs"});
        cache.put(ToolName::Read, &args, &test_output("content"));

        // Exhaust threshold
        for _ in 0..ToolResultCache::REPEAT_THRESHOLD {
            cache.get(ToolName::Read, &args);
        }
        // This hit should return the summary
        let r = cache.get(ToolName::Read, &args).unwrap();
        assert!(
            r.output.starts_with(CACHE_REPEAT_PREFIX),
            "CACHE_REPEAT_PREFIX '{}' must match start of actual message: '{}'",
            CACHE_REPEAT_PREFIX,
            &r.output[..r.output.len().min(40)]
        );
    }

    // -- Cache key format tests for new read modes --

    #[test]
    fn test_cache_key_read_count_mode() {
        let cache = test_cache();
        let args = json!({"path": "src/main.rs", "count": true});
        let key = cache.cache_key(ToolName::Read, &args).unwrap();
        assert!(
            key.contains(":count"),
            "count mode key should contain ':count', got: {key}"
        );
        // Should NOT contain offset/limit format
        assert!(!key.contains(":1:all:"));
    }

    #[test]
    fn test_cache_key_read_tail_mode() {
        let cache = test_cache();
        let args = json!({"path": "src/main.rs", "tail": 20});
        let key = cache.cache_key(ToolName::Read, &args).unwrap();
        assert!(
            key.contains(":tail=20:"),
            "tail mode key should contain ':tail=20:', got: {key}"
        );
    }

    #[test]
    fn test_cache_key_read_multi_file() {
        let cache = test_cache();
        let args = json!({"paths": ["b.rs", "a.rs"]});
        let key = cache.cache_key(ToolName::Read, &args).unwrap();
        assert!(
            key.starts_with("read-multi:"),
            "multi-file key should start with 'read-multi:', got: {key}"
        );
        // Paths should be sorted
        let a_pos = key.find("a.rs").unwrap();
        let b_pos = key.find("b.rs").unwrap();
        assert!(
            a_pos < b_pos,
            "paths should be sorted in key: {key}"
        );
    }

    #[test]
    fn test_cache_key_read_multi_file_count() {
        let cache = test_cache();
        let args = json!({"paths": ["a.rs"], "count": true});
        let key = cache.cache_key(ToolName::Read, &args).unwrap();
        assert!(
            key.contains("count=true"),
            "multi-file count key should contain 'count=true', got: {key}"
        );
    }

    #[test]
    fn test_extract_path_returns_none_for_multi_file() {
        let cache = test_cache();
        let args = json!({"paths": ["a.rs", "b.rs"]});
        assert!(
            cache.extract_path(ToolName::Read, &args).is_none(),
            "extract_path should return None for multi-file reads"
        );
    }

    #[test]
    fn test_extract_path_returns_some_for_single_file() {
        let cache = test_cache();
        let args = json!({"path": "src/main.rs"});
        assert!(
            cache.extract_path(ToolName::Read, &args).is_some(),
            "extract_path should return Some for single-file reads"
        );
    }

    // -- Generation-based invalidation tests --

    #[test]
    fn test_generation_invalidates_no_mtime_entries() {
        let mut cache = test_cache();
        let grep_args = json!({"pattern": "fn main", "path": "src/"});

        // Cache a grep result (has no mtime)
        cache.put(ToolName::Grep, &grep_args, &test_output("grep results"));
        assert!(cache.get(ToolName::Grep, &grep_args).is_some());

        // Bump generation (simulates new user message)
        cache.bump_generation();

        // Should now be a cache miss — stale generation
        assert!(
            cache.get(ToolName::Grep, &grep_args).is_none(),
            "grep entry should be invalidated after generation bump"
        );
    }

    #[test]
    fn test_generation_preserves_mtime_entries() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.rs");
        std::fs::write(&file_path, "content").unwrap();

        let mut cache = ToolResultCache::new(dir.path().to_path_buf());
        let path_str = file_path.to_string_lossy().to_string();
        let args = json!({"path": path_str});

        cache.put(ToolName::Read, &args, &test_output("content"));

        // Bump generation
        cache.bump_generation();

        // Single-file read has mtime — should still hit (file unchanged)
        let r = cache.get(ToolName::Read, &args);
        assert!(
            r.is_some(),
            "mtime entries should survive generation bump if file unchanged"
        );
    }

    #[test]
    fn test_generation_same_turn_cache_hit() {
        let mut cache = test_cache();
        let glob_args = json!({"pattern": "**/*.rs"});

        cache.put(ToolName::Glob, &glob_args, &test_output("file list"));

        // No generation bump — same turn
        let r = cache.get(ToolName::Glob, &glob_args);
        assert!(r.is_some(), "no-mtime entries should hit within same generation");
        assert_eq!(r.unwrap().output, "file list");
    }

    #[test]
    fn test_generation_multi_file_read_invalidated() {
        let mut cache = test_cache();
        let args = json!({"paths": ["a.rs", "b.rs"]});

        cache.put(ToolName::Read, &args, &test_output("multi content"));
        assert!(cache.get(ToolName::Read, &args).is_some());

        cache.bump_generation();
        assert!(
            cache.get(ToolName::Read, &args).is_none(),
            "multi-file read (no mtime) should be invalidated on generation bump"
        );
    }

    #[test]
    fn test_generation_preserves_list_with_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("src");
        std::fs::create_dir(&sub).unwrap();

        let mut cache = ToolResultCache::new(dir.path().to_path_buf());
        let path_str = sub.to_string_lossy().to_string();
        let args = json!({"path": path_str, "depth": 1});

        cache.put(ToolName::List, &args, &test_output("src/main.rs"));

        cache.bump_generation();

        // List has mtime — should survive generation bump when dir unchanged
        let r = cache.get(ToolName::List, &args);
        assert!(
            r.is_some(),
            "List entries with mtime should survive generation bump if dir unchanged"
        );
    }

    #[test]
    fn test_generation_bump_resets_repeat_counter() {
        let mut cache = test_cache();
        let grep_args = json!({"pattern": "TODO", "path": "src/"});

        cache.put(ToolName::Grep, &grep_args, &test_output("TODO items"));

        // Hit once (builds toward repeat threshold)
        let r = cache.get(ToolName::Grep, &grep_args).unwrap();
        assert_eq!(r.output, "TODO items");

        // Bump generation — entry evicted, counter cleared
        cache.bump_generation();

        // Re-cache with same content
        cache.put(ToolName::Grep, &grep_args, &test_output("TODO items"));

        // Should get full content again (counter was reset)
        let r = cache.get(ToolName::Grep, &grep_args).unwrap();
        assert_eq!(
            r.output, "TODO items",
            "repeat counter should reset after generation bump + re-cache"
        );
    }
}
