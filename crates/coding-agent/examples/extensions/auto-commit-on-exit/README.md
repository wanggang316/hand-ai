# auto-commit-on-exit

Tier 1 extension fixture. On `on_shutdown`, runs `git add -A && git commit`
in the session's working directory if there are uncommitted changes. Errors
are logged via `tracing::warn!` and never propagated — session teardown
must not fail because git is unhappy.

The pi-mono original derives the commit subject from the last assistant
message; hand's `on_shutdown` hook does not yet receive the message log, so
this fixture uses a static subject (`auto-commit: end of session`).

Ported from `pi-mono/.../examples/extensions/auto-commit-on-exit.ts`.
