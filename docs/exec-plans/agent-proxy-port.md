# ExecPlan: Port `proxy.ts` → `hand-agent::proxy`

**Status:** Draft
**Author:** Gump
**Date:** 2026-05-07

This is a living document. The Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective sections must be kept up to date as work proceeds.

## Purpose

After this change, a downstream user of `hand-agent` can call `stream_proxy(model, context, options)` to obtain an `AssistantMessageEventStream` whose events are sourced from a server-side LLM proxy (the proxy holds provider auth, strips the `partial` field from delta events to save bandwidth, and the client reconstructs the partial `AssistantMessage` locally). This unblocks the Rust port of `pi-agent-core` reaching feature parity with its TypeScript origin: every other `.ts` file in `pi-mono/packages/agent/src/` already has a working Rust counterpart, and `proxy.ts` is the last missing piece.

The user-visible behavior:

- The free function `hand_agent::stream_proxy(...)` returns a stream with the **same item type** (`AssistantMessageEvent`) and **same shape** (`Pin<Box<dyn Stream<…> + Send + 'static>>`) as `model::Client::stream_simple(...)`. Anything that consumes the stream from the model crate works unchanged with the proxy stream.
- Aborting the run via a passed `tokio_util::sync::CancellationToken` cancels the in-flight HTTP request and ends the stream with a synthesized `AssistantMessageEvent::Error { reason: Aborted, error }`.
- A future small follow-up (T6, optional in this plan) adds a `stream_fn` injection point on `AgentLoopConfig` so an `Agent` instance can be configured to use the proxy as its transport.

## Progress

- [ ] T1 — Scaffold the module and dependencies
- [ ] T2 — Define `ProxyAssistantMessageEvent` (line-protocol enum)
- [ ] T3 — Define `ProxyStreamOptions` and the proxy request body
- [ ] T4 — Implement `process_proxy_event` (pure reducer) + unit tests
- [ ] T5 — Implement `stream_proxy` (HTTP + SSE + cancellation)
- [ ] T6 — Wire `stream_fn` injection into `AgentLoopConfig` so `Agent` can opt-in
- [ ] T7 — Integration test against a mocked proxy server (`wiremock`)
- [ ] T8 — Public API surface, README example, final verification

## Surprises & Discoveries

(None yet)

## Decision Log

- **2026-05-07**: Place the new module in `packages/agent/src/proxy.rs`, not in `packages/model/src/proxy.rs`. The earlier draft `docs/exec-plans/agent-port-parity.md` (M3.T2) suggested `model::stream_proxy`. Reasoning to override that earlier note: the proxy is an agent-flavoured transport (it bundles `Model` + `Context` + agent-style options into one HTTP call) and conceptually pairs with `Agent::stream_fn`. Keeping it in `hand-agent` mirrors the TS layout (`pi-agent-core/src/proxy.ts`) and avoids polluting `hand-model` with the proxy wire format. `hand-model` retains its single responsibility: provider catalog + direct streaming.
- **2026-05-07**: Use a hand-rolled SSE line buffer (matching `pi-mono/packages/agent/src/proxy.ts:181-206`) rather than pulling in the `eventsource-stream` crate. Reasoning: the protocol is trivial (one `data: <json>` per line, `\n\n` boundaries, no `event:` names, no last-id, no retries), `hand-model` already uses the same pattern in `providers/anthropic_messages.rs:546-559`, and avoiding a dependency keeps the agent crate's surface small. We will, however, switch to a streaming `bytes_stream()` rather than `response.text().await` (which the anthropic provider uses) so that long-running streams begin emitting events immediately.
- **2026-05-07**: Returned stream type is `Pin<Box<dyn Stream<Item = AssistantMessageEvent> + Send + 'static>>` — exactly the alias `model::AssistantMessageEventStream<'static>`. Re-using the alias keeps `stream_proxy` plug-compatible with the future `stream_fn` injection point (T6).
- **2026-05-07**: Cancellation goes through `tokio_util::sync::CancellationToken` — `ProxyStreamOptions::cancel` is `Option<CancellationToken>`, not `AbortSignal`/`Box<dyn Fn>`. This matches the rest of the agent crate (already passes `CancellationToken` everywhere per `agent.rs:736-740`) and lets the implementation use `tokio::select!` to race the HTTP body read against the cancel future.
- **2026-05-07**: New error variant lives on `AgentError::Proxy { status: u16, message: String }`, not on a separate `ProxyError` type, so a single `?` works in the body of `stream_proxy` and any future agent code that calls into the proxy. Network errors from `reqwest` use `AgentError::Other` via `String` — no `From<reqwest::Error>` impl on `AgentError` is needed because the only call site that produces them is the proxy itself.

## Outcomes & Retrospective

(To be filled at completion)

## Context and Orientation

Related documents:
- Conversion guidelines: `docs/conversion-guidelines.md` — TS→Rust idioms used throughout (`Promise<T>` → `async fn`, `AsyncIterable` → `Stream`, union types → tagged enum, `throw` → `Result<T, E>`).
- Sister plan: `docs/exec-plans/agent-port-parity.md` — supersedes its M3.T2 entry (location and signature differ, see Decision Log above).
- TypeScript reference (sole source of behaviour): `/Users/wanggang/dev/opensource/pi-mono/packages/agent/src/proxy.ts` (367 lines).

Key source files to read before starting:
- `/Users/wanggang/dev/opensource/pi-mono/packages/agent/src/proxy.ts` — line-by-line behavioural reference. Important regions: 36–80 (event/options types), 116–233 (stream driver), 238–367 (`processProxyEvent` reducer).
- `packages/agent/src/lib.rs` — current public re-exports; the new module is added here.
- `packages/agent/src/error.rs` — extend with one variant.
- `packages/agent/Cargo.toml` — add the small set of new dependencies.
- `packages/model/src/types.rs:740-748` (`ToolCall`), `:836-840` (`AssistantContentBlock`), `:842-863` (`AssistantMessage`), `:967-972` (`Context`), `:489-561` (`StreamOptions` / `SimpleStreamOptions`), `:769-781` (`StopReason`), `:977-1031` (`AssistantMessageEvent`), `:359-378` (`Usage`). These are the building blocks the proxy module composes.
- `packages/model/src/utils/json_parse.rs:29` — `safe_parse_partial(&str) -> Option<Value>`. This is the Rust analogue of TS `parseStreamingJson`. Used by `process_proxy_event` for the `toolcall_delta` branch.
- `packages/model/src/api_registry.rs:14-16` — `pub type AssistantMessageEventStream<'a> = Pin<Box<dyn Stream<Item = AssistantMessageEvent> + Send + 'a>>`. The proxy returns this exact alias parameterised at `'static`.
- `packages/model/src/providers/anthropic_messages.rs:546-559` — the pattern for `data: <json>` SSE line decoding already used elsewhere in the workspace; copy the shape, but feed it from `bytes_stream()` instead of `response.text().await`.
- `packages/agent/tests/common/mod.rs` — shared test helpers (`test_model`, `MockTextProvider`).

How the pieces fit (one paragraph):

A proxy server (out of scope) accepts a JSON request `{ model, context, options }` at `POST {proxyUrl}/api/stream`, opens an upstream LLM stream itself, and re-serializes each `AssistantMessageEvent` minus the `partial` field as a one-line `data: <json>\n` SSE record terminated by an empty line. The Rust client `stream_proxy` opens that POST, reads the response body as a stream of `Bytes`, splits on `\n`, parses each `data: …` payload into `ProxyAssistantMessageEvent`, and runs it through `process_proxy_event` — which mutates a long-lived `AssistantMessage` (`partial`) on the client side and returns a fully-formed `AssistantMessageEvent` carrying that partial. The function emits these events through an `async_stream::stream! { … }` block that `yield`s on each successful event. On HTTP error, the body is parsed once for `{ "error": "..." }`, surfaced as `AgentError::Proxy`. On cancellation, an `Error { reason: Aborted, error: partial }` event is emitted, the stream ends, and any in-flight HTTP read is cancelled because the `bytes_stream()` is dropped.

Terms used in this plan:
- **Partial reconstruction**: the proxy server omits the `partial` field from `AssistantMessageEvent::*` variants on the wire. The client maintains its own `partial: AssistantMessage` and, for each incoming event, mutates it (e.g. append a delta to the right content block) and re-attaches it to the local event before yielding. This preserves identity with `model::Client::stream_simple`'s output.
- **Line buffer**: incremental decoder. New bytes are decoded as UTF-8, appended to a `String`, split on `'\n'`, and the last (possibly partial) segment is preserved across reads.
- **SSE record**: in this protocol, exactly one line of the form `data: <json>` followed by `\n`. There are no `event:`/`id:`/`retry:` fields.

## Plan of Work

The work is sliced vertically. Each task is independently verifiable; T1–T5 form a thin slice that delivers a usable `stream_proxy` against a `wiremock` server (T7); T6 wires it into `Agent`; T8 polishes the public surface.

### T1 — Scaffold module and dependencies

Create `packages/agent/src/proxy.rs` with module-level docstring referencing the TS source path, and an empty `pub use` block. Register it in `packages/agent/src/lib.rs` with `pub mod proxy;`. Add to `packages/agent/Cargo.toml`:

- `reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json", "stream"] }` — `stream` feature is required for `bytes_stream()`. `rustls-tls` matches the policy already in use elsewhere (verify by inspecting `packages/model/Cargo.toml`; if the model crate uses default `native-tls`, mirror that instead).
- `bytes = "1"` — for working with `bytes::Bytes` chunks from `reqwest`.

`futures` and `async-stream` are already present. Update `[dev-dependencies]` to add `wiremock = "0.6"` (used in T7).

Files touched: `packages/agent/Cargo.toml`, `packages/agent/src/lib.rs`, `packages/agent/src/proxy.rs` (new). ≤3 files.

Verification: `cargo check -p hand-agent` succeeds.

### T2 — `ProxyAssistantMessageEvent` enum

In `packages/agent/src/proxy.rs`, define the wire-format enum mirroring `proxy.ts:36-57`. Use `#[serde(tag = "type", rename_all = "snake_case")]` and snake_case Rust variants. Field rules:

- 13 variants: `Start`, `TextStart`, `TextDelta`, `TextEnd`, `ThinkingStart`, `ThinkingDelta`, `ThinkingEnd`, `ToolcallStart`, `ToolcallDelta`, `ToolcallEnd`, `Done`, `Error`.
- `content_index: u32` matches the alias in `model::AssistantMessageEvent`.
- `*_end` variants use `#[serde(rename = "contentSignature", default, skip_serializing_if = "Option::is_none")] content_signature: Option<String>` — the TS field is `contentSignature` (camelCase) on the wire even though the discriminator is snake_case. Verify against `proxy.ts:40,43`.
- `Done { reason: StopReason, usage: Usage }` and `Error { reason: StopReason, error_message: Option<String>, usage: Usage }`. The TS uses `Extract<StopReason, "stop" | "length" | "toolUse">` for `done` and `Extract<StopReason, "aborted" | "error">` for `error`; keep the wider `StopReason` in Rust and document the constraint in a doc comment — runtime values from the proxy will already be limited by the server.
- `Done` and `Error` carry the **non-camelCase** field `usage` directly because `Usage` already serialises with camelCase via its own derives — verify with `cargo expand` or a unit test.

Add a single `#[test]` round-tripping each variant through `serde_json::from_str` / `to_string` against fixture strings copied verbatim from a real TS proxy response. At least one fixture per variant; place fixtures inline in the test module.

Files touched: `packages/agent/src/proxy.rs`. ≤1 file.

Verification: `cargo test -p hand-agent --test proxy_event_serde` (place the test in `tests/` if external integration is preferred, or as `#[cfg(test)] mod tests` in `proxy.rs` — choose the latter, it keeps the round-trip tight to the type).

### T3 — `ProxyStreamOptions` and the request body

In `packages/agent/src/proxy.rs`, define:

```rust
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct ProxyRequestOptions {
    #[serde(skip_serializing_if = "Option::is_none")] temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")] max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")] reasoning: Option<ThinkingLevel>,
    #[serde(skip_serializing_if = "Option::is_none")] cache_retention: Option<CacheRetention>,
    #[serde(skip_serializing_if = "Option::is_none")] session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")] metadata: Option<HashMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")] transport: Option<Transport>,
    #[serde(skip_serializing_if = "Option::is_none")] thinking_budgets: Option<ThinkingBudgets>,
    #[serde(skip_serializing_if = "Option::is_none")] max_retry_delay_ms: Option<u64>,
}
```

Mirrors the `Pick<SimpleStreamOptions, …>` in `proxy.ts:59-71`. Then the public:

```rust
#[derive(Debug, Clone, Default)]
pub struct ProxyStreamOptions {
    pub auth_token: String,
    pub proxy_url: String,
    pub cancel: Option<CancellationToken>,
    pub options: SimpleStreamOptions,
}
```

`auth_token` and `proxy_url` are required; `Default` is provided so callers can `..Default::default()` against the rest. The `options` field is the full `SimpleStreamOptions` from the model crate; `stream_proxy` projects it into `ProxyRequestOptions` via a private `fn build_request_options(&SimpleStreamOptions) -> ProxyRequestOptions`. The wire body is a fixed shape:

```rust
#[derive(Serialize)]
struct ProxyRequest<'a> {
    model: &'a Model,
    context: &'a Context,
    options: ProxyRequestOptions,
}
```

Files touched: `packages/agent/src/proxy.rs`. ≤1 file.

Verification: `cargo check -p hand-agent`. Add a unit test asserting `serde_json::to_string(&ProxyRequest { ... })` produces JSON whose top-level keys are exactly `model`, `context`, `options` and that `options` only contains the keys actually set (proves `skip_serializing_if` works).

### T4 — `process_proxy_event` reducer + unit tests

Pure function. No I/O. Mirrors `processProxyEvent` in `proxy.ts:238-367` line-by-line:

```rust
fn process_proxy_event(
    event: ProxyAssistantMessageEvent,
    partial: &mut AssistantMessage,
    tool_partial_json: &mut HashMap<u32, String>,
) -> Result<Option<AssistantMessageEvent>, AgentError>
```

The extra `tool_partial_json` parameter holds the streaming JSON buffer per `content_index` for tool calls — TS hides this on the `ToolCall` content block via `(content as any).partialJson`; Rust does not allow such shenanigans and the `ToolCall` struct has no such field. The reducer:

- For `text_delta` / `thinking_delta` / `toolcall_delta`: locate `partial.content[content_index]`, mutate the appropriate field (`text`, `thinking`, or call `safe_parse_partial(buffer)` to refresh `arguments`). If the slot is not the expected variant, return `Err(AgentError::Proxy { status: 0, message: format!("…") })` matching the `throw new Error("Received text_delta for non-text content")` semantics in TS.
- For `*_start`: insert/replace the slot with a fresh `TextContent`, `ThinkingContent`, or `ToolCall` (with empty `arguments` / empty buffer in `tool_partial_json`).
- For `*_end`: set `text_signature` / `thinking_signature` from the proxy event, remove the `tool_partial_json` entry on `toolcall_end`, return the matching `AssistantMessageEvent` variant carrying the now-complete content.
- For `Done`: mutate `partial.stop_reason`, `partial.usage`, return `AssistantMessageEvent::Done { reason, message: partial.clone() }`.
- For `Error`: mutate `partial.stop_reason`, `partial.error_message`, `partial.usage`, return `AssistantMessageEvent::Error { reason, error: partial.clone() }`.
- The TS default branch warns and returns `undefined`. In Rust the enum match is exhaustive — no default arm. Translate "warn" into a `tracing::warn!` only if a future variant slips in via deserialisation, which can't happen given `#[serde(deny_unknown_fields)]` on the enum (add it).

Files touched: `packages/agent/src/proxy.rs`. ≤1 file.

Verification: in-file `#[cfg(test)] mod tests` adds 13 unit tests, one per branch, plus two cross-cutting tests:
- `text_round_trip`: feed `TextStart` → `TextDelta(" hello")` → `TextDelta(" world")` → `TextEnd { content_signature: Some("sig") }`; assert `partial.content[0]` is `Text { text: " hello world", text_signature: Some("sig") }` and the four returned events all carry that growing partial.
- `toolcall_round_trip`: same shape, with `toolcall_delta` deltas `'{"x":'` then `'1}'`. After the second delta, assert `partial.content[0]` is `ToolCall { arguments: json!({"x": 1}), … }` thanks to `safe_parse_partial` fully closing the JSON.

Run with `cargo test -p hand-agent process_proxy_event`.

### T5 — `stream_proxy` driver

Public function:

```rust
pub fn stream_proxy(
    model: &Model,
    context: Context,
    options: ProxyStreamOptions,
) -> AssistantMessageEventStream<'static>
```

Implementation outline (non-async outer, async inner via `async_stream::stream!`):

1. Clone `model` (it is `Clone` in `hand-model`) and `options.cancel` so the inner async block is `'static`.
2. Build the partial seed `AssistantMessage` mirroring `proxy.ts:121-137` (empty `content`, zeroed `Usage`, current `timestamp_ms`, `stop_reason: Stop`).
3. Inside `stream! { … }`:
   - `let client = reqwest::Client::new();` (or share a `OnceCell` if T6 reveals reuse pressure).
   - `let body = ProxyRequest { model: &model, context: &context, options: build_request_options(&options.options) };`
   - `let req = client.post(format!("{}/api/stream", options.proxy_url)).bearer_auth(&options.auth_token).json(&body);`
   - Race the HTTP send against the cancel token. On cancel before headers, yield `AssistantMessageEvent::Error { reason: Aborted, error: aborted_partial(&partial) }` and return.
   - If `!response.status().is_success()`, attempt `response.json::<ErrorBody>().await` (define `#[derive(Deserialize)] struct ErrorBody { error: Option<String> }`). Synthesize a stop-reason `Error` partial with `error_message = Some(format!("Proxy error: {} {}", status, body_msg))` and yield exactly one `AssistantMessageEvent::Error`, then return.
   - Otherwise, take `response.bytes_stream()` and run a line-buffer loop:
     - `let mut buffer = String::new();` `let mut tool_partial_json = HashMap::<u32, String>::new();`
     - For each `Result<Bytes, reqwest::Error>` chunk: race `chunk_fut` against `cancel.cancelled()`. On chunk: append `std::str::from_utf8(&bytes).unwrap_or("")` to `buffer`. Split on `'\n'`, keep the last segment in `buffer`, iterate the others: trim, ignore empty / `:`-prefixed comments, strip `data: ` prefix, `serde_json::from_str::<ProxyAssistantMessageEvent>(payload)`. Run `process_proxy_event(event, &mut partial, &mut tool_partial_json)`. On `Ok(Some(ev))`, `yield ev`. On `Err`, yield a synthesized `Error` and return.
     - On cancel mid-stream: drop the body stream (which cancels the HTTP read), yield `AssistantMessageEvent::Error { reason: Aborted, error: { partial.stop_reason = Aborted; partial.error_message = Some("Aborted".into()); partial.clone() } }`, return.
4. Wrap in `Box::pin` and return.

Files touched: `packages/agent/src/proxy.rs`, `packages/agent/src/error.rs` (add `Proxy { status: u16, message: String }` variant). ≤2 files.

Verification: `cargo build -p hand-agent`. The integration test in T7 exercises the runtime path.

### T6 — `stream_fn` injection on `AgentLoopConfig`

Currently `agent_loop.rs:404` hard-codes `client.stream_simple(&config.model, llm_context, Some(stream_opts))`. Add an alternative:

```rust
pub type StreamFn = Arc<
    dyn Fn(
            &model::Model,
            model::Context,
            model::SimpleStreamOptions,
            CancellationToken,
        ) -> AssistantMessageEventStream<'static>
        + Send
        + Sync,
>;
```

Add `pub stream_fn: Option<StreamFn>` to `AgentLoopConfig` (in `types.rs`) and `pub stream_fn: Option<StreamFn>` to `AgentOptions` (in `agent.rs`). In `stream_assistant_response`, prefer `stream_fn` if set, else fall back to `client.stream_simple`. In `Agent::build_config`, plumb the option through.

Provide a thin convenience adapter:

```rust
pub fn stream_fn_proxy(opts_template: ProxyStreamOptions) -> StreamFn {
    Arc::new(move |model, context, simple_opts, cancel| {
        let mut opts = opts_template.clone();
        opts.options = simple_opts;
        opts.cancel = Some(cancel);
        stream_proxy(model, context, opts)
    })
}
```

Files touched: `packages/agent/src/types.rs`, `packages/agent/src/agent.rs`, `packages/agent/src/agent_loop.rs`, `packages/agent/src/proxy.rs`. ≤4 files.

Verification: `cargo check -p hand-agent --tests`; existing tests still pass (`cargo test -p hand-agent`).

### T7 — Integration test against `wiremock`

Add `packages/agent/tests/proxy_test.rs`. Use `wiremock::MockServer` to host a `POST /api/stream` that responds with `Content-Type: text/event-stream` and a hand-written `data: …\n` body containing the canonical 8-event arc:

```
data: {"type":"start"}
data: {"type":"text_start","contentIndex":0}
data: {"type":"text_delta","contentIndex":0,"delta":"Hello"}
data: {"type":"text_delta","contentIndex":0,"delta":" world"}
data: {"type":"text_end","contentIndex":0}
data: {"type":"done","reason":"stop","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}
```

Tests:

- `proxy_emits_full_event_arc`: drive `stream_proxy` against the mock; collect the stream into `Vec<AssistantMessageEvent>`; assert exactly 6 events in order, the final `Done.message.content[0].text == "Hello world"`, and every `*_delta`/`*_end` event's `partial.content[0].text` reflects the running concatenation.
- `proxy_surfaces_http_error`: mock returns `401` with `{ "error": "bad token" }`; assert the stream yields exactly one `AssistantMessageEvent::Error` whose `error.error_message` contains `"bad token"`, then ends.
- `proxy_aborts_when_token_cancelled`: mock holds the response open via `wiremock::ResponseTemplate::set_delay(Duration::from_secs(5))` after the first event; cancel the token after 50 ms; assert the stream yields the first `Start`/`TextStart` events then exactly one `Error { reason: Aborted, … }` and ends within 200 ms wall-clock.

Files touched: `packages/agent/tests/proxy_test.rs` (new). ≤1 file.

Verification: `cargo test -p hand-agent --test proxy_test` — three tests pass.

### T8 — Public API and README

In `packages/agent/src/lib.rs`, add:

```rust
pub mod proxy;
pub use proxy::{stream_proxy, stream_fn_proxy, ProxyAssistantMessageEvent, ProxyStreamOptions};
```

Append a short "Proxy transport" section to `packages/agent/README.md` mirroring the TS doc-comment example at `proxy.ts:90-99`, but in Rust. Update `docs/exec-plans/agent-port-parity.md` Progress to mark M3.T2 done with a footnote pointing at this plan.

Files touched: `packages/agent/src/lib.rs`, `packages/agent/README.md`, `docs/exec-plans/agent-port-parity.md`. ≤3 files.

Verification: `cargo doc -p hand-agent --no-deps` succeeds with no broken intra-doc links; `cargo test -p hand-agent` passes; `cargo clippy -p hand-agent --tests -- -D warnings` is clean.

## Concrete Steps

```bash
# Working directory: /Users/wanggang/.touch-code/repos/hand-ai/feat-web-ui

# 1. Create a focused branch off the current one
git checkout -b feat-agent-proxy

# 2. Implement each task in order; after each task:
cargo check -p hand-agent
cargo test  -p hand-agent

# 3. After T7 the proxy test should appear in the list:
cargo test -p hand-agent --test proxy_test
# Expected:
#     running 3 tests
#     test proxy_aborts_when_token_cancelled ... ok
#     test proxy_emits_full_event_arc ... ok
#     test proxy_surfaces_http_error ... ok
#     test result: ok. 3 passed; 0 failed

# 4. Final gate
cargo fmt --all
cargo clippy -p hand-agent --tests -- -D warnings
cargo test  -p hand-agent

# 5. Commit per task (per global rule: atomic commits, /commit after each change)
```

## Validation and Acceptance

After all eight tasks complete, the following must hold:

1. `cargo test -p hand-agent` reports all pre-existing tests **still passing** plus the three new `proxy_test.rs` tests passing — total count strictly greater than baseline.
2. `cargo clippy -p hand-agent --tests -- -D warnings` exits 0.
3. `hand_agent::stream_proxy(&model, ctx, ProxyStreamOptions { auth_token, proxy_url, ..Default::default() })` compiles and returns `AssistantMessageEventStream<'static>` (verified by writing a single doc-test in `proxy.rs` that uses it without invoking it).
4. An `Agent` constructed with `AgentOptions { stream_fn: Some(stream_fn_proxy(opts)), ..Default::default() }` runs end-to-end against the same `wiremock` server in a final smoke test (optional, fold into T7 if time permits).
5. `docs/exec-plans/agent-port-parity.md` Progress section reflects that the proxy port has landed.

## Idempotence and Recovery

- Every task is a pure-additive code change (T1–T5, T7, T8) or a small additive field on existing structs (T6). Re-running any task is safe; the `Edit`/`Write` operations replace the same target ranges.
- `cargo check` / `cargo test` are idempotent and can be repeated freely.
- The `wiremock` server in T7 is created and torn down inside each `#[tokio::test]` — no shared state across tests.
- Partial completion is recoverable: T1–T4 leave the codebase compiling without the new public function being usable, which is fine. T5 introduces the public function; if T7 reveals a bug, fix in T5 and re-run.
- No git history rewrites, no migrations, no destructive operations.

## Artifacts and Notes

Reference excerpts from `pi-mono/packages/agent/src/proxy.ts` for line-level mapping:

- `proxy.ts:121-137` — partial seed initialisation (mirror in T5 step 2).
- `proxy.ts:152-165` — HTTP request shape (mirror in T5 step 3, second bullet).
- `proxy.ts:167-177` — error-body parsing (mirror in T5 step 3, third bullet).
- `proxy.ts:181-206` — line buffer loop (mirror in T5 step 3, fourth bullet).
- `proxy.ts:242-353` — reducer branches (mirror in T4).

Sample `wiremock` setup for T7:

```rust
let server = wiremock::MockServer::start().await;
wiremock::Mock::given(wiremock::matchers::method("POST"))
    .and(wiremock::matchers::path("/api/stream"))
    .respond_with(
        wiremock::ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(SSE_FIXTURE),
    )
    .mount(&server)
    .await;
```

## Interfaces and Dependencies

In `packages/agent/src/proxy.rs`, the following must exist when this plan is complete:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProxyAssistantMessageEvent { /* 13 variants per T2 */ }

#[derive(Debug, Clone, Default)]
pub struct ProxyStreamOptions {
    pub auth_token: String,
    pub proxy_url: String,
    pub cancel: Option<tokio_util::sync::CancellationToken>,
    pub options: model::SimpleStreamOptions,
}

pub fn stream_proxy(
    model: &model::Model,
    context: model::Context,
    options: ProxyStreamOptions,
) -> model::AssistantMessageEventStream<'static>;

pub fn stream_fn_proxy(template: ProxyStreamOptions) -> crate::types::StreamFn;
```

In `packages/agent/src/types.rs`:

```rust
pub type StreamFn = Arc<
    dyn Fn(
            &model::Model,
            model::Context,
            model::SimpleStreamOptions,
            tokio_util::sync::CancellationToken,
        ) -> model::AssistantMessageEventStream<'static>
        + Send
        + Sync,
>;

// On AgentLoopConfig:
pub stream_fn: Option<StreamFn>,
```

In `packages/agent/src/error.rs`:

```rust
#[error("Proxy error: HTTP {status}: {message}")]
Proxy { status: u16, message: String },
```

In `packages/agent/Cargo.toml`:

```toml
[dependencies]
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json", "stream"] }
bytes = "1"

[dev-dependencies]
wiremock = "0.6"
```

In `packages/agent/src/lib.rs` (re-exports):

```rust
pub mod proxy;
pub use proxy::{ProxyAssistantMessageEvent, ProxyStreamOptions, stream_fn_proxy, stream_proxy};
pub use types::StreamFn;
```

External dependencies:
- `reqwest` (HTTP client; rustls-tls + stream features).
- `bytes` (for `bytes::Bytes` from `bytes_stream()`).
- `tokio_util::sync::CancellationToken` — already an explicit dependency; cancellation primitive.
- `async_stream::stream!` — already an explicit dependency; the stream constructor.
- `futures::StreamExt` — already an explicit dependency; for `bytes_stream().next().await`.
- `serde_json` — already present; line decoding.
- `wiremock` (dev-only; mock proxy server in T7).
- Re-uses without new deps: `model::{Model, Context, SimpleStreamOptions, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, StopReason, Usage, ThinkingLevel, ThinkingBudgets, CacheRetention, Transport, ToolCall, AssistantContentBlock, TextContent, ThinkingContent, safe_parse_partial}`.
