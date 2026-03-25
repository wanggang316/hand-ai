//! Autocomplete component — suggestion dropdown for input fields.

use crate::tui::{Component, HandleResult};

/// A single autocomplete suggestion.
#[derive(Debug, Clone)]
pub struct Suggestion {
    /// The text to insert on selection.
    pub value: String,
    /// Display label (may differ from value).
    pub label: String,
    /// Optional description/annotation.
    pub description: Option<String>,
}

impl Suggestion {
    pub fn new(value: impl Into<String>) -> Self {
        let v: String = value.into();
        Self {
            label: v.clone(),
            value: v,
            description: None,
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

/// Autocomplete dropdown component.
pub struct AutocompleteComponent {
    suggestions: Vec<Suggestion>,
    selected_index: usize,
    visible: bool,
    max_visible: usize,
    scroll_offset: usize,
}

impl AutocompleteComponent {
    pub fn new() -> Self {
        Self {
            suggestions: Vec::new(),
            selected_index: 0,
            visible: false,
            max_visible: 8,
            scroll_offset: 0,
        }
    }

    /// Update the suggestions list.
    pub fn set_suggestions(&mut self, suggestions: Vec<Suggestion>) {
        self.suggestions = suggestions;
        self.selected_index = 0;
        self.scroll_offset = 0;
        self.visible = !self.suggestions.is_empty();
    }

    /// Get the currently selected suggestion.
    pub fn selected(&self) -> Option<&Suggestion> {
        self.suggestions.get(self.selected_index)
    }

    /// Move selection down.
    pub fn select_next(&mut self) {
        if !self.suggestions.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.suggestions.len();
            self.ensure_visible();
        }
    }

    /// Move selection up.
    pub fn select_prev(&mut self) {
        if !self.suggestions.is_empty() {
            if self.selected_index == 0 {
                self.selected_index = self.suggestions.len() - 1;
            } else {
                self.selected_index -= 1;
            }
            self.ensure_visible();
        }
    }

    /// Show the dropdown.
    pub fn show(&mut self) {
        self.visible = !self.suggestions.is_empty();
    }

    /// Hide the dropdown.
    pub fn hide(&mut self) {
        self.visible = false;
    }

    /// Whether the dropdown is visible.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Number of suggestions.
    pub fn count(&self) -> usize {
        self.suggestions.len()
    }

    /// Set max visible items.
    pub fn set_max_visible(&mut self, max: usize) {
        self.max_visible = max;
    }

    fn ensure_visible(&mut self) {
        if self.selected_index < self.scroll_offset {
            self.scroll_offset = self.selected_index;
        }
        if self.selected_index >= self.scroll_offset + self.max_visible {
            self.scroll_offset = self.selected_index + 1 - self.max_visible;
        }
    }
}

impl Default for AutocompleteComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for AutocompleteComponent {
    fn render(&self, width: u16) -> Vec<String> {
        if !self.visible || self.suggestions.is_empty() {
            return vec![];
        }

        let w = width as usize;
        let end = (self.scroll_offset + self.max_visible).min(self.suggestions.len());
        let visible = &self.suggestions[self.scroll_offset..end];

        visible
            .iter()
            .enumerate()
            .map(|(i, suggestion)| {
                let abs_index = self.scroll_offset + i;
                let is_selected = abs_index == self.selected_index;
                let indicator = if is_selected { ">" } else { " " };

                let desc = suggestion
                    .description
                    .as_deref()
                    .map(|d| format!("  \x1b[2m{}\x1b[0m", d))
                    .unwrap_or_default();

                let max_label = w.saturating_sub(4 + crate::utils::visible_width(&desc));
                let label = if suggestion.label.len() > max_label {
                    format!("{}...", &suggestion.label[..max_label.saturating_sub(3)])
                } else {
                    suggestion.label.clone()
                };

                if is_selected {
                    format!(" \x1b[7m{} {}\x1b[0m{}", indicator, label, desc)
                } else {
                    format!(" {} {}{}", indicator, label, desc)
                }
            })
            .collect()
    }

    fn handle_input(&mut self, _data: &str) -> HandleResult {
        HandleResult::Ignored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_autocomplete_empty() {
        let ac = AutocompleteComponent::new();
        assert!(!ac.is_visible());
        assert_eq!(ac.render(80).len(), 0);
    }

    #[test]
    fn test_autocomplete_with_suggestions() {
        let mut ac = AutocompleteComponent::new();
        ac.set_suggestions(vec![
            Suggestion::new("/help"),
            Suggestion::new("/model"),
            Suggestion::new("/quit"),
        ]);
        assert!(ac.is_visible());
        assert_eq!(ac.count(), 3);

        let lines = ac.render(80);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn test_autocomplete_navigation() {
        let mut ac = AutocompleteComponent::new();
        ac.set_suggestions(vec![
            Suggestion::new("a"),
            Suggestion::new("b"),
            Suggestion::new("c"),
        ]);

        assert_eq!(ac.selected().unwrap().value, "a");
        ac.select_next();
        assert_eq!(ac.selected().unwrap().value, "b");
        ac.select_next();
        assert_eq!(ac.selected().unwrap().value, "c");
        ac.select_next(); // wraps
        assert_eq!(ac.selected().unwrap().value, "a");
    }

    #[test]
    fn test_autocomplete_prev_wraps() {
        let mut ac = AutocompleteComponent::new();
        ac.set_suggestions(vec![Suggestion::new("a"), Suggestion::new("b")]);

        ac.select_prev(); // wraps to end
        assert_eq!(ac.selected().unwrap().value, "b");
    }

    #[test]
    fn test_autocomplete_hide_show() {
        let mut ac = AutocompleteComponent::new();
        ac.set_suggestions(vec![Suggestion::new("x")]);
        assert!(ac.is_visible());
        ac.hide();
        assert!(!ac.is_visible());
        ac.show();
        assert!(ac.is_visible());
    }

    #[test]
    fn test_autocomplete_max_visible() {
        let mut ac = AutocompleteComponent::new();
        ac.set_max_visible(2);
        ac.set_suggestions(vec![
            Suggestion::new("a"),
            Suggestion::new("b"),
            Suggestion::new("c"),
            Suggestion::new("d"),
        ]);

        let lines = ac.render(80);
        assert_eq!(lines.len(), 2); // Only 2 shown
    }

    #[test]
    fn test_suggestion_builder() {
        let s = Suggestion::new("cmd")
            .with_label("Command")
            .with_description("Do something");
        assert_eq!(s.value, "cmd");
        assert_eq!(s.label, "Command");
        assert_eq!(s.description.as_deref(), Some("Do something"));
    }
}
