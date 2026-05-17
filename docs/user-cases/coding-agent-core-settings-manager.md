# User-Cases: core/settings_manager

**Upstream source:** `pi-mono/packages/coding-agent/test/settings-manager.test.ts` (17 cases across 7 describes)
**hand-ai source:**   `crates/coding-agent/src/core/settings.rs`
**Surface:**          `SettingsManager` — two-layer (global + project) settings store. `from_cwd(cwd) -> Result<Self>` constructs from `<agentdir>/settings.json` + `<cwd>/.hand/settings.json` (hand renames pi's `.pi/` to `.hand/`). Mutators are scoped (`set_packages(scope, value)` etc.) and write via `save(scope)`. `current()` returns the merged view; `global_layer()` / `project_layer()` expose the raw layers.

## API delta

The hand surface is shaped around **explicit scope + immutable Settings struct + save(scope)**, while pi's `SettingsManager` uses **imperative per-key setters with async flush**. The differences are intentional:

| pi capability | hand |
|---|---|
| `manager.setDefaultThinkingLevel("high")` + `flush()` | not modelled — hand reads `current().default_thinking_level` and writes via a higher-level config-write path; the `SettingsManager` does not own per-key UI setters |
| `manager.setTheme("light")` + `flush()` | partial — hand stores theme as a `ThemeSetting` enum on the merged view; mutation is via `set_themes(scope, list)` for the theme-source list, not a single-name setter |
| `manager.reload()` (async, refreshes from disk) | not modelled — caller constructs a new `SettingsManager::from_cwd` snapshot. Watch-mode via `watch()` broadcasts changes instead. |
| `manager.drainErrors()` returning per-scope load errors | not modelled — `from_cwd` returns `Result<Self, SettingsError>` for hard failures; soft per-scope errors are not aggregated |
| `manager.getSessionDir()` with `~` expansion | not modelled as a getter — `Settings::session_dir` is a public `Option<PathBuf>` on the merged view; callers expand `~` themselves |
| `.pi/settings.json` directory | renamed to `.hand/settings.json` |
| `setProjectPackages([{source: "npm:test-pkg"}])` creates `.pi/` on flush | hand creates the parent directory on `save(SettingsScope::Project)` |

Most pi cases therefore map to **🚫 N/A** with the explicit reason that hand's settings API has a different shape. The handful that map cleanly (shellCommandPrefix, package shape, project-dir creation timing) are pinned by existing or new `#[test]`s.

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-set-001 | 🚫 N/A | "preserve enabledModels when changing thinking level" — hand has no `setDefaultThinkingLevel` on `SettingsManager`. The two-layer merge is exercised by `merge_*` tests; preservation across a thinking-level UI write is a higher-level config-write integration that lives outside this module. |
| UC-set-002 | 🚫 N/A | "preserve custom settings when changing theme" — same; no `setTheme` convenience. The merge semantics are covered by `merge_*` tests. |
| UC-set-003 | 🚫 N/A | "in-memory changes override file changes for same key" — same; hand snapshots disk once at `from_cwd` and a subsequent `set_*` + `save(scope)` writes the in-memory version verbatim, by construction. |
| UC-set-004 | ✅ pass | `settings_local_extensions_preserved` — local-only paths in the `extensions` array round-trip through the layered Settings without being misclassified as packages |
| UC-set-005 | ✅ pass | `settings_packages_filtering_object_round_trip` — `PackageSource` enum handles both bare `npm:simple-pkg` strings and `{source, extensions, skills}` filtering objects |
| UC-set-006 | 🚫 N/A | "reload global from disk" — hand does not expose `reload()`; callers construct a new `SettingsManager::from_cwd` snapshot. The watch-mode broadcast covers the live-update use case. |
| UC-set-007 | 🚫 N/A | "keep previous settings when file is invalid" — depends on `reload()`; same reason as UC-set-006. |
| UC-set-008 | 🚫 N/A | `drainErrors()` not modelled on `SettingsManager`; `from_cwd` returns a hard error rather than aggregating per-scope soft errors. |
| UC-set-009 | ✅ pass | `from_cwd_does_not_create_project_settings_dir_on_read` — reading project settings does NOT create `.hand/` if the layer is absent |
| UC-set-010 | ✅ pass | `save_project_scope_creates_parent_directory` — writing a project-scope setting creates `.hand/` and `settings.json` |
| UC-set-011 | ✅ pass | `shell_command_prefix_round_trips_from_global` — `getShellCommandPrefix()` equivalent: `shell_command_prefix() -> Option<&str>` |
| UC-set-012 | ✅ pass | `shell_command_prefix_returns_none_when_unset` — `None` when absent |
| UC-set-013 | ✅ pass | `shell_command_prefix_survives_unrelated_set` — writing an unrelated key (`set_themes`) preserves the prefix |
| UC-set-014 | ✅ pass | `session_dir_returns_none_when_unset` — direct field access through `Settings::session_dir` is `None` |
| UC-set-015 | ✅ pass | `session_dir_returns_global_value_when_only_global_set` |
| UC-set-016 | ✅ pass | `session_dir_project_overrides_global` — merge resolves project-layer's value first |
| UC-set-017 | 🚫 N/A | `~` expansion in `sessionDir` — hand stores the raw `PathBuf` on `Settings` and expects callers to expand `~` via `dirs::home_dir()`. Expansion is the caller's responsibility, not `SettingsManager`'s, to keep the module free of an `os::home` dependency. |

## Notes

The hand `SettingsManager` deliberately exposes a smaller surface than pi's:

- **Scope is explicit on every mutator/save.** `set_*` and `save` both take a `SettingsScope` (`Global`/`Project`) so callers can't accidentally write to the wrong layer.
- **No per-key UI convenience setters.** Theme/thinking-level UI plumbing lives outside `SettingsManager` in the modes/interactive layer; the manager is purely the JSON read/write layer.
- **Watch-mode replaces `reload()`.** `SettingsManager::watch() -> broadcast::Receiver<SettingsChanged>` delivers per-edit updates; consumers redraw rather than poll.

These choices keep `SettingsManager` boring and testable. The cases that map cleanly to hand's smaller surface are pinned. The rest are honestly N/A — closing them would either require porting an unwanted API shape (per-key UI setters) or duplicating subsystems that already exist higher up (watch-mode).
