# User-Cases: tools/read

**Upstream source:** `pi-mono/packages/coding-agent/test/tools.test.ts` (read-tool describe)
**hand-ai source:**   `crates/coding-agent/src/tools/read.rs`
**Surface:**          The `read` tool — return file contents to the model.
This is one of the most user-visible tools; the exact format (line
numbering, truncation message wording, error shape) directly drives
how the model thinks about file contents.

## Divergence summary

Several upstream behaviours diverge from hand's current implementation:

- **Line-number prefix:** hand prepends each line with `{N>6}→` (e.g.
  `     1→`); pi returns raw content. Visible delta in every read.
- **Truncation message wording:** hand emits
  `[Showing lines 1-2000 of 2500 total. Use offset/limit to read more.]`;
  pi emits `[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]`.
- **Limit-truncation message:** hand emits the same truncation footer;
  pi emits `[90 more lines in file. Use offset=11 to continue.]` when
  truncation is by user-supplied limit (not the default line cap).
- **Image-magic detection:** hand has no PNG/JPEG/GIF magic check —
  every read returns a text block regardless of file bytes.
- **Result `details` metadata:** hand returns only text; pi populates a
  structured `result.details.truncation` object the host can render.

Each of these surfaces below as a ❌ case with a remediation note.

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-read-001 | ❌ fail | hand prefixes each line with `{N>6}→`; pi returns raw content |
| UC-read-002 | ✅ pass | `test_read_missing_file` (error returned as success-shaped ToolResult — observable behaviour equivalent for the model) |
| UC-read-003 | ❌ fail | hand's default line cap is 2000 and wording differs from pi |
| UC-read-004 | ❌ fail | byte-limit wording differs from pi |
| UC-read-005 | ✅ pass | `test_read_with_offset_and_limit` (offset semantics correct, but output formatting differs — see UC-read-001) |
| UC-read-006 | ❌ fail | limit-truncation wording differs from pi |
| UC-read-007 | ✅ pass | offset+limit semantics correct (modulo UC-read-001 formatting) |
| UC-read-008 | ✅ pass | `test_read_offset_beyond_eof_errors` |
| UC-read-009 | ❌ fail | hand emits no structured `details.truncation` object |
| UC-read-010 | ❌ fail | hand does not detect image MIME via file magic |
| UC-read-011 | ✅ pass | files without image magic always return text (vacuously: hand never returns image blocks today) |

## Cases

### UC-read-001 — small files return their content as text

**Given** a file `test.txt` with content `Hello, world!\nLine 2\nLine 3`.
**When**  the user invokes read with `path=<that file>`.
**Then**  the returned text equals the file's content. No truncation
banner is emitted. The result carries no extra `details` metadata.

- Assertion: the result text equals the file's content byte-for-byte.
- Assertion: the result text contains no `Use offset=` banner.
- Probe (FAILS today): hand's output is
  `     1→Hello, world!\n     2→Line 2\n     3→Line 3\n` — every line
  carries a 6-wide line-number prefix and a `→` separator.
- Gap: pi's read tool returns raw file content; the line-number prefix
  is an interactive-UI affordance, not a tool-protocol concern. The
  model often quotes file content verbatim; the prefix forces it to
  do work to strip prefixes before reasoning about the bytes.
- Resolution proposal: make line-numbering opt-in via a parameter
  (defaulting OFF for parity), or strip the prefix in the tool surface
  and only render it in TUI history.

### UC-read-002 — reading a non-existent file surfaces a clean error

**Given** a path that does not point at any file on disk.
**When**  the user invokes read with that path.
**Then**  the call returns a result whose text indicates the failure
(contains `not found`, `ENOENT`, or `Failed to read`).

- Assertion: the result text contains a failure marker the model can
  parse.
- Probe: `cargo test -p hand-coding-agent test_read_missing_file -- --exact`.
- Note: pi throws; hand returns a `ToolResult::error(...)`. From the
  agent's point of view both surface as a failed tool-use result. The
  important contract — "model can tell read failed" — holds either
  way. Treated as ✅.

### UC-read-003 — files exceeding 2000 lines are truncated with a clear marker

**Given** a 2500-line file `large.txt`.
**When**  the user invokes read with no `offset`/`limit`.
**Then**  the output contains lines 1..2000 and stops; lines 2001..2500
do NOT appear; a footer line tells the user to fetch the rest with
`offset=2001`.

- Assertion: the output contains the text of line 1 and line 2000.
- Assertion: the output does NOT contain the text of line 2001.
- Assertion: the output footer reads
  `[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]`.
- Probe (FAILS today): hand emits
  `[Showing lines 1-2000 of 2500 total. Use offset/limit to read more.]`
  — wording differs.
- Resolution proposal: replace hand's truncation footer template with
  the pi wording (parameterised on `last_shown + 1`).

### UC-read-004 — files exceeding 50 KB are truncated by byte budget

**Given** a 500-line file where each line is ~200 chars (well over the
50 KB byte budget but under the 2000-line cap).
**When**  the user invokes read with no `offset`/`limit`.
**Then**  the output stops when ~50 KB of payload has been emitted; a
footer like `[Showing lines 1-N of 500 (50 KB limit). Use offset=N+1
to continue.]` is included.

- Assertion: the output contains line 1's content.
- Assertion: the footer matches the regex
  `\[Showing lines 1-\d+ of 500 \(.* limit\)\. Use offset=\d+ to continue\.\]`.
- Probe (FAILS today): hand emits
  `[Showing lines 1-N of 500 (50.0KB byte limit). Use offset=N+1 to continue.]`
  with `50.0KB` formatting. Close to pi but wording differs slightly
  (`KB limit` vs `byte limit`).
- Resolution proposal: align byte-limit footer wording with pi's
  `(<size> limit)` shape.

### UC-read-005 — `offset` skips initial lines

**Given** a 100-line file.
**When**  the user invokes read with `offset=51`.
**Then**  the output begins at line 51 and includes through line 100;
no truncation banner is emitted (it fits within limits).

- Assertion: the output contains line 51 and line 100.
- Assertion: the output does NOT contain line 50.
- Assertion: no `Use offset=` footer is appended.
- Probe: `cargo test -p hand-coding-agent test_read_with_offset_and_limit -- --exact`
  (covers offset+limit; offset-only behaviour follows the same code path).

### UC-read-006 — `limit` caps the number of lines returned

**Given** a 100-line file.
**When**  the user invokes read with `limit=10`.
**Then**  the output contains lines 1..10 only; the footer
`[90 more lines in file. Use offset=11 to continue.]` is appended.

- Assertion: the output contains line 1 and line 10.
- Assertion: the output does NOT contain line 11.
- Assertion: the output contains
  `[90 more lines in file. Use offset=11 to continue.]`.
- Probe (FAILS today): hand emits its truncation footer which differs
  in wording from pi when truncation was driven by user `limit`.
- Resolution proposal: branch hand's footer template between
  "default-cap truncation" and "user-limit truncation" with the
  matching wording for each.

### UC-read-007 — `offset` + `limit` together select a window

**Given** a 100-line file.
**When**  the user invokes read with `offset=41`, `limit=20`.
**Then**  the output contains lines 41..60 only; line 40 and line 61 do
NOT appear; a footer like
`[40 more lines in file. Use offset=61 to continue.]` is appended.

- Assertion: the output contains line 41 and line 60.
- Assertion: line 40 and line 61 are not in the output.
- Probe: `cargo test -p hand-coding-agent test_read_with_offset_and_limit -- --exact`.
- Status note: ranges/semantics correct; footer wording inherits the
  UC-read-006 gap.

### UC-read-008 — `offset` beyond EOF returns an explicit out-of-bounds error

**Given** a 3-line file.
**When**  the user invokes read with `offset=100`.
**Then**  the result is an error whose text contains
`Offset 100 is beyond end of file (3 lines total)`.

- Assertion: the result text matches the regex
  `Offset \d+ is beyond end of file \(\d+ lines total\)`.
- Probe: `cargo test -p hand-coding-agent test_read_offset_beyond_eof_errors -- --exact`.

### UC-read-009 — truncated reads expose structured `details.truncation` metadata

**Given** a 2500-line file truncated to 2000 lines.
**When**  the user invokes read.
**Then**  the returned result carries a `details.truncation` object:
- `truncated: true`
- `truncated_by: "lines"`
- `total_lines: 2500`
- `output_lines: 2000`

- Assertion: the result includes the structured truncation metadata
  the host UI / SDK can consume without parsing the text footer.
- Probe (FAILS today): hand returns plain text without a `details`
  side-channel. Closing this means extending `ToolResult` to carry a
  per-tool metadata blob and wiring it through to the host.
- Resolution proposal: add a `details: serde_json::Value` field on
  the tool result envelope and populate it from hand's read tool.

### UC-read-010 — image files are detected by file magic, not extension

**Given** a PNG payload (1x1, 67 bytes) written to a file named
`image.txt` (no `.png` extension).
**When**  the user invokes read.
**Then**  the result contains a text block with the line
`Read image file [image/png]` AND an `image` content block whose
`mimeType` is `image/png` and whose `data` is the base64 of the file.

- Assertion: the result text contains `Read image file [image/png]`.
- Assertion: the result content includes an image block with the
  right MIME and non-empty data.
- Probe (FAILS today): hand reads the bytes as UTF-8 (lossy or strict)
  and returns text. No file-magic detection exists yet.
- Resolution proposal: add a small magic-sniff (PNG `89 50 4E 47`,
  JPEG `FF D8 FF`, GIF `47 49 46`, WebP `RIFF…WEBP`) to the read tool;
  emit an `image` content block when matched and add a "Read image
  file [<mime>]" marker text.

### UC-read-011 — files with image-suggesting extensions but non-image content stay as text

**Given** a file `not-an-image.png` whose content is the text
`definitely not a png`.
**When**  the user invokes read.
**Then**  the output is plain text containing `definitely not a png`;
no image block appears.

- Assertion: the output text contains `definitely not a png`.
- Assertion: the result content has no image-typed block.
- Probe: hand currently always returns text-only, so this holds
  vacuously. Once UC-read-010 is implemented, this case must continue
  to pass.
