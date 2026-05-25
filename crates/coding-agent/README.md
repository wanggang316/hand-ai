# hand-coding-agent

Interactive terminal coding agent. Adapt it to your workflows with context files and settings.

Runs in two modes: interactive REPL and non-interactive print. Built on `hand-agent` and `model`.

## Table of Contents

- [Quick Start](#quick-start)
- [Providers & Models](#providers--models)
- [Interactive Mode](#interactive-mode)
  - [Editor](#editor)
  - [Commands](#commands)
  - [Keyboard Shortcuts](#keyboard-shortcuts)
  - [Message Queue](#message-queue)
- [Sessions](#sessions)
  - [Compaction](#compaction)
- [Settings](#settings)
- [Context Files](#context-files)
- [Programmatic Usage](#programmatic-usage)
- [CLI Reference](#cli-reference)

---

## Quick Start

```bash
cd crates/coding-agent
cargo run --bin hand
```

Authenticate with an API key:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
cargo run --bin hand
```

Then just talk to `hand`. By default, it gives the model seven tools: `read`, `write`, `edit`, `bash`, `grep`, `find`, and `ls`. The model uses these to fulfill your requests.

---

## Providers & Models

For each built-in provider, the model catalog maintains a list of tool-capable models. Authenticate via API key, then select any model via `/model` or `--model`.

```bash
# Use default (Anthropic Claude Sonnet)
cargo run --bin hand

# Use OpenAI
cargo run --bin hand -- --provider openai --model gpt-4o

# Model with provider prefix
cargo run --bin hand -- --model openai/gpt-4o

# Model with thinking level
cargo run --bin hand -- --model sonnet:high

# List available models
cargo run --bin hand -- --list-models
cargo run --bin hand -- --list-models openai
```

See [crates/model](../model) for supported providers and environment variables.

---

## Interactive Mode

The interface from top to bottom:
- **Messages** — Your messages, assistant responses, tool calls and results
- **Editor** — Where you type
- **Status** — Model, session, token usage

### Editor

| Feature | How |
|---------|-----|
| File reference | Type `@path` to include file content |
| Bash commands | `!command` runs and sends output to LLM, `!!command` runs without sending |
| Multi-line | Shift+Enter |

### Commands

Type `/` to trigger commands:

| Command | Description |
|---------|-------------|
| `/help` | Show available commands |
| `/quit`, `/exit`, `/q` | Quit |
| `/model [pattern]` | Switch model |
| `/models [search]` | List available models |
| `/session` | Show session info (path, tokens, cost) |
| `/settings` | Show current settings |
| `/thinking [level]` | Set thinking level |
| `/compact [prompt]` | Manually compact context |
| `/new` | Start a new session |
| `/resume [id]` | Browse and select from past sessions |
| `/name <name>` | Set session display name |
| `/fork [id]` | Fork current session |
| `/export [file]` | Export session to HTML |
| `/copy` | Copy last assistant message to clipboard |
| `/hotkeys` | Show keyboard shortcuts |
| `/changelog` | Display version info |

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| Ctrl+C | Clear editor / quit if empty |
| Escape | Cancel/abort current operation |
| Enter | Submit message |

### Message Queue

Submit messages while the agent is working:

- **Enter** queues a *steering* message — delivered after the current turn's tool calls finish
- Steering and follow-up queues are managed by the underlying agent runtime

---

## Sessions

Sessions are stored as JSONL files with tree structure. Each entry has an `id` and `parentId`.

### Management

```bash
# Continue most recent session
cargo run --bin hand -- --continue

# Browse and select session
cargo run --bin hand -- --resume

# Specific session
cargo run --bin hand -- --resume s_xxx_xxx

# Fork a session
cargo run --bin hand -- --fork s_xxx_xxx

# Ephemeral mode
cargo run --bin hand -- --no-session
```

Sessions auto-save to `<cwd>/.hand/sessions/`.

### Compaction

Long sessions can exhaust context windows. Compaction summarizes older messages while keeping recent ones.

**Manual:** `/compact` or `/compact <custom instructions>`

**Automatic:** Enabled by default. Triggers on context overflow or when approaching the limit.

The full history remains in the JSONL file. Configure via settings.

---

## Settings

Edit YAML files directly:

| Location | Scope |
|----------|-------|
| `~/.hand/agent/settings.yaml` | Global (all projects) |
| `<cwd>/.hand/settings.yaml` | Project (overrides global) |

Keys accept both kebab-case (canonical) and snake_case forms.

Available settings:

| Setting | Description | Default |
|---------|-------------|---------|
| `default-provider` | Default provider | `"anthropic"` |
| `default-model` | Default model ID | `"claude-sonnet-4-20250514"` |
| `default-thinking-level` | Thinking level (`off`/`minimal`/`low`/`medium`/`high`/`xhigh`) | `null` |
| `shell-path` | Shell for bash tool | System default |
| `shell-command-prefix` | Prefix for shell commands | `null` |
| `theme` | Theme name (`dark`/`light`/`high-contrast`/`system`) | `"dark"` |
| `compaction.enabled` | Enable auto-compaction | `true` |
| `compaction.threshold` | Context % trigger | `0.8` |
| `retry.max-retries` | Max retries on error | `3` |
| `quiet-startup` | Suppress startup info | `false` |

---

## Context Files

`hand` loads context files at startup and injects them into the system prompt:

- `HAND.md` — Project instructions (from cwd)
- `.hand/context.md` — Additional context

Use these for project conventions, common commands, and instructions.

### System Prompt

Override the default system prompt:

```bash
cargo run --bin hand -- --system-prompt "You are a Rust expert."
```

Or append to it:

```bash
cargo run --bin hand -- --append-system-prompt "Always use idiomatic Rust."
```

---

## Programmatic Usage

```rust
use hand_coding_agent::{AgentSession, AgentSessionEvent};

// Create session with config
let mut session = AgentSession::new(config);

// Subscribe to events
session.subscribe(|event| {
    match event {
        AgentSessionEvent::Agent(agent_event) => { /* handle */ }
        AgentSessionEvent::CompactionStart => { /* ... */ }
        AgentSessionEvent::CompactionEnd { .. } => { /* ... */ }
    }
});

// Send a message
session.send_message("What files are in this directory?").await?;
```

You can:
- Create `AgentSessionConfig` with custom settings
- Reuse `tools::create_default_tools()` or provide custom tools
- Subscribe to events for custom rendering
- Drive the agent loop with `send_message()`

---

## CLI Reference

```bash
hand [options] [message]
```

### Modes

| Flag | Description |
|------|-------------|
| (default) | Interactive mode |
| `--print` | Print response and exit |

In print mode, `hand` also reads piped stdin:

```bash
cat README.md | cargo run --bin hand -- --print --prompt "Summarize this"
```

### Model Options

| Option | Description |
|--------|-------------|
| `--provider <name>` | Provider (anthropic, openai, google, etc.) |
| `--model <pattern>` | Model pattern or ID (supports `provider/id` and `:<thinking>`) |
| `--api-key <key>` | API key (overrides env vars) |
| `--thinking <level>` | `minimal`, `low`, `medium`, `high`, `xhigh` |
| `--list-models [search]` | List available models |

### Session Options

| Option | Description |
|--------|-------------|
| `-c`, `--continue` | Continue most recent session |
| `--resume [id]` | Browse/select session or specify ID |
| `--fork [id]` | Fork a session |
| `--no-session` | Ephemeral mode |

### Tool Options

| Option | Description |
|--------|-------------|
| `--tools <list>` | Enable specific tools (comma-separated) |
| `--no-tools` | Disable all tools |

Available tools: `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls`

### Other Options

| Option | Description |
|--------|-------------|
| `-p`, `--prompt <text>` | Initial prompt |
| `-d`, `--cwd <dir>` | Working directory |
| `--system-prompt <text>` | Override system prompt |
| `--append-system-prompt <text>` | Append to system prompt |
| `-v`, `--verbose` | Verbose logging |

### Examples

```bash
# Interactive with initial prompt
cargo run --bin hand -- "List all .rs files in src/"

# Non-interactive
cargo run --bin hand -- --print --prompt "Summarize this codebase"

# Piped stdin
cat README.md | cargo run --bin hand -- --print --prompt "Summarize"

# Different model
cargo run --bin hand -- --provider openai --model gpt-4o "Help me refactor"

# Model with thinking
cargo run --bin hand -- --model sonnet:high "Solve this complex problem"

# Read-only mode
cargo run --bin hand -- --tools read,grep,find,ls --print --prompt "Review the code"
```

---

## Development

```bash
cd crates/coding-agent
cargo check
cargo test   # 82 tests
```

Source layout:
- `src/main.rs` — CLI entry, interactive/print modes
- `src/core/agent_session.rs` — Session lifecycle, event forwarding
- `src/core/session_manager.rs` — JSONL session storage
- `src/core/settings.rs` — Global/project settings
- `src/core/system_prompt.rs` — System prompt and context files
- `src/core/compaction.rs` — Context compression
- `src/core/model_resolver.rs` — Model pattern matching
- `src/core/export.rs` — HTML/JSONL export
- `src/tools/` — Built-in tool implementations

---

## License

MIT

## See Also

- [model](../model) — Core LLM API
- [hand-agent](../agent) — Agent runtime
- [hand-tui](../tui) — Terminal UI components
