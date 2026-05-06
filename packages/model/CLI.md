# Model CLI

命令行工具，用于管理和查询 AI 模型配置、运行流式聊天，以及管理 OAuth 凭证。

## 构建

```bash
cargo build --bin model-cli
```

## 命令

### 列出所有可用的 providers

```bash
cargo run --bin model-cli list-providers
```

显示所有已注册的 providers、其 API key 配置状态，以及（适用时）OAuth 登录状态。

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

### Chat 流式补全

```bash
cargo run --bin model-cli chat <provider> <model_id> "<prompt>" [flags]
```

可选 flags：

| Flag | Values | Description |
|---|---|---|
| `--transport` | `sse`, `websocket`, `auto` | 选择流式传输方式（映射到 `StreamOptions::transport`）|
| `--cache-retention` | `none`, `short`, `long` | 提示缓存策略（映射到 `StreamOptions::cache_retention`）|

示例：

```bash
cargo run --bin model-cli chat openai gpt-4o "Hello, how are you?"
cargo run --bin model-cli chat openai-codex gpt-5-codex "Hi" --transport websocket
cargo run --bin model-cli chat anthropic claude-sonnet-4-5 "Summarize" --cache-retention long
```

### OAuth 凭证管理

支持的 OAuth providers（slug 形式）：`anthropic`、`openai-codex`、`github-copilot`。

```bash
# 交互式登录（启动浏览器或显示 device code）
cargo run --bin model-cli oauth login <provider>

# 查看已认证的 providers
cargo run --bin model-cli oauth status

# 移除存储的凭证
cargo run --bin model-cli oauth logout <provider>
```

凭证存储路径：`~/.hand-ai/oauth.json`（目录权限 `0700`，文件权限 `0600`）。

`oauth login` 会调用 provider 的 `login()` 方法：
- Anthropic / OpenAI Codex 走 PKCE + 本地回环服务器，URL 会输出到 stderr，请在浏览器中打开。
- GitHub Copilot 走 device flow，user code + verification URL 会输出到 stderr。

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

## 手动冒烟测试

下列命令在本地环境冒烟新增 / 已有的 CLI 表面：

```bash
# 1. Help shows all subcommands including oauth + chat flags
cargo run -p model --bin model-cli -- --help

# 2. list-providers includes [oauth: ...] markers for the three OAuth providers
cargo run -p model --bin model-cli -- list-providers

# 3. oauth status runs without error before any login
cargo run -p model --bin model-cli -- oauth status

# 4. oauth login dispatches to the provider's login() (will print a URL or device code)
#    Cancel with Ctrl-C if you don't want to actually authenticate.
cargo run -p model --bin model-cli -- oauth login anthropic

# 5. oauth logout removes the credentials file entry
cargo run -p model --bin model-cli -- oauth logout anthropic

# 6. chat accepts the new transport / cache-retention flags
#    (Requires OPENAI_API_KEY; will hit the live API)
cargo run -p model --bin model-cli -- chat openai gpt-4o "Hi" --transport sse --cache-retention short

# 7. Invalid flag values exit non-zero with a usage message
cargo run -p model --bin model-cli -- chat openai gpt-4o "Hi" --transport carrier-pigeon
```

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

```bash
$ cargo run --bin model-cli oauth status

OAuth status:

  anthropic: authenticated, expires in 2h
  openai-codex: not authenticated
  github-copilot: authenticated

Storage: /Users/you/.hand-ai/oauth.json
```
