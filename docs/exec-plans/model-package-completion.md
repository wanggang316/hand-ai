# ExecPlan: Model Package Completion (pi-mono → hand-ai)

**Status:** Draft
**Author:** Claude (Opus 4.7)
**Date:** 2026-05-06

This is a living document. The Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective sections must be kept up to date as work proceeds.

## Purpose

After this work, the Rust `model` crate at `packages/model/` reaches feature parity with `pi-mono/packages/ai` (TypeScript). The user can:

1. Drive `Anthropic`, `OpenAI Codex`, and `GitHub Copilot` providers using OAuth credentials (no manual API key required) by calling `OAuthProvider::login()` and persisting tokens to `~/.hand-ai/oauth.json`.
2. Send requests to six additional provider/API combinations not previously supported: `openai-codex-responses` (with WebSocket and cached transports), `azure-openai-responses`, `google-vertex` (ADC-authenticated), `mistral` (`mistral-conversations` API), `cloudflare-workers-ai`, and an in-process `faux` provider for tests.
3. Pass advanced runtime options — `transport` (sse/websocket/auto), `cache_retention` (none/short/long), `signal` (cancellation), `timeout_ms`, `max_retries`, `metadata`, `on_payload`/`on_response` callbacks — and observe `response_model`, `response_id`, and structured `diagnostics` on every `AssistantMessage`.
4. Cross-route a single conversation across providers (e.g. start on Anthropic, continue on OpenAI Responses) without manually rewriting tool-call IDs, thinking blocks, or thought signatures, because `transform_messages` covers all permutations.
5. Configure provider behavior with the full `Compat` matrix (Anthropic eager-tool streaming, long cache retention, OpenRouter routing parameters, Vercel Gateway routing, Z.ai tool streaming, Qwen thinking format, etc.) without code changes.

The proof is `cargo test -p model` passing a parity suite that mirrors the 60+ TypeScript tests, and `cargo run -p model --bin model-cli -- chat` working with each new provider when the corresponding credential is present.

## Progress

- [ ] **Milestone 1** — Type-system extensions (StreamOptions, AssistantMessage, ThinkingContent, Compat, Provider/Api enums, Model.thinking_level_map)
- [ ] **Milestone 2** — Utilities foundation (event-stream, diagnostics, json-parse, sanitize-unicode, validation, headers, hash, session-resources)
- [ ] **Milestone 3** — Cross-provider transform refresh (image-tool-result routing, eager-tool-input compat, Gemini-3 unsigned tool-call handling, response-id normalization)
- [ ] **Milestone 4** — OAuth subsystem (`pkce`, `types`, `index`, `anthropic`, `openai-codex`, `github-copilot`, `oauth-page`)
- [ ] **Milestone 5** — Faux provider + parity test harness (port stream/abort/validation/empty/unicode-surrogate tests)
- [ ] **Milestone 6** — `mistral-conversations` provider + tool-id normalization tests
- [ ] **Milestone 7** — `azure-openai-responses` provider (extends `openai-responses` with Azure base-url/auth)
- [ ] **Milestone 8** — `google-vertex` provider (extends `google-generative-ai` with ADC + Vertex base-url)
- [ ] **Milestone 9** — `openai-codex-responses` provider (SSE + WebSocket + cached transports, OAuth-driven)
- [ ] **Milestone 10** — `cloudflare-workers-ai` and `cloudflare-ai-gateway` (thin OpenAI-completions overlays)
- [ ] **Milestone 11** — `register_builtins()` + Compat auto-detection from base_url
- [ ] **Milestone 12** — Stream wrapper (`stream_simple`, `complete_simple`) with cancellation/retry/timeout
- [ ] **Milestone 13** — CLI surface parity (oauth login subcommands, provider selection by transport)
- [ ] **Milestone 14** — Documentation refresh (README, CLI.md, per-provider notes)

Each milestone is independently shippable (compiles and passes its own tests). Stop points may split a milestone into "done" and "remaining" sub-bullets when work is interrupted.

## Surprises & Discoveries

(None yet — append findings as work proceeds, with concise `cargo test` output as evidence.)

## Decision Log

### D-01: New types are additive, not breaking
All `StreamOptions` / `AssistantMessage` / `Compat` / `Model` extensions add `Option<T>` fields with `#[serde(skip_serializing_if = "Option::is_none")]`. Existing JSON payloads from `models.json` and external callers continue to deserialize. Why: avoids a coordinated change with `agent`, `coding-agent`, and `tui` crates that already depend on these types.

### D-02: OAuth tokens persist to `~/.hand-ai/oauth.json` (not `~/.pi/oauth.json`)
The `pi-mono` TS client uses `~/.pi/auth.json`. We use `~/.hand-ai/oauth.json` to avoid stomping on the Node CLI's credentials and to follow the `hand-` prefix convention from `docs/conversion-plan.md`. The file format matches pi-mono's so users can hand-migrate by copying.

### D-03: WebSocket transport uses `tokio-tungstenite`
For `openai-codex-responses`, the TS code uses `ws` and a custom websocket-cached probe. Rust gets `tokio-tungstenite` (already widely used, no native TLS bundling required, integrates with `tokio`). The `websocket-cached` transport probes by sending a tiny request and timing the first frame; if cache hit time is below threshold, subsequent requests reuse the connection.

### D-04: Faux provider is gated behind `#[cfg(test)]` plus a `faux` feature
The TS `faux.ts` is part of the public API (used by downstream test suites). We expose it the same way: under `cfg(feature = "faux")` so production builds don't carry the mock helpers but `cargo test` and downstream `dev-dependencies` can opt in.

**Update (M5):** The `faux` feature is no longer in `default`. Verification commands explicitly enable it with `--features faux`. Production builds without the feature do not compile the mock helpers.

### D-05: Compat auto-detection happens at provider entry, not in `Model`
TS detects compat from `baseUrl` lazily inside each provider. We follow the same pattern: each provider has a `resolve_compat(model: &Model) -> ResolvedCompat` helper that consults `model.compat` overrides, then falls back to URL-based detection. Keeping detection inside the provider avoids polluting `types.rs` with provider-specific URL strings.

### D-06: `AssistantMessageEventStream` becomes a concrete struct, not just a type alias
Currently `pub type AssistantMessageEventStream<'a> = Pin<Box<dyn Stream<Item = ...>>>`. We add a wrapper struct in `utils/event_stream.rs` that owns a `tokio::sync::mpsc::Receiver<AssistantMessageEvent>` plus helper methods (`collect_to_message()`, `iter_text_deltas()`). This matches `AssistantMessageEventStream`'s methods in TS and unblocks the parity test suite.

### D-07: Gemini-3 cross-API tool calls drop thought_signature (no placeholder)

The original M3 spec bullet 3 ("synthesize a `thoughtSignature` placeholder when missing") was a misread of the pi-mono `google-shared.ts` reference. The TS source drops invalid/foreign signatures to undefined; pi-mono CHANGELOG entry 4032 documents that a previous `skip_thought_signature_validator` sentinel was removed because Vertex rejected it. The Rust implementation matches TS: cross-API replay to Google sets thought_signature to None; same-model replay preserves the original signature.

### D-08: OAuth loopback uses fixed ports (53692 Anthropic, 1455 Codex)

The original "pick port 0, OS-assigned" Mitigation in the Risks section is incorrect for production OAuth flows. Anthropic's and OpenAI Codex's OAuth client configurations whitelist exact `redirect_uri` strings including port. Using a dynamic port causes the IdP to reject the redirect. The Rust implementation uses the same fixed ports as the pi-mono TS reference. Trade-off: two concurrent logins against the same provider on the same host will collide on the listener bind; the second one returns a clear "address in use" error. This is acceptable because OAuth flows are user-driven and rare.

(Append further decisions as the plan executes.)

## Outcomes & Retrospective

(To be filled after each milestone and at completion. Compare against the five user-visible outcomes listed in Purpose.)

## Context and Orientation

### Related documents

- Conversion guidelines: `docs/conversion-guidelines.md`
- Conversion plan (high-level): `docs/conversion-plan.md`
- TS source of truth: `/Users/wanggang/dev/opensource/pi-mono/packages/ai/src/`
- TS test suite (parity reference): `/Users/wanggang/dev/opensource/pi-mono/packages/ai/test/`

### Current Rust crate layout (`packages/model/src/`)

- `lib.rs` — public API surface; re-exports types/clients/providers
- `types.rs` — core types: `Api`, `Provider`, `Model`, `Message`, `StreamOptions`, `Compat`, content blocks, events
- `models.rs` + `models.json` + `generate_models.rs` — model catalog and code-gen
- `api_registry.rs` — `ApiProvider` trait + `ApiProviderRegistry`
- `client.rs` — `Client` (high-level entry: `stream`, `complete`)
- `transform.rs` — cross-provider message normalization (`transform_messages`, `normalize_tool_call_id_for_anthropic`)
- `overflow.rs` — context-overflow detection
- `env_api_keys.rs` — env-var lookup, Vertex ADC cache
- `cli.rs` + `bin/model_cli.rs` — CLI surface
- `providers/` — `anthropic_messages.rs`, `bedrock.rs`, `google_generative_ai.rs`, `openai_completions.rs`, `openai_responses.rs`, `mod.rs`

### TS source map (what we need to mirror)

- `src/types.ts` (464 LOC) — superset of our `types.rs`; missing: `Transport`, `CacheRetention`, `ProviderResponse`, `ModelThinkingLevel`, `AnthropicMessagesCompat`, expanded Compat fields, `responseModel`/`responseId`/`diagnostics` on assistant messages, `redacted` on thinking, `thinkingLevelMap` on `Model`
- `src/stream.ts` (59 LOC) — high-level `streamSimple` wrapper
- `src/session-resources.ts` (24 LOC) — session-scoped resource pool (used by codex websocket-cached)
- `src/utils/event-stream.ts` (87 LOC) — `AssistantMessageEventStream` standard impl
- `src/utils/diagnostics.ts` (45 LOC) — `AssistantMessageDiagnostic` (redacted error + recovery records)
- `src/utils/json-parse.ts` (124 LOC) — partial-JSON safe parse, used in OpenAI responses partial-json cleanup
- `src/utils/validation.ts` (324 LOC) — pre-flight context validation (orphan tool calls, empty content, etc.)
- `src/utils/sanitize-unicode.ts` (25 LOC) — strips unpaired surrogates
- `src/utils/headers.ts`, `src/utils/hash.ts` — small utilities
- `src/utils/oauth/` — `anthropic.ts` (402), `github-copilot.ts` (396), `openai-codex.ts` (458), `pkce.ts` (34), `oauth-page.ts` (109), `index.ts` (152), `types.ts` (71)
- `src/providers/transform-messages.ts` (220) — extra cross-provider transforms (image-tool-result routing, eager-input fixes, Gemini-3 unsigned tool calls)
- `src/providers/register-builtins.ts` (403) — one-call registration of all built-in providers
- `src/providers/simple-options.ts` (50) — adapter from `SimpleStreamOptions` to provider-specific options
- `src/providers/google-shared.ts` (350) + `src/providers/openai-responses-shared.ts` (551) — extracted helpers reused by sibling providers
- `src/providers/azure-openai-responses.ts` (281) — Azure variant; reuses openai-responses-shared
- `src/providers/cloudflare.ts` (35) — Cloudflare overlays (Workers AI + AI Gateway)
- `src/providers/faux.ts` (499) — programmable mock provider
- `src/providers/google-vertex.ts` (568) — Vertex variant; reuses google-shared
- `src/providers/mistral.ts` (634) — `mistral-conversations` API
- `src/providers/openai-codex-responses.ts` (1323) — Codex (SSE + WebSocket + cached transports), uses OAuth
- `src/providers/github-copilot-headers.ts` (37) — Copilot header injection helpers

### Glossary

- **Api** — a wire-protocol identifier (`openai-completions`, `anthropic-messages`, `google-generative-ai`, ...). Multiple providers can speak the same Api.
- **Provider** — a hosting brand identifier (`openai`, `anthropic`, `google-vertex`, `openrouter`, ...). One provider can host models on multiple Apis.
- **Compat** — per-API tunables that adjust request/response shape for non-canonical hosts (e.g. OpenRouter routing, Z.ai tool streaming, Qwen thinking format).
- **OAuth provider** — a credential-acquisition flow (`anthropic-claude`, `openai-codex`, `github-copilot`). Independent of the API/Provider used at request time.
- **Transport** — how the client transmits the request: `sse` (default), `websocket`, `websocket-cached`, or `auto`. Only some Apis (Codex Responses) support non-SSE transports.
- **Cache retention** — prompt-cache lifetime hint: `none`, `short` (5 min on Anthropic), `long` (1h Anthropic / 24h OpenAI). Mapped per-provider.
- **Diagnostic** — a redacted record of a recovery or failure (e.g. "retried after 503", "received unsigned tool call from Gemini-3"); attached to `AssistantMessage.diagnostics`.

## Plan of Work

The work is sliced as 14 milestones. Each milestone leaves the crate compiling and the test suite green. Vertical slicing applies: e.g. Milestone 5 (faux + parity harness) lands before any new provider so subsequent provider milestones can write tests immediately.

### Milestone 1: Type-system extensions

In `packages/model/src/types.rs`, add these structures and enum variants:

- New `Transport` enum (`Sse`, `Websocket`, `WebsocketCached`, `Auto`); serialized kebab-case.
- New `CacheRetention` enum (`None`, `Short`, `Long`); serialized lowercase.
- New `ProviderResponse { status: u16, headers: HashMap<String, String> }`.
- Extend `StreamOptions` with: `transport: Option<Transport>`, `cache_retention: Option<CacheRetention>`, `metadata: Option<HashMap<String, serde_json::Value>>`, `timeout_ms: Option<u64>`, `max_retries: Option<u32>`, `signal: Option<tokio_util::sync::CancellationToken>` (skip serialize), `on_payload: Option<Arc<dyn Fn(...) -> ... + Send + Sync>>` (skip serialize), `on_response: Option<Arc<dyn Fn(...) -> ... + Send + Sync>>` (skip serialize).
- Extend `AssistantMessage` with: `response_model: Option<String>`, `response_id: Option<String>`, `diagnostics: Option<Vec<AssistantMessageDiagnostic>>` (forward-declares Milestone 2 type — keep field gated `#[serde(skip_serializing_if = "Option::is_none")]`).
- Extend `ThinkingContent` with `redacted: Option<bool>`.
- Extend `Model` with `thinking_level_map: Option<ThinkingLevelMap>`. Add `ThinkingLevelMap` (alias `HashMap<String, Option<String>>`).
- Add `AnthropicMessagesCompat { supports_eager_tool_input_streaming: Option<bool>, supports_long_cache_retention: Option<bool> }` and a new `Compat::AnthropicMessages(AnthropicMessagesCompat)` variant.
- Extend `OpenAICompletionsCompat` with: `supports_strict_mode`, `cache_control_format` (`Option<String>` carrying `"anthropic"`), `send_session_affinity_headers`, `supports_long_cache_retention`, `zai_tool_stream`, expand `thinking_format` allowed values to include `"qwen"`, `"qwen-chat-template"`, `"zai"`, `"deepseek"`.
- Extend `OpenAIResponsesCompat` with `send_session_id_header: Option<bool>`, `supports_long_cache_retention: Option<bool>`.
- Extend `OpenRouterRouting` with the full set of fields documented in `pi-mono/packages/ai/src/types.ts:351-419` (preserve names; convert TS unions to Rust `serde(untagged)` enums where unavoidable).
- Add `Provider` enum variants: `CloudflareWorkersAi`, `CloudflareAiGateway`, `Fireworks`, `Moonshotai`, `MoonshotaiCn`, `Xiaomi`, `XiaomiTokenPlanCn`, `XiaomiTokenPlanAms`, `XiaomiTokenPlanSgp`, `OpencodeGo`, `Deepseek` (also missing).
- Add `Api::MistralConversations` variant.

After this, the crate still compiles and old tests pass; new fields are dead weight until later milestones populate them.

Verification: `cargo test -p model` (existing 3 integration files) still green; `cargo run -p model --bin model-cli -- list-providers` shows the new providers as known but with no registered Api handlers yet.

### Milestone 2: Utilities foundation

Create `packages/model/src/utils/` with:

- `mod.rs`
- `event_stream.rs` — wrapper struct `EventStream` containing the existing `Pin<Box<dyn Stream>>`, plus helpers: `collect_to_message() -> Result<AssistantMessage, AssistantMessage>` (the Err carries the error stop), `text_deltas()`, `tool_calls()`. Mirrors `AssistantMessageEventStream` in `pi-mono/packages/ai/src/utils/event-stream.ts`.
- `diagnostics.rs` — `AssistantMessageDiagnostic { kind: DiagnosticKind, message: String, details: Option<serde_json::Value>, timestamp_ms: u64 }`. Mirrors `pi-mono/packages/ai/src/utils/diagnostics.ts`.
- `json_parse.rs` — `safe_parse_partial(s: &str) -> Option<serde_json::Value>` with the same heuristics as TS (drops trailing comma, balances brackets); plus `try_parse_strict`.
- `sanitize_unicode.rs` — `sanitize(s: &str) -> Cow<'_, str>` strips lone surrogates by replacing with U+FFFD.
- `validation.rs` — port the 324-LOC TS `validation.ts`: `validate_context(ctx: &Context) -> Vec<ValidationIssue>` with checks for orphan tool calls, empty content, malformed image data URIs, etc.
- `headers.rs` — `merge_headers(default: &HashMap, override: Option<&HashMap>) -> HashMap`; case-insensitive de-dup.
- `hash.rs` — `sha256_hex(bytes: &[u8]) -> String` (reuses `sha2`).
- Move existing `overflow.rs` into `utils/overflow.rs` and re-export from `lib.rs` for back-compat.

Add `pub mod utils;` to `lib.rs` and re-export the public types.

Verification: a new `tests/utils_test.rs` covers each module with at least the cases the TS test file `test/validation.test.ts` exercises.

### Milestone 3: Cross-provider transform refresh

Replace the existing `transform.rs` with a port of `pi-mono/packages/ai/src/providers/transform-messages.ts:1-220`. Add the following transforms (each tested):

- Image-bearing tool-result routing: when target is a non-image-capable text-only model, downgrade image content to a "[image omitted]" text block plus diagnostic.
- Eager-tool-input → final-tool-input compat: when target Anthropic provider lacks `supportsEagerToolInputStreaming`, drop streamed-tool blocks.
- Gemini-3 unsigned tool-call handling: synthesize a `thoughtSignature` placeholder when missing.
- Response-id normalization: when crossing OpenAI Responses ↔ Anthropic, drop `response_id` to avoid foreign-id rejection.
- Existing `transform_messages` and `normalize_tool_call_id_for_anthropic` retain their public signatures but route through the new pipeline.

Verification: parity tests ported from `test/transform-messages-copilot-openai-to-anthropic.test.ts`, `test/google-shared-image-tool-result-routing.test.ts`, `test/google-shared-gemini3-unsigned-tool-call.test.ts`, `test/anthropic-eager-tool-input-compat.test.ts`, `test/tool-call-id-normalization.test.ts`, `test/cross-provider-handoff.test.ts`.

### Milestone 4: OAuth subsystem

Create `packages/model/src/oauth/` mirroring `pi-mono/packages/ai/src/utils/oauth/`:

- `mod.rs`, `types.rs` — `OAuthCredentials`, `OAuthAuthInfo`, `OAuthProvider` trait (with `login()`, `refresh()`, `revoke()`), `OAuthProviderId`, prompt enums.
- `pkce.rs` — `generate_pkce_pair() -> (verifier, challenge)`; verifier ≥ 43 chars, challenge = base64-url(sha256(verifier)).
- `anthropic.rs` — Claude.ai OAuth (loopback redirect + PKCE).
- `openai_codex.rs` — Codex OAuth (PKCE + token exchange).
- `github_copilot.rs` — GitHub Device Flow.
- `oauth_page.rs` — embedded HTML success page served on the loopback.
- `index.rs` (re-exported as `oauth::registry`) — registry and provider lookup.

Persistence path: `dirs::home_dir()?.join(".hand-ai").join("oauth.json")`. Load lazily; write atomically with file lock (use `fs::rename` temp pattern).

Add new `Cargo.toml` deps: `tokio-util` (`sync` feature for `CancellationToken`), `tiny_http` or `hyper` for loopback (prefer `hyper` since `reqwest` already pulls it in), `base64`, `urlencoding`, `rand` for state nonce.

Verification: ported tests `test/anthropic-oauth.test.ts`, `test/openai-codex-oauth.test.ts`, `test/github-copilot-oauth.test.ts`. Integration test that mocks the auth server is acceptable; full live login is a manual-only test documented in `Concrete Steps` below.

### Milestone 5: Faux provider + parity test harness

Add `packages/model/src/providers/faux.rs` mirroring `pi-mono/packages/ai/src/providers/faux.ts`. The faux provider:

- Implements `ApiProvider` for an arbitrary `Api` (registered against `Api::Faux` — add this enum variant).
- Accepts a `Vec<FauxScript>` describing the events to emit (text deltas, tool calls, errors, abort, redacted thinking, etc.).
- Drives an `EventStream` from a `tokio::sync::mpsc` channel.

Gated behind `#[cfg(any(test, feature = "faux"))]`; export from `lib.rs` only under the feature.

Port these TS test files to `tests/parity/`:

- `test/empty.test.ts` → `parity_empty.rs`
- `test/abort.test.ts` → `parity_abort.rs`
- `test/stream.test.ts` → `parity_stream.rs`
- `test/total-tokens.test.ts` → `parity_tokens.rs`
- `test/tool-call-without-result.test.ts` → `parity_orphan_tool.rs`
- `test/unicode-surrogate.test.ts` → `parity_unicode.rs`
- `test/validation.test.ts` → `parity_validation.rs`
- `test/faux-provider.test.ts` → `parity_faux.rs`

Verification: `cargo test -p model --features faux` reports the ported test count green.

### Milestone 6: `mistral-conversations` provider

Add `packages/model/src/providers/mistral.rs` (target ~600 LOC). Mirrors `pi-mono/packages/ai/src/providers/mistral.ts`. Key concerns:

- Uses Mistral's `/v1/agents/completions` conversations endpoint, not OpenAI Completions.
- Tool-id normalization: 9-char alphanumeric uppercase IDs (`normalize_mistral_tool_id` already exists in `providers/openai_completions.rs:13`; reuse it).
- Reasoning mode: maps `ThinkingLevel` to Mistral's `mode: "reasoning"` flag.

Register `Api::MistralConversations` against this provider. Update `models.json` source-of-truth comment to allow Mistral entries to use `api: "mistral-conversations"`.

Verification: parity ports of `test/mistral-tool-schema.test.ts`, `test/mistral-reasoning-mode.test.ts`.

### Milestone 7: `azure-openai-responses` provider

Add `packages/model/src/providers/azure_openai_responses.rs` (≤ 300 LOC). Mostly a thin wrapper:

- Constructs the Azure base URL from `model.base_url` (form: `https://{resource}.openai.azure.com/openai/v1/responses?api-version=...`).
- Adds `api-key` header instead of `Authorization: Bearer`.
- Delegates body construction and SSE parsing to a refactored `openai_responses_shared` module (extract from the existing `openai_responses.rs`).

Refactor existing `openai_responses.rs` to expose `pub(crate) mod shared` containing the body-builder and SSE-decoder so both `openai-responses` and `azure-openai-responses` use it.

Verification: ports of `test/azure-openai-base-url.test.ts`.

### Milestone 8: `google-vertex` provider

Add `packages/model/src/providers/google_vertex.rs` (≤ 600 LOC). Refactor `google_generative_ai.rs` to expose a shared `google_shared` submodule and reuse it.

- Auth: ADC via `gcloud auth application-default login` token cache OR explicit `api_key`. Reuses existing `env_api_keys::clear_vertex_adc_cache` and adds `vertex_access_token()` async helper.
- Base URL: `https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}/publishers/google/models/{model}:streamGenerateContent`.

Add `Api::GoogleVertex` provider registration.

Verification: port of `test/google-vertex-api-key-resolution.test.ts`.

### Milestone 9: `openai-codex-responses` provider

The largest single milestone (target ~1300 LOC). Mirrors `pi-mono/packages/ai/src/providers/openai-codex-responses.ts`. Three transports:

- **SSE** (default): same as `openai-responses` but pointed at `https://api.openai.com/v1/codex/responses` and authenticated via OAuth bearer.
- **WebSocket**: `wss://api.openai.com/v1/codex/responses?stream=ws`. Uses `tokio-tungstenite`. Frame protocol identical to SSE event names.
- **WebSocket-cached**: same endpoint but reuses an idle connection from a `SessionResources` pool (Milestone 2 dep — add session-resources port: `packages/model/src/session_resources.rs`). Pool keyed by `(session_id, transport)`.

OAuth integration: when `options.api_key` is `None`, look up `OAuthProvider::OpenAICodex.credentials()`. Surface a `ClientError::OAuthRequired` if missing.

Verification: ports of `test/openai-codex-oauth.test.ts`, `test/openai-codex-stream.test.ts`, `test/openai-codex-cache-affinity-e2e.test.ts`.

### Milestone 10: Cloudflare overlays

Add `packages/model/src/providers/cloudflare.rs` (≤ 50 LOC). Two thin overlays on `openai-completions`:

- `cloudflare-workers-ai`: base URL `https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1`. Compat: `supports_strict_mode = false`.
- `cloudflare-ai-gateway`: base URL `https://gateway.ai.cloudflare.com/v1/{account_id}/{gateway_id}/openai`.

Both register against `Api::OpenAICompletions` keyed by provider; the existing `OpenAICompletionsProvider` handles the wire protocol.

Verification: smoke test that constructs a `Model` for each and asserts the resolved URL.

### Milestone 11: `register_builtins()` + Compat auto-detect

Create `packages/model/src/providers/register_builtins.rs` exposing:

```rust
pub fn register_builtins(registry: &ApiProviderRegistry) {
    registry.register(Api::OpenAICompletions, Box::new(OpenAICompletionsProvider::new()), Some("builtin"));
    registry.register(Api::OpenAIResponses, Box::new(OpenAIResponsesProvider::new()), Some("builtin"));
    registry.register(Api::OpenAICodexResponses, Box::new(OpenAICodexResponsesProvider::new()), Some("builtin"));
    registry.register(Api::AzureOpenAiResponses, Box::new(AzureOpenAIResponsesProvider::new()), Some("builtin"));
    registry.register(Api::AnthropicMessages, Box::new(AnthropicMessagesProvider::new()), Some("builtin"));
    registry.register(Api::BedrockConverseStream, Box::new(BedrockProvider::new()), Some("builtin"));
    registry.register(Api::GoogleGenerativeAi, Box::new(GoogleGenerativeAiProvider::new()), Some("builtin"));
    registry.register(Api::GoogleVertex, Box::new(GoogleVertexProvider::new()), Some("builtin"));
    registry.register(Api::MistralConversations, Box::new(MistralProvider::new()), Some("builtin"));
}
```

Update `Client::default()` to call `register_builtins(&self.registry)` so users get full coverage without manual wiring.

Compat auto-detect: in each OpenAI-completions-derived provider, add `resolve_compat(model: &Model) -> ResolvedCompat` that consults `model.compat`, then falls back to URL substring matching (`openrouter.ai`, `gateway.ai.cloudflare.com`, `bigmodel.cn`, etc.). Mirrors TS auto-detect in `openai-completions.ts:resolveCompat`.

Verification: ports of `test/openai-completions-cache-control-format.test.ts`, `test/zen.test.ts`, `test/azure-openai-base-url.test.ts`.

### Milestone 12: Stream wrapper with cancellation/retry/timeout

Create `packages/model/src/stream.rs` with `stream_simple()` and `complete_simple()`:

- Resolve provider from registry.
- Apply `transform_messages` for cross-provider compat.
- Wrap call in `tokio::time::timeout(options.timeout_ms)` if set.
- Wire `options.signal` (`CancellationToken`) into provider call so dropping the token aborts the in-flight stream.
- Implement retry with exponential backoff up to `max_retries` for retriable errors (HTTP 429/503/connection reset). Append a diagnostic per retry.
- Cap retry sleep at `max_retry_delay_ms`.

Verification: port of `test/abort.test.ts`, `test/responseid.test.ts`, `test/total-tokens.test.ts`.

### Milestone 13: CLI surface parity

Update `packages/model/src/cli.rs` and `bin/model_cli.rs`:

- Add `oauth` subcommand: `oauth login <provider>`, `oauth status`, `oauth logout <provider>`. Calls the matching `OAuthProvider` from Milestone 4.
- Add `--transport` flag to `chat` (sse/websocket/auto).
- Add `--cache-retention` flag.
- `list-providers` shows OAuth status for credential-driven providers.

Verification: manual smoke (documented in `Concrete Steps`).

### Milestone 14: Documentation refresh

Update:

- `packages/model/README.md` — provider matrix, OAuth flow, transport options, link to this plan.
- `packages/model/CLI.md` — full subcommand reference.
- `docs/conversion-plan.md` — mark "阶段 0" items complete; reference this exec-plan.

No code changes; verification is a manual reading pass.

## Concrete Steps

All commands assume `cd /Users/wanggang/dev/00/hand-ai`.

### Baseline (run before any milestone)

```bash
cargo build -p model
cargo test -p model
```

Expected: clean build; 3 existing test files (`integration_test.rs`, `client_test.rs`, `integration_tests.rs`) all pass.

### Per-milestone gate

After each milestone, run:

```bash
cargo build -p model --features faux
cargo test -p model --features faux
cargo clippy -p model --all-targets --features faux -- -D warnings
cargo fmt -p model -- --check
```

The `faux` feature is required because integration tests under
`packages/model/tests/parity_*.rs` import `model::FauxProvider` and friends,
which are gated behind `cfg(any(test, feature = "faux"))`. The lib's own
`cfg(test)` does not extend to integration test crates, so the feature must
be opted in explicitly.

Expected output (tail):

    test result: ok. <N> passed; 0 failed; ...

If any gate fails, fix in place — never proceed to the next milestone with broken state. Mark the milestone partially-complete in Progress.

### Milestone 4 manual verification (OAuth live login, optional)

```bash
cargo run -p model --bin model-cli -- oauth login anthropic
# follow browser flow; loopback callback writes to ~/.hand-ai/oauth.json
cargo run -p model --bin model-cli -- oauth status
# expected: "anthropic: authenticated, expires <ISO-8601>"
```

### Milestone 9 manual verification (Codex websocket)

```bash
cargo run -p model --bin model-cli -- chat --provider openai-codex --model gpt-4-codex --transport websocket -- "Hello"
```

Expected: streamed text deltas; `responseId` printed in trailing usage line.

### Final acceptance

```bash
cargo test -p model --features faux --release
cargo run -p model --bin model-cli -- list-providers
```

Expected `list-providers` output includes (sorted): `amazon-bedrock`, `anthropic`, `azure-openai-responses`, `cerebras`, `cloudflare-ai-gateway`, `cloudflare-workers-ai`, `deepseek`, `fireworks`, `github-copilot`, `google`, `google-antigravity`, `google-gemini-cli`, `google-vertex`, `groq`, `huggingface`, `kimi-coding`, `minimax`, `minimax-cn`, `mistral`, `moonshotai`, `moonshotai-cn`, `opencode`, `opencode-go`, `openai`, `openai-codex`, `openrouter`, `vercel-ai-gateway`, `xai`, `xiaomi`, `xiaomi-token-plan-ams`, `xiaomi-token-plan-cn`, `xiaomi-token-plan-sgp`, `zai`.

## Validation and Acceptance

Acceptance is anchored to the five user-visible outcomes in **Purpose**. Each is a runnable check.

1. **OAuth login works.** `cargo run -p model --bin model-cli -- oauth login anthropic` followed by `oauth status` reports `authenticated` for `anthropic`. The on-disk file `~/.hand-ai/oauth.json` contains a non-empty `access_token`.

2. **All six new providers are reachable.** `cargo run -p model --bin model-cli -- list-providers --json` returns a JSON array containing entries with `provider` matching all of: `azure-openai-responses`, `google-vertex`, `mistral`, `openai-codex`, `cloudflare-workers-ai`, plus a registered `faux` provider visible only when built with `--features faux`.

3. **Advanced runtime options take effect.** A test in `tests/integration_advanced_options.rs` (new) drives a `faux` provider, sets `signal` to a token, drops the token mid-stream, and asserts the resulting `AssistantMessage.stop_reason == StopReason::Aborted`. Same test sets `metadata` with a sentinel key and asserts the `on_payload` callback observed it.

4. **Cross-provider routing.** `tests/cross_provider_handoff.rs` (new) loads a 5-message context generated against `Anthropic`, switches the active model to `OpenAI Responses`, calls `transform_messages`, and asserts no orphan tool-calls remain and tool-call IDs are normalized.

5. **Compat matrix.** `tests/compat_resolution.rs` (new) constructs synthetic `Model`s with various `base_url`s (OpenRouter, Z.ai, Qwen-Chat, Cloudflare Workers AI, Azure) and asserts each `resolve_compat` call returns the expected `ResolvedCompat` (verified against snapshot fixtures in `tests/fixtures/compat/`).

The plan is complete when:

- All 14 milestones in Progress are checked.
- `cargo test -p model --all-features --release` passes.
- The five acceptance checks above produce the expected output on a fresh clone.

## Idempotence and Recovery

- Type extensions in Milestone 1 are additive; running them twice is a no-op (Edit tool fails on duplicate text).
- OAuth file writes use atomic temp-file + `fs::rename`; partial crashes never corrupt `oauth.json`.
- Test ports can be re-run unconditionally. Tests are hermetic — no shared global state — so order independence is preserved.
- If a milestone is interrupted, mark its Progress entry as `[~]` (in-progress) with a note describing what landed and what remains. The next session reads Progress to resume.
- A milestone can be reverted by `git revert` of its commit range; later milestones never depend on intermediate refactors except via the public `lib.rs` re-exports, which are stable.

## Artifacts and Notes

### Diff size estimate

| Milestone | Approx LOC added |
|-----------|------------------|
| 1 | ~250 (types only) |
| 2 | ~750 |
| 3 | ~400 (refactor + new) |
| 4 | ~1700 |
| 5 | ~600 + 800 tests |
| 6 | ~700 + 200 tests |
| 7 | ~300 + 100 tests |
| 8 | ~600 + 100 tests |
| 9 | ~1400 + 400 tests |
| 10 | ~80 + 50 tests |
| 11 | ~250 + 200 tests |
| 12 | ~300 + 200 tests |
| 13 | ~200 |
| 14 | docs |
| **Total** | **~7800 src + ~2100 tests** |

The Rust crate roughly doubles from 9.3k to ~17k LOC, mirroring the 29k TS LOC at a higher density (Rust eliminates many TS type-system files we don't need, like `typebox-helpers.ts`).

### New `Cargo.toml` dependencies

```toml
tokio-tungstenite = { version = "0.21", features = ["rustls-tls-webpki-roots"] }
tokio-util = { version = "0.7", features = ["sync"] }
hyper = { version = "1", features = ["server", "http1"] }
hyper-util = { version = "0.1", features = ["tokio"] }
base64 = "0.22"
urlencoding = "2"
rand = "0.8"
thiserror = "1"
```

### Risks

- **WebSocket-cached transport** is novel; first-cut implementation may need iteration. Mitigation: ship SSE first, gate WebSocket behind `--transport websocket`, treat WebSocket-cached as Milestone 9 stretch.
- **Vertex ADC** depends on `gcloud` CLI presence on dev machines. Mitigation: detect absence and surface a clear error pointing to setup docs; CI test only the `api_key` path.
- **OAuth loopback redirect** must reserve a free port and avoid colliding with developer-local servers. Mitigation: pick port 0, let OS assign, communicate URL via terminal.

## Interfaces and Dependencies

### Final public surface (after Milestone 14)

In `packages/model/src/lib.rs`, the following must be exported:

```rust
// Core types (Milestone 1)
pub use types::{
    Api, AnthropicMessagesCompat, AssistantContentBlock, AssistantMessage,
    AssistantMessageEvent, CacheRetention, Compat, Context, Cost, ImageContent,
    InputType, Message, Model, OpenAICompletionsCompat, OpenAIResponsesCompat,
    OpenRouterRouting, ProviderResponse, ProviderStreamOptions, SimpleStreamOptions,
    StopReason, StreamOptions, TextContent, ThinkingBudgets, ThinkingContent,
    ThinkingLevel, ThinkingLevelMap, Tool, ToolCall, ToolResultContent,
    ToolResultMessage, Transport, Usage, UsageCost, UserContent, UserContentBlock,
    UserMessage, VercelGatewayRouting,
};

// Utils (Milestone 2)
pub use utils::diagnostics::{AssistantMessageDiagnostic, DiagnosticKind};
pub use utils::event_stream::EventStream;
pub use utils::json_parse::{safe_parse_partial, try_parse_strict};
pub use utils::sanitize_unicode::sanitize as sanitize_unicode;
pub use utils::validation::{ValidationIssue, validate_context};
pub use utils::overflow::is_context_overflow;

// OAuth (Milestone 4)
pub use oauth::{
    OAuthCredentials, OAuthAuthInfo, OAuthProvider, OAuthProviderId,
    OAuthRegistry, anthropic::AnthropicOAuth, openai_codex::OpenAICodexOAuth,
    github_copilot::GitHubCopilotOAuth,
};

// Providers (Milestones 5-11)
pub use providers::{
    AnthropicMessagesProvider, AzureOpenAIResponsesProvider, BedrockProvider,
    CloudflareWorkersAiProvider, CloudflareAiGatewayProvider,
    GoogleGenerativeAiProvider, GoogleVertexProvider, MistralProvider,
    OpenAICodexResponsesProvider, OpenAICompletionsProvider,
    OpenAIResponsesProvider, register_builtins, resolve_compat,
};

// Faux (feature-gated)
#[cfg(feature = "faux")]
pub use providers::faux::{FauxProvider, FauxScript};

// Stream wrapper (Milestone 12)
pub use stream::{stream_simple, complete_simple};
```

### Key trait signatures

In `packages/model/src/oauth/types.rs`:

```rust
#[async_trait::async_trait]
pub trait OAuthProvider: Send + Sync {
    fn id(&self) -> OAuthProviderId;
    async fn login(&self, callbacks: &OAuthLoginCallbacks) -> Result<OAuthCredentials, OAuthError>;
    async fn refresh(&self, creds: &OAuthCredentials) -> Result<OAuthCredentials, OAuthError>;
    async fn revoke(&self, creds: &OAuthCredentials) -> Result<(), OAuthError>;
    fn is_expired(&self, creds: &OAuthCredentials) -> bool;
}
```

In `packages/model/src/utils/event_stream.rs`:

```rust
pub struct EventStream {
    inner: Pin<Box<dyn Stream<Item = AssistantMessageEvent> + Send>>,
}

impl EventStream {
    pub fn new<S>(stream: S) -> Self where S: Stream<Item = AssistantMessageEvent> + Send + 'static;
    pub async fn collect_to_message(self) -> Result<AssistantMessage, AssistantMessage>;
    pub async fn next(&mut self) -> Option<AssistantMessageEvent>;
}
```

In `packages/model/src/stream.rs`:

```rust
pub async fn stream_simple(
    client: &Client,
    model: &Model,
    context: Context,
    options: Option<SimpleStreamOptions>,
) -> Result<EventStream, ClientError>;

pub async fn complete_simple(
    client: &Client,
    model: &Model,
    context: Context,
    options: Option<SimpleStreamOptions>,
) -> Result<AssistantMessage, ClientError>;
```

### External crate dependencies (added in this plan)

- `tokio-tungstenite` — WebSocket transport for Codex Responses (Milestone 9)
- `tokio-util` (`sync`) — `CancellationToken` for `StreamOptions.signal` (Milestone 1, used in 12)
- `hyper` + `hyper-util` — loopback callback server for OAuth (Milestone 4)
- `base64`, `urlencoding`, `rand` — PKCE and URL encoding (Milestone 4)
- `thiserror` — structured errors across new modules (all milestones)

No removals; all existing dependencies stay.
