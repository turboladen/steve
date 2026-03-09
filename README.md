# Steve

A TUI AI coding agent built in Rust. Steve connects to any OpenAI-compatible LLM API, streams responses token-by-token, and provides a tool-calling loop that lets the LLM read, search, edit, and execute code within your project.

Built with [ratatui](https://github.com/ratatui/ratatui) and inspired by [opencode](https://opencode.ai).

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

Create a `.steve.jsonc` in the root of the project you want to work in (or a global `~/.config/steve/config.jsonc`). The config defines your LLM providers, models, and which environment variable holds each provider's API key.

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
          "capabilities": { "tool_call": true, "reasoning": false }
        }
      }
    }
  }
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
          "capabilities": { "tool_call": true, "reasoning": false }
        }
      }
    }
  }
}
```

**Multiple providers:**

```jsonc
{
  "model": "openai/gpt-4o",
  "small_model": "openai/gpt-4o-mini",  // used for /compact summarization
  "auto_compact": true,                  // auto-compact at 80% context usage (default: true)
  "providers": {
    "openai": {
      "base_url": "https://api.openai.com/v1",
      "api_key_env": "OPENAI_API_KEY",
      "models": {
        "gpt-4o": {
          "id": "gpt-4o",
          "name": "GPT-4o",
          "context_window": 128000,
          "capabilities": { "tool_call": true, "reasoning": false }
        },
        "gpt-4o-mini": {
          "id": "gpt-4o-mini",
          "name": "GPT-4o Mini",
          "context_window": 128000,
          "capabilities": { "tool_call": true, "reasoning": false }
        }
      }
    },
    "anthropic": {
      "base_url": "https://openrouter.ai/api/v1",
      "api_key_env": "OPENROUTER_API_KEY",
      "models": {
        "claude-sonnet": {
          "id": "anthropic/claude-3.5-sonnet",
          "name": "Claude 3.5 Sonnet",
          "context_window": 200000,
          "capabilities": { "tool_call": true, "reasoning": false }
        }
      }
    }
  }
}
```

Model references use `"provider_id/model_id"` format everywhere — in the config, in commands, and internally.

Then set the corresponding environment variable:

```bash
export OPENAI_API_KEY="sk-..."
```

### Config Reference

| Field | Required | Description |
|-------|----------|-------------|
| `model` | Yes | Default model in `provider_id/model_id` format |
| `small_model` | No | Model used for `/compact` summarization (falls back to `model`) |
| `auto_compact` | No | Auto-compact when context reaches 80% (default: `true`) |
| `providers` | Yes | Map of provider configurations |
| `providers.*.base_url` | Yes | OpenAI-compatible API endpoint |
| `providers.*.api_key_env` | Yes | Name of the env var holding the API key |
| `providers.*.models` | Yes | Map of available models for this provider |

## Usage

### Commands

| Command | Action |
|---------|--------|
| `/new` | Start a new session |
| `/rename <title>` | Rename current session |
| `/models` | List available models |
| `/model <ref>` | Switch model (e.g., `/model openai/gpt-4o`) |
| `/compact` | Compact conversation to free context window |
| `/init` | Create AGENTS.md in project root |
| `/help` | Show help |
| `/exit` | Quit |

### Keybindings

| Key | Action |
|-----|--------|
| Enter | Send message |
| Tab | Toggle Build/Plan mode |
| Ctrl+C | Cancel stream (1st press) / Quit (2nd press) |
| Mouse wheel | Scroll messages |

### Agent Modes

- **Build mode** (default) — The LLM can read, write, and execute code. Write/execute tools require your permission.
- **Plan mode** — Read-only. The LLM can explore your code but cannot modify it. Press Tab to toggle.

### Tools

Steve gives the LLM access to these tools:

| Tool | Description |
|------|-------------|
| `read` | Read file contents |
| `write` | Write a file |
| `edit` | Edit a file with search/replace |
| `patch` | Apply a patch to a file |
| `grep` | Search file contents (regex) |
| `glob` | Find files by pattern |
| `list` | List directory contents |
| `bash` | Execute shell commands |
| `question` | Ask the user a question |
| `todo` | Track tasks |
| `webfetch` | Fetch a URL |

## Development

```bash
cargo check                        # Type-check
cargo test                         # Run tests
RUST_LOG=steve=debug cargo run     # Run with debug logging
```

### Data Locations

| Path | Contents |
|------|----------|
| `~/.local/share/steve/storage/{project_id}/` | Sessions, messages, project metadata |
| `~/.local/share/steve/logs/steve.log` | Rolling daily log output |

Project ID is derived from the git root commit hash (deterministic across clones of the same repo).

## License

MIT
