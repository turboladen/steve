# Steve: Vision & Roadmap

## Vision

Steve is a terminal-native AI coding agent written in Rust — a personal power tool for developers who want full control over how AI assists their work.

It connects to any OpenAI-compatible LLM provider, streams responses with a tool-calling loop, and gives the developer veto power over every write operation. It's fast, transparent, and built to teach its developers (both human and AI) how agent systems actually work.

### Core Identity

- **Terminal-native** — not a GUI wrapper, not an IDE plugin. Designed for people who live in the terminal.
- **Provider-agnostic** — works with whatever LLM API your company approves. Currently targets Fuel iX and GitHub Copilot (both OpenAI Chat Completions spec), serving GPT, Anthropic, and other model families.
- **Transparent** — every tool call is visible, every permission is explicit, every token is counted. No magic.
- **Cost-conscious** — token usage and costs are tracked, surfaced, and minimized. The agent earns its context budget.
- **A learning vehicle** — the codebase is a reference for building LLM agent systems in Rust. Trade-offs are explored deliberately, not papered over.

### Primary Audience

Built for personal daily use and for teammates who need a company-approved, non-GUI AI coding tool. Not aiming to compete with Cursor or Claude Code broadly — instead, a focused tool that does what its users need and serves as a platform for experimenting with agent techniques.

---

## Roadmap

Milestones are ordered by impact on daily usability. Each builds on the last.

### Milestone 1: Context Resilience

*The agent can work on real codebases without hitting a wall.*

The single biggest barrier to daily use. Steve currently loads full file contents into the conversation, exhausts the context window on multi-file tasks, and (until recently) failed silently when it did.

- **Tool result truncation** — cap large file reads at a configurable max line count; include "truncated at line N of M" indicator so the LLM knows to request specific ranges.
- **System prompt guidance** — instruct the LLM to grep/glob before read, request line ranges for large files, and avoid loading entire files when a targeted read suffices.
- **Proactive context warnings** — surface a visible warning at 60% context usage. Auto-compact at 80% (exists). Hard error at 100% (just shipped).
- **Conversation pruning** — drop older tool results while keeping conversation structure intact. Lighter than full `/compact`; preserves recent context.
- **Project memory** — persistent per-repo scratchpad the agent reads and writes across sessions. Offloads accumulated knowledge from the context window to disk. Reduces the need to re-read files the agent has already understood.

### Milestone 2: Reliability & Polish

*Feels solid for daily use.*

Edge cases, error handling, and data integrity — the things that make you trust a tool enough to rely on it.

- **Session resume** — reopen the app and continue a previous conversation. Messages reloaded from storage, context reconstructed.
- **Error recovery** — network failures, API timeouts, malformed responses handled gracefully with retry or clear messaging. No silent failures.
- **Config validation** — surface clear errors on bad `steve.json` instead of cryptic panics.
- **Token tracking single source of truth** — fix the sidebar vs. context line discrepancy; one authoritative token count.
- **Cost tracking** — per-session cost calculation based on model pricing config. Historical usage persisted to storage.

### Milestone 3: Native Rust Tools

*In-process tools that are faster, portable, and return richer data.*

Steve currently shells out to system binaries for grep, glob, and runs string-based file manipulation. Native Rust implementations eliminate process spawn overhead, remove dependency on system binaries, and enable structured results.

- **Text editing via `ropey`** — rope-based in-memory buffer for edit/write/patch operations. Efficient line indexing, multi-edit batching, clean UTF-8 handling. Replaces read-modify-write String manipulation.
- **Native grep** — in-process search using `grep` crate or similar. Returns structured match results (file, line, column, context) instead of text.
- **Native glob** — `globset` + `walkdir` for file pattern matching.
- **Native tree/list** — directory listing with gitignore awareness via the `ignore` crate.
- **Language-aware file read** — detect binary files, skip generated files, truncate intelligently based on file type.
- **Structured tool results** — tools return typed data internally. Serialized to text for the LLM, but available as structured data for internal use (caching, dedup, display).

### Milestone 4: Agent Intelligence

*The agent makes better decisions about how to explore and modify code.*

- **Dynamic model routing** — configure a "model ladder" (e.g., `gpt-4o-mini` -> `gpt-4o` -> `claude-sonnet`). Agent downgrades to cheaper/faster models for simple tasks (summarization, formatting, simple edits). Routing via heuristics with user override. Each turn shows which model was used.
- **Multi-step planning** — Plan mode becomes a real planning system. Agent creates a structured plan, user approves, agent executes steps. Not just "read-only Build mode."
- **Model-family adaptation** — detect GPT vs Claude vs other behavioral quirks and adjust prompting, tool schemas, and expected output formats.
- **Self-correction** — when a tool call fails, the agent retries with adjusted parameters instead of giving up or asking the user.
- **File relevance scoring** — agent ranks which files matter before reading them, using grep/glob results and project structure.
- **Skills system** — reusable prompt+tool patterns for common workflows (review, refactor, test-write, explain). User-definable.

### Milestone 5: Ecosystem Integration

*Steve becomes a hub for development tools.*

- **MCP support** — Model Context Protocol for connecting external tool servers (databases, APIs, custom tools).
- **LSP integration** — leverage language servers for type-aware operations: go-to-definition, find references, diagnostics. Gives the agent semantic understanding beyond text search.
- **Dev tool integration** — git, test runners, linters, formatters as first-class tools with structured output.
- **Cost dashboard** — TUI view of token usage and costs over time, per-model, per-session. Builds on M2 cost tracking.

---

## Future Considerations

Ideas worth exploring once the core milestones are solid. Documented here with trade-offs so they aren't lost.

### Multi-Agent / Sub-Agent Spawning

**Concept:** The agent autonomously spawns sub-agents for parallel work — e.g., a research agent reads files while the main agent plans, or a review agent checks code while an edit agent modifies it.

**Potential benefits:**
- Faster execution of multi-file tasks (parallel reads, parallel analysis)
- Specialized agents with focused system prompts (reviewer, test-writer, researcher)
- Mirrors how Claude Code and similar tools work internally

**Trade-offs and concerns:**
- **Cost multiplication** — each sub-agent consumes its own tokens. Without clear visibility, costs could spike unexpectedly. This is especially sensitive in a company-approved tool context.
- **Complexity** — agent coordination, result merging, and error handling across parallel streams add significant architectural complexity.
- **User control** — autonomous spawning may feel like the tool is "doing things behind your back." Transparency and configurability (ask/auto/off policy) would be essential.
- **Prerequisite: cost tracking (M2)** — you need to understand your cost model before experimenting with multiplying it.

**Recommendation:** Revisit after M2 (cost tracking) and M4 (agent intelligence) are solid. Start with a simple "spawn one background agent for research" pattern before building a general multi-agent framework.

### Custom Tool Plugins

**Concept:** Let users define tools in config — shell commands wrapped as tools with JSON schemas, descriptions, and argument parsing.

**Why wait:** The tool system needs to stabilize first (M3 native tools, M4 skills). Plugin tools should build on the structured result foundation, not the current string-based approach.

### Eval Harness

**Concept:** Replay a conversation against a different model and compare results. Useful for evaluating model routing decisions and comparing model capabilities.

**Why wait:** Requires session resume (M2) and model routing (M4) as prerequisites. Natural follow-on to those features.

---

## Design Principles

These guide decisions when the roadmap doesn't have a specific answer.

1. **Show, don't hide.** Every LLM call, tool execution, and token spent should be visible to the user. Transparency builds trust and teaches how agents work.

2. **Earn context budget.** Context window space is the scarcest resource. Every token in the conversation should justify its presence. Prefer targeted reads over full files, summaries over raw output, structured data over text dumps.

3. **Fail loudly.** When something goes wrong — context exhausted, tool failed, API errored — tell the user immediately with actionable guidance. Never fail silently.

4. **Ship the simple version.** Implement the simplest version that solves the problem. Optimize and generalize only when real usage reveals the need. YAGNI applies to agent architectures too.

5. **Rust is a feature.** Performance, safety, single-binary distribution, and in-process tools are competitive advantages. Lean into them.

6. **Cost is a first-class concern.** Token usage, model selection, and cost tracking aren't afterthoughts. In a company-approved tool, cost visibility is table stakes.
