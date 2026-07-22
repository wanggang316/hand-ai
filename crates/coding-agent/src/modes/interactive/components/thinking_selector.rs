//! Selector for the active thinking / reasoning level.
//!
//! The option set is `Option::<ThinkingLevel>::None` for "off" plus
//! whatever ordered `ThinkingLevel` variants the active model exposes;
//! the constructor accepts the list of available levels because the
//! host decides which the model supports.
//!
//! Theming caveat: the renderer relies on `SelectListComponent`'s
//! built-in palette defaults until the coding-agent theme exposes a
//! dedicated SelectList slot.
//!
//! TODO(parity): theme integration deferred — see
//! docs/exec-plans/parity-completion.md §A1.

use hand_tui::{
    Component, Container, HandleResult, InputEvent, SelectItem, SelectListComponent,
    SelectListLayoutOptions,
};
use model::ThinkingLevel;
use tokio::sync::mpsc;

use super::dynamic_border::DynamicBorderComponent;

/// User-visible string label for a thinking level (or "off").
fn level_label(level: Option<ThinkingLevel>) -> &'static str {
    match level {
        None => "off",
        Some(ThinkingLevel::Minimal) => "minimal",
        Some(ThinkingLevel::Low) => "low",
        Some(ThinkingLevel::Medium) => "medium",
        Some(ThinkingLevel::High) => "high",
        Some(ThinkingLevel::Xhigh) => "xhigh",
        Some(ThinkingLevel::Max) => "max",
    }
}

/// Description shown next to each level (mirrors the TS `LEVEL_DESCRIPTIONS`).
fn level_description(level: Option<ThinkingLevel>) -> &'static str {
    match level {
        None => "No reasoning",
        Some(ThinkingLevel::Minimal) => "Very brief reasoning (~1k tokens)",
        Some(ThinkingLevel::Low) => "Light reasoning (~2k tokens)",
        Some(ThinkingLevel::Medium) => "Moderate reasoning (~8k tokens)",
        Some(ThinkingLevel::High) => "Deep reasoning (~16k tokens)",
        Some(ThinkingLevel::Xhigh) => "Extra-high reasoning (~32k tokens)",
        Some(ThinkingLevel::Max) => "Maximum reasoning",
    }
}

/// Outcome dispatched on the channel handed to
/// [`ThinkingSelectorComponent::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingOutcome {
    /// User confirmed a level. `None` means the user chose "off".
    Selected(Option<ThinkingLevel>),
    /// User cancelled the selector (Esc).
    Cancelled,
}

/// Container that renders the thinking-level picker bordered top and bottom.
pub struct ThinkingSelectorComponent {
    container: Container,
}

impl ThinkingSelectorComponent {
    /// Build a selector. `available_levels` is the ordered list shown to the
    /// user (use `None` for "off"). The selector pre-positions the cursor on
    /// `current_level` if present in the list.
    pub fn new(
        current_level: Option<ThinkingLevel>,
        available_levels: Vec<Option<ThinkingLevel>>,
        tx: mpsc::UnboundedSender<ThinkingOutcome>,
    ) -> Self {
        let items: Vec<SelectItem> = available_levels
            .iter()
            .map(|level| {
                let label = level_label(*level);
                SelectItem::new(label, label).with_description(level_description(*level))
            })
            .collect();

        let visible = items.len().max(1);
        let mut select = SelectListComponent::new(items)
            .with_visible_count(visible)
            .with_layout(SelectListLayoutOptions {
                min_primary_column_width: Some(12),
                max_primary_column_width: Some(32),
            });

        if let Some(idx) = available_levels.iter().position(|l| *l == current_level) {
            select.set_selected_index(idx);
        }

        // Map item value -> Option<ThinkingLevel>. The lookup table is captured
        // by the on_select closure so the selection callback can hand the
        // typed level back to the host.
        let levels = available_levels.clone();
        let select_tx = tx.clone();
        select.set_on_select(Box::new(move |item| {
            let chosen = levels
                .iter()
                .find(|l| level_label(**l) == item.value)
                .copied()
                .unwrap_or(None);
            let _ = select_tx.send(ThinkingOutcome::Selected(chosen));
        }));
        let cancel_tx = tx;
        select.set_on_cancel(Box::new(move || {
            let _ = cancel_tx.send(ThinkingOutcome::Cancelled);
        }));

        let mut container = Container::new();
        container.add_child(Box::new(DynamicBorderComponent::new()));
        container.add_child(Box::new(select));
        container.add_child(Box::new(DynamicBorderComponent::new()));

        Self { container }
    }
}

impl Component for ThinkingSelectorComponent {
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

    fn drain(rx: &mut mpsc::UnboundedReceiver<ThinkingOutcome>) -> Vec<ThinkingOutcome> {
        let mut out = Vec::new();
        while let Ok(o) = rx.try_recv() {
            out.push(o);
        }
        out
    }

    fn levels() -> Vec<Option<ThinkingLevel>> {
        vec![
            None,
            Some(ThinkingLevel::Minimal),
            Some(ThinkingLevel::Low),
            Some(ThinkingLevel::Medium),
            Some(ThinkingLevel::High),
            Some(ThinkingLevel::Xhigh),
            Some(ThinkingLevel::Max),
        ]
    }

    #[test]
    fn renders_all_provided_levels() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let selector = ThinkingSelectorComponent::new(Some(ThinkingLevel::Medium), levels(), tx);
        let body = selector.render(60).join("\n");
        for label in ["off", "minimal", "low", "medium", "high", "xhigh"] {
            assert!(body.contains(label), "missing label {label}");
        }
        assert!(body.contains("~8k tokens"));
    }

    #[test]
    fn enter_emits_current_selection() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector =
            ThinkingSelectorComponent::new(Some(ThinkingLevel::Medium), levels(), tx);
        selector.handle_input(&InputEvent::Raw("\r".into()));
        assert_eq!(
            drain(&mut rx),
            vec![ThinkingOutcome::Selected(Some(ThinkingLevel::Medium))]
        );
    }

    #[test]
    fn enter_at_off_emits_none() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ThinkingSelectorComponent::new(None, levels(), tx);
        selector.handle_input(&InputEvent::Raw("\r".into()));
        assert_eq!(drain(&mut rx), vec![ThinkingOutcome::Selected(None)]);
    }

    #[test]
    fn down_then_enter_advances_one_step() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ThinkingSelectorComponent::new(None, levels(), tx);
        selector.handle_input(&InputEvent::Raw("\x1b[B".into())); // Down → minimal
        selector.handle_input(&InputEvent::Raw("\r".into()));
        assert_eq!(
            drain(&mut rx),
            vec![ThinkingOutcome::Selected(Some(ThinkingLevel::Minimal))]
        );
    }

    #[test]
    fn escape_cancels() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ThinkingSelectorComponent::new(None, levels(), tx);
        selector.handle_input(&InputEvent::Raw("\x1b".into()));
        assert_eq!(drain(&mut rx), vec![ThinkingOutcome::Cancelled]);
    }

    #[test]
    fn empty_available_levels_renders_no_items_hint() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let selector = ThinkingSelectorComponent::new(None, vec![], tx);
        let body = selector.render(40).join("\n");
        assert!(body.contains("(no items)"));
    }
}
