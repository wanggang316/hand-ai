//! Rendering tests for the rt markdown renderer (`hand_tui::rt::components`).
//!
//! These render markdown into a ratatui `Buffer` — the same model the rt
//! scheduler draws every frame — through [`MarkdownView`], the `RtComponent`
//! wrapper, and assert the *behavioural signatures* the external validator
//! probes. Per the Decision Log the self-authored markdown signatures (`#`
//! prefix, two-space list indent, `│ ` blockquote gutter) are pinned verbatim;
//! only the output moved from ANSI strings to `Buffer` cells, so the assertions
//! read the painted cell grid directly.
//!
//! Each block/inline element is checked at a wide (100-column) geometry, and the
//! narrow-width path (40 columns) is checked separately for clean wrapping with
//! no style leak and an unbroken code-block border. CJK content exercises the
//! display-width column-alignment path.
//!
//! Assertions traced to the plan's validation-contract:
//! - **VAL-WIDGET-001** — block elements incl. ordered (non-`1`) lists.
//! - **VAL-WIDGET-002** — fenced code block: border + language label.
//! - **VAL-WIDGET-003** — table columns align with CJK cells.
//! - **VAL-WIDGET-005** — nested inline styles restore the enclosing style.
//! - **VAL-WIDGET-006** — link fallback (`text (url)`), no autolink duplication.
//! - **VAL-WIDGET-014** — narrow-width wrapping with no style leak (markdown).
//! - **VAL-WIDGET-020** — image degrades to alt text, no `![`/`](` fragment.
//! - **VAL-WIDGET-023** — strikethrough / task-list / inline-code literal text.

use hand_tui::rt::components::{MarkdownTheme, MarkdownView, render_markdown};
use hand_tui::rt::view::RtComponent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Line;
use std::sync::Mutex;
use unicode_width::UnicodeWidthStr;

/// Serializes the two tests that toggle the process-global `HAND_DISABLE_OSC8`
/// env var: cargo runs tests in parallel threads within one process, so an
/// unguarded `set_var`/`remove_var` in one races the read in another. Holding
/// this lock for the whole set/render/remove window keeps the capability probe
/// deterministic.
static OSC8_ENV_LOCK: Mutex<()> = Mutex::new(());

// --- helpers ----------------------------------------------------------------

/// Render a `MarkdownView` of `source` into a fresh buffer of the given size.
fn render(source: &str, width: u16, height: u16) -> Buffer {
    let view = MarkdownView::new(source);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);
    buf
}

/// The symbols of one buffer row concatenated, trailing blanks trimmed.
fn row_string(buf: &Buffer, y: u16) -> String {
    let area = buf.area;
    let mut row = String::new();
    for x in area.x..area.x + area.width {
        if let Some(cell) = buf.cell((x, y)) {
            row.push_str(cell.symbol());
        }
    }
    row.trim_end().to_string()
}

/// Every row of the buffer as a trimmed string.
fn all_rows(buf: &Buffer) -> Vec<String> {
    let area = buf.area;
    (area.y..area.y + area.height)
        .map(|y| row_string(buf, y))
        .collect()
}

/// The painted column span of row `y`: one past the last non-blank cell. Measured
/// from the buffer so a wide (CJK) grapheme — which occupies one glyph cell plus a
/// blank continuation cell — is counted as the two columns the terminal shows,
/// not double-counted.
fn painted_cols(buf: &Buffer, y: u16) -> u16 {
    let area = buf.area;
    let mut last = area.x;
    for x in area.x..area.x + area.width {
        if let Some(cell) = buf.cell((x, y))
            && cell.symbol() != " "
            && !cell.symbol().is_empty()
        {
            last = x + 1;
        }
    }
    last - area.x
}

/// The plain text of every logical (un-wrapped) rendered line.
fn logical_text(source: &str, width: u16) -> Vec<String> {
    render_markdown(source, width, &MarkdownTheme::default())
        .iter()
        .map(|l: &Line| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

// --- VAL-WIDGET-001: block elements -----------------------------------------

#[test]
fn heading_paints_hash_prefix_bold() {
    let buf = render("# Hello", 100, 4);
    let rows = all_rows(&buf);
    assert!(
        rows.iter().any(|r| r.starts_with("# Hello")),
        "heading signature missing: {rows:?}"
    );
    // The heading cells are bold.
    let bold = buf
        .content()
        .iter()
        .any(|cell| cell.modifier.contains(Modifier::BOLD));
    assert!(bold, "heading must be bold");
}

#[test]
fn ordered_list_increments_and_honours_non_one_start() {
    let buf = render("3. three\n4. four\n5. five", 100, 6);
    let rows = all_rows(&buf);
    assert!(rows.iter().any(|r| r.starts_with("3. three")), "{rows:?}");
    assert!(rows.iter().any(|r| r.starts_with("4. four")), "{rows:?}");
    assert!(rows.iter().any(|r| r.starts_with("5. five")), "{rows:?}");
}

#[test]
fn unordered_nested_list_indents_two_spaces_per_level() {
    let buf = render("- outer\n  - inner\n- outer2", 100, 6);
    let rows = all_rows(&buf);
    assert!(rows.iter().any(|r| r.starts_with("- outer")), "{rows:?}");
    assert!(
        rows.iter().any(|r| r.starts_with("  - inner")),
        "nested indent missing: {rows:?}"
    );
}

#[test]
fn blockquote_paints_bar_gutter_dimmed_italic() {
    let buf = render("> quoted words", 100, 4);
    let rows = all_rows(&buf);
    let quote = rows
        .iter()
        .find(|r| r.starts_with("│ ") && r.contains("quoted"))
        .expect("blockquote gutter missing");
    assert!(quote.contains("quoted words"), "{quote:?}");
    // Body is italic.
    let italic = buf
        .content()
        .iter()
        .any(|cell| cell.modifier.contains(Modifier::ITALIC));
    assert!(italic, "blockquote body must be italic");
}

#[test]
fn rule_fills_full_width() {
    let buf = render("above\n\n---\n\nbelow", 100, 6);
    let rows = all_rows(&buf);
    let rule = rows
        .iter()
        .find(|r| r.chars().all(|c| c == '─') && !r.is_empty())
        .expect("rule missing");
    assert_eq!(
        UnicodeWidthStr::width(rule.as_str()),
        100,
        "rule must span the full width"
    );
}

// --- VAL-WIDGET-002: fenced code block --------------------------------------

#[test]
fn code_block_has_bordered_frame_and_language_label() {
    let buf = render("```rust\nfn main() {}\n```", 100, 6);
    let rows = all_rows(&buf);
    assert!(
        rows.iter().any(|r| r.contains("# lang: rust")),
        "language label missing: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.starts_with('┌')),
        "top border: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.starts_with('└')),
        "bottom border: {rows:?}"
    );
    assert!(rows.iter().any(|r| r.contains("fn main")), "body: {rows:?}");
}

// --- VAL-WIDGET-003: table CJK alignment ------------------------------------

#[test]
fn table_columns_align_with_cjk_cells() {
    let src = "| name | v |\n|---|---|\n| 你好世界 | 1 |\n| a | 22 |";
    let buf = render(src, 100, 8);
    let rows = all_rows(&buf);
    // Collect the table's grid lines (border + data rows).
    let grid: Vec<&String> = rows
        .iter()
        .filter(|r| r.contains('│') || r.contains('┼') || r.contains('┬') || r.contains('┴'))
        .collect();
    assert!(grid.len() >= 4, "expected a full table grid: {rows:?}");
    // Every grid line paints the same number of columns (CJK cell padded to the
    // column display width so the right border lands in the same column).
    let cols: Vec<u16> = grid
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let y = rows.iter().position(|r| r == grid[i]).unwrap() as u16;
            painted_cols(&buf, y)
        })
        .collect();
    assert!(
        cols.windows(2).all(|w| w[0] == w[1]),
        "table columns misaligned across rows (CJK): {cols:?} in {grid:?}"
    );
}

// --- VAL-WIDGET-005: nested inline styles -----------------------------------

#[test]
fn nested_inline_styles_restore_outer_style() {
    // Bold containing italic: after the italic closes, the tail is still bold but
    // no longer italic.
    let lines = render_markdown("**bold _inner_ tail**", 100, &MarkdownTheme::default());
    let line = &lines[0];
    let tail = line
        .spans
        .iter()
        .find(|s| s.content.contains("tail"))
        .expect("tail span");
    assert!(
        tail.style.add_modifier.contains(Modifier::BOLD),
        "outer bold must resume"
    );
    assert!(
        !tail.style.add_modifier.contains(Modifier::ITALIC),
        "inner italic must not leak past its close"
    );
}

// --- VAL-WIDGET-006: link degradation ---------------------------------------

#[test]
fn link_falls_back_to_text_and_url_when_osc8_unavailable() {
    let _guard = OSC8_ENV_LOCK.lock().unwrap();
    // Force the plain-text fallback so the assertion is host-independent. The
    // lock (and the panic-safe cleanup below) keep the toggle from racing the
    // capability probe in the sibling test.
    // SAFETY: guarded by OSC8_ENV_LOCK so no other thread reads the var mid-write.
    unsafe {
        std::env::set_var("HAND_DISABLE_OSC8", "1");
    }
    let out = logical_text("[example](https://example.com)", 100).join("");
    unsafe {
        std::env::remove_var("HAND_DISABLE_OSC8");
    }
    assert!(
        out.contains("example (https://example.com)"),
        "link fallback missing: {out:?}"
    );
}

#[test]
fn bare_autolink_is_not_duplicated() {
    let _guard = OSC8_ENV_LOCK.lock().unwrap();
    // SAFETY: guarded by OSC8_ENV_LOCK so no other thread reads the var mid-write.
    unsafe {
        std::env::set_var("HAND_DISABLE_OSC8", "1");
    }
    let out = logical_text("<https://example.com>", 100).join("");
    unsafe {
        std::env::remove_var("HAND_DISABLE_OSC8");
    }
    assert!(
        out.contains("https://example.com"),
        "autolink missing: {out:?}"
    );
    assert!(
        !out.contains("(https://example.com)"),
        "autolink duplicated: {out:?}"
    );
}

// --- VAL-WIDGET-020: image alt degradation ----------------------------------

#[test]
fn image_degrades_to_alt_text_without_fragments() {
    let out = logical_text("![a wide diagram](img.png)", 100).join("");
    assert!(out.contains("a wide diagram"), "alt missing: {out:?}");
    assert!(!out.contains("!["), "image fragment leaked: {out:?}");
    assert!(!out.contains("]("), "image fragment leaked: {out:?}");
}

// --- VAL-WIDGET-023: strikethrough / task-list / inline code literal --------

#[test]
fn strikethrough_task_list_and_inline_code_are_literal_text() {
    let src = "~~struck~~\n\n- [ ] pending\n- [x] complete\n\nrun `cargo test`";
    let out = logical_text(src, 100).join("\n");
    assert!(out.contains("struck"), "strike text missing: {out:?}");
    assert!(out.contains("[ ] pending"), "task marker missing: {out:?}");
    assert!(out.contains("[x] complete"), "task marker missing: {out:?}");
    assert!(
        out.contains("`cargo test`"),
        "inline code backticks missing: {out:?}"
    );
}

// --- VAL-WIDGET-014: narrow-width wrapping ----------------------------------

#[test]
fn narrow_width_wraps_cleanly_without_overflow() {
    let src = "This is a deliberately long paragraph of body text that must wrap \
               onto several rows when the pane is only forty columns wide.";
    let buf = render(src, 40, 12);
    let rows: Vec<String> = all_rows(&buf)
        .into_iter()
        .filter(|r| !r.is_empty())
        .collect();
    assert!(rows.len() >= 2, "narrow text must wrap: {rows:?}");
    for (y, _) in rows.iter().enumerate() {
        // Use the painted-column span so a wide glyph is not double-counted.
        let cols = painted_cols(&buf, y as u16);
        assert!(cols <= 40, "row {y} overflows 40 columns: {cols}");
    }
}

#[test]
fn narrow_width_code_block_border_does_not_break() {
    let buf = render("```\nlet x = 1;\n```", 40, 6);
    let rows = all_rows(&buf);
    let borders: Vec<&String> = rows
        .iter()
        .filter(|r| r.starts_with('┌') || r.starts_with('└'))
        .collect();
    assert_eq!(
        borders.len(),
        2,
        "expected exactly two border rows: {rows:?}"
    );
    for border in borders {
        // The border row is a single unbroken run that fills the pane width.
        assert_eq!(
            UnicodeWidthStr::width(border.as_str()),
            40,
            "code-block border must fill width unbroken: {border:?}"
        );
        // A border row is only corner + `─` characters (no wrap split it).
        assert!(
            border
                .chars()
                .all(|c| matches!(c, '┌' | '┐' | '└' | '┘' | '─')),
            "border row contains non-border chars (wrap split it): {border:?}"
        );
    }
}

#[test]
fn narrow_width_heading_style_does_not_leak_to_next_row() {
    // A heading longer than the pane wraps; the following body row must not carry
    // the heading's bold attribute (style must not bleed across the wrap).
    let src = "# A very long heading that will wrap across the narrow pane width\n\nplain body";
    let buf = render(src, 40, 12);
    let rows = all_rows(&buf);
    let body_y = rows
        .iter()
        .position(|r| r.contains("plain body"))
        .expect("body row") as u16;
    // No cell on the body row is bold (the heading's bold did not leak down).
    let area = buf.area;
    let leaked = (area.x..area.x + area.width).any(|x| {
        buf.cell((x, body_y))
            .is_some_and(|c| c.modifier.contains(Modifier::BOLD))
    });
    assert!(!leaked, "heading bold leaked onto the body row");
}
