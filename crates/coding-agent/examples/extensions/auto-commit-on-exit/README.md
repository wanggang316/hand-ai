# auto-commit-on-exit

Tier 1 extension fixture. On `on_shutdown`, runs `git add -A && git commit`
in the session's working directory if there are uncommitted changes. Errors
are logged via `tracing::warn!` and never propagated — session teardown
must not fail because git is unhappy.

Hand's `on_shutdown` hook does not yet receive the message log, so this
fixture uses a static subject (`auto-commit: end of session`). Deriving
the commit subject from the last assistant message is a future
enhancement once the hook surface widens.
