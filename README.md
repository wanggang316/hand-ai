# Hand AI

> **Looking for the coding agent?** See **[crates/coding-agent](crates/coding-agent)** for installation and usage.

Rust-native tools for building AI agents and working with LLMs. A unified multi-provider API, a stateful agent runtime, and an interactive terminal coding agent.

## Packages

| Package | Description |
|---------|-------------|
| **[model](crates/model)** | Unified multi-provider LLM API (OpenAI, Anthropic, Google, Bedrock, etc.) |
| **[hand-agent](crates/agent)** | Agent runtime with tool calling, steering, and event streaming |
| **[hand-coding-agent](crates/coding-agent)** | Interactive terminal coding agent CLI |
| **[hand-tui](crates/tui)** | Terminal UI component library with differential rendering |
| **[web-ui](crates/web-ui)** | Web UI (documentation placeholder) |
| **[examples](examples)** | Workspace examples |

## Architecture

```
hand-coding-agent (CLI binary)
├── hand-agent (agent loop, tools, events)
│   └── model (providers, streaming, model catalog)
└── hand-tui (terminal UI components)
```

## Supported Providers

**API keys:**
- Anthropic
- OpenAI
- Azure OpenAI (Responses)
- Google Gemini
- Google Vertex
- Amazon Bedrock
- Groq
- Cerebras
- xAI
- Mistral
- OpenRouter
- Vercel AI Gateway
- ZAI
- OpenCode
- Hugging Face
- Kimi For Coding
- MiniMax

See [crates/model](crates/model) for provider details.

## Quick Start

```bash
# Run the coding agent
cd crates/coding-agent
cargo run --bin hand

# With a prompt
cargo run --bin hand -- --prompt "Explain this codebase"

# Non-interactive mode
cargo run --bin hand -- --print --prompt "Summarize src/main.rs"
```

## Development

```bash
# Build all packages
cargo check --workspace

# Run all tests (396 tests)
cargo test --workspace

# Lint
cargo clippy --workspace

# Format
cargo fmt --all

# Check everything
./check.sh
```

Per-package:

```bash
cd crates/<name>
cargo check
cargo test
```

## Contributing

See [AGENTS.md](AGENTS.md) for project conventions.

## License

MIT
