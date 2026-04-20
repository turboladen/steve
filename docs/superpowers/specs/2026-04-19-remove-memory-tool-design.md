# Remove the Memory Tool

**Issue:** steve-a77e
**Date:** 2026-04-19
**Status:** Approved

## Summary

Delete the `memory` tool and the auto-loaded project memory file. Durable
project knowledge moves to `AGENTS.md`. Cross-session task continuity remains
with beads. A one-time startup sweep removes orphan `memory.md` files from the
storage directory.

## Motivation

The current memory system is harmful more often than useful:

- **No relevance-based pruning.** When memory exceeds 4 KB, the oldest bytes
  are truncated from the top. Useful early entries get evicted while
  low-value recent entries stay.
- **Low-value content.** The LLM saves ephemeral operational details
  ("I appended X to file Y") that don't belong in persistent project memory.
- **Manual cleanup required.** Users have to edit `memory.md` by hand to
  remove stale entries, which defeats the point of the tool.
- **Redundant.** `AGENTS.md` already covers durable project knowledge, and
  beads already provides cross-session task continuity. Memory duplicates
  both poorly.

A structured self-maintaining design (typed entries, staleness detection,
LLM-driven curation) was considered and rejected. Steve targets diverse
OpenAI-compatible models including small local models (qwen3-coder, etc.)
which are brittle under elaborate memory-curation prompts. The simplest
fix is removal.

## Scope

**In scope:** delete the tool, its file, all exhaustive-match arms, its
entries in agent allow-lists and permission rules, its prompt injection,
and leftover `memory.md` files on disk. Update `AGENTS.md` redirect in the
system prompt. Update `CLAUDE.md` and `CHANGELOG.md`.

**Out of scope:** deprecation shim, opt-out flag for the sweep,
preservation/export of existing memory content, replacement feature.

## Design

### 1. Code removals

- **Delete** `src/tool/memory.rs` entirely.
- **`src/tool/mod.rs`**: remove the `ToolName::Memory` variant and the
  `MemoryAction` enum (including its `FromStr`/`Display`/serde derives and
  round-trip tests). Remove the `memory::tool()` entry from the tool
  registry.
- **Exhaustive-match sites** — the compiler will enforce each removal as
  documented in `CLAUDE.md`:
  - `app/tool_display.rs`: `extract_args_summary()`, `extract_diff_content()`
  - `export.rs`: `extract_tool_summary()`
  - `context/cache.rs`: `cache_key()`, `extract_path()`
  - `context/compressor.rs`: `compress_tool_output()`
  - `stream/tools.rs`: `build_permission_summary()`, `extract_tool_path()`
  - `tool/mod.rs`: `is_write_tool()`, `intent_category()`, `tool_marker()`,
    `visual_category()`, `gutter_char()`, `path_arg_keys()`
  - `permission/mod.rs`: `always_allowed_rules()`, `build_mode_rules()`,
    `plan_mode_rules()`
  - `tool/agent.rs`: `allowed_tools()` (remove from each agent type's list)
- **`tests/permission_integration.rs`**: remove the `Memory` branch from the
  `if/else if` chain that iterates `ToolName::iter()`.
- **UI**: remove any Memory-specific rendering branches in
  `ui/message_block.rs` and `ui/message_area/render.rs`.
- **Sub-agent prompts** (`tool/agent.rs`): grep for any mention of the
  memory tool inside Explore/Plan/General agent prompt text and remove it.

### 2. System prompt and memory injection

In `src/app/prompt.rs`:

- **Remove** the memory-injection block that loads `{storage_dir}/memory.md`,
  truncates to 2000 chars, and pushes the `## Project Memory` section
  (currently lines ~124–139).
- **Remove** the `read_memory_file()` helper (currently lines ~213–222).
- **Replace** the `"Use the \`memory\` tool to persist important discoveries
  across sessions."` line (currently line 118) with:
  `- Durable project knowledge lives in AGENTS.md files; suggest updates to
  the user rather than trying to persist it yourself.`

### 3. Startup sweep of orphan `memory.md` files

Add a new helper to `src/storage/mod.rs`:

```rust
/// Delete orphan `memory.md` files from the removed memory tool. Idempotent.
pub fn sweep_legacy_memory_files() -> usize {
    // full impl per src/storage/mod.rs — logs data_dir/read_dir failures at
    // debug/warn, continues past per-file errors, silent on NotFound.
}
```

Call it once from `main.rs` alongside other startup side-effects (log dir
setup). Log a single info-level line with the total count when non-zero so
users see the cleanup occurred. Idempotent — subsequent startups find
nothing and do no work.

### 4. Documentation and changelog

- **`CLAUDE.md`**: remove all references to `MemoryAction`, the `memory`
  tool, and `Memory` from the exhaustive-ToolName-match-sites list.
- **`CHANGELOG.md`**: new entry under an `### Removed` heading:
  > Memory tool and auto-loaded project memory. Use AGENTS.md for durable
  > project knowledge; beads handles cross-session task continuity. Existing
  > `memory.md` files are cleaned up automatically on first launch.

### 5. Testing

- Memory tool tests vanish with the file.
- `MemoryAction` round-trip tests in `tool/mod.rs` are removed with the enum.
- Add a test in `storage/mod.rs` for `sweep_legacy_memory_files` using a
  `tempdir`: create several `storage/proj-*/memory.md` fakes, run the
  sweep, assert files gone and count correct. Also assert the idempotent
  case — second call on clean state returns 0.
- Existing `ToolName::iter()` integration tests update automatically once
  the variant is gone; verify the `if/else if` chain in
  `tests/permission_integration.rs` no longer needs its Memory branch.
- Run `cargo test` and `cargo clippy --all-targets -- -D warnings`.

## Non-goals

- No deprecation shim — the tool just vanishes. The LLM will receive a
  normal "unknown tool" error if it attempts to call `memory` on an old
  session transcript; this is acceptable.
- No export of existing memory content before deletion. The feature was
  already unreliable; preserving unreliable content is not useful.
- No opt-out flag for the sweep. A one-way delete of a 4 KB-capped file
  is low-risk.
- No replacement memory feature. `AGENTS.md` and beads cover the real
  need.

## Risks

- **Data loss for users who treat `memory.md` as notes.** Acceptable:
  memory is documented as LLM-managed storage, not user notes, and the
  CHANGELOG entry is explicit.
- **CLAUDE.md drift.** The exhaustive-match list is a frequent source of
  CI failures. The compiler will enforce every match-site removal; CI's
  `cargo clippy --all-targets -- -D warnings` catches anything missed.
- **Sub-agent prompts mentioning memory.** Easy to miss. Grep check
  before finishing.
