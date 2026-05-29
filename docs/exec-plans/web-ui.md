# ExecPlan: Implement `crates/web-ui` to 100% Capability Parity with the Reference Web UI

**Status:** In progress (M0-M7 complete)
**Author:** Gump (planned with Claude)
**Date:** 2026-05-29

This is a living document. The Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective sections must be kept up to date as work proceeds.

## Purpose

After this work, `crates/web-ui` ships a complete, de-branded TypeScript + Lit browser frontend that reproduces 100% of the reference web UI's capabilities, driven by a new local Rust server (axum + WebSocket) that wraps the workspace's existing native agent / model / RPC layer. No native Rust is rewritten: the server depends on `hand-coding-agent` as a library, constructs `AgentSession` objects in-process, and reuses the existing RPC dispatch and event-subscribe logic. The browser keeps every browser-only capability (HTML sandbox, PDF/DOCX/XLSX/PPTX rendering, JS REPL, IndexedDB, attachment parsing); the in-browser "Agent" runtime of the reference UI is replaced by a thin `RemoteAgent` client that proxies over WebSocket and re-emits the same UI-facing event stream.

Concretely, by the end of this plan a user can:

1. Run a single binary (`cargo run -p hand-web-ui`), open the printed `http://127.0.0.1:<port>` URL, and chat with the agent: type a message, see streaming assistant tokens, thinking blocks, tool-call cards (bash, calculate, artifacts, javascript_repl, extract_document), per-turn cost stats, and abort mid-stream.
2. Attach an image / PDF / DOCX / XLSX / PPTX, see it parsed in-browser, previewed in an overlay, and have its extracted text reach the agent.
3. Create an HTML artifact and see it run live in a sandboxed iframe with captured console output, switch to code view, reload, copy, and download a standalone HTML file; view SVG / Markdown / image / PDF / Excel / DOCX / generic artifacts in the panel.
4. Open the model picker (fuzzy search, Thinking/Vision filters, keyboard navigation), switch model and thinking level, manage cloud-provider API keys and custom providers (Ollama / llama.cpp / vLLM / LM Studio auto-discovery), configure the document-fetch proxy, browse and load persisted sessions, and toggle theme — all without any reference-brand string appearing anywhere in the shipped frontend or server.
5. Build the production single binary with `npm run build` (Vite) followed by `cargo build -p hand-web-ui --release`, where `rust-embed` bundles the frontend bundle into the binary.

The plan is sliced into ordered, thin vertical milestones M0..M12. Each milestone is independently shippable, has its own acceptance criteria (`cargo check` / `cargo clippy` / `cargo test`, `tsc --noEmit`, plus an observable behavior), and advances the Capability Parity Matrix in §Capability Parity Matrix toward fully-checked.

The target architecture is fixed and documented in `docs/web-ui-architecture.md`; this plan does not re-litigate it. Read that document first — it defines the runtime topology, the `src/core/` local type system, the `RemoteAgent` contract, the Rust server crate design, the WebSocket wire protocol, the build tooling, and the per-feature browser-only decisions.

## Progress

- [x] **M0 — Scaffold and hello-world end-to-end**
  - [x] M0.T1 Add `crates/web-ui` to workspace members; create `hand-web-ui` binary crate that builds
  - [x] M0.T2 Create the Vite + Tailwind v4 + TypeScript frontend project that builds and type-checks
  - [x] M0.T3 Define shared WS wire types (`src/client/wire.ts`) and `src/core/` skeleton
  - [x] M0.T4 Minimal axum router: serve assets + `/ws`; one prompt streams one assistant reply end-to-end
- [x] **M1 — Chat shell**
- [x] **M2 — Message + tool rendering**
- [x] **M3 — Sandbox runtime**
- [x] **M4 — Artifacts** (frontend panel + viewers AND the browser-tool execution
  mechanism — server declares browser tools, suspends on a per-connection hub
  keyed by `tool_call_id`, `ws.rs` intercepts `tool_result` frames to resolve
  them; `run_rpc_server`/agent crates unchanged)
- [x] **M5 — Browser tools (JS REPL, extract-document)**
- [x] **M6 — Attachments** (loadAttachment ingestion core + `<attachment-tile>` +
  `<attachment-overlay>` + editor drag/drop/paste/picker). NB: delivery of
  attachments TO the agent (image content in the `prompt` frame + a server change
  to honor it) is M10's attachment dispatch — the editor collects + previews them
  now; `RemoteAgent.sendMessage` still drops the attachments arg until M10.)
- [x] **M7 — Storage (IndexedDB)**
- [ ] **M8 — Providers / models**
- [ ] **M9 — Dialogs / settings**
- [ ] **M10 — Proxy / networking / out-of-band upload-download**
- [ ] **M11 — i18n / format / theming / design system**
- [ ] **M12 — Polish, single-binary packaging, parity verification**

## Surprises & Discoveries

- **2026-05-29 (M0)**: The native agent event stream does not match the assumed
  "message_start/update/end all carry the assistant message" model. Verified
  against a live turn: `message_start` / `message_end` announce *any* message
  added to history — including the user's own message, whose `content` is a
  plain string — while the streaming assistant content arrives via
  `message_update` (content is a block array of thinking/text/toolCall), the
  finalized assistant message for the turn is carried by `turn_end.message`,
  and the full reconciled list is in `agent_end.messages`. Consequences applied
  in M0 and to carry into M1: (1) any text extraction must handle
  `content: string | ContentBlock[]`; (2) `RemoteAgent` drives the streaming
  reply from `message_update` (role === "assistant") and finalizes from
  `turn_end`, and folds user/assistant history additions from
  `message_start`/`message_end` (role-checked) in the chat-shell milestone;
  (3) the wire `message` field is a loose `WireMessage`, not strictly an
  `AssistantMessage`.
- **2026-05-29 (M4)**: The native artifacts tool's `execute` signature is
  `(toolCallId, args, signal)`, not `(args)`; the chat shell wraps it into the
  `AgentTool.execute(args)` the agent expects. The server browser-tool routing
  must call it with the toolCallId so the result can be correlated. Also: the
  HTML-artifact create path and the smoke helper relied on `requestAnimationFrame`
  to settle, which is throttled/never-fires in a backgrounded or headless tab —
  the smoke now uses `updateComplete` + a macrotask instead. Real (foregrounded)
  usage is unaffected; this only matters for headless verification.
- **2026-05-29 (M3)**: Custom-element registration is only triggered when the
  defining module is actually evaluated. Under `isolatedModules`/esbuild, a
  consumer that imports a sandbox element class **only in a type position**
  (e.g. `createElement(...) as SandboxIframe`) has its import elided, so
  `<sandbox-iframe>` never registers and `el.execute` is missing. Rule for the
  sandbox (and any custom element): consumers must import a runtime value from
  the module or add a side-effect import. M4/M5 import sandbox provider values,
  so they register it; pure-type consumers (like the smoke helper) need an
  explicit `import "./sandboxed-iframe"`.
- **2026-05-29 (M2)**: The server serializes assistant **tool-call content
  blocks with the discriminator `"toolcall"`** (all lowercase), not the
  canonical `"toolCall"` the renderers expect. A browser test (agent invoking
  the bash tool) caught the resulting missing tool-call card. Fixed by
  normalizing wire content blocks (`toolcall` -> `toolCall`) at the RemoteAgent
  boundary so core types and renderers stay canonical; applied on
  `message_update`, `turn_end`, and the `agent_end` reconciled list. The
  `toolResult` message shape (`role`, `toolCallId`, `toolName`, `content`,
  `isError`) already matched. Watch for further lowercase-variant discriminators
  on other content block kinds (image/thinking matched as-is).
- **2026-05-29 (M0)**: The reference architecture doc named a public
  `rpc::EventEnvelope`; in the workspace the event envelope and `WireSessionEvent`
  are private to `rpc::server`. This is a non-issue for the chosen design: the
  server reuses `run_rpc_server` wholesale by bridging the WebSocket onto its
  `AsyncBufRead`/`AsyncWrite` parameters (one text frame == one JSONL line), so
  no envelope type needs to be re-exported.

## Decision Log

- **2026-05-29**: The Rust server depends on `hand-coding-agent` as a library and constructs `AgentSession` in-process (not a `hand --mode rpc` subprocess). Rationale from the backend-seam analysis: `run_rpc_server` is already generic over `AsyncBufRead + AsyncWrite`, the `subscribe(|event| ...)` callback maps directly onto a per-connection outbound channel, and subprocess bridging adds a process boundary and a second serialization hop for zero benefit on a same-machine tool.
- **2026-05-29**: One `AgentSession` per WebSocket connection, owned by a dedicated per-connection task. `AgentSession::send_message` takes `&mut self` and the session is not freely shareable across tasks, so concurrent browser tabs each get their own task that owns its session.
- **2026-05-29**: The WS wire protocol is byte-compatible with the existing JSONL RPC protocol; frames travel as WebSocket text messages instead of newline-delimited bytes. Command/response `type`/`command` and event `kind` are `snake_case`; payload fields are `camelCase` — matching the serde attributes already on `RpcCommand` / `RpcResponse` / `AgentEvent`.
- **2026-05-29**: The reference client-side LLM-call layer (`createStreamFn`, `applyProxyIfNeeded`, `shouldUseProxyForProvider`) is **not ported**. LLM streaming happens on the server, which has no browser CORS constraint. `isCorsError` survives only for the `extract_document` document fetch fallback.
- **2026-05-29**: A new `tool_result` `RpcCommand` variant is added server-side to carry output of browser-executed tools (`javascript_repl`, `extract_document`, `artifacts`) back into the agent loop. This is the only client→server frame with no pre-existing native variant.
- **2026-05-29**: Provider API keys are resolved server-side from the server process environment for real LLM calls; the browser does not transmit them. The reference UI's in-browser API-key validation (a live completion call) is replaced by a server round-trip. Provider keys may still be persisted in browser IndexedDB for UI status display, documented as unencrypted at rest.
- **2026-05-29**: Custom-element tags use the `hand-` prefix only where the reference UI used its own brand prefix (`hand-chat-panel`, `hand-model-selector`). Already brand-neutral tags (`agent-interface`, `message-list`, `artifacts-panel`, `providers-models-tab`, `custom-provider-card`, `provider-key-input`, `sandbox-iframe`, `console-block`) are kept as-is.
- **2026-05-29**: The shared Lit helper library, the agent/model TypeScript types, the i18n system, and the tool-description prompt constants are reimplemented locally under `src/ui/`, `src/core/`, `src/utils/`, `src/prompts/`. The frontend imports no external agent/model/helper package by name.

## Outcomes & Retrospective

- **M7 (2026-05-29)**: IndexedDB persistence layer — `StorageBackend` +
  `IndexedDBStorageBackend` (lazy open, schema config, prefix scan, cursor index,
  quota/persist with graceful fallback), `Store` base, `AppStorage` singleton,
  and the four stores (Settings, ProviderKeys, Sessions dual-store with atomic
  save/delete and single-transaction `updateTitle`, CustomProviders). Bootstrap
  wires them (db `hand-ai`) + a resilient auto-save subscription on
  `agent_end`/`message_end`. Verified in a browser: a save → get + getAllMetadata
  → updateTitle → delete round-trip returns
  `{saved, restoredOk, metadataCount:1, titleUpdated, deletedOk}` all true,
  confirming the atomic dual-store writes. (Two transient API 500s delayed this
  milestone by one loop iteration; no code impact.)
- **M6 (2026-05-29)**: Attachment UI complete — `<attachment-tile>` (thumbnail/
  badge/delete), `<attachment-overlay>` (PDF all-pages / DOCX / Excel / PPTX /
  image / text + extracted-text toggle + download), and editor ingestion
  (paperclip picker + drag/drop + clipboard paste, max 10 / 20MB / type
  allowlist). Verified in a browser: `loadAttachment` of a text File renders an
  `<attachment-tile>`. Polish carried forward (M11/M12): the editor's validation
  errors use `alert()` (ported from the reference) — replace with an inline,
  non-blocking error. Delivery of attachments to the agent is M10.
- **M5 (2026-05-29)**: `javascript_repl` and `extract_document` browser tools
  landed on the M4 hub (server declares them; client executors run in the M3
  sandbox / via fetch+`loadAttachment`). Also implemented M6's `loadAttachment`
  ingestion core (PDF/DOCX/PPTX/XLSX/image/text processors, chunked base64).
  Verified in a browser: prompting the agent to use `javascript_repl` produced
  roles `[user, assistant, toolResult, assistant]`, ran the code in the sandbox
  (computed 70), and completed the turn — confirming the browser-tool hub
  generalizes to a second tool. `extract_document` is gate-verified and uses the
  identical hub path (only the executor body differs: fetch + 50MB cap + neutral
  CORS fallback + `loadAttachment`). Remaining M6: attachment tile/overlay
  elements + editor drag/drop/paste wiring.
- **Browser-tool execution (2026-05-29)**: The cross-cutting mechanism that lets
  the server-side agent loop invoke browser-resident tools is implemented and
  verified end-to-end. Design (clean, id-correlated): a per-connection
  `BrowserToolHub` (`tool_call_id` -> `oneshot::Sender<ToolResult>`); browser
  tools are real `AgentTool`s whose `execute` closure registers on the hub and
  awaits; `ws.rs` intercepts inbound `tool_result` frames and resolves the hub;
  the inbound interceptor and the dispatcher run as separate tasks so the
  suspended tool resolves with no deadlock. `run_rpc_server` and the agent/model
  crates are UNCHANGED (no `RpcCommand` variant added). Verified in a browser: a
  prompt asking the agent to use the `artifacts` tool produced
  roles `[user, assistant, toolResult, assistant]`, created `hello.md` in the
  panel, rendered the create card, and the turn completed with "Done!". The same
  hub serves M5's `javascript_repl` / `extract_document`. Known limitation: a
  browser-reported `isError:true` rides in `details` (the agent loop derives
  `is_error` from the closure's Ok/Err and browser tools return Ok); the error
  text is still in the content the model sees. Switch the browser tool to
  `AgentTool::new` returning `Err` if true `is_error` propagation is needed.
- **M4 frontend (2026-05-29)**: Artifacts panel + all viewers landed and verified
  via direct browser API calls: creating a markdown and an HTML artifact stores
  both (`artifacts` Map size 2, keyed by filename), the HTML create completes
  (the <=1500ms console-capture wait is correctly bounded), and opening the HTML
  artifact mounts an `<html-artifact>` element with a sandbox `<iframe>` in the
  DOM. Tab bar + Preview/Code toggle render. **Deferred to the browser-tool
  execution milestone (next):** the server-side `tool_result` `RpcCommand` and
  the server `artifacts` tool declaration, so the agent can actually drive the
  browser-resident artifacts tool. The client `artifacts` AgentTool is fully
  implemented and ready to be wired once the server routes browser tools.
- **M0 (2026-05-29)**: Verified end-to-end via a deterministic WebSocket probe
  (`get_state`, no LLM) and a live streaming prompt. The WS<->`run_rpc_server`
  bridge reuses the dispatcher unchanged.
- **M1 (2026-05-29)**: Verified in a real browser against the single-binary
  served bundle: a live prompt renders the user bubble, a collapsible thinking
  block, and the streamed assistant reply in the stable list; the streaming
  container hands off cleanly (cleared + hidden on completion); the cost stats
  bar renders aggregate usage (`formatUsage`). No app-level console errors (the
  only console error originates from the browser extension's own polyfill, not
  the app). lucide resolved to a 1.x package in this environment's registry
  rather than the public 0.x; the `icon()` helper's `IconNode` shape matched and
  build/typecheck/runtime are green.

## Context and Orientation

Related documents:
- Target architecture (read first): `docs/web-ui-architecture.md` — runtime topology, module tree, local type system, `RemoteAgent`, Rust server design, wire protocol, build/tooling, browser-only decisions.
- ExecPlan style/format reference: `docs/exec-plans/agent-port-parity.md`.

Workspace facts grounding this plan (verified):
- Existing crates: `crates/model` (model catalog + registry), `crates/agent` (`hand-agent`, owns `AgentEvent`), `crates/coding-agent` (`hand-coding-agent`, owns `AgentSession` + `rpc::{run_rpc_server, RpcCommand, RpcResponse, EventEnvelope, RpcExtensionUiRequest, RpcExtensionUiResponse}`), `crates/tui`.
- `crates/web-ui` currently contains only a `README.md` (no code) and is **not** a workspace member. Its README names the package `hand-web-ui`; this plan uses crate package name `hand-web-ui` and binary `hand-web-ui`.
- `AgentEvent` already serializes `snake_case` `type` + `camelCase` fields; the RPC envelope types are reused unchanged over the WebSocket.

How the pieces fit together (one paragraph):

The browser runs all Lit views and browser-only capabilities. `RemoteAgent` (in `web/src/client/remote-agent.ts`) implements the local `Agent` interface from `web/src/core/agent.ts`, so every existing Lit component (`AgentInterface`, `ChatPanel`, `ArtifactsPanel`) works unmodified; internally it is a thin proxy over a `WsConnection`. The Rust server (`crates/web-ui/src/`) upgrades `/ws`, owns one `AgentSession` per connection, and reuses the existing per-command dispatch and `session.subscribe` event forwarding — only the sink changes from a JSONL writer to a WebSocket sink. Server-side tools (bash, read/write/edit, grep) execute on the server and arrive as `tool_execution_*` events for rendering. Browser-only tools (`javascript_repl`, `extract_document`, `artifacts`) are requested by the server over WS, executed locally in the browser, and their output is sent back via a `tool_result` frame. Attachments and exports use out-of-band HTTP (`POST /upload`, `GET /download/:id`) to keep WebSocket frames small.

## Plan of Work

The work is sliced vertically. M0 stands up a buildable, type-checked, hello-world-streaming end-to-end skeleton. Subsequent milestones each port one analyzed subsystem as a thin slice with its own acceptance test. Milestones are ordered so each depends only on earlier ones.

### M0 — Scaffold and hello-world end-to-end

Goal: a buildable Rust binary crate and a buildable/type-checked TS frontend, wired so that opening the page, connecting `/ws`, and sending one `prompt` frame streams exactly one assistant reply back into the DOM. This proves the entire seam (workspace membership, asset serving, WS upgrade, in-process `AgentSession`, event forwarding, `RemoteAgent` event mapping) before any subsystem is fleshed out.

Files to create:
- `crates/web-ui/Cargo.toml` — package `hand-web-ui`, `[[bin]]`, deps: `hand-coding-agent`, `hand-agent`, `model`, `axum` (ws), `tokio` (full), `tower-http`, `tower`, `serde`, `serde_json`, `futures`, `rust-embed`, `mime_guess`, `thiserror`, `tracing`, `clap`.
- `crates/web-ui/src/main.rs` — clap args (`--port`, `--open`, `--dev`), bind addr, axum serve.
- `crates/web-ui/src/app.rs` — router assembly (static + `/ws`).
- `crates/web-ui/src/ws.rs` — WS upgrade; per-connection `AgentSession` task; read text frame → `serde_json::from_str::<RpcCommand>` → existing handler → serialize `RpcResponse`; forward `EventEnvelope`s from `subscribe`.
- `crates/web-ui/src/bridge.rs` — WS text-frame ↔ `RpcCommand`/`RpcResponse`/`EventEnvelope` (de)serialization.
- `crates/web-ui/src/session_factory.rs` — build `AgentSessionConfig` from request/settings.
- `crates/web-ui/src/assets.rs` — `rust-embed` Assets + content-type serving; `--dev` skips embedded assets.
- `crates/web-ui/src/error.rs` — server error type → HTTP/WS close mapping.
- `crates/web-ui/web/` — Vite project: `index.html`, `package.json`, `tsconfig.json`, `vite.config.ts` (`@tailwindcss/vite` + pdf worker asset), `src/main.ts`, `src/app.css`.
- `crates/web-ui/web/src/client/wire.ts` — envelope + client→/server→ message type unions (the shared WS wire types).
- `crates/web-ui/web/src/client/ws-connection.ts` — WebSocket lifecycle, reconnect, framing.
- `crates/web-ui/web/src/client/remote-agent.ts` — minimal `RemoteAgent` implementing `Agent` (sendMessage + subscribe + the seven event mappings).
- `crates/web-ui/web/src/core/{model.ts,messages.ts,agent.ts}` — minimal local type system skeleton (full content in M1/M2).
- Edit `/Users/wanggang/dev/00/hand-ai/Cargo.toml` — add `"crates/web-ui"` to `[workspace].members`.

Dependencies: none (first milestone).

Acceptance criteria:
- `cargo check -p hand-web-ui` and `cargo clippy -p hand-web-ui -- -D warnings` pass.
- `npm --prefix crates/web-ui/web install` then `npm --prefix crates/web-ui/web run typecheck` (`tsc --noEmit`) pass.
- `npm --prefix crates/web-ui/web run build` produces `web/dist/`.
- Observable: with the server running (`cargo run -p hand-web-ui --dev`) and Vite dev server up, the page loads, the WS connects, sending a hardcoded prompt streams an assistant reply that appears token-by-token in a bare `<div>`. (Uses the preferred smoke-test model `deepseek/deepseek-v4-flash` via configured provider env.)

### M1 — Chat shell

Goal: the full conversational shell renders and orchestrates a real conversation: layout split (chat left, artifacts right) above the 800px breakpoint with mobile overlay + floating artifacts pill; auto-scroll with the clientHeight-shrink guard; bottom-anchored editor; per-turn cost stats bar; abort button; streaming-container live render with deep-clone dirty-check; stable `MessageList` with keyed `repeat()` and pending-tool-call hiding.

Files to create (under `web/src/shell/`): `chat-panel.ts` (`<hand-chat-panel>`), `agent-interface.ts` (`<agent-interface>`), `message-list.ts` (`<message-list>`), `streaming-message-container.ts` (`<streaming-message-container>`), `message-editor.ts` (`<message-editor>`). Plus `web/src/core/convert.ts` (`defaultConvertToLlm`, `convertAttachments`, `isUserMessageWithAttachments`, `isArtifactMessage`) and finalized `web/src/core/{messages.ts,agent.ts,model.ts}`. Expand `remote-agent.ts` to own full `AgentState` and the streaming-container invariants.

Dependencies: M0.

Acceptance criteria:
- `tsc --noEmit` passes; `cargo check -p hand-web-ui` still passes.
- Observable: a multi-turn conversation renders with auto-scroll that disables on user scroll-up and re-enables near bottom and does NOT false-disable when the stats bar appears (clientHeight-shrink guard); Enter sends, Shift+Enter newlines, Escape aborts while streaming; the streaming container shows a pulsing cursor before first token and never duplicates a tool card with `MessageList`; the artifacts pill appears when artifacts exist but the panel is collapsed.

### M2 — Message + tool rendering

Goal: full message and tool-call rendering parity. Message renderer registry (`registerMessageRenderer` / `getMessageRenderer` / `renderMessage`, `MessageRole`); built-in message elements (`user-message`, `assistant-message`, `tool-message`, `tool-message-debug`, `aborted-message`, `thinking-block`); tool renderer registry (`renderTool`, `register/getToolRenderer`, `renderHeader`, `renderCollapsibleHeader`, `setShowJsonMode`) and the data-only renderers (`BashRenderer`, `CalculateRenderer`, `DefaultRenderer`, `GetCurrentTimeRenderer`). The browser-only `javascript_repl` / `extract_document` / `artifacts` renderers are registered in M4/M5.

Files to create (under `web/src/shell/messages/`): `user-message.ts`, `assistant-message.ts`, `tool-message.ts`, `tool-message-debug.ts`, `aborted-message.ts`, `thinking-block.ts`, `message-renderer-registry.ts`, `index.ts`. Under `web/src/tools/`: `renderer-registry.ts`, `bash.ts`, `calculate.ts`, `default.ts`, `get-current-time.ts`, `index.ts`. Plus `web/src/ui/` essentials used here: `markdown-block.ts`, `code-block.ts`, `expandable-section.ts`, `icons.ts`.

Dependencies: M1.

Acceptance criteria:
- `tsc --noEmit` passes.
- Observable: assistant messages render text + thinking blocks (collapsed by default, shimmer header while streaming) + tool-call cards in content order; bash tool shows `> command` then output console-block with error variant on failure; calculate shows the four progressive text states; the default renderer pretty-prints JSON params/result; `setShowJsonMode(true)` forces all tools through the default renderer; the collapsible-header DOM toggle (max-h transition + chevron swap via refs) works; an aborted turn shows the italic "Request aborted" stub.

### M3 — Sandbox runtime

Goal: the browser sandbox subsystem that underpins HTML artifacts and the JS REPL. `<sandbox-iframe>` with `execute()` (transient hidden iframe, 120s timeout, AbortSignal), `loadContent()` (persistent visible iframe), `prepareHtmlDocument()` (public, for standalone download); `RUNTIME_MESSAGE_ROUTER` singleton dispatcher; `RuntimeMessageBridge` injectable bridge-code generator; the four runtime providers (`ConsoleRuntimeProvider` required-first, `ArtifactsRuntimeProvider`, `AttachmentsRuntimeProvider`, `FileDownloadRuntimeProvider`); navigation interceptor; HTML validation gate.

Files to create (under `web/src/sandbox/`): `sandboxed-iframe.ts`, `runtime-message-router.ts`, `runtime-message-bridge.ts`, `providers/console-provider.ts`, `providers/artifacts-provider.ts`, `providers/attachments-provider.ts`, `providers/file-download-provider.ts`.

Dependencies: M2 (renderer registry types), partial M6 (Attachment type — define the type early in M3 if M6 not yet landed).

Acceptance criteria:
- `tsc --noEmit` passes.
- Observable (unit/manual): `execute()` runs `console.log('x'); return 1+1;` and resolves a `SandboxResult` with the captured console line and `returnValue` 2; `getRuntime().toString()` injection has no closure references; the `iframe` sandbox attribute is exactly `allow-scripts allow-modals`; the 120s timeout constant is preserved; chunked base64 (`0x8000`) round-trips a >1MB returned file without stack overflow.

### M4 — Artifacts

Goal: the full artifacts panel and all viewer element types. `<artifacts-panel>` (Map-backed state, imperative DOM insertion, tab bar, `reconstructFromMessages`, `openArtifact`, the `artifacts` AgentTool with create/update/rewrite/get/delete/logs); `ArtifactElement` base; viewers `html-artifact` (sandbox iframe + console capture + reload/copy/download + preview/code toggle), `svg-artifact`, `markdown-artifact`, `text-artifact`, `image-artifact`, `pdf-artifact`, `docx-artifact`, `excel-artifact`, `generic-artifact`; `<artifact-console>`; `ArtifactPill`; `ArtifactsToolRenderer` (registered in `ChatPanel` constructor with the live panel ref); `getFileType` dispatch.

Files to create (under `web/src/artifacts/`): `artifacts-panel.ts`, `artifact-element.ts`, `html-artifact.ts`, `svg-artifact.ts`, `markdown-artifact.ts`, `text-artifact.ts`, `image-artifact.ts`, `pdf-artifact.ts`, `docx-artifact.ts`, `excel-artifact.ts`, `generic-artifact.ts`, `console.ts`, `artifact-pill.ts`, `artifacts-tool-renderer.ts`, `file-type.ts`. Plus `web/src/ui/{preview-code-toggle.ts,diff.ts,button.ts}` and `public/pdf.worker.min.mjs`. Add the server-side `tool_result` `RpcCommand` variant and a server-registered `artifacts` tool declaration (schema + dynamic description) so the LLM is told the tool exists while execution stays in the browser.

Dependencies: M3 (sandbox), M2 (tool renderer registry), M6 (Attachment for AttachmentsRuntimeProvider — define type early if needed).

Acceptance criteria:
- `tsc --noEmit` passes; `cargo check -p hand-web-ui` passes (new `tool_result` variant compiles).
- Observable: the agent calls the `artifacts` tool to create an `.html` file; it appears as a tab, runs in the sandbox iframe, captures console logs (visible in `<artifact-console>`), toggles to code view and back, reloads, copies, and downloads a standalone HTML; SVG/Markdown/image/PDF/Excel/DOCX/generic viewers each render their type; `reconstructFromMessages` replays artifact history on session load without auto-opening the panel (the `onArtifactsChange` null-during-reconstruct ordering is preserved); the update command renders a Diff.

### M5 — Browser tools (JS REPL, extract-document)

Goal: the two browser-only AgentTools and their renderers, with `tool_result` round-tripping. `createJavaScriptReplTool` / `javascriptReplTool` (dynamic description from `runtimeProvidersFactory().getDescription()`, `executeJavaScript` in the hidden sandbox iframe, base64-encode returned files); `javascriptReplRenderer`; `createExtractDocumentTool` / `extractDocumentTool` (fetch with 50MB limit, CORS-proxy fallback with a neutral hand-ai settings message, delegate to `loadAttachment`); `extractDocumentRenderer`. Auto-registration side-effects via `tools/index.ts`. `RemoteAgent` routes server tool-call events for these tools to local `execute` and replies with `tool_result`.

Files to create (under `web/src/tools/`): `javascript-repl.ts`, `extract-document.ts`. Plus `web/src/prompts/prompts.ts` (verbatim-content `JAVASCRIPT_REPL_TOOL_DESCRIPTION`, `EXTRACT_DOCUMENT_DESCRIPTION`, and the runtime-provider description constants, all brand-neutral) and `web/src/utils/cors.ts` (`isCorsError`). Server-side: declare `javascript_repl` and `extract_document` tool schemas/descriptions so the model knows they exist; route their execution to the browser.

Dependencies: M3 (sandbox), M4 (Attachment/loadAttachment via M6 or shared type), M2 (renderer registry).

Acceptance criteria:
- `tsc --noEmit` passes; `cargo check -p hand-web-ui` passes.
- Observable: the agent calls `javascript_repl`; the code runs in the browser sandbox, the renderer shows collapsible code + console output + attachment-tile chips for returned files, and a `tool_result` frame returns to the server agent loop; the agent calls `extract_document` with a PDF URL; the browser fetches and parses it, returns extracted text, and the renderer shows filename/format/size; a CORS failure surfaces the neutral fallback message (no reference brand).

### M6 — Attachments

Goal: the universal attachment ingestion pipeline and viewer UI. `loadAttachment` (URL/File/Blob/ArrayBuffer → `Attachment`) with per-format processors (PDF text + 160×160 thumbnail, DOCX AST walk, PPTX zip+regex, Excel sheet→CSV, image identity, text TextDecoder), chunked base64 (`0x8000`); `<attachment-tile>`; `<attachment-overlay>` (PDF all-pages canvas, DOCX renderAsync, Excel multi-sheet tables, PPTX extracted-text, image, plain-text, extracted-text toggle, download, error display). `MessageEditor` drag-drop / paste / file-picker wiring (the editor element exists from M1; this milestone connects its attachment row to `loadAttachment`).

Files to create (under `web/src/attachments/`): `attachment-utils.ts` (the `Attachment` type + `loadAttachment` + processors), `attachment-tile.ts`, `attachment-overlay.ts`. Plus `web/src/ui/{mode-toggle.ts}` (or inline) for the overlay toggle. Configure `pdfjs-dist` worker as a Vite static asset in `vite.config.ts`.

Dependencies: M1 (MessageEditor shell). The `Attachment` type may be needed earlier by M3/M4/M5 — if so, land the type definition in this file path during the earliest consumer milestone and fill the processors here.

Acceptance criteria:
- `tsc --noEmit` passes.
- Observable: dropping / pasting / picking an image and a PDF shows tiles in the editor (PDF tile shows the thumbnail + "PDF" badge); clicking a tile opens the overlay; the PDF overlay renders all pages, the DOCX/Excel overlays render their content, the extracted-text toggle shows the raw XML extraction; sending the message delivers the attachment to the agent; large files (>1MB) encode without stack overflow.

### M7 — Storage (IndexedDB)

Goal: the full browser persistence layer. `StorageBackend` / `StorageTransaction` interfaces; `IndexedDBStorageBackend` (lazy open, schema via `IndexedDBConfig`, prefix scan, cursor index traversal, quota + persist APIs); `Store` base; `AppStorage` + `getAppStorage`/`setAppStorage` singleton; the four stores (`SettingsStore`, `ProviderKeysStore`, `SessionsStore` dual-store with atomic transactions, `CustomProvidersStore`); `SessionMetadata` / `SessionData` / `CustomProvider` types. App bootstrap wires the stores and calls `setAppStorage` (db name `hand-ai`). Auto-save subscription: persist message history to IndexedDB on `RemoteAgent` state updates.

Files to create (under `web/src/storage/`): `backend.ts`, `indexeddb-backend.ts`, `store.ts`, `app-storage.ts`, `settings-store.ts`, `provider-keys-store.ts`, `sessions-store.ts`, `custom-providers-store.ts`. Wire into `web/src/main.ts`.

Dependencies: M1 (core types: `AgentMessage`, `Model`, `ThinkingLevel`).

Acceptance criteria:
- `tsc --noEmit` passes.
- Observable: a conversation auto-saves to IndexedDB; reloading the page and selecting the session restores the full transcript and model/thinking-level; deleting a session removes it from both the primary and metadata stores atomically; `getQuotaInfo()` returns sane values (or zeros gracefully on unsupported browsers); `updateTitle` wraps both writes in a single transaction (fixing the reference two-write inconsistency).

### M8 — Providers / models

Goal: model selection and provider management parity. `<hand-model-selector>` (fuzzy subsequence-scored search, Thinking/Vision filters, keyboard nav, IME-safe, current-model checkmark, allowedProviders filter, cost/token formatting); built-in model registry enumeration (served from the Rust server via `get_available_models`, or bundled); `<providers-models-tab>`, `<custom-provider-card>`, `<custom-provider-dialog>`, `<provider-key-input>`; auto-discovery (`discoverOllamaModels`, `discoverLlamaCppModels`, `discoverVLLMModels`, `discoverLMStudioModels`) as direct browser-to-localhost calls; default base-URL prefill; Test Connection; status indicators. API-key validation is a server round-trip (not an in-browser completion).

Files to create (under `web/src/providers/`): `model-selector.ts`, `providers-models-tab.ts`, `custom-provider-card.ts`, `custom-provider-dialog.ts`, `provider-key-input.ts`, `discovery.ts`. Plus `web/src/ui/{select.ts,badge.ts,switch.ts,label.ts}`. Deps `ollama/browser`, `@lmstudio/sdk` (browser-only).

Dependencies: M7 (CustomProvidersStore, ProviderKeysStore), M1 (Model type), M9 (DialogBase — land `dialog-base.ts` here or in M9; sequence so the dialog base exists before first consumer).

Acceptance criteria:
- `tsc --noEmit` passes; `cargo check -p hand-web-ui` passes (server `get_available_models` path exercised).
- Observable: the model picker opens with fuzzy search and keyboard navigation, filters by Thinking/Vision, switches the active model server-side; adding an Ollama custom provider with Test Connection lists discovered models; the provider-key-input shows a checkmark for a stored key without revealing it and validates a new key via the server round-trip.

### M9 — Dialogs / settings

Goal: the modal dialog system and settings tabs. `DialogBase` modal base; `SettingsTab` abstract base; `<settings-dialog>` (sidebar/mobile-strip nav, display:none tab toggling); `<session-list-dialog>` (metadata cards, relative dates, usage formatting, in-UI delete confirmation replacing `window.confirm`); `<api-key-prompt-dialog>` (storage-poll resolve); `<persistent-storage-dialog>` (navigator.storage.persist with graceful fallback); `<api-keys-tab>`; `<proxy-tab>` (document-fetch proxy config, not LLM proxy).

Files to create (under `web/src/dialogs/`): `settings-dialog.ts`, `session-list-dialog.ts`, `api-key-prompt-dialog.ts`, `persistent-storage-dialog.ts`, `api-keys-tab.ts`, `proxy-tab.ts`, `settings-tab.ts`. Plus `web/src/ui/dialog-base.ts` (if not landed in M8). Wire the header buttons in `web/src/main.ts`.

Dependencies: M7 (AppStorage), M8 (ProvidersModelsTab is hosted in the settings dialog).

Acceptance criteria:
- `tsc --noEmit` passes.
- Observable: the settings dialog opens with Providers & Models + Proxy tabs and switches between them; the session-list dialog lists saved sessions with relative dates and usage, loads one on click, and deletes one via an in-UI confirmation; the API-key-prompt dialog resolves once a key is entered; the persistent-storage dialog requests persistence and degrades gracefully where unsupported.

### M10 — Proxy / networking / out-of-band upload-download

Goal: finalize the networking seam. HTTP `POST /upload` (multipart attachment bytes → content reference) and `GET /download/:id` (serve `ExportHtml` output bytes); Vite dev proxy for `/ws`, `/upload`, `/download`; `RemoteAgent` hybrid attachment dispatch (inline small base64 vs. upload reference for large files); export flow (`export_html` response path → `GET /download/:id` → browser download); document-fetch proxy applied client-side for `extract_document` only.

Files to create / edit: `crates/web-ui/src/upload.rs`, `crates/web-ui/src/download.rs`, edit `app.rs` to mount them; `web/src/client/upload.ts`; edit `vite.config.ts` for dev proxy; edit `remote-agent.ts` for hybrid attachment dispatch and export download.

Dependencies: M6 (attachments), M0 (router), M4/M9 (export flow surfaces).

Acceptance criteria:
- `cargo check -p hand-web-ui` and `cargo clippy -p hand-web-ui -- -D warnings` pass; `tsc --noEmit` passes.
- Observable: attaching a large file uploads via `POST /upload` and the prompt references it (small images still inline); exporting a session writes server-side and the browser downloads it via `GET /download/:id`; the Vite dev server proxies `/ws`, `/upload`, `/download` to the Rust server so HMR + live backend work together.

### M11 — i18n / format / theming / design system

Goal: complete the presentation layer. `i18n()` / `setLanguage()` / `translations` (~200 keys, en + de, reproduced exactly including `{param}` placeholders, brand-neutral); `format.ts` (`formatUsage`, `formatCost`, `formatModelCost`, `formatTokenCount`); the de-branded `ui/` design-system primitives not yet created (`button.ts` incl. `CopyButton`/`DownloadButton`, `input.ts` `fc()` `Input`, `theme-toggle.ts`, etc.); `app.css` design tokens (CSS custom properties for background/foreground/border/muted/secondary), the `@keyframes shimmer` + `.animate-shimmer`, thin-scrollbar rules, and the user-message gradient — all vendored locally, no external theme import.

Files to create / finalize: `web/src/utils/i18n.ts`, `web/src/utils/format.ts`, remaining `web/src/ui/*.ts`, finalize `web/src/app.css`.

Dependencies: spans all UI milestones (every component references format/i18n/tokens). Land late so the key set and token set are complete and exact.

Acceptance criteria:
- `tsc --noEmit` passes.
- Observable: every UI string resolves through `i18n()` (no hardcoded reference-brand strings); `formatUsage` produces the `↑Xk ↓Xk RXk WXk $X.XXXX` summary; the thinking-block shimmer animation plays while streaming (the `animate-shimmer` keyframe exists); the theme toggle switches light/dark via the CSS custom properties; a grep for forbidden brand substrings over `crates/web-ui/web/src/` and `crates/web-ui/src/` returns zero matches.

### M12 — Polish, single-binary packaging, parity verification

Goal: ship the self-contained binary and verify 100% parity. `rust-embed` embeds `web/dist/**` into the release binary; build ordering wrapper (`scripts/` task or `make web-ui`) runs the Vite build then `cargo build --release`; optional `build.rs` asserts `web/dist` exists for release builds; the Capability Parity Matrix is walked and every row is checked or has a documented intentional omission in the Decision Log; an end-to-end manual smoke test exercises every subsystem.

Files to create / finalize: `crates/web-ui/build.rs` (optional), a build wrapper script, finalize `crates/web-ui/README.md` (brand-neutral usage), and complete the Capability Parity Matrix status column.

Dependencies: M0–M11.

Acceptance criteria:
- `cargo build -p hand-web-ui --release` produces a single binary that serves the embedded frontend (no external files); running it and opening the URL exercises the full app offline of the Vite dev server.
- `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`, and `tsc --noEmit` all pass; `cargo test -p hand-web-ui` passes (server bridge/dispatch tests).
- Every row of the Capability Parity Matrix is marked DONE or has a Decision-Log-documented intentional omission; the parity verification in §Verification is satisfied.

## Capability Parity Matrix

This matrix is the 100%-alignment checklist. It enumerates every capability surfaced across all analyzed subsystems. Status starts as TODO and is advanced to DONE as each capability lands in its milestone. Completeness over brevity: when in doubt, a capability gets its own row.

| # | Capability | Subsystem | Milestone | Status |
|---|---|---|---|---|
| 1 | Workspace member `crates/web-ui` added; `hand-web-ui` binary builds | scaffold | M0 | DONE |
| 2 | Vite + Tailwind v4 + tsc frontend project builds and type-checks | scaffold | M0 | DONE |
| 3 | Shared WS wire types (envelope + client→/server→ catalogs) in TS | scaffold | M0 | DONE |
| 4 | axum router: serve assets + `/ws` upgrade; per-connection `AgentSession` task | scaffold | M0 | DONE |
| 5 | WS text-frame ↔ `RpcCommand`/`RpcResponse`/`EventEnvelope` bridge | scaffold | M0 | DONE |
| 6 | In-process `AgentSession` via `AgentSessionConfig` (no subprocess) | scaffold | M0 | DONE |
| 7 | Hello-world: one `prompt` streams one assistant reply end-to-end | scaffold | M0 | DONE |
| 8 | `RemoteAgent` implements local `Agent` interface | scaffold/chat-shell | M0/M1 | TODO |
| 9 | `RemoteAgent` maps the seven UI-facing `AgentEvent` variants from server frames | chat-shell | M1 | DONE |
| 10 | `RemoteAgent` mirrors `AgentState` (messages, model, thinkingLevel, tools, pendingToolCalls, isStreaming) | chat-shell | M1 | DONE |
| 11 | `RemoteAgent` initial `session_state` hydration on connect and after `agent_end` | chat-shell | M1 | DONE |
| 12 | ChatPanel layout orchestrator (`<hand-chat-panel>`, 800px breakpoint, mobile overlay) | chat-shell | M1 | DONE |
| 13 | Floating "Artifacts N" pill when artifacts exist and panel collapsed | chat-shell | M1 | DONE |
| 14 | ChatPanel `setAgent(agent, config)` with config hooks (onApiKeyRequired, onBeforeSend, onCostClick, onModelSelect, sandboxUrlProvider, toolsFactory) | chat-shell | M1 | DONE |
| 15 | ChatPanel artifact reconstruction null-`onArtifactsChange` ordering on load | chat-shell/artifacts | M1/M4 | TODO |
| 16 | AgentInterface conversational shell (`<agent-interface>`) | chat-shell | M1 | DONE |
| 17 | Auto-scroll: ResizeObserver + scroll listener, disable on scroll-up, re-enable near bottom | chat-shell | M1 | DONE |
| 18 | Auto-scroll clientHeight-shrink guard (stats bar appearance must not false-disable) | chat-shell | M1 | DONE |
| 19 | AgentInterface queries `.overflow-y-auto` / `.max-w-3xl` to attach observers | chat-shell | M1 | DONE |
| 20 | Per-turn cost stats bar with optional onCostClick | chat-shell | M1 | DONE |
| 21 | Abort button + Escape-to-abort while streaming | chat-shell | M1 | DONE |
| 22 | API-key gating + onApiKeyRequired hook | chat-shell/dialogs | M1/M9 | TODO |
| 23 | onBeforeSend hook | chat-shell | M1 | DONE |
| 24 | MessageList stable renderer with keyed `repeat()` | chat-shell | M1 | DONE |
| 25 | MessageList skips `artifact` role; pairs toolResult by toolCallId | chat-shell | M1 | DONE |
| 26 | MessageList hides pending tool calls (`hidePendingToolCalls=isStreaming`) | chat-shell | M1 | DONE |
| 27 | StreamingMessageContainer live renderer with rAF batching | chat-shell | M1 | DONE |
| 28 | StreamingMessageContainer deep-clone dirty-check (`structuredClone`) | chat-shell | M1 | DONE |
| 29 | StreamingMessageContainer pulsing cursor before first token; hides after message_end | chat-shell | M1 | DONE |
| 30 | MessageEditor auto-growing textarea (field-sizing, max-height 200px) | chat-shell | M1 | DONE |
| 31 | MessageEditor Enter-to-send / Shift+Enter newline / IME composition guard | chat-shell | M1 | DONE |
| 32 | MessageEditor left toolbar: paperclip + thinking-level Select (only when model.reasoning) | chat-shell | M1 | DONE |
| 33 | MessageEditor right toolbar: model-id button + send/stop toggle | chat-shell | M1 | DONE |
| 34 | AgentInterface props/methods (setInput, setAutoScroll, sendMessage, enable* flags) | chat-shell | M1 | DONE |
| 35 | UserMessage renderer (`user-message`, markdown + attachment chips) | message-tool-rendering | M2 | DONE |
| 36 | AssistantMessage renderer (`assistant-message`, ordered text/thinking/toolCall, usage, error/aborted) | message-tool-rendering | M2 | DONE |
| 37 | ToolMessage renderer (`tool-message`, aborted-stub synthesis, isCustom card wrap) | message-tool-rendering | M2 | DONE |
| 38 | ToolMessageDebugView (`tool-message-debug`, raw args+result code-blocks) | message-tool-rendering | M2 | DONE |
| 39 | AbortedMessage renderer (`aborted-message`) | message-tool-rendering | M2 | DONE |
| 40 | ThinkingBlock collapsible reasoning with shimmer header while streaming | message-tool-rendering | M2 | DONE |
| 41 | ExpandableSection reusable accordion (light-DOM child capture) | message-tool-rendering | M2 | DONE |
| 42 | ConsoleBlock scrolling output pane with copy + error variant | message-tool-rendering | M2 | DONE |
| 43 | Input functional component (`fc()` Input) | message-tool-rendering/ui | M2/M11 | TODO |
| 44 | Message renderer registry (register/get/renderMessage, MessageRole) | message-tool-rendering | M2 | DONE |
| 45 | Tool renderer registry (renderTool, register/getToolRenderer, toolRenderers map) | message-tool-rendering | M2 | DONE |
| 46 | renderHeader / renderCollapsibleHeader helpers (max-h + chevron ref toggle) | message-tool-rendering | M2 | DONE |
| 47 | setShowJsonMode global force-default-renderer toggle | message-tool-rendering | M2 | DONE |
| 48 | BashRenderer (three states, `> command` + output console-block) | message-tool-rendering | M2 | DONE |
| 49 | CalculateRenderer (four progressive text states + error layout) | message-tool-rendering | M2 | DONE |
| 50 | DefaultRenderer (state derivation, JSON pretty-print, Input/Output code-blocks) | message-tool-rendering | M2 | DONE |
| 51 | GetCurrentTimeRenderer (seven param/result/timezone paths) | message-tool-rendering | M2 | DONE |
| 52 | Message type extension system (CustomAgentMessages declaration merge) | chat-shell/core | M1 | DONE |
| 53 | defaultConvertToLlm (filters artifact, expands user-with-attachments) | chat-shell/core | M1 | DONE |
| 54 | convertAttachments (images→ImageContent, docs→TextContent header) | chat-shell/core | M1 | DONE |
| 55 | isUserMessageWithAttachments / isArtifactMessage guards | chat-shell/core | M1 | DONE |
| 56 | SandboxedIframe `execute()` (transient hidden iframe, 120s timeout, AbortSignal) | sandbox-runtime | M3 | DONE |
| 57 | SandboxedIframe `loadContent()` (persistent visible iframe for HTML artifacts) | sandbox-runtime | M3 | DONE |
| 58 | SandboxedIframe `prepareHtmlDocument()` (public; standalone download assembly) | sandbox-runtime | M3 | DONE |
| 59 | Sandbox `srcdoc` + optional `sandboxUrlProvider` (extension CSP) delivery modes | sandbox-runtime | M3 | DONE |
| 60 | Navigation interceptor (link/form → open-external-url postMessage) | sandbox-runtime | M3 | DONE |
| 61 | HTML validation gate (DOMParser parsererror → error page) | sandbox-runtime | M3 | DONE |
| 62 | RUNTIME_MESSAGE_ROUTER singleton dispatcher (register/set/add/remove/unregister) | sandbox-runtime | M3 | DONE |
| 63 | RuntimeMessageBridge `generateBridgeCode()` (sendRuntimeMessage, onCompleted, completionCallbacks) | sandbox-runtime | M3 | DONE |
| 64 | ConsoleRuntimeProvider (console override, complete() lifecycle, error handlers) | sandbox-runtime | M3 | DONE |
| 65 | ArtifactsRuntimeProvider (list/get/createOrUpdate/delete globals; online + offline; readWrite) | sandbox-runtime/artifacts | M3/M4 | TODO |
| 66 | AttachmentsRuntimeProvider (list/readText/readBinary attachment globals) | sandbox-runtime/attachments | M3/M6 | TODO |
| 67 | FileDownloadRuntimeProvider (returnDownloadableFile; online + offline download) | sandbox-runtime | M3 | DONE |
| 68 | getRuntime().toString() injection constraint (no closures/imports) preserved | sandbox-runtime | M3 | DONE |
| 69 | Sandbox iframe attribute `allow-scripts allow-modals` only | sandbox-runtime | M3 | DONE |
| 70 | ArtifactElement abstract base (light DOM, content get/set, getHeaderButtons) | artifacts | M4 | DONE |
| 71 | ArtifactsPanel (`<artifacts-panel>`, Map-backed state, imperative DOM insertion, tab bar) | artifacts | M4 | DONE |
| 72 | getFileType dispatch table (html/svg/markdown/image/pdf/excel/docx/text/generic) | artifacts | M4 | DONE |
| 73 | Artifact CRUD tool commands: create/update/rewrite/get/delete/logs | artifacts | M4 | DONE |
| 74 | Artifacts tool: html-create waits ≤1500ms for logs; reloadAllHtmlArtifacts after CRUD | artifacts | M4 | DONE |
| 75 | reconstructFromMessages (replays artifact + toolResult history, silent + skipWait) | artifacts | M4 | DONE |
| 76 | HtmlArtifact (sandbox iframe, console capture, preview/code toggle, reload/copy/download) | artifacts | M4 | DONE |
| 77 | HtmlArtifact standalone download with injected runtime | artifacts | M4 | DONE |
| 78 | SvgArtifact (Blob-URL preview + code view, copy/download) | artifacts | M4 | DONE |
| 79 | MarkdownArtifact (markdown-block preview + code view) | artifacts | M4 | DONE |
| 80 | TextArtifact (hljs highlight for code extensions, else plain pre) | artifacts | M4 | DONE |
| 81 | ImageArtifact (data-URL render, MIME map, error placeholder, download) | artifacts | M4 | DONE |
| 82 | PdfArtifact (pdfjs all-pages canvas at scale 1.5, worker config, download) | artifacts | M4 | DONE |
| 83 | DocxArtifact (docx-preview renderAsync + style overrides, download) | artifacts | M4 | DONE |
| 84 | ExcelArtifact (xlsx multi-sheet tabs + styled tables, download) | artifacts | M4 | DONE |
| 85 | GenericArtifact (placeholder + download with extension MIME map) | artifacts | M4 | DONE |
| 86 | artifact-console (collapsible, error count, autoscroll, copy) | artifacts | M4 | DONE |
| 87 | ArtifactPill inline clickable badge (openArtifact navigation) | artifacts | M4 | DONE |
| 88 | ArtifactsToolRenderer (create/rewrite code-block, update Diff, get, logs, delete; registered with panel ref) | artifacts | M4 | DONE |
| 89 | Browser-tool replies via `tool_result` frame (hub + ws.rs interception; no RpcCommand change needed) | artifacts/browser-tools | M4 | DONE |
| 90 | Server-side `artifacts` tool declaration (schema + description) for system prompt | artifacts | M4 | DONE |
| 91 | createJavaScriptReplTool / javascriptReplTool (dynamic description, sandbox execute, base64 files) | browser-tools | M5 | DONE |
| 92 | executeJavaScript utility (hidden iframe, console+returnValue+files, 120s timeout, abort) | browser-tools | M5 | DONE |
| 93 | javascriptReplRenderer (collapsible code + console + attachment chips; auto-register) | browser-tools | M5 | DONE |
| 94 | createExtractDocumentTool / extractDocumentTool (fetch 50MB limit, CORS fallback, loadAttachment) | browser-tools | M5 | DONE |
| 95 | extractDocumentRenderer (collapsible URL + extracted text / error console; auto-register) | browser-tools | M5 | DONE |
| 96 | extract-document neutral CORS-fallback message (no reference brand) | browser-tools | M5 | DONE |
| 97 | isCorsError predicate (extract-document fallback only) | browser-tools/utils | M5 | DONE |
| 98 | Tool auto-registration via side-effect imports (`tools/index.ts`) | browser-tools | M5 | DONE |
| 99 | RemoteAgent routes browser-tool calls to local execute and replies with `tool_result` | browser-tools/client | M5 | DONE |
| 100 | Server-side `javascript_repl` / `extract_document` tool declarations for system prompt | browser-tools | M5 | DONE |
| 101 | Attachment data model (id, type, fileName, mimeType, size, content base64, extractedText?, preview?) | attachments | M6 | DONE |
| 102 | loadAttachment universal ingestion (URL/File/Blob/ArrayBuffer) | attachments | M6 | DONE |
| 103 | PDF ingestion (page-tagged XML text + 160×160 thumbnail) | attachments | M6 | DONE |
| 104 | DOCX ingestion (docx-preview AST walk, tables) | attachments | M6 | DONE |
| 105 | PPTX ingestion (jszip + `<a:t>` regex, slide + notes) | attachments | M6 | DONE |
| 106 | Excel/XLS ingestion (xlsx sheet→CSV per sheet) | attachments | M6 | DONE |
| 107 | Image ingestion (base64 + preview identity) | attachments | M6 | DONE |
| 108 | Plain-text ingestion (TextDecoder, extension allowlist) | attachments | M6 | DONE |
| 109 | Chunked base64 encoding (0x8000) for large files | attachments | M6 | DONE |
| 110 | AttachmentTile (`attachment-tile`, thumbnail/icon, PDF badge, delete, opens overlay) | attachments | M6 | DONE |
| 111 | AttachmentOverlay (`attachment-overlay`, full-screen, header, backdrop/Escape close) | attachments | M6 | DONE |
| 112 | AttachmentOverlay PDF viewer (all pages canvas scale 1.5, task cleanup) | attachments | M6 | DONE |
| 113 | AttachmentOverlay DOCX viewer (renderAsync + style overrides) | attachments | M6 | DONE |
| 114 | AttachmentOverlay Excel viewer (multi-sheet tabs, styled tables) | attachments | M6 | DONE |
| 115 | AttachmentOverlay PPTX viewer (extracted-text pre) | attachments | M6 | DONE |
| 116 | AttachmentOverlay image viewer | attachments | M6 | DONE |
| 117 | AttachmentOverlay plain-text viewer | attachments | M6 | DONE |
| 118 | AttachmentOverlay extracted-text toggle (PDF/DOCX/Excel) | attachments | M6 | DONE |
| 119 | AttachmentOverlay file download (base64→Blob→anchor) | attachments | M6 | DONE |
| 120 | AttachmentOverlay error display state | attachments | M6 | DONE |
| 121 | pdfjs-dist worker configured as Vite static asset | attachments/build | M6 | DONE |
| 122 | MessageEditor drag-and-drop file upload with overlay | attachments/chat-shell | M6 | DONE |
| 123 | MessageEditor clipboard paste image capture | attachments/chat-shell | M6 | DONE |
| 124 | MessageEditor file picker + attachment tile row with delete (max 10, 20MB, accepted types) | attachments/chat-shell | M6 | DONE |
| 125 | StorageBackend / StorageTransaction interfaces | storage | M7 | DONE |
| 126 | IndexedDBStorageBackend (lazy open, schema config, prefix scan, cursor index, quota, persist) | storage | M7 | DONE |
| 127 | Store abstract base + setBackend/getBackend | storage | M7 | DONE |
| 128 | AppStorage facade + getAppStorage/setAppStorage singleton | storage | M7 | DONE |
| 129 | SettingsStore | storage | M7 | DONE |
| 130 | ProviderKeysStore (per-provider key, has/list, never exposes value) | storage | M7 | DONE |
| 131 | SessionsStore dual-store (sessions + sessions-metadata, atomic save/delete) | storage | M7 | DONE |
| 132 | SessionsStore getAllMetadata desc, getLatestSessionId, updateTitle (single transaction) | storage | M7 | DONE |
| 133 | CustomProvidersStore (UUID-keyed, getAll, has) | storage | M7 | DONE |
| 134 | SessionMetadata / SessionData / CustomProvider types | storage | M7 | DONE |
| 135 | IndexedDB schema config types (IndexedDBConfig/StoreConfig/IndexConfig) | storage | M7 | DONE |
| 136 | Auto-save: persist message history to IndexedDB on RemoteAgent state updates | storage/client | M7 | DONE |
| 137 | Model registry enumeration (built-in providers + models, served via `get_available_models`) | providers-models | M8 | TODO |
| 138 | Subsequence-scored fuzzy model search | providers-models | M8 | TODO |
| 139 | Capability filter: reasoning/thinking models | providers-models | M8 | TODO |
| 140 | Capability filter: vision/image models | providers-models | M8 | TODO |
| 141 | Keyboard-navigable model picker dialog (`hand-model-selector`, IME-safe, current-model checkmark) | providers-models | M8 | TODO |
| 142 | allowedProviders filter for the model selector | providers-models | M8 | TODO |
| 143 | Model cost formatting (formatModelCost, formatTokens K/M) | providers-models/utils | M8/M11 | TODO |
| 144 | Custom provider CRUD (UUID-keyed IndexedDB) | providers-models | M8 | TODO |
| 145 | Auto-discovery: Ollama (tools capability filter, context_length) | providers-models | M8 | TODO |
| 146 | Auto-discovery: llama.cpp (`/v1/models`) | providers-models | M8 | TODO |
| 147 | Auto-discovery: vLLM (`/v1/models`, max_model_len) | providers-models | M8 | TODO |
| 148 | Auto-discovery: LM Studio (`@lmstudio/sdk` WebSocket) | providers-models | M8 | TODO |
| 149 | Custom provider dialog Test Connection (discoverModels, first-5 list) | providers-models | M8 | TODO |
| 150 | CustomProviderCard status indicator (connected/checking/disconnected) | providers-models | M8 | TODO |
| 151 | Default base-URL prefill by provider type | providers-models | M8 | TODO |
| 152 | ProviderKeyInput (`provider-key-input`, show key presence without revealing) | providers-models | M8 | TODO |
| 153 | API-key validation via server round-trip (replaces in-browser completion) | providers-models | M8 | TODO |
| 154 | ProvidersModelsTab (`providers-models-tab`, cloud + custom sections, add/edit/refresh/delete) | providers-models | M8 | TODO |
| 155 | SettingsTab abstract base | dialogs-settings | M9 | TODO |
| 156 | ApiKeysTab (`api-keys-tab`) | dialogs-settings | M9 | TODO |
| 157 | ProxyTab (`proxy-tab`, document-fetch proxy config) | dialogs-settings | M9 | TODO |
| 158 | SettingsDialog (`settings-dialog`, sidebar/mobile-strip nav, display:none tab toggle) | dialogs-settings | M9 | TODO |
| 159 | SessionListDialog (`session-list-dialog`, metadata cards, relative dates, usage, in-UI delete confirm) | dialogs-settings | M9 | TODO |
| 160 | ApiKeyPromptDialog (`api-key-prompt-dialog`, storage-poll resolve, interval cleanup) | dialogs-settings | M9 | TODO |
| 161 | PersistentStorageDialog (`persistent-storage-dialog`, navigator.storage.persist, graceful fallback) | dialogs-settings | M9 | TODO |
| 162 | DialogBase modal base (open/close, backdrop, modalWidth/Height) | dialogs-settings/ui | M8/M9 | TODO |
| 163 | App header: sessions / new-session / inline-editable title / theme toggle / settings | dialogs-settings/bootstrap | M9 | TODO |
| 164 | POST /upload endpoint (attachment bytes → content reference) | proxy-networking | M10 | TODO |
| 165 | GET /download/:id endpoint (serve ExportHtml output bytes) | proxy-networking | M10 | TODO |
| 166 | Vite dev proxy for /ws, /upload, /download | proxy-networking/build | M10 | TODO |
| 167 | RemoteAgent hybrid attachment dispatch (inline small base64 vs. upload reference) | proxy-networking/client | M10 | TODO |
| 168 | Export flow: export_html response → GET /download/:id → browser download | proxy-networking | M10 | TODO |
| 169 | Document-fetch proxy applied client-side for extract_document only | proxy-networking | M10 | TODO |
| 170 | WsConnection lifecycle (connect, reconnect, framing) | proxy-networking/client | M0/M10 | TODO |
| 171 | i18n translation system (~200 keys, en + de, exact placeholders, brand-neutral) | utils/i18n | M11 | TODO |
| 172 | i18n() / setLanguage() / translations exports | utils/i18n | M11 | TODO |
| 173 | formatUsage / formatCost / formatTokenCount | utils/format | M11 | TODO |
| 174 | ui/ design-system primitives (Button, CopyButton, DownloadButton, Select, Switch, Badge, Label) | ui | M11 | TODO |
| 175 | markdown-block / code-block / diff / preview-code-toggle / theme-toggle / mode-toggle elements | ui | M2/M4/M6/M11 | TODO |
| 176 | app.css design tokens (CSS custom properties) | theming | M11 | TODO |
| 177 | @keyframes shimmer + .animate-shimmer (thinking block) | theming | M11 | TODO |
| 178 | Thin-scrollbar rules + user-message gradient (brand-neutral palette) | theming | M11 | TODO |
| 179 | Tool description prompt constants reproduced verbatim, brand-neutral (prompts.ts) | utils/prompts | M5 | DONE |
| 180 | WS command catalog: prompt/steer/follow_up/abort/abort_bash/bash | wire/backend-seam | M1/M5 | TODO |
| 181 | WS command catalog: new_session/switch_session/fork/clone | wire/backend-seam | M9 | TODO |
| 182 | WS command catalog: get_state/get_messages/get_fork_messages/get_last_assistant_text | wire/backend-seam | M1 | DONE |
| 183 | WS command catalog: set_model/cycle_model/get_available_models | wire/backend-seam | M8 | TODO |
| 184 | WS command catalog: set_thinking_level/cycle_thinking_level | wire/backend-seam | M1 | DONE |
| 185 | WS command catalog: set_steering_mode/set_follow_up_mode | wire/backend-seam | M1 | DONE |
| 186 | WS command catalog: compact/set_auto_compaction/set_auto_retry/abort_retry | wire/backend-seam | M1 | DONE |
| 187 | WS command catalog: get_session_stats/export_html/set_session_name/get_commands | wire/backend-seam | M9/M10 | TODO |
| 188 | Extension UI protocol: extension_ui_request server→client (select/confirm/input/editor/notify/setStatus/setWidget/setTitle/set_editor_text) | wire/backend-seam | M9 | TODO |
| 189 | Extension UI protocol: extension_ui_response client→server | wire/backend-seam | M9 | TODO |
| 190 | Event catalog: agent_start/turn_start/message_start/message_update/message_end/turn_end/agent_end | wire/backend-seam | M1 | DONE |
| 191 | Event catalog: tool_execution_start/update/end | wire/backend-seam | M2 | DONE |
| 192 | Event catalog: compaction_start/end, error, session_info_changed | wire/backend-seam | M1/M9 | TODO |
| 193 | Reuse run_rpc_server dispatch (Prompt/Bash interruptible select! races) unchanged | backend-seam | M0/M1 | TODO |
| 194 | session.subscribe → per-connection outbound WS channel | backend-seam | M0 | DONE |
| 195 | API keys resolved server-side from env (never sent to browser) | backend-seam | M0/M8 | TODO |
| 196 | rust-embed single-binary asset serving (release) | build | M12 | TODO |
| 197 | Build ordering wrapper (Vite build → cargo build --release) | build | M12 | TODO |
| 198 | Two-terminal dev workflow (cargo --dev + Vite HMR via proxy) | build | M10/M12 | TODO |
| 199 | Brand-neutrality: zero forbidden substrings across frontend + server source | de-branding | M11/M12 | TODO |
| 200 | Custom message extension pattern (CustomAgentMessages declaration-merge + custom renderer + customConvertToLlm) | utils-wiring/core | M2 | DONE |
| 201 | agent.steer() exposed on RemoteAgent for custom-message injection | client | M1 | DONE |
| 202 | Documented carry-forward constraints (get_state latency, Compact.customInstructions dropped, absolute session paths) | backend-seam | M12 | TODO |

## Verification and Acceptance

"100% parity" is demonstrated by the conjunction of two things:

1. **The Capability Parity Matrix is fully checked.** Every row in §Capability Parity Matrix is marked DONE, or carries a Decision-Log entry explaining an intentional, justified omission (e.g. a reference behavior whose underlying native support is documented as not-yet-available, such as `Compact.custom_instructions` being dropped server-side). No row may be silently skipped.

2. **Every milestone's acceptance criteria are met.** For each milestone M0..M12 the listed gates pass:
   - Rust gates: `cargo check -p hand-web-ui`, `cargo clippy -p hand-web-ui -- -D warnings`, and (where the milestone adds server logic) `cargo test -p hand-web-ui`. The workspace-wide `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` pass at M12.
   - Frontend gate: `tsc --noEmit` (`npm --prefix crates/web-ui/web run typecheck`) passes at every milestone; `npm run build` produces `web/dist/` from M0 onward.
   - Observable behavior: each milestone lists a concrete, human-observable behavior in the running app; M12's behavior is the full end-to-end smoke test exercising every subsystem from the single release binary.

The plan is complete when the release binary built by M12 serves the embedded frontend with no external file dependencies, a user can perform every action listed in §Purpose, and a brand-neutrality grep over `crates/web-ui/web/src/` and `crates/web-ui/src/` for the forbidden substrings returns zero matches.

## Idempotence and Recovery

Every step is a file create/edit on tracked files plus npm install of frontend dependencies; no irreversible external state is created. Failed Rust builds revert with `git restore`; a broken frontend reverts the same way and re-runs `npm install`. Each milestone is an independent branch off the web-ui feature base, so a failure in a later milestone does not block an earlier one from merging. If the workspace fails to build after adding `crates/web-ui` to members, the safest recovery is to keep the crate's `src/main.rs` minimal (M0) until each subsystem lands, so `cargo check --workspace` stays green throughout. Per the repository commit convention, run an atomic commit after each logical change.
