# User-Cases: model/stream — retry classification

**Upstream source:** `pi-mono/packages/ai/src/types.ts` (`isRetriable`) +
the retry behaviour exercised indirectly across provider e2e tests.
pi has no dedicated retry-classification test file; the contract is
implicit in the production code that decides whether to retry on each
streaming error.
**hand-ai source:**   `crates/model/src/stream.rs`
**Surface:**          `is_retriable_error(message)` — classifies error
strings as transient (retry with exponential backoff) vs terminal
(propagate to the user). `compute_backoff(attempt, max_delay_ms)` —
exponential-base backoff capped at the user's max delay. The model
stream applies both on every retriable error encountered.

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-stream-001 | ✅ pass | `retriable_recognizes_429_and_503` |
| UC-stream-002 | ✅ pass | `retriable_rejects_400_and_500` |
| UC-stream-003 | ✅ pass | `retriable_recognizes_provider_network_error` |
| UC-stream-004 | ✅ pass | `retriable_still_rejects_other_5xx` (501, 505) |
| UC-stream-005 | ✅ pass | `retriable_recognizes_request_ended_without_chunks` |
| UC-stream-006 | ✅ pass | `retriable_recognizes_transient_provider_tokens` |
| UC-stream-007 | ✅ pass | `retriable_recognizes_network_connection_lost` |
| UC-stream-008 | ✅ pass | `backoff_grows_exponentially_and_caps` |

## Cases

### UC-stream-001 — HTTP 429 and 503 are retriable

**Given** an error message of the form `HTTP 429 too many requests`
or `HTTP 503 service unavailable` (or contains `connection reset`,
`ECONNRESET`).
**When**  `is_retriable_error(msg)` is called.
**Then**  it returns `true`. The agent loop retries with backoff.

- Probe: `cargo test -p model retriable_recognizes_429_and_503 -- --exact`.

### UC-stream-002 — HTTP 400, 401, and 500 are terminal

**Given** an error message `HTTP 400 bad request`, `HTTP 401
unauthorized`, or `HTTP 500 internal server error`.
**When**  `is_retriable_error` runs.
**Then**  it returns `false`. The error propagates immediately.

- Probe: `cargo test -p model retriable_rejects_400_and_500 -- --exact`.
- Why 500 is terminal: a 500 means a real server-side bug that won't
  improve on retry. We don't want the agent to silently retry forever.

### UC-stream-003 — `finish_reason: network_error` and `HTTP 502/504` are retriable

**Given** an error string `Provider finish_reason: network_error`
(z.ai surfaces transient blips this way) or `HTTP 502 bad gateway`
or `HTTP 504 gateway timeout`.
**When**  `is_retriable_error` runs.
**Then**  it returns `true`.

- Probe: `cargo test -p model retriable_recognizes_provider_network_error -- --exact`.

### UC-stream-004 — 5xx statuses outside the transient set stay terminal

**Given** `HTTP 501 not implemented` or `HTTP 505 http version not
supported`.
**When**  `is_retriable_error` runs.
**Then**  it returns `false` — only 502, 503, 504 are recognised
transient 5xx codes.

- Probe: `cargo test -p model retriable_still_rejects_other_5xx -- --exact`.

### UC-stream-005 — "ended without … chunks" wordings are retriable

**Given** an error string `request ended without sending any chunks`
or `Stream ended without response body`.
**When**  `is_retriable_error` runs.
**Then**  it returns `true`.

- Probe: `cargo test -p model retriable_recognizes_request_ended_without_chunks -- --exact`.

### UC-stream-006 — a panel of transient provider tokens are retriable

**Given** an error message containing any of:
- `overloaded_error`
- `rate limit exceeded` / `Too Many Requests`
- `fetch failed`
- `service unavailable`
- `socket hang up`
- `upstream connect error`
- `reset before headers were received`
- `other side closed`
- `request timed out after 60s` / `AbortError: timeout`
- `Stream terminated unexpectedly`
- `retry delay 30 seconds`
- `http2 request did not get a response`
**When**  `is_retriable_error` runs on each.
**Then**  every one returns `true`.

- Probe: `cargo test -p model retriable_recognizes_transient_provider_tokens -- --exact`.

### UC-stream-007 — Apple's `Network connection lost.` wording is retriable

**Given** an error string `Network connection lost.` or
`Provider returned: Network connection lost. Try again.`.
**When**  `is_retriable_error` runs.
**Then**  it returns `true`.

- Probe: `cargo test -p model retriable_recognizes_network_connection_lost -- --exact`.
- Negative cases also pinned: `Network is fine.` and `Connection
  details: ...` must NOT match (no false-positive on adjacent
  wordings).

### UC-stream-008 — backoff doubles each attempt and caps at the configured maximum

**Given** `compute_backoff(attempt, max_delay_ms = 30000)` with
`DEFAULT_BASE_RETRY_DELAY_MS` baseline (1000ms).
**When**  the function is called for attempts 1..20.
**Then**:
- attempt 1 → 1000 ms (base)
- attempt 2 → 2000 ms
- attempt 3 → 4000 ms
- attempt 4 → 8000 ms
- attempt 5 → 16000 ms
- attempt 6 → 30000 ms (capped)
- attempt 7..20 → 30000 ms (capped)

- Probe: `cargo test -p model backoff_grows_exponentially_and_caps -- --exact`.
- Why: a single transient blip should retry quickly; a sustained
  provider outage should back off rapidly to avoid hammering the
  provider while waiting for it to recover.
