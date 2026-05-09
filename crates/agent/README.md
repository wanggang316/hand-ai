# hand-agent

Stateful agent with tool execution and event streaming. Built on `model`.

## Installation

```toml
[dependencies]
hand-agent = { path = "../agent" }
model = { path = "../model" }
```

## Quick Start

```rust
use hand_agent::Agent;
use model::{Client, get_model};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new();
    let model = get_model("anthropic", "claude-sonnet-4-20250514").expect("model not found");

    let mut agent = Agent::new(client, model);
    agent.set_system_prompt("You are a helpful assistant.");

    let result = agent.prompt("Hello!").await?;
    println!("messages: {}", result.messages.len());
    Ok(())
}
```

## Core Concepts

### AgentMessage vs LLM Message

The agent works with standard LLM messages (`user`, `assistant`, `toolResult`). The `transform_context` callback can modify messages before each LLM call (e.g., pruning old messages, injecting context).

### Message Flow

```
messages → transform_context() → messages → LLM
              (optional)
```

## Event Flow

The agent emits events for UI updates.

### prompt() Event Sequence

```
prompt("Hello")
├─ AgentStart
├─ TurnStart
├─ MessageStart   { user message }
├─ MessageEnd     { user message }
├─ MessageStart   { assistant message }
├─ MessageUpdate  { streaming chunks... }
├─ MessageEnd     { assistant message }
├─ TurnEnd
└─ AgentEnd
```

### With Tool Calls

```
prompt("Read config.json")
├─ AgentStart
├─ TurnStart
├─ MessageStart/End  { user message }
├─ MessageStart      { assistant with tool call }
├─ MessageUpdate...
├─ MessageEnd
├─ ToolExecutionStart  { tool_call_id, tool_name, args }
├─ ToolExecutionEnd    { tool_call_id, result }
├─ MessageStart/End  { tool result message }
├─ TurnEnd
│
├─ TurnStart           ← next turn
├─ MessageStart        { assistant responds to tool result }
├─ MessageUpdate...
├─ MessageEnd
├─ TurnEnd
└─ AgentEnd
```

Tool execution mode is configurable:
- `parallel` (default): execute allowed tools concurrently
- `sequential`: execute tool calls one by one

### Event Types

| Event | Description |
|-------|-------------|
| `AgentStart` | Agent begins processing |
| `AgentEnd` | Agent completes with all new messages |
| `TurnStart` | New turn begins (one LLM call + tool executions) |
| `TurnEnd` | Turn completes |
| `MessageStart` | Any message begins (user, assistant, toolResult) |
| `MessageUpdate` | **Assistant only.** Includes streaming delta |
| `MessageEnd` | Message completes |
| `ToolExecutionStart` | Tool begins |
| `ToolExecutionEnd` | Tool completes |

## Agent Options

```rust
let mut agent = Agent::new(client, model);

// System prompt
agent.set_system_prompt("You are a coding assistant.");

// Tools
agent.add_tool(my_tool);
agent.set_tools(vec![tool1, tool2]);

// Thinking level
agent.set_thinking_level(Some(ThinkingLevel::High));

// Tool execution mode
agent.set_tool_execution_mode(ToolExecutionMode::Sequential);

// Stream options (temperature, max_tokens, api_key)
agent.set_stream_options(Some(options));

// Hooks
agent.set_before_tool_call(Some(Box::new(|ctx| {
    Box::pin(async move { BeforeToolCallResult::default() })
})));
```

## Tools

Define tools using `AgentTool`:

```rust
use hand_agent::{AgentTool, ToolResult};

let read_file = AgentTool {
    name: "read_file".to_string(),
    description: "Read a file's contents".to_string(),
    parameters: serde_json::json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "File path" }
        },
        "required": ["path"]
    }),
    execute: Box::new(|_id, args, _signal| {
        Box::pin(async move {
            let path = args["path"].as_str().unwrap_or("");
            match std::fs::read_to_string(path) {
                Ok(content) => ToolResult::text(content),
                Err(e) => ToolResult::error(format!("Failed to read: {e}")),
            }
        })
    }),
};

agent.add_tool(read_file);
```

### Error Handling

Return `ToolResult::error()` when a tool fails. The agent reports errors to the LLM with `is_error: true`.

## State Management

```rust
agent.set_system_prompt("New prompt");
agent.set_model(new_model);
agent.set_thinking_level(Some(ThinkingLevel::Medium));
agent.replace_messages(new_messages);
agent.clear_messages();
agent.reset();
```

## Steering and Follow-up

Steering messages interrupt the agent while tools are running. Follow-up messages queue work after the agent would otherwise stop.

```rust
// While agent is running tools
agent.steer(user_message);

// After the agent finishes its current work
agent.follow_up(user_message);

// Clear queues
agent.clear_steering_queue();
agent.clear_follow_up_queue();
agent.clear_all_queues();
```

When steering messages are detected after a turn completes:
1. All tool calls from the current assistant message have finished
2. Steering messages are injected
3. The LLM responds on the next turn

Follow-up messages are checked only when there are no more tool calls and no steering messages.

## Low-Level API

For direct control without the `Agent` class:

```rust
use hand_agent::{run_agent_loop, AgentContext, AgentLoopConfig, AgentEventSink};

let context = AgentContext {
    system_prompt: "You are helpful.".to_string(),
    messages: vec![],
    model_id: "gpt-4o".to_string(),
    is_streaming: false,
};

let config = AgentLoopConfig { /* ... */ };
let emit: AgentEventSink = Box::new(|event| { /* handle event */ });

run_agent_loop(prompt_messages, &mut context, &tools, &config, &client, emit).await?;
```

## Proxy Transport

For applications that route LLM calls through a proxy server (the proxy
holds provider auth and keeps API keys off the client), use
`stream_fn_proxy` to swap the agent's transport:

```rust,ignore
use hand_agent::{Agent, AgentOptions, ProxyStreamOptions, stream_fn_proxy};
use model::{Client, get_model};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new();
    let model = get_model("anthropic", "claude-sonnet-4-20250514")
        .expect("model not found");

    let stream_fn = stream_fn_proxy(ProxyStreamOptions {
        auth_token: std::env::var("MY_APP_AUTH_TOKEN")?,
        proxy_url: "https://genai.example.com".into(),
        ..Default::default()
    });

    let mut agent = Agent::with_options(
        client,
        model,
        AgentOptions {
            stream_fn: Some(stream_fn),
            ..Default::default()
        },
    );
    agent.set_system_prompt("You are a helpful assistant.");

    let result = agent.prompt("Hello!").await?;
    println!("messages: {}", result.messages.len());
    Ok(())
}
```

The proxy server must accept `POST {proxy_url}/api/stream` with a JSON
body of `{ model, context, options }` and respond with `text/event-stream`
delivering `ProxyAssistantMessageEvent` payloads as `data: <json>` SSE
records.

For low-level use (your own agent loop or no `Agent` at all), call
`stream_proxy` directly:

```rust,ignore
use hand_agent::{stream_proxy, ProxyStreamOptions};
use futures::StreamExt;
use model::{Context, get_model};

let model = get_model("anthropic", "claude-sonnet-4-20250514")
    .expect("model not found");
let context = Context::default();
let auth_token = std::env::var("MY_APP_AUTH_TOKEN")?;

let opts = ProxyStreamOptions {
    auth_token,
    proxy_url: "https://genai.example.com".into(),
    ..Default::default()
};
let mut stream = stream_proxy(&model, context, opts);
while let Some(event) = stream.next().await {
    // event: model::AssistantMessageEvent
    // Errors and cancellation are delivered as terminal events on the
    // same stream — no separate Result channel.
}
```

## Development

```bash
cd packages/agent
cargo check
cargo test   # 28 tests (agent_test + agent_loop_test)
```

## License

MIT

## See Also

- [model](../model) — Unified LLM API
- [hand-coding-agent](../coding-agent) — Terminal coding agent
