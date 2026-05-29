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

全部里程碑（M0–M12）已完成：聊天外壳、消息/工具渲染、沙箱运行时、artifacts
（9 种 viewer + 浏览器工具执行）、浏览器工具（JS REPL、extract_document）、附件
（解析 + tile + overlay）、IndexedDB 存储、provider/模型选择、对话框/设置/顶栏、
上传下载与附件投递、i18n/主题，以及单二进制打包。详见上述 ExecPlan 的能力对齐矩阵。

## 运行

### 单二进制（自包含 release）

```bash
scripts/build-web-ui.sh        # 构建前端 (Vite) 并 cargo build --release，内嵌前端资源
./target/release/hand-web-ui   # 打开打印出的 http://127.0.0.1:<port>
```

release 二进制通过 `rust-embed` 内嵌 `web/dist`，无需任何外部文件，可在任意目录运行。

### 开发（前端热更新 + 真实后端）

```bash
# 终端 1：Rust server。--web-dir 指向磁盘上的前端目录即进入“从磁盘提供资源”模式
cargo run -p hand-web-ui -- --web-dir crates/web-ui/web/dist

# 终端 2：Vite 开发服务器（代理 /ws、/upload、/download 到 Rust server）
npm --prefix crates/web-ui/web install
npm --prefix crates/web-ui/web run dev
```

provider API key 从 server 进程环境读取（绝不下发到浏览器）。可用 `--model` /
`--provider` 覆盖默认模型。不带 `--web-dir` 运行时使用内嵌资源；内嵌资源缺失时
`/` 回退到内置连通性探测页。

## License

MIT
