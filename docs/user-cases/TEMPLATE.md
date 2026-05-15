# User-Cases: <module name>

**Upstream source:** `pi-mono/packages/<package>/test/<file>.test.ts`
**hand-ai source:**   `crates/<crate>/src/<path>/<module>.rs`
**Maintainer note:**  <one-line rationale for this module's surface>

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-<slug>-001 | ⚠️ pending | — |

## Cases

### UC-<slug>-001 — short summary of the behaviour

**Given** the user is in directory `/tmp/x` and a file `foo.txt` exists there.
**When**  the user invokes `<command or API call>`.
**Then**  the observable result is `<exact expected output / file shape / exit code>`.

- Assertion: <one specific claim a probe can check>
- Assertion: <another claim>
- Probe: `cargo test -p <crate> <test-name> -- --exact` covers this case.

### UC-<slug>-002 — next behaviour

**Given** ...
**When**  ...
**Then**  ...

- Assertion: ...
- Probe: ...

<!-- Append new cases at the bottom. Never renumber existing ones. -->
