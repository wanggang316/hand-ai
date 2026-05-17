# User-Cases: plan-mode-utils

**Upstream source:** `pi-mono/packages/coding-agent/test/plan-mode-utils.test.ts` (33 cases)
**hand-ai source:**   N/A — plan mode is not yet ported.

## Status

All 33 cases are 🚫 N/A: pi's plan-mode utilities (`cleanStepText`, `extractDoneSteps`, `extractTodoItems`, etc.) operate on a plan-mode message format that hand does not yet emit or parse. The feature is tracked as a separate parity item; until plan mode lands, the parsing helpers have nothing to consume.

| ID | Status | Reason |
|----|--------|--------|
| UC-plan-001..033 | 🚫 N/A | hand has no `plan_mode` module (`grep -r "plan_mode" crates/` is empty). Plan-mode is a multi-component feature (UI mode, message templates, step extraction, history tracking) that requires a dedicated port. Re-open these cases when the feature ships. |

## Notes

Plan mode is a distinct UX surface (Shift+Tab to enter a "planning" mode where the agent emits structured `## Done`/`## TODO` blocks). Porting it means more than just the parsing helpers — the agent prompts, the TUI mode banner, the message-format conventions, and the history-merging logic all need to come along. Filing as a single architectural-divergence N/A bucket until the feature exists.
