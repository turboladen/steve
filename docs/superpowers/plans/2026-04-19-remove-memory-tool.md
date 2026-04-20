# Remove Memory Tool Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the `memory` tool and its auto-loaded project memory. Clean up orphan `memory.md` files on startup. Redirect durable project knowledge to `AGENTS.md`.

**Architecture:** Four small commits landed together: (1) add an idempotent startup sweep of orphan `memory.md` files, (2) remove memory injection and helper from the system prompt, (3) delete the memory tool file, enum variant, `MemoryAction` enum, `is_memory()` helper, and every exhaustive-match arm across the codebase, (4) update docs and changelog. The compiler enforces the exhaustive-match removals.

**Tech Stack:** Rust 2024, strum-derived enums, tracing for logs, `directories` crate for data dir resolution, `tempfile` for tests.

**Issue:** steve-a77e
**Spec:** `docs/superpowers/specs/2026-04-19-remove-memory-tool-design.md`

---

## File Structure

- **Create:** none (all edits are to existing files)
- **Delete:** `src/tool/memory.rs`
- **Modify:** `src/storage/mod.rs`, `src/main.rs`, `src/app/prompt.rs`, `src/tool/mod.rs`, `src/tool/agent.rs`, `src/permission/mod.rs`, `src/stream/tools.rs`, `src/stream/phases.rs`, `src/export.rs`, `src/context/cache.rs`, `src/context/compressor.rs`, `src/app/tool_display.rs`, `src/ui/message_block.rs`, `src/ui/message_area/render.rs`, `tests/permission_integration.rs`, `CLAUDE.md`, `CHANGELOG.md`

---

## Task 1: Add `sweep_legacy_memory_files` with TDD

**Files:**
- Modify: `src/storage/mod.rs`

Steve uses `{data_dir}/storage/{project_id}/memory.md` for the per-project memory file. Sweeping all subdirectories of `{data_dir}/storage/` and unlinking each `memory.md` cleans up every project in one pass. The sweep is idempotent — subsequent runs find no files and do no work.

- [ ] **Step 1.1: Add the failing test to `src/storage/mod.rs`**

Open `src/storage/mod.rs` and find the existing `mod tests` block (near the bottom, after the `data_dir()` helper at line ~152). Add this test inside the `mod tests` block:

```rust
    #[test]
    fn sweep_removes_memory_files_across_projects() {
        let dir = tempdir().expect("failed to create temp dir");
        let storage_root = dir.path();

        // Simulate two project storage dirs, each with a memory.md + an unrelated file.
        for proj in ["proj-alpha", "proj-beta"] {
            let p = storage_root.join(proj);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("memory.md"), "stale content").unwrap();
            std::fs::write(p.join("keep.txt"), "should remain").unwrap();
        }

        let removed = sweep_legacy_memory_files_in(storage_root);
        assert_eq!(removed, 2, "should remove both memory.md files");

        for proj in ["proj-alpha", "proj-beta"] {
            let p = storage_root.join(proj);
            assert!(!p.join("memory.md").exists(), "memory.md should be gone");
            assert!(p.join("keep.txt").exists(), "unrelated files must stay");
        }

        // Second call is idempotent.
        let removed_again = sweep_legacy_memory_files_in(storage_root);
        assert_eq!(removed_again, 0, "second sweep finds nothing");
    }

    #[test]
    fn sweep_handles_missing_storage_dir() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let removed = sweep_legacy_memory_files_in(&missing);
        assert_eq!(removed, 0);
    }
```

- [ ] **Step 1.2: Run the test and watch it fail**

Run: `cargo test -p steve sweep_ --lib`
Expected: compile error — `sweep_legacy_memory_files_in` not found.

- [ ] **Step 1.3: Implement the sweep function**

In `src/storage/mod.rs`, immediately below the existing `fn data_dir()` helper, add:

```rust
/// Delete orphan `memory.md` files from the removed memory tool. Idempotent.
pub fn sweep_legacy_memory_files() -> usize {
    let storage_root = match data_dir() {
        Ok(dir) => dir,
        Err(err) => {
            tracing::debug!(error = %err, "sweep skipped: could not determine data dir");
            return 0;
        }
    };
    sweep_legacy_memory_files_in(&storage_root)
}

fn sweep_legacy_memory_files_in(storage_root: &std::path::Path) -> usize {
    let entries = match std::fs::read_dir(storage_root) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(err) => {
            tracing::warn!(
                path = %storage_root.display(),
                error = %err,
                "sweep skipped: could not read storage root",
            );
            return 0;
        }
    };

    let mut removed = 0;
    for result in entries {
        let entry = match result {
            Ok(e) => e,
            Err(err) => {
                tracing::debug!(error = %err, "skipping unreadable directory entry");
                continue;
            }
        };

        let is_dir = match entry.file_type() {
            Ok(ft) => ft.is_dir(),
            Err(err) => {
                tracing::debug!(
                    path = %entry.path().display(),
                    error = %err,
                    "could not determine file type, skipping",
                );
                continue;
            }
        };
        if !is_dir {
            continue;
        }

        let memory_file = entry.path().join("memory.md");
        match std::fs::remove_file(&memory_file) {
            Ok(()) => {
                tracing::info!(
                    path = %memory_file.display(),
                    "removed legacy memory.md",
                );
                removed += 1;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!(
                    path = %memory_file.display(),
                    error = %err,
                    "failed to remove legacy memory.md",
                );
            }
        }
    }
    removed
}
```

- [ ] **Step 1.4: Run the tests and watch them pass**

Run: `cargo test -p steve sweep_ --lib`
Expected: both tests PASS.

- [ ] **Step 1.5: Run clippy and nightly fmt**

Run: `cargo clippy --all-targets -- -D warnings`
Run: `cargo +nightly fmt`
Expected: clean output; no formatting diff.

- [ ] **Step 1.6: Commit**

```bash
git add src/storage/mod.rs
git commit -m "$(cat <<'EOF'
feat(storage): add legacy memory.md sweep helper (steve-a77e)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Wire the sweep into startup

**Files:**
- Modify: `src/main.rs`

The sweep must run once per process launch, after the logger is initialised (so info-level sweep logs actually appear) and before the first command dispatch.

- [ ] **Step 2.1: Add the sweep call to `main`**

In `src/main.rs`, after the `tracing::info!("steve starting up");` line (currently line 52) and before the `match cli.command { ... }` block (currently line 55), add:

```rust
    // Idempotent sweep of orphan memory.md files left by the removed memory tool.
    let removed = steve::storage::sweep_legacy_memory_files();
    if removed > 0 {
        tracing::info!(count = removed, "removed legacy memory.md files");
    }
```

- [ ] **Step 2.2: Verify it compiles**

Run: `cargo build`
Expected: successful build. If `sweep_legacy_memory_files` isn't exported, check `src/lib.rs` — the `storage` module is already `pub`, so the function should be reachable as `steve::storage::sweep_legacy_memory_files`.

- [ ] **Step 2.3: Run the full test suite**

Run: `cargo test`
Expected: all tests pass (no regressions).

- [ ] **Step 2.4: Commit**

```bash
git add src/main.rs
git commit -m "$(cat <<'EOF'
feat(main): sweep legacy memory.md files on startup (steve-a77e)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Remove memory injection from the system prompt

**Files:**
- Modify: `src/app/prompt.rs`

Three coordinated edits: (a) swap the directive line that tells the LLM to use the memory tool, (b) delete the memory-loading block, (c) delete the now-unused `read_memory_file` helper.

- [ ] **Step 3.1: Replace the directive on line 118**

In `src/app/prompt.rs`, find the string block that currently contains (around line 111–118):

```rust
        identity.push_str("\n\n\
            ## How You Work\n\
            - You can only access files within the project root. All paths are resolved relative to it.\n\
            - **Build mode**: Read tools are auto-approved. Write tools (edit, write, patch) and bash require user permission.\n\
            - **Plan mode**: Read-only. Write tools are unavailable. Use this for analysis and planning.\n\
            - The user sees your tool calls and results in the TUI. Be concise — tool output consumes context window space.\n\
            - When context runs low, the conversation may be automatically compacted into a summary.\n\
            - Use the `memory` tool to persist important discoveries across sessions.");
```

Replace the last line's bullet with the AGENTS.md redirect — final edited block:

```rust
        identity.push_str("\n\n\
            ## How You Work\n\
            - You can only access files within the project root. All paths are resolved relative to it.\n\
            - **Build mode**: Read tools are auto-approved. Write tools (edit, write, patch) and bash require user permission.\n\
            - **Plan mode**: Read-only. Write tools are unavailable. Use this for analysis and planning.\n\
            - The user sees your tool calls and results in the TUI. Be concise — tool output consumes context window space.\n\
            - When context runs low, the conversation may be automatically compacted into a summary.\n\
            - Durable project knowledge lives in AGENTS.md files; suggest updates to the user rather than trying to persist it yourself.");
```

- [ ] **Step 3.2: Delete the memory-loading block**

Find the block (currently lines 124–139) that begins `// Load project memory if it exists (with shared lock for safe concurrent access)` and pushes a `## Project Memory` section onto `parts`. Delete the entire block:

```rust
        // Load project memory if it exists (with shared lock for safe concurrent access)
        let memory_path = self.storage.base_dir().join("memory.md");
        if let Ok(memory) = Self::read_memory_file(&memory_path)
            && !memory.trim().is_empty()
        {
            let truncated = if memory.len() > 2000 {
                let end = memory.floor_char_boundary(2000);
                format!(
                    "{}...\n(use memory tool to read full content)",
                    &memory[..end]
                )
            } else {
                memory
            };
            parts.push(format!("\n## Project Memory\n\n{truncated}"));
        }
```

The surrounding code (before: `parts.push(TOOL_GUIDANCE.to_string());`; after: the task summary injection) stays intact.

- [ ] **Step 3.3: Delete the `read_memory_file` helper**

Find the helper (currently lines 213–222):

```rust
    /// Read the memory file with a shared lock for safe concurrent access.
    pub(super) fn read_memory_file(path: &std::path::Path) -> Result<String, std::io::Error> {
        use std::io::Read;
        let file = std::fs::File::open(path)?;
        file.lock_shared()?;
        let mut content = String::new();
        (&file).read_to_string(&mut content)?;
        let _ = file.unlock();
        Ok(content)
    }
```

Delete it entirely.

- [ ] **Step 3.4: Check for a now-unused import**

The deleted block used `memory.floor_char_boundary(...)`, which comes from `DateTimeExt`/`str::floor_char_boundary`. Grep the file for other `floor_char_boundary` usage:

Run: `rg -n "floor_char_boundary" src/app/prompt.rs`

If there are no remaining callers in this file, check for a corresponding unused import at the top and remove it. If there are other callers, leave the import alone.

- [ ] **Step 3.5: Build and test**

Run: `cargo build`
Run: `cargo test -p steve --lib`
Expected: both succeed.

- [ ] **Step 3.6: Commit**

```bash
git add src/app/prompt.rs
git commit -m "$(cat <<'EOF'
feat(prompt): drop memory auto-injection; redirect to AGENTS.md (steve-a77e)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Delete the memory tool and all its usage sites

**Files:**
- Delete: `src/tool/memory.rs`
- Modify: `src/tool/mod.rs`, `src/tool/agent.rs`, `src/permission/mod.rs`, `src/stream/tools.rs`, `src/stream/phases.rs`, `src/export.rs`, `src/context/cache.rs`, `src/context/compressor.rs`, `src/app/tool_display.rs`, `src/ui/message_block.rs`, `src/ui/message_area/render.rs`, `tests/permission_integration.rs`

This must land in a single commit — partial removal does not compile. Each step below is one concrete edit. Compile after the enum variant and helper are gone; the compiler will guide you through any missed match arm.

### 4a — Delete the tool file and module declaration

- [ ] **Step 4.1: Delete `src/tool/memory.rs`**

```bash
git rm src/tool/memory.rs
```

- [ ] **Step 4.2: Remove the module declaration in `src/tool/mod.rs`**

At line 11 of `src/tool/mod.rs`, delete:

```rust
pub mod memory;
```

### 4b — Remove the `ToolName::Memory` variant and `is_memory()` helper

- [ ] **Step 4.3: Remove the variant from `ToolName`**

In `src/tool/mod.rs`, find the `ToolName` enum (starts at line 80). Delete the `Memory,` line (currently line 98).

- [ ] **Step 4.4: Remove the `is_memory()` helper method**

In `src/tool/mod.rs` (currently lines 148–151), delete:

```rust
    /// Whether this is the memory tool.
    pub fn is_memory(self) -> bool {
        matches!(self, ToolName::Memory)
    }
```

### 4c — Remove the `MemoryAction` enum

- [ ] **Step 4.5: Remove the `MemoryAction` enum in `src/tool/mod.rs`**

Delete the enum (currently lines 346–354):

```rust
/// Actions supported by the `memory` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumString, Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum MemoryAction {
    Read,
    Append,
    Replace,
}
```

- [ ] **Step 4.6: Remove `MemoryAction` round-trip tests**

In the `mod tests` block of `src/tool/mod.rs`, grep for any test that references `MemoryAction`:

Run: `rg -n "MemoryAction" src/tool/mod.rs`

Delete any test functions that exercise `MemoryAction::Read/Append/Replace` round-trips. If none exist, skip.

### 4d — Update exhaustive matches inside `src/tool/mod.rs`

- [ ] **Step 4.7: Fix `path_arg_keys()` (currently line 163)**

In the match arm (currently lines 175–181) that begins with `ToolName::Bash`, remove `| ToolName::Memory`:

```rust
            ToolName::Bash
            | ToolName::Question
            | ToolName::Task
            | ToolName::Webfetch
            | ToolName::FindSymbol
            | ToolName::Agent => &[],
```

- [ ] **Step 4.8: Fix `intent_category()` (currently line 188)**

In the Editing arm (currently lines 198–205), remove `| ToolName::Memory`:

```rust
            ToolName::Edit
            | ToolName::Write
            | ToolName::Patch
            | ToolName::Move
            | ToolName::Copy
            | ToolName::Delete
            | ToolName::Mkdir => IntentCategory::Editing,
```

- [ ] **Step 4.9: Fix `visual_category()` (currently line 215)**

In the Write arm (currently lines 225–232), remove `| ToolName::Memory`:

```rust
            ToolName::Edit
            | ToolName::Write
            | ToolName::Patch
            | ToolName::Move
            | ToolName::Copy
            | ToolName::Delete
            | ToolName::Mkdir => ToolVisualCategory::Write,
```

- [ ] **Step 4.10: Fix `gutter_char()` (currently line 243)**

In the ✎ arm (currently lines 253–260), remove `| ToolName::Memory`:

```rust
            ToolName::Edit
            | ToolName::Write
            | ToolName::Patch
            | ToolName::Move
            | ToolName::Copy
            | ToolName::Delete
            | ToolName::Mkdir => "\u{270e}", // ✎ (1 col)
```

- [ ] **Step 4.11: Fix `tool_marker()` (currently line 272)**

In the ✎ arm (currently lines 282–289), remove `| ToolName::Memory`:

```rust
            ToolName::Edit
            | ToolName::Write
            | ToolName::Patch
            | ToolName::Move
            | ToolName::Copy
            | ToolName::Delete
            | ToolName::Mkdir => "\u{270e}", // ✎
```

- [ ] **Step 4.12: Fix the registry match (currently line 537)**

In `src/tool/mod.rs`, find the registry builder (around line 510 onward). Delete the line:

```rust
                ToolName::Memory => memory::tool(),
```

- [ ] **Step 4.13: Fix the doc comments mentioning memory**

Near the top of `src/tool/mod.rs`:
- Line 39: `/// Read-only + webfetch + lsp — uses tool_read color, · marker.` — no memory mention, leave alone.
- Line 39 below: `/// Write tools + memory — uses tool_write color, ✎ marker.` → change to `/// Write tools — uses tool_write color, ✎ marker.`
- Line 53 (inside `IntentCategory`): `/// File mutations (edit, write, patch, memory).` → change to `/// File mutations (edit, write, patch).`

Run `rg -n "memory" src/tool/mod.rs` to confirm no remaining hits after edits.

### 4e — Update in-file tests in `src/tool/mod.rs`

- [ ] **Step 4.14: Fix the write-marker test array (currently line 706)**

In the `tool_marker_categories_consistent` test (around line 743), delete the `ToolName::Memory,` line from the write-marker array (currently line 714):

```rust
        for t in [
            ToolName::Edit,
            ToolName::Write,
            ToolName::Patch,
            ToolName::Move,
            ToolName::Copy,
            ToolName::Delete,
            ToolName::Mkdir,
        ] {
```

- [ ] **Step 4.15: Fix the `is_memory()` callsites in test loops**

In `src/tool/mod.rs`, `is_memory()` is called at lines 751, 783, 814. Each looks like:

```rust
            } else if t.is_write_tool() || t.is_memory() {
```

Replace each with:

```rust
            } else if t.is_write_tool() {
```

Use `rg -n "is_memory" src/tool/mod.rs` to confirm all three sites are updated.

### 4f — Update external exhaustive matches

- [ ] **Step 4.16: `src/tool/agent.rs` line 83**

Delete the `ToolName::Memory,` line from the allowed-tools list.

- [ ] **Step 4.17: `src/permission/mod.rs` — always-allowed rules**

At line 405, delete:

```rust
        rule(ToolName::Memory, Allow),
```

At line 486 in the test, delete the Memory assertion block:

```rust
        assert_eq!(
            engine.check(ToolName::Memory, None, None),
            PermissionAction::Allow
        );
```

- [ ] **Step 4.18: `src/stream/tools.rs` line 129**

In the match arm covering `Read | Grep | Glob | List | Question | Task | Webfetch | Memory | Symbols | Lsp | FindSymbol`, remove `| ToolName::Memory` so it reads:

```rust
        ToolName::Read
        | ToolName::Grep
        | ToolName::Glob
        | ToolName::List
        | ToolName::Question
        | ToolName::Task
        | ToolName::Webfetch
        | ToolName::Symbols
        | ToolName::Lsp
        | ToolName::FindSymbol => {
```

- [ ] **Step 4.19: `src/stream/phases.rs` line 201**

The partition predicate currently includes `&& !tc.tool_name.is_memory()`. Since `is_memory()` is removed, delete that clause entirely. Before:

```rust
            matches!(tc.action, PermissionAction::Allow)
                && !tc.tool_name.is_write_tool()
                && !tc.tool_name.is_memory()
                && !tc.tool_name.is_task()
                && !matches!(tc.tool_name, ToolName::Question | ToolName::Agent)
                && (tc.tool_name != ToolName::Lsp || is_lsp_read_op(&tc.args))
```

After:

```rust
            matches!(tc.action, PermissionAction::Allow)
                && !tc.tool_name.is_write_tool()
                && !tc.tool_name.is_task()
                && !matches!(tc.tool_name, ToolName::Question | ToolName::Agent)
                && (tc.tool_name != ToolName::Lsp || is_lsp_read_op(&tc.args))
```

Also update the nearby comment (currently lines 192–196) that mentions "memory tool (append action)":

Before:

```rust
    // Partition: auto-allowed read-only tools can run in parallel.
    // Write tools (edit, write, patch), memory tool (append action),
    // task tool (writes to storage), and LSP rename (heavier mutex hold)
    // always go to sequential phase. LSP read operations (diagnostics,
    // definition, references) are safe for parallel execution.
```

After:

```rust
    // Partition: auto-allowed read-only tools can run in parallel.
    // Write tools (edit, write, patch), task tool (writes to storage),
    // and LSP rename (heavier mutex hold) always go to sequential phase.
    // LSP read operations (diagnostics, definition, references) are safe
    // for parallel execution.
```

- [ ] **Step 4.20: `src/export.rs` — line 197 and line 406**

Line 197: in the match arm, remove `| ToolName::Memory`. Before:

```rust
            ToolName::Question | ToolName::Task | ToolName::Memory => String::new(),
```

After:

```rust
            ToolName::Question | ToolName::Task => String::new(),
```

Line 406: in the test `extract_tool_summary_all_tools`, remove Memory from the `matches!` call:

```rust
            if matches!(tool, ToolName::Question | ToolName::Task) {
```

- [ ] **Step 4.21: `src/context/cache.rs` — line 399 and line 432**

At each site, remove `| ToolName::Memory` from the match arm.

- [ ] **Step 4.22: `src/context/compressor.rs` — line 160**

In the `compress_generic` arm, remove `| ToolName::Memory`. After editing:

```rust
        ToolName::Move
        | ToolName::Copy
        | ToolName::Delete
        | ToolName::Mkdir
        | ToolName::Question
        | ToolName::Task
        | ToolName::Webfetch
        | ToolName::Symbols
        | ToolName::Lsp
        | ToolName::FindSymbol
        | ToolName::Agent => compress_generic(content),
```

- [ ] **Step 4.23: `src/app/tool_display.rs` — three sites**

Line 93 — delete the entire `ToolName::Memory` arm:

```rust
        ToolName::Memory => args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
```

Line 269 — remove `| ToolName::Memory` from the None bucket. After editing, that arm reads:

```rust
        ToolName::Read
        | ToolName::Grep
        | ToolName::Glob
        | ToolName::List
        | ToolName::Bash
        | ToolName::Question
        | ToolName::Task
        | ToolName::Webfetch
        | ToolName::Move
        | ToolName::Copy
        | ToolName::Delete
        | ToolName::Mkdir
        | ToolName::Symbols
        | ToolName::Lsp
        | ToolName::FindSymbol
        | ToolName::Agent => None,
```

Line 422 — remove the test assertion:

```rust
        assert_eq!(extract_args_summary(ToolName::Memory, &args), "");
```

- [ ] **Step 4.24: `src/ui/message_block.rs` line 726**

Delete the `ToolName::Memory,` line from the `non_write_tools_stay_collapsed` test array.

- [ ] **Step 4.25: `src/ui/message_area/render.rs` line 1537**

Delete the `ToolName::Memory,` line from the `infer_group_intent_write_tools` test array.

- [ ] **Step 4.26: `tests/permission_integration.rs` — two sites**

Line 204: remove `|| tool == ToolName::Memory` from the Standard profile branch.
Line 273: remove `|| tool == ToolName::Memory` from the Plan mode branch.

### 4g — Verify everything compiles and passes

- [ ] **Step 4.27: Compile**

Run: `cargo build`
Expected: clean build. If it fails, the compiler points to any missed match arm — fix and rerun.

- [ ] **Step 4.28: Grep for residual `Memory` references**

Run: `rg -n "ToolName::Memory|MemoryAction|is_memory\(|pub mod memory|memory::tool|crate::tool::memory" src/ tests/`

Expected: no hits. If any remain, fix them.

- [ ] **Step 4.29: Run all tests**

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 4.30: Clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 4.31: Nightly rustfmt**

Run: `cargo +nightly fmt`
Expected: no diff.

- [ ] **Step 4.32: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
refactor: remove memory tool and all usage sites (steve-a77e)

Deletes tool/memory.rs, the ToolName::Memory variant, the MemoryAction
enum, the is_memory() helper, and every exhaustive-match arm that
referenced Memory. Permission rules, agent allow-lists, stream phase
partitioning, UI rendering, and integration tests are updated.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Update `CLAUDE.md` and `CHANGELOG.md`

**Files:**
- Modify: `CLAUDE.md`, `CHANGELOG.md`

- [ ] **Step 5.1: `CLAUDE.md` — remove `MemoryAction` from the enum list**

In `CLAUDE.md`, find the paragraph that begins `Tool operations use typed enums (` (currently around line 91–95). It lists: `EditOperation, SymbolsOperation, LspOperation, FindSymbolOperation, MemoryAction, TaskAction`. Remove `MemoryAction,` — after editing:

```
Tool operations use typed enums (`EditOperation`, `SymbolsOperation`, `LspOperation`,
`FindSymbolOperation`, `TaskAction`) instead of string matching. Tree-sitter languages use
```

Run: `rg -n "Memory|memory" CLAUDE.md` and remove any remaining mentions of the memory tool from the exhaustive-match-sites list in the "Exhaustive `ToolName` Match Locations" section.

- [ ] **Step 5.2: `CHANGELOG.md` — add an Unreleased / Removed entry**

`CHANGELOG.md` currently begins `## [0.3.0] - 2026-04-06` as the first release heading. Insert a new `## [Unreleased]` section above it with a Removed entry:

```markdown
## [Unreleased]

### Removed

- Memory tool and auto-loaded project memory. Use AGENTS.md for durable project knowledge; beads handles cross-session task continuity. Existing `memory.md` files are cleaned up automatically on first launch.

## [0.3.0] - 2026-04-06
```

- [ ] **Step 5.3: Commit**

```bash
git add CLAUDE.md CHANGELOG.md
git commit -m "$(cat <<'EOF'
docs: drop memory tool references (steve-a77e)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Session close

**Files:** none (git + beads only)

- [ ] **Step 6.1: Final verification**

Run: `cargo test`
Run: `cargo clippy --all-targets -- -D warnings`
Run: `cargo +nightly fmt --check`
Run: `rg -n "ToolName::Memory|MemoryAction|is_memory\(|memory::tool" src/ tests/`

Expected: tests pass, clippy clean, fmt clean, no residual Memory references.

- [ ] **Step 6.2: Close the beads issue**

```bash
bd close steve-a77e --reason="Memory tool removed; orphan memory.md files swept on startup; AGENTS.md is now the pointer for durable project knowledge."
```

- [ ] **Step 6.3: Push everything**

```bash
git pull --rebase
bd dolt push
git push
git status
```

Expected: `git status` reports `up to date with origin/main` (or the working branch's upstream).

---

## Notes for the implementer

- **Line numbers drift.** Every line number in this plan was accurate at spec time; the file may have shifted by the time you read it. Use the surrounding code snippets as anchors and the compiler as your guide.
- **Compiler-driven refactoring is your friend.** Once `ToolName::Memory` is gone, every missed match arm is a compile error. Delete and recompile until clean.
- **Don't introduce `#[allow(...)]`.** If clippy complains, investigate — per `CLAUDE.md`, allows need a justifying comment. Most likely fix is to remove a now-dead helper.
- **Intermediate commits should build.** Tasks 1, 2, 3, 4, 5 each commit compilable, test-passing state. Task 4 is the only multi-file commit because partial removal does not compile.
