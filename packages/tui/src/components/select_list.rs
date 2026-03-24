//! Select list component — scrollable selection list.

use crate::keys::{KeyName, parse_key};
use crate::tui::{Component, HandleResult};
use crate::utils;

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

/// Callback types.
pub type OnSelect = Box<dyn Fn(&SelectItem) + Send>;
pub type OnCancel = Box<dyn Fn() + Send>;

/// Scrollable selection list with keyboard navigation.
pub struct SelectListComponent {
    items: Vec<SelectItem>,
    selected: usize,
    scroll_offset: usize,
    visible_count: usize,
    on_select: Option<OnSelect>,
    on_cancel: Option<OnCancel>,
}

impl SelectListComponent {
    pub fn new(items: Vec<SelectItem>) -> Self {
        Self {
            items,
            selected: 0,
            scroll_offset: 0,
            visible_count: 10,
            on_select: None,
            on_cancel: None,
        }
    }

    pub fn with_visible_count(mut self, count: usize) -> Self {
        self.visible_count = count;
        self
    }

    pub fn set_on_select(&mut self, callback: OnSelect) {
        self.on_select = Some(callback);
    }

    pub fn set_on_cancel(&mut self, callback: OnCancel) {
        self.on_cancel = Some(callback);
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn selected_item(&self) -> Option<&SelectItem> {
        self.items.get(self.selected)
    }

    pub fn set_items(&mut self, items: Vec<SelectItem>) {
        self.items = items;
        self.selected = 0;
        self.scroll_offset = 0;
    }

    fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            if self.selected < self.scroll_offset {
                self.scroll_offset = self.selected;
            }
        }
    }

    fn move_down(&mut self) {
        if self.selected + 1 < self.items.len() {
            self.selected += 1;
            if self.selected >= self.scroll_offset + self.visible_count {
                self.scroll_offset = self.selected + 1 - self.visible_count;
            }
        }
    }
}

impl Component for SelectListComponent {
    fn render(&self, width: u16) -> Vec<String> {
        if self.items.is_empty() {
            return vec!["\x1b[90m(no items)\x1b[0m".to_string()];
        }

        let max_label_width = self
            .items
            .iter()
            .map(|i| utils::visible_width(&i.label))
            .max()
            .unwrap_or(0)
            .min(width as usize / 2);

        let end = (self.scroll_offset + self.visible_count).min(self.items.len());
        let mut lines = Vec::new();

        for i in self.scroll_offset..end {
            let item = &self.items[i];
            let is_selected = i == self.selected;

            let indicator = if is_selected { "▸ " } else { "  " };
            let label = utils::pad_to_width(&item.label, max_label_width);

            let line = if let Some(desc) = &item.description {
                let desc_width = (width as usize)
                    .saturating_sub(max_label_width + 5); // indicator + gap
                let truncated_desc = if utils::visible_width(desc) > desc_width {
                    utils::truncate_to_width(desc, desc_width)
                } else {
                    desc.clone()
                };
                format!(
                    "{}{}  \x1b[90m{}\x1b[0m",
                    indicator, label, truncated_desc
                )
            } else {
                format!("{}{}", indicator, label)
            };

            if is_selected {
                lines.push(format!("\x1b[7m{}\x1b[0m", line));
            } else {
                lines.push(line);
            }
        }

        // Scroll indicator
        if self.items.len() > self.visible_count {
            let info = format!(
                " {}/{} ",
                self.selected + 1,
                self.items.len()
            );
            lines.push(format!("\x1b[90m{}\x1b[0m", info));
        }

        lines
    }

    fn handle_input(&mut self, data: &str) -> HandleResult {
        let key = parse_key(data);
        if key.is_release {
            return HandleResult::Ignored;
        }

        match &key.name {
            KeyName::Up | KeyName::Char('k') if key.modifiers.ctrl || matches!(key.name, KeyName::Up) => {
                self.move_up();
                HandleResult::Handled
            }
            KeyName::Down | KeyName::Char('j') if key.modifiers.ctrl || matches!(key.name, KeyName::Down) => {
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
                HandleResult::Handled
            }
            KeyName::End => {
                self.selected = self.items.len().saturating_sub(1);
                if self.items.len() > self.visible_count {
                    self.scroll_offset = self.items.len() - self.visible_count;
                }
                HandleResult::Handled
            }
            KeyName::Enter => {
                if let Some(cb) = &self.on_select
                    && let Some(item) = self.items.get(self.selected) {
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

        list.handle_input("\x1b[B"); // Down
        assert_eq!(list.selected_index(), 1);

        list.handle_input("\x1b[B"); // Down
        assert_eq!(list.selected_index(), 2);

        list.handle_input("\x1b[B"); // Down (at end)
        assert_eq!(list.selected_index(), 2);

        list.handle_input("\x1b[A"); // Up
        assert_eq!(list.selected_index(), 1);
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
        assert!(lines[0].contains("no items"));
    }

    #[test]
    fn test_select_list_home_end() {
        let mut list = SelectListComponent::new(test_items());
        list.handle_input("\x1b[F"); // End
        assert_eq!(list.selected_index(), 2);

        list.handle_input("\x1b[H"); // Home
        assert_eq!(list.selected_index(), 0);
    }

    #[test]
    fn test_select_list_set_items() {
        let mut list = SelectListComponent::new(test_items());
        list.handle_input("\x1b[B"); // Move down
        assert_eq!(list.selected_index(), 1);

        list.set_items(vec![SelectItem::new("x", "X")]);
        assert_eq!(list.selected_index(), 0);
    }
}
