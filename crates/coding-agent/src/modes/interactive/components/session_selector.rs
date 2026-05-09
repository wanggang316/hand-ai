//! Minimal-viable session selector dialog.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/session-selector.ts`.
//!
//! ## Scope of this port
//!
//! The TS source is ~1023 lines and bundles:
//!
//! - `SessionInfo` listing with sort/scope/name-filter toggles.
//! - Search input with `re:`, `name:`, fuzzy and phrase tokens.
//! - Inline rename (`Ctrl+Y`) and delete (with `trash` shellout) flows.
//! - Bulk loading progress / status messages with auto-hide.
//! - A status header that flickers between modes.
//!
//! Per the worktree brief, the Rust port lands as a **minimal viable
//! subset**: the core list + arrow navigation + `Enter` select + `Esc`
//! cancel. Everything else is tracked as `TODO(parity)`. Drivers that need
//! the full surface should:
//!
//! 1. Filter / sort the [`SessionInfo`] vector with
//!    [`super::session_selector_search::filter_and_sort_sessions`] before
//!    handing it in (the search helpers were ported in an earlier wave).
//! 2. Use the supplied [`SessionSelectorEvent`] channel to coordinate
//!    deletion / rename via the [`crate::core::session_manager::SessionManager`].
//!
//! The TS source uses two-step delete confirmation (first press marks, second
//! press deletes, third press elsewhere clears). That stateful flow is left
//! as a TODO: a delete event simply emits the path and lets the driver
//! show a confirm dialog separately.

use hand_tui::Component;
use hand_tui::keybindings::{Keybinding, get_keybindings};
use hand_tui::tui::{HandleResult, InputEvent};
use hand_tui::utils::{truncate_to_width, visible_width};
use tokio::sync::mpsc;

use super::dynamic_border::DynamicBorderComponent;
use super::keybinding_hints::key_hint_for;
use crate::core::session_manager::SessionInfo;

/// Outcome surfaced via the events channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSelectorEvent {
    /// User confirmed a session — payload is its on-disk path.
    Selected(std::path::PathBuf),
    /// User pressed `tui.select.cancel`.
    Cancelled,
}

const ACCENT: &str = "\x1b[36m";
const MUTED: &str = "\x1b[90m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

/// Minimal viable session-list dialog. The list is read-only — caller
/// pre-filters and pre-sorts the slice.
pub struct SessionSelectorComponent {
    sessions: Vec<SessionInfo>,
    selected_index: usize,
    border: DynamicBorderComponent,
    events: mpsc::UnboundedSender<SessionSelectorEvent>,
    max_visible: usize,
    title: String,
}

impl SessionSelectorComponent {
    /// Construct a new dialog. `sessions` are rendered in the order given.
    pub fn new(
        sessions: Vec<SessionInfo>,
        events: mpsc::UnboundedSender<SessionSelectorEvent>,
    ) -> Self {
        Self {
            sessions,
            selected_index: 0,
            border: DynamicBorderComponent::new(),
            events,
            max_visible: 12,
            title: "Sessions".into(),
        }
    }

    /// Builder: customize the title rendered above the list.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Builder: customize the maximum rows rendered before scrolling kicks
    /// in.
    pub fn with_max_visible(mut self, max_visible: usize) -> Self {
        self.max_visible = max_visible;
        self
    }

    /// Replace the displayed sessions. Resets selection to the top.
    pub fn set_sessions(&mut self, sessions: Vec<SessionInfo>) {
        self.sessions = sessions;
        self.selected_index = 0;
    }

    /// Currently-highlighted session.
    pub fn selected(&self) -> Option<&SessionInfo> {
        self.sessions.get(self.selected_index)
    }

    /// Visible window of sessions, used for both rendering and tests.
    fn visible_window(&self) -> (usize, usize) {
        let total = self.sessions.len();
        if total == 0 || self.max_visible == 0 {
            return (0, 0);
        }
        let visible = self.max_visible.min(total);
        // Keep the selected entry inside the window.
        let mut start = self
            .selected_index
            .saturating_sub(visible.saturating_sub(1));
        if start + visible > total {
            start = total.saturating_sub(visible);
        }
        // Default-anchor: when selection fits without scrolling, start at 0.
        if self.selected_index < visible {
            start = 0;
        }
        (start, (start + visible).min(total))
    }

    fn move_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    fn move_down(&mut self) {
        if self.selected_index + 1 < self.sessions.len() {
            self.selected_index += 1;
        }
    }
}

fn format_row(s: &SessionInfo, selected: bool, width: usize) -> String {
    let cursor = if selected { "▸ " } else { "  " };
    let name = s.name.as_deref().unwrap_or("(unnamed)");
    let id_short = s.id.chars().take(8).collect::<String>();
    let preview_raw = if s.first_message.is_empty() {
        "(no messages)".to_string()
    } else {
        s.first_message.replace('\n', " ")
    };
    let line_plain = format!("{cursor}{name} · {id_short}  {preview_raw}");
    let truncated = truncate_to_width(&line_plain, width.saturating_sub(2));
    if selected {
        format!("{ACCENT}{BOLD}{truncated}{RESET}")
    } else {
        truncated
    }
}

impl Component for SessionSelectorComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let width_us = width as usize;
        let mut out = Vec::new();
        out.extend(self.border.render(width));

        // Title.
        out.push(pad_line(
            &format!("{ACCENT}{BOLD}{}{RESET}", self.title),
            width,
        ));

        // List rows.
        if self.sessions.is_empty() {
            out.push(pad_line(&format!("{MUTED}(no sessions){RESET}"), width));
        } else {
            let (start, end) = self.visible_window();
            for i in start..end {
                let row = format_row(&self.sessions[i], i == self.selected_index, width_us);
                out.push(pad_line(&row, width));
            }
            if self.sessions.len() > self.max_visible {
                out.push(pad_line(
                    &format!(
                        "{MUTED}({}/{} sessions){RESET}",
                        self.selected_index + 1,
                        self.sessions.len()
                    ),
                    width,
                ));
            }
        }

        // Hint.
        let hint = format!(
            "{}  {}  {}",
            key_hint_for("tui.select.up", "↑"),
            key_hint_for("tui.select.down", "↓"),
            key_hint_for("tui.select.confirm", "open"),
        );
        out.push(pad_line(&format!("{MUTED}{hint}{RESET}"), width));

        out.extend(self.border.render(width));
        out
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        let raw = match event {
            InputEvent::Raw(s) => s.clone(),
            _ => return HandleResult::Ignored,
        };
        let kb = get_keybindings();

        if kb.matches(&raw, Keybinding::SelectUp) {
            self.move_up();
            return HandleResult::Handled;
        }
        if kb.matches(&raw, Keybinding::SelectDown) {
            self.move_down();
            return HandleResult::Handled;
        }
        if kb.matches(&raw, Keybinding::SelectConfirm)
            && let Some(s) = self.selected()
        {
            let _ = self
                .events
                .send(SessionSelectorEvent::Selected(s.path.clone()));
            return HandleResult::Handled;
        }
        if kb.matches(&raw, Keybinding::SelectCancel) {
            let _ = self.events.send(SessionSelectorEvent::Cancelled);
            return HandleResult::Handled;
        }

        HandleResult::Ignored
    }
}

fn pad_line(line: &str, width: u16) -> String {
    let target = width as usize;
    let current = visible_width(line);
    if current >= target {
        line.to_string()
    } else {
        format!("{line}{}", " ".repeat(target - current))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn session(id: &str, name: Option<&str>) -> SessionInfo {
        SessionInfo {
            path: PathBuf::from(format!("/tmp/{id}.jsonl")),
            id: id.into(),
            cwd: "/tmp".into(),
            timestamp: 0,
            modified: 0,
            message_count: 1,
            name: name.map(str::to_string),
            parent_session_path: None,
            first_message: format!("hello from {id}"),
            all_messages_text: String::new(),
        }
    }

    fn make(
        sessions: Vec<SessionInfo>,
    ) -> (
        SessionSelectorComponent,
        mpsc::UnboundedReceiver<SessionSelectorEvent>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        (SessionSelectorComponent::new(sessions, tx), rx)
    }

    #[test]
    fn renders_session_names() {
        let (c, _rx) = make(vec![
            session("aaaa1111", Some("alpha")),
            session("bbbb2222", Some("beta")),
        ]);
        let blob = c.render(80).join("\n");
        assert!(blob.contains("alpha"));
        assert!(blob.contains("beta"));
    }

    #[test]
    fn empty_list_renders_placeholder() {
        let (c, _rx) = make(vec![]);
        let blob = c.render(80).join("\n");
        assert!(blob.contains("(no sessions)"));
    }

    #[test]
    fn arrow_keys_move_selection() {
        let (mut c, _rx) = make(vec![
            session("a1", Some("a")),
            session("b1", Some("b")),
            session("c1", Some("c")),
        ]);
        c.handle_input(&InputEvent::Raw("\x1b[B".into()));
        assert_eq!(c.selected().map(|s| s.id.clone()), Some("b1".into()));
        c.handle_input(&InputEvent::Raw("\x1b[B".into()));
        c.handle_input(&InputEvent::Raw("\x1b[A".into()));
        assert_eq!(c.selected().map(|s| s.id.clone()), Some("b1".into()));
    }

    #[test]
    fn enter_emits_selected_event_with_path() {
        let (mut c, mut rx) = make(vec![session("a1", Some("a"))]);
        c.handle_input(&InputEvent::Raw("\r".into()));
        match rx.try_recv() {
            Ok(SessionSelectorEvent::Selected(p)) => {
                assert_eq!(p, PathBuf::from("/tmp/a1.jsonl"));
            }
            other => panic!("expected Selected, got {other:?}"),
        }
    }

    #[test]
    fn escape_emits_cancelled() {
        let (mut c, mut rx) = make(vec![session("a", None)]);
        c.handle_input(&InputEvent::Raw("\x1b".into()));
        match rx.try_recv() {
            Ok(SessionSelectorEvent::Cancelled) => {}
            other => panic!("expected Cancelled, got {other:?}"),
        }
    }
}
