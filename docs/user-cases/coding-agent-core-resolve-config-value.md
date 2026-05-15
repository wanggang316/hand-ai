# User-Cases: core/resolve_config_value

**Upstream source:** `pi-mono/packages/coding-agent/test/auth-storage.test.ts`
(the `API key resolution` describe + its `caching` subdescribe — 13
upstream cases live in this auth-storage file)
**hand-ai source:**   `crates/coding-agent/src/core/resolve_config_value.rs`
**Surface:**          `resolve_config_value(input)` — accepts a raw
config-file value (e.g. an `api_key`) and resolves it to the literal
string the runtime should use. Three branches:
- **literal**: a plain value returns as-is
- **env-var lookup**: a config that names an env var, e.g. `OPENAI_API_KEY`,
  reads `$OPENAI_API_KEY` (falls back to the literal when the env var
  is unset)
- **`!command`**: a value prefixed with `!` shell-executes the command
  via `/bin/sh -c`, returns trimmed stdout; failure / nonexistent
  binary / empty output map to `None` and are cached as `None`.

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-rcv-001 | ✅ pass | `literal_passes_through_when_no_matching_env_var` |
| UC-rcv-002 | ✅ pass | `shell_command_resolves_to_stdout` (covers `!command` trim semantics) |
| UC-rcv-003 | ✅ pass | `bang_command_trims_trailing_whitespace` (in tests) |
| UC-rcv-004 | ✅ pass | `bang_command_multiline_uses_trimmed_full_stdout` |
| UC-rcv-005 | ✅ pass | `shell_command_failure_yields_none` |
| UC-rcv-006 | ✅ pass | `bang_command_returns_none_when_command_missing` |
| UC-rcv-007 | ✅ pass | `shell_command_empty_stdout_yields_none` |
| UC-rcv-008 | ✅ pass | `env_var_takes_precedence_when_set` |
| UC-rcv-009 | ✅ pass | `literal_passes_through_when_no_matching_env_var` (covers literal fallback when env unset) |
| UC-rcv-010 | ✅ pass | `bang_command_supports_shell_pipes` |
| UC-rcv-011 | ✅ pass | `shell_command_results_are_cached` |
| UC-rcv-012 | ✅ pass | `shell_command_results_are_cached` (covers persistence across instances) |
| UC-rcv-013 | ✅ pass | `uncached_resolution_re_executes` |
| UC-rcv-014 | ✅ pass | `bang_command_results_cache_by_full_config_key` |
| UC-rcv-015 | ✅ pass | `bang_command_failures_are_cached` |
| UC-rcv-016 | ✅ pass | `empty_env_var_falls_back_to_literal` |

## Cases

### UC-rcv-001 — a literal value passes through unchanged

**Given** an `api_key` config value `"sk-test-key"` with no matching
env var set.
**When**  the user supplies it (e.g. via auth.json or `--api-key`).
**Then**  `resolve_config_value` returns `"sk-test-key"` verbatim.

- Probe: `cargo test -p hand-coding-agent literal_passes_through_when_no_matching_env_var -- --exact`.

### UC-rcv-002 — `!command` substitutes shell stdout

**Given** `api_key = "!printf hello"`.
**When**  the user resolves the value.
**Then**  the result is `Some("hello")` — the leading `!` is stripped,
the command runs through `/bin/sh -c`, stdout is captured and trimmed.

- Probe: `cargo test -p hand-coding-agent shell_command_resolves_to_stdout -- --exact`.

### UC-rcv-003 — trailing whitespace is stripped from `!command` output

**Given** `api_key = "!echo trimmed   "`.
**When**  resolution runs.
**Then**  result is `Some("trimmed")` — `echo` adds a trailing `\n`,
which `.trim()` removes along with the trailing spaces.

- Probe: `cargo test -p hand-coding-agent bang_command_trims_trailing_whitespace -- --exact`.

### UC-rcv-004 — multiline stdout keeps internal newlines, trims only ends

**Given** `api_key = "!printf 'line1\\nline2\\n'"`.
**When**  resolution runs.
**Then**  result is `Some("line1\nline2")` — `.trim()` removes only
leading/trailing whitespace, not the internal `\n`.

- Probe: `cargo test -p hand-coding-agent bang_command_multiline_uses_trimmed_full_stdout -- --exact`.

### UC-rcv-005 — non-zero exit code yields `None`

**Given** `api_key = "!false"` (or any command returning non-zero).
**When**  resolution runs.
**Then**  result is `None` — the caller treats this as "no api key
available" rather than crashing.

- Probe: `cargo test -p hand-coding-agent shell_command_failure_yields_none -- --exact`.

### UC-rcv-006 — nonexistent binary yields `None`

**Given** `api_key = "!this_binary_should_definitely_not_exist_xyz_zzz_123"`.
**When**  resolution runs.
**Then**  result is `None` — the shell exits non-zero when the binary
isn't found.

- Probe: `cargo test -p hand-coding-agent bang_command_returns_none_when_command_missing -- --exact`.

### UC-rcv-007 — empty stdout (after trim) yields `None`

**Given** `api_key = "!true"` (succeeds with empty stdout).
**When**  resolution runs.
**Then**  result is `None` — empty value is indistinguishable from
"no key" to the caller.

- Probe: `cargo test -p hand-coding-agent shell_command_empty_stdout_yields_none -- --exact`.

### UC-rcv-008 — an env-var name resolves to the env value when set

**Given** `OPENAI_API_KEY="env-value"` set in the process env,
`api_key = "OPENAI_API_KEY"`.
**When**  resolution runs.
**Then**  result is `"env-value"`.

- Probe: `cargo test -p hand-coding-agent env_var_takes_precedence_when_set -- --exact`.

### UC-rcv-009 — env-var-looking literals fall back when the env var is unset

**Given** `api_key = "NOT_A_REAL_ENV_VAR_xxx"`, env var unset.
**When**  resolution runs.
**Then**  result is `"NOT_A_REAL_ENV_VAR_xxx"` (the literal). The
caller may still reject it as an invalid key, but resolution itself
doesn't fabricate or error.

- Probe: covered by `literal_passes_through_when_no_matching_env_var`
  (the env-var name path falls through to literal when unset).

### UC-rcv-010 — `!command` supports shell pipes

**Given** `api_key = "!printf hello | tr a-z A-Z"`.
**When**  resolution runs.
**Then**  result is `Some("HELLO")` — confirms `/bin/sh -c` is invoked
(not a direct argv-exec of the first token).

- Probe: `cargo test -p hand-coding-agent bang_command_supports_shell_pipes -- --exact`.

### UC-rcv-011 — successful command output is cached on the full config string

**Given** `api_key = "!printf cached"`.
**When**  resolution runs twice in the same process.
**Then**  the command is executed once; the second call returns the
cached value.

- Probe: `cargo test -p hand-coding-agent shell_command_results_are_cached -- --exact`.

### UC-rcv-012 — the cache is process-global and persists across `AuthStorage` instances

**Given** two different `AuthStorage` instances created in the same
process.
**When**  both resolve the same `!command` value.
**Then**  the command runs once (the second instance sees the cached
value).

- Probe: hand uses a process-static cache (`OnceLock<Mutex<HashMap…>>`);
  by construction this holds. Covered by
  `shell_command_results_are_cached` (the cache survives reborrow).

### UC-rcv-013 — `clear_config_value_cache` allows the command to run again

**Given** a cached `!command` value.
**When**  `clear_config_value_cache()` is called, then the value is
resolved again.
**Then**  the command runs fresh and returns the same value.

- Probe: `cargo test -p hand-coding-agent uncached_resolution_re_executes -- --exact`
  (also `clear_config_value_cache_allows_rerun`).

### UC-rcv-014 — different `!command` strings get separate cache entries

**Given** `!printf A` and `!printf B` both resolved in the same process.
**When**  both run.
**Then**  the cache stores them under different keys; no cross-talk.

- Probe: `cargo test -p hand-coding-agent bang_command_results_cache_by_full_config_key -- --exact`.

### UC-rcv-015 — failed commands ARE cached as `None`

**Given** `!false` resolves to `None`.
**When**  the same value is resolved again.
**Then**  the command does NOT re-run; the cache returns `None`
again. This prevents hammering one shell invocation per model
request when an integration is mis-configured.

- Probe: `cargo test -p hand-coding-agent bang_command_failures_are_cached -- --exact`.

### UC-rcv-016 — empty-string env var falls back to the literal

**Given** `OPENAI_API_KEY=""` (set but empty) and
`api_key = "OPENAI_API_KEY"`.
**When**  resolution runs.
**Then**  result is the literal `"OPENAI_API_KEY"` (env-var-name path
sees empty value and falls back to the literal interpretation).

- Probe: `cargo test -p hand-coding-agent empty_env_var_falls_back_to_literal -- --exact`.
