//! Settings selector dialog.
//!
//! ## Current scope (minimal viable)
//!
//! The full settings selector is a large component that builds a
//! `Vec<SettingItem>` from a `SettingsConfig` view-model, wires
//! per-item callbacks, and embeds submenus that pop a `SelectList`
//! for enum choices. To stay within scope
//! and avoid blocking on the (not-yet-ported) submenu primitives in
//! `hand_tui`, the Rust port renders **only the top-level settings list**:
//!
//! 1. Caller hands in a fully-built `Vec<SettingEntry>` (the same primitive
//!    `SettingsListComponent` consumes), wrapping their domain config with
//!    whatever id scheme they like.
//! 2. The component owns a [`SettingsListComponent`], routes inbound key
//!    events into it, and emits [`SettingsSelectorEvent`]s on an
//!    [`mpsc::UnboundedSender`].
//! 3. Submenu / sub-select-list flows are left as `TODO(parity)` and are
//!    expected to be driven by the host (push another component on the
//!    stack, pop it back when done).
//!
//! Search across settings (`enableSearch: true` in TS) is also deferred —
//! `SettingsListComponent` documents that surface as out-of-scope for the
//! crate. See the file header note in `hand-tui::components::settings_list`.

use hand_tui::Component;
use hand_tui::components::settings_list::{
    SettingEntry, SettingValue, SettingsListComponent, SettingsListTheme,
};
use hand_tui::keybindings::{Keybinding, get_keybindings};
use hand_tui::tui::{HandleResult, InputEvent};
use tokio::sync::mpsc;

use super::dynamic_border::DynamicBorderComponent;

/// Events surfaced by [`SettingsSelectorComponent`]. The driver maps `id`
/// strings back to typed config mutations.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsSelectorEvent {
    /// User changed a setting. `value` is the post-change rendering of the
    /// `SettingValue` (`true` / `false` / enum choice / number).
    Changed { id: String, value: String },
    /// User pressed `tui.select.cancel`.
    Cancelled,
}

/// Settings selector dialog rendering a borderless top + [`SettingsListComponent`]
/// + borderless bottom panel.
pub struct SettingsSelectorComponent {
    list: SettingsListComponent,
    border: DynamicBorderComponent,
    events: mpsc::UnboundedSender<SettingsSelectorEvent>,
    /// Snapshot of the previous value for each entry, indexed by id. Used
    /// to determine whether a `toggle_edit` actually mutated the value (so
    /// we don't fire spurious `Changed` events on no-ops).
    snapshot: Vec<(String, String)>,
}

impl SettingsSelectorComponent {
    /// Construct a new selector. `entries` are the settings to display.
    /// `max_visible` mirrors the `10` rows the TS source uses.
    pub fn new(
        entries: Vec<SettingEntry>,
        max_visible: usize,
        events: mpsc::UnboundedSender<SettingsSelectorEvent>,
    ) -> Self {
        let snapshot = entries
            .iter()
            .map(|e| (e.key.clone(), value_string(&e.value)))
            .collect();
        let list = SettingsListComponent::new(entries)
            .with_max_visible(max_visible)
            .with_description(true)
            .with_hint(true)
            .with_theme(SettingsListTheme::default());
        Self {
            list,
            border: DynamicBorderComponent::new(),
            events,
            snapshot,
        }
    }

    /// Borrow the inner settings list (mostly for tests).
    pub fn list(&self) -> &SettingsListComponent {
        &self.list
    }

    /// Refresh the cached snapshot after an external mutation. Drivers that
    /// rebuild the entry list out-of-band should call this to keep
    /// change-detection sane.
    pub fn refresh_snapshot(&mut self) {
        self.snapshot = self
            .list
            .entries()
            .iter()
            .map(|e| (e.key.clone(), value_string(&e.value)))
            .collect();
    }

    fn detect_change_and_emit(&mut self) {
        // Walk both lists in parallel; emit Changed for any divergence and
        // refresh the snapshot afterwards.
        let mut events = Vec::new();
        for (idx, entry) in self.list.entries().iter().enumerate() {
            let new_value = value_string(&entry.value);
            if let Some((id, old_value)) = self.snapshot.get(idx)
                && id == &entry.key
                && old_value != &new_value
            {
                events.push(SettingsSelectorEvent::Changed {
                    id: entry.key.clone(),
                    value: new_value.clone(),
                });
            }
        }
        if !events.is_empty() {
            self.refresh_snapshot();
            for ev in events {
                let _ = self.events.send(ev);
            }
        }
    }
}

fn value_string(v: &SettingValue) -> String {
    match v {
        SettingValue::Bool(b) => b.to_string(),
        SettingValue::Enum { choices, selected } => {
            choices.get(*selected).cloned().unwrap_or_default()
        }
        SettingValue::String(s) => s.clone(),
        SettingValue::Number(n) => n.to_string(),
    }
}

impl Component for SettingsSelectorComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let mut out = Vec::new();
        out.extend(self.border.render(width));
        out.extend(self.list.render(width));
        out.extend(self.border.render(width));
        out
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        let raw = match event {
            InputEvent::Raw(s) => s.clone(),
            _ => return HandleResult::Ignored,
        };

        let kb = get_keybindings();

        // While editing a String/Number, hand keystrokes off to the list's
        // edit buffer via toggle_edit / cancel_edit. We can't poke the
        // edit buffer directly (private), so we fall back to confirming on
        // Enter and cancelling on Escape — string edits aren't usable until
        // SettingsListComponent grows a public edit-buffer API.
        // TODO(parity): wire string-edit keystrokes once SettingsListComponent
        // exposes edit_buffer mutators.
        if self.list.is_editing() {
            if kb.matches(&raw, Keybinding::SelectConfirm) {
                self.list.toggle_edit();
                self.detect_change_and_emit();
                return HandleResult::Handled;
            }
            if kb.matches(&raw, Keybinding::SelectCancel) {
                self.list.cancel_edit();
                return HandleResult::Handled;
            }
            return HandleResult::Ignored;
        }

        if kb.matches(&raw, Keybinding::SelectUp) {
            self.list.prev();
            return HandleResult::Handled;
        }
        if kb.matches(&raw, Keybinding::SelectDown) {
            self.list.next();
            return HandleResult::Handled;
        }
        if kb.matches(&raw, Keybinding::SelectConfirm) {
            self.list.toggle_edit();
            self.detect_change_and_emit();
            return HandleResult::Handled;
        }
        if kb.matches(&raw, Keybinding::SelectCancel) {
            let _ = self.events.send(SettingsSelectorEvent::Cancelled);
            return HandleResult::Handled;
        }

        HandleResult::Ignored
    }

    fn invalidate(&mut self) {
        self.list.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<SettingEntry> {
        vec![
            SettingEntry {
                key: "autocompact".into(),
                value: SettingValue::Bool(true),
                description: "Auto-compact context".into(),
            },
            SettingEntry {
                key: "transport".into(),
                value: SettingValue::Enum {
                    choices: vec!["sse".into(), "websocket".into(), "auto".into()],
                    selected: 2,
                },
                description: "Transport selection".into(),
            },
        ]
    }

    fn make() -> (
        SettingsSelectorComponent,
        mpsc::UnboundedReceiver<SettingsSelectorEvent>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        (SettingsSelectorComponent::new(entries(), 10, tx), rx)
    }

    #[test]
    fn cancel_emits_cancelled_event() {
        let (mut c, mut rx) = make();
        c.handle_input(&InputEvent::Raw("\x1b".into()));
        match rx.try_recv() {
            Ok(SettingsSelectorEvent::Cancelled) => {}
            other => panic!("expected Cancelled, got {other:?}"),
        }
    }

    #[test]
    fn toggling_bool_emits_changed_event() {
        let (mut c, mut rx) = make();
        // First entry is selected by default; confirm flips Bool.
        c.handle_input(&InputEvent::Raw("\r".into()));
        match rx.try_recv() {
            Ok(SettingsSelectorEvent::Changed { id, value }) => {
                assert_eq!(id, "autocompact");
                assert_eq!(value, "false");
            }
            other => panic!("expected Changed, got {other:?}"),
        }
    }

    #[test]
    fn cycling_enum_emits_changed_event() {
        let (mut c, mut rx) = make();
        // Move to second entry (transport).
        c.handle_input(&InputEvent::Raw("\x1b[B".into())); // down arrow
        // Confirm cycles enum from "auto" (idx 2) to "sse" (idx 0).
        c.handle_input(&InputEvent::Raw("\r".into()));
        match rx.try_recv() {
            Ok(SettingsSelectorEvent::Changed { id, value }) => {
                assert_eq!(id, "transport");
                assert_eq!(value, "sse");
            }
            other => panic!("expected Changed, got {other:?}"),
        }
    }

    #[test]
    fn navigation_does_not_emit_change() {
        let (mut c, mut rx) = make();
        c.handle_input(&InputEvent::Raw("\x1b[B".into()));
        c.handle_input(&InputEvent::Raw("\x1b[A".into()));
        assert!(rx.try_recv().is_err(), "navigation must not emit events");
    }

    #[test]
    fn render_includes_borders_and_entries() {
        let (c, _rx) = make();
        let lines = c.render(60);
        let blob = lines.join("\n");
        assert!(blob.contains("autocompact"));
        assert!(blob.contains("transport"));
    }
}
