# Test classification after the ratatui migration (A/B/C)

When the legacy string-rendering stack was deleted (`refactor(tui): remove the
legacy string-rendering stack`), the integration tests that exercised it were
removed alongside the code. This table records how each removed test was
classified and where its behaviour is covered on the new (ratatui) stack, so a
future reader can confirm no behavioural coverage was lost.

- **A — behavioural**: tested a user-visible behaviour that must still be covered
  somewhere. Every A-class file maps to a surviving `crates/tui/tests/rt_*.rs`
  test and/or a `VAL-` contract assertion.
- **B — legacy-renderer mechanics**: tested the deleted string renderer's
  internals with no behavioural meaning on the new stack. Correctly deleted; the
  surviving *behaviour* (if any) is re-pinned on the rt stack.
- **C — utility**: tested a kept utility (`fuzzy`, `terminal_image`,
  `utils::{truncate_to_width, visible_width, wrap_text}`). The test survives.

## Removed / trimmed test files

| Deleted file | Class | Covering rt test / VAL- assertion (or rationale) |
|---|---|---|
| `tests/common/mod.rs` | B | Helper for the deleted string stack (`TestTerminal`, `strip_ansi`/`visible_width` over `Component::render(width)`). rt tests use ratatui `TestBackend` + `Buffer`. |
| `tests/autocomplete.rs` | A | `rt_autocomplete.rs` (slash/path popup open, prefix filter, Tab/Enter accept, ≤8-row window, esc close) + `rt_gallery`. VAL-EDITOR-005/006/007/008/021/022/025. |
| `tests/editor.rs` | A | `rt_editor.rs`, `rt_editor_paste.rs`, `rt_editor_undo.rs`. VAL-EDITOR-001/003/009/010/011/014. |
| `tests/input.rs` | A | Single-line `InputComponent` folded into the rt editor + focus routing: `rt_editor.rs`, `rt_focus.rs`. VAL-EDITOR-001, VAL-CORE-028. |
| `tests/keybindings.rs` | A | Registry behaviour re-pinned in `coding-agent/tests/keybindings_fixtures.rs`. VAL-COMPAT-001/002/003/006, VAL-CORE-031. |
| `tests/keys.rs` | A | Legacy string-byte parsing deleted (crossterm parses bytes); canonicalization re-pinned in `rt_events.rs`. VAL-CORE-014/030/031. |
| `tests/markdown.rs` | A | `rt_markdown.rs` (headings, lists, blockquote, rule, code block, tables/CJK, inline, links, images, strikethrough/task-list, narrow wrap). VAL-WIDGET-001..006/014/020/023. |
| `tests/image.rs` | A | `rt_image.rs`, `rt_image_scrollback.rs`. VAL-IMG-003/008/016/017/018. |
| `tests/select_list.rs` | A | `rt_lists.rs` (first-selected, index clamp, prefix filter, filter-clear, empty no-panic, wrap/window/jump). VAL-WIDGET-007/008/009/015. |
| `tests/truncated_text.rs` | A | `rt_components.rs` truncated-text tests (fits/ellipsis/pad/CJK-narrow). VAL-WIDGET-018. |
| `tests/stdin_buffer.rs` | B | Legacy raw-byte reader; crossterm's `EventStream` owns byte framing now. Single-event guarantees re-pinned in `rt_events.rs`. VAL-CORE-015/030/039. |
| `tests/terminal.rs` | B | Tested the deleted in-memory `TestTerminal` double. Replaced by ratatui `TestBackend`; session escape emission re-pinned in `rt_session.rs`. |
| `tests/overlay_non_capturing.rs` | A | `rt_overlay.rs` (LIFO capture, pass-through, block-below). VAL-OVERLAY-005/030. |
| `tests/overlay_options.rs` | A | `rt_overlay.rs` (nine anchors, margins, clamp, full-bleed no-overflow, dim). VAL-OVERLAY-020/030, VAL-CORE-025. |
| `tests/overlay_short_content.rs` | A | Viewport-anchored centering + pad-to-height; `rt_overlay.rs` anchor/clamp tests compose against a full `Rect`/`Buffer`. VAL-OVERLAY-020/030. |
| `tests/tui_overlay_style_leak.rs` | A | `rt_overlay.rs` (no residue after pop / resize-while-open) + `rt_viewport_boundary.rs`. VAL-OVERLAY-008, VAL-CORE-026/034. |
| `tests/tui_render.rs` | B | Tested the deleted string `DiffRenderer` internals + `Container`. ratatui owns diffing; draw-coalescing/focus re-pinned in `rt_scheduler.rs`, `rt_focus.rs`. VAL-CORE-003/004/023/028. |
| `tests/tui_shutdown.rs` | B | Pinned the string stack's shutdown byte ordering. Exit-erase re-pinned in `rt_viewport_boundary.rs`. VAL-CORE-016, VAL-COMPAT-012/013. |
| `tests/fuzzy.rs`, `tests/terminal_image.rs`, `tests/truncate_to_width.rs`, `tests/wrap_ansi.rs` | C | Not deleted — survive at HEAD (only a `mod common;` import line was stripped). Test the kept utilities; all pass. |

## Historical-bug regressions

Both curated historical-bug regression tests survive on the new stack and pass:

| Regression test | Pins |
|---|---|
| `tests/bug_regression_isimageline_startswith.rs` | `image_fallback` output width is driven by `max_cols`, never by a malicious label — the historical crash path cannot reproduce by construction. |
| `tests/regression_regional_indicator_width.rs` | Lone/paired regional-indicator graphemes are width 2 so the differential renderer never drifts. |

`git ls-tree <pre-removal>^ crates/tui/tests/` confirms these were the only two
regression files, and both are present at HEAD — no historical-bug regression was
lost in the deletion.
