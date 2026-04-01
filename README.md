# Steve

<p align="center">
  <img src="i-am-steve.png" alt="Steve" width="200">
</p>

A Rust TUI AI coding agent with built-in LSP integration, tree-sitter analysis, and a layered
permission system. Steve connects to any OpenAI-compatible LLM API, streams responses
token-by-token, and provides an 18-tool calling loop that lets the LLM read, search, edit, and
execute code within your project.

Built with [ratatui](https://github.com/ratatui/ratatui) and inspired by
[opencode](https://opencode.ai).

## How Steve is Different

- **Rust-native TUI** — ratatui + crossterm, no Electron or browser overhead, ~40 source files
- **Any OpenAI-compatible API** — OpenAI, Ollama, OpenRouter, local models, same config format
- **Built-in LSP client** — custom JSON-RPC transport for Rust, Python, TypeScript, JSON, and Ruby;
  auto-detects from project markers
- **Bundled tree-sitter grammars** — 12 languages for structural code analysis (list symbols, find
  scope, go to definition)
- **Layered permission system** — 3 profiles, path-based glob rules, persistent grants, inline diff
  preview
- **Smart context management** — auto-compact at 80%, tool result cache with mtime invalidation,
  feedback loop detection
- **Two-phase tool execution** — read-only tools run in parallel, writes execute sequentially
- **Persistent task tracking** — tasks, bugs, and epics survive across sessions; CLI and TUI
  interfaces
- **Local-first storage** — flat JSON files with atomic writes, no cloud dependency

## Quick Start

### Prerequisites

- Rust (edition 2024)
- An API key for any OpenAI-compatible LLM provider

### Build & Run

```bash
cargo build            # Debug build
cargo build --release  # Release build
cargo run              # Run (requires .steve.jsonc or global config)
```

### Configuration

Create a `.steve.jsonc` in the root of the project you want to work in (or a global
`~/.config/steve/config.jsonc`). The config defines your LLM providers, models, and which
environment variable holds each provider's API key.

**Minimal example — OpenAI:**

```jsonc
{
  "model": "openai/gpt-4o",
  "providers": {
    "openai": {
      "base_url": "https://api.openai.com/v1",
      "api_key_env": "OPENAI_API_KEY",
      "models": {
        "gpt-4o": {
          "id": "gpt-4o",
          "name": "GPT-4o",
          "context_window": 128000,
          "capabilities": { "tool_call": true, "reasoning": false },
        },
      },
    },
  },
}
```

**Using a different provider (e.g. Ollama, OpenRouter, any OpenAI-compatible API):**

```jsonc
{
  "model": "ollama/llama3",
  "providers": {
    "ollama": {
      "base_url": "http://localhost:11434/v1",
      "api_key_env": "OLLAMA_API_KEY",
      "models": {
        "llama3": {
          "id": "llama3",
          "name": "Llama 3",
          "context_window": 8192,
          "capabilities": { "tool_call": true, "reasoning": false },
        },
      },
    },
  },
}
```

**Multiple providers with permissions:**

```jsonc
{
  "model": "openai/gpt-4o",
  "small_model": "openai/gpt-4o-mini", // used for /compact and title generation
  "auto_compact": true, // auto-compact at 80% context usage (default: true)
  "permission_profile": "standard", // "trust", "standard" (default), or "cautious"
  "allow_tools": ["edit", "bash"], // auto-allow these tools regardless of profile
  "providers": {
    "openai": {
      "base_url": "https://api.openai.com/v1",
      "api_key_env": "OPENAI_API_KEY",
      "models": {
        "gpt-4o": {
          "id": "gpt-4o",
          "name": "GPT-4o",
          "context_window": 128000,
          "capabilities": { "tool_call": true, "reasoning": false },
        },
        "gpt-4o-mini": {
          "id": "gpt-4o-mini",
          "name": "GPT-4o Mini",
          "context_window": 128000,
          "capabilities": { "tool_call": true, "reasoning": false },
        },
      },
    },
    "anthropic": {
      "base_url": "https://openrouter.ai/api/v1",
      "api_key_env": "OPENROUTER_API_KEY",
      "models": {
        "claude-sonnet": {
          "id": "anthropic/claude-3.5-sonnet",
          "name": "Claude 3.5 Sonnet",
          "context_window": 200000,
          "capabilities": { "tool_call": true, "reasoning": false },
        },
      },
    },
  },
}
```

Model references use `"provider_id/model_id"` format everywhere — in the config, in commands, and
internally.

Then set the corresponding environment variable:

```bash
export OPENAI_API_KEY="sk-..."
```

### Config Reference

| Field                     | Required | Description                                                                     |
| ------------------------- | -------- | ------------------------------------------------------------------------------- |
| `model`                   | Yes      | Default model in `provider_id/model_id` format                                  |
| `small_model`             | No       | Model for `/compact` summarization and title generation (falls back to `model`) |
| `auto_compact`            | No       | Auto-compact when context reaches 80% (default: `true`)                         |
| `permission_profile`      | No       | `"trust"`, `"standard"` (default), or `"cautious"`                              |
| `allow_tools`             | No       | Tools to auto-allow regardless of profile (e.g., `["edit", "bash"]`)            |
| `permission_rules`        | No       | Path-based permission rules (see [Permission System](#permission-system))       |
| `theme`                   | No       | `"auto"` (default), `"dark"`, or `"light"`                                      |
| `providers`               | Yes      | Map of provider configurations                                                  |
| `providers.*.base_url`    | Yes      | OpenAI-compatible API endpoint                                                  |
| `providers.*.api_key_env` | Yes      | Name of the env var holding the API key                                         |
| `providers.*.models`      | Yes      | Map of available models for this provider                                       |

## Usage

### Commands

| Command                           | Action                                       |
| --------------------------------- | -------------------------------------------- |
| `/new`                            | Start a new session                          |
| `/rename <title>`                 | Rename current session                       |
| `/model <ref>`                    | Switch model (e.g., `/model openai/gpt-4o`)  |
| `/models`                         | List available models                        |
| `/compact`                        | Compact conversation to free context window  |
| `/sessions`                       | Browse past sessions                         |
| `/tasks`                          | List all tasks                               |
| `/task-new <title>`               | Create a task                                |
| `/task-done <id>`                 | Complete a task                              |
| `/task-show <id>`                 | Show task details                            |
| `/task-edit <id> <field>=<value>` | Edit a task                                  |
| `/epics`                          | List epics                                   |
| `/epic-new <title>`               | Create an epic                               |
| `/diagnostics`                    | Show health dashboard (LSP, config, storage) |
| `/init`                           | Create AGENTS.md in project root             |
| `/export-debug`                   | Export session as markdown for debugging     |
| `/help`                           | Show help                                    |
| `/quit` or `/exit`                | Quit                                         |

### Keybindings

| Key             | Action                                            |
| --------------- | ------------------------------------------------- |
| Enter           | Send message                                      |
| Shift+Enter     | Insert newline                                    |
| Tab             | Accept autocomplete / toggle Build–Plan mode      |
| Up/Down         | Navigate autocomplete / scroll messages           |
| PageUp/PageDown | Scroll messages one page                          |
| Ctrl+C          | Cancel stream (first press) / quit (second press) |
| Ctrl+B          | Toggle sidebar (auto → hide → show → auto)        |
| Mouse wheel     | Scroll messages                                   |
| Click+drag      | Select text (auto-copies to clipboard)            |

### Agent Modes

- **Build mode** (default) — The LLM can read, write, and execute code. Write/execute tools require
  your permission (varies by profile).
- **Plan mode** — The LLM can explore and analyze your code but cannot modify it. Write tools and
  Agent are denied; Bash and Webfetch still require permission. Press Tab to toggle.

The input bar shows the current mode, working directory, token usage, and context pressure
percentage.

## Tools

Steve gives the LLM access to 18 tools, grouped by category.

### Reading & Exploration

| Tool       | Description                                                                                             |
| ---------- | ------------------------------------------------------------------------------------------------------- |
| `read`     | Read file contents with optional line range                                                             |
| `grep`     | Search file contents with regex                                                                         |
| `glob`     | Find files by glob pattern                                                                              |
| `list`     | List directory contents with configurable depth                                                         |
| `symbols`  | Tree-sitter structural analysis — list symbols, find scope, go to definition (12 languages)             |
| `lsp`      | Language Server Protocol — diagnostics, go to definition, find references, rename preview (5 languages) |
| `webfetch` | Fetch a URL and return content as plain text                                                            |

### Writing & Modification

| Tool     | Description                                                                          |
| -------- | ------------------------------------------------------------------------------------ |
| `edit`   | Edit a file (find/replace, multi-replace, insert lines, delete lines, replace range) |
| `write`  | Write or overwrite a file (creates parent directories)                               |
| `patch`  | Apply a unified diff patch to a file                                                 |
| `move`   | Move or rename a file or directory                                                   |
| `copy`   | Copy a file or directory (recursive for directories)                                 |
| `delete` | Delete a file or directory                                                           |
| `mkdir`  | Create a directory and parent directories                                            |
| `memory` | Read, append, or replace the agent's persistent memory file                          |

### Execution & Interaction

| Tool       | Description                                                           |
| ---------- | --------------------------------------------------------------------- |
| `bash`     | Execute shell commands (rejects commands that duplicate native tools) |
| `question` | Ask the user a question and wait for a response                       |
| `task`     | Create, update, complete, list, or delete tasks and epics             |

## Features

### Permission System

Steve uses a layered permission system to control tool access:

**Profiles** determine the baseline behavior in Build mode:

| Profile              | Auto-allowed                                  | Requires permission                      |
| -------------------- | --------------------------------------------- | ---------------------------------------- |
| `trust`              | All tools                                     | None                                     |
| `standard` (default) | Read tools, LSP, Memory, Task, Question       | Write tools, Bash, Webfetch, Agent       |
| `cautious`           | Question, Task                                | Everything else (including reads)        |

**Modes** control what the LLM can do:

| Mode                 | Auto-allowed                            | Ask              | Denied              |
| -------------------- | --------------------------------------- | ---------------- | ------------------- |
| **Build** (default)  | Per profile above                       | Per profile above | None               |
| **Plan**             | Read tools, LSP, Memory, Task, Question | Bash, Webfetch   | Write tools, Agent  |

Note: Plan mode rules are the same regardless of profile — the profile only affects Build mode.

**Path rules** provide fine-grained control over specific paths:

```jsonc
{
  "permission_rules": [
    { "tool": "edit", "pattern": "src/**", "action": "allow" },
    { "tool": "edit", "pattern": "*.lock", "action": "deny" },
    { "tool": "*", "pattern": "!project", "action": "deny" },
  ],
}
```

The special pattern `!project` matches any path that resolves outside the project root — useful for
preventing the LLM from reading or writing files outside your project. Paths are normalized against
the project root before matching, so both relative (`src/main.rs`) and absolute
(`/Users/.../src/main.rs`) paths work correctly with glob patterns. Note: for `move`/`copy` tools,
permission checks apply to the destination path only.

Rules are evaluated first-match-wins: path rules take priority over `allow_tools`, which takes
priority over profile defaults.

**Persistent grants**: When prompted, press `a` to allow a tool permanently — the grant is saved to
`.steve.jsonc` and persists across sessions.

**Diff preview**: Permission prompts for write tools show an inline diff of the proposed change.

### LSP Integration

Steve includes a built-in LSP client with custom JSON-RPC transport over stdio. Language servers are
auto-detected from project marker files and resolved from PATH.

| Language   | Server                       |
| ---------- | ---------------------------- |
| Rust       | `rust-analyzer`              |
| Python     | `pyright` / `pylsp`          |
| TypeScript | `typescript-language-server` |
| JSON       | `vscode-json-languageserver` |
| Ruby       | `solargraph` / `ruby-lsp`    |

**Operations**: `diagnostics` (compiler errors/warnings), `definition` (go to definition),
`references` (find all references), `rename` (preview rename refactoring).

### Tree-sitter Symbols

Bundled tree-sitter grammars provide structural code analysis without requiring external tools.

**Supported languages** (12): Rust, Python, JavaScript, TypeScript, TSX, Go, C, C++, Java, Ruby,
TOML, JSON.

**Operations**: `list_symbols` (functions, structs, classes, etc.), `find_scope` (enclosing symbol
at a line), `find_definition` (locate a symbol's definition).

### Context Management

- **Auto-compact**: Triggers at 80% context window usage, using the `small_model` for summarization
- **Tool result cache**: Session-scoped, path-normalized keys, auto-invalidates on file mtime
  changes
- **Compressor**: Replaces already-seen tool results with compact summaries to free context
- **Feedback loop detection**: After repeated cache hits, returns a short summary to break read
  loops

The input bar border color shifts from gray through amber to red as context pressure increases (40%
→ 60% → 80%).

### Task & Epic Tracking

Tasks, bugs, and epics persist across sessions in local storage.

- **ID format**: `{project}-{kind}{hex}` (e.g., `steve-ta3f0` for a task, `steve-e12b` for an epic)
- **TUI**: Use the `task` tool or `/tasks`, `/task-new`, `/epics` commands
- **CLI**: `steve task list`, `steve task new "title"`, `steve task done <id>`
- **Sidebar**: Active tasks shown in the Todos tab

### Diagnostics

The `/diagnostics` command shows a health dashboard covering LSP server status, configuration
validation, storage state, and detected project languages.

## Development

```bash
cargo check                        # Type-check
cargo test                         # Run all 650+ tests
RUST_LOG=steve=debug cargo run     # Run with debug logging
```

### Data Locations

| Platform | Path                                                        | Contents                     |
| -------- | ----------------------------------------------------------- | ---------------------------- |
| macOS    | `~/Library/Application Support/steve/storage/{project_id}/` | Sessions, messages, metadata |
| Linux    | `~/.local/share/steve/storage/{project_id}/`                | Sessions, messages, metadata |
| Both     | `{data_dir}/logs/steve.log.YYYY-MM-DD`                      | Rolling daily log output     |

Project ID is derived from the git root commit hash (deterministic across clones of the same repo).

### Logging

Logs are written to `{data_dir}/logs/` using daily rolling files via `tracing-appender`. Override
the log level with:

```bash
RUST_LOG=steve=debug cargo run
```

### Running Tests

```bash
cargo test                  # All tests (unit + integration)
cargo test --lib            # Library tests only
cargo test --test <name>    # Specific integration test
```

Integration tests in `tests/` cover permissions, config loading, and tool execution. Some LSP
integration tests are `#[ignore]` and require language servers on PATH.

## License

MIT
