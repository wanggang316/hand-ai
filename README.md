# Hand AI

> **Looking for the coding agent?** See **[packages/coding-agent](packages/coding-agent)** for installation and usage.

Rust-native tools for building AI agents and working with LLMs. A unified multi-provider API, a stateful agent runtime, and an interactive terminal coding agent.

## Packages

| Package | Description |
|---------|-------------|
| **[model](packages/model)** | Unified multi-provider LLM API (OpenAI, Anthropic, Google, Bedrock, etc.) |
| **[hand-agent](packages/agent)** | Agent runtime with tool calling, steering, and event streaming |
| **[hand-coding-agent](packages/coding-agent)** | Interactive terminal coding agent CLI |
| **[hand-tui](packages/tui)** | Terminal UI component library with differential rendering |
| **[web-ui](packages/web-ui)** | Web UI (documentation placeholder) |
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

See [packages/model](packages/model) for provider details.

## Quick Start

```bash
# Run the coding agent
cd packages/coding-agent
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
cd packages/<name>
cargo check
cargo test
```

## Contributing

See [AGENTS.md](AGENTS.md) for project conventions.

## License

MIT
