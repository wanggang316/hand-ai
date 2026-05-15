# Module Inventory

Tracks which hand-ai modules have user-case files, the upstream test source,
and current coverage health. Updated as each module's UC file lands.

| Module file | Upstream | hand-ai crate | Case count | Pass | Fail | Pending |
|-------------|----------|---------------|-----------:|-----:|-----:|--------:|
| coding-agent-tools-path-utils.md | pi-mono/packages/coding-agent/test/path-utils.test.ts | coding-agent | 12 | 12 | 0 | 0 |
| coding-agent-tools-file-mutation-queue.md | pi-mono/packages/coding-agent/test/file-mutation-queue.test.ts | coding-agent | 7 | 7 | 0 | 0 |
| coding-agent-tools-find.md | pi-mono/packages/coding-agent/test/tools.test.ts (find describe) | coding-agent | 8 | 7 | 1 | 0 |
| coding-agent-tools-read.md | pi-mono/packages/coding-agent/test/read-tool.test.ts (et al) | coding-agent | — | — | — | — |
| coding-agent-tools-grep.md | pi-mono/packages/coding-agent/test/tools.test.ts (grep describe) | coding-agent | 6 | 5 | 1 | 0 |
| coding-agent-tools-edit.md | pi-mono/packages/coding-agent/test/edit-tool*.test.ts | coding-agent | — | — | — | — |
| coding-agent-tools-write.md | pi-mono/packages/coding-agent/test/write-tool*.test.ts | coding-agent | — | — | — | — |
| coding-agent-tools-ls.md | pi-mono/packages/coding-agent/test/tools.test.ts (ls describe) | coding-agent | 5 | 5 | 0 | 0 |
| coding-agent-cli-args.md | pi-mono/packages/coding-agent/test/args.test.ts | coding-agent | — | — | — | — |
| coding-agent-core-resolve-config-value.md | pi-mono/packages/coding-agent/test/auth-storage.test.ts (subset) | coding-agent | — | — | — | — |
| coding-agent-core-model-resolver.md | pi-mono/packages/coding-agent/test/model-resolver.test.ts | coding-agent | — | — | — | — |
| coding-agent-core-auth-storage.md | pi-mono/packages/coding-agent/test/auth-storage.test.ts | coding-agent | — | — | — | — |
| coding-agent-core-session-manager.md | pi-mono/packages/coding-agent/test/session-*.test.ts | coding-agent | — | — | — | — |
| coding-agent-core-bash-executor.md | pi-mono/packages/coding-agent/test/bash-*.test.ts | coding-agent | — | — | — | — |
| coding-agent-tools-render-utils.md | pi-mono/packages/coding-agent/test/render-*.test.ts | coding-agent | — | — | — | — |
| coding-agent-core-system-prompt.md | pi-mono/packages/coding-agent/test/system-prompt*.test.ts | coding-agent | — | — | — | — |
| model-stream-retry.md | pi-mono/packages/ai/test/retry-*.test.ts (et al) | model | — | — | — | — |
| tui-keys.md | pi-mono/packages/tui/test/keys*.test.ts | tui | — | — | — | — |
| tui-autocomplete.md | pi-mono/packages/tui/test/autocomplete*.test.ts | tui | — | — | — | — |

A `—` in any column means "not yet measured" — the file hasn't been
authored yet, or the count hasn't been recomputed since the last edit.

## Rollup

- **Total cases authored:** 38
- **Pass:** 36
- **Fail:** 2
- **Pending:** 0

### Known failures (drive remediation)

- **UC-find-002** — hand's find tool does not honour `.gitignore` (no fd
  backing). Resolution: pull in the `ignore` crate or shell out to `fd`.
- **UC-grep-002** — hand exposes `max_matches` instead of `limit`; the
  truncation footer wording differs from pi. Resolution: align schema
  + footer text.
