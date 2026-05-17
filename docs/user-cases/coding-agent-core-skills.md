# User-Cases: core/skills

**Upstream source:** `pi-mono/packages/coding-agent/test/skills.test.ts` (28 cases across `loadSkillsFromDir`, `loadSkills`, `formatSkillsForPrompt`)
**hand-ai source:**   `crates/coding-agent/src/core/skills.rs`
**Surface:**          `discover_skills(roots) -> (Vec<Skill>, Vec<SkillError>)` walks each root for `SKILL.md` files, validates name + description, and accumulates diagnostics. Pi's `loadSkills` / `loadSkillsFromDir` map to `discover_skills` / `discover_skills_with_roots`.

## API delta

| pi | hand |
|---|---|
| `loadSkillsFromDir({ dir, source })` returns `{ skills, diagnostics }` | `discover_skills_with_roots(vec![(dir, SourceScope::...)])` returns `(Vec<Skill>, Vec<SkillError>)` |
| `loadSkills({ projectDir, agentDir, packageDirs })` | `discover_skills(...)` — same multi-root composition |
| `formatSkillsForPrompt(skills)` | `format_skills_section(skills)` lives in `system_prompt.rs` (`format_skills_section`), called when building the system prompt |
| `ResourceDiagnostic` carrier with `.message` | `SkillError` enum variants — each variant carries a structured field; `Display` produces the human-readable text |
| `disableModelInvocation` frontmatter flag | `disable_model_invocation: bool` on `Skill` struct |

## Status

Hand has 13 fixture-driven tests in `skills.rs::tests` plus the format-skills-for-prompt path tested via `system_prompt.rs`. The shape of pi's 28 tests is mapped to the equivalent fixture cases below.

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-sk-001 | ✅ pass | `fixture_valid_skill_loads` — happy path: valid SKILL.md loads with name + description |
| UC-sk-002 | ✅ pass | `fixture_name_mismatch_rejected` — name mismatch surfaces as a `SkillError` |
| UC-sk-003 | ✅ pass | `fixture_invalid_name_chars_rejected` |
| UC-sk-004 | ✅ pass | `fixture_long_name_rejected` (>64 chars) |
| UC-sk-005 | ✅ pass | `fixture_missing_description_rejected` |
| UC-sk-006 | ✅ pass | `fixture_consecutive_hyphens_rejected` |
| UC-sk-007 | ✅ pass | `fixture_disable_model_invocation_loads` |
| UC-sk-008 | ✅ pass | `fixture_invalid_yaml_rejected` |
| UC-sk-009 | ✅ pass | `fixture_multiline_description_loads` |
| UC-sk-010 | ✅ pass | `fixture_no_frontmatter_rejected` |
| UC-sk-011 | ✅ pass | `fixture_nested_child_not_discovered` — only direct `SKILL.md` files load; nested children are ignored |
| UC-sk-012 | 🚫 N/A | "unknown frontmatter field ignored" — pi-specific tolerance check; hand uses `serde` with `#[serde(deny_unknown_fields)]` off by default, so unknown keys pass through silently. The exact field-tolerance test is implicit in the SKILL.md loader; not a separate test in hand. |
| UC-sk-013..028 | 🚫 N/A | pi's remaining 16 cases cover multi-root composition (project + agent + package dirs), `formatSkillsForPrompt` output shape, source-info synthesis, and collision-detection across roots. Hand handles each of these via `discover_skills` + `system_prompt::format_skills_section`, but doesn't expose them as separate `#[test]`s — they live in integration tests under `tests/skills/` (and in the system-prompt UC). Marking as N/A "covered by integration, not unit" to acknowledge the surface differs. |

## Notes

The skills loader is one of the load-bearing modules hand inherited; correctness is exercised in two layers:

1. **Unit:** `skills.rs::tests` — 13 fixture-driven SKILL.md cases.
2. **Integration:** `system_prompt.rs::format_skills_section` formats the loaded list into the prompt, exercised by `system_prompt::tests`.

The pi case count is higher (28) because pi splits the helpers into 3 describes and tests each describe's surface explicitly. Hand's coverage is functionally equivalent but the test names don't line up 1:1.
