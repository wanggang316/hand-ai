//! One-shot **rt-native** session picker, used by `hand --resume`.
//!
//! This is the driver-*startup* half of resume: before the interactive driver is
//! constructed, `hand --resume` mounts a self-contained session selector, waits for
//! the user's choice, tears the loop down, and returns the selected session path
//! (or `None` for a clean cancel). Unlike the driver-side `/resume` overlay (which
//! runs *inside* the interactive loop and switches the live session in place), this
//! helper owns its own rt session guard + scheduler + input pump and is the sole
//! foreground consumer — it runs during CLI startup, hands its result back to
//! `run_interactive`, and leaves the terminal restored to cooked state with no
//! orphan UI (VAL-CHAT-036).
//!
//! It reuses the [`SessionPicker`](crate::modes::interactive::rt_driver::session_picker::SessionPicker)
//! selector and the overlay runtime unchanged — the same construct-in / channel-out
//! shape the `/resume` overlay uses — so the list rendering, navigation, and
//! Enter/Esc semantics are identical between the two entry points. The only thing
//! this module adds is the standalone rt loop that hosts a selector with no editor
//! or footer beneath it.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use hand_tui::rt::events::{RtInputEvent, spawn_event_pump};
use hand_tui::rt::scheduler::{FrameRequester, FrameScheduler, draw_synchronized};
use hand_tui::rt::session::{EraseOnDrop, SessionError, SessionGuard, SessionTerminal};
use tokio::sync::mpsc;

use crate::core::error::CodingAgentError;
use crate::core::session_manager::SessionInfo;
use crate::modes::interactive::rt_driver::input::{draw_overlay_panel, overlay_interior_width};
use crate::modes::interactive::rt_driver::overlay::{
    self, SelectorController, SharedOverlay, new_done_signal, new_shared_overlay,
};
use crate::modes::interactive::rt_driver::session_picker::{SessionOutcome, SessionPicker};

/// Bound on the rt input event channel for the standalone picker. A small buffer
/// suffices — the picker only consumes arrows / Enter / Esc.
const EVENT_CHANNEL_CAPACITY: usize = 32;

/// Failure modes for [`select_session`].
#[derive(Debug, thiserror::Error)]
pub enum SessionPickerError {
    /// Session listing failed.
    #[error("session listing failed: {0}")]
    Listing(#[from] CodingAgentError),

    /// A terminal-session error from the rt stack (not a TTY, a session already
    /// active, or a raw-mode / escape-write I/O failure).
    #[error("terminal session error: {0}")]
    Session(#[from] SessionError),

    /// A plain I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Show the one-shot rt session picker. Returns the on-disk path of the chosen
/// session, or `None` if the user cancelled (Esc) or the list was empty.
///
/// `sessions` is supplied by the caller (typically
/// [`SessionManager::list`](crate::core::session_manager::SessionManager::list)) so
/// this helper stays decoupled from the loader's exact source. Establishes an rt
/// session guard (raw mode, panic-restore), paints the picker as a centered modal
/// dialog over an otherwise-empty viewport, and converges every exit path — a pick,
/// an Esc cancel, or stdin EOF — on one teardown that drops the scheduler (erasing
/// the viewport) and restores the terminal.
pub async fn select_session(
    sessions: Vec<SessionInfo>,
) -> Result<Option<PathBuf>, SessionPickerError> {
    let mut guard = SessionGuard::enter()?;
    let terminal = guard.terminal()?;
    let result = run_picker(sessions, terminal).await;
    guard.restore();
    result
}

/// The standalone picker loop over a concrete rt terminal.
///
/// Spawns the frame scheduler (the sole painter), the input pump, mounts the
/// [`SessionPicker`], routes keys into it via [`overlay::dispatch_key`], and awaits
/// its single outcome. Split from [`select_session`] so the picker's teardown
/// ordering (drop the requester → drain the final frame → stop the pump) is a single
/// place; the pure selector contract is unit-tested directly against
/// [`SessionPicker`], and the runtime validator exercises the full loop under tmux.
async fn run_picker(
    sessions: Vec<SessionInfo>,
    terminal: SessionTerminal,
) -> Result<Option<PathBuf>, SessionPickerError> {
    let overlays = new_shared_overlay();
    let done = new_done_signal();

    let (requester, scheduler) = spawn_picker_scheduler(terminal, overlays.clone());
    let (mut events, pump) = spawn_event_pump(EVENT_CHANNEL_CAPACITY);

    let (tx, mut rx) = mpsc::unbounded_channel::<SessionOutcome>();
    done.store(false, Ordering::SeqCst);
    let picker = SessionPicker::new(sessions, tx, done.clone());
    let controller: Arc<Mutex<dyn SelectorController>> = Arc::new(Mutex::new(picker));
    overlay::mount(&overlays, &requester, controller, done.clone());
    requester.request_frame();

    let selection = picker_input_loop(&mut events, &overlays, &requester, &mut rx).await;

    // Single teardown: drop the last requester so the scheduler drains its final
    // frame (EraseOnDrop wipes the viewport) and stops; then stop the pump (its
    // shutdown flag ends the bounded-poll loop within one poll interval).
    drop(requester);
    let _ = scheduler.await;
    pump.shutdown();

    Ok(selection)
}

/// The picker's input loop: route each key through the mounted selector until it
/// emits an outcome (Enter/Esc) or the event stream closes (stdin EOF → cancel).
///
/// Returns the selected path, or `None` on cancel / EOF / an empty list. Every key
/// is owned by the modal overlay (there is no editor beneath), and a finished
/// selector closes the dialog; the loop then observes the emitted outcome.
async fn picker_input_loop(
    events: &mut mpsc::Receiver<RtInputEvent>,
    overlays: &SharedOverlay,
    requester: &FrameRequester,
    rx: &mut mpsc::UnboundedReceiver<SessionOutcome>,
) -> Option<PathBuf> {
    loop {
        // A pending outcome (from a prior key) resolves the loop first.
        if let Ok(outcome) = rx.try_recv() {
            return outcome_path(outcome);
        }
        match events.recv().await {
            Some(RtInputEvent::Key(key)) => {
                overlay::dispatch_key(overlays, requester, &key);
                requester.request_frame();
                // The key may have raised the selector's outcome; check it now so
                // Enter/Esc resolve without waiting for another event.
                if let Ok(outcome) = rx.try_recv() {
                    return outcome_path(outcome);
                }
            }
            Some(RtInputEvent::Resize { .. }) => {
                requester.request_frame();
            }
            Some(_) => {}
            // Stdin EOF: the pump dropped its sender. Treat as a clean cancel.
            None => return None,
        }
    }
}

/// Map a picker outcome to the resume path, or `None` on cancel.
fn outcome_path(outcome: SessionOutcome) -> Option<PathBuf> {
    match outcome {
        SessionOutcome::Selected { path, .. } => Some(path),
        SessionOutcome::Cancelled => None,
    }
}

/// Spawn the standalone picker's frame scheduler.
///
/// The draw closure paints only the mounted selector as a full-width, bordered
/// panel anchored to the bottom of the otherwise-empty viewport — reusing the
/// driver's [`draw_overlay_panel`] so the look matches the `/resume` overlay
/// (the M6 panel layout, minus the input box the driver glues it to). Wrapped
/// in [`EraseOnDrop`] so the viewport region is wiped when the scheduler task
/// ends, leaving no orphan panel before the guard restores.
fn spawn_picker_scheduler(
    terminal: SessionTerminal,
    overlays: SharedOverlay,
) -> (FrameRequester, tokio::task::JoinHandle<std::io::Result<()>>) {
    let mut terminal = EraseOnDrop::new(terminal);
    FrameScheduler::spawn(move || {
        let width = {
            let area = terminal.get_frame().area();
            overlay_interior_width(area.width)
        };
        // The standalone CLI picker has no interactive DriverState, so it
        // renders on the default (historical) palette.
        let palette = crate::modes::interactive::theme::ThemePalette::default();
        let overlay_lines = overlays.render_lines(width, &palette);
        let mut stdout = std::io::stdout();
        draw_synchronized(&mut stdout, |_w| {
            terminal.draw(|frame| {
                if let Some(lines) = overlay_lines.clone() {
                    let area = frame.area();
                    draw_overlay_panel(frame.buffer_mut(), area, area.bottom(), lines, &palette);
                }
            })?;
            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(id: &str, name: &str) -> SessionInfo {
        SessionInfo {
            path: PathBuf::from(format!("/tmp/{id}.jsonl")),
            id: id.to_string(),
            cwd: "/tmp".to_string(),
            timestamp: 0,
            modified: 0,
            message_count: 0,
            name: Some(name.to_string()),
            parent_session_path: None,
            first_message: format!("first message for {id}"),
            all_messages_text: String::new(),
        }
    }

    /// The selector contract the standalone loop relies on: Enter emits the
    /// highlighted session's path, and `outcome_path` maps it to `Some(path)`. This
    /// drives the exact key sequence the input loop feeds without standing up a real
    /// terminal (the full rt loop needs a TTY; the runtime validator exercises it
    /// end-to-end under tmux).
    #[tokio::test]
    async fn enter_resolves_to_the_selected_session_path() {
        let (tx, mut rx) = mpsc::unbounded_channel::<SessionOutcome>();
        let done = new_done_signal();
        let sessions = vec![make_session("abc", "first"), make_session("xyz", "second")];
        let mut picker = SessionPicker::new(sessions.clone(), tx, done.clone());

        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use hand_tui::rt::events::RtKey;
        let key = |id: &str| RtKey {
            key_id: Some(id.to_string()),
            raw: KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        };
        // Drive the second row, then Enter — exactly what the input loop does.
        picker.handle_key(&key("down"));
        picker.handle_key(&key("enter"));

        let outcome = rx.recv().await.expect("Enter must emit an outcome");
        assert_eq!(outcome_path(outcome), Some(sessions[1].path.clone()));
    }

    /// Esc resolves to `None` — the clean-cancel path (VAL-CHAT-036).
    #[tokio::test]
    async fn escape_resolves_to_none() {
        let (tx, mut rx) = mpsc::unbounded_channel::<SessionOutcome>();
        let done = new_done_signal();
        let mut picker = SessionPicker::new(vec![make_session("abc", "first")], tx, done.clone());

        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use hand_tui::rt::events::RtKey;
        picker.handle_key(&RtKey {
            key_id: Some("escape".to_string()),
            raw: KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        });

        let outcome = rx.recv().await.expect("Esc must emit Cancelled");
        assert_eq!(outcome_path(outcome), None);
    }

    /// An empty selector has no session to resolve — the loop returns `None` on the
    /// Esc that leaves it (the only exit from an empty picker).
    #[test]
    fn empty_picker_has_no_path_to_resolve() {
        let (tx, _rx) = mpsc::unbounded_channel::<SessionOutcome>();
        let done = new_done_signal();
        let picker = SessionPicker::new(vec![], tx, done);
        assert!(picker.is_empty(), "empty session list");
    }
}
