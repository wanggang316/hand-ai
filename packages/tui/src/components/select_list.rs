//! Select list component — scrollable selection list.
//
// audit: M3.T5 — parity reviewed against pi-tui/select-list.ts on 2026-05-07.
// non-goal: TS exposes a `truncatePrimary` callback hook for custom column
// truncation. The Rust port leaves that as future work — the default
// `utils::truncate_to_width` covers all current call sites.

use crate::keys::{Key, KeyName, parse_key};
use crate::tui::{Component, HandleResult, InputEvent};
use crate::utils;

/// Default primary-column width (mirrors TS `DEFAULT_PRIMARY_COLUMN_WIDTH`).
pub const DEFAULT_PRIMARY_COLUMN_WIDTH: usize = 32;

/// An item in the select list.
#[derive(Debug, Clone)]
pub struct SelectItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

impl SelectItem {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            description: None,
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

/// Theme for [`SelectListComponent`], mirroring TS's `SelectListTheme`.
///
/// Each field is an ANSI prefix (e.g. `"\x1b[36m"`); rendering wraps the
/// corresponding text with the prefix and a `\x1b[0m` reset.
#[derive(Debug, Clone, Default)]
pub struct SelectListTheme {
    pub selected_prefix: Option<String>,
    pub selected_text: Option<String>,
    pub description: Option<String>,
    pub scroll_info: Option<String>,
    pub no_match: Option<String>,
}

/// Layout knobs for [`SelectListComponent`], mirroring TS's
/// `SelectListLayoutOptions` (sans the `truncatePrimary` callback hook —
/// see the module-level non-goal note).
#[derive(Debug, Clone, Default)]
pub struct SelectListLayoutOptions {
    pub min_primary_column_width: Option<usize>,
    pub max_primary_column_width: Option<usize>,
}

/// Callback types.
pub type OnSelect = Box<dyn Fn(&SelectItem) + Send>;
pub type OnCancel = Box<dyn Fn() + Send>;
pub type OnSelectionChange = Box<dyn Fn(&SelectItem) + Send>;

/// Scrollable selection list with keyboard navigation.
pub struct SelectListComponent {
    items: Vec<SelectItem>,
    /// Indices into `items` that survive the current filter.
    filtered: Vec<usize>,
    selected: usize,
    scroll_offset: usize,
    visible_count: usize,
    /// Current filter string (case-insensitive prefix on `value`).
    filter: String,
    theme: SelectListTheme,
    layout: SelectListLayoutOptions,
    on_select: Option<OnSelect>,
    on_cancel: Option<OnCancel>,
    on_selection_change: Option<OnSelectionChange>,
}

impl SelectListComponent {
    pub fn new(items: Vec<SelectItem>) -> Self {
        let filtered = (0..items.len()).collect();
        Self {
            items,
            filtered,
            selected: 0,
            scroll_offset: 0,
            visible_count: 10,
            filter: String::new(),
            theme: SelectListTheme::default(),
            layout: SelectListLayoutOptions::default(),
            on_select: None,
            on_cancel: None,
            on_selection_change: None,
        }
    }

    pub fn with_visible_count(mut self, count: usize) -> Self {
        self.visible_count = count;
        self
    }

    pub fn with_theme(mut self, theme: SelectListTheme) -> Self {
        self.theme = theme;
        self
    }

    pub fn with_layout(mut self, layout: SelectListLayoutOptions) -> Self {
        self.layout = layout;
        self
    }

    pub fn set_theme(&mut self, theme: SelectListTheme) {
        self.theme = theme;
    }

    pub fn set_layout(&mut self, layout: SelectListLayoutOptions) {
        self.layout = layout;
    }

    pub fn set_on_select(&mut self, callback: OnSelect) {
        self.on_select = Some(callback);
    }

    pub fn set_on_cancel(&mut self, callback: OnCancel) {
        self.on_cancel = Some(callback);
    }

    pub fn set_on_selection_change(&mut self, callback: OnSelectionChange) {
        self.on_selection_change = Some(callback);
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Currently focused item (filtered view).
    pub fn selected_item(&self) -> Option<&SelectItem> {
        self.filtered
            .get(self.selected)
            .and_then(|&i| self.items.get(i))
    }

    /// Number of items surviving the current filter.
    pub fn filtered_len(&self) -> usize {
        self.filtered.len()
    }

    pub fn set_items(&mut self, items: Vec<SelectItem>) {
        self.items = items;
        self.selected = 0;
        self.scroll_offset = 0;
        self.recompute_filter();
    }

    /// Apply a case-insensitive prefix filter on each item's `value`. Empty
    /// string clears the filter. Mirrors TS `setFilter`.
    pub fn set_filter(&mut self, filter: impl Into<String>) {
        self.filter = filter.into();
        self.selected = 0;
        self.scroll_offset = 0;
        self.recompute_filter();
    }

    /// Clamp + assign selection within the filtered view. Mirrors TS
    /// `setSelectedIndex`.
    pub fn set_selected_index(&mut self, index: usize) {
        let len = self.filtered.len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = index.min(len - 1);
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + self.visible_count {
            self.scroll_offset = self.selected + 1 - self.visible_count;
        }
    }

    fn recompute_filter(&mut self) {
        let needle = self.filter.to_lowercase();
        if needle.is_empty() {
            self.filtered = (0..self.items.len()).collect();
        } else {
            self.filtered = self
                .items
                .iter()
                .enumerate()
                .filter(|(_, it)| it.value.to_lowercase().starts_with(&needle))
                .map(|(i, _)| i)
                .collect();
        }
    }

    fn move_up(&mut self) {
        let len = self.filtered.len();
        if len == 0 {
            return;
        }
        // Wrap-around per TS semantics.
        self.selected = if self.selected == 0 {
            len - 1
        } else {
            self.selected - 1
        };
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + self.visible_count {
            self.scroll_offset = self.selected + 1 - self.visible_count;
        }
        self.notify_selection_change();
    }

    fn move_down(&mut self) {
        let len = self.filtered.len();
        if len == 0 {
            return;
        }
        self.selected = if self.selected + 1 >= len {
            0
        } else {
            self.selected + 1
        };
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + self.visible_count {
            self.scroll_offset = self.selected + 1 - self.visible_count;
        }
        self.notify_selection_change();
    }

    fn notify_selection_change(&self) {
        if let (Some(cb), Some(item)) = (&self.on_selection_change, self.selected_item()) {
            cb(item);
        }
    }

    fn primary_column_width(&self) -> usize {
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

        const PRIMARY_COLUMN_GAP: usize = 2;
        let widest = self
            .filtered
            .iter()
            .filter_map(|&i| self.items.get(i))
            .map(|item| {
                let label = if item.label.is_empty() {
                    &item.value
                } else {
                    &item.label
                };
                utils::visible_width(label) + PRIMARY_COLUMN_GAP
            })
            .max()
            .unwrap_or(0);
        widest.clamp(min, max)
    }
}

impl Component for SelectListComponent {
    fn render(&self, width: u16) -> Vec<String> {
        // No matches → "no match" hint.
        if self.filtered.is_empty() {
            let msg = if self.items.is_empty() {
                "  (no items)"
            } else {
                "  No matching items"
            };
            return vec![apply_ansi(self.theme.no_match.as_deref(), msg, "\x1b[90m")];
        }

        let primary_width = self.primary_column_width().min(width as usize / 2).max(1);

        let end = (self.scroll_offset + self.visible_count).min(self.filtered.len());
        let mut lines = Vec::new();

        for i in self.scroll_offset..end {
            let item_idx = self.filtered[i];
            let item = &self.items[item_idx];
            let is_selected = i == self.selected;

            let indicator = if is_selected { "▸ " } else { "  " };
            let display_label = if item.label.is_empty() {
                &item.value
            } else {
                &item.label
            };
            let label = utils::pad_to_width(display_label, primary_width);

            let line = if let Some(desc) = &item.description {
                let desc_single: String = desc
                    .chars()
                    .map(|c| match c {
                        '\n' | '\r' => ' ',
                        _ => c,
                    })
                    .collect::<String>()
                    .trim()
                    .to_string();
                let desc_width = (width as usize).saturating_sub(primary_width + 5);
                let truncated_desc = if utils::visible_width(&desc_single) > desc_width {
                    utils::truncate_to_width(&desc_single, desc_width)
                } else {
                    desc_single
                };
                let desc_styled =
                    apply_ansi(self.theme.description.as_deref(), &truncated_desc, "\x1b[90m");
                format!("{indicator}{label}  {desc_styled}")
            } else {
                format!("{indicator}{label}")
            };

            if is_selected {
                lines.push(apply_ansi(
                    self.theme.selected_text.as_deref(),
                    &line,
                    "\x1b[7m",
                ));
            } else {
                lines.push(line);
            }
        }

        // Scroll indicator
        if self.filtered.len() > self.visible_count {
            let info = format!("  ({}/{})", self.selected + 1, self.filtered.len());
            lines.push(apply_ansi(self.theme.scroll_info.as_deref(), &info, "\x1b[90m"));
        }

        lines
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        match event {
            InputEvent::Key(key) => self.handle_key(key),
            InputEvent::Raw(data) | InputEvent::Paste(data) => self.handle_key(&parse_key(data)),
            _ => HandleResult::Ignored,
        }
    }
}

impl SelectListComponent {
    fn handle_key(&mut self, key: &Key) -> HandleResult {
        if key.is_release {
            return HandleResult::Ignored;
        }

        match &key.name {
            KeyName::Up | KeyName::Char('k')
                if key.modifiers.ctrl || matches!(key.name, KeyName::Up) =>
            {
                self.move_up();
                HandleResult::Handled
            }
            KeyName::Down | KeyName::Char('j')
                if key.modifiers.ctrl || matches!(key.name, KeyName::Down) =>
            {
                self.move_down();
                HandleResult::Handled
            }
            KeyName::PageUp => {
                for _ in 0..self.visible_count {
                    self.move_up();
                }
                HandleResult::Handled
            }
            KeyName::PageDown => {
                for _ in 0..self.visible_count {
                    self.move_down();
                }
                HandleResult::Handled
            }
            KeyName::Home => {
                self.selected = 0;
                self.scroll_offset = 0;
                self.notify_selection_change();
                HandleResult::Handled
            }
            KeyName::End => {
                let len = self.filtered.len();
                self.selected = len.saturating_sub(1);
                if len > self.visible_count {
                    self.scroll_offset = len - self.visible_count;
                }
                self.notify_selection_change();
                HandleResult::Handled
            }
            KeyName::Enter => {
                if let Some(cb) = &self.on_select
                    && let Some(item) = self.selected_item()
                {
                    cb(item);
                }
                HandleResult::Handled
            }
            KeyName::Escape => {
                if let Some(cb) = &self.on_cancel {
                    cb();
                }
                HandleResult::Handled
            }
            _ => HandleResult::Ignored,
        }
    }
}

/// Helper: wrap `text` with `theme_prefix` if provided, otherwise with
/// `default_prefix`. Both branches close with `\x1b[0m`.
fn apply_ansi(theme_prefix: Option<&str>, text: &str, default_prefix: &str) -> String {
    let prefix = theme_prefix.unwrap_or(default_prefix);
    if prefix.is_empty() {
        text.to_string()
    } else {
        format!("{prefix}{text}\x1b[0m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_items() -> Vec<SelectItem> {
        vec![
            SelectItem::new("a", "Alpha").with_description("First letter"),
            SelectItem::new("b", "Beta").with_description("Second letter"),
            SelectItem::new("c", "Gamma"),
        ]
    }

    #[test]
    fn test_select_list_render() {
        let list = SelectListComponent::new(test_items());
        let lines = list.render(80);
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_select_list_navigation() {
        let mut list = SelectListComponent::new(test_items());
        assert_eq!(list.selected_index(), 0);

        list.handle_input(&InputEvent::Raw("\x1b[B".into())); // Down
        assert_eq!(list.selected_index(), 1);

        list.handle_input(&InputEvent::Raw("\x1b[B".into())); // Down
        assert_eq!(list.selected_index(), 2);

        // Down at end wraps to top per TS semantics.
        list.handle_input(&InputEvent::Raw("\x1b[B".into()));
        assert_eq!(list.selected_index(), 0);

        list.handle_input(&InputEvent::Raw("\x1b[A".into())); // Up wraps to bottom
        assert_eq!(list.selected_index(), 2);
    }

    #[test]
    fn test_select_list_selected_item() {
        let list = SelectListComponent::new(test_items());
        let item = list.selected_item().unwrap();
        assert_eq!(item.value, "a");
    }

    #[test]
    fn test_select_list_empty() {
        let list = SelectListComponent::new(vec![]);
        let lines = list.render(80);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("(no items)"));
    }

    #[test]
    fn test_select_list_home_end() {
        let mut list = SelectListComponent::new(test_items());
        list.handle_input(&InputEvent::Raw("\x1b[F".into())); // End
        assert_eq!(list.selected_index(), 2);

        list.handle_input(&InputEvent::Raw("\x1b[H".into())); // Home
        assert_eq!(list.selected_index(), 0);
    }

    #[test]
    fn test_select_list_set_items() {
        let mut list = SelectListComponent::new(test_items());
        list.handle_input(&InputEvent::Raw("\x1b[B".into())); // Move down
        assert_eq!(list.selected_index(), 1);

        list.set_items(vec![SelectItem::new("x", "X")]);
        assert_eq!(list.selected_index(), 0);
    }

    #[test]
    fn test_select_list_set_filter_keeps_prefix_matches() {
        let mut list = SelectListComponent::new(test_items());
        list.set_filter("b");
        assert_eq!(list.filtered_len(), 1);
        assert_eq!(list.selected_item().unwrap().value, "b");
    }

    #[test]
    fn test_select_list_set_filter_no_match_renders_hint() {
        let mut list = SelectListComponent::new(test_items());
        list.set_filter("zzz");
        assert_eq!(list.filtered_len(), 0);
        let lines = list.render(80);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("No matching"));
    }

    #[test]
    fn test_select_list_set_filter_clears() {
        let mut list = SelectListComponent::new(test_items());
        list.set_filter("a");
        assert_eq!(list.filtered_len(), 1);
        list.set_filter("");
        assert_eq!(list.filtered_len(), 3);
    }

    #[test]
    fn test_select_list_set_selected_index_clamps() {
        let mut list = SelectListComponent::new(test_items());
        list.set_selected_index(99);
        assert_eq!(list.selected_index(), 2);
    }

    #[test]
    fn test_select_list_theme_applied() {
        let theme = SelectListTheme {
            no_match: Some("\x1b[31m".into()),
            ..SelectListTheme::default()
        };
        let mut list = SelectListComponent::new(test_items()).with_theme(theme);
        list.set_filter("zzz");
        let lines = list.render(80);
        assert!(lines[0].contains("\x1b[31m"));
    }

    #[test]
    fn test_select_list_layout_pads_label_column() {
        let layout = SelectListLayoutOptions {
            min_primary_column_width: Some(20),
            max_primary_column_width: Some(20),
        };
        let list = SelectListComponent::new(test_items()).with_layout(layout);
        let lines = list.render(80);
        // First rendered line includes a 20-wide label column → label "Alpha"
        // padded with spaces.
        assert!(lines[0].contains("Alpha             "));
    }

    #[test]
    fn test_select_list_selection_change_callback() {
        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_cb = Arc::clone(&captured);
        let mut list = SelectListComponent::new(test_items());
        list.set_on_selection_change(Box::new(move |item| {
            captured_cb.lock().unwrap().push(item.value.clone());
        }));
        list.handle_input(&InputEvent::Raw("\x1b[B".into())); // down
        list.handle_input(&InputEvent::Raw("\x1b[B".into())); // down
        let seen = captured.lock().unwrap();
        assert_eq!(seen.as_slice(), &["b".to_string(), "c".to_string()]);
    }
}
