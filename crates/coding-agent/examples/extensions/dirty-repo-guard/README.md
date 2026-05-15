# dirty-repo-guard

Tier 1 extension fixture. Blocks `write` / `edit` tool calls when the
session's working directory has uncommitted git changes.

Hand's extension API does not yet expose `session_before_switch` /
`session_before_fork`, so this fixture approximates the same intent at
the closest available hook (`before_tool_call`), guarding the
destructive built-in tools.

## Behaviour

- Tool not in `{write, edit}` → `Continue`.
- `git status --porcelain` fails (not a repo, git missing) → `Continue`.
- Stdout non-empty → `Cancel("dirty repo: commit or stash before editing")`.
