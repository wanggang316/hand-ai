# hand-agent

通用 Agent 运行时，负责把模型调用、工具执行和消息状态组织成一个可复用的 agent loop。

这个包基于 Rust 与 `model` 包实现。

## 安装

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
    let model = get_model("openai", "gpt-4o").expect("model not found");

    let mut agent = Agent::new(client, model);
    agent.set_system_prompt("You are a coding assistant.");

    let result = agent.prompt("Explain Rust ownership in one paragraph.").await?;
    println!("messages: {}", result.messages.len());
    Ok(())
}
```

## 核心概念

### `Agent`

高层包装，管理：

- 当前消息状态
- 选用的模型
- 工具列表
- 流式选项
- steering / follow-up 队列

### `run_agent_loop`

低层 API，直接接受：

- 初始 prompt 消息
- `AgentContext`
- 工具定义列表
- `AgentLoopConfig`
- `model::Client`
- `AgentEventSink`

如果你需要完全控制事件流或上下文拼接，优先使用这个低层接口。

## 工具调用

工具通过 `AgentTool` 描述：

- `name`
- `description`
- `parameters`（JSON Schema 风格）
- `execute`（异步执行函数）

Agent loop 会根据模型产生的 tool call 执行对应工具，并把结果以 `ToolResult` / `ToolResultMessage` 形式回填到上下文。

## 事件流

`AgentEvent` 是 UI 和上层运行时的主要观察接口。当前事件类型包括：

- `AgentStart` / `AgentEnd`
- `TurnStart` / `TurnEnd`
- `MessageStart` / `MessageUpdate` / `MessageEnd`
- `ToolExecutionStart` / `ToolExecutionEnd`

`coding-agent` 正是基于这套事件流来显示模型输出和工具执行状态。

## 状态管理

`AgentState` / `AgentContext` 持有：

- `system_prompt`
- `messages`
- `model_id`
- `is_streaming`

高层 `Agent` 提供的常用方法包括：

- `set_system_prompt()`
- `set_model()`
- `set_stream_options()`
- `set_tool_execution_mode()`
- `add_tool()` / `set_tools()`
- `replace_messages()` / `clear_messages()`
- `prompt()` / `prompt_with_messages()` / `continue()`
- `steer()` / `follow_up()`

## Steering 与 Follow-up

Agent 在 loop 运行过程中支持两种额外消息来源：

- steering：在运行中插入额外引导消息
- follow-up：当模型原本准备停止时，再追加一轮消息

这两个能力通过 `AgentLoopConfig` 中的异步回调提供，高层 `Agent` 则用内部队列进行管理。

## Hook

当前类型系统已经定义：

- `BeforeToolCallHook`
- `AfterToolCallHook`
- `BeforeToolCallContext`
- `AfterToolCallContext`

这些接口用于工具调用前后的审计、拦截和结果改写。

## 低层 API 示例

自行收集事件时：

```rust
use hand_agent::{run_agent_loop, AgentEventSink, AgentLoopConfig};
```

然后构造 `AgentEventSink = Box::new(|event| { ... })` 即可接收实时事件。

## 测试

测试主要覆盖：

- agent 基本交互
- loop 中的工具调用
- 事件行为
- 公共测试辅助工具

```bash
cd packages/agent
cargo check
cargo test
```

## License

MIT
