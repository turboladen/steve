# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.2.0]: https://github.com/turboladen/steve/compare/0.1.0...v0.2.0
[0.1.0]: https://github.com/turboladen/steve/releases/tag/0.1.0
