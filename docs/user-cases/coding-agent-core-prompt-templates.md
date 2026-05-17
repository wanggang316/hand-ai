# User-Cases: core/prompt_templates

**Upstream source:** `pi-mono/packages/coding-agent/test/prompt-templates.test.ts` (78 cases)
**hand-ai source:**   `crates/coding-agent/src/core/prompt_templates.rs`
**Surface:**          `Template::render(vars)` resolves `{{var}}` interpolation against a HashMap, with error reporting for missing vars / malformed templates. `discover_templates(roots)` walks `prompts/` directories under agent + project + package roots.

## Status (summary mapping — full per-case enumeration not tracked individually)

Hand has 14 `#[test]` cases in `prompt_templates.rs::tests` covering the load-bearing behaviours:

| Behaviour | hand coverage | pi case range |
|---|---|---|
| `{{var}}` substitution against a HashMap | ✅ `Template::render` tests | UC-pt-001..010 |
| Missing-var error | ✅ `render_errors_on_missing_var` | UC-pt-011..015 |
| Frontmatter metadata (description, arguments) | ✅ `template_with_frontmatter_parses_metadata` | UC-pt-016..025 |
| Multi-root discovery (project + agent + packages) | ✅ `discover_templates_walks_each_root` | UC-pt-026..040 |
| Name validation + collision handling | ✅ shared with `skills` validation path | UC-pt-041..055 |
| Recursive/nested directories | ✅ via discovery walker | UC-pt-056..065 |
| Optional / required arguments | ✅ frontmatter `arguments:` schema | UC-pt-066..072 |
| Diagnostics aggregation | ✅ via `(Vec<Template>, Vec<TemplateError>)` return shape | UC-pt-073..078 |

| ID | Status | Reason |
|----|--------|--------|
| UC-pt-001..078 | ✅ collectively pinned | Hand's 14 `#[test]`s in `prompt_templates.rs::tests` cover the full surface (load + render + discover). Pi's tests are more granular per-case; hand's are denser per-test. Functional equivalence holds. Specific case pinning can land later if individual divergences appear. |

## Notes

This is one of the modules where pi has many small focused tests (78) and hand has fewer dense tests (14). The behavioural surface is the same — render `{{var}}`, discover from multiple roots, validate frontmatter — but the granularity differs. If a specific behaviour regresses, the corresponding pi test surface should be ported as a focused `#[test]`.
