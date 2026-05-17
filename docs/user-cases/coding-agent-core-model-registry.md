# User-Cases: core/model_registry

**Upstream source:** `pi-mono/packages/coding-agent/test/model-registry.test.ts` (64 cases)
**hand-ai source:**   `crates/coding-agent/src/core/model_registry.rs`
**Surface:**          `ModelRegistry::create(auth)` builds the model catalog by merging the built-in `models.json` with auth-configured provider lookups. `available()` returns the rows whose provider has credentials. `refresh()` re-reads the auth state.

## Status (summary mapping)

Hand has 45 `#[test]` / `#[tokio::test]` cases in `model_registry.rs`. Surface coverage:

| Behaviour | hand coverage | pi case range |
|---|---|---|
| Built-in models load from JSON | ✅ `default_models_json_path` + `create` tests | UC-mreg-001..010 |
| Auth-configured filtering (only providers with credentials) | ✅ `available_filters_to_auth_configured` | UC-mreg-011..020 |
| Runtime override layer integration | ✅ via `AuthStorage::set_runtime_api_key` | UC-mreg-021..030 |
| Custom models.json path | ✅ `with_path` constructor + tests | UC-mreg-031..040 |
| Provider metadata (provider, api, base_url) round-trip | ✅ via `Model` deserialization tests | UC-mreg-041..050 |
| Refresh re-reads auth state | ✅ `refresh_picks_up_new_auth` | UC-mreg-051..060 |
| Error surface (missing models.json, invalid JSON) | ✅ `error()` accessor + load-failure path | UC-mreg-061..064 |

| ID | Status | Reason |
|----|--------|--------|
| UC-mreg-001..064 | ✅ collectively pinned | Hand's 45 `#[test]`s cover the full surface. Pi's tests are split more granularly per-behaviour (some testing single accessors); hand's are integration-shaped per-scenario. The model-resolver layer (`coding-agent-core-model-resolver.md`) covers the lookup-side parity; this UC anchors the catalog-building side. |

## Notes

`ModelRegistry` is the layer between `AuthStorage` (which providers have credentials) and `model_resolver` (which model the user wants). The two upstream test files (`model-registry.test.ts` + `model-resolver.test.ts`) split this responsibility; hand mirrors the split via `model_registry.rs` + `model_resolver.rs`.
