# hand-web-ui

浏览器端 AI 聊天界面：一个 TypeScript + Lit 前端，由本地 Rust server（axum +
WebSocket）驱动。server 以库依赖方式复用工作区已有的原生 agent / model / RPC
能力，每个 WebSocket 连接持有一个 in-process 的 `AgentSession`；浏览器通过
WebSocket 与之通信，并保留全部浏览器专属能力（HTML 沙箱、PDF/DOCX/XLSX 预览、
JS REPL、IndexedDB、附件解析）。

设计与计划见：

- 架构规范：[`docs/web-ui-architecture.md`](../../docs/web-ui-architecture.md)
- 实施计划与能力对齐矩阵：[`docs/exec-plans/web-ui.md`](../../docs/exec-plans/web-ui.md)

## 当前状态

M0（脚手架与端到端打通）已完成：

- `hand-web-ui` 二进制 crate 已加入 workspace，`cargo check` / `cargo clippy` 通过；
- 前端工程（Vite + Tailwind v4 + TypeScript）`tsc --noEmit` 与 `vite build` 通过；
- `/ws` 将 WebSocket 桥接到既有的 JSONL RPC 派发器（`run_rpc_server`），命令派发、
  事件转发、中断 race 全部原样复用；
- 一条 `prompt` 可端到端流式返回助手回复。

后续里程碑（聊天外壳、消息/工具渲染、artifacts、沙箱、附件、存储、provider 管理、
对话框、代理/上传下载、i18n/主题、打包）见上述 ExecPlan。

## 运行

### 开发（前端热更新 + 真实后端）

```bash
# 终端 1：启动 Rust server（默认 127.0.0.1:4137）
cargo run -p hand-web-ui

# 终端 2：启动 Vite 开发服务器（代理 /ws 到 Rust server）
npm --prefix crates/web-ui/web install
npm --prefix crates/web-ui/web run dev
```

provider API key 从 server 进程环境读取（绝不下发到浏览器）。可用 `--model` /
`--provider` 覆盖默认模型。

### 单进程冒烟

```bash
npm --prefix crates/web-ui/web run build   # 产出 web/dist
cargo run -p hand-web-ui                    # 直接打开打印出的 http://127.0.0.1:<port>
```

未构建前端时，`/` 会回退到一个内置连通性探测页，直接演示流式 seam。

## License

MIT
