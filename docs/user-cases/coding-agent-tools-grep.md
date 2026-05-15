# User-Cases: tools/grep

**Upstream source:** `pi-mono/packages/coding-agent/test/tools.test.ts` (grep-tool describe)
**hand-ai source:**   `crates/coding-agent/src/tools/grep.rs`
**Surface:**          The `grep` tool — search file contents by pattern.
Hand wraps the host `rg` (ripgrep) binary and post-processes its output
to (a) clip line length at 500 chars so a minified bundle can't dump
megabytes into the model context, and (b) stop flag parsing with `--`
to prevent `--pre=…` preprocessor RCE.

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-grep-001 | ✅ pass | `test_grep_basic` |
| UC-grep-002 | ❌ fail | hand uses `max_matches` not `limit`; truncation footer wording differs |
| UC-grep-003 | ✅ pass | `test_grep_flag_pattern_does_not_execute_preprocessor` |
| UC-grep-004 | ✅ pass | `test_grep_clips_long_match_lines`, `test_truncate_long_lines_clips_long_line`, `test_truncate_long_lines_respects_utf8_boundary` |
| UC-grep-005 | ✅ pass | `test_grep_no_matches` |
| UC-grep-006 | ✅ pass | `test_grep_missing_pattern` |

## Cases

### UC-grep-001 — single-file search includes the filename prefix on each match

**Given** a file `example.txt` with contents:
```
first line
match line
last line
```
**When**  the user runs grep with pattern `match` and path
`example.txt`.
**Then**  the output contains a line of the form
`example.txt:2: match line` — filename, colon, line number, colon,
matched text.

- Assertion: the output text contains `example.txt:2: match line`.
- Probe: `cargo test -p hand-coding-agent test_grep_basic -- --exact`.

### UC-grep-002 — global limit + context lines (FAILING under hand)

**Given** a file with two `match` lines and surrounding context:
```
before
match one
after
middle
match two
after two
```
**When**  the user runs grep with `pattern=match`, `limit=1`, `context=1`.
**Then**  the output contains the first match with one line of
before/after context AND a footer saying the limit was reached:
- `context.txt-1- before`
- `context.txt:2: match one`
- `context.txt-3- after`
- `[1 matches limit reached. Use limit=2 for more, or refine pattern]`
And `match two` does NOT appear.

- Assertion: the four lines above appear in the output.
- Assertion: `match two` does NOT appear.
- Probe (FAILS): hand's grep tool exposes `max_matches` (default 100),
  not `limit`. A user passing `limit=1` against hand sees the parameter
  silently ignored and gets all matches. The truncation footer text
  also differs.
- Gap: align hand's grep tool schema with pi — rename `max_matches` to
  `limit` (with `max_matches` as a deprecated alias for one release),
  and emit the `[N matches limit reached. Use limit=M for more, or
  refine pattern]` footer when capped.
- Resolution proposal: small schema rename + footer wording update in
  `crates/coding-agent/src/tools/grep.rs`, plus a new unit test
  reproducing the upstream scenario.

### UC-grep-003 — flag-shaped patterns are treated as literal search text, not CLI flags

**Given** a directory containing an executable `payload.sh` that, if
invoked as ripgrep's `--pre` preprocessor, would create a marker file
`grep-injection-marker`, plus a search target `target.txt`.
**When**  the user runs grep with pattern `--pre=<path-to-payload>`
and path = test directory.
**Then**  the output reports no matches; the marker file is NOT created;
the preprocessor was never invoked.

- Assertion: the output contains `No matches found` (or similar).
- Assertion: the marker file does NOT exist after the call.
- Probe: `cargo test -p hand-coding-agent test_grep_flag_pattern_does_not_execute_preprocessor -- --exact`.
- Why: this is a real RCE vector when the pattern comes from an LLM
  acting on attacker-controlled content. Mitigation: insert `--` before
  the pattern when shelling out to `rg` so flag parsing stops.

### UC-grep-004 — match lines longer than 500 chars are clipped with a `... [truncated]` suffix

**Given** a file containing a single line of 1000 `x` characters that
matches the pattern.
**When**  the user runs grep.
**Then**  the printed line is at most 500 characters of content plus a
literal trailing ` ... [truncated]` annotation. Multi-byte UTF-8
boundaries are respected — clipping never lands mid-codepoint.

- Assertion: the output line containing the match has length ≤ 500
  (printable chars) + the literal `... [truncated]`.
- Assertion: the output remains valid UTF-8.
- Probe: `cargo test -p hand-coding-agent test_grep_clips_long_match_lines test_truncate_long_lines_clips_long_line test_truncate_long_lines_respects_utf8_boundary -- --exact`.

### UC-grep-005 — a pattern with no matches returns a clean "no matches" message

**Given** any directory.
**When**  the user runs grep with a pattern matching nothing.
**Then**  the output contains `No matches` (no panic, no empty result).

- Assertion: the output text contains `No matches`.
- Probe: `cargo test -p hand-coding-agent test_grep_no_matches -- --exact`.

### UC-grep-006 — a missing `pattern` parameter surfaces a clean error

**Given** any directory.
**When**  the user invokes grep without supplying `pattern`.
**Then**  the result is an error whose text contains
`Missing required parameter`.

- Assertion: the result text contains `Missing required parameter`.
- Probe: `cargo test -p hand-coding-agent test_grep_missing_pattern -- --exact`.
