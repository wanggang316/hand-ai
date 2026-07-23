//! Scrollback history insertion.
//!
//! Moves finalized output above the live inline viewport into the terminal's
//! native scrollback. Content is committed through
//! [`Terminal::insert_before`](ratatui::Terminal::insert_before), which — with
//! the `scrolling-regions` feature enabled — uses terminal scroll regions so the
//! viewport does not flicker.
//!
//! # Why pre-wrapping is mandatory
//!
//! `insert_before(height, F)` takes a **fixed** `height: u16` and hands the
//! closure a `height × viewport_width` [`Buffer`]. It does **no** wrapping of its
//! own (ratatui#1365): anything a `Line` renders past the buffer width is simply
//! truncated, and any height mismatch either clips the tail or leaves blank rows.
//! So the caller must wrap every logical line to the current width **first**,
//! then pass the total number of resulting visual rows as `height`.
//!
//! [`wrap_lines`] is that pre-wrap step, kept as a pure function so its
//! correctness (grapheme-cluster boundaries, CJK/emoji/regional-indicator width,
//! style continuation across a wrap) is unit-tested without a live terminal.
//! [`HistorySink::commit_lines`] is the thin terminal-facing wrapper that wraps,
//! computes the height, and performs the single `insert_before`.
//!
//! # Style fidelity
//!
//! Wrapping is done at the *styled grapheme* level: each logical [`Line`] is
//! decomposed into `(symbol, Style)` cells via
//! [`Line::styled_graphemes`](ratatui::text::Line::styled_graphemes), packed into
//! visual rows by display width, and each row is rebuilt as an owned
//! `Line<'static>` whose spans coalesce runs of equal style. Because every cell
//! carries its own resolved style, a styled block leaves no attribute "leaking"
//! into the rows below it — the buffer cells past the block are simply never
//! written by this block.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// Minimum width we will ever wrap to. A zero or absurdly small width would make
/// wrapping ill-defined (a single wide grapheme cannot fit), so we clamp up: at
/// width 1 a width-2 grapheme still gets its own row rather than looping forever.
const MIN_WRAP_WIDTH: u16 = 1;

/// Display width of a single grapheme cluster in terminal columns.
///
/// Uses `unicode-width` over the whole cluster so ZWJ emoji, variation selectors,
/// and combining marks collapse to their base width. Isolated regional
/// indicators (and flag pairs) are pinned to width 2 to match how terminals
/// actually render them, mirroring the legacy renderer's convention and avoiding
/// auto-wrap drift on flag emoji.
fn grapheme_width(cluster: &str) -> usize {
    let mut chars = cluster.chars();
    if let Some(first) = chars.next() {
        let cp = first as u32;
        if (0x1F1E6..=0x1F1FF).contains(&cp) {
            // Regional indicator: single indicator or a two-indicator flag both
            // render in two columns.
            return 2;
        }
    }
    // `UnicodeWidthStr::width` already sums the cluster's codepoints with the
    // correct east-asian/zero-width rules; a whole-cluster call keeps ZWJ
    // sequences and VS16 at their base width.
    UnicodeWidthStr::width(cluster)
}

/// A single styled cell produced while decomposing a [`Line`] for wrapping.
struct Cell {
    symbol: String,
    style: Style,
    width: usize,
}

/// Wrap one logical [`Line`] into one or more visual rows, each no wider than
/// `width` display columns.
///
/// Wrapping is column-based (not word-aware): this is history/scrollback text
/// that must land in the terminal exactly as many rows as [`wrap_lines`]
/// reported, so a purely deterministic column wrap is what keeps the committed
/// height and the painted rows in lock-step. Grapheme clusters are never split —
/// a wide grapheme (CJK, emoji, flag) that would overflow the current row starts
/// the next row whole. Each output row inherits the exact per-cell style of its
/// graphemes, so styling continues seamlessly across a wrap and never bleeds past
/// the block.
///
/// An empty logical line yields exactly one empty visual row, so blank lines in
/// the input are preserved one-for-one in the output height.
fn wrap_line(line: &Line<'_>, width: u16) -> Vec<Line<'static>> {
    let width = width.max(MIN_WRAP_WIDTH) as usize;

    // Decompose into styled cells. `styled_graphemes` resolves each grapheme's
    // effective style (line style patched by span style), so we never have to
    // re-derive styling while packing rows.
    let cells: Vec<Cell> = line
        .styled_graphemes(Style::default())
        .map(|g| Cell {
            symbol: g.symbol.to_string(),
            style: g.style,
            width: grapheme_width(g.symbol),
        })
        .collect();

    if cells.is_empty() {
        // Preserve the blank line so the committed height matches the input line
        // count; the line's own style rides along in case it painted a bg.
        return vec![Line::default().style(line.style)];
    }

    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Cell> = Vec::new();
    let mut current_width = 0usize;

    for cell in cells {
        let cell_width = cell.width;
        // A grapheme wider than the whole row still gets a row to itself rather
        // than looping forever; otherwise start a new row once it would overflow.
        if current_width > 0 && current_width + cell_width > width {
            rows.push(build_row(&current, line.style));
            current.clear();
            current_width = 0;
        }
        current_width += cell_width;
        current.push(cell);
    }
    if !current.is_empty() {
        rows.push(build_row(&current, line.style));
    }

    rows
}

/// Rebuild one visual row from its packed cells, coalescing adjacent cells that
/// share a style into a single [`Span`] so the row is compact but pixel-identical
/// to the source styling.
fn build_row(cells: &[Cell], line_style: Style) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut run_style: Option<Style> = None;

    for cell in cells {
        match run_style {
            Some(style) if style == cell.style => buf.push_str(&cell.symbol),
            _ => {
                if let Some(style) = run_style.take() {
                    spans.push(Span::styled(std::mem::take(&mut buf), style));
                }
                run_style = Some(cell.style);
                buf.push_str(&cell.symbol);
            }
        }
    }
    if let Some(style) = run_style {
        spans.push(Span::styled(buf, style));
    }

    // Carry the source line's own style so a line-level background/attribute is
    // preserved on the wrapped rows.
    Line::from(spans).style(line_style)
}

/// Pre-wrap a block of logical lines to `width`, in emission order.
///
/// This is the pure core of [`HistorySink::commit_lines`]: the returned vector is
/// exactly the sequence of visual rows that will be written into scrollback, and
/// its length is the `height` to pass to
/// [`Terminal::insert_before`](ratatui::Terminal::insert_before). Order is
/// preserved: row `i` of logical line `n` precedes every row of logical line
/// `n+1`. Every returned row has display width `≤ width` and never splits a
/// grapheme cluster.
#[must_use]
pub fn wrap_lines(lines: &[Line<'_>], width: u16) -> Vec<Line<'static>> {
    lines
        .iter()
        .flat_map(|line| wrap_line(line, width))
        .collect()
}

/// Commits finalized content into the terminal's native scrollback, above the
/// live inline viewport.
///
/// Stateless: it owns no buffer and mutates nothing. Each
/// [`commit_lines`](HistorySink::commit_lines) call is a single, self-contained
/// `insert_before` — once rows are handed to the terminal they belong to
/// scrollback and are never revisited, which is exactly the immutability the
/// history model requires. Kept as a unit struct (rather than free functions) so
/// call sites read as `history.commit_lines(...)` and so future per-sink state
/// (e.g. a running commit counter) has a home without churning the signature.
#[derive(Debug, Default, Clone, Copy)]
pub struct HistorySink;

impl HistorySink {
    /// Create a history sink.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Commit `lines` into scrollback above `terminal`'s inline viewport.
    ///
    /// The lines are width-aware pre-wrapped to the terminal's current width
    /// (see [`wrap_lines`]); the total number of resulting visual rows becomes
    /// the fixed `height` handed to
    /// [`Terminal::insert_before`](ratatui::Terminal::insert_before). Rendering
    /// happens inside the `insert_before` closure into the `height × width`
    /// buffer, one visual row per buffer row, so committed height and painted
    /// rows are always in lock-step.
    ///
    /// A commit of zero rows (empty input) is a no-op: it performs no
    /// `insert_before`, so it neither scrolls the terminal nor emits a byte.
    ///
    /// # Ordering vs. the frame scheduler
    ///
    /// `insert_before` must be called *between* viewport draws, never
    /// interleaved with a `terminal.draw`. Callers integrating with the frame
    /// scheduler should commit history on the same thread that owns the
    /// terminal, at a point where no draw is in flight; the scheduler then
    /// repaints the (unchanged) viewport on its next frame.
    ///
    /// # Errors
    ///
    /// Propagates any backend error from `insert_before` (e.g. a failed write to
    /// the underlying terminal).
    pub fn commit_lines<B>(
        &mut self,
        terminal: &mut ratatui::Terminal<B>,
        lines: Vec<Line<'_>>,
    ) -> Result<(), B::Error>
    where
        B: ratatui::backend::Backend,
    {
        // Pick up any pending backend resize *before* reading the wrap width, so
        // a commit that lands right after a resize wraps to the *new* width, not
        // the width the last draw saw. `get_frame().area().width` reflects the
        // viewport area, which only moves when the terminal resizes — and an
        // inline viewport resizes lazily, on `draw`/`autoresize`, never on its
        // own. Without this, the first block committed after a narrow/widen would
        // pre-wrap to the stale width and land clipped or short in scrollback
        // (the VAL-CORE-009/010 failure). `autoresize` is a no-op when the size
        // is unchanged, so the steady-state commit path pays nothing.
        terminal.autoresize()?;
        let width = terminal.get_frame().area().width;
        let rows = wrap_lines(&lines, width);
        self.commit_rows(terminal, rows)
    }

    /// Commit already-wrapped visual `rows` into scrollback.
    ///
    /// The lower-level entry point used when the caller has already pre-wrapped
    /// (or when rows come from a source that must not be re-wrapped). `rows` are
    /// written verbatim, one per buffer row, in order. Empty input is a no-op.
    ///
    /// # Errors
    ///
    /// Propagates any backend error from `insert_before`.
    pub fn commit_rows<B>(
        &mut self,
        terminal: &mut ratatui::Terminal<B>,
        rows: Vec<Line<'static>>,
    ) -> Result<(), B::Error>
    where
        B: ratatui::backend::Backend,
    {
        let height = match u16::try_from(rows.len()) {
            Ok(0) => return Ok(()),
            Ok(height) => height,
            // More rows than a u16 can address in one insert is not a real
            // terminal scenario; clamp to the max so we never wrap the height to
            // a small value and silently drop the bulk of the block.
            Err(_) => u16::MAX,
        };

        terminal.insert_before(height, |buf| {
            let area = buf.area;
            for (offset, row) in rows.iter().enumerate() {
                let Ok(offset) = u16::try_from(offset) else {
                    break;
                };
                let y = area.y + offset;
                if y >= area.y + area.height {
                    break;
                }
                buf.set_line(area.x, y, row, area.width);
            }
        })
    }
}
