//! The rt-native `/fork` selector — the fork-from-user-message picker built on the
//! [overlay runtime](super::overlay).
//!
//! It is a [`SelectorController`]: it produces rt-native styled [`Line`]s and
//! consumes keys while it is the mounted modal overlay. Its behaviour is the
//! fork picker's (VAL-OVERLAY-023):
//!
//! - the dialog is titled **"Fork from Message"** and lists the session's past user
//!   messages, one per row, **multi-line text folded to a single line**;
//! - the **most recent** user message is pre-selected on open (a bare Enter forks at
//!   the latest turn);
//! - **↑/↓ navigate with wrap** — Up on the first row wraps to the last, Down on the
//!   last wraps to the first (the per-selector nav nail VAL-OVERLAY-002 pins `/fork`
//!   as *wrap*);
//! - **Enter** confirms the highlighted message's entry id, **Esc** cancels — each
//!   emits exactly one [`ForkOutcome`] on the outcome channel and raises the
//!   [`DoneSignal`](super::overlay::DoneSignal) so the runtime unmounts it.
//!
//! The driver owns the fork itself (`session.fork` + the `[branch]` collapsible
//! summary + the `[forked at: …]` status line) and the no-data degradation (an empty
//! user-message list lands the no-data status, VAL-OVERLAY-019); this component is
//! pure UI + pick logic over its constructor inputs — the reusable
//! construct-in / channel-out selector shape.

use std::sync::atomic::Ordering;

use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::HandleOutcome;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::mpsc;

use super::overlay::{DoneSignal, SelectorController};

/// The most rows shown at once; the window scrolls to keep the selection visible.
const MAX_VISIBLE: usize = 10;

/// The dialog title, pinned by VAL-OVERLAY-023.
pub const TITLE: &str = "Fork from Message";

/// One forkable user message: its stable JSONL entry id and its (single-line-folded)
/// display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkItem {
    /// The JSONL entry id, emitted on Enter and handed to `session.fork`.
    pub entry_id: String,
    /// The user-message text, folded to a single line at render time.
    pub text: String,
}

/// The outcome the selector emits on its channel — exactly one per open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkOutcome {
    /// The user confirmed this entry id (Enter).
    Selected(String),
    /// The user cancelled (Esc) — nothing is forked.
    Cancelled,
}

/// The rt-native `/fork` picker component.
pub struct UserMessageSelector {
    /// The forkable user messages, chronological (oldest first, newest last).
    messages: Vec<ForkItem>,
    /// The highlighted row (index into `messages`).
    selected: usize,
    /// The outcome channel; exactly one [`ForkOutcome`] is sent on confirm/cancel.
    tx: mpsc::UnboundedSender<ForkOutcome>,
    /// Raised on the terminal key (Enter/Esc) so the overlay runtime unmounts this.
    done: DoneSignal,
}

impl UserMessageSelector {
    /// Build a selector over `messages`, pre-selecting the **most recent** (last)
    /// entry so a bare Enter forks at the latest turn (VAL-OVERLAY-023).
    #[must_use]
    pub fn new(
        messages: Vec<ForkItem>,
        tx: mpsc::UnboundedSender<ForkOutcome>,
        done: DoneSignal,
    ) -> Self {
        let selected = messages.len().saturating_sub(1);
        Self {
            messages,
            selected,
            tx,
            done,
        }
    }

    /// The highlighted entry id, if the list is non-empty (test/introspection aid).
    #[must_use]
    pub fn highlighted_id(&self) -> Option<&str> {
        self.messages
            .get(self.selected)
            .map(|m| m.entry_id.as_str())
    }

    /// The highlighted row index (test aid).
    #[must_use]
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Whether the list is empty (the no-data state — the driver never mounts this,
    /// but the guard keeps the component total).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Move the cursor up one row, wrapping to the bottom (`/fork` is a wrap
    /// selector, VAL-OVERLAY-002).
    fn move_up(&mut self) {
        let len = self.messages.len();
        if len == 0 {
            return;
        }
        self.selected = if self.selected == 0 {
            len - 1
        } else {
            self.selected - 1
        };
    }

    /// Move the cursor down one row, wrapping to the top.
    fn move_down(&mut self) {
        let len = self.messages.len();
        if len == 0 {
            return;
        }
        self.selected = (self.selected + 1) % len;
    }

    /// Emit the highlighted entry id (Enter). A no-op when the list is empty.
    fn confirm(&self) -> bool {
        if let Some(item) = self.messages.get(self.selected) {
            let _ = self.tx.send(ForkOutcome::Selected(item.entry_id.clone()));
            true
        } else {
            false
        }
    }

    /// Emit the cancel outcome (Esc) — nothing is forked.
    fn cancel(&self) {
        let _ = self.tx.send(ForkOutcome::Cancelled);
    }

    /// The visible slice `[start, end)`, windowed so the selection stays on screen.
    fn visible_window(&self) -> (usize, usize) {
        let count = self.messages.len();
        if count <= MAX_VISIBLE {
            return (0, count);
        }
        let half = MAX_VISIBLE / 2;
        let start = self
            .selected
            .saturating_sub(half)
            .min(count.saturating_sub(MAX_VISIBLE));
        (start, (start + MAX_VISIBLE).min(count))
    }

    /// The picker body rendered as styled lines (title, subtitle, windowed rows, key
    /// hint), wrapped to `width`.
    fn body_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let muted = Style::default().fg(Color::DarkGray);
        let accent = Style::default().fg(Color::Cyan);

        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(Span::styled(
            TITLE.to_string(),
            accent.add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "Select a user message to branch a new session from that point".to_string(),
            muted,
        )));
        lines.push(Line::from(String::new()));

        let (start, end) = self.visible_window();
        let count = self.messages.len();
        for i in start..end {
            let item = &self.messages[i];
            let folded = fold_single_line(&item.text);
            let is_selected = i == self.selected;

            let mut spans: Vec<Span<'static>> = Vec::new();
            if is_selected {
                spans.push(Span::styled("→ ".to_string(), accent));
                spans.push(Span::styled(folded, accent.add_modifier(Modifier::BOLD)));
            } else {
                spans.push(Span::raw("  ".to_string()));
                spans.push(Span::raw(folded));
            }
            lines.push(Line::from(spans));
            lines.push(Line::from(Span::styled(
                format!("  message {} of {count}", i + 1),
                muted,
            )));
        }

        if end - start < count {
            lines.push(Line::from(Span::styled(
                format!("  ({}/{count})", self.selected + 1),
                muted,
            )));
        }

        lines.push(Line::from(String::new()));
        lines.push(Line::from(Span::styled(
            "↑/↓ navigate   Enter fork   Esc cancel".to_string(),
            muted,
        )));
        lines
    }
}

/// Fold a (possibly multi-line) user message to a single display line: newlines
/// become spaces, runs of whitespace collapse, and the ends are trimmed.
#[must_use]
pub fn fold_single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl SelectorController for UserMessageSelector {
    fn render_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.body_lines(width)
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        match key.key_id.as_deref() {
            Some("up") => {
                self.move_up();
                HandleOutcome::Consumed
            }
            Some("down") => {
                self.move_down();
                HandleOutcome::Consumed
            }
            Some("enter") => {
                if self.confirm() {
                    self.done.store(true, Ordering::SeqCst);
                }
                HandleOutcome::Consumed
            }
            Some("escape") => {
                self.cancel();
                self.done.store(true, Ordering::SeqCst);
                HandleOutcome::Consumed
            }
            // A modal selector owns every key so none reaches the editor beneath
            // (VAL-OVERLAY-005), even keys it does not act on.
            _ => HandleOutcome::Consumed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn item(id: &str, text: &str) -> ForkItem {
        ForkItem {
            entry_id: id.to_string(),
            text: text.to_string(),
        }
    }

    fn key_id(id: &str) -> RtKey {
        RtKey {
            key_id: Some(id.to_string()),
            raw: KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        }
    }

    fn selector(
        messages: Vec<ForkItem>,
    ) -> (
        UserMessageSelector,
        mpsc::UnboundedReceiver<ForkOutcome>,
        DoneSignal,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let done = super::super::overlay::new_done_signal();
        (
            UserMessageSelector::new(messages, tx, done.clone()),
            rx,
            done,
        )
    }

    fn body_text(sel: &UserMessageSelector) -> String {
        sel.body_lines(80)
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

    fn drain(rx: &mut mpsc::UnboundedReceiver<ForkOutcome>) -> Option<ForkOutcome> {
        rx.try_recv().ok()
    }

    // --- title + latest preselect (VAL-OVERLAY-023) ----------------------------

    #[test]
    fn renders_the_fork_title_and_messages() {
        let (sel, _rx, _done) = selector(vec![item("a", "hello"), item("b", "world")]);
        let body = body_text(&sel);
        assert!(body.contains(TITLE), "title missing: {body}");
        assert!(body.contains("hello"), "{body}");
        assert!(body.contains("world"), "{body}");
    }

    #[test]
    fn most_recent_message_is_preselected_and_bare_enter_forks_there() {
        // The last (newest) message is preselected, so a bare Enter forks at it.
        let (mut sel, mut rx, done) = selector(vec![
            item("first", "one"),
            item("second", "two"),
            item("third", "three"),
        ]);
        assert_eq!(sel.highlighted_id(), Some("third"), "latest preselected");
        sel.handle_key(&key_id("enter"));
        assert!(done.load(Ordering::SeqCst), "enter raises the done flag");
        assert_eq!(drain(&mut rx), Some(ForkOutcome::Selected("third".into())));
    }

    // --- multi-line folded to one line -----------------------------------------

    #[test]
    fn multiline_message_is_folded_to_a_single_line() {
        let (sel, _rx, _done) = selector(vec![item("a", "line one\nline two\n\n  line three")]);
        let body = body_text(&sel);
        assert!(
            body.contains("line one line two line three"),
            "multiline must fold to one line: {body}"
        );
    }

    // --- wrap navigation (VAL-OVERLAY-002): Up on first wraps to last -----------

    #[test]
    fn up_on_the_first_row_wraps_to_the_last() {
        let (mut sel, _rx, _done) = selector(vec![item("a", "1"), item("b", "2"), item("c", "3")]);
        // Preselected on the last (c). Down wraps to first (a).
        sel.handle_key(&key_id("down"));
        assert_eq!(
            sel.highlighted_id(),
            Some("a"),
            "down on last wraps to first"
        );
        // Up on the first wraps to the last.
        sel.handle_key(&key_id("up"));
        assert_eq!(sel.highlighted_id(), Some("c"), "up on first wraps to last");
    }

    #[test]
    fn enter_after_navigation_forks_the_highlighted_message() {
        let (mut sel, mut rx, _done) = selector(vec![item("a", "1"), item("b", "2")]);
        // Preselected on b (last). Up → a.
        sel.handle_key(&key_id("up"));
        sel.handle_key(&key_id("enter"));
        assert_eq!(drain(&mut rx), Some(ForkOutcome::Selected("a".into())));
    }

    #[test]
    fn escape_emits_cancelled_and_raises_done() {
        let (mut sel, mut rx, done) = selector(vec![item("a", "1")]);
        sel.handle_key(&key_id("escape"));
        assert!(done.load(Ordering::SeqCst));
        assert_eq!(drain(&mut rx), Some(ForkOutcome::Cancelled));
    }

    #[test]
    fn fold_single_line_collapses_whitespace() {
        assert_eq!(fold_single_line("a\nb"), "a b");
        assert_eq!(
            fold_single_line("  spaced   out \n text "),
            "spaced out text"
        );
        assert_eq!(fold_single_line("single"), "single");
    }
}
