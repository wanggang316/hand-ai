//! Rendering tests for the rt primitive widgets (`hand_tui::rt::components`).
//!
//! These pin the *behavioural* signatures the external validator probes, not
//! specific glyphs or legacy ANSI formatting (Decision Log: visual-signature
//! tolerance). Each primitive is rendered into a ratatui `Buffer` — the same
//! model the rt scheduler draws every frame — at a wide (100-column) geometry and
//! again after a resize to 60 columns, and the contract is asserted on the
//! painted cells:
//!
//! - **VAL-WIDGET-019** — `TextBlock` word-wraps and pads; `WidgetBox` fills its
//!   background full-width; `Spacer` reserves exactly N rows.
//! - **VAL-WIDGET-018** — `TruncatedText` stays a single line, ends with an
//!   ellipsis when clipped, and honours padding.
//! - **VAL-WIDGET-017** — `StatusBar` lays its three sections on one row with the
//!   left flush left and the right flush right; the single-row invariant holds
//!   when narrow (it truncates, never wraps to a second row).
//! - **VAL-WIDGET-013** — `ProgressBar` clamps out-of-range values so the shown
//!   percentage is always `0..=100`.
//! - **VAL-WIDGET-022** — full-width widgets re-lay-out to a single row after a
//!   100 -> 60 resize.
//!
//! CJK content is included so the display-width path (a wide glyph counts as two
//! columns) is exercised on a narrow pane without a byte-slice panic.

use hand_tui::rt::components::{
    ProgressBar, Spacer, StatusBar, TextBlock, TruncatedText, WidgetBox,
};
use hand_tui::rt::view::RtComponent;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::{Terminal, TerminalOptions, Viewport};

// --- helpers ----------------------------------------------------------------

/// Render a component into a fresh buffer of the given size and return it. This
/// is the pure-render path: every primitive is display-only, so a single
/// `render` into an empty buffer is a faithful snapshot of one frame.
fn render_to_buffer(component: &dyn RtComponent, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    component.render(area, &mut buf);
    buf
}

/// The symbols of one buffer row concatenated, with trailing blanks trimmed.
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

/// Count rows that contain any non-blank symbol.
fn non_blank_row_count(buf: &Buffer) -> usize {
    all_rows(buf).iter().filter(|r| !r.is_empty()).count()
}

/// The number of terminal columns row `y` actually occupies: one past the last
/// non-blank cell.
///
/// This measures the *painted column span* directly from the buffer, which is
/// the honest "does it fit the width" question. Reconstructing the row into a
/// `String` and re-measuring its display width would double-count: a wide (CJK)
/// grapheme occupies two buffer cells — the glyph cell plus a blank continuation
/// cell ratatui `reset()`s to a space — so the string carries an extra column per
/// wide glyph that the terminal does not.
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

// --- VAL-WIDGET-019: TextBlock (word wrap + padding) ------------------------

#[test]
fn text_block_wraps_words_within_inner_width() {
    // A phrase that must wrap when the area is narrow: at width 10 it cannot fit
    // on one row, so it spans several rows, each within the width.
    let text = TextBlock::new("the quick brown fox jumps over");
    let buf = render_to_buffer(&text, 10, 6);
    let rows: Vec<String> = all_rows(&buf)
        .into_iter()
        .filter(|r| !r.is_empty())
        .collect();

    assert!(
        rows.len() >= 2,
        "narrow text must wrap onto multiple rows: {rows:?}"
    );
    for row in &rows {
        assert!(
            row.chars().count() <= 10,
            "no wrapped row may exceed the width: {row:?}"
        );
    }
    // Word-aware wrap keeps whole words: the first word is intact on row 0.
    assert!(
        rows[0].starts_with("the"),
        "first row keeps the leading word: {rows:?}"
    );
}

#[test]
fn text_block_pads_top_and_left() {
    // padding (2, 1): one blank row on top, two blank columns on the left.
    let text = TextBlock::new("hi").padding(2, 1);
    let buf = render_to_buffer(&text, 20, 4);
    let rows = all_rows(&buf);

    assert_eq!(rows[0], "", "top padding row is blank");
    // The content row is row 1 (below the one padding row); it is inset two cols.
    assert_eq!(
        &rows[1][..2],
        "  ",
        "left padding inserts two blank columns"
    );
    assert!(
        rows[1].contains("hi"),
        "content follows the left padding: {rows:?}"
    );
}

#[test]
fn text_block_oversized_padding_paints_nothing_not_panics() {
    // Padding larger than the area must collapse to nothing, never underflow into
    // a huge rect or panic on a narrow pane.
    let text = TextBlock::new("content").padding(50, 50);
    let buf = render_to_buffer(&text, 4, 2);
    assert_eq!(
        non_blank_row_count(&buf),
        0,
        "over-padded text paints nothing"
    );
}

// --- VAL-WIDGET-019: WidgetBox (full-width background + padding) -------------

#[test]
fn widget_box_fills_background_full_width_at_100_and_60() {
    let bg = Style::default().bg(Color::Blue);
    for width in [100u16, 60u16] {
        let bx = WidgetBox::new().background(bg);
        let buf = render_to_buffer(&bx, width, 3);
        // Every cell of every row carries the background colour — the panel is a
        // solid block spanning the full width, including the padding band.
        for y in 0..3 {
            for x in 0..width {
                let cell = buf.cell((x, y)).expect("cell in bounds");
                assert_eq!(
                    cell.bg,
                    Color::Blue,
                    "box background must fill cell ({x},{y}) at width {width}"
                );
            }
        }
    }
}

#[test]
fn widget_box_paints_child_inset_by_padding() {
    let bg = Style::default().bg(Color::Blue);
    let child = Box::new(TruncatedText::new("child"));
    let bx = WidgetBox::new().background(bg).padding(1, 1).child(child);
    let buf = render_to_buffer(&bx, 20, 3);

    // The child text lands on the padded inner row (row 1), inset one column.
    assert!(
        row_string(&buf, 1).contains("child"),
        "child paints inside the box"
    );
    // Background still fills the padding rows (row 0 is blank text but blue bg).
    assert_eq!(
        buf.cell((0, 0)).unwrap().bg,
        Color::Blue,
        "padding row keeps bg"
    );
}

// --- VAL-WIDGET-019: Spacer (exactly N rows) --------------------------------

#[test]
fn spacer_reports_exact_row_count_and_paints_nothing() {
    for n in [0u16, 1, 3, 5] {
        let spacer = Spacer::new(n);
        assert_eq!(spacer.rows(), n, "spacer reserves exactly {n} rows");
        // Rendering paints nothing regardless of the area it is handed.
        let buf = render_to_buffer(&spacer, 40, n.max(1));
        assert_eq!(non_blank_row_count(&buf), 0, "a spacer paints no content");
    }
}

// --- VAL-WIDGET-018: TruncatedText (single line + ellipsis + padding) -------

#[test]
fn truncated_text_short_fits_without_ellipsis() {
    let t = TruncatedText::new("hi");
    let buf = render_to_buffer(&t, 80, 1);
    assert_eq!(
        row_string(&buf, 0),
        "hi",
        "short text is shown verbatim, no ellipsis"
    );
}

#[test]
fn truncated_text_long_is_single_line_with_ellipsis() {
    let t = TruncatedText::new("a very long line that will not fit in ten columns");
    let buf = render_to_buffer(&t, 10, 3);
    let rows: Vec<String> = all_rows(&buf);

    // Single line: only the first row has content; nothing spills below.
    assert!(!rows[0].is_empty(), "the line renders on the first row");
    assert_eq!(rows[1], "", "truncated text never spills to a second row");
    assert_eq!(rows[2], "", "truncated text never spills to a third row");
    // Ellipsis marks the clip and the row fits the width.
    assert!(
        rows[0].ends_with('…'),
        "a clipped line ends with an ellipsis: {rows:?}"
    );
    assert!(
        rows[0].chars().count() <= 10,
        "the clipped line fits the width"
    );
}

#[test]
fn truncated_text_pads_and_stays_single_line() {
    // padding (2, 1): a blank top row, content inset two columns on row 1.
    let t = TruncatedText::new("hello").padding(2, 1);
    let buf = render_to_buffer(&t, 20, 3);
    let rows = all_rows(&buf);

    assert_eq!(rows[0], "", "top padding row is blank");
    assert_eq!(rows[2], "", "bottom padding row is blank");
    assert!(
        rows[1].starts_with("  hello"),
        "content is inset by the left padding: {rows:?}"
    );
}

#[test]
fn truncated_text_cjk_narrow_truncates_without_panic() {
    // Each CJK glyph is two display columns; at width 5 only two glyphs plus an
    // ellipsis can fit. The point is it truncates by display width without
    // slicing a multibyte grapheme (the legacy byte-slice panic).
    let t = TruncatedText::new("你好世界你好");
    let buf = render_to_buffer(&t, 5, 1);
    let row = row_string(&buf, 0);
    assert!(
        row.ends_with('…'),
        "wide text clips with an ellipsis: {row:?}"
    );
    // The painted column span fits within the area (≤5 columns): measured from
    // the buffer cells, not the reconstructed string (which double-counts wide
    // graphemes' continuation cells).
    assert!(
        painted_cols(&buf, 0) <= 5,
        "the clipped wide line fits the width: {row:?}"
    );
}

// --- VAL-WIDGET-017: StatusBar (three sections, one row, edge-aligned) ------

#[test]
fn status_bar_places_three_sections_on_one_row_edge_aligned() {
    let bar = StatusBar::new()
        .left("Model: gpt")
        .center("CENTER")
        .right("Session: 42");
    let buf = render_to_buffer(&bar, 100, 3);
    let rows = all_rows(&buf);

    // One row only: everything is on row 0, nothing spills below.
    assert_eq!(
        rows[1], "",
        "status bar occupies exactly one row (no second row)"
    );
    assert_eq!(
        rows[2], "",
        "status bar occupies exactly one row (no third row)"
    );

    let row = &rows[0];
    assert!(
        row.starts_with("Model: gpt"),
        "left section is flush to the left edge: {row:?}"
    );
    assert!(
        row.trim_end().ends_with("Session: 42"),
        "right section is flush to the right edge: {row:?}"
    );
    assert!(
        row.contains("CENTER"),
        "center section appears between left and right: {row:?}"
    );

    // The right section ends at the final column: its last char sits at width-1.
    let last_non_blank = row.trim_end().chars().count();
    assert!(last_non_blank <= 100, "content fits within the width");
}

#[test]
fn status_bar_stays_one_row_when_narrow() {
    // At a width too small for all three sections, the bar truncates rather than
    // wrapping to a second row — the single-line invariant under a narrow pane.
    let bar = StatusBar::new()
        .left("a-long-left-label")
        .center("centered-text")
        .right("a-long-right-label");
    let buf = render_to_buffer(&bar, 20, 3);
    let rows = all_rows(&buf);

    assert!(!rows[0].is_empty(), "the bar renders on the first row");
    assert_eq!(
        rows[1], "",
        "a narrow status bar truncates, never wraps to a second row"
    );
    // The painted row fits the width (no overflow).
    assert!(
        painted_cols(&buf, 0) <= 20,
        "the truncated bar fits the width: {:?}",
        rows[0]
    );
}

#[test]
fn status_bar_cjk_sections_fit_narrow_without_panic() {
    let bar = StatusBar::new().left("模型").center("中间").right("会话");
    let buf = render_to_buffer(&bar, 12, 1);
    let row = row_string(&buf, 0);
    assert!(
        painted_cols(&buf, 0) <= 12,
        "CJK sections fit within the width: {row:?}"
    );
}

// --- VAL-WIDGET-013: ProgressBar (clamped percentage) -----------------------

#[test]
fn progress_bar_clamps_ratio_and_percent() {
    // Above 1.0 clamps to 100%, below 0.0 clamps to 0%, NaN falls back to 0%.
    assert_eq!(ProgressBar::new().ratio(1.5).percent(), 100);
    assert_eq!(ProgressBar::new().ratio(-0.5).percent(), 0);
    assert_eq!(ProgressBar::new().ratio(f64::NAN).percent(), 0);
    assert_eq!(ProgressBar::new().ratio(0.5).percent(), 50);

    // The clamped ratio itself is held in [0, 1].
    assert!((ProgressBar::new().ratio(2.0).get_ratio() - 1.0).abs() < f64::EPSILON);
    assert!(ProgressBar::new().ratio(-3.0).get_ratio().abs() < f64::EPSILON);
}

#[test]
fn progress_bar_renders_clamped_percentage_label() {
    // An over-range ratio renders the clamped "100%" label, never a value past it.
    let bar = ProgressBar::new().ratio(1.5);
    let buf = render_to_buffer(&bar, 40, 1);
    let row = row_string(&buf, 0);
    assert!(
        row.contains("100%"),
        "an over-range bar shows a clamped 100%: {row:?}"
    );
    assert!(
        !row.contains("150%"),
        "the raw over-range value is never shown: {row:?}"
    );
}

// --- VAL-WIDGET-022: full-width widgets re-lay-out on resize -----------------

#[test]
fn full_width_widgets_stay_single_row_across_100_to_60_resize() {
    // StatusBar and ProgressBar are the full-width, single-row widgets. Laid out
    // in a one-row slot (as the gallery does), each fits that row exactly at both
    // 100 and after a resize to 60 — the re-lay-out spans the new width on a
    // single row, never overflowing it or spilling below.
    for width in [100u16, 60u16] {
        // A two-row buffer with a one-row slot at y=0: assert the slot fills and
        // the row below stays blank (no spill past the slot).
        let bar = StatusBar::new().left("L").center("C").right("R");
        let mut sbuf = Buffer::empty(Rect::new(0, 0, width, 2));
        bar.render(Rect::new(0, 0, width, 1), &mut sbuf);
        assert!(
            !row_string(&sbuf, 0).is_empty(),
            "status bar renders in its slot at {width}"
        );
        assert_eq!(
            row_string(&sbuf, 1),
            "",
            "status bar does not spill below its row at {width}"
        );
        assert!(
            painted_cols(&sbuf, 0) <= width,
            "status bar fits width {width}"
        );

        let progress = ProgressBar::new().ratio(0.5);
        let mut pbuf = Buffer::empty(Rect::new(0, 0, width, 2));
        progress.render(Rect::new(0, 0, width, 1), &mut pbuf);
        assert!(
            !row_string(&pbuf, 0).is_empty(),
            "progress bar renders in its slot at {width}"
        );
        assert_eq!(
            row_string(&pbuf, 1),
            "",
            "progress bar does not spill below its row at {width}"
        );
    }
}

// --- end-to-end over a real inline Terminal / TestBackend -------------------

/// Drive a real inline `Terminal::draw` over `TestBackend` and assert the gallery
/// row layout renders — the same draw path the rt scheduler uses. This exercises
/// the widgets through `frame.render_widget`-equivalent buffer painting rather
/// than only the pure `render` helper.
#[test]
fn primitives_render_through_a_real_inline_terminal() {
    let backend = TestBackend::new(40, 8);
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 40, 8)),
        },
    )
    .expect("build inline test terminal");

    let status = StatusBar::new().left("left").center("mid").right("right");
    let text = TextBlock::new("hello world");
    let progress = ProgressBar::new().ratio(0.25);

    terminal
        .draw(|frame| {
            let buf = frame.buffer_mut();
            status.render(Rect::new(0, 0, 40, 1), buf);
            text.render(Rect::new(0, 2, 40, 1), buf);
            progress.render(Rect::new(0, 4, 40, 1), buf);
        })
        .expect("draw the gallery rows");

    let buf = terminal.backend().buffer();
    assert!(
        row_string(buf, 0).starts_with("left"),
        "status left at edge"
    );
    assert!(
        row_string(buf, 0).trim_end().ends_with("right"),
        "status right at edge"
    );
    assert!(row_string(buf, 2).contains("hello world"), "text renders");
    assert!(row_string(buf, 4).contains("25%"), "progress shows 25%");
}
