# User-Cases: tools/render_utils

**Upstream source:** Implicit via `pi-mono/packages/coding-agent/src/utils/shell.ts`
(`sanitizeBinaryOutput`) and `src/core/tools/render-utils.ts`. pi has no
dedicated test file; behaviour is exercised indirectly across multiple
suites. hand's own unit tests in `tools/render_utils.rs` define the
parity contract.
**hand-ai source:**   `crates/coding-agent/src/tools/render_utils.rs`
**Surface:**          Tool-result rendering — strip ANSI, drop \r,
sanitize C0 / Unicode-format chars, emit image-fallback indicators
when the terminal can't render graphics. Wrong rendering corrupts the
TUI scrollback or, worse, smuggles a terminal-control sequence into
the model's view.

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-render-001 | ✅ pass | `shorten_path_replaces_home_with_tilde` |
| UC-render-002 | ✅ pass | `shorten_path_leaves_other_paths_alone` |
| UC-render-003 | ✅ pass | `replace_tabs_uses_three_spaces` |
| UC-render-004 | ✅ pass | `replace_tabs_handles_no_tabs` |
| UC-render-005 | ✅ pass | `normalize_display_text_strips_cr` |
| UC-render-006 | ✅ pass | `get_text_output_concatenates_text_blocks` |
| UC-render-007 | ✅ pass | `get_text_output_strips_ansi_escapes` |
| UC-render-008 | ✅ pass | `get_text_output_strips_c0_controls_and_format_chars` |
| UC-render-009 | ✅ pass | `sanitize_binary_output_pure_helper` |
| UC-render-010 | ✅ pass | `get_text_output_emits_fallback_indicator_for_images_when_no_protocol` |
| UC-render-011 | ✅ pass | `get_text_output_skips_indicator_when_graphics_supported_and_show_images` |
| UC-render-012 | ✅ pass | `get_text_output_filters_non_text_when_no_protocol_and_images_hidden` |

## Cases

### UC-render-001 — paths under `$HOME` shorten to `~/...` in display strings

**Given** a path string under the current `$HOME`, e.g.
`/Users/alice/Documents/file.txt`.
**When**  the path is fed through the display-shortener helper.
**Then**  the result begins with `~/` and the home prefix is replaced.

- Probe: `cargo test -p hand-coding-agent shorten_path_replaces_home_with_tilde -- --exact`.

### UC-render-002 — paths outside `$HOME` are left alone

**Given** a path string not under `$HOME`, e.g. `/etc/hosts`.
**When**  the same shortener runs.
**Then**  the result equals the input string verbatim.

- Probe: `cargo test -p hand-coding-agent shorten_path_leaves_other_paths_alone -- --exact`.

### UC-render-003 — `\t` characters are rendered as three ASCII spaces

**Given** any text containing tab characters.
**When**  the renderer applies the tab-replacement helper.
**Then**  each tab is replaced with three spaces (so terminal width
calculation stays predictable and tools that pipe through the renderer
don't blow up on weird tab widths).

- Probe: `cargo test -p hand-coding-agent replace_tabs_uses_three_spaces -- --exact`.

### UC-render-004 — text without tabs passes through unchanged

**Given** text with no tab character.
**When**  the tab-replacement helper runs.
**Then**  the output equals the input.

- Probe: `cargo test -p hand-coding-agent replace_tabs_handles_no_tabs -- --exact`.

### UC-render-005 — `\r` carriage returns are stripped from display text

**Given** display text containing a `\r` (often introduced by Windows
tool output or escape-sequence fragments).
**When**  the display-normaliser runs.
**Then**  every `\r` is removed; `\n` is preserved verbatim so line
boundaries stay intact.

- Probe: `cargo test -p hand-coding-agent normalize_display_text_strips_cr -- --exact`.

### UC-render-006 — multiple text blocks in a tool result concatenate with `\n` between

**Given** a `ToolResult` containing two text blocks `"red text"` and
`"blue text"`.
**When**  the renderer flattens the content list to a single string.
**Then**  the output is `red text\nblue text` (one newline between
blocks). The boundary IS a real newline, not a literal `\\n`.

- Probe: `cargo test -p hand-coding-agent get_text_output_concatenates_text_blocks -- --exact`.

### UC-render-007 — ANSI escape sequences are stripped from text blocks

**Given** a text block containing `\u{001B}[31mred text\u{001B}[0m`
(red + reset).
**When**  the renderer flattens the result.
**Then**  the output is the literal string `red text` — no escape
bytes, no control characters.

- Probe: `cargo test -p hand-coding-agent get_text_output_strips_ansi_escapes -- --exact`.

### UC-render-008 — C0 control chars and U+FFF9..U+FFFB format chars are stripped

**Given** a text block containing C0 control characters (BEL 0x07,
VT 0x0B, FF 0x0C) and Unicode interlinear-annotation format chars
(U+FFF9, U+FFFA, U+FFFB) interspersed with normal text.
**When**  the renderer flattens the result.
**Then**  every offending code point is removed; `\t \n \r` and DEL
(U+007F) are preserved (DEL is not in the C0 range).

- Probe: `cargo test -p hand-coding-agent get_text_output_strips_c0_controls_and_format_chars sanitize_binary_output_pure_helper -- --exact`.

### UC-render-009 — `sanitize_binary_output` is a pure char-filter helper

**Given** a string passed directly through `sanitize_binary_output`
(not via the result renderer).
**When**  the function returns.
**Then**  the output retains `\t`, `\n`, `\r`, ASCII printable, DEL,
and arbitrary Unicode codepoints outside the U+FFF9..U+FFFB block;
every other C0 (≤ 0x1F except those three) is dropped, as are
U+FFF9..U+FFFB.

- Probe: `cargo test -p hand-coding-agent sanitize_binary_output_pure_helper -- --exact`.

### UC-render-010 — image blocks render a fallback indicator when the terminal has no graphics protocol

**Given** a `ToolResult` containing one image block (PNG, any size) and
the terminal capabilities report no kitty / iterm2 protocol available
AND `show_images` is true.
**When**  the renderer flattens the result.
**Then**  the output text contains a labelled image-fallback box (a
visible ASCII / Unicode box with the MIME type and approximate
dimensions) so the model sees that "there was an image here" without
the raw bytes flooding context.

- Probe: `cargo test -p hand-coding-agent get_text_output_emits_fallback_indicator_for_images_when_no_protocol -- --exact`.

### UC-render-011 — image blocks are EXCLUDED from text output when the terminal supports graphics

**Given** an image block and terminal caps reporting kitty/iterm2
support AND `show_images` is true.
**When**  the renderer flattens the result.
**Then**  the image block does NOT appear in the text payload — the
host is expected to render it out-of-band through the graphics
protocol; embedding the bytes in text would corrupt the rendered cell.

- Probe: `cargo test -p hand-coding-agent get_text_output_skips_indicator_when_graphics_supported_and_show_images -- --exact`.

### UC-render-012 — image blocks are filtered out when `show_images` is false and no graphics protocol exists

**Given** an image block, no graphics caps, AND `show_images=false`.
**When**  the renderer flattens.
**Then**  neither the image bytes NOR a fallback indicator appears —
the host has signalled "no images at all".

- Probe: `cargo test -p hand-coding-agent get_text_output_filters_non_text_when_no_protocol_and_images_hidden -- --exact`.
