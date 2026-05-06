# model

Unified LLM API with automatic model discovery, provider configuration, token and cost tracking, and streaming across multiple providers.

Only includes models that support tool calling (function calling), as this is essential for agentic workflows.

## Table of Contents

- [Supported Providers](#supported-providers)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Tools](#tools)
- [Streaming Events](#streaming-events)
- [Thinking/Reasoning](#thinkingreasoning)
- [Stop Reasons](#stop-reasons)
- [APIs, Models, and Providers](#apis-models-and-providers)
- [Cross-Provider Handoffs](#cross-provider-handoffs)
- [Environment Variables](#environment-variables)
- [CLI Tools](#cli-tools)

## Supported Providers

**API key providers:**
- **OpenAI** — GPT-4o, o3, o4-mini, etc.
- **Azure OpenAI** (Responses API)
- **OpenAI Codex** (Responses API)
- **Anthropic** — Claude Sonnet, Opus, Haiku
- **Google** — Gemini 2.5 Pro, Flash
- **Google Vertex** — Gemini via Vertex AI
- **Amazon Bedrock** — Claude via ConverseStream
- **Groq** — Llama, Mixtral
- **Cerebras** — Llama (fast inference)
- **xAI** — Grok
- **Mistral** — Mistral Large, Codestral
- **OpenRouter** — Multi-provider routing
- **Vercel AI Gateway**
- **MiniMax**
- **Hugging Face**
- **OpenCode** (Zen, Go)
- **Kimi For Coding** (Moonshot AI)
- **Any OpenAI-compatible API** — Ollama, vLLM, LM Studio, etc.

## Installation

```toml
[dependencies]
model = { path = "../model" }
```

## Quick Start

```rust
use model::{Client, Context, Message, UserMessage, Tool, get_model};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new();
    let model = get_model("openai", "gpt-4o").expect("model not found");

    let context = Context {
        system_prompt: Some("You are a concise assistant.".into()),
        messages: vec![Message::User(UserMessage::new_text("What time is it?"))],
        tools: None,
    };

    // Option 1: Streaming
    let mut stream = client.stream_simple(&model, context.clone(), None)?;
    use futures::StreamExt;
    while let Some(event) = stream.next().await {
        match event {
            model::AssistantMessageEvent::TextDelta { delta, .. } => {
                print!("{delta}");
            }
            model::AssistantMessageEvent::Done { message, .. } => {
                println!("\nTokens: {} in, {} out", message.usage.input, message.usage.output);
                println!("Cost: ${:.4}", message.usage.cost.total);
            }
            _ => {}
        }
    }

    // Option 2: Complete response
    let response = client.complete_simple(&model, context, None).await?;
    println!("{:?}", response.content);
    Ok(())
}
```

## Tools

Define tools with JSON Schema parameters:

```rust
use model::Tool;

let tools = vec![Tool {
    name: "get_weather".to_string(),
    description: "Get current weather for a location".to_string(),
    parameters: serde_json::json!({
        "type": "object",
        "properties": {
            "location": { "type": "string", "description": "City name" }
        },
        "required": ["location"]
    }),
}];

let context = Context {
    system_prompt: Some("You are helpful.".into()),
    messages: vec![Message::User(UserMessage::new_text("What's the weather in Tokyo?"))],
    tools: Some(tools),
};
```

Handle tool calls from the response:

```rust
for block in &response.content {
    match block {
        AssistantContentBlock::Text(t) => println!("{}", t.text),
        AssistantContentBlock::ToolCall(tc) => {
            println!("Tool: {}({})", tc.name, tc.arguments);
            // Execute tool, then add ToolResultMessage to context
        }
        _ => {}
    }
}
```

## Streaming Events

The stream returns `AssistantMessageEvent` variants:

| Event | Description |
|-------|-------------|
| `Start` | Stream begins, includes initial partial message |
| `TextStart` | Text content block started |
| `TextDelta` | Incremental text chunk |
| `TextEnd` | Text block complete |
| `ThinkingStart` | Thinking/reasoning started |
| `ThinkingDelta` | Incremental thinking chunk |
| `ThinkingEnd` | Thinking complete |
| `ToolCallStart` | Tool call started |
| `ToolCallDelta` | Tool call arguments streaming |
| `ToolCallEnd` | Tool call complete with parsed arguments |
| `Done` | Stream complete with final `AssistantMessage` |
| `Error` | Error occurred |

## Thinking/Reasoning

Models that support reasoning (Claude, o3, Gemini 2.5) can be configured with thinking levels:

```rust
use model::{SimpleStreamOptions, ThinkingLevel};

let mut options = SimpleStreamOptions::default();
options.reasoning = Some(ThinkingLevel::High);

let stream = client.stream_simple(&model, context, Some(options))?;
```

Thinking levels: `Minimal`, `Low`, `Medium`, `High`, `Xhigh`

Thinking content streams via `ThinkingStart`/`ThinkingDelta`/`ThinkingEnd` events.

## Stop Reasons

| Reason | Description |
|--------|-------------|
| `Stop` | Model finished naturally |
| `Length` | Max tokens reached |
| `ToolUse` | Model wants to call tools |
| `Error` | Error occurred |
| `Aborted` | Request was aborted |

## APIs, Models, and Providers

### Querying the Model Catalog

```rust
use model::{models, get_model, get_models, get_provider_keys, calculate_cost};

// List all provider keys
let providers = get_provider_keys();

// List models for a provider
let openai_models = get_models("openai");

// Get a specific model
let model = get_model("anthropic", "claude-sonnet-4-20250514").unwrap();

// Calculate cost
let cost = calculate_cost(&model, &usage);
```

### Provider Architecture

Each provider implements the `ApiProvider` trait and is registered with `ApiProviderRegistry`:

| API | Provider Implementation |
|-----|----------------------|
| `openai-completions` | `OpenAICompletionsProvider` |
| `openai-responses` | `OpenAIResponsesProvider` |
| `azure-openai-responses` | `OpenAIResponsesProvider` |
| `openai-codex-responses` | `OpenAIResponsesProvider` |
| `anthropic-messages` | `AnthropicMessagesProvider` |
| `bedrock-converse-stream` | `BedrockProvider` |
| `google-generative-ai` | `GoogleGenerativeAiProvider` |
| `google-gemini-cli` | `GoogleGenerativeAiProvider` |
| `google-vertex` | `GoogleGenerativeAiProvider` |

All 9 API types have registered providers. `Client::new()` registers them automatically.

## Cross-Provider Handoffs

Context (`Context`) is provider-agnostic. You can switch models mid-session:

```rust
let anthropic_model = get_model("anthropic", "claude-sonnet-4-20250514").unwrap();
let response = client.complete_simple(&anthropic_model, context.clone(), None).await?;

// Switch to OpenAI with the same context
let openai_model = get_model("openai", "gpt-4o").unwrap();
let response = client.complete_simple(&openai_model, context, None).await?;
```

The `transform` module handles cross-provider message normalization (thinking blocks, tool call IDs, etc.).

## Environment Variables

API key resolution (`env_api_keys`):

| Provider | Environment Variable(s) |
|----------|------------------------|
| OpenAI | `OPENAI_API_KEY` |
| Anthropic | `ANTHROPIC_OAUTH_TOKEN`, `ANTHROPIC_API_KEY` |
| Google | `GEMINI_API_KEY` |
| Vertex | `GOOGLE_APPLICATION_CREDENTIALS` + `GOOGLE_CLOUD_PROJECT` + `GOOGLE_CLOUD_LOCATION` |
| Bedrock | `AWS_PROFILE` or `AWS_ACCESS_KEY_ID`+`AWS_SECRET_ACCESS_KEY` or `AWS_BEARER_TOKEN_BEDROCK` |
| Groq | `GROQ_API_KEY` |
| Cerebras | `CEREBRAS_API_KEY` |
| xAI | `XAI_API_KEY` |
| Mistral | `MISTRAL_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| GitHub Copilot | `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN` |

## CLI Tools

### model-cli

```bash
cargo run --bin model-cli -- help
cargo run --bin model-cli -- list-providers
cargo run --bin model-cli -- list-models
cargo run --bin model-cli -- list-models openai
cargo run --bin model-cli -- check-keys
cargo run --bin model-cli -- model-info openai gpt-4o
```

### generate_models

Fetches and merges model definitions into `src/models.json`:

```bash
cargo run --bin generate_models
```

## Development

```bash
cd packages/model
cargo check
cargo test        # 97 unit + 42 integration tests
cargo clippy
```

## License

MIT

## See Also

- [hand-agent](../agent) — Agent runtime
- [hand-coding-agent](../coding-agent) — Terminal coding agent
- [hand-tui](../tui) — Terminal UI components
