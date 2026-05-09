# dirty-repo-guard

Tier 1 extension fixture. Blocks `write` / `edit` tool calls when the
session's working directory has uncommitted git changes.

The pi-mono original hooks `session_before_switch` / `session_before_fork`,
which hand's extension API does not expose yet. We approximate the same
intent at the closest hook hand does provide (`before_tool_call`), guarding
the destructive built-in tools.

## Behaviour

- Tool not in `{write, edit}` → `Continue`.
- `git status --porcelain` fails (not a repo, git missing) → `Continue`.
- Stdout non-empty → `Cancel("dirty repo: commit or stash before editing")`.

Ported from `pi-mono/.../examples/extensions/dirty-repo-guard.ts`.
