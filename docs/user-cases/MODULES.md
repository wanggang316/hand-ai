# Module Inventory

Tracks which hand-ai modules have user-case files, the upstream test source,
and current coverage health. Updated as each module's UC file lands.

| Module file | Upstream | hand-ai crate | Case count | Pass | Fail | Pending |
|-------------|----------|---------------|-----------:|-----:|-----:|--------:|
| coding-agent-tools-path-utils.md | pi-mono/packages/coding-agent/test/path-utils.test.ts | coding-agent | 12 | 12 | 0 | 0 |
| coding-agent-tools-file-mutation-queue.md | pi-mono/packages/coding-agent/test/file-mutation-queue.test.ts | coding-agent | 7 | 7 | 0 | 0 |
| coding-agent-tools-find.md | pi-mono/packages/coding-agent/test/tools.test.ts (find describe) | coding-agent | 8 | 7 | 1 | 0 |
| coding-agent-tools-read.md | pi-mono/packages/coding-agent/test/tools.test.ts (read describe) | coding-agent | 11 | 8 | 3 | 0 |
| coding-agent-tools-grep.md | pi-mono/packages/coding-agent/test/tools.test.ts (grep describe) | coding-agent | 6 | 6 | 0 | 0 |
| coding-agent-tools-edit.md | pi-mono/packages/coding-agent/test/tools.test.ts (edit + fuzzy + CRLF describes) | coding-agent | 31 | 10 | 11 | 10 |
| coding-agent-tools-write.md | pi-mono/packages/coding-agent/test/tools.test.ts (write describe) | coding-agent | 5 | 5 | 0 | 0 |
| coding-agent-tools-ls.md | pi-mono/packages/coding-agent/test/tools.test.ts (ls describe) | coding-agent | 5 | 5 | 0 | 0 |
| coding-agent-cli-args.md | pi-mono/packages/coding-agent/test/args.test.ts | coding-agent | 60 | 22 | 30 | 8 |
| coding-agent-core-resolve-config-value.md | pi-mono/packages/coding-agent/test/auth-storage.test.ts (API-key + caching subset) | coding-agent | 16 | 16 | 0 | 0 |
| coding-agent-core-model-resolver.md | pi-mono/packages/coding-agent/test/model-resolver.test.ts | coding-agent | 31 | 11 | 3 | 17 |
| coding-agent-core-auth-storage.md | pi-mono/packages/coding-agent/test/auth-storage.test.ts (oauth/persistence/status/runtime-override) | coding-agent | 11 | 5 | 5 | 1 |
| coding-agent-core-session-manager.md | pi-mono/packages/coding-agent/test/{session-info-modified-timestamp,session-cwd,sdk-session-manager}.test.ts | coding-agent | 7 | 1 | 1 | 5 |
| coding-agent-tools-bash.md | pi-mono/packages/coding-agent/test/tools.test.ts (bash describe) + bash-execution-width.test.ts | coding-agent | 17 | 7 | 5 | 5 |
| coding-agent-tools-render-utils.md | hand parity contract (pi has no dedicated test file) | coding-agent | 12 | 12 | 0 | 0 |
| coding-agent-core-system-prompt.md | pi-mono/packages/coding-agent/test/system-prompt.test.ts | coding-agent | 7 | 3 | 4 | 0 |
| model-stream-retry.md | hand parity contract (pi has no dedicated retry-classification test file) | model | 8 | 8 | 0 | 0 |
| tui-keys.md | pi-mono/packages/tui/test/keys.test.ts | tui | 59 | 59 | 0 | 0 |
| tui-autocomplete.md | pi-mono/packages/tui/test/autocomplete.test.ts | tui | 25 | 1 | 10 | 14 |

A `—` in any column means "not yet measured" — the file hasn't been
authored yet, or the count hasn't been recomputed since the last edit.

## Rollup

- **Total cases authored:** 338
- **Pass:** 212
- **Fail:** 71
- **Pending:** 55

### Known failures (drive remediation)

- **UC-find-002** — hand's find tool does not honour `.gitignore` (no fd
  backing). Resolution: pull in the `ignore` crate or shell out to `fd`.
- ~~UC-grep-002~~ ✅ FIXED — grep schema now accepts `limit` (canonical)
  with `max_matches` as a deprecated alias; the truncation footer
  emits the pi-aligned `[N matches limit reached. ...]` wording when
  the per-file cap is hit.
- **UC-read-001** — hand prepends every output line with `{N>6}→`;
  pi returns raw content. Resolution: drop the prefix at the tool
  surface (or make it opt-in), keep numbering in the TUI renderer.
- ~~UC-read-003/004/006~~ ✅ FIXED — all three truncation footers
  (default 2000-line cap, 50 KB byte cap, user-supplied limit) now
  match pi's wording exactly: `[Showing lines N-M of T. Use offset=M+1
  to continue.]`, `[Showing lines N-M of T (<size> limit). Use
  offset=M+1 to continue.]`, `[K more lines in file. Use offset=M+1
  to continue.]`.
- **UC-read-009** — hand emits no structured `details.truncation`
  metadata; pi populates the side-channel for host consumption.
- **UC-read-010** — hand never detects image MIME via file magic; pi
  emits an image block when bytes match a known header.
- ~~UC-sysp-001~~ ✅ FIXED — emits `Available tools:\n(none)` for empty
  tools and the `Show file paths clearly` guideline is anchored. (And
  UC-sysp-002.)
- **UC-sysp-004/005** — hand has no `tool_snippets` channel; custom
  tools can't be advertised at the protocol level.
- **UC-sysp-006/007** — hand's `custom_guidelines` is a string, not a
  list; no dedup/trim semantics.
- **UC-args-002** — hand binds `-v` to `--verbose`, not `--version`.
- **UC-args-012/013** — `--resume` / `-r` bare (no value) not allowed
  by hand's clap derive.
- **UC-args-026** — `--models <csv>` flag missing.
- **UC-args-028..038** — `--extension`, `--no-extensions`, `--skill`,
  `--prompt-template`, `--theme` (and their `-e` short forms) all
  missing.
- **UC-args-040/041** — `--no-prompt-templates` / `--no-themes` missing.
- **UC-args-043** — `-nc` shorthand missing.
- **UC-args-047** — `-nt` shorthand missing.
- **UC-args-048/049** — `--no-builtin-tools` / `-nbt` missing.
- **UC-args-051** — `-t` shorthand missing.
- **UC-args-054** — positional args bind to a single `prompt`, not a
  `messages: Vec<String>`.
- **UC-args-055/056** — `@<path>` arg recognition missing.
- **UC-args-057..059** — unknown-flag capture (instead of parse error)
  missing.
- **UC-mr-027/028/029** — default model lookups (`openai`, `zai`,
  `minimax`, `cerebras`, `vercel-ai-gateway`) likely drift from pi's
  current values; needs a snapshot-equality test.
- **UC-as-001** — no `get_api_key` async with OAuth refresh + lock
  compromise recovery on `AuthStorage`.
- **UC-as-005..008** — no `reload`/`drain_errors`, no `get_auth_status`
  redactor, no runtime-override layer.

## Phase 1 complete — suite breadth landed

All 18 originally-scheduled modules now have a `.md` file in
`docs/user-cases/`. Phase 2 splits into two parallel tracks:

### Track A: Phase-2 module breadth (remaining pi suites)

Smaller pi test files not yet translated. Each is < 20 cases.

- `compaction*.test.ts` (compaction core, extensions, serialization,
  summary-reasoning) — ~50 cases together
- `extensions-*.test.ts` (discovery, runner, input-event) — ~62 cases
- `settings-manager*.test.ts` — ~18 cases
- `session-selector-*.test.ts` — ~19 cases (tui interaction)
- `agent-session-*.test.ts` — ~30 cases (concurrency, branching,
  retry, runtime events)
- `package-manager*.test.ts` — ~103 cases
- `prompt-templates.test.ts`, `resource-loader.test.ts`,
  `skills.test.ts`, `frontmatter.test.ts` — runtime-asset loaders
- `interactive-mode-*.test.ts` — TUI driver cases
- Various smaller files (rpc-*, paths, plan-mode-utils, theme-export,
  initial-message, version-check, etc.)

### Track B: ❌ remediation

The 77 failing UC items already enumerated under "Known failures"
above drive concrete fixes:

1. **find/.gitignore** (UC-find-002) — switch to `ignore::WalkBuilder`
2. **grep API alignment** (UC-grep-002) — rename `max_matches`→`limit`
3. **read output format** (UC-read-001/003/004/006/009/010) — drop
   line-number prefix; align truncation wording; add structured
   `details.truncation`; add image-magic detection
4. **system_prompt** (UC-sysp-001/004..007) — add `tool_snippets` map,
   switch `custom_guidelines` to dedup'd list, emit `(none)` placeholder
5. **cli/args** (UC-args-002/012/013/026/028..057) — many small clap
   adjustments; biggest is unifying positional `prompt` →
   `messages: Vec<String>` and adding `@<file>` recognition + the
   `--extension/--skill/--prompt-template/--theme` family
6. **bash full-output persistence** (UC-bash-004/015/016) — persist
   truncated payloads to tempfile, surface path in footer + `details`
7. **bash command_prefix** (UC-bash-008/009) — add config option
8. **auth_storage** (UC-as-001/005..008) — add `get_api_key` async
   with OAuth refresh + lock recovery, `reload`+`drain_errors`,
   `get_auth_status` redactor, runtime-override layer
9. **tools/edit edits array** (UC-edit-005..010, 025, 031) — schema
   change to support multi-edit atomicity
10. **autocomplete fd parity** (UC-ac-* cluster) — `ignore::WalkBuilder`
    with symlink follow, quoted-path support
11. **model_resolver default-table drift** (UC-mr-027/028/029) —
    snapshot-equality test against pi's `defaultModelPerProvider`

Each remediation item is scoped small enough to be one commit. The
loop runs them one at a time, re-verifies the affected user-cases
flip from ❌ to ✅, and commits.
