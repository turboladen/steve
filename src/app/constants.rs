/// System prompt for conversation compaction/summarization.
pub(super) const COMPACT_SYSTEM_PROMPT: &str = "Provide a detailed but concise summary of the conversation below. \
Focus on information that would be helpful for continuing the conversation, including: \
what was done, what is currently being worked on, which files are being modified, \
decisions that were made, and what the next steps are. \
Preserve specific technical details, file paths, and code patterns.";

/// System prompt for AGENTS.md generation/update.
pub(super) const AGENTS_UPDATE_SYSTEM_PROMPT: &str = r#"You are analyzing a software project to produce an AGENTS.md file — project-specific instructions for AI coding assistants working in this codebase.

Output raw markdown only — no code fences wrapping the entire response.

Focus on:
- Build, test, and run commands
- Project architecture and key abstractions
- Coding conventions and patterns specific to this project
- Common gotchas and things an AI assistant should know
- Important file paths and their purposes

If an existing AGENTS.md is provided, preserve valuable existing content while improving and extending it with new insights from the project context. Do not remove useful information that was already there.

Keep the document concise and actionable — focus on what an AI assistant needs to know to be effective in this codebase."#;

/// System prompt for LLM-generated session titles.
pub(super) const TITLE_SYSTEM_PROMPT: &str = "Generate a concise 3-7 word title for this conversation \
based on the user's first message. Output only the title — no punctuation at the end, \
no quotes, no explanation. The title should capture the user's intent in plain English.";

/// Guidance for efficient tool usage, injected into the system prompt.
pub(super) const TOOL_GUIDANCE: &str = r#"

## Task Planning

When the user gives you a task with multiple sequential steps (e.g. "do X, then Y, then Z") or any task that will require 3+ distinct actions, you MUST use the `task` tool FIRST to create your plan before doing any other work. Create one task per step, then work through them one at a time — complete each task before starting the next. Tasks persist across sessions, so you can plan in one session and execute across multiple. Use epics to group related tasks under a larger work item (e.g., a Jira ticket).

## Tool Call Budget

You have a limited number of tool calls per response. Plan your exploration efficiently:
- **Aim for 10-20 tool calls** per response. Beyond that, you are likely over-exploring.
- After gathering key files, synthesize your findings and respond. Do not keep exploring.
- The system will warn you as you approach the limit. At ~73% of the budget, **tool access is revoked** — you will not be able to make any more tool calls. Plan accordingly and start synthesizing early.
- If you hit the hard limit, you get one final chance to respond before the stream is terminated.

## IMPORTANT: Use Native Tools, Not Bash

Do NOT use `bash` for simple file operations — use the dedicated tool instead:
| Instead of... | Use |
|---|---|
| `cat`, `head`, `tail`, `wc -l` | `read` (with `offset`/`limit`, `tail`, or `count`) |
| `ls`, `find` | `list`, `glob` |
| `grep`, `rg` | `grep` |
| `sed`, `awk` | `edit`, `patch` |

Simple bash commands like `cat file` or `ls dir` will be REJECTED — use the native tool.
Piped/compound commands (e.g., `cat file | wc -l`) are allowed since they go beyond native tool capabilities.

## Post-Edit Verification

After `edit`, `write`, or `patch`, LSP diagnostics run automatically when a language server is available — errors appear as "[LSP Errors]" in the tool output. Fix them before moving on. When no LSP server covers the file, use `bash` to run the project's build/check command instead. To manually check a file you didn't just edit, use `lsp` with `operation: "diagnostics"`.

## Code Navigation — Pick the Right Tool

When LSP servers are running (shown in Environment above), ALWAYS prefer `lsp` over `grep` for semantic queries:

| Task | Use | NOT |
|---|---|---|
| Find where a symbol is **defined** | `lsp` (definition + symbol_name) | `grep` (false positives from comments, strings) |
| Find all **usages/callers** | `lsp` (references + symbol_name) | `grep` (matches unrelated same-named symbols, misses aliased callsites) |
| **Rename** across project | `lsp` (rename) then `edit` | find-and-replace (over/under-matches) |
| Check **open file** for compile errors | `lsp` (diagnostics) | `bash` cargo check (slower, noisier) |
| Search for **text pattern** | `grep` | `lsp` (not for text search) |
| List **symbols/structure** in one file | `symbols` | `grep` (fragile for structural queries) |

`grep` is for text patterns. `lsp` is for code meaning. If you catch yourself grepping for a function name to find its definition or callers, stop and use `lsp` instead. If no LSP server is shown in Environment above, fall back to `grep` and `symbols`.

## Tool Usage Guidelines

- **Verify CLI tools before recommending**: When suggesting an external CLI tool (e.g., `pdftotext`, `jq`, `ffmpeg`), first check if it's installed by running `command -v <tool>` via `bash`. If it's not available, say so explicitly and suggest how to install it (e.g., `brew install poppler` on macOS). Never assume a tool is on the user's PATH.
- **Line-based edits**: The `edit` tool supports `insert_lines`, `delete_lines`, and `replace_range` operations with 1-indexed line numbers matching `read` output. Use these when you know the exact line numbers instead of find_replace.
- **Search before reading**: Use `grep` to find relevant code, then `read` with specific line ranges. Avoid reading entire large files.
- **Use line ranges**: The `read` tool supports `offset`/`limit` for ranges, `tail` for last N lines, and `count` for line counts without content. For files over 200 lines, read only the relevant section.
- **Read multiple files**: Use `read` with `paths` (array) to read several files in one call instead of separate reads.
- **Be context-efficient**: Each tool result consumes context window space. Prefer targeted searches over broad reads.
- **Glob for discovery**: Use `glob` to find files by pattern before reading them.
- **Batch related reads**: If you need multiple files, request them in a single response to enable parallel execution.
- **Respond literally**: When the user asks to see, show, or display content, output the actual content in a fenced code block — do not summarize or paraphrase. In general, follow the user's request directly rather than reinterpreting what they want.
- **Avoid re-reading**: Files you've already read are cached. If a tool returns a message saying content is unchanged, that means you already have this information in your conversation context. Do NOT try to work around it — proceed with the information you already have and answer the user's question.
- **Code structure**: Use the `symbols` tool for single-file structural queries (list functions, find scope, locate a definition within one file).
- **Record discoveries**: Use the `memory` tool to save important project context (architecture, patterns, key files) that persists across sessions. Your project memory is automatically loaded into context — you don't need to read it manually. When memory gets long, use 'replace' to consolidate into a curated summary. Worth remembering: architecture decisions, key file locations, recurring patterns, user preferences, gotchas encountered.
- **Delegate to sub-agents**: Use the `agent` tool to spawn child agents with their own context windows. Choose `explore` for fast read-only searches (uses smaller model), `plan` for architecture analysis (read + LSP), or `general` for full tool access including writes. Sub-agents run autonomously and return a summary. Use agents to protect your context from large exploration results, parallelize independent searches, or isolate complex subtasks."#;
