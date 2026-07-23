//! Text primitives: word-wrapped multi-line text and single-line truncated text.
//!
//! [`TextBlock`] is the rt counterpart to the legacy `TextComponent`: multi-line
//! text with word wrapping and optional padding. [`TruncatedText`] is the rt
//! counterpart to the legacy `TruncatedTextComponent`: a single line that clips
//! with an ellipsis when it is too wide, with optional padding.
//!
//! Both paint into a ratatui [`Buffer`]. [`TextBlock`] delegates wrapping to
//! ratatui's [`Paragraph`] with [`Wrap`] (word-boundary, whitespace-trimming),
//! offset by its padding. [`TruncatedText`] does its own display-width-aware
//! truncation (via [`truncate_with_ellipsis`]) because ratatui has no
//! ellipsis-on-clip mode.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Paragraph, Widget, Wrap};

use super::truncate_with_ellipsis;
use crate::rt::events::RtKey;
use crate::rt::view::{HandleOutcome, RtComponent};

/// Multi-line text with word wrapping and optional padding.
///
/// Word-wraps its content to the render area's inner width (area width minus
/// twice the horizontal padding), leaving `padding_x` blank columns on each side
/// and `padding_y` blank rows above and below. Wrapping is word-aware and
/// trims trailing whitespace at each wrap, delegated to ratatui's [`Paragraph`].
pub struct TextBlock {
    text: String,
    padding_x: u16,
    padding_y: u16,
    style: Style,
}

impl TextBlock {
    /// A text block with no padding and the default style.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            padding_x: 0,
            padding_y: 0,
            style: Style::default(),
        }
    }

    /// Set horizontal and vertical padding, in cells/rows.
    #[must_use]
    pub fn padding(mut self, x: u16, y: u16) -> Self {
        self.padding_x = x;
        self.padding_y = y;
        self
    }

    /// Set the style applied to the text.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Replace the text content.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }

    /// The current text.
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl RtComponent for TextBlock {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        // Carve the padded inner rect out of `area`; a padding larger than the
        // area collapses to a zero-sized inner rect (nothing painted) rather than
        // wrapping negatively.
        let inner = pad_rect(area, self.padding_x, self.padding_y);
        if inner.is_empty() {
            return;
        }
        Paragraph::new(self.text.as_str())
            .style(self.style)
            .wrap(Wrap { trim: true })
            .render(inner, buf);
    }

    fn handle_key(&mut self, _key: &RtKey) -> HandleOutcome {
        HandleOutcome::Ignored
    }
}

/// A single line of text that truncates with an ellipsis when too wide, with
/// optional padding.
///
/// Only the first source line is shown (content is clipped at the first newline).
/// The visible text is truncated to the inner width — area width minus twice the
/// horizontal padding — appending `…` when truncation occurs, measured in display
/// columns so a CJK/emoji cell is kept or dropped whole. The single-line
/// invariant holds at any width: it never spills onto a second row.
pub struct TruncatedText {
    text: String,
    padding_x: u16,
    padding_y: u16,
    style: Style,
}

impl TruncatedText {
    /// A truncated-text line with no padding and the default style.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            padding_x: 0,
            padding_y: 0,
            style: Style::default(),
        }
    }

    /// Set horizontal and vertical padding.
    #[must_use]
    pub fn padding(mut self, x: u16, y: u16) -> Self {
        self.padding_x = x;
        self.padding_y = y;
        self
    }

    /// Set the style applied to the line.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Replace the text content.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }

    /// The current text.
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl RtComponent for TruncatedText {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let inner = pad_rect(area, self.padding_x, self.padding_y);
        if inner.is_empty() {
            return;
        }
        // First source line only; a truncated line is single-line by contract.
        let first = self.text.split('\n').next().unwrap_or("");
        let shown = truncate_with_ellipsis(first, inner.width as usize);
        // Paint on the top row of the padded inner rect; `set_stringn` clips to
        // the given width so even a mis-measured wide glyph cannot overflow the
        // area into a neighbouring region.
        buf.set_stringn(inner.x, inner.y, &shown, inner.width as usize, self.style);
    }

    fn handle_key(&mut self, _key: &RtKey) -> HandleOutcome {
        HandleOutcome::Ignored
    }
}

/// Shrink `area` by `pad_x` columns on each side and `pad_y` rows top and bottom.
///
/// Returns a zero-sized rect (which callers treat as "paint nothing") when the
/// padding exceeds the area, so an oversized padding on a narrow terminal never
/// underflows into a huge rect.
fn pad_rect(area: Rect, pad_x: u16, pad_y: u16) -> Rect {
    let dx = pad_x.saturating_mul(2);
    let dy = pad_y.saturating_mul(2);
    if area.width <= dx || area.height <= dy {
        return Rect::new(area.x, area.y, 0, 0);
    }
    Rect::new(
        area.x + pad_x,
        area.y + pad_y,
        area.width - dx,
        area.height - dy,
    )
}
