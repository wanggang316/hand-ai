# model

统一的模型目录与流式推理客户端。

这个包提供：

- 跨 provider 统一的消息与上下文类型
- 基于事件流的模型输出接口
- 内置模型目录查询能力
- API provider 注册表
- `model-cli` / `generate_models` 两个命令行工具

## 功能概览

- `Client`：统一的流式与非流式调用入口
- `Context` / `Message` / `AssistantMessageEvent`：统一的数据结构
- `models()` / `get_model()`：查询内置模型目录
- `ApiProviderRegistry`：注册和管理底层 provider 实现
- `env_api_keys`：统一检查环境变量中的 API Key

## 安装

在 workspace 内作为本地依赖使用：

```toml
[dependencies]
model = { path = "../model" }
```

## Quick Start

```rust
use model::{Client, Context, Message, UserMessage, get_model};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new();
    let model = get_model("openai", "gpt-4o").expect("model not found");

    let context = Context {
        system_prompt: Some("You are a concise assistant.".into()),
        messages: vec![Message::User(UserMessage::new_text("Say hello"))],
        tools: None,
    };

    let message = client.complete_simple(&model, context, None).await?;
    println!("{message:?}");
    Ok(())
}
```

## 核心 API

### `Client`

- `Client::stream()`：返回底层 `AssistantMessageEvent` 流
- `Client::stream_simple()`：使用简化参数的事件流接口
- `Client::complete()`：消费完整事件流并返回最终 `AssistantMessage`
- `Client::complete_simple()`：简化参数版本的完整调用

### 模型目录

- `models()`：读取内置 `src/models.json`
- `get_provider_keys()`：列出 provider key
- `get_models(provider)`：列出指定 provider 的模型
- `get_model(provider, model_id)`：查询单个模型
- `calculate_cost(model, usage)`：根据 token 用量估算成本

## 事件流

流式接口返回 `AssistantMessageEvent`，常见事件包括：

- `Start`
- `TextStart` / `TextDelta` / `TextEnd`
- `ThinkingStart` / `ThinkingDelta` / `ThinkingEnd`
- `ToolCallStart` / `ToolCallDelta` / `ToolCallEnd`
- `Done`
- `Error`

这套事件模型是 `agent` 和 `coding-agent` 的基础。

## Provider 与模型目录

这个包同时包含两层能力：

- 模型目录：`src/models.json` 中维护了多 provider 的模型定义
- 运行时 provider：由 `ApiProviderRegistry` 注册具体实现

`Client::new()` 会自动注册内置 provider。当前默认注册的 API 包括：

- `openai-completions`
- `anthropic-messages`

其它 API 类型是否可用，以 `packages/model/src/client.rs` 中的注册逻辑为准。

## CLI

### `model-cli`

```bash
cargo run --bin model-cli -- help
```

常用命令：

```bash
cargo run --bin model-cli -- list-providers
cargo run --bin model-cli -- list-models
cargo run --bin model-cli -- list-models openai
cargo run --bin model-cli -- check-keys
cargo run --bin model-cli -- model-info openai gpt-4o
```

更完整的命令说明见 `packages/model/CLI.md`。

### `generate_models`

- 二进制：`packages/model/src/generate_models.rs`
- 作用：抓取并合并模型列表后写入 `src/models.json`

```bash
cargo run --bin generate_models
```

## 环境变量

API Key 解析逻辑位于 `src/env_api_keys.rs`。常见变量包括：

- `OPENAI_API_KEY`
- `ANTHROPIC_API_KEY`
- `ANTHROPIC_OAUTH_TOKEN`
- `GEMINI_API_KEY`
- `GROQ_API_KEY`
- `MISTRAL_API_KEY`

## 开发

```bash
cd packages/model
cargo check
cargo test
```

如果修改了 provider、模型目录或消息协议，README、`CLI.md` 和测试应一起更新。
