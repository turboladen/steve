# Context Management Layer for Steve

## Context

Steve currently sends the **full conversation history on every LLM API call**, including during the tool-call loop in `stream.rs`. A typical "read 3 files, then respond" interaction costs 2 full-context API calls where the second re-sends everything from the first. This results in ~2x the necessary token cost, compounding with each tool loop iteration.

**Goal**: Reduce API costs by 40-60% through smart context management, and improve latency via parallel tool execution. All changes must work with any OpenAI-compatible API (targeting Fuel iX first).

**Scope**: Three subsystems — tool result compression, tool result caching, and parallel read-only tool execution. Future phases (plan+execute, schema pruning, enriched tools) are deferred.

---

## Phase 1: Tool Result Compression

**The single highest-impact change.** Estimated savings: 30-50% on multi-step sessions.

### What

Before each LLM API call within the tool loop, compress tool results the LLM has already processed in previous loop iterations. Replace full outputs with heuristic summaries.

### New file: `src/context/mod.rs` + `src/context/compressor.rs`

Create a `compress_old_tool_results()` function that takes the `messages` vec and a `keep_recent_count` (how many recent tool results to leave uncompressed — typically the current iteration's batch).

**Compression heuristics** (all client-side, no LLM calls):

| Tool | Full output | Compressed form |
|------|------------|-----------------|
| `read` | 2000 tokens of file content | `"[Previously read: src/main.rs, 150 lines, Rust. Defines: fn main(), mod app. Re-read if needed.]"` (~30 tokens) |
| `grep` | 500 tokens of matches | `"[Previously searched: 'pattern' → 12 matches in 5 files (src/app.rs:5, src/stream.rs:3). Re-search if needed.]"` (~25 tokens) |
| `glob` | 200 file paths | `"[Previously globbed: '**/*.rs' → 33 files. Re-glob if needed.]"` (~15 tokens) |
| `bash` | 1000 tokens of output | `"[Previously ran: 'cargo build' → exit 0, success.]"` or `"[Previously ran: 'cargo test' → exit 1, 3 failures. Re-run if needed.]"` (~15 tokens) |
| `list` | 500 tokens of directory listing | `"[Previously listed: src/ → 15 entries. Re-list if needed.]"` (~12 tokens) |

### Changes to `src/stream.rs`

At line ~137, before building the `CreateChatCompletionRequest`, call the compressor:

```rust
// Track how many messages existed before this iteration's tool results
let pre_iteration_count = messages.len() - current_iteration_tool_count;
compress_old_tool_results(&mut messages, current_iteration_tool_count);
```

Need to track `current_iteration_tool_count` — reset to 0 at the top of the outer loop (line 127), incremented each time a tool result is pushed (line 460).

### Implementation details

- The compressor identifies tool result messages by their type (`ChatCompletionRequestMessage::Tool`)
- It replaces the `content` field text with the compressed summary
- It preserves the `tool_call_id` so the conversation structure remains valid
- For `read` results: detect language from file extension, count lines, extract first few function/struct/class names via simple regex
- For `grep` results: count total matches, group by file, show top 3 files
- For `bash` results: extract exit code from last line pattern, detect success/failure keywords
- Only compress tool results from **completed** loop iterations (the LLM has already seen these and acted on them)

---

## Phase 2: Tool Result Caching

**Estimated savings**: 20-40% on sessions with repeated file reads (very common in edit workflows).

### New file: `src/context/cache.rs`

A `ToolResultCache` struct that:
- Maps `(tool_name, canonical_args_hash)` → `CachedResult { output, tool_call_id_first_seen }`
- Session-scoped (lives in the stream task, created fresh per `spawn_stream`)
- Invalidation: when `edit`, `write`, or `patch` executes on a path, all cache entries referencing that path are evicted

### Cache key construction

| Tool | Cache key |
|------|-----------|
| `read` | `("read", normalize_path(path), offset, limit)` |
| `grep` | `("grep", pattern, path, include)` |
| `glob` | `("glob", pattern, path)` |
| `list` | `("list", normalize_path(path), depth)` |
| `bash` | Not cached (side effects) |
| `edit/write/patch` | Not cached + triggers invalidation |

### Cache hit behavior

On cache hit, the tool handler still "executes" (instantly, from cache), but the output sent to the LLM is a compact reference:

```
[Cached: same content as tool_call {original_id}. File unchanged.]
```

This saves tokens because the LLM doesn't re-process the full content.

### Changes to `src/tool/mod.rs`

Add `ToolRegistry::execute_cached()` method that wraps `execute()`:
1. Compute cache key from tool name + args
2. Check cache → if hit and not invalidated, return compact reference
3. If miss, execute normally, store result in cache, return full output

### Changes to `src/stream.rs`

- Create `ToolResultCache` before the tool loop
- Pass it to `execute_cached()` instead of `execute()` at line 440
- After write/edit/patch execution, call `cache.invalidate_path(path)`

---

## Phase 3: Parallel Read-Only Tool Execution

**Estimated savings**: Latency reduction of 50-80% for multi-read operations (no token savings, but major UX improvement).

### Changes to `src/stream.rs` (lines 311-466)

Restructure the tool execution loop:

1. **Partition** pending tool calls into two groups:
   - `auto_allowed`: tools where `permission_engine.check()` returns `Allow` (typically read, grep, glob, list)
   - `needs_interaction`: tools where check returns `Ask` or `Deny`

2. **Execute auto-allowed tools in parallel** using `tokio::task::spawn_blocking` (since tool handlers are synchronous `Fn(Value, ToolContext) -> Result<ToolOutput>`):
   ```rust
   let handles: Vec<_> = auto_allowed.iter().map(|tc| {
       let registry = registry.clone();
       let ctx = ctx.clone();
       let args = args.clone();
       let name = tc.function_name.clone();
       tokio::task::spawn_blocking(move || registry.execute(&name, args, ctx))
   }).collect();

   let results = futures::future::join_all(handles).await;
   ```

3. **Execute permission-required tools sequentially** (preserving existing permission handshake flow)

4. **Collect all results** in original tool call order, push to messages vec

### Requirements

- `ToolRegistry` must be wrapped in `Arc` (already is: `tool_registry: Option<std::sync::Arc<ToolRegistry>>`)
- `ToolContext` must be `Clone` (already is: `#[derive(Clone)]`)
- Tool handlers must be `Send + Sync` (already are: `Box<dyn Fn(...) + Send + Sync>`)

### Interaction with cache (Phase 2)

Cache lookups happen inside `execute_cached()`, so parallel execution naturally benefits from caching — cache hits return immediately, only cache misses do actual I/O.

---

## Files to Create

| File | Purpose |
|------|---------|
| `src/context/mod.rs` | Module declaration for context management |
| `src/context/compressor.rs` | Tool result compression logic |
| `src/context/cache.rs` | Tool result caching with invalidation |

## Files to Modify

| File | Changes |
|------|---------|
| `src/stream.rs` | Integrate compressor before API calls, add cache, restructure tool loop for parallel execution |
| `src/tool/mod.rs` | Add `execute_cached()` method, make `ToolRegistry` clonable or add `Arc` wrapper for parallel use |
| `src/main.rs` or `src/lib.rs` | Add `mod context;` declaration |

## Files NOT Modified

- `src/app.rs` — no changes to event handling, system prompt, or history building
- `src/tool/*.rs` — individual tool implementations unchanged
- `src/permission/mod.rs` — permission logic unchanged
- `src/config/types.rs` — no new config options in this phase

---

## Verification

1. **Build**: `cargo build` must succeed with no errors
2. **Manual test — compression**:
   - Start Steve, ask it to read multiple files and then make an edit
   - Enable `RUST_LOG=steve=debug` and check logs for compression activity
   - Compare token usage before/after (visible in session metadata)
3. **Manual test — caching**:
   - Read a file, then ask a question that causes the same file to be re-read
   - Verify logs show cache hit and compact reference sent to LLM
   - Edit the file, then re-read — verify cache miss (invalidation worked)
4. **Manual test — parallel execution**:
   - Ask a question that triggers 3+ simultaneous reads (e.g., "explain the architecture of the tool system")
   - Check logs for parallel spawn_blocking calls vs sequential execution
   - Verify results are still in correct order in conversation
5. **Regression**: Verify existing behavior — single tool calls, permission prompts, compaction, cancellation all still work correctly

---

## Implementation Order

1. Create `src/context/mod.rs` and `src/context/compressor.rs` — implement compression
2. Integrate compressor into `src/stream.rs` tool loop
3. Test compression manually
4. Create `src/context/cache.rs` — implement caching with invalidation
5. Add `execute_cached()` to `src/tool/mod.rs`
6. Integrate cache into `src/stream.rs`
7. Test caching manually
8. Restructure tool execution loop in `src/stream.rs` for parallel execution
9. Test parallel execution manually
10. End-to-end testing of all three features together

## Future Phases (Deferred)

- Phase 4: Tactical improvements (output caps, loop depth limit, grep context lines)
- Phase 5: Plan+Execute model (structured planning tool, client-side batching)
- Phase 6: Adaptive tool schema pruning
