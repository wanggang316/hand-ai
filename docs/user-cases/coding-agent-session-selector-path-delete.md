# User-Cases: modes/interactive/session_selector — path delete

**Upstream source:** `pi-mono/packages/coding-agent/test/session-selector-path-delete.test.ts` (7 cases)
**hand-ai source:**   `crates/coding-agent/src/modes/interactive/components/session_selector.rs`
**Surface:**          The session-picker TUI component. The pi tests focus specifically on **path-delete semantics under symlink aliasing** — a session file reached via a directory symlink should still be deleteable, and the picker should not crash when the cwd path appears under multiple aliases.

## Status

| ID | Status | Reason |
|----|--------|--------|
| UC-ssp-001..007 | 🚫 N/A | hand's `SessionSelectorComponent` exists (`modes/interactive/components/session_selector.rs`) but its API surface differs from pi's: hand's delete flow goes through `agent_session_runtime::delete_session(session_path)` rather than a component-level method, and symlink resolution happens at the `path_utils::canonicalize_path` layer (covered by UC-paths-002/003/005). The pi test exercises a tightly coupled `SessionSelectorComponent.deletePath(path)` flow with symlink-aware state that is not modelled the same way in hand. The underlying behaviour (canonical-path comparison) is covered by the path-utils UC. |

## Notes

If hand grows a component-level delete handler (e.g. for keyboard shortcut handling that needs to update the in-memory session list in lockstep with the on-disk delete), these 7 cases can be re-opened as a dedicated UC. The pi test fixtures (`createSymlinkedSessionPaths` setup with `alias-a` / `alias-b` → `real` symlinks) are a good baseline to reuse if and when that happens.
