# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-04-06

### Added

- MCP client support with remote server OAuth2 authentication
- `/mcp` tabbed overlay for browsing MCP servers, tools, resources, and prompts
- Parallel sub-agent execution (Explore/Plan types run concurrently via `tokio::spawn`)
- Parallel agent results stream as they complete, not in call order
- Tree-sitter parsers for bash, fish, yaml, hcl, lua, and css
- Clickable URLs in message area (cmd-click)
- Colorblind-safe theme palette (photo-derived)
- Model picker at startup when no model is configured
- OSC 7 CWD reporting on startup
- CI pipeline with cargo check, nextest, clippy, and fmt
- Branded OAuth callback pages and README with Steve logo
- 80+ new unit tests across app, stream, and tool modules

### Changed

- Replaced compressor heuristics with tree-sitter for smarter output compression
- Replaced language name strings with `TreeSitterLang` enum
- Replaced action/operation strings with typed enums (`EditOperation`, `MemoryAction`, `TaskAction`, `SymbolsOperation`, `LspOperation`)
- Split `app.rs` into focused submodules (event_loop, key_handling, input, commands, session, prompt, context, helpers, tool_display)
- Split `stream.rs` into submodules (agent, tools, recovery, phases) with extracted tool execution phases
- Split `message_area` and `sidebar` into mod.rs + render.rs submodules
- Restructured `mcp/`, `lsp/`, `config/`, `cli/` — inlined types, extracted server/manager modules
- Compact sidebar layout (saves ~5 vertical lines), merged Changes into Git section
- Replaced `which` shell-out with `which` crate for binary discovery
- Used `workspace_folders` instead of deprecated `root_uri` for LSP init
- Enabled `clippy::cargo` lint, fixed all 105 clippy warnings
- Added `rustfmt.toml` with `imports_granularity = "Crate"`
- Updated jsonc-parser 0.32.3, ratatui-textarea 0.8 → 0.9, rmcp to 1.3
- Updated actions/checkout from v4 to v6

### Fixed

- Interjection response appended to previous assistant message instead of new block
- Agent progress routed to wrong tool call (now matched by `call_id`)
- Config model ignored when resuming session (used stale session model)
- MCP tool identity shown as 'bash' placeholder in permission prompts
- Mtime-less cache entries not invalidated between user turns
- OAuth retry when stored credentials are rejected
- Tab completed commands but also executed them
- Raw markdown preserved when copying mouse-selected text
- Synchronous log writer to prevent empty log files
- Percent-encoded OSC 7 path with edge-case handling
- Badge contrast for light theme, explicit dark text on Build/Plan mode badge
- Interjection channel drained before exiting text-only stream responses
- Config tests isolated from real global config
- Scroll clamp, overlay mutual exclusion
- Unsafe unwraps replaced with proper error handling
- Localhost fallback when `set_host` fails

## [0.2.0] - 2026-03-21

### Added

- Agent tool for delegating subtasks to child agents (Explore/Plan/General types)
- Elapsed timer display (per-activity spinner + total in input context line)
- Session picker overlay (replaces numeric session browser)
- Nested AGENTS.md support with walk-up discovery from CWD to project root
- `/agents-update` command for LLM-powered AGENTS.md generation
- Project-root-aware path normalization for permission rules
- Enhanced read tool: tail, count, multi-file, and max_lines modes
- Sidebar: path shortening, alphabetical sorting, git status on own line, tasks 3-line layout
- Integration tests for native tool redirect system

### Changed

- Replaced `futures` crate with `tokio-stream` for StreamExt
- Replaced `fs2` with native `File::lock()` (Rust 2024)
- Removed `async-trait` dependency (manual ChatStreamProvider desugar)
- LSP sidebar shows binary name instead of language name

### Fixed

- Context exhaustion recovery via tool stripping + deferred compression
- Output truncation vs context exhaustion distinction on `finish_reason=Length`
- Tool loop stalls with directive cache messages and stuck detection
- Agent tool deadlock + defense-in-depth
- Sub-agent token usage accumulation for correct cost display
- Character-level selection highlighting (was whole-span)
- Duplicate keystroke processing from non-Press key events
- Eager context_window initialization for border color shifting
- Dot-directories included in @ file completion

## [0.1.0] - 2026-03-12

### Added

- TUI with ratatui — streaming responses, tool calls, sidebar
- Tool system: read, grep, glob, list, edit, write, patch, move, copy, delete, mkdir, bash,
  webfetch, memory, question, task, symbols, lsp
- Permission system with Build/Plan modes and Trust/Standard/Cautious profiles
- Context management: compressor, tool result cache, auto-compact at 80%
- LSP integration (Rust, Python, TypeScript, JSON, Ruby)
- Task system with persistent storage, CLI subcommands, and sidebar display
- Session management with browsing, title generation, cost tracking
- Ambient context pressure (border color shifting)
- Intent indicators per tool group
- Sidebar: changeset panel, sessions, tasks, LSP status
- Fenced code block rendering with syntax highlighting
- @ file references with autocomplete
- Click-drag text selection with clipboard copy
- Markdown formatting in assistant messages
- Interactive model picker and question tool dialogs
- Configurable permission profiles with path-based rules and persistent grants
- Usage analytics with SQLite persistence and `steve data` browser
- Multi-line input with Shift+Enter, command autocomplete
- Inline diff preview on permission prompts
- Retry transient API errors with exponential backoff

### Fixed

- Tool loop feedback loops (compressor/cache cycle)
- Token pipeline (prompt vs cumulative metrics)
- Scroll direction, scroll overflow
- Truncated tool calls on `finish_reason=Length`

[0.3.0]: https://github.com/turboladen/steve/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/turboladen/steve/compare/0.1.0...v0.2.0
[0.1.0]: https://github.com/turboladen/steve/releases/tag/0.1.0
