//! Selector for the active coding-agent color theme.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/theme-selector.ts`.
//!
//! pi-mono's selector pulls the available theme list from a global registry.
//! Until the theme system is ported the available list is taken as a
//! constructor argument (view-model pattern, see Phase-2 footer port). The
//! component never mutates the list — it just renders, navigates, and emits a
//! [`ThemeOutcome`].
//!
//! In addition to confirm/cancel, the TS source also calls an
//! `onPreview(theme)` callback whenever the highlighted item changes so the
//! host can repaint the screen with the previewed theme. The Rust port keeps
//! the same shape: outcomes include a `Preview` variant emitted on every
//! selection-change tick.
//!
//! TODO(parity): theme integration deferred — see
//! docs/exec-plans/parity-completion.md §A1.

use hand_tui::{
    Component, Container, HandleResult, InputEvent, SelectItem, SelectListComponent,
    SelectListLayoutOptions,
};
use tokio::sync::mpsc;

use super::dynamic_border::DynamicBorderComponent;

/// Outcome dispatched on the channel handed to
/// [`ThemeSelectorComponent::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeOutcome {
    /// User confirmed the highlighted theme.
    Selected(String),
    /// User cancelled the selector (Esc).
    Cancelled,
    /// Highlighted item changed — the host should repaint with this theme as
    /// a preview. Mirrors the TS `onPreview` hook.
    Preview(String),
}

/// Container that renders the theme picker bordered top and bottom.
pub struct ThemeSelectorComponent {
    container: Container,
}

impl ThemeSelectorComponent {
    /// Build a selector. `current_theme` is highlighted on entry (and gets a
    /// `(current)` description suffix). `available_themes` defines the
    /// ordered set shown to the user; if it's empty the inner select list
    /// renders the standard "no items" hint.
    pub fn new(
        current_theme: impl Into<String>,
        available_themes: Vec<String>,
        tx: mpsc::UnboundedSender<ThemeOutcome>,
    ) -> Self {
        let current_theme = current_theme.into();
        let items: Vec<SelectItem> = available_themes
            .iter()
            .map(|name| {
                let item = SelectItem::new(name.clone(), name.clone());
                if name == &current_theme {
                    item.with_description("(current)")
                } else {
                    item
                }
            })
            .collect();

        let visible = available_themes.len().clamp(1, 10);
        let mut select = SelectListComponent::new(items)
            .with_visible_count(visible)
            .with_layout(SelectListLayoutOptions {
                min_primary_column_width: Some(12),
                max_primary_column_width: Some(32),
            });
        if let Some(idx) = available_themes.iter().position(|n| n == &current_theme) {
            select.set_selected_index(idx);
        }

        let select_tx = tx.clone();
        select.set_on_select(Box::new(move |item| {
            let _ = select_tx.send(ThemeOutcome::Selected(item.value.clone()));
        }));
        let cancel_tx = tx.clone();
        select.set_on_cancel(Box::new(move || {
            let _ = cancel_tx.send(ThemeOutcome::Cancelled);
        }));
        let preview_tx = tx;
        select.set_on_selection_change(Box::new(move |item| {
            let _ = preview_tx.send(ThemeOutcome::Preview(item.value.clone()));
        }));

        let mut container = Container::new();
        container.add_child(Box::new(DynamicBorderComponent::new()));
        container.add_child(Box::new(select));
        container.add_child(Box::new(DynamicBorderComponent::new()));

        Self { container }
    }
}

impl Component for ThemeSelectorComponent {
    fn render(&self, width: u16) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        self.container.handle_input(event)
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(rx: &mut mpsc::UnboundedReceiver<ThemeOutcome>) -> Vec<ThemeOutcome> {
        let mut out = Vec::new();
        while let Ok(o) = rx.try_recv() {
            out.push(o);
        }
        out
    }

    fn themes() -> Vec<String> {
        vec!["dark".to_string(), "light".to_string(), "solar".to_string()]
    }

    #[test]
    fn renders_all_themes_with_current_marker() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let selector = ThemeSelectorComponent::new("light", themes(), tx);
        let body = selector.render(60).join("\n");
        for name in ["dark", "light", "solar"] {
            assert!(body.contains(name), "missing {name}");
        }
        assert!(body.contains("(current)"));
    }

    #[test]
    fn enter_emits_selected_with_current_value() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ThemeSelectorComponent::new("light", themes(), tx);
        selector.handle_input(&InputEvent::Raw("\r".into()));
        assert!(matches!(
            drain(&mut rx).as_slice(),
            [ThemeOutcome::Selected(s)] if s == "light"
        ));
    }

    #[test]
    fn navigation_emits_preview_then_select_emits_selected() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ThemeSelectorComponent::new("dark", themes(), tx);
        selector.handle_input(&InputEvent::Raw("\x1b[B".into())); // Down
        selector.handle_input(&InputEvent::Raw("\r".into()));
        let events = drain(&mut rx);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], ThemeOutcome::Preview(s) if s == "light"));
        assert!(matches!(&events[1], ThemeOutcome::Selected(s) if s == "light"));
    }

    #[test]
    fn escape_emits_cancel() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ThemeSelectorComponent::new("dark", themes(), tx);
        selector.handle_input(&InputEvent::Raw("\x1b".into()));
        assert_eq!(drain(&mut rx), vec![ThemeOutcome::Cancelled]);
    }

    #[test]
    fn unknown_current_theme_does_not_panic() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ThemeSelectorComponent::new("nonexistent", themes(), tx);
        // First item ("dark") should still be selectable.
        selector.handle_input(&InputEvent::Raw("\r".into()));
        assert!(matches!(
            drain(&mut rx).as_slice(),
            [ThemeOutcome::Selected(s)] if s == "dark"
        ));
    }
}
