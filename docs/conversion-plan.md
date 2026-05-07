# Pi-Mono → Hand-AI 转换计划

> 将 TypeScript monorepo `pi-mono` 转换为 Rust workspace `hand-ai`。
> 命名规则：所有 `pi-` 前缀替换为 `hand-`。

---

## 总览

### 源项目包结构 (pi-mono)

```
pi-ai ← pi-agent-core ← pi-coding-agent ← pi-mom
  ↑                          ↑
  └── pi-web-ui              └── pi-tui
                                      ↑
                             pi-pods ──┘（仅依赖 agent-core）
```

### 目标 Rust Workspace

```
packages/
├── model/          ✅ 已基本完成（对应 pi-ai → hand-model）
├── agent/          🔲 待实现（对应 pi-agent-core → hand-agent）
├── tui/            🔲 待实现（对应 pi-tui → hand-tui）
├── coding-agent/   🔲 待实现（对应 pi-coding-agent → hand-coding-agent）
└── web-ui/         ⏸️  暂不转换（Web 前端保持 JS 生态更合理）
```

### 不转换的部分

| 包 | 原因 |
|----|------|
| `pi-web-ui` | Web 组件用 Lit/TailwindCSS，保持在 JS 生态更合理 |
| `pi-mom` | Slack bot，独立关注点，优先级低 |
| `pi-pods` | GPU 管理 CLI，独立关注点，优先级低 |

---

## 阶段 0：Model crate 补全（pi-ai → hand-model）

> **状态**：✅ 已完成。Model package completion 的 M1–M14 全部交付，详见
> [`docs/exec-plans/model-package-completion.md`](exec-plans/model-package-completion.md)。

### 0.1 已完成（基础）

- [x] 核心类型系统（Message, Content, StreamOptions, Usage, Cost...）
- [x] ApiProvider trait 和 Registry
- [x] Client struct（stream / complete API）
- [x] OpenAI Completions provider
- [x] 模型注册表（200+ models from models.json）
- [x] 环境变量 API Key 管理
- [x] CLI 工具（list-providers, list-models, chat...）
- [x] 测试框架

### 0.2 Provider 补全（M1–M11）

- [x] **Anthropic Messages provider**（SSE、thinking blocks、tool_use blocks、eager-tool-input compat）
- [x] **Google Generative AI provider**（Gemini 系列；含 Gemini CLI 凭证流）
- [x] **OpenAI Responses provider**
- [x] **Azure OpenAI Responses provider**（M7，与 OpenAI Responses 共享解析）
- [x] **AWS Bedrock provider**（converse-stream）
- [x] **Mistral Conversations provider**（M6，含 9 字符 tool-id 规范化与 reasoning mode）
- [x] **Google Vertex provider**（M8，ADC + API Key 双路径）
- [x] **OpenAI Codex Responses provider**（M9，SSE + WebSocket + WebsocketCached + OAuth）
- [x] **Cloudflare Workers AI / AI Gateway 覆盖层**（M10）
- [x] **Faux provider + parity 测试 harness**（M5）
- [x] **`register_builtins()` + Compat URL 自动检测**（M11）

### 0.3 功能补全（M1–M4, M12–M14）

- [x] **类型系统扩展**（M1）：`Transport`、`CacheRetention`、`ProviderResponse`、`AssistantMessageDiagnostic`、`ThinkingLevelMap`、`AnthropicMessagesCompat`、`OpenRouterRouting` 全字段、Compat 矩阵扩展。
- [x] **utils 模块**（M2）：`event_stream`、`diagnostics`、`json_parse`（safe partial parse）、`sanitize_unicode`、`validation`、`headers`、`hash`、`overflow`。
- [x] **Cross-provider transform 重构**（M3）：image-tool-result routing、eager-tool-input、Gemini-3 unsigned tool calls、response-id normalization。
- [x] **OAuth 子系统**（M4）：Anthropic / OpenAI Codex / GitHub Copilot；PKCE + 设备流；凭证存于 `~/.hand-ai/oauth.json`。
- [x] **`stream_simple` / `complete_simple` 包装层**（M12）：`signal` 取消、`timeout_ms`、`max_retries` 指数退避、`metadata`、`on_payload` / `on_response` 回调、自动 `transform_messages`。
- [x] **CLI surface 对齐**（M13）：`oauth login/status/logout`、`chat --transport`、`chat --cache-retention`、`list-providers` 显示 OAuth 状态。
- [x] **文档刷新**（M14）：本文件、`packages/model/README.md`、`packages/model/CLI.md`、ExecPlan Progress / Outcomes 更新。

---

## 阶段 1：Agent crate（pi-agent-core → hand-agent）

> 核心 Agent 运行时，这是整个系统的骨干。

### 1.1 核心类型

对应文件: `packages/agent/src/types.ts`

```rust
// 核心类型定义
pub enum AgentMessage {
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
    Custom(CustomMessage),            // 扩展用
}

pub struct AgentTool {
    pub name: String,
    pub description: String,
    pub schema: serde_json::Value,    // JSON Schema
    pub label: Option<String>,
    pub execute: ToolExecuteFn,       // async fn
}

pub type ToolExecuteFn = Box<dyn Fn(serde_json::Value) -> BoxFuture<'static, ToolResult> + Send + Sync>;

pub struct ToolResult {
    pub content: String,
    pub details: Option<String>,
    pub is_error: bool,
}

pub enum AgentEvent {
    AgentStart,
    TurnStart { turn: u32 },
    MessageUpdate(AssistantMessageEvent),
    ToolExecutionStart { tool: String, args: serde_json::Value },
    ToolExecutionEnd { tool: String, result: ToolResult },
    TurnEnd { turn: u32, stop_reason: StopReason },
    AgentEnd,
    Error(AgentError),
}
```

### 1.2 Agent Loop

对应文件: `packages/agent/src/agent-loop.ts`

```rust
pub struct AgentLoopConfig {
    pub max_turns: Option<u32>,
    pub tools: Vec<AgentTool>,
    pub tool_execution_mode: ToolExecutionMode, // Sequential | Parallel
    pub hooks: AgentHooks,
}

pub enum ToolExecutionMode {
    Sequential,
    Parallel,
}

pub struct AgentHooks {
    pub before_tool_call: Option<BeforeToolCallHook>,
    pub after_tool_call: Option<AfterToolCallHook>,
    pub steering_message: Option<SteeringMessageHook>,
    pub follow_up_message: Option<FollowUpMessageHook>,
}

/// 核心 agent loop — 单次 turn
pub async fn agent_loop(
    stream_fn: &dyn ApiProvider,
    model: &Model,
    messages: &mut Vec<AgentMessage>,
    config: &AgentLoopConfig,
) -> Result<AgentLoopResult, AgentError> { ... }

/// 持续运行直到不再需要 tool call
pub async fn agent_loop_continue(
    stream_fn: &dyn ApiProvider,
    model: &Model,
    messages: &mut Vec<AgentMessage>,
    config: &AgentLoopConfig,
    event_sink: &dyn EventSink,
) -> Result<AgentResult, AgentError> { ... }
```

### 1.3 Agent struct

对应文件: `packages/agent/src/agent.ts`

```rust
pub struct Agent {
    client: Client,
    model: Model,
    messages: Vec<AgentMessage>,
    config: AgentLoopConfig,
    system_prompt: Option<String>,
}

impl Agent {
    pub fn new(client: Client, model: Model, config: AgentLoopConfig) -> Self;
    pub fn set_system_prompt(&mut self, prompt: String);
    pub fn add_tool(&mut self, tool: AgentTool);
    pub async fn run(&mut self, input: &str) -> Result<AgentResult, AgentError>;
    pub fn messages(&self) -> &[AgentMessage];
    pub fn clear_messages(&mut self);
}
```

### 1.4 Event Sink

```rust
/// 事件发送抽象（替代 TS 的回调）
pub trait EventSink: Send + Sync {
    fn emit(&self, event: AgentEvent);
}

// Channel 实现
pub struct ChannelEventSink {
    sender: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
}
```

### 1.5 Proxy / 自定义 StreamFn

对应: `packages/agent/src/proxy.ts`

```rust
/// 支持自定义后端（代理、缓存等）
pub trait StreamFn: Send + Sync {
    fn stream(
        &self,
        messages: &[AgentMessage],
        tools: &[AgentTool],
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static>;
}
```

### 1.6 依赖

```toml
[dependencies]
hand-model = { path = "../model" }
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
futures = "0.3"
thiserror = "2"
tracing = "0.1"       # 结构化日志（替代 console.log）
```

---

## 阶段 2：TUI crate（pi-tui → hand-tui）

> 终端 UI 库，差分渲染。

### 2.1 核心抽象

对应: `packages/tui/src/tui.ts`

```rust
/// 组件 trait（替代 TS 的 Component 接口）
pub trait Component: Send {
    fn render(&self, width: u16, height: u16) -> Vec<Line>;
    fn handle_input(&mut self, event: InputEvent) -> HandleResult;
}

/// 可聚焦组件（支持光标/IME）
pub trait Focusable: Component {
    fn cursor_position(&self) -> Option<(u16, u16)>;
}

/// 终端 UI 引擎
pub struct Tui {
    terminal: Terminal,
    root: Box<dyn Component>,
}

impl Tui {
    pub fn new(root: Box<dyn Component>) -> Result<Self, io::Error>;
    pub async fn run(&mut self) -> Result<(), io::Error>;
}
```

### 2.2 组件实现

按优先级排序：

| 优先级 | 组件 | 对应 TS | 说明 |
|--------|------|---------|------|
| P0 | `Text` | `Text.ts` | 基础文本渲染 |
| P0 | `Input` | `Input.ts` | 单行输入 |
| P0 | `Box` | `Box.ts` | 布局容器 |
| P1 | `Markdown` | `Markdown.ts` | Markdown 渲染（用 `pulldown-cmark`） |
| P1 | `Editor` | `Editor.ts` | 多行编辑器（最复杂） |
| P1 | `SelectList` | `SelectList.ts` | 选择列表 |
| P2 | `Loader` | `Loader.ts` | 加载指示器 |
| P2 | `Spacer` | `Spacer.ts` | 空白间距 |
| P2 | `Image` | `Image.ts` | 终端图片 |

### 2.3 终端基础设施

```rust
// 终端能力检测
pub struct TerminalCapabilities {
    pub supports_color: bool,
    pub supports_unicode: bool,
    pub supports_images: bool,  // iTerm2, Kitty 等
    pub supports_mouse: bool,
}

// 差分渲染引擎
pub struct DiffRenderer {
    prev_frame: Vec<Line>,
    current_frame: Vec<Line>,
}
```

### 2.4 依赖

```toml
[dependencies]
crossterm = "0.28"         # 跨平台终端操作（替代 raw ANSI）
unicode-width = "0.2"      # Unicode 字符宽度
pulldown-cmark = "0.12"    # Markdown 解析
syntect = "5"              # 语法高亮
colored = "3"              # 终端着色
```

### 2.5 替代方案考虑

可以考虑基于 `ratatui` 构建（Rust 生态最成熟的 TUI 框架），但 pi-tui 有自己的差分渲染引擎，如果要保持功能一致性可能需要自行实现。建议：

- **方案 A**：基于 `crossterm` 从零实现（保持与原 hand-tui 的功能对等）
- **方案 B**：基于 `ratatui` 构建（利用成熟生态，但 API 不同）
- **推荐方案 A**：因为 hand-tui 的核心卖点是差分渲染和自定义 Component 模型

---

## 阶段 3：Coding Agent crate（pi-coding-agent → hand-coding-agent）

> 最大最复杂的包，交互式编码 Agent。

### 3.1 核心模块

| 模块 | 对应 TS 文件 | 说明 |
|------|-------------|------|
| `agent_session` | `core/agent-session.ts` | Agent 会话生命周期管理 |
| `tools/` | `core/tools/*.ts` | 7 个编码工具 |
| `session_manager` | `core/session-manager.ts` | 会话持久化与分支 |
| `settings_manager` | `core/settings-manager.ts` | 用户配置 |
| `model_registry` | `core/model-registry.ts` | 动态模型发现 |
| `extensions/` | `core/extensions/` | 插件系统 |
| `compaction/` | `core/compaction/` | 对话压缩 |
| `bash_executor` | `core/bash-executor.ts` | 安全的 Bash 执行 |
| `resource_loader` | `core/resource-loader.ts` | 技能/提示/主题加载 |

### 3.2 Tools（编码工具）

```rust
pub fn create_read_tool(config: &ToolConfig) -> AgentTool;
pub fn create_write_tool(config: &ToolConfig) -> AgentTool;
pub fn create_edit_tool(config: &ToolConfig) -> AgentTool;
pub fn create_bash_tool(config: &ToolConfig) -> AgentTool;
pub fn create_find_tool(config: &ToolConfig) -> AgentTool;
pub fn create_grep_tool(config: &ToolConfig) -> AgentTool;
pub fn create_ls_tool(config: &ToolConfig) -> AgentTool;
```

每个工具的参数用 `schemars::JsonSchema` derive：

```rust
#[derive(Deserialize, JsonSchema)]
pub struct ReadParams {
    /// 要读取的文件路径
    pub path: String,
    /// 起始行号（从 1 开始）
    #[serde(default)]
    pub start_line: Option<u32>,
    /// 结束行号
    #[serde(default)]
    pub end_line: Option<u32>,
}
```

### 3.3 运行模式

```rust
pub enum RunMode {
    Interactive,   // 完整 TUI
    Print,         // 简单文本输出
    Rpc,           // 远程调用接口
}

// 模式 trait
pub trait AgentMode: Send {
    async fn run(&mut self, session: &mut AgentSession) -> Result<(), CodingAgentError>;
}
```

### 3.4 会话管理

```rust
pub struct AgentSession {
    agent: Agent,
    tools: Vec<AgentTool>,
    session_manager: SessionManager,
    settings: SettingsManager,
    extensions: ExtensionRuntime,
    mode: RunMode,
}

pub struct SessionManager {
    base_dir: PathBuf,
    // 会话树：支持分支和回溯
}

impl SessionManager {
    pub fn save(&self, messages: &[AgentMessage]) -> Result<(), io::Error>;
    pub fn load(&self, session_id: &str) -> Result<Vec<AgentMessage>, io::Error>;
    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>, io::Error>;
    pub fn branch(&self, from: &str) -> Result<String, io::Error>;
}
```

### 3.5 扩展系统

```rust
pub trait Extension: Send + Sync {
    fn name(&self) -> &str;
    fn on_before_tool_call(&self, tool: &str, args: &serde_json::Value) -> Option<HookAction>;
    fn on_after_tool_call(&self, tool: &str, result: &ToolResult) -> Option<HookAction>;
    fn on_agent_start(&self, session: &AgentSession);
    fn commands(&self) -> Vec<SlashCommand>;
}

pub struct ExtensionRuntime {
    extensions: Vec<Box<dyn Extension>>,
}
```

### 3.6 CLI 入口

```rust
// 使用 clap 解析命令行
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "hand", about = "Interactive coding agent")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// 直接传入 prompt
    #[arg(short, long)]
    pub prompt: Option<String>,

    /// 运行模式
    #[arg(long, default_value = "interactive")]
    pub mode: RunMode,

    /// 模型
    #[arg(short, long)]
    pub model: Option<String>,
}
```

### 3.7 依赖

```toml
[dependencies]
hand-model = { path = "../model" }
hand-agent = { path = "../agent" }
hand-tui = { path = "../tui" }
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
tokio = { version = "1", features = ["full"] }
glob = "0.3"
globset = "0.4"
ignore = "0.4"
similar = "2"            # diff
syntect = "5"            # 语法高亮
schemars = "1"           # JSON Schema 生成
tracing = "0.1"
tracing-subscriber = "0.3"
dirs = "6"               # 用户目录
```

---

## 阶段 4：集成与完善

### 4.1 系统提示与 Skill 系统

- 从 hand-coding-agent 提取系统提示模板
- 实现 skill 加载器（从文件系统读取 markdown 片段）
- 实现 context file 加载（.hand 目录）

### 4.2 对话压缩 (Compaction)

```rust
pub struct CompactionConfig {
    pub max_context_tokens: u32,
    pub summary_model: String,
}

pub async fn compact_messages(
    messages: &[AgentMessage],
    config: &CompactionConfig,
    client: &Client,
) -> Result<Vec<AgentMessage>, CompactionError>;
```

### 4.3 测试策略

见下方独立章节「测试规范」。

---

## 测试规范

> **原则：每一行业务代码都必须有对应的测试用例覆盖，保证程序正常执行。**
> 测试先于实现编写（或至少同步编写），任何 PR 必须通过全部测试才能合并。

### 总体分层

| 层级 | 位置 | 运行方式 | 说明 |
|------|------|---------|------|
| 单元测试 | `src/` 内 `#[cfg(test)] mod tests` | `cargo test` | 模块内部逻辑，不依赖外部 |
| 集成测试 | `tests/*.rs` | `cargo test` | 跨模块协作，使用 Mock Provider |
| E2E 测试 | `tests/*.rs` + `#[ignore]` | `cargo test -- --ignored` | 真实 LLM API，CI 中按需触发 |
| 属性测试 | `#[cfg(test)]` 内使用 `proptest` | `cargo test` | 随机输入验证不变量 |

### 命名规范

```rust
// 单元测试模块
#[cfg(test)]
mod tests {
    use super::*;

    // 命名格式: test_<被测函数/场景>_<预期行为>
    #[test]
    fn test_calculate_cost_returns_zero_for_free_model() { ... }

    #[tokio::test]
    async fn test_stream_emits_text_events_in_order() { ... }

    // 边界/异常用例加后缀
    #[test]
    fn test_get_model_returns_error_for_unknown_id() { ... }

    #[test]
    fn test_parse_model_handles_empty_json() { ... }
}
```

### 测试基础设施（共享 fixtures）

每个 crate 的 `tests/` 目录下建立 `common/mod.rs`，提供可复用的测试辅助：

```rust
// tests/common/mod.rs

use hand_model::*;

/// 创建最小可用的测试 Model
pub fn test_model() -> Model {
    Model {
        id: "test-model".into(),
        name: "Test Model".into(),
        api: Api::OpenAiCompletions,
        provider: Provider::OpenAI,
        base_url: "https://api.test.com".into(),
        reasoning: false,
        input: vec![InputType::Text],
        cost: Cost { input: 1.0, output: 2.0, cache_read: 0.5, cache_write: 0.75 },
        context_window: 128000,
        max_tokens: 4096,
        headers: None,
        compat: None,
    }
}

/// 创建最小可用的 Context
pub fn test_context(prompt: &str) -> Context {
    Context {
        system_prompt: Some("You are a test assistant.".into()),
        messages: vec![Message::User(UserMessage {
            content: UserContent::Text(prompt.into()),
            timestamp: 0,
        })],
        tools: None,
    }
}

/// Mock Provider — 返回固定文本
pub struct MockTextProvider {
    pub response: String,
}

impl ApiProvider for MockTextProvider {
    fn stream(&self, _model: Model, _ctx: Context, _opts: Option<StreamOptions>)
        -> AssistantMessageEventStream<'static>
    {
        let text = self.response.clone();
        Box::pin(async_stream::stream! {
            yield AssistantMessageEvent::Start;
            yield AssistantMessageEvent::TextStart { content_index: 0 };
            yield AssistantMessageEvent::TextDelta { content_index: 0, delta: text.clone() };
            yield AssistantMessageEvent::TextEnd { content_index: 0 };
            yield AssistantMessageEvent::Done(AssistantMessage {
                content: vec![ContentBlock::Text(TextContent { text })],
                stop_reason: StopReason::Stop,
                usage: Usage::default(),
                cost: None,
                ..Default::default()
            });
        })
    }

    fn stream_simple(&self, model: Model, ctx: Context, opts: Option<SimpleStreamOptions>)
        -> AssistantMessageEventStream<'static>
    {
        self.stream(model, ctx, opts.map(|o| o.base))
    }
}

/// Mock Provider — 返回 tool call
pub struct MockToolProvider {
    pub tool_name: String,
    pub tool_args: serde_json::Value,
}

/// Mock Provider — 返回错误
pub struct MockErrorProvider {
    pub error_message: String,
}

/// Mock Provider — 模拟 thinking/reasoning
pub struct MockThinkingProvider {
    pub thinking_text: String,
    pub response_text: String,
}

/// Mock Provider — 模拟多轮对话（根据 messages 长度返回不同内容）
pub struct MockMultiTurnProvider;

/// Mock Provider — 模拟流式分块（每个字符一个 delta）
pub struct MockChunkedStreamProvider {
    pub response: String,
    pub chunk_size: usize,
}
```

---

### hand-model crate 测试用例清单

#### 类型系统 (`types.rs`)

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| T-001 | `test_message_user_serialize_roundtrip` | 单元 | UserMessage 序列化→反序列化保持一致 |
| T-002 | `test_message_assistant_serialize_roundtrip` | 单元 | AssistantMessage 序列化→反序列化 |
| T-003 | `test_message_tool_result_serialize_roundtrip` | 单元 | ToolResultMessage 序列化→反序列化 |
| T-004 | `test_content_block_text_serialize` | 单元 | TextContent 正确序列化为 JSON |
| T-005 | `test_content_block_thinking_serialize` | 单元 | ThinkingContent 序列化 |
| T-006 | `test_content_block_image_serialize` | 单元 | ImageContent 含 base64 数据 |
| T-007 | `test_content_block_tool_call_serialize` | 单元 | ToolCall 含 name/id/arguments |
| T-008 | `test_stop_reason_variants` | 单元 | 所有 StopReason 变体的序列化 |
| T-009 | `test_thinking_level_ordering` | 单元 | Minimal < Low < Medium < High < Xhigh |
| T-010 | `test_usage_default_is_zero` | 单元 | Usage::default() 所有字段为 0 |
| T-011 | `test_stream_options_default` | 单元 | StreamOptions::default() 所有字段 None |
| T-012 | `test_simple_stream_options_build_base` | 单元 | build_base_options 正确设置默认值 |
| T-013 | `test_simple_stream_options_clamp_reasoning` | 单元 | xhigh 被 clamp 到 high |
| T-014 | `test_adjust_max_tokens_for_thinking` | 单元 | 各 thinking level 的 token budget 正确 |
| T-015 | `test_adjust_max_tokens_respects_model_limit` | 单元 | 不超过 model.max_tokens |
| T-016 | `test_api_enum_all_variants_serializable` | 单元 | 14 个 Api 变体全部可序列化 |
| T-017 | `test_provider_enum_all_variants_serializable` | 单元 | 22 个 Provider 变体全部可序列化 |
| T-018 | `test_input_type_enum_variants` | 单元 | Text, Image 序列化 |
| T-019 | `test_user_content_text_and_blocks` | 单元 | UserContent::Text 和 Blocks 两种变体 |
| T-020 | `test_cost_calculation_precision` | 单元 | 浮点精度在合理范围内 |

#### 模型注册表 (`models.rs`)

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| T-030 | `test_models_json_parses_successfully` | 单元 | 内嵌 JSON 能正确解析 |
| T-031 | `test_models_returns_non_empty` | 单元 | 至少返回 1 个 model |
| T-032 | `test_get_model_known_id` | 单元 | 已知 model id 返回 Ok |
| T-033 | `test_get_model_unknown_id` | 单元 | 未知 id 返回 None/Err |
| T-034 | `test_get_models_by_provider` | 单元 | 每个 provider 至少有 1 个 model |
| T-035 | `test_get_provider_keys_sorted` | 单元 | 返回值已排序 |
| T-036 | `test_get_providers_covers_all` | 单元 | 至少覆盖主要 provider |
| T-037 | `test_calculate_cost_basic` | 单元 | input=100, output=50 的 cost 正确 |
| T-038 | `test_calculate_cost_with_cache` | 单元 | cache_read/write 参与计算 |
| T-039 | `test_calculate_cost_zero_usage` | 单元 | 全零 usage 返回 0 cost |
| T-040 | `test_supports_xhigh` | 单元 | 已知 xhigh 模型返回 true |
| T-041 | `test_not_supports_xhigh` | 单元 | 普通模型返回 false |
| T-042 | `test_models_are_equal` | 单元 | 相同 model 比较返回 true |
| T-043 | `test_models_not_equal_different_id` | 单元 | 不同 id 返回 false |
| T-044 | `test_all_models_have_valid_api` | 单元 | 每个 model 的 api 字段合法 |
| T-045 | `test_all_models_have_positive_context_window` | 单元 | context_window > 0 |
| T-046 | `test_all_models_have_non_negative_cost` | 单元 | cost 字段 >= 0 |

#### API Key 管理 (`env_api_keys.rs`)

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| T-050 | `test_get_api_key_from_env` | 单元 | 设置环境变量后能获取到 |
| T-051 | `test_get_api_key_missing_returns_none` | 单元 | 未设置时返回 None |
| T-052 | `test_anthropic_oauth_priority` | 单元 | OAUTH_TOKEN 优先于 API_KEY |
| T-053 | `test_copilot_token_fallback_chain` | 单元 | COPILOT → GH → GITHUB 顺序 |
| T-054 | `test_provider_env_var_mapping_complete` | 单元 | 每个 Provider 都有对应的 env var |

#### Provider Registry (`api_registry.rs`)

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| T-060 | `test_register_and_get_provider` | 单元 | 注册后可获取 |
| T-061 | `test_get_unregistered_returns_none` | 单元 | 未注册的 api 返回 None |
| T-062 | `test_register_overwrites_existing` | 单元 | 重复注册覆盖前者 |
| T-063 | `test_unregister_by_source` | 单元 | 按 source_id 注销 |
| T-064 | `test_clear_removes_all` | 单元 | clear 后 get_all 为空 |
| T-065 | `test_get_all_returns_registered` | 单元 | 返回所有已注册 provider |
| T-066 | `test_registry_thread_safe` | 集成 | 多线程并发注册/获取不 panic |

#### Client (`client.rs`)

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| T-070 | `test_client_new_has_builtin_providers` | 单元 | 默认注册了 OpenAI 等 |
| T-071 | `test_client_stream_basic_text` | 集成 | Mock → stream → 收到 text events |
| T-072 | `test_client_stream_tool_call` | 集成 | Mock → stream → 收到 tool call events |
| T-073 | `test_client_stream_thinking` | 集成 | Mock → stream → 收到 thinking events |
| T-074 | `test_client_stream_error_event` | 集成 | Mock → stream → 收到 error event |
| T-075 | `test_client_complete_returns_message` | 集成 | complete() 消费 stream 返回完整 message |
| T-076 | `test_client_complete_simple` | 集成 | complete_simple() 简化调用 |
| T-077 | `test_client_stream_unknown_provider` | 集成 | 未注册 provider → ProviderNotFound |
| T-078 | `test_client_stream_empty_response` | 集成 | 空 stream → StreamEndedWithoutResult |
| T-079 | `test_client_multi_turn_conversation` | 集成 | 多轮对话 messages 累积正确 |
| T-080 | `test_client_stream_chunked_text` | 集成 | 文本分多个 delta 到达，最终拼接正确 |
| T-081 | `test_client_stream_multiple_content_blocks` | 集成 | 一个 response 含 text + tool_call |
| T-082 | `test_client_stream_with_custom_options` | 集成 | temperature/max_tokens 传递正确 |

#### OpenAI Completions Provider (`providers/openai_completions.rs`)

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| T-090 | `test_convert_messages_user_text` | 单元 | UserMessage → OpenAI 格式 |
| T-091 | `test_convert_messages_user_image` | 单元 | 含 image 的 UserMessage 转换 |
| T-092 | `test_convert_messages_assistant` | 单元 | AssistantMessage → OpenAI 格式 |
| T-093 | `test_convert_messages_tool_result` | 单元 | ToolResultMessage → tool role |
| T-094 | `test_convert_messages_thinking_content` | 单元 | thinking block 正确处理 |
| T-095 | `test_map_thinking_level` | 单元 | ThinkingLevel → ReasoningEffort 映射 |
| T-096 | `test_system_prompt_to_developer_role` | 单元 | system prompt → developer/system |
| T-097 | `test_tools_schema_conversion` | 单元 | Tool → OpenAI function 格式 |
| T-098 | `test_compat_overrides_applied` | 单元 | Compat 字段覆盖默认行为 |
| T-099 | `test_normalize_mistral_tool_id` | 单元 | Mistral tool id 规范化 |
| T-100 | `test_stream_openai_real_api` | E2E | 真实 OpenAI API 调用（`#[ignore]`） |
| T-101 | `test_stream_openai_with_tools` | E2E | 真实 API + tool calling（`#[ignore]`） |

#### Anthropic Messages Provider（待实现）

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| T-110 | `test_convert_messages_to_anthropic` | 单元 | 消息格式转换 |
| T-111 | `test_anthropic_system_prompt_handling` | 单元 | system 作为顶层字段 |
| T-112 | `test_anthropic_thinking_blocks` | 单元 | thinking 输入输出格式 |
| T-113 | `test_anthropic_tool_use_format` | 单元 | tool_use block 格式 |
| T-114 | `test_anthropic_tool_result_format` | 单元 | tool_result block 格式 |
| T-115 | `test_anthropic_image_content` | 单元 | base64 image source 格式 |
| T-116 | `test_anthropic_sse_parsing` | 单元 | SSE event → AssistantMessageEvent |
| T-117 | `test_anthropic_stream_real_api` | E2E | 真实 Anthropic API（`#[ignore]`） |

#### Google Generative AI Provider（待实现）

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| T-120 | `test_convert_messages_to_google` | 单元 | 消息格式转换 |
| T-121 | `test_google_system_instruction` | 单元 | system prompt 处理 |
| T-122 | `test_google_function_calling` | 单元 | tool → functionDeclaration |
| T-123 | `test_google_stream_real_api` | E2E | 真实 Google API（`#[ignore]`） |

---

### hand-agent crate 测试用例清单

#### Agent 核心 (`agent.rs`)

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| A-001 | `test_agent_new_default_state` | 单元 | 新建 Agent messages 为空 |
| A-002 | `test_agent_set_system_prompt` | 单元 | 设置后 system_prompt 正确 |
| A-003 | `test_agent_add_tool` | 单元 | 添加后 tools 列表增长 |
| A-004 | `test_agent_clear_messages` | 单元 | 清空后 messages 为空 |
| A-005 | `test_agent_run_basic_text` | 集成 | Mock → run → 返回文本 |
| A-006 | `test_agent_run_with_tool` | 集成 | Mock → run → 执行 tool → 返回结果 |
| A-007 | `test_agent_run_multi_turn` | 集成 | 多次 run → messages 累积 |
| A-008 | `test_agent_messages_immutable_ref` | 单元 | messages() 返回不可变引用 |

#### Agent Loop (`agent_loop.rs`)

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| A-010 | `test_agent_loop_single_turn_text` | 集成 | 单轮文本应答，stop_reason=Stop |
| A-011 | `test_agent_loop_single_turn_tool_call` | 集成 | 单轮 tool call，stop_reason=ToolUse |
| A-012 | `test_agent_loop_continue_until_stop` | 集成 | tool call → tool result → text → 结束 |
| A-013 | `test_agent_loop_max_turns_limit` | 集成 | 达到 max_turns 后强制停止 |
| A-014 | `test_agent_loop_parallel_tool_execution` | 集成 | 多个 tool call 并行执行 |
| A-015 | `test_agent_loop_sequential_tool_execution` | 集成 | 多个 tool call 顺序执行 |
| A-016 | `test_agent_loop_tool_error_propagated` | 集成 | tool 返回 is_error=true 时正确传递 |
| A-017 | `test_agent_loop_empty_tool_list` | 集成 | 无 tool 时不触发 tool call 逻辑 |

#### Agent Hooks

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| A-020 | `test_before_tool_call_hook_called` | 集成 | hook 被正确调用，参数正确 |
| A-021 | `test_before_tool_call_hook_can_cancel` | 集成 | hook 返回 false → 跳过执行 |
| A-022 | `test_after_tool_call_hook_called` | 集成 | 执行后 hook 被调用，result 正确 |
| A-023 | `test_steering_message_injected` | 集成 | steering hook 返回消息被插入 |
| A-024 | `test_follow_up_message_injected` | 集成 | follow-up hook 返回消息被追加 |
| A-025 | `test_hooks_all_none_no_error` | 集成 | 所有 hook 为 None 时正常运行 |

#### Event Sink

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| A-030 | `test_channel_event_sink_emits` | 单元 | emit → receiver 收到事件 |
| A-031 | `test_event_sink_event_order` | 集成 | AgentStart → TurnStart → ... → AgentEnd 顺序 |
| A-032 | `test_event_sink_tool_events` | 集成 | ToolExecutionStart → ToolExecutionEnd 配对 |
| A-033 | `test_event_sink_message_update_events` | 集成 | 含 TextDelta 等流式事件 |

#### Agent Types

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| A-040 | `test_agent_message_serialize_all_variants` | 单元 | User/Assistant/ToolResult/Custom |
| A-041 | `test_tool_result_with_error` | 单元 | is_error=true 的 ToolResult |
| A-042 | `test_tool_result_with_details` | 单元 | details 可选字段 |
| A-043 | `test_agent_event_all_variants` | 单元 | 每个 AgentEvent 变体可构造 |
| A-044 | `test_tool_execution_mode_default` | 单元 | 默认值符合预期 |

---

### hand-tui crate 测试用例清单

#### 差分渲染引擎

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| U-001 | `test_diff_renderer_first_frame_full_render` | 单元 | 首帧完整输出 |
| U-002 | `test_diff_renderer_no_change_no_output` | 单元 | 相同帧无输出 |
| U-003 | `test_diff_renderer_single_line_change` | 单元 | 仅变化行重绘 |
| U-004 | `test_diff_renderer_line_added` | 单元 | 新增行正确处理 |
| U-005 | `test_diff_renderer_line_removed` | 单元 | 删除行正确处理 |
| U-006 | `test_diff_renderer_resize` | 单元 | 终端尺寸变化后全量重绘 |

#### 文本工具

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| U-010 | `test_visible_width_ascii` | 单元 | 纯 ASCII 宽度 = 字符数 |
| U-011 | `test_visible_width_cjk` | 单元 | CJK 字符宽度 = 2 |
| U-012 | `test_visible_width_emoji` | 单元 | Emoji 宽度正确 |
| U-013 | `test_visible_width_ansi_escape_ignored` | 单元 | ANSI 转义序列不计入宽度 |
| U-014 | `test_visible_width_mixed` | 单元 | ASCII + CJK + Emoji 混合 |
| U-015 | `test_truncate_to_width` | 单元 | 截断到指定显示宽度 |
| U-016 | `test_truncate_cjk_boundary` | 单元 | CJK 字符边界不断裂 |

#### 组件 — Input

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| U-020 | `test_input_initial_empty` | 单元 | 初始内容为空 |
| U-021 | `test_input_type_char` | 单元 | 输入字符后内容正确 |
| U-022 | `test_input_backspace` | 单元 | 删除字符 |
| U-023 | `test_input_cursor_movement` | 单元 | 左右移动光标 |
| U-024 | `test_input_home_end` | 单元 | Home/End 跳转 |
| U-025 | `test_input_delete_word` | 单元 | Ctrl+W 删除单词 |
| U-026 | `test_input_render_width` | 单元 | 渲染输出不超过指定宽度 |
| U-027 | `test_input_cursor_position` | 单元 | Focusable 返回正确光标位置 |
| U-028 | `test_input_unicode` | 单元 | CJK/Emoji 输入与显示 |

#### 组件 — Editor

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| U-030 | `test_editor_insert_text` | 单元 | 插入文本 |
| U-031 | `test_editor_newline` | 单元 | 回车换行 |
| U-032 | `test_editor_delete_line` | 单元 | 删除整行 |
| U-033 | `test_editor_undo_redo` | 单元 | 撤销/重做 |
| U-034 | `test_editor_selection` | 单元 | 选区创建和删除 |
| U-035 | `test_editor_copy_paste` | 单元 | kill ring 操作 |
| U-036 | `test_editor_scroll` | 单元 | 超出视口时滚动 |
| U-037 | `test_editor_syntax_highlight` | 单元 | 语法高亮输出含 ANSI |
| U-038 | `test_editor_large_file` | 单元 | 大文件不 panic（性能边界） |

#### 组件 — Markdown

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| U-040 | `test_markdown_heading` | 单元 | # 标题渲染 |
| U-041 | `test_markdown_code_block` | 单元 | 代码块含语法高亮 |
| U-042 | `test_markdown_inline_code` | 单元 | 行内代码 |
| U-043 | `test_markdown_bold_italic` | 单元 | **bold** / *italic* |
| U-044 | `test_markdown_list` | 单元 | 有序/无序列表 |
| U-045 | `test_markdown_link` | 单元 | 链接渲染 |
| U-046 | `test_markdown_table` | 单元 | 表格对齐 |
| U-047 | `test_markdown_empty_input` | 单元 | 空字符串不 panic |

#### 组件 — SelectList

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| U-050 | `test_select_list_render_items` | 单元 | 正确渲染列表 |
| U-051 | `test_select_list_navigate_down` | 单元 | 下移选中项 |
| U-052 | `test_select_list_navigate_up` | 单元 | 上移选中项 |
| U-053 | `test_select_list_wrap_around` | 单元 | 到底后回到顶部 |
| U-054 | `test_select_list_select_item` | 单元 | 回车选中 |
| U-055 | `test_select_list_empty` | 单元 | 空列表不 panic |

---

### hand-coding-agent crate 测试用例清单

#### Tools — Read

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| C-001 | `test_read_existing_file` | 集成 | 读取已有文件返回内容 |
| C-002 | `test_read_nonexistent_file` | 集成 | 文件不存在返回错误 |
| C-003 | `test_read_with_line_range` | 集成 | start_line/end_line 截取正确 |
| C-004 | `test_read_binary_file` | 集成 | 二进制文件提示而非崩溃 |
| C-005 | `test_read_large_file_truncation` | 集成 | 超大文件自动截断 |
| C-006 | `test_read_utf8_file` | 集成 | UTF-8 编码文件正确读取 |
| C-007 | `test_read_empty_file` | 集成 | 空文件返回空内容 |
| C-008 | `test_read_symlink` | 集成 | 符号链接正确跟随 |
| C-009 | `test_read_permission_denied` | 集成 | 无权限文件返回错误 |

#### Tools — Write

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| C-010 | `test_write_new_file` | 集成 | 创建新文件 |
| C-011 | `test_write_overwrite_existing` | 集成 | 覆盖已有文件 |
| C-012 | `test_write_creates_parent_dirs` | 集成 | 自动创建父目录 |
| C-013 | `test_write_utf8_content` | 集成 | 写入含中文内容 |
| C-014 | `test_write_empty_content` | 集成 | 写入空字符串 |
| C-015 | `test_write_preserves_permissions` | 集成 | 不改变文件权限 |

#### Tools — Edit

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| C-020 | `test_edit_replace_single_occurrence` | 集成 | 替换唯一匹配 |
| C-021 | `test_edit_replace_all` | 集成 | replace_all=true 替换全部 |
| C-022 | `test_edit_no_match_returns_error` | 集成 | 无匹配返回错误 |
| C-023 | `test_edit_ambiguous_match_returns_error` | 集成 | 多处匹配且非 replace_all 返回错误 |
| C-024 | `test_edit_preserves_surrounding_content` | 集成 | 替换不影响其他内容 |
| C-025 | `test_edit_multiline_old_string` | 集成 | 跨行替换 |
| C-026 | `test_edit_preserves_indentation` | 集成 | 保持缩进 |

#### Tools — Bash

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| C-030 | `test_bash_simple_command` | 集成 | `echo hello` 返回 "hello" |
| C-031 | `test_bash_exit_code_zero` | 集成 | 成功命令 exit code 0 |
| C-032 | `test_bash_exit_code_nonzero` | 集成 | 失败命令返回非零 + stderr |
| C-033 | `test_bash_timeout` | 集成 | 超时命令被终止 |
| C-034 | `test_bash_working_directory` | 集成 | 在指定目录执行 |
| C-035 | `test_bash_env_variables` | 集成 | 环境变量传递 |
| C-036 | `test_bash_pipe_command` | 集成 | 管道命令执行 |
| C-037 | `test_bash_output_truncation` | 集成 | 超长输出截断 |
| C-038 | `test_bash_dangerous_command_blocked` | 集成 | 危险命令（rm -rf /）拦截 |

#### Tools — Find / Grep / Ls

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| C-040 | `test_find_by_glob_pattern` | 集成 | `**/*.rs` 匹配 |
| C-041 | `test_find_respects_gitignore` | 集成 | .gitignore 中的文件不出现 |
| C-042 | `test_find_empty_result` | 集成 | 无匹配返回空 |
| C-043 | `test_grep_literal_match` | 集成 | 精确字符串搜索 |
| C-044 | `test_grep_regex_match` | 集成 | 正则表达式搜索 |
| C-045 | `test_grep_with_context_lines` | 集成 | -C 参数上下文行 |
| C-046 | `test_grep_no_match` | 集成 | 无匹配返回空 |
| C-047 | `test_ls_directory` | 集成 | 列出目录内容 |
| C-048 | `test_ls_nonexistent_dir` | 集成 | 不存在目录返回错误 |
| C-049 | `test_ls_hidden_files` | 集成 | 是否列出隐藏文件 |

#### Session Manager

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| C-060 | `test_session_save_and_load` | 集成 | 保存后加载内容一致 |
| C-061 | `test_session_list` | 集成 | 列出所有 session |
| C-062 | `test_session_branch` | 集成 | 从已有 session 分支 |
| C-063 | `test_session_save_overwrites` | 集成 | 同 id 保存覆盖旧内容 |
| C-064 | `test_session_load_nonexistent` | 集成 | 不存在的 session 返回错误 |
| C-065 | `test_session_persistence_across_restart` | 集成 | 文件系统持久化验证 |
| C-066 | `test_session_concurrent_access` | 集成 | 并发读写不损坏数据 |

#### Compaction（对话压缩）

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| C-070 | `test_compact_short_conversation_unchanged` | 集成 | 短对话不触发压缩 |
| C-071 | `test_compact_long_conversation_reduced` | 集成 | 长对话压缩后 token 减少 |
| C-072 | `test_compact_preserves_recent_messages` | 集成 | 最近 N 条消息不被压缩 |
| C-073 | `test_compact_preserves_system_prompt` | 集成 | system prompt 保留 |
| C-074 | `test_compact_summary_is_valid_message` | 集成 | 压缩摘要是合法 Message |

#### Extension System

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| C-080 | `test_extension_register_and_list` | 单元 | 注册后可列出 |
| C-081 | `test_extension_before_tool_hook_called` | 集成 | 钩子正确触发 |
| C-082 | `test_extension_after_tool_hook_called` | 集成 | 钩子正确触发 |
| C-083 | `test_extension_custom_command_registered` | 单元 | 自定义命令可注册 |
| C-084 | `test_extension_multiple_extensions` | 集成 | 多个 extension 按顺序调用 |

#### Settings Manager

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| C-090 | `test_settings_load_default` | 单元 | 默认配置合理 |
| C-091 | `test_settings_save_and_load` | 集成 | 保存后加载一致 |
| C-092 | `test_settings_update_single_field` | 集成 | 更新单个字段不丢失其他 |
| C-093 | `test_settings_invalid_json_fallback` | 集成 | 损坏文件回退默认值 |

#### CLI 参数解析

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| C-100 | `test_cli_no_args_interactive_mode` | 单元 | 无参数 → Interactive |
| C-101 | `test_cli_prompt_flag` | 单元 | `--prompt "..."` 解析 |
| C-102 | `test_cli_model_flag` | 单元 | `--model xxx` 解析 |
| C-103 | `test_cli_mode_print` | 单元 | `--mode print` 解析 |
| C-104 | `test_cli_unknown_flag_error` | 单元 | 未知参数报错 |

#### 端到端 Agent 会话

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| C-110 | `test_e2e_simple_question` | E2E | 提问 → 文本回答（`#[ignore]`） |
| C-111 | `test_e2e_read_file_tool` | E2E | 要求读文件 → 调用 read tool（`#[ignore]`） |
| C-112 | `test_e2e_edit_file_tool` | E2E | 要求修改文件 → 调用 edit tool（`#[ignore]`） |
| C-113 | `test_e2e_multi_tool_chain` | E2E | 连续多个 tool call（`#[ignore]`） |

---

### 边界与异常测试（跨 crate 通用）

| 编号 | 测试用例 | 分类 | 说明 |
|------|---------|------|------|
| X-001 | `test_empty_messages_list` | 单元 | 空 messages 不 panic |
| X-002 | `test_empty_string_inputs` | 单元 | 空字符串作为各字段 |
| X-003 | `test_very_long_string_input` | 单元 | 超长字符串（100KB+）不 OOM |
| X-004 | `test_unicode_edge_cases` | 单元 | ZWJ、变体选择器、组合字符 |
| X-005 | `test_concurrent_stream_access` | 集成 | 多个 stream 并发运行 |
| X-006 | `test_serde_unknown_field_ignored` | 单元 | JSON 中多余字段不报错 |
| X-007 | `test_serde_missing_optional_field` | 单元 | 缺少 Option 字段默认 None |
| X-008 | `test_serde_missing_required_field_error` | 单元 | 缺少必填字段报错 |
| X-009 | `test_invalid_json_returns_error` | 单元 | 畸形 JSON 不 panic |
| X-010 | `test_null_json_value_handling` | 单元 | JSON null → None 或合理默认 |

---

### 测试执行与 CI 规范

#### 本地运行

```bash
# 全量单元+集成测试（不含 E2E）
cargo test --workspace

# 单个 crate
cargo test -p hand-model
cargo test -p hand-agent
cargo test -p hand-tui
cargo test -p hand-coding-agent

# E2E 测试（需要 API key）
cargo test --workspace -- --ignored

# 带输出
cargo test --workspace -- --nocapture

# 指定用例
cargo test -p hand-model test_calculate_cost
```

#### CI Pipeline（GitHub Actions）

```yaml
name: Test
on: [push, pull_request]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Check formatting
        run: cargo fmt --all -- --check
      - name: Clippy lint
        run: cargo clippy --workspace --all-targets -- -D warnings
      - name: Unit & Integration tests
        run: cargo test --workspace
      - name: Doc tests
        run: cargo test --workspace --doc

  e2e:
    runs-on: ubuntu-latest
    if: github.event_name == 'push' && github.ref == 'refs/heads/main'
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: E2E tests
        env:
          OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}
          ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
        run: cargo test --workspace -- --ignored
```

#### 质量门禁

| 指标 | 要求 |
|------|------|
| `cargo test --workspace` | 全部通过 |
| `cargo clippy -- -D warnings` | 零警告 |
| `cargo fmt -- --check` | 格式合规 |
| `cargo doc --no-deps` | 文档构建无错误 |
| 每个 `pub fn` | 至少 1 个对应测试 |
| 每个 `pub struct/enum` | 至少 serialize/deserialize roundtrip 测试 |
| 每个 error variant | 至少 1 个触发该 variant 的测试 |

---

## 实施顺序与依赖关系

```
阶段 0: model crate 补全
    │   (补全 Anthropic/Google provider)
    ▼
阶段 1: agent crate
    │   (依赖 model)
    │
    ├──────────────┐
    ▼              ▼
阶段 2: tui     阶段 3.1: coding-agent 核心
 crate           (tools, session, 不含 TUI)
    │              │
    └──────┬───────┘
           ▼
    阶段 3.2: coding-agent 交互模式
           │ (集成 TUI)
           ▼
    阶段 4: 集成完善
           (compaction, extensions, skills)
```

**关键路径**: model → agent → coding-agent core → coding-agent interactive

**可并行**:
- TUI crate 可与 agent crate 同时开发
- coding-agent 的 tools 实现可与 agent loop 同时开发

---

## 估计工作量分布

| 阶段 | 复杂度 | 说明 |
|------|--------|------|
| 阶段 0 | 中 | 主要是 HTTP API 对接，模式已有 |
| 阶段 1 | 中 | 核心架构，但代码量不大 |
| 阶段 2 | 高 | 终端渲染复杂度高，尤其 Editor 组件 |
| 阶段 3 | 很高 | 最大的包，功能密集 |
| 阶段 4 | 中 | 打磨与集成 |

---

## 开发约定

1. **每个阶段完成后运行 `cargo clippy` + `cargo test`**
2. **类型优先**：先定义类型和 trait，再实现
3. **增量验证**：每实现一个 provider/tool 就写测试
4. **文档**：公开 API 必须有 `///` doc comment
5. **错误处理**：每个 crate 有自己的 Error enum，使用 `thiserror`
6. **日志**：统一使用 `tracing` crate
7. **Feature flags**：Provider 用 feature gate，按需编译
   ```toml
   [features]
   default = ["openai", "anthropic"]
   openai = []
   anthropic = []
   google = []
   all-providers = ["openai", "anthropic", "google", "bedrock", "azure"]
   ```
