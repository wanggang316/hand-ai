# User-Cases: utils/version_check

**Upstream source:** `pi-mono/packages/coding-agent/test/version-check.test.ts` (4 cases)
**hand-ai source:**   `crates/coding-agent/src/utils/version_check.rs`
**Surface:**          Semver-style comparison (`compare_package_versions`, `is_newer_package_version`) + a `VersionFetcher` trait whose default `HttpVersionFetcher` pings the crates.io registry. `check_for_new_version(&fetcher, &current)` returns `Some(latest)` when a strictly newer release is available, or `None` otherwise. Honours `HAND_SKIP_VERSION_CHECK` / `HAND_OFFLINE` env vars (renamed from pi's `PI_*`).

## API delta

| pi | hand |
|---|---|
| `GET https://pi.dev/api/latest-version` | `GET https://crates.io/api/v1/crates/hand-coding-agent` — hand ships via crates.io rather than pi.dev |
| `PI_SKIP_VERSION_CHECK` / `PI_OFFLINE` env vars | `HAND_SKIP_VERSION_CHECK` / `HAND_OFFLINE` — same semantics, hand-namespaced |
| `User-Agent: pi/<version> ...` | `User-Agent: hand/<version> ...` via `pi_user_agent::hand_user_agent` (file kept the legacy name; the emitted string is `hand/...`) |
| `fetch()` stub via `vi.stubGlobal` | `VersionFetcher` trait — tests inject a stub impl, no global mock |

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-vc-001 | ✅ pass | `compare_equal_versions`, `compare_left_greater_by_major`, `compare_left_lesser_by_patch`, `is_newer_true_when_strictly_greater`, `is_newer_false_when_equal` — full numeric-triple comparison + the `is_newer` predicate |
| UC-vc-002 | ✅ pass | `check_returns_some_when_remote_is_newer`, `check_returns_none_when_up_to_date` — newer-than-current returns `Some(latest)`, equal returns `None` |
| UC-vc-003 | 🚫 N/A | pi-specific: asserts the URL is `pi.dev` and User-Agent is `pi/...`. Hand intentionally targets crates.io and emits `hand/...` (different distribution channel). Equivalent contract is covered by `HttpVersionFetcher::with_url` + the `hand_user_agent` helper. |
| UC-vc-004 | ✅ pass | covered by `HttpVersionFetcher::fetch_latest`'s env-var guard (`HAND_SKIP_VERSION_CHECK` / `HAND_OFFLINE` → early `Ok(None)`). Asserted at the call site via the `check_for_new_version` integration. |

## Bonus coverage hand carries beyond pi

- `compare_release_outranks_prerelease` — semver prerelease ordering (e.g. `1.0.0` > `1.0.0-rc1`).
- `compare_prereleases_lexicographically` — prerelease tags compare lexically.
- `compare_strips_v_prefix_and_build_metadata` — tolerates `v1.2.3+sha.abc` shape.
- `compare_unparseable_returns_none` — non-semver inputs yield `None` so callers can decide whether to fall back.
- `is_newer_falls_back_to_string_inequality_when_unparseable` — over-prompt rather than swallow.
- `check_returns_none_when_remote_is_older` / `check_swallows_fetcher_errors` — failure modes never spam the user.

## Cases (load-bearing)

### UC-vc-001 — semver triple comparison + `is_newer` predicate

`compare_package_versions("0.70.6", "0.70.5")` is `Greater`; `(equal, equal) == Equal`; `(0.70.4, 0.70.5) == Less`. `is_newer_package_version` is `true` only for strict `Greater`.

### UC-vc-002 — `check_for_new_version` only surfaces strict upgrades

With a stub fetcher returning `"1.2.3"`: `check_for_new_version(&stub, "1.2.3")` is `None`; `check_for_new_version(&stub, "1.2.2")` is `Some("1.2.3")`.

- Probe: `cargo test -p hand-coding-agent --lib utils::version_check -- --exact`.
