//! Primitive display widgets for the rt stack.
//!
//! These are the rt-native counterparts to the legacy `crate::components`
//! primitives (`text`, `box_component`, `spacer`, `truncated_text`,
//! `status_bar`, `progress_bar`). Where the legacy widgets render to
//! `Vec<String>` of ANSI-coded lines, these implement [`RtComponent`] and paint
//! directly into a ratatui [`Buffer`](ratatui::buffer::Buffer) — the model the
//! rt scheduler draws every frame.
//!
//! # Design
//!
//! - **Native widgets where they fit.** [`TextBlock`] wraps ratatui's
//!   [`Paragraph`](ratatui::widgets::Paragraph) word-wrap; [`ProgressBar`] wraps
//!   [`Gauge`](ratatui::widgets::Gauge). The custom parts (single-line status-bar
//!   layout, ellipsis truncation) are the pieces ratatui has no built-in for.
//! - **Display-only.** These primitives show content; none of them own a caret
//!   or consume keys, so [`RtComponent::handle_key`] returns
//!   [`HandleOutcome::Ignored`] and [`RtComponent::cursor`] defaults to `None`.
//!   Interactive widgets (input, editor) are separate.
//! - **Width correctness.** Truncation and single-line layout measure *display*
//!   width via [`unicode-width`](unicode_width), so CJK/emoji cells count as two
//!   columns and a narrow terminal truncates rather than overflowing or slicing
//!   a multibyte grapheme mid-byte (the legacy byte-slice panic this avoids).
//!
//! # Behavioural signatures
//!
//! Tests pin *behaviour* — segment order, column alignment, clamping, exact row
//! counts, the single-line invariant — not specific glyphs or legacy ANSI text
//! formatting (Decision Log: visual-signature tolerance). A primitive is correct
//! if, at 100 columns and after a resize to 60, its contract still holds.

mod markdown;
mod progress_bar;
mod select_list;
mod settings_list;
mod spacer;
mod status_bar;
pub mod syntax_highlight;
mod text_block;
mod widget_box;

pub use markdown::{
    CodeHighlighter, MarkdownTheme, MarkdownView, plain_code_highlighter, render_markdown,
};
pub use progress_bar::ProgressBar;
pub use select_list::{
    DEFAULT_PRIMARY_COLUMN_WIDTH, SelectItem, SelectList, SelectListLayout, SelectOutcome,
};
pub use settings_list::{SettingEntry, SettingValue, SettingsList};
pub use spacer::Spacer;
pub use status_bar::StatusBar;
pub use syntax_highlight::{default_highlighter, default_markdown_theme, highlight};
pub use text_block::{TextBlock, TruncatedText};
pub use widget_box::WidgetBox;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Display width of a string in terminal columns (CJK/emoji aware).
///
/// Thin re-export of [`UnicodeWidthStr::width`] so callers in this module do not
/// each import the trait; the whole point is that a two-column glyph counts as
/// two, which is what makes single-line truncation and status-bar layout land on
/// the right column instead of overflowing.
pub(crate) fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncate `s` to at most `max_cols` display columns, appending an ellipsis
/// (`…`, one column) when truncation actually happens.
///
/// Never splits a grapheme across the column budget: it accumulates whole
/// characters by display width and stops before overflowing, so a CJK/emoji cell
/// is kept or dropped as a unit rather than sliced mid-byte. When `s` already
/// fits it is returned unchanged (no ellipsis). When `max_cols` is `0` the result
/// is empty. When the budget is `1` only the ellipsis is emitted (there is no
/// room for content plus a marker).
///
/// The ellipsis is reserved *before* filling, matching how a terminal shows a
/// clipped line: `"hello"` in 3 columns becomes `"he…"`, not `"hel"`.
pub(crate) fn truncate_with_ellipsis(s: &str, max_cols: usize) -> String {
    if max_cols == 0 {
        return String::new();
    }
    if display_width(s) <= max_cols {
        return s.to_string();
    }
    // Truncation is required, so reserve one column for the ellipsis marker.
    let budget = max_cols.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}
