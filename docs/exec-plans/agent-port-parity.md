# ExecPlan: Bring `hand-agent` to Parity with `pi-agent-core`

**Status:** Draft
**Author:** Gump (planned with Claude)
**Date:** 2026-05-06

This is a living document. The Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective sections must be kept up to date as work proceeds.

## Purpose

After this work, the `hand-agent` crate exposes the same runtime contract as `@mariozechner/pi-agent-core` does in TypeScript: a host application can construct an `Agent`, register tools, subscribe to a fully-featured event stream, run a prompt that may stream multi-turn responses with parallel tool calls, abort it mid-flight, drive it with steering / follow-up messages, and observe the resulting transcript — all without the runtime silently dropping events, losing cancellation, or executing tool calls in fake-parallel.

Concretely, by the end of this plan a Rust caller can:

1. `let token = CancellationToken::new(); agent.subscribe(...); agent.prompt("…").await;` — and the subscribed listener sees `agent_start → message_* → tool_execution_* → turn_end → agent_end` for every run, including failed runs.
2. Cancel a running prompt via `agent.abort()` (or by cancelling the token), and observe a single `agent_end` event with a synthesized aborted assistant message.
3. Register two tools, both `executionMode = "parallel"`, and verify with a stopwatch test that two `sleep(50ms)` tools complete in ~50ms wall-clock instead of ~100ms.
4. Return `{ terminate: true }` from every tool result in a batch and observe the loop exit after `turn_end` without another LLM call.
5. Provide a `before_tool_call` that throws → the loop emits a `tool_execution_end` with `is_error = true` and the next turn proceeds.
6. Provide `prepare_arguments` + a JSON schema mismatch → an error tool result is emitted without crashing.

The plan is implemented in three milestones aligned with the P0/P1/P2 priority bands. Each milestone is a candidate PR.

## Progress

- [ ] **Milestone 1 (P0): Cancellation, listener wiring, lifecycle error fallback, real parallelism, terminate / shouldStopAfterTurn**
  - [ ] M1.T1 Thread `CancellationToken` through agent loop and stream consumption
  - [ ] M1.T2 Wire listeners through a shared event sink (Arc<Mutex<Vec<Listener>>>)
  - [ ] M1.T3 Inject failure assistant message + final `agent_end` on lifecycle error
  - [ ] M1.T4 Replace fake-parallel with `futures::future::join_all` + per-tool execution-mode override
  - [ ] M1.T5 Add `should_stop_after_turn` hook and `terminate` early-stop semantics
- [ ] **Milestone 2 (P1): Hook completeness, schema validation, streaming tool updates, robust error capture**
  - [ ] M2.T1 Tool execute trait: `&CancellationToken` + `OnUpdate` callback, `Result` return
  - [ ] M2.T2 `prepare_arguments` + JSON Schema validation via `jsonschema` crate
  - [ ] M2.T3 `Agent::continue()` drains steering/follow-up when last message is assistant
  - [ ] M2.T4 `prompt(...)` overloads: text, single message, batch, with images
  - [ ] M2.T5 Wire `convert_to_llm`, `transform_context`, `get_api_key` from `Agent` into `AgentLoopConfig`; expose `streaming_message` / `pending_tool_calls` on state
- [ ] **Milestone 3 (P2): `AgentMessage` abstraction, `stream_proxy`, missing config fields**
  - [ ] M3.T1 Introduce `AgentMessage` enum with a `Custom(serde_json::Value)` variant; default convert filters it out
  - [x] M3.T2 Port `proxy.ts` → `hand_agent::stream_proxy` with `ProxyAssistantMessageEvent` and partial reconstruction (landed in `docs/exec-plans/agent-proxy-port.md`)
  - [ ] M3.T3 Add `thinking_budgets`, `transport`, `session_id`, `max_retry_delay_ms` to `Agent` / `AgentLoopConfig`

## Surprises & Discoveries

(None yet)

## Decision Log

- **2026-05-06**: Use `tokio_util::sync::CancellationToken` rather than inventing an `AbortSignal` shim. Rationale: it is the idiomatic Rust async-cancellation primitive, integrates cleanly with `tokio::select!`, and droppable streams cancel underlying I/O when the agent stops polling. We do not push cancellation into the `model` crate's `stream_simple` signature in this plan — instead, the agent loop wraps `stream.next()` in `select!` against the token. Adding signal support to the model crate is a follow-up.
- **2026-05-06**: Listener storage on `Agent` becomes `Arc<Mutex<Vec<Listener>>>` so the event sink closure can clone the Arc into the loop without re-borrowing `&mut self`. Sync `Mutex` is fine because emission is short and non-async.
- **2026-05-06**: Listener signature is `Fn(&AgentEvent, &CancellationToken)` (synchronous), not async. Rationale: the TS version awaits listeners, which complicates Rust ownership for little benefit. Listeners that need to `await` can spawn a tokio task. Revisit if a real use case requires back-pressure.
- **2026-05-06**: Tool `execute` returns `Result<ToolResult, BoxError>` rather than `ToolResult` directly. The loop wraps it into an error `ToolResult` so panics-via-Result match TS try/catch behavior. This is a breaking change to the existing `ToolExecuteFn` type signature — acceptable because the crate is pre-1.0 and not yet consumed externally.
- **2026-05-06**: Plan does not include a Rust port of `Agent.signal` accessor as a public field; instead, expose `agent.cancellation_token() -> &CancellationToken` so callers can clone and pass to nested tasks.

## Outcomes & Retrospective

(To be filled at milestone completion)

## Context and Orientation

Related documents:
- Conversion guidelines: `docs/conversion-guidelines.md` — defines TS→Rust idioms (联合类型→tagged enum, 回调→Fn trait or channel, errors via `thiserror`+`Result`, etc.)
- High-level conversion plan: `docs/conversion-plan.md` (sections "阶段 1：Agent crate" and "1.5 Proxy")
- Source of truth for behavior: `/Users/wanggang/dev/opensource/pi-mono/packages/agent/` (TypeScript reference implementation)

Key source files to read before starting any task:
- `crates/agent/src/types.rs` — current Rust types; needs additive changes (no full rewrite)
- `crates/agent/src/agent_loop.rs` — main loop; the largest delta lives here
- `crates/agent/src/agent.rs` — high-level wrapper; hooks need wiring, listeners need real plumbing
- `crates/agent/src/error.rs` — extend with `Cancelled`, `SchemaValidation` variants
- `crates/agent/src/lib.rs` — re-export surface; keep stable except where types are renamed
- `crates/agent/tests/common/mod.rs` — shared test helpers; already provides `test_model`, etc.
- `crates/model/src/types.rs` lines ~340-430 — `StreamOptions` / `SimpleStreamOptions`
- Mirror files in `pi-mono/packages/agent/src/` (TypeScript) — read alongside the Rust file when porting
- `pi-mono/packages/agent/test/agent-loop.test.ts` (1278 lines) — the canonical behavior contract; use it as a checklist when writing Rust tests

How the pieces fit together (one paragraph):

`Agent` (in `agent.rs`) owns the conversation transcript and wires user-facing hooks (`subscribe`, `prompt`, `continue`, `abort`, `steer`, `follow_up`) onto `run_agent_loop` / `run_agent_loop_continue` (in `agent_loop.rs`). The loop emits `AgentEvent` values to a sink callback; `Agent` constructs that sink so it both reduces internal state (e.g., updates `pending_tool_calls`) and forwards to subscribed listeners. The loop calls `model::Client::stream_simple` to stream an assistant response, then iterates over assistant tool-call blocks, invokes the registered `AgentTool::execute`, optionally honoring `before_tool_call` / `after_tool_call` hooks, and appends the resulting `ToolResultMessage` to the context before the next turn. Steering messages are drained between turns; follow-up messages are drained when the loop would otherwise exit. Cancellation must short-circuit at three points: between events, while waiting on the LLM stream, and between tool calls.

Terms used in this plan:
- **Lifecycle error fallback**: when `prompt()` itself errors out (panic in convert, network reset before any event), TS injects a synthesized assistant message with `stopReason = "error"` into the transcript and emits a final `agent_end` so listeners always see a clean close. Rust currently returns `Err` and emits nothing.
- **Real parallelism**: `executeToolCallsParallel` in TS uses `Promise.all` over async closures so multiple tool executions overlap. Rust currently `.await`s them one at a time inside a `for` loop — sequential. We replace this with `futures::future::join_all`.
- **Per-tool execution mode override**: an `AgentTool` may set `executionMode = "sequential"`. If any tool in a batch is sequential, the whole batch is run sequentially even when the global mode is parallel.
- **Terminate semantics**: a tool may set `terminate: true` on its `ToolResult`. The loop exits early *only* when every finalized tool result in the batch has `terminate = true`.
- **Steering vs follow-up**: steering is drained between turns while the agent is still expected to keep going; follow-up is drained at the boundary where the agent would otherwise stop. Each has a delivery mode (`all` or `one-at-a-time`).
- **Listener**: a `Fn(&AgentEvent, &CancellationToken) + Send + Sync` registered via `Agent::subscribe`.

## Plan of Work

The work is sliced vertically. Each milestone delivers a coherent capability that has its own integration test and is independently shippable.

### Milestone 1: Cancellation, listeners, error fallback, real parallelism, terminate

This milestone closes the P0 correctness gaps — the kind a downstream consumer would notice within an hour. After it lands, the Rust `Agent` is observably equivalent to the TS one in normal happy-path and abort scenarios, and tools actually run in parallel when configured to.

**M1.T1 — Thread `CancellationToken` through the loop.** Add a `tokio_util::sync::CancellationToken` field to `Agent`. `Agent::abort()` calls `cancel()` on it. `prompt`/`continue` create a child token per run, store it on `Agent`, and pass it as a parameter all the way down: `run_agent_loop(prompts, ctx, tools, cfg, client, emit, &cancel)` and onward into `stream_assistant_response`, `execute_tool_calls*`, hooks. Inside `stream_assistant_response`, wrap `stream.next()` in `tokio::select! { _ = cancel.cancelled() => ..., maybe = stream.next() => ... }` and on cancellation produce a synthesized `AssistantMessage` with `stop_reason = StopReason::Aborted`. Inside `execute_tool_calls*`, check `cancel.is_cancelled()` between calls and skip remaining preparations. Hooks receive `&CancellationToken` and are responsible for honoring it. Add new error variant `AgentError::Cancelled`. Files touched: `agent_loop.rs`, `agent.rs`, `types.rs`, `error.rs`. ~4 files.

**M1.T2 — Wire listeners.** Replace `listeners: Vec<Box<dyn Fn(AgentEvent) + Send + Sync>>` on `Agent` with `listeners: Arc<Mutex<Vec<Arc<dyn Fn(&AgentEvent, &CancellationToken) + Send + Sync>>>>`. `subscribe` returns a `SubscriptionHandle` whose `Drop` removes the listener. `build_event_sink` clones the `Arc<Mutex<...>>` and the cancellation token into a closure of type `AgentEventSink = Arc<dyn Fn(AgentEvent) + Send + Sync>`. The sink also holds an `Arc<Mutex<MutableState>>` (or similar) to update `state.streaming_message` / `state.pending_tool_calls` reactively — this requires moving those fields out of `&mut self` access and into shared state, since the sink is invoked from inside `&Agent` borrow. Defer the state-sharing detail to a small helper struct `AgentRuntimeState`. Files touched: `agent.rs`, `types.rs`, `agent_loop.rs` (sink type signature). ~3 files.

**M1.T3 — Lifecycle error fallback.** In `Agent::run_loop` and `Agent::continue_run`, wrap the loop call in a `match`. On `Err`, build a `Message::Assistant(AssistantMessage { stop_reason: aborted ? Aborted : Error, error_message: Some(err.to_string()), content: vec![], usage: Usage::default(), … })`, push it to the transcript, store `state.error`, and synchronously emit `AgentEvent::AgentEnd { messages: vec![failure_msg] }` through the sink before propagating / swallowing the error per API. Mirrors `handleRunFailure` in `pi-mono/packages/agent/src/agent.ts:463`. Files touched: `agent.rs`. ~1 file.

**M1.T4 — Real parallelism + per-tool override.** Rewrite `execute_tool_calls_parallel` to (a) run `prepare_tool_call` sequentially for all calls, (b) collect futures `async move { execute_prepared_tool_call → finalize_executed_tool_call → emit tool_execution_end }` into a `Vec<BoxFuture>`, (c) drive them with `futures::future::join_all`. Also implement the "any sequential tool downgrades the batch" rule: before dispatching, if any tool found in `tools` has `execution_mode == Some(Sequential)`, call `execute_tool_calls_sequential` instead. Add an `execution_mode: Option<ToolExecutionMode>` field to `AgentTool`. Mirrors `executeToolCalls` at `pi-mono/packages/agent/src/agent-loop.ts:350-365`. Files touched: `agent_loop.rs`, `types.rs`. ~2 files.

**M1.T5 — `should_stop_after_turn` + `terminate`.** Add `should_stop_after_turn: Option<ShouldStopAfterTurnFn>` to `AgentLoopConfig` where the function takes a borrow of an inline `ShouldStopAfterTurnContext { message, tool_results, context, new_messages }` and returns `BoxFuture<'_, bool>`. Add `terminate: Option<bool>` to `ToolResult` and `AfterToolCallResult`. After `turn_end`, if every `ToolResultMessage` in the batch carries `details.terminate == true` (or implement via a parallel `Vec<bool>` collected from `FinalizedToolCallOutcome`), break out of the inner loop without polling steering. After `turn_end`, also call `should_stop_after_turn` if set; on `true`, emit `agent_end` and return. Files touched: `agent_loop.rs`, `types.rs`. ~2 files.

Acceptance for Milestone 1: two new integration tests pass —
- `aborts_mid_stream`: spawn an agent, call `prompt(...)` in one task, call `abort()` from another after 10ms; assert exactly one `agent_end` is observed and the synthesized last message has `stop_reason == Aborted`.
- `parallel_tools_actually_overlap`: register two tools that each `tokio::time::sleep(Duration::from_millis(50))` and return; trigger an assistant message that calls both; assert wall-clock < 80ms.

### Milestone 2: Hook completeness, schema validation, streaming tool updates, robust error capture

This milestone closes the P1 gaps. After it lands, an `AgentTool` author can fully exercise the same surface a TS tool author has: streaming progress updates, schema-validated arguments, panic-safe execution, and prompt overloads.

**M2.T1 — Tool execute contract.** Change `ToolExecuteFn` to `Box<dyn for<'a> Fn(ToolExecuteCtx<'a>) -> BoxFuture<'a, Result<ToolResult, BoxError>> + Send + Sync>`, where `ToolExecuteCtx<'a> { tool_call_id: String, args: serde_json::Value, cancel: &'a CancellationToken, on_update: OnUpdate }` and `OnUpdate = Box<dyn Fn(ToolResult) + Send + Sync>`. The loop's `execute_prepared_tool_call` constructs `OnUpdate` to push `AgentEvent::ToolExecutionUpdate { partial_result }` via the sink; on `Err` from `execute`, build `ToolResult::error(err.to_string())` and `is_error = true`. Files touched: `types.rs`, `agent_loop.rs`, `tests/common/mod.rs` (helper). ~3 files.

**M2.T2 — `prepare_arguments` + JSON Schema validation.** Add `prepare_arguments: Option<Box<dyn Fn(serde_json::Value) -> serde_json::Value + Send + Sync>>` to `AgentTool`. In `prepare_tool_call`, after lookup, call `prepare_arguments` if present, then validate against `tool.parameters` using the `jsonschema` crate (`jsonschema = "0.18"` workspace dependency, compiled once per tool via `OnceCell`). On validation failure, return `ToolCallPreparation::Immediate { result: ToolResult::error(...), is_error: true }` with the validator's first error message. Add `AgentError::SchemaValidation { tool_name, message }`. Files touched: `agent/Cargo.toml`, `types.rs`, `agent_loop.rs`, `error.rs`. ~4 files.

**M2.T3 — `continue()` queue drain.** Port the logic at `pi-mono/packages/agent/src/agent.ts:326-353`: when `continue()` is called and the last message is `assistant`, first try to drain the steering queue; if non-empty, run as a prompt with `skip_initial_steering_poll = true`; else try follow-up; else error. Move the drain logic into `Agent` and pass `skip_initial_steering_poll` to `build_config` so the first `get_steering_messages` call returns empty. Files touched: `agent.rs`. ~1 file.

**M2.T4 — `prompt` overloads.** Replace the current `prompt(text)` / `prompt_with_messages(msgs)` split with a single `prompt<P: IntoPromptInput>(input: P)` plus a `prompt_text_with_images(text, images)` helper. Define `IntoPromptInput` with impls for `&str`, `String`, `Message`, `Vec<Message>`. Reset `state.is_streaming` consistently in all branches. Files touched: `agent.rs`, `types.rs` (for the trait). ~2 files.

**M2.T5 — Full hook wiring + state exposure.** In `Agent::build_config`, replace the hardcoded `None` for `convert_to_llm`, `transform_context`, `get_api_key` with values cloned from `self`. Add `Agent::set_convert_to_llm`. Add `state.streaming_message: Option<Message>` and `state.pending_tool_calls: HashSet<String>` (read-only accessors); `build_event_sink` (from M1.T2) now keeps these in sync on `MessageStart` / `MessageUpdate` / `MessageEnd` / `ToolExecutionStart` / `ToolExecutionEnd`. Files touched: `agent.rs`, `types.rs`. ~2 files.

Acceptance for Milestone 2: integration tests pass —
- `tool_update_events_arrive_in_order`: an `execute` that calls `on_update` three times produces three `ToolExecutionUpdate` events with ordered partial results, before the final `ToolExecutionEnd`.
- `schema_validation_rejects_bad_args`: a tool with `parameters` requiring `path: string` is invoked with `{ path: 42 }`; the loop emits an error tool result whose text contains "schema" or the validator's message; the loop continues to the next turn.
- `continue_drains_steering_when_assistant_last`: queue a steering message, then call `continue()` from a transcript whose last message is assistant; assert the steering message is the next user-role message in `agent.messages()`.

### Milestone 3: `AgentMessage` abstraction, `stream_proxy`, missing config fields

This milestone closes the P2 gaps. It is mostly additive and unblocks downstream crates (`hand-coding-agent`, future `hand-web-ui` analogue) from needing to fork the agent for custom message types or proxy transport.

**M3.T1 — `AgentMessage` enum.** Introduce
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum AgentMessage {
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
    Custom(CustomAgentMessage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomAgentMessage {
    pub kind: String,
    pub payload: serde_json::Value,
    pub timestamp: i64,
}
```
Replace `Vec<Message>` in `AgentContext`, `AgentState`, events, and hooks with `Vec<AgentMessage>`. The default `convert_to_llm` filters out `Custom`. Provide `From<Message> for AgentMessage` so call sites need minimal changes. Files touched: `types.rs`, `agent_loop.rs`, `agent.rs`, `tests/common/mod.rs`. ~4 files.

**M3.T2 — `stream_proxy` port.** New module `crates/model/src/proxy.rs` (decision: belongs in `model`, mirroring TS where `proxy.ts` lives in agent only because TS conflates layers — Rust splits more cleanly). Defines `ProxyAssistantMessageEvent` enum (tag `"type"`, snake_case), `ProxyStreamOptions`, and `pub fn stream_proxy(model: &Model, context: Context, options: ProxyStreamOptions) -> impl Stream<Item = AssistantMessageEvent>` using `reqwest` with SSE line buffering and `parse_streaming_json` from `model`. Re-export from `model::lib.rs`. Mirrors `pi-mono/packages/agent/src/proxy.ts:116-368`. Files touched: `crates/model/src/proxy.rs` (new), `crates/model/src/lib.rs`, `crates/model/Cargo.toml` (already has reqwest). ~3 files.

**M3.T3 — Missing config fields.** Add `thinking_budgets: Option<ThinkingBudgets>`, `transport: Option<Transport>`, `session_id: Option<String>`, `max_retry_delay_ms: Option<u64>` to `Agent`. Wire into `build_config` → `SimpleStreamOptions`. Most are pass-through. Files touched: `agent.rs`, `types.rs`. ~2 files.

Acceptance for Milestone 3: integration tests pass —
- `custom_message_round_trip`: push a `CustomAgentMessage`, run the loop, verify the default `convert_to_llm` filters it out of the LLM-bound vector but it still appears in `agent.messages()`.
- `stream_proxy_decodes_event_sequence`: feed a recorded SSE byte stream through `stream_proxy` (using `mockito` or an in-process `tokio::io::duplex`) and verify the event sequence matches a golden `Vec<AssistantMessageEvent>`.

## Concrete Steps

Run from repo root `/Users/wanggang/.touch-code/repos/hand-ai/feat-agent` unless stated.

Per-task workflow:

```bash
# 1. Branch off feat-agent (one branch per milestone)
git checkout -b feat-agent-m1

# 2. Implement task; build incrementally
cargo build -p hand-agent

# 3. Run the agent crate's tests
cargo test -p hand-agent

# Expected baseline (after this plan completes M1):
#     Running tests/agent_loop_test.rs
#         test cancels_mid_stream                          ... ok
#         test parallel_tools_actually_overlap             ... ok
#         test all_tools_terminate_breaks_loop             ... ok
#         test should_stop_after_turn_exits_after_turn_end ... ok
#         ... (existing tests still pass)
#     test result: ok. N passed; 0 failed

# 4. Format + clippy gate
cargo fmt --check
cargo clippy -p hand-agent -- -D warnings

# 5. Commit, push, PR
```

For Milestone 2 add `cargo test -p hand-agent --test agent_test schema_validation_rejects_bad_args`, etc. For Milestone 3 add `cargo test -p model proxy`.

## Validation and Acceptance

The plan is complete when:

1. `cargo test -p hand-agent` reports all tests passing, including the new tests listed under each milestone's acceptance criteria.
2. `cargo clippy -p hand-agent -- -D warnings` reports zero warnings.
3. The `examples/` directory contains at least one new file `examples/agent_abort.rs` that demonstrates `agent.abort()` cancelling a long-running tool, runnable with `cargo run --example agent_abort`. Output should show `tool_execution_start` followed by `agent_end` with no `tool_execution_end`.
4. A manual diff of `crates/agent/src/lib.rs` against `pi-mono/packages/agent/src/index.ts` shows every TS export has a Rust counterpart (or a documented intentional omission in `docs/exec-plans/agent-port-parity.md` Decision Log).
5. The test file `pi-mono/packages/agent/test/agent-loop.test.ts` has been read end-to-end and every test scenario it covers is either ported, intentionally skipped (with rationale in Decision Log), or replaced by an equivalent Rust test.

## Idempotence and Recovery

Every step is a code edit on tracked files; no external state is created. Failed builds can be reverted with `git restore`. Each milestone branches independently from `feat-agent`, so failure of M2 does not block M1 from merging.

If `cargo test` fails partway through a milestone, run `cargo test -p hand-agent --test <name> -- --nocapture` to surface event sequences. The agent loop is deterministic given a fake `Client`; flakes indicate a real bug, not test infrastructure.

If introducing the `AgentMessage` enum (M3.T1) breaks downstream crates inside this workspace, fix call sites in `crates/coding-agent` and `examples/` in the same PR rather than leaving the workspace in a broken state.

## Artifacts and Notes

The TS reference behavior contract for the loop is in `pi-mono/packages/agent/src/agent-loop.ts`. Particularly load-bearing sections:
- Lines 153-246: outer + inner loops, steering vs follow-up handoff
- Lines 350-365: "any sequential tool downgrades the batch" rule
- Lines 511-513: `shouldTerminateToolBatch` — early stop only when *every* result has `terminate=true`
- Lines 581-616: try/catch around `tool.execute` produces an error `ToolResult` rather than throwing

Sample expected event sequence for a single-turn run with one parallel tool batch (used as a fixture in tests):

```
agent_start
turn_start
message_start { user }
message_end { user }
message_start { assistant (partial) }
message_update × N { assistant }
message_end { assistant }
tool_execution_start { tool_call_id: "a" }
tool_execution_start { tool_call_id: "b" }
tool_execution_end { tool_call_id: ?, ... }   # completion order
tool_execution_end { tool_call_id: ?, ... }
message_start { toolResult, source order }
message_end { toolResult }
message_start { toolResult, source order }
message_end { toolResult }
turn_end { message: assistant, tool_results: [..., ...] }
agent_end { messages: [...] }
```

The order asymmetry (completion order for `tool_execution_end`, source order for the `message_*` pair around `toolResult`) is the contract from `pi-mono` and must be preserved.

## Interfaces and Dependencies

In `crates/agent/Cargo.toml` add:

```toml
tokio-util = { version = "0.7", features = ["rt"] }
jsonschema = { version = "0.18", default-features = false }
```

In `crates/agent/src/types.rs`, the post-plan public surface includes:

```rust
pub enum AgentMessage { User(UserMessage), Assistant(AssistantMessage), ToolResult(ToolResultMessage), Custom(CustomAgentMessage) }

pub struct CustomAgentMessage { pub kind: String, pub payload: serde_json::Value, pub timestamp: i64 }

pub struct AgentTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub label: String,
    pub execution_mode: Option<ToolExecutionMode>,
    pub prepare_arguments: Option<Box<dyn Fn(serde_json::Value) -> serde_json::Value + Send + Sync>>,
    pub execute: ToolExecuteFn,
}

pub struct ToolResult {
    pub content: Vec<ToolResultContent>,
    pub details: Option<serde_json::Value>,
    pub terminate: Option<bool>,
}

pub type ToolExecuteFn = Box<
    dyn for<'a> Fn(ToolExecuteCtx<'a>) -> BoxFuture<'a, Result<ToolResult, BoxError>>
        + Send
        + Sync,
>;

pub struct ToolExecuteCtx<'a> {
    pub tool_call_id: String,
    pub args: serde_json::Value,
    pub cancel: &'a tokio_util::sync::CancellationToken,
    pub on_update: OnUpdate,
}

pub type OnUpdate = std::sync::Arc<dyn Fn(ToolResult) + Send + Sync>;

pub struct ShouldStopAfterTurnContext<'a> {
    pub message: &'a AssistantMessage,
    pub tool_results: &'a [ToolResultMessage],
    pub context: &'a AgentContext,
    pub new_messages: &'a [AgentMessage],
}

pub type ShouldStopAfterTurnFn =
    Box<dyn for<'a> Fn(ShouldStopAfterTurnContext<'a>) -> BoxFuture<'a, bool> + Send + Sync>;
```

In `crates/agent/src/agent_loop.rs`, the post-plan exported entry points:

```rust
pub type AgentEventSink = std::sync::Arc<dyn Fn(AgentEvent) + Send + Sync>;

pub async fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    context: &mut AgentContext,
    tools: &[AgentTool],
    config: &AgentLoopConfig,
    client: &model::Client,
    emit: &AgentEventSink,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<AgentLoopResult, AgentError>;

pub async fn run_agent_loop_continue(
    context: &mut AgentContext,
    tools: &[AgentTool],
    config: &AgentLoopConfig,
    client: &model::Client,
    emit: &AgentEventSink,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<AgentLoopResult, AgentError>;
```

In `crates/agent/src/agent.rs`, `Agent` gains:

```rust
impl Agent {
    pub fn subscribe<F>(&self, listener: F) -> SubscriptionHandle
    where
        F: Fn(&AgentEvent, &CancellationToken) + Send + Sync + 'static;

    pub fn abort(&self);
    pub fn cancellation_token(&self) -> CancellationToken;

    pub async fn prompt<P: IntoPromptInput>(&mut self, input: P) -> Result<AgentLoopResult, AgentError>;
    pub async fn r#continue(&mut self) -> Result<AgentLoopResult, AgentError>;

    pub fn set_should_stop_after_turn(&mut self, hook: Option<ShouldStopAfterTurnFn>);
    pub fn set_convert_to_llm(&mut self, f: Option<ConvertToLlmFn>);
    pub fn set_transform_context(&mut self, f: Option<TransformContextFn>);
    pub fn set_get_api_key(&mut self, f: Option<GetApiKeyFn>);
}

pub struct SubscriptionHandle { /* drops to unsubscribe */ }
```

In `crates/model/src/proxy.rs` (new in M3.T2):

```rust
pub fn stream_proxy(
    model: &Model,
    context: Context,
    options: ProxyStreamOptions,
) -> AssistantMessageEventStream<'static>;

pub struct ProxyStreamOptions {
    pub auth_token: String,
    pub proxy_url: String,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub reasoning: Option<ThinkingLevel>,
    pub session_id: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub max_retry_delay_ms: Option<u64>,
    pub cancel: Option<CancellationToken>,
}
```
