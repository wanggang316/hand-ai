//! The rt-native **OAuth flow status overlay** — the `/login <oauth-provider>`
//! progress dialog migrated onto the [overlay runtime](super::overlay).
//!
//! Unlike the pick-one selectors this overlay is **driven from the outside**: the
//! driver runs the async OAuth `login()` on the turn runner and streams progress
//! (the authorize URL, device-code prompt, waiting / error notices) into a shared
//! line buffer this component renders. It is a [`SelectorController`] only so it can
//! sit on the same modal-overlay runtime (repaint each frame, capture keys); its
//! sole interactive key is **Esc**, which requests cancellation.
//!
//! # Browser suppression
//!
//! The overlay only ever *shows* the authorize URL as a status line — it never
//! launches a browser. The driver builds the OAuth callbacks so `on_open_url` pushes
//! a line here rather than shelling out to `open`, which is exactly what lets a
//! network-blocked probe exercise the failure path without spawning anything
//! (VAL-OVERLAY-028). A real user Cmd/Ctrl-clicks the printed URL.
//!
//! # Failure keeps the session usable
//!
//! When the flow fails (network blocked, callback timeout), the driver unmounts this
//! overlay and commits a red `[oauth: login failed: …]` line to scrollback; the chat
//! editor is reachable again on the next key (VAL-OVERLAY-028). The overlay itself
//! carries no session state, so nothing leaks on teardown.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::HandleOutcome;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::overlay::{DoneSignal, SelectorController};
use crate::modes::interactive::theme::ThemePalette;

/// A shared, appendable status line buffer the driver writes and the overlay reads.
///
/// The OAuth login callbacks (running on the turn runner) push lines here; the
/// scheduler paints them each frame. A plain blocking `Mutex` — every critical
/// section is a small push / clone.
#[derive(Clone, Default)]
pub struct OAuthStatus {
    lines: Arc<Mutex<Vec<String>>>,
    /// Set when the user presses Esc, so the driver's login future can observe the
    /// cancel and stop waiting.
    cancelled: Arc<AtomicBool>,
}

impl OAuthStatus {
    /// A fresh status buffer seeded with `initial` lines.
    #[must_use]
    pub fn new(initial: Vec<String>) -> Self {
        Self {
            lines: Arc::new(Mutex::new(initial)),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Append a status line and (implicitly, via the driver's repaint) surface it.
    pub fn push(&self, line: impl Into<String>) {
        // The status lines are auxiliary cosmetic state (like the keybindings
        // mutex): recover from a poisoned lock rather than fatally panicking, so
        // a panic elsewhere doesn't cascade into losing the login progress buffer.
        self.lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(line.into());
    }

    /// A snapshot of the current lines.
    #[must_use]
    pub fn snapshot(&self) -> Vec<String> {
        self.lines.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Whether the user asked to cancel (pressed Esc).
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn mark_cancelled(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

/// The rt-native OAuth progress overlay. Renders [`OAuthStatus`] lines and closes on
/// Esc (raising its [`DoneSignal`] and flagging the shared cancel).
pub struct OAuthFlowOverlay {
    title: String,
    status: OAuthStatus,
    done: DoneSignal,
}

impl OAuthFlowOverlay {
    /// Build an overlay titled for `provider_name`, backed by `status`.
    #[must_use]
    pub fn new(provider_name: impl Into<String>, status: OAuthStatus, done: DoneSignal) -> Self {
        Self {
            title: format!("Login to {}", provider_name.into()),
            status,
            done,
        }
    }

    fn body_lines(&self, width: u16, palette: &ThemePalette) -> Vec<Line<'static>> {
        let muted = Style::default().fg(palette.dim);
        let accent = Style::default().fg(palette.accent);

        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(Span::styled(
            self.title.clone(),
            accent.add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(String::new()));
        for status in self.status.snapshot() {
            lines.push(Line::from(status));
        }
        lines.push(Line::from(String::new()));
        lines.push(Line::from(Span::styled(
            "(esc to cancel)".to_string(),
            muted,
        )));

        let _ = width;
        lines
    }
}

impl SelectorController for OAuthFlowOverlay {
    fn render_lines(&self, width: u16, palette: &ThemePalette) -> Vec<Line<'static>> {
        self.body_lines(width, palette)
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        match key.key_id.as_deref() {
            Some("escape") => {
                self.status.mark_cancelled();
                self.done.store(true, Ordering::SeqCst);
                HandleOutcome::Consumed
            }
            // A modal overlay owns every key so it never reaches the editor beneath.
            _ => HandleOutcome::Consumed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn overlay() -> (OAuthFlowOverlay, OAuthStatus, DoneSignal) {
        let status = OAuthStatus::new(vec!["Starting OAuth login…".to_string()]);
        let done = super::super::overlay::new_done_signal();
        let ov = OAuthFlowOverlay::new("Anthropic", status.clone(), done.clone());
        (ov, status, done)
    }

    fn key_id(id: &str) -> RtKey {
        RtKey {
            key_id: Some(id.to_string()),
            raw: KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        }
    }

    fn body_text(ov: &OAuthFlowOverlay) -> String {
        ov.body_lines(80, &ThemePalette::default())
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

    #[test]
    fn renders_pushed_status_lines() {
        let (ov, status, _done) = overlay();
        status.push("Open this URL: https://claude.ai/oauth/authorize?…");
        let body = body_text(&ov);
        assert!(body.contains("Starting OAuth login"), "{body}");
        assert!(body.contains("https://claude.ai/oauth/authorize"), "{body}");
        assert!(body.contains("(esc to cancel)"), "{body}");
    }

    #[test]
    fn escape_flags_cancel_and_raises_done() {
        let (mut ov, status, done) = overlay();
        assert!(!status.is_cancelled());
        ov.handle_key(&key_id("escape"));
        assert!(done.load(Ordering::SeqCst), "esc raises done");
        assert!(status.is_cancelled(), "esc flags the shared cancel");
    }

    #[test]
    fn status_buffer_is_shared_across_clones() {
        let status = OAuthStatus::new(vec![]);
        let clone = status.clone();
        clone.push("device code: ABCD-1234");
        assert_eq!(
            status.snapshot(),
            vec!["device code: ABCD-1234".to_string()]
        );
    }
}
