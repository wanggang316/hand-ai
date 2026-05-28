# model

Unified LLM client for the `hand-ai` workspace. Provides automatic model discovery, provider configuration, token and cost tracking, OAuth credential management, and streaming across 11 wire protocols.

Only includes models that support tool calling (function calling), as this is essential for agentic workflows.

For the full design history and milestone-by-milestone evolution, see [`docs/exec-plans/model-package-completion.md`](../../docs/exec-plans/model-package-completion.md).

## Table of Contents

- [Quick Start](#quick-start)
- [Provider Matrix](#provider-matrix)
- [Transports](#transports)
- [OAuth](#oauth)
- [Streaming Events](#streaming-events)
- [Advanced Stream Options](#advanced-stream-options)
- [Tools](#tools)
- [Thinking / Reasoning](#thinking--reasoning)
- [Stop Reasons](#stop-reasons)
- [Cross-Provider Handoffs](#cross-provider-handoffs)
- [Environment Variables](#environment-variables)
- [CLI Tools](#cli-tools)
- [Development](#development)

## Quick Start

```rust
use model::{Client, Context, Message, UserMessage, get_model, register_builtins};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `Client::new()` already calls `register_builtins`, registering every
    // built-in API provider (openai-completions, openai-responses,
    // anthropic-messages, google-*, bedrock, mistral, ...). Call it
    // explicitly only when you build a `Client` from a custom registry.
    let client = Client::new();
    let _ = register_builtins; // re-exported for callers with custom registries

    // To pay for only a subset of providers (smaller binary, fewer
    // transitive deps), build via the fluent ClientBuilder:
    //
    //   use model::{Client, Api};
    //   let _ui_only = Client::builder()
    //       .with_builtin(Api::AnthropicMessages)
    //       .with_builtin(Api::OpenAICompletions)
    //       .with_builtin(Api::GoogleGenerativeAi)
    //       .build();
    //
    // `with_provider(api, custom)` plugs in a provider implemented
    // outside this crate. `build()` cannot fail; an empty registry is
    // legal — `stream()` returns `ClientError::ProviderNotFound` at
    // runtime if a model targets an unregistered Api.

    let model = get_model("openai", "gpt-4o").expect("model not found");
    let context = Context {
        system_prompt: Some("You are a concise assistant.".into()),
        messages: vec![Message::User(UserMessage::new_text("What time is it?"))],
        tools: None,
    };

    // Streaming with the simple wrapper (handles transform_messages, retries,
    // timeouts, cancellation, on_payload / on_response callbacks).
    let mut stream = client.stream_simple(&model, context.clone(), None)?;
    while let Some(event) = stream.next().await {
        match event {
            model::AssistantMessageEvent::TextDelta { delta, .. } => print!("{delta}"),
            model::AssistantMessageEvent::Done { message, .. } => {
                println!("\nUsage: {} in / {} out", message.usage.input, message.usage.output);
            }
            _ => {}
        }
    }

    // Or block until completion.
    let _response = client.complete_simple(&model, context, None).await?;
    Ok(())
}
```

## Provider Matrix

The crate registers 11 wire-protocol APIs via `register_builtins`. Each row lists the `Api` identifier, the providers known to speak it, and the dominant authentication method.

| Api identifier             | Providers                                                                 | Auth          |
|----------------------------|---------------------------------------------------------------------------|---------------|
| `openai-completions`       | OpenAI, Groq, Cerebras, xAI, OpenRouter, Vercel AI Gateway, Cloudflare Workers AI, Cloudflare AI Gateway, Z.ai, Qwen / Moonshot / Xiaomi / Deepseek, OpenCode, MiniMax, HuggingFace, ... (plus any OpenAI-compatible host) | API key       |
| `openai-responses`         | OpenAI                                                                    | API key       |
| `openai-codex-responses`   | OpenAI Codex                                                              | OAuth (PKCE)  |
| `azure-openai-responses`   | Azure OpenAI                                                              | API key (`api-key` header) |
| `anthropic-messages`       | Anthropic, GitHub Copilot (proxied)                                       | API key or OAuth |
| `bedrock-converse-stream`  | Amazon Bedrock                                                            | AWS SigV4 / bearer |
| `google-generative-ai`     | Google AI Studio (Gemini), Google Antigravity, Google Gemini CLI          | API key or OAuth |
| `google-gemini-cli`        | Google Gemini CLI                                                         | OAuth         |
| `google-vertex`            | Google Vertex AI                                                          | ADC (gcloud) or API key |
| `mistral-conversations`    | Mistral La Plateforme                                                     | API key       |
| `faux`                     | In-process test double (feature-gated by `faux`)                          | n/a           |

Provider-specific Compat (OpenRouter routing, Z.ai tool streaming, Qwen thinking format, Anthropic eager tool input streaming, etc.) is auto-detected from `model.base_url` and can be overridden via `Model.compat`.

## Transports

`StreamOptions::transport` selects how the request is delivered. Most APIs only support `Sse`; `openai-codex-responses` additionally accepts `Websocket` and `WebsocketCached`.

| Transport          | Description                                                                |
|--------------------|----------------------------------------------------------------------------|
| `Sse` (default)    | HTTP request with `text/event-stream` response.                            |
| `Websocket`        | Single-shot WebSocket via `tokio-tungstenite`. Codex only.                 |
| `WebsocketCached`  | Reuses an idle WebSocket from a `SessionResources` pool. Codex only.       |
| `Auto`             | Provider chooses (Codex prefers `WebsocketCached` if a session is set).    |

The CLI exposes the same selector via `--transport sse|websocket|auto` on the `chat` subcommand.

## OAuth

Three providers ship with first-class OAuth support:

- **Anthropic Claude** — PKCE + loopback redirect (port 53692).
- **OpenAI Codex** — PKCE + loopback redirect (port 1455).
- **GitHub Copilot** — Device flow.

Credentials persist to `~/.hand-ai/oauth.json` (directory `0700`, file `0600`). Use the CLI:

```bash
cargo run -p model --bin model-cli -- oauth login anthropic
cargo run -p model --bin model-cli -- oauth status
cargo run -p model --bin model-cli -- oauth logout anthropic
```

See [`CLI.md`](./CLI.md) for the full OAuth subcommand reference. Programmatic access is exposed via the `model::oauth` module (`OAuthProvider` trait, `AnthropicOAuth`, `OpenAICodexOAuth`, `GitHubCopilotOAuth`).

## Streaming Events

The stream returns `AssistantMessageEvent` variants:

| Event             | Description                                          |
|-------------------|------------------------------------------------------|
| `Start`           | Stream begins, includes initial partial message.     |
| `TextStart`       | Text content block started.                          |
| `TextDelta`       | Incremental text chunk.                              |
| `TextEnd`         | Text block complete.                                 |
| `ThinkingStart`   | Thinking / reasoning started.                        |
| `ThinkingDelta`   | Incremental thinking chunk.                          |
| `ThinkingEnd`     | Thinking complete.                                   |
| `ToolCallStart`   | Tool call started.                                   |
| `ToolCallDelta`   | Tool call arguments streaming.                       |
| `ToolCallEnd`     | Tool call complete with parsed arguments.            |
| `Done`            | Stream complete with final `AssistantMessage`.       |
| `Error`           | Error occurred.                                      |

The `AssistantMessage` carried by `Done` includes `response_model`, `response_id`, and a `diagnostics: Option<Vec<AssistantMessageDiagnostic>>` recording any retries, recoveries, or transform-stage notes.

## Advanced Stream Options

`StreamOptions` (and the higher-level `SimpleStreamOptions`) expose runtime behavior beyond a basic prompt:

| Field             | Type                                                            | Purpose |
|-------------------|------------------------------------------------------------------|---------|
| `transport`       | `Option<Transport>`                                              | Wire protocol selection. |
| `cache_retention` | `Option<CacheRetention>` — `None` / `Short` / `Long`             | Prompt cache lifetime hint (5 min / 1h / 24h depending on provider). |
| `signal`          | `Option<CancellationToken>`                                      | Cooperative cancellation; cancelling aborts the in-flight stream. |
| `timeout_ms`      | `Option<u64>`                                                    | Wraps the call in `tokio::time::timeout`. |
| `max_retries`     | `Option<u32>`                                                    | Retries with exponential backoff on transient errors (HTTP 429/503/connection reset). Each retry appends a diagnostic. |
| `metadata`        | `Option<HashMap<String, serde_json::Value>>`                     | Free-form metadata forwarded to provider callbacks. |
| `on_payload`      | `Option<Arc<dyn Fn(...)>>`                                       | Invoked with the outbound request payload before transmission. |
| `on_response`     | `Option<Arc<dyn Fn(...)>>`                                       | Invoked with the `ProviderResponse { status, headers }` after the response head arrives. |

`stream_simple` and `complete_simple` apply `transform_messages` for cross-provider compatibility, then layer cancellation, retry, and timeout on top of the provider call.

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
```

Handle tool calls from the response:

```rust
for block in &response.content {
    match block {
        AssistantContentBlock::Text(t) => println!("{}", t.text),
        AssistantContentBlock::ToolCall(tc) => {
            println!("Tool: {}({})", tc.name, tc.arguments);
            // Execute tool, then add a ToolResultMessage to context.
        }
        _ => {}
    }
}
```

## Thinking / Reasoning

Models that support reasoning (Claude, o3, Gemini 2.5, Qwen, Z.ai, Deepseek) can be configured with thinking levels:

```rust
use model::{SimpleStreamOptions, ThinkingLevel};

let mut options = SimpleStreamOptions::default();
options.reasoning = Some(ThinkingLevel::High);
let stream = client.stream_simple(&model, context, Some(options))?;
```

Thinking levels: `Minimal`, `Low`, `Medium`, `High`, `Xhigh`. Levels are clamped per-model via `Model.thinking_level_map`.

Thinking content streams via `ThinkingStart` / `ThinkingDelta` / `ThinkingEnd` events. `ThinkingContent.redacted = Some(true)` indicates a model-redacted reasoning block.

## Stop Reasons

| Reason     | Description                              |
|------------|------------------------------------------|
| `Stop`     | Model finished naturally.                |
| `Length`   | Max tokens reached.                      |
| `ToolUse`  | Model wants to call tools.               |
| `Error`    | Error occurred.                          |
| `Aborted`  | Request was cancelled (signal token).    |

## Cross-Provider Handoffs

`Context` is provider-agnostic. You can switch models mid-session:

```rust
let anthropic_model = get_model("anthropic", "claude-sonnet-4-5").unwrap();
let response = client.complete_simple(&anthropic_model, context.clone(), None).await?;

// Switch to OpenAI with the same context.
let openai_model = get_model("openai", "gpt-4o").unwrap();
let response = client.complete_simple(&openai_model, context, None).await?;
```

The `transform` module handles cross-provider message normalization: image-bearing tool-result routing, eager-tool-input compat, Gemini-3 unsigned tool calls, response-id normalization, tool-call-id rewriting for Anthropic, and thinking-block fidelity.

## Environment Variables

API key resolution (`env_api_keys`):

| Provider        | Environment Variable(s)                                                     |
|-----------------|-----------------------------------------------------------------------------|
| OpenAI          | `OPENAI_API_KEY`                                                            |
| Anthropic       | `ANTHROPIC_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`                                |
| Google          | `GEMINI_API_KEY`                                                            |
| Vertex          | ADC (`gcloud auth application-default login`) + `GOOGLE_CLOUD_PROJECT` + `GOOGLE_CLOUD_LOCATION`, or `GOOGLE_APPLICATION_CREDENTIALS` |
| Bedrock         | `AWS_PROFILE` or `AWS_ACCESS_KEY_ID`+`AWS_SECRET_ACCESS_KEY`, or `AWS_BEARER_TOKEN_BEDROCK` |
| Groq            | `GROQ_API_KEY`                                                              |
| Cerebras        | `CEREBRAS_API_KEY`                                                          |
| xAI             | `XAI_API_KEY`                                                               |
| Mistral         | `MISTRAL_API_KEY`                                                           |
| OpenRouter      | `OPENROUTER_API_KEY`                                                        |
| GitHub Copilot  | `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`                          |

OAuth tokens persisted in `~/.hand-ai/oauth.json` take precedence over env vars for Anthropic, OpenAI Codex, and GitHub Copilot.

## CLI Tools

### model-cli

```bash
cargo run -p model --bin model-cli -- help
cargo run -p model --bin model-cli -- list-providers
cargo run -p model --bin model-cli -- list-models openai
cargo run -p model --bin model-cli -- check-keys
cargo run -p model --bin model-cli -- model-info openai gpt-4o
cargo run -p model --bin model-cli -- chat openai gpt-4o "Hello" --transport sse --cache-retention long
cargo run -p model --bin model-cli -- oauth login anthropic
```

Full reference: [`CLI.md`](./CLI.md).

### generate_models

Fetches and merges model definitions into `src/models.json`:

```bash
cargo run -p model --bin generate_models
```

## Development

```bash
cd crates/model
cargo build
cargo test --features faux        # 320+ unit + integration + parity tests
cargo clippy --all-targets --features faux -- -D warnings
cargo fmt -- --check
```

The `faux` feature must be enabled to compile the parity test suite (`tests/parity_*.rs`).

## License

MIT

## See Also

- [hand-agent](../agent) — Agent runtime
- [hand-coding-agent](../coding-agent) — Terminal coding agent
- [hand-tui](../tui) — Terminal UI components
- [`docs/exec-plans/model-package-completion.md`](../../docs/exec-plans/model-package-completion.md) — design history and milestones
