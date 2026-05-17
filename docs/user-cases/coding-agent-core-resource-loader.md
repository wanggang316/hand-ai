# User-Cases: core/resource_loader

**Upstream source:** `pi-mono/packages/coding-agent/test/resource-loader.test.ts` (19 cases)
**hand-ai source:**   `crates/coding-agent/src/core/skills.rs`, `crates/coding-agent/src/core/extensions/*.rs`, `crates/coding-agent/src/core/prompt_templates.rs`

## API delta

pi exposes a unified `DefaultResourceLoader` that owns discovery for skills + extensions + prompts + themes and supports an async `reload()`. Hand decomposes this into per-kind loaders:

| pi (`DefaultResourceLoader`) | hand equivalent |
|---|---|
| `getSkills()` | `crate::core::skills::discover_skills(...)` |
| `getExtensions()` | `crate::core::extensions::discovery::discover_extensions(...)` |
| `getPrompts()` | `crate::core::prompt_templates::discover_prompts(...)` |
| `getThemes()` | `crate::modes::interactive::theme::discover_themes(...)` |
| `reload()` async | callers reconstruct via the relevant `discover_*` call; no unifying object holds the cached results |

The pi-level "ResourceLoader as a single coordinator with reload semantics" is intentionally not modelled in hand — the per-kind discovery functions are pure and cheap enough that callers re-run them on demand. The `agent_session_services` module ties the per-kind results together at session-create time without a central cache.

## Status

| ID | Status | Reason |
|----|--------|--------|
| UC-rl-001..019 | 🚫 N/A | Architecture-level divergence: hand has no `ResourceLoader` coordinator. The behaviour each pi test exercises (skill discovery, extension loading, prompt scanning, theme listing, agent-dir vs. cwd precedence) is split across the four per-kind modules. The corresponding behaviour is covered by the per-kind UC docs (`coding-agent-core-skills.md`, etc.) and integration tests under `tests/`. |

## Notes

If hand later grows a unifying `ResourceLoader` (e.g. for watch-mode coalescing or for a single `reload()` cycle that re-runs all four discoveries), these 19 cases can be re-opened in a dedicated UC doc.
