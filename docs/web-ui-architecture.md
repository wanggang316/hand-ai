# Web UI Architecture

> Target architecture for `crates/web-ui`: a de-branded TypeScript + Lit browser
> frontend that reproduces the full capability set of the reference web UI, driven
> by a new local Rust server (axum + WebSocket) that wraps the workspace's existing
> native agent / model / RPC layer. No native Rust is rewritten; the browser talks
> to the server, and the server reuses `hand-coding-agent`'s session and RPC code
> unchanged.

---

## 1. Overview and Runtime Topology

The web UI is a two-process system that runs entirely on the user's machine.

```
┌──────────────────────────────────────────────────────────────────────────┐
│  Browser (TypeScript + Lit)                                                │
│                                                                            │
│  ┌──────────────┐   ┌───────────────────┐   ┌──────────────────────────┐  │
│  │ Chat Shell   │   │ Artifacts Panel   │   │ Dialogs / Settings       │  │
│  │ (Lit views)  │   │ (HTML sandbox,    │   │ (model picker, sessions, │  │
│  │              │   │  PDF/DOCX/XLSX)    │   │  provider keys, proxy)   │  │
│  └──────┬───────┘   └─────────┬─────────┘   └────────────┬─────────────┘  │
│         │                     │                          │                 │
│         └──────────┬──────────┴──────────────────────────┘                 │
│                    │                                                        │
│            ┌───────▼────────┐         ┌──────────────────────────────┐     │
│            │ RemoteAgent    │         │ IndexedDB (sessions, keys,    │     │
│            │ (event mirror) │         │  settings, custom providers)  │     │
│            └───────┬────────┘         └──────────────────────────────┘     │
└────────────────────┼───────────────────────────────────────────────────────┘
                     │  WebSocket (JSON text frames)
                     │  HTTP    (static assets, file upload/download)
┌────────────────────▼───────────────────────────────────────────────────────┐
│  Local Rust server  (crate: hand-web-ui, bin: hand-web-ui)                   │
│                                                                              │
│  ┌──────────────┐  ┌───────────────────┐  ┌─────────────────────────────┐   │
│  │ axum router  │  │ WS connection task │  │ rust-embed static assets    │   │
│  │ + tower-http │  │ (one AgentSession  │  │ (bundled Vite build output) │   │
│  │              │  │  per connection)   │  │                             │   │
│  └──────────────┘  └─────────┬─────────┘  └─────────────────────────────┘   │
│                              │                                               │
│              ┌───────────────▼────────────────────────────────┐             │
│              │  hand-coding-agent (reused unchanged)            │             │
│              │  AgentSession · run_rpc_server core · ModelReg.  │             │
│              │  · hand-agent (AgentEvent) · model (catalog)     │             │
│              └─────────────────────────────────────────────────┘             │
└──────────────────────────────────────────────────────────────────────────────┘
```

Key properties:

- **Single local binary.** The Rust server is a standalone binary that serves the
  built frontend assets (embedded via `rust-embed`) on an HTTP route and upgrades a
  WebSocket on a known path (`/ws`). Running the binary and opening the printed
  `http://127.0.0.1:<port>` URL is the entire deploy story; there is no separate
  static host.
- **One session per WebSocket connection.** Each browser tab opens one WebSocket;
  the server owns exactly one `AgentSession` for that connection. Concurrent tabs
  are independent sessions, matching the existing single-task-per-session model of
  `run_rpc_server`.
- **The browser keeps all rendering and browser-only capabilities.** Lit
  components, the HTML artifact sandbox iframe, attachment parsing
  (`pdfjs-dist` / `xlsx` / `docx-preview` / `jszip`), the JavaScript REPL sandbox,
  IndexedDB persistence, and syntax highlighting all stay client-side.
- **The server owns the agent loop.** LLM streaming, tool execution orchestration,
  model registry, compaction, bash execution, slash commands, extension dispatch,
  and API-key resolution all live in the reused native Rust crates. The browser's
  in-process "Agent" of the reference UI is replaced by a thin `RemoteAgent` proxy.

---

## 2. Frontend Module Layout and Local Type System

### 2.1 Directory tree under `crates/web-ui/web/`

The frontend is a self-contained TypeScript project. It mirrors the analyzed
subsystems but uses hand-ai-native, brand-neutral names. No external agent/model
TypeScript packages are imported; the types those packages used to provide are
redefined locally under `src/core/`.

```
crates/web-ui/web/
├── index.html                     # Vite entry HTML, mounts <hand-app>
├── package.json
├── tsconfig.json
├── vite.config.ts                 # @tailwindcss/vite plugin + pdf worker asset
├── tailwind.config.ts             # Tailwind v4 config (or CSS-first @theme)
├── public/
│   └── pdf.worker.min.mjs          # pdfjs-dist worker, served as static asset
└── src/
    ├── main.ts                     # App bootstrap (see §2.4)
    ├── app.css                     # Tailwind v4 entry + design tokens + keyframes
    │
    ├── core/                       # Local de-branded type system (replaces removed
    │   │                           #   agent-core / ai TS packages — see §2.2)
    │   ├── messages.ts             # AgentMessage union, ContentBlock, ToolCall,
    │   │                           #   ToolResultMessage, Usage, custom roles
    │   ├── model.ts                # Model<T>, Api, ThinkingLevel, cost types
    │   ├── agent.ts                # Agent interface + AgentEvent union (UI-facing)
    │   ├── convert.ts              # defaultConvertToLlm, convertAttachments, guards
    │   └── tool.ts                 # AgentTool interface (local execute tools)
    │
    ├── client/                     # Server bridge (see §3 and §5)
    │   ├── remote-agent.ts         # RemoteAgent — event-emitting Agent proxy
    │   ├── ws-connection.ts        # WebSocket lifecycle, reconnect, framing
    │   ├── wire.ts                 # Envelope + client→/server→ message types
    │   └── upload.ts               # HTTP attachment upload helper
    │
    ├── shell/                      # chat-shell subsystem
    │   ├── chat-panel.ts           # <hand-chat-panel> layout orchestrator
    │   ├── agent-interface.ts      # <agent-interface> conversational shell
    │   ├── message-list.ts         # <message-list> stable history renderer
    │   ├── streaming-message-container.ts  # <streaming-message-container>
    │   ├── message-editor.ts       # <message-editor> input widget
    │   └── messages/               # message-tool-rendering subsystem
    │       ├── user-message.ts
    │       ├── assistant-message.ts
    │       ├── tool-message.ts
    │       ├── tool-message-debug.ts
    │       ├── aborted-message.ts
    │       ├── thinking-block.ts
    │       ├── message-renderer-registry.ts
    │       └── index.ts
    │
    ├── tools/                      # tool renderers + browser-only tools
    │   ├── renderer-registry.ts    # renderTool, register/getToolRenderer, headers
    │   ├── bash.ts                 # BashRenderer
    │   ├── calculate.ts            # CalculateRenderer
    │   ├── default.ts              # DefaultRenderer
    │   ├── get-current-time.ts     # GetCurrentTimeRenderer
    │   ├── javascript-repl.ts      # browser-only: tool + renderer (auto-register)
    │   ├── extract-document.ts     # browser-only: tool + renderer (auto-register)
    │   └── index.ts                # side-effect imports → registration
    │
    ├── artifacts/                  # artifacts subsystem (all client-side)
    │   ├── artifacts-panel.ts      # <artifacts-panel>
    │   ├── artifact-element.ts     # ArtifactElement abstract base
    │   ├── html-artifact.ts        # <html-artifact> (sandbox iframe)
    │   ├── svg-artifact.ts
    │   ├── markdown-artifact.ts
    │   ├── text-artifact.ts
    │   ├── image-artifact.ts
    │   ├── pdf-artifact.ts
    │   ├── docx-artifact.ts
    │   ├── excel-artifact.ts
    │   ├── generic-artifact.ts
    │   ├── console.ts              # <artifact-console>
    │   ├── artifact-pill.ts
    │   ├── artifacts-tool-renderer.ts
    │   └── file-type.ts            # getFileType dispatch table
    │
    ├── sandbox/                    # sandbox-runtime subsystem (browser-only)
    │   ├── sandboxed-iframe.ts     # <sandbox-iframe> (execute + loadContent)
    │   ├── runtime-message-router.ts  # RUNTIME_MESSAGE_ROUTER singleton
    │   ├── runtime-message-bridge.ts  # injectable bridge code generator
    │   └── providers/
    │       ├── console-provider.ts        # ConsoleRuntimeProvider (required)
    │       ├── artifacts-provider.ts      # ArtifactsRuntimeProvider
    │       ├── attachments-provider.ts    # AttachmentsRuntimeProvider
    │       └── file-download-provider.ts  # FileDownloadRuntimeProvider
    │
    ├── attachments/                # attachments subsystem (browser-only)
    │   ├── attachment-utils.ts     # loadAttachment + per-format processors
    │   ├── attachment-tile.ts      # <attachment-tile>
    │   └── attachment-overlay.ts   # <attachment-overlay>
    │
    ├── storage/                    # IndexedDB persistence (browser-only)
    │   ├── backend.ts              # StorageBackend / StorageTransaction interfaces
    │   ├── indexeddb-backend.ts    # IndexedDBStorageBackend
    │   ├── store.ts                # Store abstract base
    │   ├── app-storage.ts          # AppStorage + get/setAppStorage singleton
    │   ├── settings-store.ts
    │   ├── provider-keys-store.ts
    │   ├── sessions-store.ts
    │   └── custom-providers-store.ts
    │
    ├── providers/                  # providers-models subsystem
    │   ├── model-selector.ts       # <hand-model-selector> picker dialog
    │   ├── providers-models-tab.ts # <providers-models-tab>
    │   ├── custom-provider-card.ts
    │   ├── custom-provider-dialog.ts
    │   ├── provider-key-input.ts
    │   └── discovery.ts            # local-server model auto-discovery (Ollama, etc.)
    │
    ├── dialogs/                    # dialogs-settings subsystem
    │   ├── settings-dialog.ts      # <settings-dialog>
    │   ├── session-list-dialog.ts  # <session-list-dialog>
    │   ├── api-key-prompt-dialog.ts
    │   ├── persistent-storage-dialog.ts
    │   ├── api-keys-tab.ts
    │   ├── proxy-tab.ts
    │   └── settings-tab.ts         # SettingsTab abstract base
    │
    ├── ui/                         # de-branded mini-lit equivalents
    │   ├── icons.ts                # lucide icon() helper
    │   ├── button.ts               # Button, CopyButton, DownloadButton
    │   ├── input.ts                # Input fc()
    │   ├── select.ts, switch.ts, badge.ts, label.ts
    │   ├── dialog-base.ts          # DialogBase modal base
    │   ├── markdown-block.ts       # <markdown-block>
    │   ├── code-block.ts           # <code-block>
    │   ├── console-block.ts        # <console-block>
    │   ├── diff.ts                 # <diff> / Diff
    │   ├── preview-code-toggle.ts
    │   ├── expandable-section.ts   # <expandable-section>
    │   └── theme-toggle.ts
    │
    ├── prompts/
    │   └── prompts.ts              # tool description constants (verbatim content)
    │
    └── utils/
        ├── format.ts              # formatUsage, formatCost, formatTokenCount, ...
        ├── i18n.ts                # i18n(), setLanguage(), translations (en/de)
        └── cors.ts                # isCorsError (extract-document fallback only)
```

Notes:

- The reference UI's `createStreamFn` / `applyProxyIfNeeded` / `shouldUseProxyForProvider`
  client-side LLM-call layer is **not ported**. LLM streaming happens on the server,
  so there is no browser-side stream function and no client-side CORS proxy decision.
  `isCorsError` survives only because `extract_document` still fetches documents from
  the browser and needs to surface a helpful fallback message.
- `ui/` is the brand-neutral replacement for the reference UI's shared Lit helper
  package. Either vendor the small set of helpers used (`fc`, `icon`, `Button`,
  `Select`, `Badge`, `DialogBase`, `markdown-block`, `code-block`, `Diff`) or wrap
  `lucide` + `lit` directly. No external helper package is referenced.

### 2.2 Local TypeScript type system (`src/core/`)

The removed agent/model TS packages are replaced by local, de-branded types that
are **structurally compatible** with the JSON the Rust server emits (the existing
RPC types in `hand-coding-agent` already serialize camelCase). These types are the
single source of truth for the frontend; nothing imports an external agent or model
package.

```ts
// src/core/model.ts
export type Api =
  | "anthropic-messages"
  | "openai-completions"
  | "openai-responses"
  | "google-generative-ai"
  | string;

export type ThinkingLevel = "off" | "minimal" | "low" | "medium" | "high";

export interface ModelCost {
  input: number; output: number; cacheRead: number; cacheWrite: number;
}

export interface Model<A extends Api = Api> {
  id: string;
  name: string;
  api: A;
  provider: string;
  baseUrl?: string;
  reasoning: boolean;          // drives the thinking-level selector
  input: ("text" | "image")[];
  contextWindow: number;
  maxTokens: number;
  cost?: ModelCost;
}
```

```ts
// src/core/messages.ts
export interface TextContent { type: "text"; text: string; }
export interface ImageContent { type: "image"; data: string; mimeType: string; }
export type ContentBlock = TextContent | ImageContent | ThinkingContent | ToolCall;

export interface ToolCall {
  type: "toolCall";
  id: string;
  name: string;
  arguments: unknown;
}

export interface Usage {
  input: number; output: number; cacheRead: number; cacheWrite: number;
  totalTokens: number;
  cost: { input: number; output: number; cacheRead: number; cacheWrite: number; total: number };
}

export interface UserMessage { role: "user"; content: string | ContentBlock[]; timestamp?: number; }
export interface AssistantMessage {
  role: "assistant";
  content: ContentBlock[];
  usage?: Usage;
  stopReason?: "stop" | "aborted" | "error" | "toolUse";
  model?: string;
}
export interface ToolResultMessage<D = unknown> {
  role: "toolResult";
  toolCallId: string;
  content: { type: "text" | "image"; text?: string }[];
  isError: boolean;
  details?: D;
}

// UI-only roles layered on top via a CustomAgentMessages extension point.
export interface UserMessageWithAttachments {
  role: "user-with-attachments";
  content: string | (TextContent | ImageContent)[];
  timestamp: number;
  attachments?: Attachment[];
}
export interface ArtifactMessage {
  role: "artifact";
  action: "create" | "update" | "delete";
  filename: string;
  content?: string;
  title?: string;
  timestamp: string;
}

// Declaration-merging extension point so consumer code can add roles
// (mirrors the reference CustomAgentMessages augmentation pattern).
export interface CustomAgentMessages {
  "user-with-attachments": UserMessageWithAttachments;
  "artifact": ArtifactMessage;
}

export type AgentMessage =
  | UserMessage | AssistantMessage | ToolResultMessage
  | CustomAgentMessages[keyof CustomAgentMessages];

export type MessageRole = AgentMessage["role"];
```

`ThinkingLevel` maps to the server's `ThinkingLevel` enum
(`Minimal/Low/Medium/High/Xhigh`); the UI's `"off"` is the absence of a level and is
sent as a clear/disable on the wire. `Attachment` is defined in
`src/attachments/attachment-utils.ts` and matches the reference shape
(`id, type, fileName, mimeType, size, content base64, extractedText?, preview?`).

### 2.3 UI-facing Agent contract (`src/core/agent.ts`)

```ts
export type AgentEvent =
  | { type: "agent_start" }
  | { type: "turn_start" }
  | { type: "message_start"; message: AssistantMessage }
  | { type: "message_update"; message: AssistantMessage; isStreaming: boolean }
  | { type: "message_end"; message: AssistantMessage }
  | { type: "turn_end" }
  | { type: "agent_end"; stopReason: string };

export interface AgentState {
  messages: AgentMessage[];
  model: Model;
  thinkingLevel: ThinkingLevel;
  tools: AgentTool[];
  pendingToolCalls: ReadonlySet<string>;
  isStreaming: boolean;
}

export interface Agent {
  readonly state: AgentState;
  subscribe(cb: (event: AgentEvent) => void): () => void;
  sendMessage(text: string, attachments?: Attachment[]): Promise<void>;
  abort(): void;
  setModel(model: Model): void;
  setThinkingLevel(level: ThinkingLevel): void;
  getApiKey?(provider: string): Promise<string | undefined>;
  steer?(message: AgentMessage): void;
}
```

This is the exact interface every Lit component already expects
(`AgentInterface.setupSessionSubscription`, `ChatPanel.setAgent`,
`ArtifactsPanel.agent`). The only implementation in the web app is `RemoteAgent`.

### 2.4 App bootstrap (`main.ts`)

1. Construct the four stores and an `IndexedDBStorageBackend` (db name `hand-ai`,
   versioned schema). Wire backends and call `setAppStorage(...)`.
2. Open a `WsConnection` to `/ws` (same origin as the served page).
3. Construct a `RemoteAgent` over that connection.
4. Construct `<hand-chat-panel>` and call `setAgent(remoteAgent, { onApiKeyRequired,
   onBeforeSend, onCostClick, toolsFactory })`.
5. Render the app header (sessions button → `SessionListDialog`, new-session,
   inline-editable title, theme toggle, settings → `SettingsDialog` with
   `ProvidersModelsTab` + `ProxyTab`).
6. Subscribe to `RemoteAgent` state updates for auto-save into IndexedDB.

---

## 3. The RemoteAgent Client

`RemoteAgent` is the linchpin: it implements the `Agent` interface from §2.3 so that
every existing Lit component works unmodified, while internally it is a thin proxy
over the WebSocket. It owns the local `AgentState` and re-emits the UI-facing
`AgentEvent` stream from server messages.

### 3.1 Responsibilities

- **State mirror.** Holds `messages`, `model`, `thinkingLevel`, `tools`,
  `pendingToolCalls`, `isStreaming`. Hydrated from the initial `session_state`
  frame on connect and after each `agent_end`.
- **Event re-emission.** Translates inbound server frames into the seven
  UI-facing `AgentEvent` variants and fans them out to subscribers. The subscriber
  set is exactly the callbacks registered by `AgentInterface`, `ChatPanel`, etc.
- **Outbound commands.** `sendMessage`, `abort`, `setModel`, `setThinkingLevel`
  become WebSocket sends.
- **Streaming bridge.** On `message_update` it invokes the same
  `StreamingMessageContainer.setMessage(message, immediate)` path the reference UI
  uses; on `message_end` it appends the finalized message to the stable list and
  clears the streaming container; on `agent_end` it clears `isStreaming`.
- **Local tool execution.** Browser-only tools (`javascript_repl`,
  `extract_document`, and the `artifacts` tool) run in the browser. When the server
  emits a tool-call for one of these, `RemoteAgent` routes it to the local
  `AgentTool.execute`, then sends a `tool_result` frame back. Server-side tools
  (bash, read/write/edit, grep, etc.) execute on the server and arrive only as
  `tool_execution_*` events for rendering.

### 3.2 Event mapping (server frame → UI AgentEvent)

| Server frame (`AgentEvent` kind) | RemoteAgent action | UI effect |
|---|---|---|
| `agent_start` | emit `agent_start`; set `isStreaming=true` | `requestUpdate()` |
| `turn_start` | emit `turn_start` | `requestUpdate()` |
| `message_start` | seed streaming container with empty assistant message; emit `message_start` | pulsing cursor shown |
| `message_update` | rebuild partial `AssistantMessage` from delta, call `StreamingMessageContainer.setMessage(msg, false)`; emit `message_update` | live render (rAF-batched) |
| `tool_execution_start` | add id to `pendingToolCalls`; if browser-only tool, begin local execution | pending tool slot |
| `tool_execution_update` | update partial tool result | streaming tool render |
| `tool_execution_end` | finalize tool result; remove id from `pendingToolCalls` | tool result card |
| `message_end` | append finalized assistant message to `messages`; clear streaming container; emit `message_end` | stable history grows |
| `turn_end` | emit `turn_end` | `requestUpdate()` |
| `agent_end` | set `isStreaming=false`; clear streaming container; reconcile `messages` from `agent_end.messages`; emit `agent_end` | input re-enabled |

### 3.3 Streaming-container correctness invariants

These reference-UI behaviors are load-bearing and `RemoteAgent` must preserve them:

- The streaming container must receive a **deep-cloned** message
  (`structuredClone` or `JSON.parse(JSON.stringify(...))`) so Lit's dirty check
  fires on mutated nested objects. Skipping this causes silent no-render bugs.
- `MessageList` hides pending tool calls (`hidePendingToolCalls = isStreaming`) so
  the streaming container and the stable list never both render the same in-flight
  tool card. `RemoteAgent` keeps `pendingToolCalls` and `isStreaming` in sync from
  the `tool_execution_*` and `agent_end` frames.
- The browser-only `artifacts` tool description is dynamic. The server constructs
  the LLM system prompt; the artifacts tool schema/description must be registered on
  **both** sides — declared server-side (so the model is told the tool exists) and
  implemented client-side (so `ArtifactsPanel.tool.execute` runs locally and replies
  with a `tool_result`).

---

## 4. Rust Server Crate Design (`crates/web-ui`)

### 4.1 Chosen approach: in-process library dependency

The server is a **new binary crate that depends on `hand-coding-agent` as a
library** and constructs `AgentSession` objects in-process. It does **not** spawn
`hand --mode rpc` as a subprocess.

Justification (from the backend-seam analysis):

1. `run_rpc_server` is already generic over `AsyncBufRead + AsyncWrite`. The same
   command-dispatch / event-forwarding logic can be driven by WebSocket halves
   instead of stdio with no protocol redesign.
2. Subprocess bridging would add a process boundary, PID lifecycle management, pipe
   plumbing, and a second JSONL serialization hop for zero benefit on a same-machine
   tool.
3. `AgentSession` is already constructed programmatically by the CLI/TUI modes via
   `AgentSessionConfig`; the web server does the same.
4. The `subscribe(|event| ...)` callback maps directly onto a per-connection
   outbound channel that feeds the WebSocket sink — exactly how `run_rpc_server`
   wires its `mpsc` outbound channel today.
5. One `AgentSession` per WebSocket connection is the natural unit. Because
   `AgentSession::send_message` takes `&mut self` and the session is not `Send`-free
   to share, each connection gets its own dedicated task that **owns** its session.

The server reuses the existing dispatch logic in
`hand_coding_agent::rpc::server` and the wire types in
`hand_coding_agent::rpc::types` (`RpcCommand`, `RpcResponse`, `EventEnvelope`,
`RpcExtensionUiRequest`, `RpcExtensionUiResponse`). The only new code is the axum
HTTP/WS plumbing and the text-frame ↔ struct (de)serialization that replaces the
JSONL line framing.

### 4.2 Crate dependencies

```toml
# crates/web-ui/Cargo.toml
[package]
name = "hand-web-ui"
version = "0.1.0"
edition = "2024"
license = "MIT"

[[bin]]
name = "hand-web-ui"
path = "src/main.rs"

[dependencies]
hand-coding-agent = { path = "../coding-agent" }   # AgentSession, rpc::*
hand-agent        = { path = "../agent" }           # AgentEvent (re-exported via rpc)
model             = { path = "../model" }           # Model catalog, registry

axum       = { version = "0.7", features = ["ws"] }
tokio      = { version = "1", features = ["full"] }
tower-http = { version = "0.6", features = ["trace", "compression-br"] }
tower      = "0.5"
serde      = { version = "1", features = ["derive"] }
serde_json = "1"
futures    = "0.3"
rust-embed = "8"                                    # bundle built frontend assets
mime_guess = "2"
thiserror  = "2"
tracing    = "0.1"
clap       = { version = "4", features = ["derive"] }  # --port, --open, --dev
```

### 4.3 Module layout

```
crates/web-ui/
├── Cargo.toml
├── build.rs                # optional: assert web/dist exists for embed in release
├── web/                    # frontend project (see §2.1)
└── src/
    ├── main.rs             # CLI args, bind addr, axum serve, optional --open
    ├── app.rs              # Router assembly: static + /ws + /upload + /download
    ├── ws.rs               # WebSocket upgrade handler; per-conn AgentSession task
    ├── bridge.rs           # WS text-frame ↔ RpcCommand/RpcResponse/EventEnvelope
    ├── session_factory.rs  # build AgentSessionConfig from request / settings
    ├── assets.rs           # rust-embed Assets + content-type serving
    ├── upload.rs           # POST /upload — attachment bytes → content reference
    ├── download.rs         # GET /download/:id — serve ExportHtml output bytes
    └── error.rs            # server error type → HTTP/WS close mapping
```

### 4.4 How existing code is reused unchanged

- **Dispatch loop.** `ws.rs` adapts the WebSocket `Stream`/`Sink` into the shape
  `run_rpc_server` expects, or — preferably — calls the same per-command handlers
  directly: read a text frame, `serde_json::from_str::<RpcCommand>`, run the existing
  handler, serialize the resulting `RpcResponse`, and forward `EventEnvelope`s from
  the `subscribe` callback. The `Prompt`/`Bash` interruptible `select!` races
  (`Steer`/`FollowUp`/`Abort`/`AbortBash`) are preserved by driving the same code
  path.
- **Events.** `session.subscribe(move |event| tx.send(EventEnvelope::from(event)))`
  is identical to the existing stdio path; only the sink changes from a JSONL writer
  to a WebSocket sink.
- **Model registry / sessions / compaction / extensions / slash commands.** All
  reached through `AgentSession` methods (`model_registry()`, `compact()`,
  `collected_slash_commands()`, fork/clone, etc.). No reimplementation.
- **API keys.** Resolved server-side at model-registry build time from environment
  (or extended to accept keys on the WS handshake). Keys are never sent to the
  browser.

Known carry-forward constraints (from the seam analysis): `AgentSession` is not
freely shareable across tasks, so the per-connection task owns it; `get_state` /
`get_messages` are deferred until an in-flight prompt completes (document this
latency); `Compact.custom_instructions` is currently dropped server-side and must
not be advertised as working; `ExportHtml` writes to the server filesystem, so the
browser download flow needs the `GET /download/:id` follow-up endpoint.

---

## 5. WebSocket Wire Protocol

### 5.1 Envelope

Every frame is a UTF-8 JSON text message. There are three top-level frame shapes,
distinguished by their `type` field. They are byte-compatible with the existing
JSONL protocol — the only change is that frames travel as WebSocket text messages
instead of newline-delimited bytes.

```jsonc
// client → server: an RPC command
{ "type": "<snake_case_command>", "id": "opt-correlation-id", /* ...payload */ }

// server → client: a response to a command
{ "id": "opt-correlation-id", "type": "response",
  "command": "<snake_case>", "success": true, "data": { /* ... */ }, "error": null }

// server → client: a pushed session event
{ "type": "event", "event": { "kind": "<tag>", /* ... */ } }
```

All field names follow the existing native convention: command/response `type` and
`command` are `snake_case`; event `kind` is `snake_case`; payload fields are
`camelCase`. This matches the serde attributes already on `RpcCommand` /
`RpcResponse` / `AgentEvent`, so the server serializes its existing types directly.

### 5.2 Client → server catalog (maps 1:1 to `RpcCommand`)

| Wire `type` | Payload | Native `RpcCommand` variant | Effect |
|---|---|---|---|
| `prompt` | `{ message, images?, streamingBehavior? }` | `Prompt` | Drive the agent loop; emits the streaming event sequence |
| `steer` | `{ message, images? }` | `Steer` | Inject mid-turn steering message |
| `follow_up` | `{ message, images? }` | `FollowUp` | Queue a follow-up message |
| `abort` | `{}` | `Abort` | Cancel the current turn → `agent_end` (aborted) |
| `abort_bash` | `{}` | `AbortBash` | Cancel in-flight bash |
| `bash` | `{ command }` | `Bash` | Run a subprocess; interruptible |
| `new_session` | `{ parentSession? }` | `NewSession` | Reset / fork the session |
| `switch_session` | `{ sessionPath }` | `SwitchSession` | Load a persisted session |
| `fork` | `{ entryId }` | `Fork` | Branch the conversation at an entry |
| `clone` | `{}` | `Clone` | Duplicate the session |
| `get_state` | `{}` | `GetState` | Return `RpcSessionState` snapshot |
| `get_messages` | `{}` | `GetMessages` | Return full transcript |
| `get_fork_messages` | `{}` | `GetForkMessages` | Return fork branch points |
| `get_last_assistant_text` | `{}` | `GetLastAssistantText` | Last assistant text block |
| `set_model` | `{ provider, modelId }` | `SetModel` | Update active model |
| `cycle_model` | `{}` | `CycleModel` | Cycle to next model |
| `get_available_models` | `{}` | `GetAvailableModels` | Full model catalog |
| `set_thinking_level` | `{ level }` | `SetThinkingLevel` | Set reasoning level |
| `cycle_thinking_level` | `{}` | `CycleThinkingLevel` | Cycle reasoning level |
| `set_steering_mode` | `{ mode }` | `SetSteeringMode` | All / OneAtATime |
| `set_follow_up_mode` | `{ mode }` | `SetFollowUpMode` | All / OneAtATime |
| `compact` | `{ customInstructions? }` | `Compact` | Run compaction turn |
| `set_auto_compaction` | `{ enabled }` | `SetAutoCompaction` | Toggle auto-compact |
| `set_auto_retry` | `{ enabled }` | `SetAutoRetry` | Toggle auto-retry |
| `abort_retry` | `{}` | `AbortRetry` | Cancel pending retry |
| `get_session_stats` | `{}` | `GetSessionStats` | Token/cost stats |
| `export_html` | `{ outputPath? }` | `ExportHtml` | Export; returns server path (then `GET /download/:id`) |
| `set_session_name` | `{ name }` | `SetSessionName` | Rename session |
| `get_commands` | `{}` | `GetCommands` | Builtin + extension slash commands |
| `extension_ui_response` | `{ id }` + `{ value } \| { confirmed } \| { cancelled }` | `RpcExtensionUiResponse` | Reply to an extension UI request |
| `tool_result` | `{ toolCallId, toolName, content, isError, details? }` | (new) browser-only tool reply | Return local tool output to the agent loop |

`tool_result` is the one client→server frame with no pre-existing `RpcCommand`
variant: it carries the output of browser-executed tools (`javascript_repl`,
`extract_document`, `artifacts`) back into the server's agent loop. It is added as
a new `RpcCommand` variant on the server.

### 5.3 Server → client catalog

**Responses** — direct serialization of `RpcResponse`, one per command, e.g.
`get_state` → `RpcSessionState`; `get_available_models` → `{ models: [...] }`;
`bash` → `{ stdout, stderr, exitCode?, truncated }`; `export_html` → `{ path }`;
`get_messages` → `{ messages: [...] }`; `get_commands` → `{ commands: [...] }`.

**Events** — `{ "type": "event", "event": { "kind": ..., ... } }`. The `agent` kind
wraps the native `AgentEvent` union; these are the frames `RemoteAgent` maps to
UI-facing `AgentEvent`s (§3.2).

| Event frame | Native source | RemoteAgent / UI mapping |
|---|---|---|
| `{ kind: "agent", type: "agent_start" }` | `AgentEvent::AgentStart` | UI `agent_start`; `isStreaming=true` |
| `{ kind: "agent", type: "turn_start" }` | `AgentEvent::TurnStart` | UI `turn_start` |
| `{ kind: "agent", type: "message_start", message }` | `AgentEvent::MessageStart` | seed streaming container |
| `{ kind: "agent", type: "message_update", message, assistantMessageEvent }` | `AgentEvent::MessageUpdate` | `StreamingMessageContainer.setMessage` |
| `{ kind: "agent", type: "message_end", message }` | `AgentEvent::MessageEnd` | append to stable list; clear container |
| `{ kind: "agent", type: "turn_end", message, toolResults }` | `AgentEvent::TurnEnd` | UI `turn_end` |
| `{ kind: "agent", type: "agent_end", messages }` | `AgentEvent::AgentEnd` | reconcile state; `isStreaming=false` |
| `{ kind: "agent", type: "tool_execution_start", toolCallId, toolName, args }` | `AgentEvent::ToolExecutionStart` | pending tool slot; maybe local exec |
| `{ kind: "agent", type: "tool_execution_update", toolCallId, toolName, args, partialResult }` | `AgentEvent::ToolExecutionUpdate` | streaming tool render |
| `{ kind: "agent", type: "tool_execution_end", toolCallId, toolName, result, isError }` | `AgentEvent::ToolExecutionEnd` | finalize tool result card |
| `{ kind: "compaction_start" }` / `{ kind: "compaction_end", summary }` | `AgentSessionEvent::CompactionStart/End` | compaction status |
| `{ kind: "error", message }` | `AgentSessionEvent::Error` | error toast / message |
| `{ kind: "session_info_changed", name }` | `AgentSessionEvent::SessionInfoChanged` | update title |

**Extension UI requests** — `{ "type": "extension_ui_request", "id", "method", ... }`
(`select` / `confirm` / `input` / `editor` / `notify` / `setStatus` / `setWidget` /
`setTitle` / `set_editor_text`). Direct serialization of `RpcExtensionUiRequest`;
the browser renders the appropriate dialog and replies with the
`extension_ui_response` client frame.

### 5.4 Out-of-band: attachments and exports (HTTP)

Embedding large base64 images inside `prompt` frames bloats the WebSocket. The
protocol therefore adds two HTTP endpoints alongside `/ws`:

- `POST /upload` — multipart body of attachment bytes. The server stores them and
  returns a content reference (e.g. a sha256 key). The browser embeds that reference
  in the subsequent `prompt` frame's `images`/content blocks. (For small images,
  inline base64 in the `prompt` frame remains supported.)
- `GET /download/:id` — streams back the bytes of a server-side artifact such as the
  `ExportHtml` output file, so the browser can trigger a download.

---

## 6. Build and Tooling

### 6.1 Frontend (`crates/web-ui/web/`)

- **Vite** for dev server and production bundling. `vite.config.ts` uses the
  Tailwind v4 Vite plugin and exposes the `pdfjs-dist` worker as a static asset via
  `import.meta.url` so `GlobalWorkerOptions.workerSrc` resolves at runtime.
- **Tailwind v4** for styling. `app.css` is the Tailwind entry; it also defines the
  design tokens (CSS custom properties for background/foreground/border/muted/
  secondary), the `@keyframes shimmer` + `.animate-shimmer` used by the thinking
  block, the thin-scrollbar rules, and the user-message gradient. These are vendored
  locally — no external theme package is imported.
- **`tsc`** for type checking (`tsc --noEmit` in CI). Vite handles transpilation;
  `tsc` is the type gate.
- Browser-only libraries stay frontend dependencies: `lit`, `lucide`, `pdfjs-dist`,
  `xlsx`, `docx-preview`, `jszip`, and a TypeBox-equivalent for tool param schemas.

```jsonc
// package.json scripts
{
  "scripts": {
    "dev": "vite",
    "build": "tsc --noEmit && vite build --outDir dist",
    "typecheck": "tsc --noEmit",
    "preview": "vite preview"
  }
}
```

### 6.2 Server (`crates/web-ui/`)

- **cargo** builds the binary as a normal workspace member.
- **Single-binary asset serving.** `rust-embed` embeds `web/dist/**` into the
  release binary. `assets.rs` serves embedded files with correct content types; the
  WebSocket lives at `/ws`. In a release build the binary is fully self-contained —
  no external file dependencies.
- **Build ordering.** Frontend `vite build` must run before `cargo build --release`
  so `web/dist` exists for embedding. A small wrapper (`make web-ui` or a workspace
  task in `scripts/`) runs `npm --prefix crates/web-ui/web run build` then
  `cargo build -p hand-web-ui --release`. An optional `build.rs` asserts `web/dist`
  exists for release builds and prints a clear error otherwise.

### 6.3 Dev workflow

- **Two-terminal dev loop.** Run `cargo run -p hand-web-ui -- --dev` (server binds a
  port, serves `/ws`, and — in `--dev` — skips embedded assets) and `npm --prefix
  crates/web-ui/web run dev` (Vite dev server with HMR). Vite proxies `/ws`,
  `/upload`, and `/download` to the Rust server. This gives instant frontend HMR
  while the real Rust agent backend runs live.
- **Single-binary smoke test.** `npm run build` then `cargo run -p hand-web-ui`
  (no `--dev`) serves the embedded bundle exactly as a release build would.

---

## 7. Browser-Only Feature Decisions

| Feature | Location | Decision and rationale |
|---|---|---|
| **HTML artifact sandbox iframe** | Browser (`src/sandbox/`, `src/artifacts/html-artifact.ts`) | The sandboxed `<iframe>` with `allow-scripts allow-modals`, the `RUNTIME_MESSAGE_ROUTER` postMessage bridge, and the injected runtime providers are inherently browser-only. The server is not involved in HTML preview or console capture. Preserve the imperative iframe insertion (light DOM) and the `getRuntime().toString()` injection constraint (no closures/imports inside injected functions). |
| **JavaScript REPL** | Browser (`src/tools/javascript-repl.ts` + sandbox) | Executes user JS inside the hidden sandbox iframe with a hard 120s timeout. The server requests the tool call over WS and receives the result via a `tool_result` frame; it never executes the code. Preserve chunked base64 (`0x8000`) for returned files. |
| **PDF / DOCX / XLSX / PPTX preview & extraction** | Browser (`src/attachments/`, artifact viewers, `extract_document` tool) | `pdfjs-dist`, `docx-preview`, `xlsx`, `jszip` have no Rust equivalent and stay client-side. `extract_document` fetches the URL from the browser (with a CORS-proxy fallback that surfaces a neutral hand-ai settings message) and parses locally, then returns text via `tool_result`. The `pdfjs-dist` worker is configured as a Vite static asset. |
| **IndexedDB storage** | Browser (`src/storage/`) | Sessions, provider keys, settings, and custom providers persist in IndexedDB (db name `hand-ai`). IndexedDB is a browser API; it stays client-side. The server owns the live agent loop but the browser owns the durable session catalog and reads/writes message history locally. (If session persistence is later moved server-side, the `switch_session` / `session_list` WS path already exists.) Document that provider keys are stored unencrypted at rest in the browser; in this architecture the server resolves keys from its own environment for actual LLM calls, so the browser need not transmit them. |
| **CORS proxy** | Eliminated for LLM calls; browser-only for `extract_document` | The reference UI's client-side CORS proxy (`createStreamFn`, `applyProxyIfNeeded`, `shouldUseProxyForProvider`) is **not ported** — the server makes LLM calls directly and has no browser CORS constraint. The only remaining proxy concern is the optional `extract_document` document fetch, configured by a user setting (`ProxyTab`) and applied client-side. |
| **Attachment upload** | Hybrid | Small images may be inlined as base64 in the `prompt` frame; large files use `POST /upload` and a returned content reference to keep WebSocket frames small. |
| **Provider auto-discovery (Ollama / llama.cpp / vLLM / LM Studio)** | Browser (`src/providers/discovery.ts`) | These are direct browser-to-localhost calls and stay client-side. API-key validation, which the reference UI performed via a live in-browser completion, is replaced by a server round-trip (`set_model` + `get_available_models`, or a dedicated validation command) so keys are not exercised from the browser. |

---

## Appendix: Naming and De-branding

- All emitted custom-element tags use the `hand-` prefix where the reference UI used
  its own brand prefix (e.g. the chat panel tag is `hand-chat-panel`; the model
  picker is `hand-model-selector`). Tags that were already brand-neutral
  (`agent-interface`, `message-list`, `artifacts-panel`, `providers-models-tab`,
  `custom-provider-card`, `provider-key-input`, `sandbox-iframe`, `console-block`)
  are kept as-is.
- The frontend imports no external agent/model/helper packages by name. The shared
  Lit helpers, the agent-core types, the AI/model types, the i18n system, and the
  tool-description prompt constants are all reimplemented locally under
  `src/ui/`, `src/core/`, `src/utils/`, and `src/prompts/`.
- The Rust server reuses `hand-coding-agent`, `hand-agent`, and `model` directly; no
  new copies of the agent/model/RPC logic are created.
