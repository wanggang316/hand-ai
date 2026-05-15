# User-Cases: tools/file_mutation_queue

**Upstream source:** `pi-mono/packages/coding-agent/test/file-mutation-queue.test.ts`
**hand-ai source:**   `crates/coding-agent/src/tools/file_mutation_queue.rs`
**Surface:**          `with_file_mutation_queue(path, fut)` — every write
and edit operation must funnel through this queue so the agent can't race
itself when the model fires two parallel `tool_use` blocks that touch the
same file. Canonical-path keying ensures symlink aliases share one mutex.

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-fmq-001 | ✅ pass | `same_path_serialises_concurrent_callers` |
| UC-fmq-002 | ✅ pass | `different_paths_run_in_parallel` |
| UC-fmq-003 | ✅ pass | `same_path_via_symlink_shares_queue` (cfg unix) |
| UC-fmq-004 | ✅ pass | `returns_closure_value` |
| UC-fmq-005 | ✅ pass | `missing_file_uses_literal_key` |
| UC-fmq-006 | ✅ pass | `test_edit_serialises_concurrent_calls_to_same_file` (in `tools/edit.rs`) |
| UC-fmq-007 | ✅ pass | `test_edit_and_write_share_mutation_queue` (in `tools/edit.rs`) |

## Cases

### UC-fmq-001 — two callers for the same path serialise in start order

**Given** two async callers each call `with_file_mutation_queue` for the
same path; the first holds the slot for ~30 ms before completing.
**When**  both callers are awaited concurrently.
**Then**  the second caller's body never starts until the first caller's
body finishes — the observable ordering is
`first:start → first:end → second:start → second:end`.

- Assertion: no interleaving of the two bodies is ever observed.
- Assertion: the second caller still returns its body's value correctly
  after the wait.
- Probe: `cargo test -p hand-coding-agent same_path_serialises_concurrent_callers -- --exact`.

### UC-fmq-002 — two callers for different paths run in parallel

**Given** two async callers, one for path A and one for path B, each
holding their slot for ~30 ms.
**When**  both are awaited concurrently.
**Then**  their bodies overlap in time — `B:start` is observed before
`A:end` (and vice-versa); total wall time is ~30 ms, not ~60 ms.

- Assertion: `B:start` precedes `A:end` in the event log.
- Assertion: both bodies' return values surface.
- Probe: `cargo test -p hand-coding-agent different_paths_run_in_parallel -- --exact`.

### UC-fmq-003 — a path and its symlink alias share one queue (POSIX)

**Given** a regular file `target.txt` and a symlink `alias.txt` pointing
at it.
**When**  caller A invokes the queue for `target.txt` (holding its slot
for ~30 ms) while caller B invokes the queue for `alias.txt`.
**Then**  the observable ordering is `target:start → target:end →
alias:start → alias:end` — the two paths resolve to the same canonical
on-disk inode and share the mutex.

- Assertion: no interleaving of the two bodies is ever observed.
- Assertion: both bodies still produce the expected return values.
- Probe (Unix only): `cargo test -p hand-coding-agent same_path_via_symlink_shares_queue -- --exact`.

### UC-fmq-004 — the queue returns the closure's value through

**Given** an async closure that returns the string `"ok"`.
**When**  the closure is wrapped in `with_file_mutation_queue`.
**Then**  awaiting the wrapper produces the string `"ok"` — the queue is
transparent to the caller's return type.

- Assertion: the wrapped call yields the inner closure's value verbatim.
- Probe: `cargo test -p hand-coding-agent returns_closure_value -- --exact`.

### UC-fmq-005 — a path that does not exist on disk still gets a queue keyed on its literal value

**Given** a path `/tmp/some-future-file.txt` that does not yet exist.
**When**  two callers race against that path through the queue.
**Then**  the queue serialises them on the literal-path key (canonical
resolution is impossible because the inode does not exist yet); the same
behaviour as case UC-fmq-001 holds for the non-existent path.

- Assertion: no interleaving of the two bodies is observed.
- Probe: `cargo test -p hand-coding-agent missing_file_uses_literal_key -- --exact`.

### UC-fmq-006 — two parallel edit-tool calls against the same file preserve BOTH edits

**Given** a file containing `alpha\nbeta\ngamma\n` and two parallel
edit-tool invocations: one replaces `alpha` with `ALPHA`, the other
replaces `beta` with `BETA`.
**When**  both edit calls are awaited concurrently.
**Then**  the final file content is `ALPHA\nBETA\ngamma\n` — neither edit
is dropped because the file mutation queue forced them to run sequentially.

- Assertion: the file's final byte sequence equals the expected three
  lines, with both replacements present.
- Probe: `cargo test -p hand-coding-agent test_edit_serialises_concurrent_calls_to_same_file -- --exact`.

### UC-fmq-007 — edit and write tools share the same queue for one path

**Given** a file containing `original\n`. An edit-tool call replaces
`original` with `edited`. A short time later (~5 ms), a write-tool call
fires against the same path with content `replacement\n`.
**When**  both calls are awaited concurrently.
**Then**  the final file content is `replacement\n` — the write
serialises after the edit completes; no torn-write happens, and the
write's content (being later in queue order) wins.

- Assertion: the file's final content equals `replacement\n` exactly.
- Probe: `cargo test -p hand-coding-agent test_edit_and_write_share_mutation_queue -- --exact`.
- Status note: hand's coverage races N parallel writes + N parallel edits
  (rather than pi's exact 1-edit + 1-write ordering) and asserts no torn
  content and a well-formed final file — strictly stronger than the
  upstream assertion.
