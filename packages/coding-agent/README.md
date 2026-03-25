# hand-coding-agent

交互式编码代理，对外提供 `hand` 命令。

这个包提供终端交互、会话持久化、上下文压缩和内建文件工具。

## Quick Start

```bash
cd packages/coding-agent
cargo run --bin hand
```

单次执行：

```bash
cargo run --bin hand -- --prompt "Explain the current project structure"
```

非交互输出模式：

```bash
cargo run --bin hand -- --print --prompt "Summarize src/main.rs"
printf 'Review this repo' | cargo run --bin hand -- --print
```

## CLI 选项

当前 `hand` 支持以下参数：

- `-p`, `--prompt <TEXT>`：初始 prompt
- `-m`, `--model <MODEL>`：模型 ID
- `--provider <PROVIDER>`：provider 名称
- `--resume <SESSION_ID>`：恢复会话
- `-d`, `--cwd <DIR>`：工作目录
- `-v`, `--verbose`：启用详细日志
- `--print`：非交互模式
- `--system-prompt <TEXT>`：覆盖默认 system prompt

默认 provider 为 `anthropic`，默认模型为 `claude-sonnet-4-20250514`。实际可用性取决于 `packages/model` 中已注册的 provider 实现和本地环境变量配置。

## 运行模式

### Interactive

不传 `--print` 时，`hand` 会进入 REPL 风格交互模式。

内建命令：

- `/help`
- `/quit` / `/exit` / `/q`
- `/model`
- `/session`

### Print

传入 `--print` 后：

- 若提供 `--prompt`，处理单次输入后退出
- 否则从标准输入读取全部内容并处理后退出

## 内建工具

当前默认注册 7 个工具：

- `read`
- `write`
- `edit`
- `bash`
- `grep`
- `find`
- `ls`

系统提示词会根据已启用工具自动生成对应的使用约束，例如优先使用 `read` 查看文件、优先用 `edit` 修改已有文件、优先用 `grep`/`find`/`ls` 做搜索与遍历。

## 会话

会话由 `SessionManager` 以 JSONL 格式持久化。

- 目录：`<cwd>/.hand/sessions/`
- 文件名：`<session_id>.jsonl`
- 记录类型：`session`、`message`、`model_change`、`compaction`、`label`

恢复会话：

```bash
cargo run --bin hand -- --resume s_xxx_xxx
```

## 上下文压缩

`AgentSession` 会在上下文过长时触发 compaction：

- 保留最近消息
- 生成压缩摘要记录到 session
- 从最近一次压缩点之后重建上下文

相关配置由 `SettingsManager` 提供，默认启用。

## 设置文件

当前设置来源有两层：

- 全局：`~/.hand/agent/settings.json`
- 项目：`<cwd>/.hand/settings.json`

合并后的设置项包括：

- `default_provider`
- `default_model`
- `default_thinking_level`
- `shell_path`
- `shell_command_prefix`
- `theme`
- `compaction`
- `retry`
- `quiet_startup`

## 上下文文件

启动时会读取以下项目上下文文件，并注入 system prompt：

- `HAND.md`
- `.hand/context.md`

上下文文件加载路径为这两个位置。

## 输出事件

运行时，`AgentSession` 会把底层 `AgentEvent` 转发给订阅者，用于：

- 实时输出文本流
- 展示 thinking 片段
- 展示工具执行开始/结束
- 显示压缩开始/结束事件

CLI 主程序就是通过 `session.subscribe(...)` 渲染这些事件。

## 作为库使用

```rust
use hand_coding_agent::{AgentSession, AgentSessionEvent};
```

你可以：

- 自己创建 `AgentSessionConfig`
- 复用 `tools::create_default_tools()` 或传入自定义工具
- 通过 `subscribe()` 接管事件渲染
- 调用 `send_message()` 驱动一次 agent loop

## 开发

```bash
cd packages/coding-agent
cargo check
cargo test
```

相关源码分布：

- `src/main.rs`：CLI 入口
- `src/core/agent_session.rs`：会话生命周期与事件转发
- `src/core/session_manager.rs`：JSONL 会话存储
- `src/core/settings.rs`：全局/项目设置加载与合并
- `src/core/system_prompt.rs`：system prompt 与上下文文件加载
- `src/tools/`：内建工具实现

## License

MIT
