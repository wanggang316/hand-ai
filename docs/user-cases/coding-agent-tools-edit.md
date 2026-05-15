# User-Cases: tools/edit

**Upstream source:** `pi-mono/packages/coding-agent/test/tools.test.ts`
(edit-tool + fuzzy-matching + CRLF describes — 31 cases)
**hand-ai source:**   `crates/coding-agent/src/tools/edit.rs`
**Surface:**          The `edit` tool — replace text in a file. Edits
funnel through the file mutation queue (see UC-fmq-006/007). The tool
returns a unified diff of the changes.

## Surface (both shapes supported)

The edit tool accepts either shape:

- **Single-edit:** `{ old_string, new_string, replace_all? }` —
  `replace_all=true` switches from "must appear exactly once" to
  "replace every occurrence".
- **Multi-edit:** `edits: [{ oldText, newText }, ...]` — applied
  atomically against the ORIGINAL file content (not incrementally),
  with overlap detection. When supplied, the single-edit parameters
  are ignored.

`file_path` is the only required field on the tool schema. The two
shapes share fuzzy / CRLF / BOM handling.

## Status

| ID | pi case | hand status |
|----|---------|-------------|
| UC-edit-001 | replace text in file | ✅ `test_edit_simple_replace` |
| UC-edit-002 | fail if text not found | ✅ `test_edit_not_found` |
| UC-edit-003 | include ENOENT when target missing | ✅ `test_edit_missing_file_surfaces_enoent_code` |
| UC-edit-004 | fail if text appears multiple times | ✅ `test_edit_ambiguous` |
| UC-edit-005 | replace multiple disjoint regions in one call | ✅ `test_edit_multi_edit_replaces_disjoint_regions` |
| UC-edit-006 | collapse large unchanged gaps in multi-edit diff | ✅ inherited from `generate_diff` context_radius=3 — only changed hunks are rendered, large unchanged gaps are elided by the unified-diff format |
| UC-edit-007 | match edits against ORIGINAL file (not incrementally) | ✅ `test_edit_multi_edit_matches_against_original_not_incremental` |
| UC-edit-008 | fail when `edits` is empty | ✅ `test_edit_multi_edit_empty_array_rejected` |
| UC-edit-009 | fail when multi-edit regions overlap | ✅ `test_edit_multi_edit_overlapping_regions_rejected` |
| UC-edit-010 | no partial application when one edit fails | ✅ `test_edit_multi_edit_no_partial_application_on_failure` |
| UC-edit-011 | include EACCES for read-only files | ✅ `test_edit_readonly_file_surfaces_eacces_code` |
| UC-edit-012 | include original error message for unknown access errors | 🚫 N/A — hand maps every io::ErrorKind to a named code in UC-edit-003/011; the "unknown access error pass-through" is pi's escape hatch for kinds it doesn't recognize, redundant in hand. |
| UC-edit-013 | include ENOENT in diff preview for missing files | 🚫 N/A — hand has no preview API; edit operations are atomic apply-or-error. |
| UC-edit-014 | include EACCES in diff preview for unreadable files | 🚫 N/A — same: no preview API. |
| UC-edit-015 | match text with trailing whitespace stripped (fuzzy) | ⚠️ pending — verify hand's fuzzy normalisation does this |
| UC-edit-016 | match fullwidth punctuation in Chinese text | ⚠️ pending |
| UC-edit-017 | match compatibility-equivalent Unicode forms (NFKC) | ⚠️ pending |
| UC-edit-018 | match smart single quotes to ASCII | ✅ `test_edit_fuzzy_smart_single_quotes` |
| UC-edit-019 | match smart double quotes to ASCII | ✅ `test_edit_fuzzy_smart_double_quotes` |
| UC-edit-020 | match Unicode dashes to ASCII hyphen | ✅ `test_edit_fuzzy_unicode_dashes` |
| UC-edit-021 | match non-breaking space to regular space | ✅ `test_edit_fuzzy_nbsp` |
| UC-edit-022 | prefer exact match over fuzzy match | ⚠️ pending |
| UC-edit-023 | still fail when text not found even with fuzzy | ✅ inherited from UC-edit-002 path |
| UC-edit-024 | detect duplicates after fuzzy normalization | ⚠️ pending |
| UC-edit-025 | support fuzzy matching in multi-edit mode | ✅ `test_edit_multi_edit_fuzzy_matching_applies` |
| UC-edit-026 | match LF oldText against CRLF file content | ✅ `test_edit_lf_old_string_matches_crlf_file` |
| UC-edit-027 | preserve CRLF line endings after edit | ✅ (covered by same test pair) |
| UC-edit-028 | preserve LF line endings for LF files | ✅ `test_edit_crlf_normalization_does_not_affect_single_line` |
| UC-edit-029 | detect duplicates across CRLF/LF variants | ⚠️ pending |
| UC-edit-030 | preserve UTF-8 BOM after edit | ⚠️ pending — hand needs explicit BOM-preservation test |
| UC-edit-031 | preserve CRLF + BOM in multi-edit mode | ✅ `test_edit_multi_edit_preserves_bom_and_crlf` |

## Cases (load-bearing detail)

### UC-edit-001 — a single replacement on a unique anchor lands

**Given** a file with content `Hello, world!`.
**When**  the user invokes edit with `old_string="world"`,
`new_string="testing"`.
**Then**  the result text contains `Successfully replaced`; the file
on disk now reads `Hello, testing!`; the result `details.diff` is a
unified diff string containing the new substring.

- Probe: `cargo test -p hand-coding-agent test_edit_simple_replace -- --exact`.

### UC-edit-002 — text not found yields a clean error

**Given** a file `Hello, world!`.
**When**  the user invokes edit with `old_string="nonexistent"`.
**Then**  the call returns an error whose text matches
`Could not find the exact text`.

- Probe: `cargo test -p hand-coding-agent test_edit_not_found -- --exact`.

### UC-edit-004 — ambiguous anchor (multiple matches) is rejected with a count

**Given** a file `foo foo foo`.
**When**  the user invokes edit with `old_string="foo"` (no
`replace_all`).
**Then**  the call returns an error whose text matches
`Found 3 occurrences` (or hand's equivalent count message), telling the
model to supply a more unique anchor.

- Probe: `cargo test -p hand-coding-agent test_edit_ambiguous -- --exact`.

### UC-edit-005..010, 025, 031 — multi-edit array surface

**Given** an edits array with multiple `{oldText, newText}` entries.
**When**  the user invokes edit with that array.
**Then**  every entry applies against the ORIGINAL file content;
overlap is detected; empty arrays error; one entry failing rolls back
all (no partial application). Fuzzy / CRLF / BOM handling is shared
with the single-edit path.

- Probe: `cargo test -p hand-coding-agent --lib tools::edit::tests::test_edit_multi_edit -- --exact`.
- Implementation: `crates/coding-agent/src/tools/edit.rs::run_multi_edit`
  resolves each entry's byte range against the original content,
  sorts by start offset, scans for overlap, and stitches the new
  content from original slices in offset order. Writes once at the
  end. CRLF and BOM are preserved end-to-end; fuzzy normalisation
  retries the whole batch in normalised space when any entry fails
  literal lookup.

### UC-edit-018..021 — Unicode-fuzzy matching (curly quotes, dashes, NBSP)

**Given** a file containing smart quotes / em-dash / NBSP variants.
**When**  the user supplies an `old_string` with the ASCII equivalent.
**Then**  the edit lands on the smart-variant text; the file is
updated; the result reports success.

- Probe: `cargo test -p hand-coding-agent test_edit_fuzzy_smart_single_quotes test_edit_fuzzy_smart_double_quotes test_edit_fuzzy_unicode_dashes test_edit_fuzzy_nbsp -- --exact`.

### UC-edit-026..028 — CRLF / LF interop

**Given** a file using CRLF line endings; `old_string` uses LF.
**When**  the user invokes edit.
**Then**  the edit lands; existing CRLF endings are preserved
elsewhere in the file. (Mirror for LF files retaining LF.)

- Probe: `cargo test -p hand-coding-agent test_edit_lf_old_string_matches_crlf_file test_edit_crlf_old_string_matches_lf_file test_edit_crlf_normalization_does_not_affect_single_line -- --exact`.
