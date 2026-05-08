//! Yes/No selector for the "show images inline" preference.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/show-images-selector.ts`.
//!
//! pi-mono wraps a [`SelectListComponent`] in dynamic borders and emits a
//! boolean callback on selection. The Rust port keeps the same shape but uses
//! Tokio channels (`mpsc::UnboundedSender`) instead of `Box<dyn Fn>` callbacks
//! so the host driver can `recv` outcomes without juggling synchronization
//! primitives.
//!
//! Theming caveat: the TS source pulls the SelectList palette from the
//! coding-agent theme. Until the theme port lands the renderer relies on
//! `SelectListComponent`'s built-in defaults (matches the dark theme's
//! visual style closely enough for the parity gates).
//!
//! TODO(parity): theme integration deferred — see
//! docs/exec-plans/parity-completion.md §A1.

use hand_tui::{
    Component, Container, HandleResult, InputEvent, SelectItem, SelectListComponent,
    SelectListLayoutOptions,
};
use tokio::sync::mpsc;

use super::dynamic_border::DynamicBorderComponent;

/// Outcome dispatched on the channel handed to [`ShowImagesSelectorComponent::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowImagesOutcome {
    /// User confirmed a value (`true` → show inline, `false` → text placeholder).
    Selected(bool),
    /// User cancelled the selector (Esc).
    Cancelled,
}

/// Container that renders a Yes/No picker for the `show_images` setting.
pub struct ShowImagesSelectorComponent {
    container: Container,
}

impl ShowImagesSelectorComponent {
    /// Build a new selector pre-positioned on `current_value` (true → "Yes").
    /// Outcomes are forwarded to `tx` exactly once per user action.
    pub fn new(current_value: bool, tx: mpsc::UnboundedSender<ShowImagesOutcome>) -> Self {
        let items = vec![
            SelectItem::new("yes", "Yes").with_description("Show images inline in terminal"),
            SelectItem::new("no", "No").with_description("Show text placeholder instead"),
        ];

        let mut select = SelectListComponent::new(items)
            .with_visible_count(5)
            .with_layout(SelectListLayoutOptions {
                min_primary_column_width: Some(12),
                max_primary_column_width: Some(32),
            });
        select.set_selected_index(if current_value { 0 } else { 1 });

        let select_tx = tx.clone();
        select.set_on_select(Box::new(move |item| {
            let _ = select_tx.send(ShowImagesOutcome::Selected(item.value == "yes"));
        }));
        let cancel_tx = tx;
        select.set_on_cancel(Box::new(move || {
            let _ = cancel_tx.send(ShowImagesOutcome::Cancelled);
        }));

        let mut container = Container::new();
        container.add_child(Box::new(DynamicBorderComponent::new()));
        container.add_child(Box::new(select));
        container.add_child(Box::new(DynamicBorderComponent::new()));

        Self { container }
    }
}

impl Component for ShowImagesSelectorComponent {
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

    fn drain(rx: &mut mpsc::UnboundedReceiver<ShowImagesOutcome>) -> Vec<ShowImagesOutcome> {
        let mut out = Vec::new();
        while let Ok(o) = rx.try_recv() {
            out.push(o);
        }
        out
    }

    #[test]
    fn renders_borders_and_two_options() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let selector = ShowImagesSelectorComponent::new(true, tx);
        let lines = selector.render(40);
        // Two borders + two list rows is the minimum.
        assert!(lines.len() >= 4);
        let body = lines.join("\n");
        assert!(body.contains("Yes"));
        assert!(body.contains("No"));
        assert!(body.contains("inline"));
    }

    #[test]
    fn enter_emits_selected_outcome() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ShowImagesSelectorComponent::new(true, tx);
        selector.handle_input(&InputEvent::Raw("\r".into()));
        assert_eq!(drain(&mut rx), vec![ShowImagesOutcome::Selected(true)]);
    }

    #[test]
    fn navigating_then_enter_emits_no() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ShowImagesSelectorComponent::new(true, tx);
        selector.handle_input(&InputEvent::Raw("\x1b[B".into())); // Down
        selector.handle_input(&InputEvent::Raw("\r".into()));
        assert_eq!(drain(&mut rx), vec![ShowImagesOutcome::Selected(false)]);
    }

    #[test]
    fn escape_emits_cancel() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ShowImagesSelectorComponent::new(false, tx);
        selector.handle_input(&InputEvent::Raw("\x1b".into()));
        assert_eq!(drain(&mut rx), vec![ShowImagesOutcome::Cancelled]);
    }

    #[test]
    fn current_value_false_preselects_no() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ShowImagesSelectorComponent::new(false, tx);
        selector.handle_input(&InputEvent::Raw("\r".into()));
        assert_eq!(drain(&mut rx), vec![ShowImagesOutcome::Selected(false)]);
    }
}
