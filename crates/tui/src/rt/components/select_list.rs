//! Select-list component — a scrollable, filterable selection list.
//!
//! The rt-native counterpart to the legacy `SelectListComponent`. Where the
//! legacy widget renders to `Vec<String>` of ANSI-coded lines and consumes raw
//! byte events, this implements [`RtComponent`]: it paints styled [`Line`]s into
//! a ratatui [`Buffer`] and consumes structured [`RtKey`]s. The *behaviour* is
//! pinned to the legacy widget; only the render target and event model move.
//!
//! # Pinned behaviour
//!
//! - **Wrap navigation.** Up/Down (and their `ctrl+k`/`ctrl+j` aliases), plus
//!   PageUp/PageDown, all wrap past the ends — moving up from the first item
//!   lands on the last, and vice versa. This is the deliberate counterpart to
//!   [`SettingsList`](super::SettingsList), whose navigation *clamps* at the
//!   ends instead (Decision Log: the two lists disagree on end behaviour on
//!   purpose). Home/End jump to the first/last item without wrapping.
//! - **Window + counter.** At most `visible_count` rows are shown; when the
//!   filtered list is longer, a `(n/total)` counter line follows, and the window
//!   scrolls to keep the selection visible.
//! - **Prefix filter.** [`set_filter`](SelectList::set_filter) keeps the items
//!   whose `value` starts with the filter, **case-insensitively** — a prefix
//!   match, not a substring or fuzzy one (Decision Log). An empty filter restores
//!   the full list; a filter matching nothing renders a "No matching items" hint.
//! - **Two columns.** The primary (label) column is clamped to `[min, max]`
//!   display columns (default 32) and never wider than half the area; an optional
//!   description column is dimmed, flattened to a single line (newlines → spaces),
//!   and truncated with an ellipsis when it overflows.
//! - **Empty.** An empty item list renders a "(no items)" hint.
//!
//! The selection indicator is `▸ ` and the selected row is painted reversed so it
//! reads as a highlight bar across the whole width.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Widget;

use super::{display_width, truncate_with_ellipsis};
use crate::rt::events::RtKey;
use crate::rt::view::{HandleOutcome, RtComponent};

/// Default primary-column width in display columns.
pub const DEFAULT_PRIMARY_COLUMN_WIDTH: usize = 32;

/// Columns of gap reserved after the primary column before the description.
const PRIMARY_COLUMN_GAP: usize = 2;

/// The outcome of a terminal key (Enter/Escape) on the list, for a host that
/// wants to react without wiring a callback.
///
/// [`SelectList::handle_key`] returns this via [`SelectList::take_outcome`]:
/// Enter yields [`SelectOutcome::Selected`] with the focused item's index (into
/// the *unfiltered* item list), Escape yields [`SelectOutcome::Cancelled`]. It is
/// a one-shot latch — reading it clears it — so a host polls it once per frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectOutcome {
    /// Enter pressed on the item at this index in the unfiltered `items`.
    Selected(usize),
    /// Escape pressed.
    Cancelled,
}

/// An item in a [`SelectList`].
#[derive(Debug, Clone)]
pub struct SelectItem {
    /// The value the filter matches against (case-insensitive prefix).
    pub value: String,
    /// The label shown in the primary column; falls back to `value` when empty.
    pub label: String,
    /// Optional dimmed description shown in the second column.
    pub description: Option<String>,
}

impl SelectItem {
    /// A new item with the given value and label and no description.
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            description: None,
        }
    }

    /// Attach a description shown in the dimmed second column.
    #[must_use]
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

/// Layout knobs for the primary column.
#[derive(Debug, Clone, Default)]
pub struct SelectListLayout {
    /// Minimum primary-column width in display columns.
    pub min_primary_column_width: Option<usize>,
    /// Maximum primary-column width in display columns.
    pub max_primary_column_width: Option<usize>,
}

/// A scrollable, filterable selection list painted into a ratatui buffer.
pub struct SelectList {
    /// All items, in their original order. Selection outcomes index into this.
    items: Vec<SelectItem>,
    /// Indices into `items` surviving the current filter, in original order.
    filtered: Vec<usize>,
    /// Selection index *within the filtered view*.
    selected: usize,
    /// Top of the visible window, within the filtered view.
    scroll_offset: usize,
    /// Rows of items shown before scrolling kicks in.
    visible_count: usize,
    /// Current filter string (case-insensitive prefix on `value`).
    filter: String,
    /// Primary-column layout knobs.
    layout: SelectListLayout,
    /// Latched terminal outcome (Enter/Escape), cleared on read.
    outcome: Option<SelectOutcome>,
}

impl SelectList {
    /// A new list over `items`, with the first item selected.
    pub fn new(items: Vec<SelectItem>) -> Self {
        let filtered = (0..items.len()).collect();
        Self {
            items,
            filtered,
            selected: 0,
            scroll_offset: 0,
            visible_count: 10,
            filter: String::new(),
            layout: SelectListLayout::default(),
            outcome: None,
        }
    }

    /// Set the number of item rows shown before scrolling kicks in.
    #[must_use]
    pub fn visible_count(mut self, count: usize) -> Self {
        self.visible_count = count.max(1);
        self
    }

    /// Set the primary-column layout knobs.
    #[must_use]
    pub fn layout(mut self, layout: SelectListLayout) -> Self {
        self.layout = layout;
        self
    }

    /// The selection index within the filtered view.
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// The currently focused item, if any survives the filter.
    pub fn selected_item(&self) -> Option<&SelectItem> {
        self.filtered
            .get(self.selected)
            .and_then(|&i| self.items.get(i))
    }

    /// The number of items surviving the current filter.
    pub fn filtered_len(&self) -> usize {
        self.filtered.len()
    }

    /// Replace the items, resetting selection, scroll, and re-applying the filter.
    pub fn set_items(&mut self, items: Vec<SelectItem>) {
        self.items = items;
        self.selected = 0;
        self.scroll_offset = 0;
        self.recompute_filter();
    }

    /// Apply a case-insensitive **prefix** filter on each item's `value`.
    ///
    /// An empty string clears the filter (all items survive). Selection and
    /// scroll reset to the first surviving item. This is a prefix match, not a
    /// substring or fuzzy one (Decision Log).
    pub fn set_filter(&mut self, filter: impl Into<String>) {
        self.filter = filter.into();
        self.selected = 0;
        self.scroll_offset = 0;
        self.recompute_filter();
    }

    /// Take the latched terminal outcome (Enter/Escape), clearing it.
    ///
    /// A one-shot latch: a host polls it once per frame to learn whether the user
    /// committed a selection or cancelled since the last poll.
    pub fn take_outcome(&mut self) -> Option<SelectOutcome> {
        self.outcome.take()
    }

    fn recompute_filter(&mut self) {
        let needle = self.filter.to_lowercase();
        self.filtered = if needle.is_empty() {
            (0..self.items.len()).collect()
        } else {
            self.items
                .iter()
                .enumerate()
                .filter(|(_, it)| it.value.to_lowercase().starts_with(&needle))
                .map(|(i, _)| i)
                .collect()
        };
    }

    /// Move the selection one step, wrapping past the ends. `+1` is down, `-1` is
    /// up. A no-op when the filtered view is empty.
    fn step(&mut self, delta: isize) {
        let len = self.filtered.len();
        if len == 0 {
            return;
        }
        let len_i = len as isize;
        // Wrap into `[0, len)` with a single rem after adding `len` so a negative
        // step never underflows.
        let next = (self.selected as isize + delta).rem_euclid(len_i);
        self.selected = next as usize;
        self.clamp_scroll();
    }

    /// Move the selection by a whole window, wrapping like a single step (the
    /// PageUp/PageDown contract mirrors Up/Down — wrap, not clamp).
    fn page(&mut self, direction: isize) {
        let step = self.visible_count.max(1) as isize;
        self.step(direction * step);
    }

    /// Keep the visible window over the selection.
    fn clamp_scroll(&mut self) {
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + self.visible_count {
            self.scroll_offset = self.selected + 1 - self.visible_count;
        }
    }

    /// The primary-column width for the current area: the widest label (plus a
    /// gap) clamped to `[min, max]`, then to at most half the area width.
    fn primary_column_width(&self, area_width: usize) -> usize {
        let raw_min = self
            .layout
            .min_primary_column_width
            .or(self.layout.max_primary_column_width)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);
        let raw_max = self
            .layout
            .max_primary_column_width
            .or(self.layout.min_primary_column_width)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);
        let min = raw_min.min(raw_max).max(1);
        let max = raw_min.max(raw_max).max(1);

        let widest = self
            .filtered
            .iter()
            .filter_map(|&i| self.items.get(i))
            .map(|item| display_width(self.label_of(item)) + PRIMARY_COLUMN_GAP)
            .max()
            .unwrap_or(0);
        widest.clamp(min, max).min((area_width / 2).max(1))
    }

    /// The label shown for an item — its `label`, or `value` when the label is
    /// empty.
    fn label_of<'a>(&self, item: &'a SelectItem) -> &'a str {
        if item.label.is_empty() {
            &item.value
        } else {
            &item.label
        }
    }
}

impl RtComponent for SelectList {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let width = area.width as usize;

        // Empty / no-match hints.
        if self.filtered.is_empty() {
            let msg = if self.items.is_empty() {
                "  (no items)"
            } else {
                "  No matching items"
            };
            let line = Line::from(Span::styled(
                msg,
                Style::default().add_modifier(Modifier::DIM),
            ));
            Text::from(vec![line]).render(area, buf);
            return;
        }

        let primary_width = self.primary_column_width(width);
        let end = (self.scroll_offset + self.visible_count).min(self.filtered.len());

        let mut lines: Vec<Line<'static>> = Vec::new();
        for i in self.scroll_offset..end {
            let item = &self.items[self.filtered[i]];
            let is_selected = i == self.selected;

            let indicator = if is_selected { "▸ " } else { "  " };
            // Ellipsize an over-wide label to the primary-column width — matching
            // the description column — instead of letting ratatui hard-clip it at
            // the right edge with no `…`. A label that already fits is returned
            // unchanged, then padded out to the column width as before.
            let clipped = truncate_with_ellipsis(self.label_of(item), primary_width);
            let label = pad_to_width(&clipped, primary_width);

            let mut spans: Vec<Span<'static>> = vec![Span::raw(format!("{indicator}{label}"))];

            if let Some(desc) = &item.description {
                let flattened = flatten(desc);
                // The description column starts after the indicator (2), primary
                // column, and a 2-column gap; reserve one more so a full-width
                // description never abuts the right edge.
                let used = 2 + primary_width + PRIMARY_COLUMN_GAP;
                let desc_budget = width.saturating_sub(used + 1);
                let shown = truncate_with_ellipsis(&flattened, desc_budget);
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    shown,
                    Style::default().add_modifier(Modifier::DIM),
                ));
            }

            let mut line = Line::from(spans);
            if is_selected {
                // Paint the whole selected row reversed so it reads as a highlight
                // bar across the full width, not just the glyphs.
                line = line.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            lines.push(line);
        }

        // Window counter, shown only when the list overflows the window.
        if self.filtered.len() > self.visible_count {
            let info = format!("  ({}/{})", self.selected + 1, self.filtered.len());
            lines.push(Line::from(Span::styled(
                info,
                Style::default().add_modifier(Modifier::DIM),
            )));
        }

        Text::from(lines).render(area, buf);
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        let Some(id) = key.key_id.as_deref() else {
            return HandleOutcome::Ignored;
        };
        match id {
            "up" | "ctrl+k" => {
                self.step(-1);
                HandleOutcome::Consumed
            }
            "down" | "ctrl+j" => {
                self.step(1);
                HandleOutcome::Consumed
            }
            "pageUp" => {
                self.page(-1);
                HandleOutcome::Consumed
            }
            "pageDown" => {
                self.page(1);
                HandleOutcome::Consumed
            }
            "home" => {
                self.selected = 0;
                self.scroll_offset = 0;
                HandleOutcome::Consumed
            }
            "end" => {
                let len = self.filtered.len();
                self.selected = len.saturating_sub(1);
                self.scroll_offset = len.saturating_sub(self.visible_count);
                HandleOutcome::Consumed
            }
            "enter" => {
                if let Some(&item_idx) = self.filtered.get(self.selected) {
                    self.outcome = Some(SelectOutcome::Selected(item_idx));
                }
                HandleOutcome::Consumed
            }
            "escape" => {
                self.outcome = Some(SelectOutcome::Cancelled);
                HandleOutcome::Consumed
            }
            _ => HandleOutcome::Ignored,
        }
    }
}

/// Flatten a description to a single line: newlines/carriage-returns become
/// spaces, and leading/trailing whitespace is trimmed.
fn flatten(text: &str) -> String {
    text.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Pad `text` on the right with spaces to `width` display columns. A string
/// already at or beyond `width` is returned unchanged (never truncated here —
/// the primary column is clamped so labels are expected to fit).
fn pad_to_width(text: &str, width: usize) -> String {
    let w = display_width(text);
    if w >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - w))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(id: &str, code: KeyCode) -> RtKey {
        RtKey {
            key_id: Some(id.to_string()),
            raw: KeyEvent::new(code, KeyModifiers::NONE),
        }
    }

    fn items() -> Vec<SelectItem> {
        vec![
            SelectItem::new("alpha", "Alpha").with_description("first"),
            SelectItem::new("beta", "Beta"),
            SelectItem::new("gamma", "Gamma"),
        ]
    }

    #[test]
    fn down_wraps_at_end() {
        let mut list = SelectList::new(items());
        list.handle_key(&key("down", KeyCode::Down));
        list.handle_key(&key("down", KeyCode::Down));
        assert_eq!(list.selected_index(), 2);
        list.handle_key(&key("down", KeyCode::Down));
        assert_eq!(list.selected_index(), 0, "down at end wraps to top");
    }

    #[test]
    fn up_wraps_at_start() {
        let mut list = SelectList::new(items());
        list.handle_key(&key("up", KeyCode::Up));
        assert_eq!(list.selected_index(), 2, "up at start wraps to bottom");
    }

    #[test]
    fn ctrl_j_k_alias_move() {
        let mut list = SelectList::new(items());
        list.handle_key(&key("ctrl+j", KeyCode::Char('j')));
        assert_eq!(list.selected_index(), 1);
        list.handle_key(&key("ctrl+k", KeyCode::Char('k')));
        assert_eq!(list.selected_index(), 0);
    }

    #[test]
    fn prefix_filter_is_case_insensitive() {
        let mut list = SelectList::new(items());
        list.set_filter("AL");
        assert_eq!(list.filtered_len(), 1);
        assert_eq!(list.selected_item().unwrap().value, "alpha");
    }

    #[test]
    fn prefix_filter_is_not_substring() {
        let mut list = SelectList::new(items());
        // "amma" is a substring of "gamma" but not a prefix → no match.
        list.set_filter("amma");
        assert_eq!(list.filtered_len(), 0);
    }

    #[test]
    fn overwide_primary_label_gets_ellipsis() {
        // A label wider than the clamped primary column must be truncated with
        // `…` (matching the description column), not hard-clipped by ratatui.
        let list = SelectList::new(vec![SelectItem::new(
            "v",
            "a-very-long-label-that-overflows",
        )])
        .layout(SelectListLayout {
            min_primary_column_width: Some(8),
            max_primary_column_width: Some(8),
        });

        let area = Rect::new(0, 0, 40, 3);
        let mut buf = Buffer::empty(area);
        list.render(area, &mut buf);

        let first_row: String = (0..area.width)
            .map(|x| buf[(x, 0)].symbol())
            .collect::<String>();
        assert!(
            first_row.contains('…'),
            "over-wide primary label should ellipsize, got: {first_row:?}"
        );
    }
}
