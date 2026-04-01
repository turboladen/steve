# Code Review Agent — Design Spec

## Overview

Built-in code review agent that spawns multiple LLM reviewers in parallel, each with a
different model and specialized review focus. Results render sequentially in the conversation
as labeled blocks with graded findings.

## Motivation

Different models catch different issues. Running Opus and GPT-4o (or any N models) on the
same diff produces complementary feedback — each model has blind spots the other covers. This
is a batteries-included feature: the agent is built into Steve, configured via the existing
config file, and invoked with a slash command.

## Command Interface

```
/code-review              → reviews all uncommitted changes (staged + unstaged)
/code-review src/foo.rs   → reviews only that file's uncommitted changes
/code-review src/         → reviews uncommitted changes in that directory
```

**Flow:**

1. User types `/code-review`
2. Steve gathers the diff (`git diff HEAD`, or scoped to the path argument)
3. If diff is empty → "Nothing to review — no uncommitted changes"
4. If `agents.code-review` not configured → "Code review agent not configured — add
   `agents.code-review.reviewers` to your config, then run `/reload`"
5. Spawn N reviewer sub-agents in parallel
6. Show loading indicator with reviewer count
7. As each reviewer finishes, render its output as a labeled block in the conversation

## Config Schema

New `agents` section in config (global or project-level):

```jsonc
{
  "agents": {
    "code-review": {
      "reviewers": [
        { "model": "anthropic/claude-opus", "label": "Opus" },
        { "model": "openai/gpt-4o", "label": "GPT-4o" }
      ]
    }
  }
}
```

- `reviewers`: list of 1–N reviewer entries (required, non-empty)
- `model`: required, `"provider/model"` format — must resolve in `ProviderRegistry`
- `label`: optional, defaults to the model ref if omitted
- If `agents.code-review` is absent or `reviewers` is empty, `/code-review` shows a
  configuration hint
- Config is re-read on `/reload` (see Config Reload section)

## Architecture

### New Code

**`agents/` module (new)**

- `agents/mod.rs` — public types (`ReviewResult`, `ReviewerConfig` re-export)
- `agents/review.rs` — `run_code_review()` orchestration function:
  - Takes `AgentSpawner`, list of `ReviewerConfig`s, diff text, optional AGENTS.md content
  - Spawns N `run_sub_agent` calls concurrently via `futures::future::join_all`
  - Each sub-agent gets a specialized system prompt, the diff as user message, and
    read-only + LSP + MCP tools
  - Returns `Vec<ReviewResult>` (label + text + usage)

**Config types (in `config/mod.rs`)**

```rust
struct AgentsConfig {
    code_review: Option<CodeReviewAgentConfig>,
}

struct CodeReviewAgentConfig {
    reviewers: Vec<ReviewerConfig>,
}

struct ReviewerConfig {
    model: String,
    label: Option<String>,
}
```

Added as `agents: Option<AgentsConfig>` on `Config`. Merges like other config sections
(project overrides global).

**Command variants**

- `Command::CodeReview(Option<String>)` — parsed from `/code-review [path]`
- `Command::Reload` — parsed from `/reload`

**Event variant**

- `AppEvent::ReviewFinish { results: Vec<ReviewResult> }`

### Modified Existing Code

**`run_sub_agent` in `stream/agent.rs`**

Add a `model_override: Option<String>` parameter so the review orchestrator can specify the
model per reviewer, rather than deriving it from `AgentType`.

**`AgentType` in `tool/agent.rs`**

New `Review` variant with:
- Tools: read-only set + LSP + MCP (same as `Plan` plus MCP inheritance)
- Custom system prompt (see Review System Prompt section)

**Exhaustive match locations** (per CLAUDE.md)

All existing `AgentType` match arms need the new `Review` variant added.

## Review Sub-agents

### Tools Available

Each reviewer gets:
- `read`, `grep`, `glob`, `list`, `symbols`, `lsp` — read-only exploration + semantic analysis
- MCP tools — inherited from parent session (same as `AgentType::General`)

This lets reviewers trace references, find callers, check diagnostics, and access external
context without being able to modify anything.

### System Prompt — Specialized Review Focuses

Instead of giving every reviewer the same generic prompt, each gets a specialized focus:

1. **Bugs & Logic** — look for bugs, logic errors, security issues, edge cases. Use LSP to
   trace callers and check impact. Use tools to read surrounding code for context.
2. **Conventions & Quality** — check adherence to AGENTS.md conventions, code quality,
   naming, test coverage, project patterns. Reference AGENTS.md content provided in the prompt.

Focus assignment by reviewer count:
- **1 reviewer:** gets a combined prompt covering both Bugs & Logic and Conventions & Quality
- **2 reviewers:** one gets Bugs & Logic, the other gets Conventions & Quality
- **3+ reviewers:** first two get the specialized focuses, additional reviewers get a general
  "comprehensive review" prompt covering both angles

### Context Provided to Reviewers

- The diff (as the user message, prefixed with "Review the following code changes:")
- AGENTS.md content (if present) — so reviewers understand project conventions
- Project root path — implicit via tool context, so they can explore surrounding code

### Finding Format

Each finding must include all of the following (the system prompt enforces this):

1. **File path + line number** — exact location, e.g. `src/stream/mod.rs:142`. Use `grep`,
   `read`, or `lsp` to verify line numbers against the current file, not just diff offsets.
2. **Code snippet** — the problematic line(s) quoted verbatim from the source.
3. **What's wrong** — clear explanation of the issue.
4. **Impact** — what breaks or could break if unfixed. Use `lsp` and `grep` to trace callers
   and identify concrete impact (e.g. "callers X and Y pass None here, which would panic").
5. **Suggested fix** — concrete code change or specific description of what to do. Should be
   actionable enough that a coding agent or human can apply it without further research.

### Grading System

Each finding is graded on two axes:

- **Severity**: Critical / Important / Suggestion
- **Confidence**: 0–100 score indicating likelihood this is a real issue vs. false positive

All findings are presented regardless of score — no automatic filtering. The user decides
what to act on.

Examples of false positives reviewers should score low:
- Pre-existing issues not introduced by this diff
- Issues a linter/compiler would catch
- Pedantic nitpicks a senior engineer wouldn't flag
- General quality concerns not called out in AGENTS.md

## Event Flow & Rendering

1. `/code-review` parsed → `Command::CodeReview(path)`
2. Handler gathers diff, validates config, checks diff non-empty
3. Sets `is_loading = true`, shows activity "Running code review (N reviewers)..."
4. Spawns async task running N `run_sub_agent` calls in parallel
5. Sub-agent `AgentProgress` events forwarded to parent (user sees tool activity per reviewer)
6. All reviewers finish → `AppEvent::ReviewFinish { results }`
7. Handler renders labeled blocks:

```
── Review: Opus ──────────────────────────────
### Findings

**[Critical, 95]** Possible panic in `stream/mod.rs:142`
> let value = map.get(key).unwrap();
**Impact:** `handle_event()` in `app/event_loop.rs:88` passes user-supplied keys —
a missing key panics the stream task silently.
**Fix:** Use `map.get(key).ok_or_else(|| anyhow!("unknown key: {key}"))?`

**[Suggestion, 60]** Consider extracting helper in `app/input.rs:88`
> if self.is_loading || self.streaming_active || self.pending_permission.is_some() {
**Impact:** This guard is duplicated in 3 command handlers.
**Fix:** Extract `fn is_busy(&self) -> bool` and call it from each handler.

── Review: GPT-4o ────────────────────────────
### Findings
...
```

**Cancellation:** Cancel token is a child of the main session's token. Ctrl+C or starting a
new message cancels all reviewers.

**In-session:** Review output appears in the current conversation as labeled message blocks.
The user can follow up conversationally ("fix issue #3", "explain that second point").

## Config Reload

New `/reload` command:

- Re-runs `config::load()` with current project root
- Updates `self.config` on `App`
- Rebuilds `ProviderRegistry` if providers changed
- Picks up new `agents` config
- Shows system message: "Config reloaded" (plus any non-fatal warnings)
- Does **not** affect current session's model or conversation history

No file watchers or hot-reload — edit the file, type `/reload`, done.

## Out of Scope (MVP)

- Auto-detection / suggestion ("this looks like a code review request")
- User-defined agents beyond code-review
- User-defined categories or review focuses
- Confidence-based filtering (grading is present, filtering is not)
- Background/detached review execution
- Side-by-side or merged review output
- Reviewing specific commits or ranges (`HEAD~3..HEAD`)
