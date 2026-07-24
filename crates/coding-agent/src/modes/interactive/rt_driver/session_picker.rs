//! The rt-native **session picker** selector — the resume list migrated onto the
//! [overlay runtime](super::overlay).
//!
//! Like the [`ModelSelector`](super::model_selector::ModelSelector) it is a
//! [`SelectorController`]: it produces rt-native styled [`Line`]s and consumes keys
//! while it is the mounted modal overlay. Its behaviour is the resume picker's:
//!
//! - **↑/↓ navigate** the list of resumable sessions (no wrap — a bounded list
//!   reads more naturally with clamped ends);
//! - the highlighted row is cursor-marked and accented; each row shows the session
//!   name (or `(unnamed)`), a short id, and the first-message preview;
//! - an **empty list** shows the `(no sessions)` placeholder and stays open until
//!   the user presses Esc — there is nothing to pick, so Enter is inert
//!   (VAL-OVERLAY-010 / VAL-CHAT-032);
//! - **Enter** confirms the highlighted session, **Esc** cancels — each emits
//!   exactly one [`SessionOutcome`] on the outcome channel and raises the
//!   [`DoneSignal`](super::overlay::DoneSignal) so the runtime unmounts it.
//!
//! The driver owns session listing and the resume itself (`switch_session` +
//! replay); this component is pure UI + pick logic over its constructor inputs —
//! the reusable construct-in / channel-out selector shape shared by the `/resume`
//! overlay and the one-shot `--resume` CLI picker.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::HandleOutcome;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::mpsc;

use crate::core::session_manager::SessionInfo;

use super::keys::NavKeys;
use super::overlay::{DoneSignal, SelectorController};
use crate::modes::interactive::theme::ThemePalette;

/// The most rows of the session list shown at once; the window scrolls to keep the
/// selection visible.
const MAX_VISIBLE: usize = 12;

/// The placeholder shown when no resumable sessions exist. The picker stays open on
/// this state until Esc, matching the legacy selector's `(no sessions)` row.
pub const EMPTY_PLACEHOLDER: &str = "(no sessions)";

/// The outcome the picker emits on its channel — exactly one per open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOutcome {
    /// The user confirmed a session (Enter) — carries its id and on-disk path so
    /// the driver can `switch_session` by path (jsonl) or by id (sqlite).
    Selected { id: String, path: PathBuf },
    /// The user cancelled (Esc) — nothing is resumed.
    Cancelled,
}

/// The rt-native session-resume picker component.
pub struct SessionPicker {
    /// The resumable sessions, in the order the caller supplied (most-recent
    /// first, per [`SessionManager::list`](crate::core::session_manager::SessionManager::list)).
    sessions: Vec<SessionInfo>,
    /// The highlighted row (index into `sessions`).
    selected: usize,
    /// The dialog title rendered above the list.
    title: String,
    /// The outcome channel; exactly one [`SessionOutcome`] is sent on confirm/cancel.
    tx: mpsc::UnboundedSender<SessionOutcome>,
    /// Raised on the terminal key (Enter/Esc) so the overlay runtime unmounts this.
    done: DoneSignal,
    /// The resolved navigation keys, snapshotted from the live app-layer table when
    /// the picker mounted (VAL-OVERLAY-021).
    nav: NavKeys,
}

impl SessionPicker {
    /// Build a picker over `sessions` (rendered in the given order) with the
    /// default navigation keys.
    #[must_use]
    pub fn new(
        sessions: Vec<SessionInfo>,
        tx: mpsc::UnboundedSender<SessionOutcome>,
        done: DoneSignal,
    ) -> Self {
        Self::with_nav(sessions, tx, done, NavKeys::default())
    }

    /// Build a picker with the given resolved navigation keys.
    #[must_use]
    pub fn with_nav(
        sessions: Vec<SessionInfo>,
        tx: mpsc::UnboundedSender<SessionOutcome>,
        done: DoneSignal,
        nav: NavKeys,
    ) -> Self {
        Self {
            sessions,
            selected: 0,
            title: "Resume session".to_string(),
            tx,
            done,
            nav,
        }
    }

    /// The highlighted session, if the list is non-empty.
    #[must_use]
    pub fn highlighted(&self) -> Option<&SessionInfo> {
        self.sessions.get(self.selected)
    }

    /// Whether the list is empty (the `(no sessions)` state).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Move the cursor up one row (clamped at the top).
    fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move the cursor down one row (clamped at the bottom).
    fn move_down(&mut self) {
        if self.selected + 1 < self.sessions.len() {
            self.selected += 1;
        }
    }

    /// Emit the highlighted session as the confirmed outcome (Enter). A no-op when
    /// the list is empty — there is nothing to resume, so the picker stays open.
    fn confirm(&self) -> bool {
        if let Some(session) = self.highlighted() {
            let _ = self.tx.send(SessionOutcome::Selected {
                id: session.id.clone(),
                path: session.path.clone(),
            });
            true
        } else {
            false
        }
    }

    /// Emit the cancel outcome (Esc) — nothing is resumed.
    fn cancel(&self) {
        let _ = self.tx.send(SessionOutcome::Cancelled);
    }

    /// The visible slice `[start, end)` of the list, windowed so the selection
    /// stays on screen.
    fn visible_window(&self) -> (usize, usize) {
        let count = self.sessions.len();
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

    /// The list body rendered as styled lines (the title, the windowed rows or the
    /// empty placeholder, and the key hint), wrapped to `width`.
    fn body_lines(&self, width: u16, palette: &ThemePalette) -> Vec<Line<'static>> {
        let muted = Style::default().fg(palette.dim);
        let accent = Style::default().fg(palette.accent);

        let mut lines: Vec<Line<'static>> = Vec::new();

        // Title.
        lines.push(Line::from(Span::styled(
            self.title.clone(),
            accent.add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(String::new()));

        if self.sessions.is_empty() {
            // The empty state: stays open until Esc (VAL-CHAT-032).
            lines.push(Line::from(Span::styled(
                EMPTY_PLACEHOLDER.to_string(),
                muted,
            )));
        } else {
            let (start, end) = self.visible_window();
            for i in start..end {
                lines.push(session_row(&self.sessions[i], i == self.selected, palette));
            }
            // A position footnote when the list is windowed.
            let count = self.sessions.len();
            if end - start < count {
                lines.push(Line::from(Span::styled(
                    format!("  ({}/{})", self.selected + 1, count),
                    muted,
                )));
            }
        }

        lines.push(Line::from(String::new()));
        lines.push(Line::from(Span::styled(
            self.nav.hint_line("open", "cancel"),
            muted,
        )));

        let _ = width;
        lines
    }
}

/// Render one session row: a cursor mark, the session name, a short id, and the
/// first-message preview. The highlighted row is accent-bold.
fn session_row(session: &SessionInfo, selected: bool, palette: &ThemePalette) -> Line<'static> {
    let accent = Style::default().fg(palette.accent);
    let muted = Style::default().fg(palette.dim);

    let name = session
        .name
        .as_deref()
        .filter(|n| !n.is_empty())
        .unwrap_or("(unnamed)");
    let id_short: String = session.id.chars().take(8).collect();
    let preview = if session.first_message.is_empty() {
        "(no messages)".to_string()
    } else {
        session.first_message.replace('\n', " ")
    };

    if selected {
        Line::from(vec![
            Span::styled("→ ".to_string(), accent),
            Span::styled(name.to_string(), accent.add_modifier(Modifier::BOLD)),
            Span::styled(format!(" · {id_short}  "), muted),
            Span::styled(preview, accent),
        ])
    } else {
        Line::from(vec![
            Span::raw("  ".to_string()),
            Span::raw(name.to_string()),
            Span::styled(format!(" · {id_short}  "), muted),
            Span::styled(preview, muted),
        ])
    }
}

impl SelectorController for SessionPicker {
    fn render_lines(&self, width: u16, palette: &ThemePalette) -> Vec<Line<'static>> {
        self.body_lines(width, palette)
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        // Navigation resolves against the snapshotted app-layer keys, so a user
        // remap drives the picker (VAL-OVERLAY-021).
        let Some(id) = key.key_id.as_deref() else {
            // A modal selector owns even a bare-modifier key so it never reaches the
            // editor beneath (VAL-OVERLAY-005).
            return HandleOutcome::Consumed;
        };
        if self.nav.is_up(id) {
            self.move_up();
        } else if self.nav.is_down(id) {
            self.move_down();
        } else if self.nav.is_confirm(id) {
            // Enter on an empty list is inert: nothing to resume, so the picker
            // stays open and the done flag is not raised (VAL-CHAT-032).
            if self.confirm() {
                self.done.store(true, Ordering::SeqCst);
            }
        } else if self.nav.is_cancel(id) {
            self.cancel();
            self.done.store(true, Ordering::SeqCst);
        }
        // A modal selector owns every key (VAL-OVERLAY-005).
        HandleOutcome::Consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn make_session(id: &str, name: Option<&str>, first: &str) -> SessionInfo {
        SessionInfo {
            path: PathBuf::from(format!("/tmp/{id}.jsonl")),
            id: id.to_string(),
            cwd: "/tmp".to_string(),
            timestamp: 0,
            modified: 0,
            message_count: 1,
            name: name.map(str::to_string),
            parent_session_path: None,
            first_message: first.to_string(),
            all_messages_text: String::new(),
        }
    }

    fn key_id(id: &str) -> RtKey {
        RtKey {
            key_id: Some(id.to_string()),
            raw: KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        }
    }

    fn picker(
        sessions: Vec<SessionInfo>,
    ) -> (
        SessionPicker,
        mpsc::UnboundedReceiver<SessionOutcome>,
        DoneSignal,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let done = super::super::overlay::new_done_signal();
        let picker = SessionPicker::new(sessions, tx, done.clone());
        (picker, rx, done)
    }

    fn body_text(picker: &SessionPicker) -> String {
        picker
            .body_lines(80, &ThemePalette::default())
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

    fn drain(rx: &mut mpsc::UnboundedReceiver<SessionOutcome>) -> Option<SessionOutcome> {
        rx.try_recv().ok()
    }

    // --- list rendering (VAL-OVERLAY-010) --------------------------------

    #[test]
    fn renders_session_names_and_previews() {
        let (p, _rx, _done) = picker(vec![
            make_session("aaaa1111", Some("alpha"), "hello from alpha"),
            make_session("bbbb2222", Some("beta"), "hello from beta"),
        ]);
        let body = body_text(&p);
        assert!(body.contains("alpha"), "{body}");
        assert!(body.contains("beta"), "{body}");
        assert!(body.contains("hello from alpha"), "preview shown: {body}");
        // The short id is present.
        assert!(body.contains("aaaa1111"), "short id: {body}");
    }

    #[test]
    fn unnamed_session_shows_the_unnamed_placeholder() {
        let (p, _rx, _done) = picker(vec![make_session("id1", None, "first msg")]);
        assert!(body_text(&p).contains("(unnamed)"));
    }

    // --- navigation (clamped, no wrap) -----------------------------------

    #[test]
    fn arrows_move_the_selection_clamped_at_the_ends() {
        let (mut p, _rx, _done) = picker(vec![
            make_session("a", Some("a"), "a"),
            make_session("b", Some("b"), "b"),
            make_session("c", Some("c"), "c"),
        ]);
        assert_eq!(p.selected, 0);
        // Up at the top is a clamped no-op (no wrap).
        p.handle_key(&key_id("up"));
        assert_eq!(p.selected, 0, "up at the top clamps");
        // Down moves; a fourth down clamps at the last row.
        for _ in 0..4 {
            p.handle_key(&key_id("down"));
        }
        assert_eq!(p.selected, 2, "down clamps at the bottom");
        assert_eq!(p.highlighted().map(|s| s.id.as_str()), Some("c"));
    }

    // --- Enter / Esc outcomes (VAL-OVERLAY-010) --------------------------

    #[test]
    fn enter_confirms_the_highlighted_session_with_id_and_path() {
        let (mut p, mut rx, done) = picker(vec![
            make_session("first-id", Some("first"), "one"),
            make_session("second-id", Some("second"), "two"),
        ]);
        p.handle_key(&key_id("down"));
        p.handle_key(&key_id("enter"));
        assert!(done.load(Ordering::SeqCst), "enter raises the done flag");
        match drain(&mut rx) {
            Some(SessionOutcome::Selected { id, path }) => {
                assert_eq!(id, "second-id");
                assert_eq!(path, PathBuf::from("/tmp/second-id.jsonl"));
            }
            other => panic!("expected Selected, got {other:?}"),
        }
    }

    #[test]
    fn escape_emits_cancelled_and_raises_done() {
        let (mut p, mut rx, done) = picker(vec![make_session("a", Some("a"), "a")]);
        p.handle_key(&key_id("escape"));
        assert!(done.load(Ordering::SeqCst), "escape raises the done flag");
        assert!(matches!(drain(&mut rx), Some(SessionOutcome::Cancelled)));
    }

    // --- empty state stays open until Esc (VAL-CHAT-032 / VAL-OVERLAY-010) -

    #[test]
    fn empty_list_shows_the_placeholder() {
        let (p, _rx, _done) = picker(vec![]);
        assert!(p.is_empty());
        assert!(
            body_text(&p).contains(EMPTY_PLACEHOLDER),
            "empty state shows the placeholder"
        );
    }

    #[test]
    fn enter_on_an_empty_list_is_inert_and_keeps_the_picker_open() {
        // VAL-CHAT-032: with no sessions, Enter emits nothing and does not raise the
        // done flag — the picker stays open until Esc.
        let (mut p, mut rx, done) = picker(vec![]);
        p.handle_key(&key_id("enter"));
        assert!(
            !done.load(Ordering::SeqCst),
            "enter on an empty list must not close the picker"
        );
        assert!(
            drain(&mut rx).is_none(),
            "no outcome emitted on empty Enter"
        );
    }

    #[test]
    fn escape_on_an_empty_list_cancels() {
        // The only way out of the empty picker is Esc → Cancelled (which the driver
        // turns into the yellow cancel line).
        let (mut p, mut rx, done) = picker(vec![]);
        p.handle_key(&key_id("escape"));
        assert!(done.load(Ordering::SeqCst));
        assert!(matches!(drain(&mut rx), Some(SessionOutcome::Cancelled)));
    }

    #[test]
    fn a_plain_key_is_consumed_but_inert() {
        // A modal selector consumes every key so it never reaches the editor, but a
        // printable key does nothing to the picker (no filter here yet).
        let (mut p, _rx, done) = picker(vec![make_session("a", Some("a"), "a")]);
        let printable = RtKey {
            key_id: Some("x".to_string()),
            raw: KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        };
        assert_eq!(p.handle_key(&printable), HandleOutcome::Consumed);
        assert!(
            !done.load(Ordering::SeqCst),
            "a plain key does not close it"
        );
    }
}
