# Model CLI

命令行工具，用于管理和查询 AI 模型配置。

## 构建

```bash
cargo build --bin model-cli
```

## 命令

### 列出所有可用的 providers

```bash
cargo run --bin model-cli list-providers
```

显示所有已注册的 providers 及其 API key 配置状态。

### 列出模型

```bash
# 列出特定 provider 的所有模型
cargo run --bin model-cli list-models openai

# 列出所有 providers 的所有模型
cargo run --bin model-cli list-models
```

### 检查 API Keys 配置

```bash
cargo run --bin model-cli check-keys
```

显示每个 provider 的 API key 配置状态详情。

### 查看特定模型信息

```bash
cargo run --bin model-cli model-info <provider> <model_id>

# 示例
cargo run --bin model-cli model-info openai gpt-4o
```

显示模型的详细信息，包括：
- 模型 ID 和名称
- API 类型
- 上下文窗口大小
- 最大 token 数
- 成本（每百万 token）
- 兼容性配置

### 帮助

```bash
cargo run --bin model-cli help
```

## 环境变量

CLI 会检查以下环境变量来确定 API key 配置状态：

- `OPENAI_API_KEY` - OpenAI
- `ANTHROPIC_API_KEY` / `ANTHROPIC_OAUTH_TOKEN` - Anthropic
- `GEMINI_API_KEY` - Google Gemini
- `GROQ_API_KEY` - Groq
- `XAI_API_KEY` - xAI
- `MISTRAL_API_KEY` - Mistral
- 以及更多...

完整列表请参考 `src/env_api_keys.rs`。

## 示例输出

```bash
$ cargo run --bin model-cli model-info openai gpt-4o

Model Information:

  ID: gpt-4o
  Name: GPT-4o
  Provider: OpenAI
  API: OpenAIResponses
  Base URL: https://api.openai.com/v1
  Reasoning: false
  Input types: [Text, Image]
  Context window: 128000
  Max tokens: 16384

Cost (per million tokens):
  Input: $2.5000
  Output: $10.0000
  Cache read: $1.2500
  Cache write: $0.0000
```
