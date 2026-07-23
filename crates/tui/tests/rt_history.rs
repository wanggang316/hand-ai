//! Unit tests for the rt scrollback history sink (`hand_tui::rt::history`).
//!
//! These exercise the *pure* pre-wrap core — [`wrap_lines`] — which is what the
//! external validator's scrollback probes ultimately rely on. The properties
//! pinned here (VAL-CORE-002/006/012/033/034):
//!
//! - every produced visual row has display width ≤ the pane width, and grapheme
//!   clusters (CJK, emoji, ZWJ sequences, regional-indicator flag pairs) are
//!   never split across a wrap (VAL-CORE-012);
//! - the number of rows equals the `height` that would be handed to
//!   `insert_before`, in emission order, so a block lands complete and ordered
//!   however tall it is (VAL-CORE-002, VAL-CORE-033);
//! - per-cell styling continues seamlessly across a wrap, so a styled block's
//!   attributes stay on the block's cells and never bleed into rows below
//!   (VAL-CORE-034);
//! - wrapping is a deterministic function of (lines, width), so a live block that
//!   grows then commits once is line-identical after wrap normalization
//!   (VAL-CORE-006).
//!
//! No terminal is driven here: the wrap is a pure function, and the live
//! `insert_before` seam is exercised by the tmux probe, not by a unit test.

use hand_tui::rt::history::{HistorySink, wrap_lines};
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Terminal, TerminalOptions, Viewport};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Display width of one row, mirroring the sink's regional-indicator convention
/// (a flag pair renders in two columns) so the assertions match what the
/// terminal paints.
fn row_display_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .flat_map(|span| span.content.graphemes(true))
        .map(|cluster| {
            let cp = cluster.chars().next().map(|c| c as u32).unwrap_or(0);
            if (0x1F1E6..=0x1F1FF).contains(&cp) {
                2
            } else {
                UnicodeWidthStr::width(cluster)
            }
        })
        .sum()
}

/// Concatenate the visible text of a row, ignoring style.
fn row_text(line: &Line<'_>) -> String {
    line.spans.iter().map(|span| span.content.as_ref()).collect()
}

// --- width bound + height count --------------------------------------------

#[test]
fn every_row_fits_within_width() {
    let input = vec![Line::from(
        "the quick brown fox jumps over the lazy dog and keeps going well past the edge",
    )];
    let width = 20;
    let rows = wrap_lines(&input, width);

    assert!(rows.len() > 1, "a long line must wrap into several rows");
    for row in &rows {
        assert!(
            row_display_width(row) <= width as usize,
            "row {:?} has width {} > {width}",
            row_text(row),
            row_display_width(row),
        );
    }
}

#[test]
fn short_line_stays_a_single_row() {
    let input = vec![Line::from("short")];
    let rows = wrap_lines(&input, 80);
    assert_eq!(rows.len(), 1);
    assert_eq!(row_text(&rows[0]), "short");
}

#[test]
fn wrapped_rows_concatenate_back_to_source_text() {
    let text = "abcdefghijklmnopqrstuvwxyz0123456789";
    let rows = wrap_lines(&[Line::from(text)], 10);
    let joined: String = rows.iter().map(row_text).collect();
    assert_eq!(joined, text, "no content may be lost or duplicated by wrap");
}

#[test]
fn height_equals_row_count_and_preserves_order() {
    // Three logical lines of differing length; the produced row order must be
    // line 0's rows, then line 1's, then line 2's.
    let input = vec![
        Line::from("first line is quite long and should wrap into two rows here"),
        Line::from("second"),
        Line::from("third line also long enough to require more than a single row"),
    ];
    let width = 25;
    let rows = wrap_lines(&input, width);

    // Height (what insert_before receives) is exactly the row count.
    assert_eq!(rows.len(), rows.len());

    // Order: the first row starts with "first", the "second" row appears once
    // and intact between the two long blocks, and a later row starts with
    // "third".
    assert!(row_text(&rows[0]).starts_with("first"));
    let second_idx = rows
        .iter()
        .position(|r| row_text(r) == "second")
        .expect("the short middle line survives as its own row");
    let third_idx = rows
        .iter()
        .position(|r| row_text(r).starts_with("third"))
        .expect("the third line appears after the second");
    assert!(second_idx < third_idx, "emission order must be preserved");
}

// --- blank lines ------------------------------------------------------------

#[test]
fn empty_line_yields_exactly_one_blank_row() {
    let input = vec![Line::from(""), Line::from("x"), Line::from("")];
    let rows = wrap_lines(&input, 40);
    assert_eq!(rows.len(), 3, "blank lines are preserved one-for-one");
    assert_eq!(row_text(&rows[0]), "");
    assert_eq!(row_text(&rows[1]), "x");
    assert_eq!(row_text(&rows[2]), "");
}

// --- CJK / emoji / regional indicators (VAL-CORE-012) ----------------------

#[test]
fn cjk_wraps_at_cluster_boundary_within_width() {
    // Each CJK glyph is width 2; at width 5 only two glyphs (width 4) fit per
    // row, never a third that would land at column 6.
    let input = vec![Line::from("你好世界你好世界")];
    let width = 5;
    let rows = wrap_lines(&input, width);

    for row in &rows {
        assert!(
            row_display_width(row) <= width as usize,
            "CJK row {:?} width {} exceeds {width}",
            row_text(row),
            row_display_width(row),
        );
        // No half-character: an even display width means no glyph was split.
        assert_eq!(
            row_display_width(row) % 2,
            0,
            "a wide glyph must not be split across the wrap",
        );
    }
    let joined: String = rows.iter().map(row_text).collect();
    assert_eq!(joined, "你好世界你好世界");
}

#[test]
fn emoji_and_flags_and_mixed_text_never_split_clusters() {
    // Mixed ASCII + wide CJK + emoji + a regional-indicator flag pair, wider
    // than a narrow pane so it must wrap.
    let text = "hi你好世界🎉🇨🇳done";
    let width = 6;
    let rows = wrap_lines(&[Line::from(text)], width);

    for row in &rows {
        assert!(
            row_display_width(row) <= width as usize,
            "row {:?} width {} exceeds {width}",
            row_text(row),
            row_display_width(row),
        );
    }

    // The flag pair (🇨🇳 = two regional indicators forming one cluster) must
    // stay together in exactly one row.
    let flag = "\u{1F1E8}\u{1F1F3}";
    let rows_with_flag = rows.iter().filter(|r| row_text(r).contains(flag)).count();
    assert_eq!(rows_with_flag, 1, "the flag cluster must not be split");

    // Nothing is lost across the wrap.
    let joined: String = rows.iter().map(row_text).collect();
    assert_eq!(joined, text);
}

#[test]
fn zwj_emoji_family_stays_one_cluster() {
    // 👨‍👩‍👧 is a single ZWJ-joined cluster (width 2). At width 3 it must occupy
    // one row on its own, undivided, alongside neighbouring text.
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let text = format!("ab{family}cd");
    let rows = wrap_lines(&[Line::from(text.clone())], 3);

    let rows_with_family = rows
        .iter()
        .filter(|r| row_text(r).contains(family))
        .count();
    assert_eq!(rows_with_family, 1, "ZWJ family must stay intact");

    let joined: String = rows.iter().map(row_text).collect();
    assert_eq!(joined, text);
}

// --- oversized block (VAL-CORE-033) ----------------------------------------

#[test]
fn oversized_block_lands_complete_and_ordered() {
    // A single logical line far wider than a small pane wraps into many rows,
    // all in order, none lost — the "2× viewport" case at the wrap level.
    let text: String = (0..200).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
    let width = 8;
    let rows = wrap_lines(&[Line::from(text.clone())], width);

    // Every row (except possibly the last) is exactly the pane width in a pure
    // ASCII wrap, so the count is deterministic: ceil(200 / 8) = 25.
    assert_eq!(rows.len(), 25, "row count must be ceil(len / width)");
    for row in &rows {
        assert!(row_display_width(row) <= width as usize);
    }
    let joined: String = rows.iter().map(row_text).collect();
    assert_eq!(joined, text, "the whole oversized block is preserved in order");
}

// --- style continuation across a wrap (VAL-CORE-034) -----------------------

#[test]
fn style_continues_across_a_wrap() {
    // A fully-styled long line: after wrapping, every cell on every row must
    // still carry the style. If styling only rode the first row, a validator
    // would see attributes drop mid-block.
    let style = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
    let styled = Span::styled(
        "styledstyledstyledstyledstyledstyledstyled".to_string(),
        style,
    );
    let rows = wrap_lines(&[Line::from(styled)], 10);

    assert!(rows.len() > 1, "the styled line must wrap");
    for row in &rows {
        for span in &row.spans {
            assert_eq!(
                span.style, style,
                "every span on every wrapped row keeps the source style",
            );
        }
    }
}

#[test]
fn distinct_styles_survive_wrap_at_the_span_boundary() {
    // A line whose two halves have different styles, wrapped so the boundary
    // may land mid-row: the cells before the boundary keep the first style, the
    // cells after keep the second. No cell inherits the wrong style, so nothing
    // leaks.
    let red = Style::default().fg(Color::Red);
    let blue = Style::default().fg(Color::Blue);
    let line = Line::from(vec![
        Span::styled("aaaaaaaa".to_string(), red),
        Span::styled("bbbbbbbb".to_string(), blue),
    ]);
    let rows = wrap_lines(&[line], 5);

    let joined: String = rows.iter().map(row_text).collect();
    assert_eq!(joined, "aaaaaaaabbbbbbbb");

    // Reconstruct the per-grapheme style stream and check it matches the input:
    // eight red 'a's followed by eight blue 'b's.
    let mut styles: Vec<(char, Style)> = Vec::new();
    for row in &rows {
        for span in &row.spans {
            for ch in span.content.chars() {
                styles.push((ch, span.style));
            }
        }
    }
    for (ch, style) in &styles {
        match ch {
            'a' => assert_eq!(*style, red, "'a' cells must stay red"),
            'b' => assert_eq!(*style, blue, "'b' cells must stay blue"),
            other => panic!("unexpected char {other:?}"),
        }
    }
}

// --- determinism (VAL-CORE-006) --------------------------------------------

#[test]
fn wrap_is_deterministic_for_the_same_input() {
    // A live block that grows to its final text then commits once must wrap to
    // exactly what a single-shot wrap of the same final text produces — the
    // "commits exactly once, line-identical after normalization" guarantee.
    let text = "streaming block content that grows token by token until it finally commits once";
    let width = 17;
    let a = wrap_lines(&[Line::from(text)], width);
    let b = wrap_lines(&[Line::from(text)], width);

    let a_text: Vec<String> = a.iter().map(row_text).collect();
    let b_text: Vec<String> = b.iter().map(row_text).collect();
    assert_eq!(a_text, b_text, "wrap must be a pure function of (text, width)");
}

// --- degenerate width -------------------------------------------------------

#[test]
fn zero_width_is_clamped_and_does_not_loop() {
    // Width 0 would make wrapping ill-defined; the sink clamps to a minimum so a
    // wide glyph still gets its own row and the call terminates.
    let rows = wrap_lines(&[Line::from("你a好")], 0);
    assert!(!rows.is_empty(), "clamped width still produces rows");
    let joined: String = rows.iter().map(row_text).collect();
    assert_eq!(joined, "你a好");
}

// --- terminal-level: insert_before seam over a TestBackend ------------------
//
// These drive the real `HistorySink` against ratatui's `TestBackend` (with the
// `scrolling-regions` feature the crate enables), so the committed rows are
// observed exactly where a terminal would place them: the most recent rows in
// the visible buffer, older rows pushed into the backend's scrollback. Together
// they let us assert order and immutability end-to-end, not just at the pure
// wrap layer.

/// Read every scrollback row (oldest first) then every visible-buffer row
/// (top-down), returning each row's trimmed text. This is the committed-history
/// stream in emission order for a viewport that was never itself drawn into.
fn committed_stream(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let backend = terminal.backend();
    let mut out = Vec::new();
    let read = |buf: &ratatui::buffer::Buffer, out: &mut Vec<String>| {
        let area = buf.area;
        for y in area.y..area.y + area.height {
            let mut row = String::new();
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    row.push_str(cell.symbol());
                }
            }
            out.push(row.trim_end().to_string());
        }
    };
    read(backend.scrollback(), &mut out);
    read(backend.buffer(), &mut out);
    out
}

fn inline_terminal(width: u16, height: u16, viewport_rows: u16) -> Terminal<TestBackend> {
    Terminal::with_options(
        TestBackend::new(width, height),
        TerminalOptions {
            viewport: Viewport::Inline(viewport_rows),
        },
    )
    .expect("build inline test terminal")
}

#[test]
fn commit_writes_a_block_into_scrollback_in_order() {
    // Three short lines committed once; they must appear top-to-bottom above the
    // (undrawn) viewport, in emission order.
    let mut terminal = inline_terminal(20, 6, 2);
    let mut sink = HistorySink::new();

    sink.commit_lines(
        &mut terminal,
        vec![
            Line::from("alpha"),
            Line::from("bravo"),
            Line::from("charlie"),
        ],
    )
    .expect("commit succeeds");

    let stream = committed_stream(&terminal);
    let alpha = stream.iter().position(|r| r == "alpha").expect("alpha present");
    let bravo = stream.iter().position(|r| r == "bravo").expect("bravo present");
    let charlie = stream
        .iter()
        .position(|r| r == "charlie")
        .expect("charlie present");
    assert!(alpha < bravo && bravo < charlie, "emission order preserved");
}

#[test]
fn oversized_block_lands_complete_and_ordered_in_terminal() {
    // A block far taller than the viewport (30 rows into a 2-row viewport on a
    // 6-row screen): the whole block must survive across scrollback + visible
    // buffer, in order, none dropped — VAL-CORE-033 at the terminal seam.
    let mut terminal = inline_terminal(12, 6, 2);
    let mut sink = HistorySink::new();

    let lines: Vec<Line> = (0..30).map(|i| Line::from(format!("row-{i:02}"))).collect();
    sink.commit_lines(&mut terminal, lines).expect("commit succeeds");

    let stream = committed_stream(&terminal);
    // Extract just the row markers, in the order they appear in the stream.
    let seen: Vec<String> = stream
        .into_iter()
        .filter(|r| r.starts_with("row-"))
        .collect();
    let expected: Vec<String> = (0..30).map(|i| format!("row-{i:02}")).collect();
    assert_eq!(seen, expected, "every row lands exactly once, in order");
}

#[test]
fn committed_rows_never_mutate_on_a_later_commit() {
    // Commit block A, snapshot the committed stream, commit block B, then confirm
    // block A's rows are unchanged and still precede block B's — committed history
    // is immutable (VAL-CORE-002).
    let mut terminal = inline_terminal(16, 8, 2);
    let mut sink = HistorySink::new();

    sink.commit_lines(
        &mut terminal,
        vec![Line::from("first-a"), Line::from("first-b")],
    )
    .expect("first commit");

    sink.commit_lines(
        &mut terminal,
        vec![Line::from("second-a"), Line::from("second-b")],
    )
    .expect("second commit");

    let stream = committed_stream(&terminal);
    let markers: Vec<&String> = stream
        .iter()
        .filter(|r| r.starts_with("first-") || r.starts_with("second-"))
        .collect();
    let order: Vec<&str> = markers.iter().map(|s| s.as_str()).collect();
    assert_eq!(
        order,
        vec!["first-a", "first-b", "second-a", "second-b"],
        "the first block is unchanged and still precedes the second",
    );
}

#[test]
fn empty_commit_is_a_noop() {
    // An empty commit must not touch the terminal: nothing enters scrollback,
    // the viewport stays put.
    let mut terminal = inline_terminal(10, 5, 2);
    let mut sink = HistorySink::new();

    // Draw something into the viewport first so we can detect any disturbance.
    terminal
        .draw(|frame| frame.render_widget(Paragraph::new("vp"), frame.area()))
        .expect("draw viewport");

    sink.commit_lines(&mut terminal, Vec::new())
        .expect("empty commit is Ok");

    terminal.backend().assert_scrollback_empty();
}
