# User-Cases: tools/path_utils

**Upstream source:** `pi-mono/packages/coding-agent/test/path-utils.test.ts`
**hand-ai source:**   `crates/coding-agent/src/tools/path_utils.rs`
**Surface:**          `expand_path`, `resolve_to_cwd`, `resolve_read_path` —
the three functions every tool routes user-supplied paths through before
touching disk. Wrong behaviour here silently corrupts every other tool.

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-path-utils-001 | ✅ pass | `expand_path_handles_tilde_alone` |
| UC-path-utils-002 | ✅ pass | `expand_path_handles_tilde_slash` |
| UC-path-utils-003 | ✅ pass | `expand_path_normalizes_unicode_spaces` |
| UC-path-utils-004 | ✅ pass | `resolve_to_cwd_preserves_absolute` |
| UC-path-utils-005 | ✅ pass | `resolve_to_cwd_joins_relative` |
| UC-path-utils-006 | ✅ pass | `resolve_read_path_returns_literal_when_present` |
| UC-path-utils-007 | ✅ pass | `resolve_read_path_probes_nfd_alone` |
| UC-path-utils-008 | ✅ pass | `resolve_read_path_probes_curly_quote_alone` |
| UC-path-utils-009 | ✅ pass | `resolve_read_path_probes_nfd_plus_curly` |
| UC-path-utils-010 | ✅ pass | `resolve_read_path_probes_am_pm_variant` |
| UC-path-utils-011 | ✅ pass | `resolve_read_path_probes_lowercase_am_pm_variant` |
| UC-path-utils-012 | ✅ pass | `resolve_read_path_returns_resolved_when_no_variant_matches` |

(`expand_path_strips_at_prefix`, `expand_path_strips_at_then_expands_tilde`,
and `expand_path_leaves_plain_relative` are hand-side coverage of the `@`
prefix and plain-relative fallthrough; no upstream parity case exists for
them. Covered by Rust unit tests but not enumerated here.)

## Cases

### UC-path-utils-001 — bare tilde expands to the home directory

**Given** the user's `$HOME` is some absolute path on this machine.
**When**  the path resolver is asked to expand the literal string `~`.
**Then**  the returned path is the absolute home directory and contains no
literal `~` character.

- Assertion: the returned string equals `$HOME` and is absolute.
- Assertion: the returned string does NOT contain the character `~`.
- Probe: `cargo test -p hand-coding-agent expand_path_handles_tilde_alone -- --exact`.

### UC-path-utils-002 — `~/<rest>` expands to `<home>/<rest>`

**Given** the user's `$HOME` is some absolute path.
**When**  the path resolver is asked to expand `~/Documents/file.txt`.
**Then**  the returned path is `$HOME/Documents/file.txt` (or the platform
equivalent) and contains no `~/` prefix.

- Assertion: the returned path is absolute.
- Assertion: the returned path ends with `Documents/file.txt`.
- Assertion: the returned string does NOT contain the substring `~/`.
- Probe: `cargo test -p hand-coding-agent expand_path_handles_tilde_slash -- --exact`.

### UC-path-utils-003 — Unicode no-break / narrow / hair / etc. spaces are normalised to ASCII space

**Given** a user pastes a path whose visible spaces are actually U+00A0
NO-BREAK SPACE (or U+2000..U+200A, or U+202F), e.g. copied from a macOS
screenshot filename.
**When**  the path resolver expands the string.
**Then**  every exotic-space code point is replaced with a plain U+0020
ASCII space so the path string matches what the underlying filesystem call
expects.

- Assertion: `expand_path("file\u{00A0}name.txt")` returns a path whose
  string form equals `file name.txt` (regular space).
- Assertion: the same holds for U+202F (narrow no-break space) and
  U+2009..U+200A (thin / hair spaces).
- Probe: `cargo test -p hand-coding-agent expand_path_normalizes_unicode_spaces -- --exact`.

### UC-path-utils-004 — absolute paths resolve unchanged against any cwd

**Given** any working directory (e.g. `/some/cwd`).
**When**  the resolver is asked to resolve the absolute path
`/absolute/path/file.txt` against that cwd.
**Then**  the returned path is exactly `/absolute/path/file.txt`; the cwd
plays no role.

- Assertion: the returned path is the input string verbatim.
- Probe: `cargo test -p hand-coding-agent resolve_to_cwd_preserves_absolute -- --exact`.

### UC-path-utils-005 — relative paths resolve against the user's cwd

**Given** a working directory `/some/cwd`.
**When**  the resolver is asked to resolve `relative/file.txt`.
**Then**  the returned path is `/some/cwd/relative/file.txt`.

- Assertion: the returned path is absolute.
- Assertion: the returned path equals `<cwd>/<relative>` after standard
  path joining.
- Probe: `cargo test -p hand-coding-agent resolve_to_cwd_joins_relative -- --exact`.

### UC-path-utils-006 — an existing file resolves to its literal path

**Given** a file named `test-file.txt` exists in the user's cwd.
**When**  the resolver is asked to resolve `test-file.txt` against that cwd.
**Then**  the returned path is `<cwd>/test-file.txt` — no Unicode probing
fires because the literal path already exists.

- Assertion: the returned path is the joined cwd+name.
- Assertion: the returned path points at a file on disk.
- Probe: `cargo test -p hand-coding-agent resolve_read_path_returns_literal_when_present -- --exact`.

### UC-path-utils-007 — typing NFC `é` finds an NFD `é` file (macOS)

**Given** a file is stored on disk under the NFD byte sequence
`file\u{0065}\u{0301}.txt` (i.e. `e` + combining acute accent).
**When**  the user types `file\u{00E9}.txt` (NFC `é` as one codepoint).
**Then**  the resolver probes the NFD variant and returns the path to the
on-disk file.

- Assertion: the returned path can be read with `std::fs::metadata` (i.e.
  it actually exists).
- Assertion: the basename matches `file<single accented char>.txt` in
  either normalisation form.
- Probe: `cargo test -p hand-coding-agent resolve_read_path_probes_nfd_alone -- --exact`.

### UC-path-utils-008 — typing ASCII apostrophe finds a curly-quote file

**Given** a file is stored under `it\u{2019}s mine.txt` (U+2019 right
single quotation mark — what macOS uses in some user-facing strings).
**When**  the user types `it's mine.txt` (U+0027 ASCII apostrophe).
**Then**  the resolver probes the curly-quote variant and returns the
on-disk path.

- Assertion: the returned path exists on disk.
- Assertion: the basename uses U+2019, not U+0027.
- Probe: `cargo test -p hand-coding-agent resolve_read_path_probes_curly_quote_alone -- --exact`.

### UC-path-utils-009 — combined NFC `é` + curly quote resolves (French screenshot)

**Given** a file `Capture d\u{2019}\u{00E9}cran.txt` (curly apostrophe +
NFC `é`) — the canonical macOS French screenshot filename shape — is on
disk.
**When**  the user types `Capture d'\u{00E9}cran.txt` (ASCII apostrophe).
**Then**  the resolver returns the on-disk path with curly apostrophe
preserved.

- Assertion: the returned path equals `<cwd>/Capture d\u{2019}\u{00E9}cran.txt`.
- Probe: `cargo test -p hand-coding-agent resolve_read_path_probes_nfd_plus_curly -- --exact`.

### UC-path-utils-010 — macOS screenshot AM with narrow-no-break-space resolves from a regular space

**Given** a file `Screenshot 2024-01-01 at 10.00.00\u{202F}AM.png`
(narrow-no-break-space U+202F before `AM`) is on disk.
**When**  the user types the same name with a regular space:
`Screenshot 2024-01-01 at 10.00.00 AM.png`.
**Then**  the resolver returns the on-disk path with U+202F preserved.

- Assertion: the returned path exists.
- Assertion: the basename contains U+202F, not U+0020, before `AM`.
- Probe: `cargo test -p hand-coding-agent resolve_read_path_probes_am_pm_variant -- --exact`.

### UC-path-utils-011 — lowercase `am`/`pm` (en_AU) resolves under the narrow-no-break-space probe

**Given** a file `Screenshot 2024-01-01 at 10.00.00\u{202F}am.png`
(lowercase `am`, narrow-no-break-space) is on disk — the form en_AU and
similar locales produce.
**When**  the user types `Screenshot 2024-01-01 at 10.00.00 am.png` with
a regular space.
**Then**  the resolver returns the on-disk path (case-insensitive `am`/`pm`
probe matches).

- Assertion: the returned path exists on disk.
- Probe: `cargo test -p hand-coding-agent resolve_read_path_probes_lowercase_am_pm_variant -- --exact`.

### UC-path-utils-012 — falls back to the literal resolved path when no Unicode variant matches

**Given** no file matching any probed variant exists in the cwd.
**When**  the user asks to resolve `nonexistent.txt`.
**Then**  the resolver returns `<cwd>/nonexistent.txt` unchanged so the
caller can surface a clean "file not found" error rather than a path the
user did not type.

- Assertion: the returned path equals `<cwd>/nonexistent.txt`.
- Assertion: the returned path does NOT exist on disk.
- Probe: `cargo test -p hand-coding-agent resolve_read_path_returns_resolved_when_no_variant_matches -- --exact`.
