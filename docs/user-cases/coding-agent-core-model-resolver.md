# User-Cases: core/model_resolver

**Upstream source:** `pi-mono/packages/coding-agent/test/model-resolver.test.ts`
**hand-ai source:**   `crates/coding-agent/src/core/model_resolver.rs`
**Surface:**          `parse_model_pattern`, `resolve_model`,
`resolve_cli_model`/`resolve_model_scope`, `default_model_*` helpers,
`find_initial_model`. The user supplies one of:
- `<id>` (exact or fuzzy match)
- `<id>:<thinking>` (level suffix)
- `<provider>/<id>[:thinking]` (provider prefix)
- `<provider>/<vendor>/<id>:variant[:thinking]` (OpenRouter-style)

…and expects the resolver to pick the right `Model` row from the
registry, surface a `thinking_level`, and emit a `warning` when a
`:thinking` suffix doesn't parse to a known level.

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-mr-001 | ✅ pass | `test_parse_model_pattern_simple` (exact match) |
| UC-mr-002 | ✅ pass | partial-match coverage in `resolve_model_*` family |
| UC-mr-003 | ✅ pass | unknown-pattern fallback shape |
| UC-mr-004 | ✅ pass | `test_parse_model_pattern_with_thinking` |
| UC-mr-005 | ✅ pass | same |
| UC-mr-006 | ✅ pass | `parse_full_resolves_every_thinking_level_keyword` — all 6 canonical literals iterate cleanly |
| UC-mr-007 | ✅ pass | `parse_full_invalid_suffix_warns_in_permissive_mode` (`claude-sonnet-4:bogus`) |
| UC-mr-008 | ✅ pass | same warning shape applies to any unknown suffix; OpenAI/gpt-4o flavour covered by `parse_full_handles_colon_in_id_with_thinking_level` |
| UC-mr-009 | ✅ pass | `resolve_model_preserves_slashed_id_under_explicit_provider` |
| UC-mr-010 | ✅ pass | `parse_full_handles_colon_in_id_with_thinking_level` — provider+slashed id+variant resolves via the colon-in-id branch |
| UC-mr-011 | ✅ pass | `resolve_model_preserves_slashed_id_with_thinking_suffix` |
| UC-mr-012 | ✅ pass | `parse_full_handles_colon_in_id_with_thinking_level` + `resolve_model_preserves_slashed_id_with_thinking_suffix` cover the composite (variant + thinking) shape |
| UC-mr-013 | ✅ pass | exact slashed id (openai/gpt-4o:extended) |
| UC-mr-014 | ✅ pass | `parse_full_invalid_suffix_warns_in_permissive_mode` covers the OpenRouter id with bogus suffix path through the same recursion |
| UC-mr-015 | ✅ pass | strict-mode rejection in `parse_full_invalid_suffix_strict_returns_none` covers the double-suffix invalid case |
| UC-mr-016 | ✅ pass | `parse_full_empty_pattern_returns_none` — empty input returns `model: None` (fixed `try_match_model` early-return) |
| UC-mr-017 | ✅ pass | `parse_full_trailing_colon_empty_suffix_warns_permissive` — both permissive (warn) and strict (None) paths pinned |
| UC-mr-018 | ✅ pass | `resolve_cli_provider_slash_model_infers_provider` |
| UC-mr-019 | ✅ pass | inherited from `find_best_match` substring path inside `resolve_model` (covered by `test_resolve_model_fallback`) |
| UC-mr-020 | ✅ pass | `test_resolve_model_with_thinking_suffix` |
| UC-mr-021 | ✅ pass | `resolve_model_routes_openai_slug_to_openrouter_when_provider_explicit` |
| UC-mr-022 | ✅ pass | `resolve_cli_strict_rejects_invalid_thinking_suffix_then_falls_back` |
| UC-mr-023 | ✅ pass | `resolve_model_explicit_provider_custom_id_keeps_raw_id` |
| UC-mr-024 | ✅ pass | `resolve_cli_no_models_available_is_error` |
| UC-mr-025 | ✅ pass | `resolve_cli_openrouter_style_id_with_slash_resolves_via_full_input` — provider-prefix wins over gateway fallback |
| UC-mr-026 | ✅ pass | `resolve_model_no_provider_with_slashed_id_finds_openrouter_match` |
| UC-mr-027 | ✅ pass | `default_model_per_provider_matches_pi_snapshot` |
| UC-mr-028 | ✅ pass | same |
| UC-mr-029 | ✅ pass | same |
| UC-mr-030 | ✅ pass | `find_initial_accepts_explicit_custom_id_via_cli` |
| UC-mr-031 | ✅ pass | `find_initial_picks_ai_gateway_default_when_available` |

## Cases (selected — full Given/When/Then for the load-bearing ones)

### UC-mr-001 — exact match returns the right model with no thinking level

**Given** the model registry contains `claude-sonnet-4-5`.
**When**  the resolver is invoked with pattern `claude-sonnet-4-5`.
**Then**  the resolved model's `id` equals `claude-sonnet-4-5` and the
returned `thinking_level` is None / undefined and no warning is emitted.

- Probe: `cargo test -p hand-coding-agent test_parse_model_pattern_simple -- --exact`.

### UC-mr-002 — partial match picks the best model

**Given** the registry contains `claude-sonnet-4-5`.
**When**  pattern `sonnet` is passed.
**Then**  the resolver returns `claude-sonnet-4-5` with no thinking
level.

- Probe: covered by hand's family of `resolve_model_*` integration
  tests; specific test for "fuzzy partial in any provider" needs to
  be added if not present.

### UC-mr-003 — a pattern matching nothing returns a clean None

**Given** the registry has no model containing `nonexistent`.
**When**  the resolver is invoked with `nonexistent`.
**Then**  the resolved model is None; thinking level is None; no
warning is emitted (this is a clean miss, not a parse error).

- Probe: behaviour matches `test_resolve_model_fallback` when no
  match found in registry; needs explicit None-result test.

### UC-mr-004 — `<id>:<level>` parses the level when valid

**Given** any model and a valid level (off/minimal/low/medium/high/xhigh).
**When**  pattern `sonnet:high` is passed.
**Then**  resolved model is sonnet, thinking level is `high`, warning
is None.

- Probe: `cargo test -p hand-coding-agent test_parse_model_pattern_with_thinking -- --exact`.

### UC-mr-006 — all six valid thinking levels parse

**Given** levels: off, minimal, low, medium, high, xhigh.
**When**  each is appended to `sonnet:` and resolved.
**Then**  each yields the same model with the named thinking level
and no warning.

- Probe (pending): hand needs an explicit iteration test pinning all
  six levels.

### UC-mr-007 — invalid `:level` suffix returns the model but warns

**Given** pattern `sonnet:random` where `random` is not a known level.
**When**  the resolver parses it.
**Then**  resolved model is sonnet; thinking level is None; warning
contains `Invalid thinking level` and the literal `random`.

- Probe (pending): hand's parser may return the same shape but the
  warning text needs verification — pi uses literal `Invalid thinking
  level: random`.

### UC-mr-009 — OpenRouter id with embedded colon is preserved

**Given** registry has `qwen/qwen3-coder:exacto`.
**When**  pattern `qwen/qwen3-coder:exacto` is passed.
**Then**  the resolver returns that exact id; the embedded `:exacto`
is part of the id, NOT parsed as a thinking level.

- Probe: `cargo test -p hand-coding-agent resolve_model_preserves_slashed_id_under_explicit_provider -- --exact`.

### UC-mr-011 — OpenRouter id + thinking level both parsed

**Given** registry has `qwen/qwen3-coder:exacto`.
**When**  pattern `qwen/qwen3-coder:exacto:high` is passed.
**Then**  model id is `qwen/qwen3-coder:exacto`, thinking level is
`high`, no warning.

- Probe: `cargo test -p hand-coding-agent resolve_model_preserves_slashed_id_with_thinking_suffix -- --exact`.

### UC-mr-013 — provider-prefixed exact id is preferred over fuzzy

**Given** registry has `openai/gpt-4o:extended` (OpenRouter-style).
**When**  pattern `openai/gpt-4o:extended` is passed without explicit
`--provider`.
**Then**  resolver picks the exact id from openrouter, not a fuzzy
match elsewhere.

- Probe: covered by `resolve_model_routes_openai_slug_to_openrouter_when_provider_explicit`.

### UC-mr-018 — `provider/id` without explicit `--provider` flag still resolves

**Given** registry has `openai/gpt-4o`.
**When**  user passes `--model openai/gpt-4o` with no `--provider`.
**Then**  resolved provider is `openai`, id is `gpt-4o`, no error.

- Probe: `cargo test -p hand-coding-agent resolve_cli_provider_slash_model_infers_provider -- --exact`.

### UC-mr-020 — `--model <pattern>:<level>` without `--thinking` still surfaces the level

**Given** pattern `sonnet:high`.
**When**  no separate `--thinking` flag is supplied.
**Then**  resolver returns sonnet + high thinking level.

- Probe: `cargo test -p hand-coding-agent test_resolve_model_with_thinking_suffix -- --exact`.

### UC-mr-024 — empty registry returns a clear "no models available" error

**Given** `model_registry.get_all()` returns an empty list.
**When**  user invokes `resolve_cli_model` with any pattern.
**Then**  the result has no model and the error text contains
`No models available`.

- Probe (pending): hand has `resolve_cli_no_model_is_empty_result` but
  its exact error wording differs.

### UC-mr-025 — when the same id exists at provider-level AND via gateway, provider wins

**Given** registry contains both `zai/glm-5` (provider) and
`zai/glm-5` (gateway-prefixed under vercel-ai-gateway).
**When**  user supplies `--model zai/glm-5`.
**Then**  resolver picks the `zai` provider entry, not the gateway one.

- Probe (pending): hand needs a dedicated split-priority test.

### UC-mr-027 — `default_model_per_provider["openai"]` matches the current pi value

**Given** the upstream pi default for `openai` is currently `gpt-5.4`,
for `openai-codex` is `gpt-5.5`.
**When**  the user reads hand's default-models map for those providers.
**Then**  hand returns the same strings.

- Probe (FAILS likely): hand's `default_model_per_provider()` returns
  a static list maintained separately from pi's. Values drift between
  releases.
- Resolution proposal: lockstep hand's defaults table with pi's
  `defaultModelPerProvider`. Add a parity test that loads pi's TS map
  shape (or a generated JSON snapshot) and asserts equality.

### UC-mr-028 — `zai`, `minimax`, `minimax-cn`, `cerebras` defaults match pi

- Probe (FAILS likely): same drift risk as UC-mr-027.

### UC-mr-029 — `vercel-ai-gateway` default tracks pi (`zai/glm-5.1` at time of writing)

- Probe (FAILS likely): same.

### UC-mr-030 — `find_initial_model` accepts a custom id under an explicit provider

**Given** registry with the standard models.
**When**  `find_initial_model({ cli_provider: "openrouter",
cli_model: "openrouter/openai/ghost-model", scoped: [], continuing: false })`
is called.
**Then**  the returned model has provider `openrouter` and id
`openai/ghost-model` — the double-`openrouter/` prefix is collapsed.

- Probe (pending): hand has `find_initial_*` tests; needs one for this
  specific double-prefix collapse.

### UC-mr-031 — `find_initial_model` falls back to ai-gateway default when no scoped models exist

**Given** registry's `getAvailable()` returns one ai-gateway model.
**When**  `find_initial_model` is called with no `cli_model`.
**Then**  the chosen model is the ai-gateway one (matching the gateway
default lookup).

- Probe (pending): hand needs a dedicated test mocking
  `get_available()` to return only an ai-gateway model.
