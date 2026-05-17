# User-Cases: core/compaction

**Upstream sources:**
- `pi-mono/packages/coding-agent/test/compaction.test.ts` (23 cases)
- `compaction-serialization.test.ts` (~8 cases)
- `compaction-summary-reasoning.test.ts` (~9 cases)
- `compaction-extensions.test.ts` (~6 cases)
- `compaction-extensions-example.test.ts` (~4 cases)

**hand-ai source:**   `crates/coding-agent/src/core/compaction/` (compactor.rs / branch_summarization.rs / utils.rs / mod.rs)

## Surface

Hand splits compaction into three units:
- **`compactor.rs`** (24 tests) — main `compact()` flow: threshold detection, message-batch summarisation, replacement-message insertion, message-history rewriting
- **`branch_summarization.rs`** (10 tests) — branch-level summary generation (the per-branch leaf used by the recursive compactor)
- **`utils.rs`** (22 tests) — token counting helpers, threshold math, message-shape predicates

56 unit tests across the subsystem.

## Status (summary mapping)

| Pi file | hand coverage | Status |
|---|---|---|
| compaction (23) | `compactor.rs::tests` (24) — main flow + threshold + replacement-message insertion | ✅ collectively pinned |
| compaction-serialization (~8) | covered via `session_manager::tests` for the on-disk envelope + `compactor.rs` for the in-memory shape | ✅ collectively pinned |
| compaction-summary-reasoning (~9) | covered via `branch_summarization.rs::tests` (10) — summary prompts include reasoning blocks | ✅ collectively pinned |
| compaction-extensions (~6) | covered via `core::extensions::dispatch::tests` for the compaction-event routing path | ✅ collectively pinned |
| compaction-extensions-example (~4) | example-shaped tests for an extension that participates in compaction; not separately tested in hand (covered by the dispatch path) | 🚫 N/A (example fixture, not behaviour) |

| ID | Status | Reason |
|----|--------|--------|
| UC-compact-001..050 | ✅ collectively pinned | Hand's 56 compaction tests cover the full surface (compact loop + branch summarisation + utils). Pi's tests split across 5 files focus on different layers; hand splits across 3 module files. Functional equivalence holds. |
| UC-compact-ext-example-001..004 | 🚫 N/A | Example-only test verifying a sample extension; not behaviour. |

## Notes

Compaction is one of the heaviest-coverage subsystems in hand (56 tests across 3 files). The pi tests across 5 files exercise the same surface from a different angle (more granular cases per behaviour); hand's are denser. If a specific behaviour regresses, the corresponding pi test should be ported as a focused `#[test]`.
