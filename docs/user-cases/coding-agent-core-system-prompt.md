# User-Cases: core/system_prompt

**Upstream source:** `pi-mono/packages/coding-agent/test/system-prompt.test.ts`
**hand-ai source:**   `crates/coding-agent/src/core/system_prompt.rs`
**Surface:**          `build_system_prompt(options)` — assembles the
system prompt the model sees on every turn. Composition rules: empty
tool list shows `(none)`; default tools listed with their snippets;
custom tools rendered when `prompt_snippet` is provided; user
`prompt_guidelines` appended (deduplicated, trimmed) below the default
guidelines; `custom_prompt` replaces the whole body but `custom_guidelines`
still appends.

## API delta

pi's `BuildSystemPromptOptions` carries:
- `selectedTools: string[]` and a separate `toolSnippets: Record<string, string>`
- `promptGuidelines: string[]` (list, dedup + trim semantics)
- `contextFiles: string[]`
- `skills: Skill[]`
- `cwd: string`

hand's `BuildSystemPromptOptions` carries:
- `tools: &[String]` — name-only list, no per-tool snippet override
- `custom_guidelines: Option<&str>` — single string, not a deduplicated
  list
- `context_files: Vec<String>` ✓
- `skills: &[Skill]` ✓
- `cwd: &Path` ✓
- `custom_prompt: Option<&str>` ✓

The two architectural deltas — no `tool_snippets` map and no
list-of-strings `prompt_guidelines` with dedup — drive failures
UC-sysp-004..007.

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-sysp-001 | ✅ pass | `empty_tools_emits_none_placeholder` |
| UC-sysp-002 | ✅ pass | `show_file_paths_guideline_always_present` |
| UC-sysp-003 | ✅ pass | `test_build_system_prompt_basic`, `test_tool_guidelines_generated` |
| UC-sysp-004 | ✅ pass | `tool_snippets_render_custom_tool_with_description` |
| UC-sysp-005 | ✅ pass | `tool_snippets_absent_falls_back_to_bare_name_listing` |
| UC-sysp-006 | ✅ pass | `append_system_prompt_entries_render_as_separate_bullets` |
| UC-sysp-007 | ✅ pass | `append_system_prompt_dedups_and_trims` |

## Cases

### UC-sysp-001 — empty tools list emits `Available tools:\n(none)`

**Given** `build_system_prompt` called with an empty `tools` slice and
default everything else.
**When**  the prompt is assembled.
**Then**  the prompt contains the literal substring
`Available tools:\n(none)`.

- Assertion: the prompt text contains `Available tools:\n(none)`.
- Probe (FAILS today): hand suppresses the Available-tools section when
  the slice is empty (or emits a different wording). A fresh probe
  would not find the substring.
- Resolution proposal: always emit the section, with `(none)` as the
  placeholder when no tools are selected.

### UC-sysp-002 — pending: the "Show file paths clearly" guideline is always present

**Given** `build_system_prompt` called with any options (even empty
tools).
**When**  the prompt is assembled.
**Then**  the prompt contains the substring `Show file paths clearly`
— a guideline anchored to user-experience consistency regardless of
tool set.

- Assertion: the prompt contains `Show file paths clearly`.
- Probe: not currently in hand's template; need to verify by reading
  the generated prompt body.
- Status note: pending until I confirm whether hand has an equivalent
  guideline under a different wording (e.g. "include file paths in
  responses"). If equivalent, this case resolves to ✅ with the
  wording aligned.

### UC-sysp-003 — default tool names appear in the Available-tools section

**Given** `tools: ["read", "bash", "edit", "write"]`.
**When**  the prompt is assembled.
**Then**  the prompt contains lines beginning with `- read:`, `- bash:`,
`- edit:`, `- write:` (with colon-prefixed descriptions following each
tool name).

- Assertion: the prompt contains `- read:` (and one per tool).
- Probe: `cargo test -p hand-coding-agent test_build_system_prompt_basic test_tool_guidelines_generated -- --exact`.

### UC-sysp-004 — custom tools render in the Available-tools section when a snippet is supplied

**Given** `selectedTools: ["read", "dynamic_tool"]` and
`toolSnippets: { dynamic_tool: "Run dynamic test behavior" }`.
**When**  the prompt is assembled.
**Then**  the prompt contains
`- dynamic_tool: Run dynamic test behavior`.

- Assertion: the prompt contains the literal line above.
- Probe (FAILS today): hand has no `tool_snippets` parameter. A custom
  tool name lands in the prompt only if hand's hard-coded template
  knows about it.
- Resolution proposal: add an optional
  `tool_snippets: HashMap<String, String>` field to
  `BuildSystemPromptOptions` and render the listed tools using
  `format!("- {name}: {snippet}")`. Falls back to the existing
  template entry when no snippet is supplied.

### UC-sysp-005 — custom tools are omitted from the Available-tools section when no snippet is supplied

**Given** `selectedTools: ["read", "dynamic_tool"]` and no `toolSnippets`.
**When**  the prompt is assembled.
**Then**  the prompt does NOT contain `dynamic_tool` anywhere.

- Assertion: the prompt does NOT contain the substring `dynamic_tool`.
- Probe (FAILS today): blocked on UC-sysp-004 — without a snippet
  channel, "custom tool without snippet" is not a distinguishable
  state for hand to omit.

### UC-sysp-006 — `prompt_guidelines` entries are appended below the default guidelines

**Given** `selectedTools: ["read", "dynamic_tool"]` and
`promptGuidelines: ["Use dynamic_tool for project summaries."]`.
**When**  the prompt is assembled.
**Then**  the prompt contains
`- Use dynamic_tool for project summaries.` as a bulleted line in the
guidelines section.

- Assertion: the prompt contains the literal bullet above.
- Probe (FAILS today): hand's `custom_guidelines` accepts a single
  string and dumps it under a `# Project Guidelines` header verbatim,
  not bulleted.
- Resolution proposal: switch `custom_guidelines` to a `Vec<String>`
  (or `Option<&[&str]>`), emit each entry as `- {entry}`, and apply
  dedup+trim semantics described in UC-sysp-007.

### UC-sysp-007 — `prompt_guidelines` entries are deduplicated and whitespace-trimmed

**Given** `promptGuidelines` with three entries:
`["Use dynamic_tool for summaries.", "  Use dynamic_tool for summaries.  ", "   "]`.
**When**  the prompt is assembled.
**Then**  the prompt contains exactly one bulleted line
`- Use dynamic_tool for summaries.` — duplicates collapsed; the
whitespace-only third entry dropped entirely.

- Assertion: the count of `- Use dynamic_tool for summaries.` lines in
  the prompt equals 1.
- Probe (FAILS today): blocked on UC-sysp-006.
- Resolution proposal: after switching to a list, dedup by trimmed
  value; drop empty entries.
