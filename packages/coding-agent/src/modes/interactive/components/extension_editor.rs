//! Multi-line editor dialog used by extensions.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/extension-editor.ts`.
//!
//! ## Surface differences from the TS source
//!
//! 1. **Composition over inheritance.** The TS class extends `Container` and
//!    inherits child rendering from pi-tui. The Rust port owns the children
//!    explicitly — top border, title, embedded [`EditorComponent`], hint,
//!    bottom border — and renders them in sequence, mirroring the layout.
//!
//! 2. **`tui.start()` / `tui.stop()` is the driver's job.** pi-mono's
//!    `openExternalEditor` calls `this.tui.stop()` before spawning `$EDITOR`
//!    and `this.tui.start()` after. The Rust port surfaces that hook as an
//!    [`ExtensionEditorEvent::ExternalEditorRequested`] event with the
//!    current text — the driver is expected to suspend the TUI, run the
//!    editor, and feed the new text back via
//!    [`ExtensionEditorComponent::set_text`]. This keeps the component pure
//!    and lets the driver manage the rendering lifecycle (per the
//!    conversion guidelines: channels over `Box<dyn Fn>`).
//!
//! 3. **Submit / cancel through a channel.** Instead of constructor
//!    callbacks, the component takes an [`mpsc::Sender`] and emits
//!    [`ExtensionEditorEvent::Submit`] / `Cancel`.

use std::sync::mpsc::Sender;

use hand_tui::components::editor::EditorComponent;
use hand_tui::keybindings::{Keybinding, get_keybindings};
use hand_tui::tui::{Component, Focusable, HandleResult, InputEvent};
use hand_tui::utils::visible_width;

use super::dynamic_border::DynamicBorderComponent;
use super::keybinding_hints::key_hint_for;

/// Events surfaced by [`ExtensionEditorComponent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionEditorEvent {
    /// User pressed `tui.select.confirm` — the payload is the current text.
    Submit(String),
    /// User pressed `tui.select.cancel` (escape / ctrl+c).
    Cancel,
    /// User pressed the configured external-editor shortcut. The driver
    /// should suspend the TUI, spawn `$VISUAL`/`$EDITOR` against the payload,
    /// then call [`ExtensionEditorComponent::set_text`] with the result and
    /// resume rendering.
    ExternalEditorRequested(String),
}

/// Multi-line editor dialog shown by extensions to capture free-form text.
pub struct ExtensionEditorComponent {
    title: String,
    border: DynamicBorderComponent,
    editor: EditorComponent,
    hint: String,
    events: Sender<ExtensionEditorEvent>,
    focused: bool,
    /// Raw key sequence that triggers the external-editor flow. The TS
    /// source binds it to `app.editor.external` via the (not-yet-ported)
    /// app-keybinding manager. Drivers that want the feature pass a
    /// concrete sequence (e.g. `"\x07"` for ctrl-g, the TS default); pass
    /// `None` to disable.
    external_editor_key: Option<String>,
}

impl ExtensionEditorComponent {
    /// Construct a new dialog. `prefill` is loaded into the editor as
    /// initial text. `external_editor_key` enables the `Ctrl+G`-style
    /// suspend-and-edit flow when set.
    pub fn new(
        title: impl Into<String>,
        prefill: Option<&str>,
        external_editor_key: Option<String>,
        events: Sender<ExtensionEditorEvent>,
    ) -> Self {
        let mut editor = EditorComponent::new();
        if let Some(text) = prefill {
            editor.set_text(text);
        }

        let mut hint = format!(
            "{}  {}  {}",
            key_hint_for("tui.select.confirm", "submit"),
            key_hint_for("tui.input.newLine", "newline"),
            key_hint_for("tui.select.cancel", "cancel"),
        );
        if external_editor_key.is_some() {
            hint.push_str("  ctrl+g external editor");
        }

        Self {
            title: title.into(),
            border: DynamicBorderComponent::new(),
            editor,
            hint,
            events,
            focused: false,
            external_editor_key,
        }
    }

    /// Borrow the embedded editor (read-only). Useful for inspecting state
    /// from a driver.
    pub fn editor(&self) -> &EditorComponent {
        &self.editor
    }

    /// Borrow the embedded editor mutably. Useful for the driver to feed
    /// back the text after running an external editor.
    pub fn editor_mut(&mut self) -> &mut EditorComponent {
        &mut self.editor
    }

    /// Replace the editor's text. Mirrors `editor.setText()` in pi-mono;
    /// callers (or the driver) use it to restore content after the external
    /// editor flow completes.
    pub fn set_text(&mut self, text: &str) {
        self.editor.set_text(text);
    }

    /// Current editor text.
    pub fn text(&self) -> String {
        self.editor.text()
    }
}

const ACCENT: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

fn pad_line(line: &str, width: u16) -> String {
    let target = width as usize;
    let current = visible_width(line);
    if current >= target {
        line.to_string()
    } else {
        format!("{line}{}", " ".repeat(target - current))
    }
}

impl Component for ExtensionEditorComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let mut out = Vec::new();
        out.extend(self.border.render(width));
        out.push(pad_line("", width));
        out.push(pad_line(&format!("{ACCENT}{}{RESET}", self.title), width));
        out.push(pad_line("", width));
        out.extend(self.editor.render(width));
        out.push(pad_line("", width));
        out.push(pad_line(&self.hint, width));
        out.push(pad_line("", width));
        out.extend(self.border.render(width));
        out
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        let raw = match event {
            InputEvent::Raw(s) => s.clone(),
            _ => return self.editor.handle_input(event),
        };

        let kb = get_keybindings();

        // Cancel takes priority — TS source intercepts before everything.
        if kb.matches(&raw, Keybinding::SelectCancel) {
            let _ = self.events.send(ExtensionEditorEvent::Cancel);
            return HandleResult::Handled;
        }

        // External-editor shortcut.
        if let Some(key) = &self.external_editor_key
            && key == &raw
        {
            let _ = self
                .events
                .send(ExtensionEditorEvent::ExternalEditorRequested(
                    self.editor.text(),
                ));
            return HandleResult::Handled;
        }

        // Submit (Enter) — we send via channel and consume.
        if kb.matches(&raw, Keybinding::SelectConfirm) {
            let _ = self
                .events
                .send(ExtensionEditorEvent::Submit(self.editor.text()));
            return HandleResult::Handled;
        }

        // Everything else goes to the editor.
        self.editor.handle_input(event)
    }

    fn invalidate(&mut self) {
        self.editor.invalidate();
    }
}

impl Focusable for ExtensionEditorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.editor.set_focused(focused);
    }

    fn cursor_position(&self) -> Option<(u16, u16)> {
        self.editor.cursor_position()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn make() -> (
        ExtensionEditorComponent,
        mpsc::Receiver<ExtensionEditorEvent>,
    ) {
        let (tx, rx) = mpsc::channel();
        let comp =
            ExtensionEditorComponent::new("Edit prompt", Some("hello"), Some("\x07".into()), tx);
        (comp, rx)
    }

    #[test]
    fn prefill_loads_into_editor() {
        let (c, _rx) = make();
        assert_eq!(c.text(), "hello");
    }

    #[test]
    fn cancel_emits_event_on_escape() {
        let (mut c, rx) = make();
        c.set_focused(true);
        c.handle_input(&InputEvent::Raw("\x1b".into()));
        match rx.try_recv() {
            Ok(ExtensionEditorEvent::Cancel) => {}
            other => panic!("expected Cancel, got {other:?}"),
        }
    }

    #[test]
    fn external_editor_emits_request_with_current_text() {
        let (mut c, rx) = make();
        c.set_focused(true);
        c.handle_input(&InputEvent::Raw("\x07".into()));
        match rx.try_recv() {
            Ok(ExtensionEditorEvent::ExternalEditorRequested(t)) => assert_eq!(t, "hello"),
            other => panic!("expected ExternalEditorRequested, got {other:?}"),
        }
    }

    #[test]
    fn submit_carries_current_text() {
        let (mut c, rx) = make();
        c.set_focused(true);
        c.set_text("world");
        // Default `Keybinding::SelectConfirm` matches "\r".
        c.handle_input(&InputEvent::Raw("\r".into()));
        match rx.try_recv() {
            Ok(ExtensionEditorEvent::Submit(t)) => assert_eq!(t, "world"),
            other => panic!("expected Submit, got {other:?}"),
        }
    }

    #[test]
    fn render_includes_title_and_hint() {
        let (c, _rx) = make();
        let lines = c.render(60);
        let blob = lines.join("\n");
        assert!(blob.contains("Edit prompt"));
        assert!(blob.contains("submit"));
        assert!(blob.contains("cancel"));
    }
}
