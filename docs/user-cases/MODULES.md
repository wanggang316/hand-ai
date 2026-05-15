# Module Inventory

Tracks which hand-ai modules have user-case files, the upstream test source,
and current coverage health. Updated as each module's UC file lands.

| Module file | Upstream | hand-ai crate | Case count | Pass | Fail | Pending |
|-------------|----------|---------------|-----------:|-----:|-----:|--------:|
| coding-agent-tools-path-utils.md | pi-mono/packages/coding-agent/test/path-utils.test.ts | coding-agent | 12 | 12 | 0 | 0 |
| coding-agent-tools-file-mutation-queue.md | pi-mono/packages/coding-agent/test/file-mutation-queue.test.ts | coding-agent | 7 | 7 | 0 | 0 |
| coding-agent-tools-find.md | pi-mono/packages/coding-agent/test/tools.test.ts (find describe) | coding-agent | 8 | 7 | 1 | 0 |
| coding-agent-tools-read.md | pi-mono/packages/coding-agent/test/tools.test.ts (read describe) | coding-agent | 11 | 5 | 6 | 0 |
| coding-agent-tools-grep.md | pi-mono/packages/coding-agent/test/tools.test.ts (grep describe) | coding-agent | 6 | 5 | 1 | 0 |
| coding-agent-tools-edit.md | pi-mono/packages/coding-agent/test/edit-tool*.test.ts | coding-agent | — | — | — | — |
| coding-agent-tools-write.md | pi-mono/packages/coding-agent/test/tools.test.ts (write describe) | coding-agent | 5 | 5 | 0 | 0 |
| coding-agent-tools-ls.md | pi-mono/packages/coding-agent/test/tools.test.ts (ls describe) | coding-agent | 5 | 5 | 0 | 0 |
| coding-agent-cli-args.md | pi-mono/packages/coding-agent/test/args.test.ts | coding-agent | 60 | 22 | 30 | 8 |
| coding-agent-core-resolve-config-value.md | pi-mono/packages/coding-agent/test/auth-storage.test.ts (API-key + caching subset) | coding-agent | 16 | 16 | 0 | 0 |
| coding-agent-core-model-resolver.md | pi-mono/packages/coding-agent/test/model-resolver.test.ts | coding-agent | 31 | 11 | 3 | 17 |
| coding-agent-core-auth-storage.md | pi-mono/packages/coding-agent/test/auth-storage.test.ts (oauth/persistence/status/runtime-override) | coding-agent | 11 | 5 | 5 | 1 |
| coding-agent-core-session-manager.md | pi-mono/packages/coding-agent/test/session-*.test.ts | coding-agent | — | — | — | — |
| coding-agent-core-bash-executor.md | pi-mono/packages/coding-agent/test/bash-*.test.ts | coding-agent | — | — | — | — |
| coding-agent-tools-render-utils.md | hand parity contract (pi has no dedicated test file) | coding-agent | 12 | 12 | 0 | 0 |
| coding-agent-core-system-prompt.md | pi-mono/packages/coding-agent/test/system-prompt.test.ts | coding-agent | 7 | 1 | 5 | 1 |
| model-stream-retry.md | pi-mono/packages/ai/test/retry-*.test.ts (et al) | model | — | — | — | — |
| tui-keys.md | pi-mono/packages/tui/test/keys*.test.ts | tui | — | — | — | — |
| tui-autocomplete.md | pi-mono/packages/tui/test/autocomplete*.test.ts | tui | — | — | — | — |

A `—` in any column means "not yet measured" — the file hasn't been
authored yet, or the count hasn't been recomputed since the last edit.

## Rollup

- **Total cases authored:** 191
- **Pass:** 113
- **Fail:** 51
- **Pending:** 27

### Known failures (drive remediation)

- **UC-find-002** — hand's find tool does not honour `.gitignore` (no fd
  backing). Resolution: pull in the `ignore` crate or shell out to `fd`.
- **UC-grep-002** — hand exposes `max_matches` instead of `limit`; the
  truncation footer wording differs from pi. Resolution: align schema
  + footer text.
- **UC-read-001** — hand prepends every output line with `{N>6}→`;
  pi returns raw content. Resolution: drop the prefix at the tool
  surface (or make it opt-in), keep numbering in the TUI renderer.
- **UC-read-003** — hand's truncation footer wording differs from pi.
  Resolution: replace the template with pi's
  `[Showing lines 1-N of T. Use offset=N+1 to continue.]`.
- **UC-read-004** — hand's byte-limit footer wording differs (`50.0KB
  byte limit` vs pi's `(<size> limit)`).
- **UC-read-006** — hand uses one truncation footer for both
  default-cap and user-limit truncation; pi has a distinct
  `[N more lines in file. Use offset=M to continue.]` for the
  user-limit case.
- **UC-read-009** — hand emits no structured `details.truncation`
  metadata; pi populates the side-channel for host consumption.
- **UC-read-010** — hand never detects image MIME via file magic; pi
  emits an image block when bytes match a known header.
- **UC-sysp-001** — hand suppresses the Available-tools section when
  the tools slice is empty; pi emits `(none)`.
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

## Next-batch backlog

Authored ordered by ascending case count so the suite breadths first.
Each batch ends with a passing build + a commit + a MODULES update.

1. **cli/args** (~18 cases) — `pi-mono/.../test/args.test.ts`
2. **bash_executor** — `bash-execution-width.test.ts` + the `bash tool`
   subset of `tools.test.ts` (~16 cases together)
3. **model_resolver** (~33 cases) — `model-resolver.test.ts`
4. **auth_storage** (~6 cases) — `auth-storage.test.ts`
5. **resolve_config_value** (~8 cases) — derived from
   `auth-storage.test.ts` `!command` subset + the dedicated test file
6. **tools/edit** (~31 cases) — `tools.test.ts` edit-tool + edit-tool
   fuzzy matching + edit-tool CRLF describes; `edit-tool-legacy-input`,
   `edit-tool-no-full-redraw`
7. **bash tool** (~16 cases) — `tools.test.ts` bash-tool describe
8. **session_manager** (~26 cases) — `session-*.test.ts` family
9. **stream / retry** — `pi-mono/.../packages/ai/test/retry-*.test.ts`
10. **tui/keys** — `pi-mono/.../packages/tui/test/keys*.test.ts`
11. **tui/autocomplete** — `pi-mono/.../packages/tui/test/autocomplete*.test.ts`

After the suite breadth is complete (every module has a `.md` file),
the failing UC cluster drives a remediation milestone:

- gitignore-aware find tool (UC-find-002)
- grep API alignment (UC-grep-002)
- read tool output format alignment (UC-read-001..010)
- system_prompt API surface alignment (UC-sysp-*)
