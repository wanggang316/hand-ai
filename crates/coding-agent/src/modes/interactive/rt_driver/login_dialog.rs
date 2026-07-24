//! The rt-native **API-key login dialog** — a single-line secret input migrated
//! onto the [overlay runtime](super::overlay).
//!
//! It is a [`SelectorController`] with a **text field** (unlike the list-style
//! selectors): it accumulates the typed / pasted API key and, on Enter, emits the
//! trimmed value. Two migration-critical behaviours it nails:
//!
//! - **Paste lands whole** ([`handle_paste`](SelectorController::handle_paste)): a
//!   multi-character API key arriving as one bracketed-paste event is inserted in a
//!   single shot, never folded to one character (VAL-OVERLAY-027 — the fix away from
//!   the legacy single-character paste collapse).
//! - **The secret never surfaces as plaintext.** The on-screen field renders masked
//!   (one `•` per character), and the dialog holds the key in memory only until it
//!   emits it on Enter — it never commits the plaintext to scrollback. The driver's
//!   confirmation line names the *provider*, not the key, so a submitted key is
//!   absent from every captured screen / scrollback frame (VAL-OVERLAY-016).
//!
//! Editing surface: printable keys append, **Backspace** deletes the last
//! character, **Enter** submits the trimmed value (an empty submit is a cancel), and
//! **Esc** cancels. Each terminal key raises the
//! [`DoneSignal`](super::overlay::DoneSignal) so the runtime unmounts the dialog.

use std::sync::atomic::Ordering;

use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::HandleOutcome;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::mpsc;

use super::overlay::{DoneSignal, SelectorController};

/// The outcome the key dialog emits on its channel — exactly one per open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyDialogOutcome {
    /// The user submitted a non-empty (trimmed) key. Carries the trimmed value.
    Submitted(String),
    /// The user cancelled (Esc, or Enter on an empty field).
    Cancelled,
}

/// The rt-native API-key entry dialog.
pub struct LoginKeyDialog {
    /// The display name shown in the prompt (e.g. `"Anthropic"`).
    provider_name: String,
    /// The key the user has entered so far. Held in memory only; never committed to
    /// scrollback and always rendered masked.
    value: String,
    /// The outcome channel; exactly one [`KeyDialogOutcome`] is sent.
    tx: mpsc::UnboundedSender<KeyDialogOutcome>,
    /// Raised on the terminal key (Enter/Esc) so the overlay runtime unmounts this.
    done: DoneSignal,
}

impl LoginKeyDialog {
    /// Build a key dialog prompting for `provider_name`'s API key.
    #[must_use]
    pub fn new(
        provider_name: impl Into<String>,
        tx: mpsc::UnboundedSender<KeyDialogOutcome>,
        done: DoneSignal,
    ) -> Self {
        Self {
            provider_name: provider_name.into(),
            value: String::new(),
            tx,
            done,
        }
    }

    /// The number of characters entered so far (test aid). Never exposes the value.
    #[must_use]
    pub fn len(&self) -> usize {
        self.value.chars().count()
    }

    /// Whether the field is empty (test aid).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// Insert a whole paste payload in one shot — the migration fix: the entire API
    /// key lands intact rather than being folded to a single character
    /// (VAL-OVERLAY-027). A payload with embedded newlines has them stripped so a
    /// single-line key field stays single-line (a paste of `sk-...\n` still yields
    /// `sk-...`).
    fn insert_paste(&mut self, text: &str) {
        for ch in text.chars() {
            if ch != '\n' && ch != '\r' {
                self.value.push(ch);
            }
        }
    }

    /// Emit the submitted (trimmed) key, or a cancel when it is empty after trimming.
    fn submit(&self) {
        let trimmed = self.value.trim();
        if trimmed.is_empty() {
            // An empty submit is a cancel — the driver reports "[/login cancelled …]".
            let _ = self.tx.send(KeyDialogOutcome::Cancelled);
        } else {
            let _ = self
                .tx
                .send(KeyDialogOutcome::Submitted(trimmed.to_string()));
        }
    }

    /// Emit the cancel outcome (Esc).
    fn cancel(&self) {
        let _ = self.tx.send(KeyDialogOutcome::Cancelled);
    }

    /// Whether `key` is a printable character to append. Control-chorded keys are
    /// not key text.
    fn typed_char(key: &RtKey) -> Option<char> {
        use crossterm::event::{KeyCode, KeyModifiers};
        if key
            .raw
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return None;
        }
        match key.raw.code {
            KeyCode::Char(c) => Some(c),
            _ => None,
        }
    }

    /// The dialog body as styled lines: the prompt, the masked field, and the
    /// `(esc to cancel)` hint. The field is masked so the secret never appears in a
    /// captured frame (VAL-OVERLAY-016).
    fn body_lines(&self, width: u16) -> Vec<Line<'static>> {
        let muted = Style::default().fg(Color::DarkGray);
        let accent = Style::default().fg(Color::Cyan);

        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(Span::styled(
            format!("Login to {}", self.provider_name),
            accent.add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(String::new()));
        lines.push(Line::from(Span::styled(
            format!("Paste your {} API key and press Enter:", self.provider_name),
            muted,
        )));

        // The masked field: one bullet per entered character, never the plaintext.
        let masked: String = "•".repeat(self.value.chars().count());
        lines.push(Line::from(vec![
            Span::styled("> ", accent),
            Span::raw(masked),
        ]));

        lines.push(Line::from(String::new()));
        lines.push(Line::from(Span::styled(
            "(esc to cancel)".to_string(),
            muted,
        )));

        let _ = width;
        lines
    }
}

impl SelectorController for LoginKeyDialog {
    fn render_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.body_lines(width)
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        match key.key_id.as_deref() {
            Some("enter") => {
                self.submit();
                self.done.store(true, Ordering::SeqCst);
                HandleOutcome::Consumed
            }
            Some("escape") => {
                self.cancel();
                self.done.store(true, Ordering::SeqCst);
                HandleOutcome::Consumed
            }
            Some("backspace") => {
                self.value.pop();
                HandleOutcome::Consumed
            }
            _ => {
                if let Some(c) = Self::typed_char(key) {
                    self.value.push(c);
                }
                // A modal dialog owns every key so it never reaches the editor
                // beneath (VAL-OVERLAY-005).
                HandleOutcome::Consumed
            }
        }
    }

    fn handle_paste(&mut self, text: &str) -> HandleOutcome {
        // VAL-OVERLAY-027: the whole pasted key lands in one shot.
        self.insert_paste(text);
        HandleOutcome::Consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn dialog() -> (
        LoginKeyDialog,
        mpsc::UnboundedReceiver<KeyDialogOutcome>,
        DoneSignal,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let done = super::super::overlay::new_done_signal();
        let dialog = LoginKeyDialog::new("Anthropic", tx, done.clone());
        (dialog, rx, done)
    }

    fn key_id(id: &str) -> RtKey {
        RtKey {
            key_id: Some(id.to_string()),
            raw: KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        }
    }

    fn char_key(c: char) -> RtKey {
        RtKey {
            key_id: Some(c.to_string()),
            raw: KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        }
    }

    fn body_text(d: &LoginKeyDialog) -> String {
        d.body_lines(80)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<KeyDialogOutcome>) -> Option<KeyDialogOutcome> {
        rx.try_recv().ok()
    }

    // --- paste lands whole (VAL-OVERLAY-027) ------------------------------

    #[test]
    fn a_pasted_key_lands_whole_not_folded_to_one_char() {
        let (mut d, mut rx, done) = dialog();
        let key = "sk-ant-api03-THE-WHOLE-KEY-abcdefghijklmnop";
        d.handle_paste(key);
        assert_eq!(d.len(), key.chars().count(), "the entire paste landed");
        d.handle_key(&key_id("enter"));
        assert!(done.load(Ordering::SeqCst));
        assert_eq!(
            drain(&mut rx),
            Some(KeyDialogOutcome::Submitted(key.to_string())),
            "the whole key round-trips on submit"
        );
    }

    #[test]
    fn paste_strips_trailing_newline() {
        let (mut d, mut rx, _done) = dialog();
        d.handle_paste("sk-key-with-newline\n");
        d.handle_key(&key_id("enter"));
        assert_eq!(
            drain(&mut rx),
            Some(KeyDialogOutcome::Submitted(
                "sk-key-with-newline".to_string()
            ))
        );
    }

    // --- the secret never surfaces (VAL-OVERLAY-016) ----------------------

    #[test]
    fn the_field_is_masked_never_plaintext() {
        let (mut d, _rx, _done) = dialog();
        d.handle_paste("super-secret-key-123");
        let body = body_text(&d);
        assert!(
            !body.contains("super-secret-key-123"),
            "the plaintext key must never appear in the rendered field: {body}"
        );
        // The mask shows one bullet per character.
        assert!(body.contains(&"•".repeat("super-secret-key-123".len())));
    }

    // --- typing + backspace edit ------------------------------------------

    #[test]
    fn typed_chars_append_and_backspace_deletes() {
        let (mut d, mut rx, _done) = dialog();
        for c in "abcd".chars() {
            d.handle_key(&char_key(c));
        }
        assert_eq!(d.len(), 4);
        d.handle_key(&key_id("backspace"));
        assert_eq!(d.len(), 3);
        d.handle_key(&key_id("enter"));
        assert_eq!(
            drain(&mut rx),
            Some(KeyDialogOutcome::Submitted("abc".to_string()))
        );
    }

    // --- enter submits trimmed; empty = cancel ----------------------------

    #[test]
    fn enter_submits_the_trimmed_value() {
        let (mut d, mut rx, _done) = dialog();
        d.handle_paste("   sk-padded-key   ");
        d.handle_key(&key_id("enter"));
        assert_eq!(
            drain(&mut rx),
            Some(KeyDialogOutcome::Submitted("sk-padded-key".to_string())),
            "the submitted key is trimmed"
        );
    }

    #[test]
    fn empty_submit_is_a_cancel() {
        let (mut d, mut rx, done) = dialog();
        d.handle_key(&key_id("enter"));
        assert!(done.load(Ordering::SeqCst));
        assert_eq!(
            drain(&mut rx),
            Some(KeyDialogOutcome::Cancelled),
            "an empty Enter cancels rather than submitting a blank key"
        );
    }

    #[test]
    fn whitespace_only_submit_is_a_cancel() {
        let (mut d, mut rx, _done) = dialog();
        d.handle_paste("     ");
        d.handle_key(&key_id("enter"));
        assert_eq!(drain(&mut rx), Some(KeyDialogOutcome::Cancelled));
    }

    #[test]
    fn escape_cancels() {
        let (mut d, mut rx, done) = dialog();
        d.handle_paste("sk-something");
        d.handle_key(&key_id("escape"));
        assert!(done.load(Ordering::SeqCst));
        assert_eq!(drain(&mut rx), Some(KeyDialogOutcome::Cancelled));
    }

    // --- the (esc to cancel) hint -----------------------------------------

    #[test]
    fn renders_the_cancel_hint() {
        let (d, _rx, _done) = dialog();
        assert!(body_text(&d).contains("(esc to cancel)"));
    }
}
