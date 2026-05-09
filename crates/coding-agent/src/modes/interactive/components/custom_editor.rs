//! Coding-agent specific editor wrapper that adds app-level shortcut routing
//! on top of [`hand_tui::EditorComponent`].
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/custom-editor.ts`.
//!
//! ## Why a wrapper rather than inheritance
//!
//! The TS source `class CustomEditor extends Editor` and overrides
//! `handleInput` to intercept app-level shortcuts before delegating to the
//! base editor. Rust has no inheritance, and our conversion guidelines (see
//! `.claude/conversion-guidelines.md`) forbid extending the base class via a
//! supertrait — instead, [`CustomEditor`] **wraps** an
//! [`EditorComponent`] by composition. The wrapped editor is exposed via
//! [`CustomEditor::editor`] / [`CustomEditor::editor_mut`] so callers can
//! drive it just like a bare editor when they need to.
//!
//! ## Why string-keyed shortcuts
//!
//! pi-mono's `CustomEditor` consults a `KeybindingsManager` indexed by
//! [`AppKeybinding`] (e.g. `"app.interrupt"`, `"app.exit"`,
//! `"app.clipboard.pasteImage"`). The Rust `app.*` keybinding table has not
//! been ported yet — `core/keybindings.ts` (370 lines) is its own unit of
//! work, and `hand_tui::Keybinding` only knows `tui.*` namespaces. So this
//! wrapper exposes its own minimal mapping: callers register `(raw_key,
//! handler)` pairs and the wrapper performs literal byte-string matching
//! against the inbound `InputEvent`. Once the app keybinding port lands, this
//! type can be re-keyed in a follow-up without breaking the wider component
//! surface.
//!
//! Special handlers (`on_escape`, `on_ctrl_d`, `on_paste_image`,
//! `on_extension_shortcut`) keep parity with the TS-side dynamic slots used
//! by the driver to swap behaviours mid-flight.

use hand_tui::components::editor::EditorComponent;
use hand_tui::tui::{Component, Focusable, HandleResult, InputEvent};

/// Closure type for app-level shortcut handlers. They run on the UI thread
/// and may mutate driver state via captured channels.
pub type ActionHandler = Box<dyn FnMut() + Send>;
/// Closure type for the extension-shortcut hook. Returns `true` if the
/// extension consumed the input.
pub type ExtensionShortcutHandler = Box<dyn FnMut(&str) -> bool + Send>;

/// Custom editor that intercepts app-level shortcuts before delegating to
/// the base [`EditorComponent`].
pub struct CustomEditor {
    editor: EditorComponent,
    /// Raw bytes that trigger `app.interrupt` (typically `"\x1b"` / esc).
    interrupt_keys: Vec<String>,
    /// Raw bytes that trigger `app.exit` (typically `"\x04"` / ctrl-d).
    exit_keys: Vec<String>,
    /// Raw bytes that trigger `app.clipboard.pasteImage`.
    paste_image_keys: Vec<String>,
    /// Generic action handlers indexed by their raw key.
    actions: Vec<(String, ActionHandler)>,
    /// Optional special handlers (mirroring the TS dynamic slots).
    on_escape: Option<ActionHandler>,
    on_ctrl_d: Option<ActionHandler>,
    on_paste_image: Option<ActionHandler>,
    on_extension_shortcut: Option<ExtensionShortcutHandler>,
}

impl CustomEditor {
    /// Build a new wrapper around `editor` with no shortcuts configured.
    pub fn new(editor: EditorComponent) -> Self {
        Self {
            editor,
            interrupt_keys: Vec::new(),
            exit_keys: Vec::new(),
            paste_image_keys: Vec::new(),
            actions: Vec::new(),
            on_escape: None,
            on_ctrl_d: None,
            on_paste_image: None,
            on_extension_shortcut: None,
        }
    }

    /// Borrow the wrapped editor (read-only).
    pub fn editor(&self) -> &EditorComponent {
        &self.editor
    }

    /// Borrow the wrapped editor mutably.
    pub fn editor_mut(&mut self) -> &mut EditorComponent {
        &mut self.editor
    }

    /// Configure the raw-key set that triggers `app.interrupt`.
    pub fn set_interrupt_keys(&mut self, keys: Vec<String>) {
        self.interrupt_keys = keys;
    }

    /// Configure the raw-key set that triggers `app.exit`.
    pub fn set_exit_keys(&mut self, keys: Vec<String>) {
        self.exit_keys = keys;
    }

    /// Configure the raw-key set that triggers `app.clipboard.pasteImage`.
    pub fn set_paste_image_keys(&mut self, keys: Vec<String>) {
        self.paste_image_keys = keys;
    }

    /// Register a generic action handler bound to a single raw-key sequence.
    pub fn on_action(&mut self, raw_key: impl Into<String>, handler: ActionHandler) {
        self.actions.push((raw_key.into(), handler));
    }

    /// Set the dynamic `onEscape` handler. Replaces any prior handler.
    pub fn set_on_escape(&mut self, handler: Option<ActionHandler>) {
        self.on_escape = handler;
    }

    /// Set the dynamic `onCtrlD` handler. Replaces any prior handler.
    pub fn set_on_ctrl_d(&mut self, handler: Option<ActionHandler>) {
        self.on_ctrl_d = handler;
    }

    /// Set the dynamic `onPasteImage` handler.
    pub fn set_on_paste_image(&mut self, handler: Option<ActionHandler>) {
        self.on_paste_image = handler;
    }

    /// Set the extension shortcut hook. Returns `true` from the closure to
    /// indicate the input was consumed.
    pub fn set_on_extension_shortcut(&mut self, handler: Option<ExtensionShortcutHandler>) {
        self.on_extension_shortcut = handler;
    }

    fn matches(keys: &[String], raw: &str) -> bool {
        keys.iter().any(|k| k == raw)
    }
}

impl Component for CustomEditor {
    fn render(&self, width: u16) -> Vec<String> {
        self.editor.render(width)
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        // Only intercept Raw payloads. Structured Key events bypass
        // app-level shortcuts and go straight to the base editor.
        let raw = match event {
            InputEvent::Raw(s) => s.clone(),
            _ => return self.editor.handle_input(event),
        };

        // 1. Extension-registered shortcut gets first crack.
        if let Some(handler) = self.on_extension_shortcut.as_mut()
            && handler(&raw)
        {
            return HandleResult::Handled;
        }

        // 2. Paste-image keybinding.
        if Self::matches(&self.paste_image_keys, &raw)
            && let Some(handler) = self.on_paste_image.as_mut()
        {
            handler();
            return HandleResult::Handled;
        }

        // 3. Interrupt — only fires the handler when autocomplete is *not*
        // active, mirroring the TS source. When autocomplete is active the
        // base editor consumes the escape to dismiss the popup.
        if Self::matches(&self.interrupt_keys, &raw) {
            if self.editor.autocomplete_state().is_none()
                && let Some(handler) = self.on_escape.as_mut()
            {
                handler();
                return HandleResult::Handled;
            }
            // Fall through to base editor for autocomplete-cancel.
            return self.editor.handle_input(event);
        }

        // 4. Exit (Ctrl-D) — only when the editor buffer is empty.
        if Self::matches(&self.exit_keys, &raw)
            && self.editor.text().is_empty()
            && let Some(handler) = self.on_ctrl_d.as_mut()
        {
            handler();
            return HandleResult::Handled;
        }

        // 5. Generic action handlers (skip the two we already special-cased).
        for (key, handler) in self.actions.iter_mut() {
            if key == &raw {
                handler();
                return HandleResult::Handled;
            }
        }

        // 6. Pass everything else to the base editor.
        self.editor.handle_input(event)
    }

    fn invalidate(&mut self) {
        self.editor.invalidate();
    }
}

impl Focusable for CustomEditor {
    fn focused(&self) -> bool {
        self.editor.focused()
    }

    fn set_focused(&mut self, focused: bool) {
        self.editor.set_focused(focused);
    }

    fn cursor_position(&self) -> Option<(u16, u16)> {
        self.editor.cursor_position()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn ev(s: &str) -> InputEvent {
        InputEvent::Raw(s.to_string())
    }

    #[test]
    fn extension_shortcut_runs_first() {
        let mut e = CustomEditor::new(EditorComponent::new());
        e.set_focused(true);
        let hit = Arc::new(Mutex::new(false));
        let hit_c = Arc::clone(&hit);
        e.set_on_extension_shortcut(Some(Box::new(move |data| {
            if data == "@x" {
                *hit_c.lock().unwrap() = true;
                true
            } else {
                false
            }
        })));
        let r = e.handle_input(&ev("@x"));
        assert_eq!(r, HandleResult::Handled);
        assert!(*hit.lock().unwrap());
    }

    #[test]
    fn interrupt_fires_on_escape_when_autocomplete_inactive() {
        let mut e = CustomEditor::new(EditorComponent::new());
        e.set_focused(true);
        e.set_interrupt_keys(vec!["\x1b".into()]);
        let hit = Arc::new(Mutex::new(false));
        let hit_c = Arc::clone(&hit);
        e.set_on_escape(Some(Box::new(move || *hit_c.lock().unwrap() = true)));
        e.handle_input(&ev("\x1b"));
        assert!(*hit.lock().unwrap());
    }

    #[test]
    fn exit_only_fires_when_buffer_empty() {
        let mut e = CustomEditor::new(EditorComponent::new());
        e.set_focused(true);
        e.set_exit_keys(vec!["\x04".into()]);

        let hit = Arc::new(Mutex::new(0u32));
        let hit_c = Arc::clone(&hit);
        e.set_on_ctrl_d(Some(Box::new(move || {
            *hit_c.lock().unwrap() += 1;
        })));

        // Buffer empty → handler runs.
        e.handle_input(&ev("\x04"));
        assert_eq!(*hit.lock().unwrap(), 1);

        // Now type something; ctrl-d must fall through (no handler call).
        e.editor_mut().set_text("hi");
        e.handle_input(&ev("\x04"));
        assert_eq!(*hit.lock().unwrap(), 1);
    }

    #[test]
    fn generic_action_handler_runs_on_match() {
        let mut e = CustomEditor::new(EditorComponent::new());
        e.set_focused(true);
        let hit = Arc::new(Mutex::new(0u32));
        let hit_c = Arc::clone(&hit);
        e.on_action(
            "\x05", // ctrl+e
            Box::new(move || {
                *hit_c.lock().unwrap() += 1;
            }),
        );
        e.handle_input(&ev("\x05"));
        assert_eq!(*hit.lock().unwrap(), 1);
    }

    #[test]
    fn unmatched_input_falls_through_to_editor() {
        let mut e = CustomEditor::new(EditorComponent::new());
        e.set_focused(true);
        e.handle_input(&ev("a"));
        // The wrapped editor should now contain "a".
        assert_eq!(e.editor().text(), "a");
    }

    #[test]
    fn paste_image_handler_runs_on_match() {
        let mut e = CustomEditor::new(EditorComponent::new());
        e.set_focused(true);
        e.set_paste_image_keys(vec!["\x10".into()]);
        let hit = Arc::new(Mutex::new(false));
        let hit_c = Arc::clone(&hit);
        e.set_on_paste_image(Some(Box::new(move || *hit_c.lock().unwrap() = true)));
        e.handle_input(&ev("\x10"));
        assert!(*hit.lock().unwrap());
    }
}
