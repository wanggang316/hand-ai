# User-Case Suite

User-observable behavioural acceptance criteria for `hand` (Rust workspace).
Each file in this directory translates the upstream TypeScript test cases
into Given/When/Then assertions phrased from the user's perspective. A case
passes when `hand` produces the same observable behaviour the user would see
running the upstream tool against the same inputs.

## Why user-cases (not unit tests)

Unit tests are tied to internal API shape. User-cases are pinned to the
*surface* a user touches — CLI flags, file outputs, terminal rendering,
session JSONL, tool inputs/outputs. They keep us honest about behavioural
parity even when we refactor the Rust internals freely.

## Layout

```
docs/user-cases/
  README.md                  ← this file
  TEMPLATE.md                ← copy when starting a new module
  <module>.md                ← one file per module
```

Module file naming convention: `<crate>-<module-path>.md`, e.g.
`coding-agent-tools-path-utils.md`. The crate prefix lets a reader find the
implementation by `crates/<crate>/src/<module-path>/`.

## Case format

Each case is one block of:

```
### UC-<module>-<NNN> — <one-line summary>

**Given** <preconditions / fixtures>
**When**  <user action / API call / CLI invocation>
**Then**  <observable outcome the user would verify>

- Assertion: <specific testable claim>
- Assertion: <another claim>
- Probe: <how a fresh validator should observe the outcome>
```

- `<module>` is a short slug matching the file (e.g. `path-utils`).
- `<NNN>` is a three-digit zero-padded sequence, unique within the file.
  IDs are stable — never renumber. Append-only.
- Each `Probe:` describes the cheapest observable check: a CLI command, a
  file-on-disk shape, a printed line, an exit code. Probes must NOT read
  Rust internals — the validator agent is forbidden from doing that.
- One case captures one assertion cluster. If two outcomes are independent,
  split into two cases.

## Status rolling-up

Each module file carries a status table at the top:

```
| ID | Status | Verified-by |
|----|--------|-------------|
| UC-path-utils-001 | ✅ pass | crates/coding-agent/src/tools/path_utils.rs (tests) |
| UC-path-utils-002 | ❌ fail | issue: <note>                                       |
| UC-path-utils-003 | ⚠️  pending | not yet probed                                |
```

`Status` is one of: `✅ pass`, `❌ fail`, `⚠️ pending`, `🚫 N/A`.

## Workflow

1. Read the upstream test file in `pi-mono/packages/*/test/*.test.ts`.
2. For each `it("...", ...)` block, write one UC-*-NNN case here.
3. Run `cargo test` against existing hand coverage to see which IDs
   already pass; mark them `✅ pass` with the file/path that covers them.
4. Hand pending cases to `hs-validate-runtime` for behavioural probes.
5. Failing probes drive new Rust tests or implementation fixes.
6. Loop until every case is `✅ pass` or `🚫 N/A` (with justification).

## Reference index

Upstream test source: `/Users/wanggang/dev/opensource/pi-mono/packages/`.
hand-ai source: `crates/<crate>/src/`.
