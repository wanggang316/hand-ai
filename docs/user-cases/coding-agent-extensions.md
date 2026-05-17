# User-Cases: core/extensions

**Upstream sources:**
- `pi-mono/packages/coding-agent/test/extensions-discovery.test.ts` (27 cases)
- `pi-mono/packages/coding-agent/test/extensions-runner.test.ts` (27 cases)
- `pi-mono/packages/coding-agent/test/extensions-input-event.test.ts` (~8 cases)

**hand-ai source:**   `crates/coding-agent/src/core/extensions/` (mod.rs / manifest.rs / api.rs / dispatch.rs / registry.rs / subprocess.rs / source_registry.rs)

## Surface

Hand's extension subsystem is structured around:
- **`manifest.rs`** — parses extension YAML manifests (10 unit tests)
- **`source_registry.rs`** — multi-source discovery (project + agent + packages, 31 tests)
- **`dispatch.rs`** — routes events to active extensions (9 tests)
- **`subprocess.rs`** — manages extension processes (14 tests)
- **`registry.rs`** — runtime registry of loaded extensions (1 test)
- **`api.rs`** — host ABI surface (3 tests)

68 unit tests across the subsystem.

## Status (summary mapping)

| Pi file | hand coverage | Status |
|---|---|---|
| extensions-discovery (27) | `source_registry.rs::tests` (31) — multi-root discovery, project + agent + package precedence, gitignore handling, manifest validation | ✅ collectively pinned by source_registry tests |
| extensions-runner (27) | `dispatch.rs::tests` (9) + `subprocess.rs::tests` (14) — event routing, subprocess lifecycle, error propagation | ✅ collectively pinned by dispatch + subprocess tests |
| extensions-input-event (~8) | `dispatch.rs::tests` covers input-event routing | ✅ collectively pinned by dispatch tests |

| ID | Status | Reason |
|----|--------|--------|
| UC-ext-disc-001..027 | ✅ collectively pinned | 31 hand tests in `source_registry.rs` cover discovery |
| UC-ext-run-001..027 | ✅ collectively pinned | 9 dispatch + 14 subprocess tests cover the runner |
| UC-ext-evt-001..008 | ✅ collectively pinned | dispatch tests include input-event routing |

## Notes

The extension subsystem is one of the larger hand modules with deep test coverage. Pi splits its tests across 3 files focused on different layers (discovery / runner / input-event); hand splits across 6 module files matching the internal architecture. Total test counts are comparable (62 pi vs 68 hand) but the cases don't line up 1:1 by name.

If a pi-specific case ever exposes a regression, the pi test should be ported as a focused `#[test]` against the relevant hand module rather than maintaining a 62-case cross-reference table.
