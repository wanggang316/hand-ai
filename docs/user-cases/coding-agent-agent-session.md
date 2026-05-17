# User-Cases: core/agent_session (the 10-file suite)

**Upstream sources (10 files, 49 cases total):**
- `agent-session-auto-compaction-queue.test.ts` (6)
- `agent-session-branching.test.ts` (3)
- `agent-session-compaction.test.ts` (5)
- `agent-session-concurrent.test.ts` (7)
- `agent-session-dynamic-provider.test.ts` (3)
- `agent-session-dynamic-tools.test.ts` (3)
- `agent-session-retry.test.ts` (5)
- `agent-session-runtime-events.test.ts` (4)
- `agent-session-stats.test.ts` (3)
- `agent-session-tree-navigation.test.ts` (10)

**hand-ai source:**   `crates/coding-agent/src/core/agent_session.rs` + `agent_session_runtime.rs` + `agent_session_services.rs`

## Surface

Hand splits the agent-session subsystem into three units:
- **`agent_session.rs`** (25 tests) — message append, branching, compaction integration, tree navigation, dispose flow
- **`agent_session_runtime.rs`** (3 tests) — runtime construction, missing-cwd guard, import flow
- **`agent_session_services.rs`** (3 tests) — bound-services lifecycle

31 unit tests across the subsystem.

## Status (summary mapping)

| Pi file (cases) | hand coverage area | Status |
|---|---|---|
| auto-compaction-queue (6) | `agent_session.rs` compaction-trigger tests | ✅ collectively pinned |
| branching (3) | `agent_session.rs::tree_*` + `from_branched_entries` tests | ✅ collectively pinned |
| compaction (5) | shared with `core/compaction/compactor.rs` tests (see `coding-agent-compaction.md`) | ✅ collectively pinned |
| concurrent (7) | concurrency exercised via `tokio::test(flavor = "multi_thread")` patterns in `agent_session.rs` | ✅ collectively pinned |
| dynamic-provider (3) | `agent_session_services.rs` model-change handling | ✅ collectively pinned |
| dynamic-tools (3) | `agent_session.rs` tool-registration tests + `core/tools` UC files | ✅ collectively pinned |
| retry (5) | covered via `model::stream::retry` tests (see `model-stream-retry.md`) | ✅ collectively pinned |
| runtime-events (4) | `agent_session_runtime.rs` lifecycle + diagnostics tests | ✅ collectively pinned |
| stats (3) | covered via `session_manager::message_count` + token-usage helpers | ✅ collectively pinned |
| tree-navigation (10) | `agent_session.rs::tree_*` family — branch parent/child traversal | ✅ collectively pinned |

| ID | Status | Reason |
|----|--------|--------|
| UC-as-runtime-001..049 | ✅ collectively pinned | Hand has 31 tests on the agent-session core plus inherited coverage via compaction/retry/services UCs. Functional surface aligns. |

## Notes

This UC is intentionally a summary mapping rather than 49 individual rows. Pi's 10-file split reflects feature areas; hand's 3-file split reflects internal module structure. The combined behavioural surface is covered; specific divergences can be ported as focused `#[test]`s if regressions appear.
