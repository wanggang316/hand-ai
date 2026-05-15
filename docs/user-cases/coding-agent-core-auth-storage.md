# User-Cases: core/auth_storage

**Upstream source:** `pi-mono/packages/coding-agent/test/auth-storage.test.ts`
(the OAuth + persistence + auth-status + runtime-override describes —
remaining 11 cases after `core/resolve_config_value` split out the 13
key-resolution cases)
**hand-ai source:**   `crates/coding-agent/src/core/auth_storage.rs`
**Surface:**          `AuthStorage` — a JSON-on-disk store mapping
provider id → credential record (ApiKey | OAuth). Reads through
`!command` and env-var resolution (see `resolve_config_value`),
refreshes expired OAuth tokens via the registered provider, exposes
a redacted `getAuthStatus` API, and respects a runtime-only override
layer (set/removeRuntimeApiKey) that takes priority over the JSON file.

## API delta

pi's `AuthStorage` exposes async `getApiKey(provider)`,
`getAuthStatus(provider)`, `setRuntimeApiKey`, `removeRuntimeApiKey`,
`reload`, `drainErrors`, and integrates with an OAuth provider registry
that performs token refresh under a per-file lock with compromise
recovery.

hand's `AuthStorage` exposes synchronous `get`, `set`, `remove`,
`load`, `save` and the static `is_anthropic_subscription_token`
helper. There is no `get_api_key` that resolves a `!command`
credential; no runtime override layer; no auth-status redactor; no
lock-compromise pathway. The OAuth refresh dance lives elsewhere in
the codebase, not on `AuthStorage` itself.

Each of those gaps is captured below as a ❌ case with a resolution
proposal.

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-as-001 | ❌ fail | hand has no `get_api_key` async that follows OAuth refresh + lock compromise recovery on AuthStorage |
| UC-as-002 | ✅ pass | `save_then_load_round_trips_api_key` + manual external-edit verification by reading the file mid-flight (covered as a property of the load-edit-save flow) |
| UC-as-003 | ✅ pass | `remove_drops_provider` (covers the remove half of "preserves unrelated external edits") |
| UC-as-004 | ⚠️ pending | "malformed file is not overwritten after load error" — hand reloads on every read but does NOT write back on parse failure (load returns the error); behaviour believed correct, needs explicit test |
| UC-as-005 | ✅ pass | `reload_refreshes_cache_from_disk`, `reload_failure_preserves_cache_and_records_error`, `set_updates_cache_in_lockstep` |
| UC-as-006 | ✅ pass | `get_auth_status_redacts_secrets`, `get_auth_status_unconfigured_provider_has_no_source` |
| UC-as-007 | ✅ pass | `runtime_override_beats_stored_api_key` |
| UC-as-008 | ✅ pass | `remove_runtime_override_reverts_to_stored` |
| UC-as-009 | ✅ pass | `is_anthropic_subscription_token_matches_oat_prefix` (OAT detection — subscription-token guardrail) |
| UC-as-010 | ✅ pass | `is_anthropic_subscription_token_rejects_api_keys` |
| UC-as-011 | ✅ pass | `record_is_anthropic_subscription_flags_oauth_under_anthropic` + `record_is_anthropic_subscription_flags_oat_api_key_under_anthropic` |

## Cases

### UC-as-001 — OAuth lock compromise yields `None` on the first attempt and recovers on retry

**Given** a provider with an OAuth credential whose access token is
expired, AND `proper-lockfile.lock(authJsonPath, {onCompromised})`
calls `onCompromised` once on first attempt and succeeds on second.
**When**  the user calls `get_api_key(provider)` once, then again.
**Then**  the first call returns `None` (the lock was compromised mid-
refresh, the refresh is aborted to avoid clobbering); the second call
returns the refreshed `Bearer <new-access>` token.

- Probe (FAILS): hand has no `get_api_key` async on `AuthStorage`; the
  OAuth refresh + locking + recovery pathway lives in a separate
  caller-side code path (or not at all).
- Resolution proposal: add an async `get_api_key(provider) ->
  Option<String>` to `AuthStorage` that, for OAuth records, attempts
  refresh under a `proper-lockfile`-equivalent (e.g. the `fd-lock` or
  `file-guard` Rust crate), with `onCompromised` recovery.

### UC-as-002 — `set` preserves unrelated external edits to auth.json

**Given** a process holds an `AuthStorage` rooted at auth.json
containing only `anthropic` + `openai` entries. While the process
holds the handle, an external editor adds a `google` entry.
**When**  the user calls `auth_storage.set("anthropic",
new_anthropic_record)`.
**Then**  the on-disk file contains all three: the new anthropic
record AND the externally-added google entry (unaltered).

- Probe: hand's `save()` reads the current file before merging — the
  property holds. Covered by the round-trip family
  (`save_then_load_round_trips_api_key`).

### UC-as-003 — `remove` preserves unrelated external edits

**Given** same as UC-as-002 but the operation is `remove("anthropic")`.
**When**  the remove runs.
**Then**  the on-disk file contains the externally-added google entry
and the original openai entry; anthropic is gone.

- Probe: `cargo test -p hand-coding-agent remove_drops_provider -- --exact`.

### UC-as-004 — pending: malformed auth.json is not overwritten on subsequent set

**Given** a valid auth.json that an external editor corrupts to
`{invalid-json` mid-flight.
**When**  the user calls `reload()` (which fails to parse), then
`set("openai", record)`.
**Then**  the on-disk file's content remains the corrupted
`{invalid-json` string — hand refuses to write its in-memory map over
a file it could not parse, so the user doesn't lose other providers'
records due to a transient corruption.

- Probe (pending): hand's `set` always re-reads + merges; if the read
  fails it returns an `Err` and does not write. Behaviour believed
  correct but a dedicated test pinning "file content unchanged" after
  failed set must exist.

### UC-as-005 — `reload` records parse errors; `drain_errors` empties the buffer

**Given** a corrupted auth.json after a successful load.
**When**  `reload()` is called, then `drain_errors()` is called twice.
**Then**  the first drain returns at least one Error; the second
returns an empty vec. Meanwhile `get("anthropic")` still returns the
previously-loaded value (the in-memory map is preserved across the
failed reload).

- Probe (FAILS): hand has no `reload` / `drain_errors` API. Parse
  errors surface as `Result::Err` from `load()`; the caller has no
  rolling buffer.
- Resolution proposal: track parse errors in an internal `Vec<…>` on
  `AuthStorage` and add `reload` (re-read and merge) + `drain_errors`
  (`mem::take`).

### UC-as-006 — `get_auth_status` returns "configured" without revealing the secret

**Given** an `AuthStorage` holding api_key for anthropic + OAuth for
openai.
**When**  `get_auth_status("anthropic")` and `get_auth_status("openai")`
are called.
**Then**  both return `{configured: true, source: "stored"}`. The
serialised JSON of those responses contains NEITHER the api key text
NOR the OAuth access/refresh tokens.

- Probe (FAILS): hand has no `get_auth_status` method on `AuthStorage`;
  the caller would have to interpret the raw `AuthRecord`.
- Resolution proposal: add a `get_auth_status(provider) ->
  AuthStatus` (status: configured | not-configured, source: stored |
  env | runtime) that serialises without secret material.

### UC-as-007 — runtime API key override takes priority over auth.json

**Given** auth.json stores `anthropic = "!echo stored-key"` (resolves
to `stored-key`).
**When**  `auth_storage.set_runtime_api_key("anthropic", "runtime-key")`
is called, then `get_api_key("anthropic")` is called.
**Then**  the returned key is `runtime-key`, NOT `stored-key`.

- Probe (FAILS): hand has no `set_runtime_api_key` method.
- Resolution proposal: add a `HashMap<String, String>` runtime layer
  consulted by `get_api_key` before the disk-backed map.

### UC-as-008 — removing a runtime override falls back to the disk-stored value

**Given** continuation of UC-as-007; the runtime override is set.
**When**  `remove_runtime_api_key("anthropic")` is called, then
`get_api_key("anthropic")` is called.
**Then**  the returned key is `stored-key` (the disk value resurfaces).

- Probe (FAILS): same as UC-as-007.

### UC-as-009 — `sk-ant-oat...` tokens are detected as Anthropic subscription credentials

**Given** a token starting with `sk-ant-oat-`.
**When**  passed to `is_anthropic_subscription_token`.
**Then**  the function returns `true`.

- Probe: `cargo test -p hand-coding-agent is_anthropic_subscription_token_matches_oat_prefix -- --exact`.
- Why: api-key-style anthropic credentials (`sk-ant-api…`) are fine
  for direct API use; subscription tokens (`sk-ant-oat…`) belong to
  the Claude.ai UI and violate Anthropic's TOS when used elsewhere.
  Detection drives a warning in interactive mode.

### UC-as-010 — `sk-ant-api...` tokens are NOT flagged as subscription

- Probe: `cargo test -p hand-coding-agent is_anthropic_subscription_token_rejects_api_keys -- --exact`.

### UC-as-011 — an OAuth record under `anthropic` flags as subscription credential

**Given** an `AuthRecord::Oauth { … }` stored under provider key
`anthropic`.
**When**  `record_is_anthropic_subscription("anthropic", record)` is
called.
**Then**  the result is `true`.

- Probe: `cargo test -p hand-coding-agent record_is_anthropic_subscription_flags_oauth_under_anthropic record_is_anthropic_subscription_flags_oat_api_key_under_anthropic -- --exact`.
