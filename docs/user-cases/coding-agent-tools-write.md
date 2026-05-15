# User-Cases: tools/write

**Upstream source:** `pi-mono/packages/coding-agent/test/tools.test.ts` (write-tool describe)
**hand-ai source:**   `crates/coding-agent/src/tools/write.rs`
**Surface:**          The `write` tool — full-file replacement. Parent
directories are auto-created. Writes funnel through the file mutation
queue (see UC-fmq-007) so concurrent edits/writes never tear.

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-write-001 | ✅ pass | `test_write_new_file` |
| UC-write-002 | ✅ pass | `test_write_creates_dirs` |
| UC-write-003 | ✅ pass | `test_write_overwrite` |
| UC-write-004 | ✅ pass | `test_write_missing_params` |
| UC-write-005 | ✅ pass | `test_write_expands_tilde` |

## Cases

### UC-write-001 — writing a fresh file reports success and lands on disk

**Given** a writable directory containing no file `write-test.txt`.
**When**  the user invokes write with `path=<dir>/write-test.txt` and
`content="Test content"`.
**Then**  the call returns a success-shaped result whose text contains
`Successfully wrote`, the path is mentioned in the output, and the file
on disk now contains exactly `Test content`.

- Assertion: result text contains `Successfully wrote`.
- Assertion: result text mentions the target path.
- Assertion: `std::fs::read_to_string(path) == "Test content"`.
- Probe: `cargo test -p hand-coding-agent test_write_new_file -- --exact`.

### UC-write-002 — parent directories are auto-created

**Given** a writable directory; the target `path` references
`<dir>/nested/dir/test.txt` where `nested/dir` does not exist.
**When**  the user invokes write with that path and some content.
**Then**  the missing `nested/dir/` is created, the file is written, and
the result text contains `Successfully wrote`.

- Assertion: result text contains `Successfully wrote`.
- Assertion: the parent directory exists after the call.
- Assertion: the file's content matches.
- Probe: `cargo test -p hand-coding-agent test_write_creates_dirs -- --exact`.

### UC-write-003 — writing to an existing file overwrites it

**Given** a file containing `old content`.
**When**  the user invokes write against that path with `content="new"`.
**Then**  the file's content becomes exactly `new` — old bytes are
gone, no append.

- Assertion: `std::fs::read_to_string(path) == "new"`.
- Probe: `cargo test -p hand-coding-agent test_write_overwrite -- --exact`.

### UC-write-004 — missing required parameters surface a clean error

**Given** any directory.
**When**  the user invokes write without `content` (or without `path`).
**Then**  the result text contains `Missing required parameter` and
identifies which one.

- Assertion: result text contains `Missing required parameter`.
- Probe: `cargo test -p hand-coding-agent test_write_missing_params -- --exact`.

### UC-write-005 — `~/...` paths expand to `$HOME` before writing

**Given** a writable `$HOME` (real or test-overridden).
**When**  the user invokes write with `path="~/written.txt"`.
**Then**  the file lands at `<HOME>/written.txt`, NOT at the literal
`<cwd>/~/written.txt`.

- Assertion: `<HOME>/written.txt` exists after the call.
- Assertion: no file `~` directory is created in cwd.
- Probe: `cargo test -p hand-coding-agent test_write_expands_tilde -- --exact`.
