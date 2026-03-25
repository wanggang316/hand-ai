# hand-ai

Rust 原生的 AI 工具集合，用于统一接入大模型、构建带工具调用的 Agent，以及实现交互式编码代理。

编码代理使用说明见 `packages/coding-agent/README.md`。

## Packages

| Package | Description |
|---------|-------------|
| `packages/model` | 统一的模型目录、消息类型、流式客户端和模型查询工具 |
| `packages/agent` | Agent loop、工具执行、事件流和状态管理 |
| `packages/coding-agent` | `hand` 命令行编码代理，带会话、上下文压缩和内建工具 |
| `packages/tui` | Rust 终端 UI 组件库与差量渲染器 |
| `packages/web-ui` | Web UI 说明文档 |

## 仓库结构

- `packages/model`：底层 LLM 抽象，定义 `Context`、`Message`、`Model`、`Client`
- `packages/agent`：在 `model` 之上实现通用 agent loop 和工具调用
- `packages/coding-agent`：面向终端用户的交互式 coding agent
- `packages/tui`：供终端界面复用的组件与渲染基础设施
- `examples`：工作区示例代码

## 开发

```bash
# 检查整个 workspace
cargo check --workspace

# 运行整个 workspace 的测试
cargo test --workspace

# 按仓库脚本执行检查
./check.sh
```

按包检查：

```bash
cd packages/<name>
cargo check
cargo test
```

## 约定

- README 描述以当前代码实现为准
- 代码与测试规则以仓库说明和当前任务要求为准
- 修改功能时，优先同步更新对应包的 README

## License

MIT
