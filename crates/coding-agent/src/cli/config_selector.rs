//! One-shot **rt-native** resource-configuration picker, used by `hand config`.
//!
//! This is the driver-*less* config dialog: `hand config` mounts a self-contained
//! resource selector, lets the user flip checkboxes with immediate visual feedback
//! (VAL-CHAT-037), tears the loop down on Esc / Ctrl+C, and returns the recorded
//! toggles. It owns its own rt session guard + scheduler + input pump and is the sole
//! foreground consumer — the same standalone-rt shape the `--resume`
//! [session picker](super::session_picker) uses.
//!
//! It reuses the
//! [`ConfigSelector`](crate::modes::interactive::rt_driver::config_selector::ConfigSelector)
//! selector and the overlay runtime unchanged (the construct-in / channel-out
//! selector contract), so the list rendering, navigation, and toggle / Esc / Ctrl+C
//! semantics live in one place. The only thing this module adds is the standalone rt
//! loop that hosts the selector as a centered modal dialog over an empty viewport,
//! and it leaves the terminal restored to cooked state with no orphan UI.
//!
//! The YAML write-back path (translating a toggle into a settings edit) is not yet
//! ported (see `core::extensions::source_registry`'s `add_source_to_settings` /
//! `remove_source_from_settings`); the recorded toggles are surfaced for the caller
//! and the checkbox flip is immediate inside the selector, so the user sees feedback.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use hand_tui::rt::events::{RtInputEvent, spawn_event_pump};
use hand_tui::rt::scheduler::{FrameRequester, FrameScheduler, draw_synchronized};
use hand_tui::rt::session::{EraseOnDrop, SessionError, SessionGuard, SessionTerminal};
use tokio::sync::mpsc;

use crate::core::extensions::source_registry::ResolvedPaths;
use crate::modes::interactive::rt_driver::config_selector::{ConfigOutcome, ConfigSelector};
use crate::modes::interactive::rt_driver::input::{draw_overlay, overlay_interior_width};
use crate::modes::interactive::rt_driver::overlay::{
    self, SelectorController, SharedOverlay, new_done_signal, new_shared_overlay,
};

/// Bound on the rt input event channel for the standalone selector. A small buffer
/// suffices — the selector only consumes arrows / space / Esc / Ctrl+C.
const EVENT_CHANNEL_CAPACITY: usize = 32;

/// Outcome of a successful run of [`select_config`]. An inspection surface for tests
/// and callers that want a summary line after the dialog closes.
#[derive(Debug, Default, Clone)]
pub struct ConfigSelectorOutcome {
    /// Each toggle the user issued before dismissing the dialog, in chronological
    /// order. Persistence is best-effort and lives in the driver — see the module
    /// docs.
    pub toggles: Vec<ToggleRecord>,
    /// `true` when the user pressed Ctrl+C (a hard exit) rather than dismissing with
    /// Esc.
    pub aborted: bool,
}

/// One toggle recorded while the dialog was up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToggleRecord {
    pub path: PathBuf,
    pub enabled: bool,
}

/// Failure modes for [`select_config`].
#[derive(Debug, thiserror::Error)]
pub enum ConfigSelectorCliError {
    /// A terminal-session error from the rt stack (not a TTY, a session already
    /// active, or a raw-mode / escape-write I/O failure).
    #[error("terminal session error: {0}")]
    Session(#[from] SessionError),

    /// A plain I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Show the one-shot rt config selector. Returns when the user dismisses the dialog
/// (Esc) or aborts (Ctrl+C). The returned [`ConfigSelectorOutcome`] captures every
/// toggle the user made.
///
/// Establishes an rt session guard (raw mode, panic-restore), paints the selector as
/// a centered modal dialog over an otherwise-empty viewport, and converges every
/// exit path — an Esc dismiss, a Ctrl+C abort, or stdin EOF — on one teardown that
/// drops the scheduler (erasing the viewport) and restores the terminal to cooked
/// state (VAL-CHAT-037).
pub async fn select_config(
    resolved: ResolvedPaths,
) -> Result<ConfigSelectorOutcome, ConfigSelectorCliError> {
    let mut guard = SessionGuard::enter()?;
    let terminal = guard.terminal()?;
    let result = run_selector(resolved, terminal).await;
    guard.restore();
    Ok(result)
}

/// The standalone selector loop over a concrete rt terminal.
///
/// Spawns the frame scheduler (the sole painter) and the input pump, mounts the
/// [`ConfigSelector`], routes keys into it via [`overlay::dispatch_key`], and drains
/// its outcomes until a terminal key (Esc/Ctrl+C) or stdin EOF ends the loop. Split
/// from [`select_config`] so the teardown ordering (drop the requester → drain the
/// final frame → stop the pump) is a single place; the selector contract is
/// unit-tested directly against [`ConfigSelector`], and the runtime validator
/// exercises the full loop under tmux.
async fn run_selector(resolved: ResolvedPaths, terminal: SessionTerminal) -> ConfigSelectorOutcome {
    let overlays = new_shared_overlay();
    let done = new_done_signal();

    let (requester, scheduler) = spawn_selector_scheduler(terminal, overlays.clone());
    let (mut events, pump) = spawn_event_pump(EVENT_CHANNEL_CAPACITY);

    let (tx, mut rx) = mpsc::unbounded_channel::<ConfigOutcome>();
    done.store(false, Ordering::SeqCst);
    let selector = ConfigSelector::new(&resolved, tx, done.clone());
    let controller: Arc<Mutex<dyn SelectorController>> = Arc::new(Mutex::new(selector));
    overlay::mount(&overlays, &requester, controller, done.clone());
    requester.request_frame();

    let outcome = selector_input_loop(&mut events, &overlays, &requester, &mut rx).await;

    // Single teardown: drop the last requester so the scheduler drains its final
    // frame (EraseOnDrop wipes the viewport) and stops; then stop the pump.
    drop(requester);
    let _ = scheduler.await;
    pump.abort();

    outcome
}

/// The selector's input loop: route each key through the mounted selector,
/// accumulating [`ConfigOutcome::Toggled`] records, until a terminal outcome (Esc →
/// dismiss, Ctrl+C → abort) or the event stream closes (stdin EOF → dismiss).
async fn selector_input_loop(
    events: &mut mpsc::Receiver<RtInputEvent>,
    overlays: &SharedOverlay,
    requester: &FrameRequester,
    rx: &mut mpsc::UnboundedReceiver<ConfigOutcome>,
) -> ConfigSelectorOutcome {
    let mut outcome = ConfigSelectorOutcome::default();
    loop {
        // Drain any outcomes raised by a prior key first.
        if let Some(done) = drain_outcomes(rx, &mut outcome) {
            return done;
        }
        match events.recv().await {
            Some(RtInputEvent::Key(key)) => {
                overlay::dispatch_key(overlays, requester, &key);
                requester.request_frame();
                // The key may have raised a toggle or a terminal outcome; drain now so
                // Esc/Ctrl+C resolve without waiting for another event.
                if let Some(done) = drain_outcomes(rx, &mut outcome) {
                    return done;
                }
            }
            Some(RtInputEvent::Resize { .. }) => {
                requester.request_frame();
            }
            Some(_) => {}
            // Stdin EOF: the pump dropped its sender. Treat as a clean dismiss.
            None => return outcome,
        }
    }
}

/// Drain every buffered [`ConfigOutcome`] into `outcome`, returning the finished
/// outcome when a terminal key (Cancel/Exit) was seen, or `None` to keep looping.
fn drain_outcomes(
    rx: &mut mpsc::UnboundedReceiver<ConfigOutcome>,
    outcome: &mut ConfigSelectorOutcome,
) -> Option<ConfigSelectorOutcome> {
    while let Ok(event) = rx.try_recv() {
        match event {
            ConfigOutcome::Toggled { path, enabled, .. } => {
                outcome.toggles.push(ToggleRecord { path, enabled });
            }
            ConfigOutcome::Cancelled => return Some(outcome.clone()),
            ConfigOutcome::Exit => {
                outcome.aborted = true;
                return Some(outcome.clone());
            }
        }
    }
    None
}

/// Spawn the standalone selector's frame scheduler.
///
/// The draw closure paints only the mounted selector as a centered, dimmed, bordered
/// modal dialog over an empty viewport — reusing the driver's [`draw_overlay`] so the
/// placement is pixel-identical to the in-driver overlays. Wrapped in
/// [`EraseOnDrop`] so the viewport region is wiped when the scheduler task ends,
/// leaving no orphan dialog before the guard restores.
fn spawn_selector_scheduler(
    terminal: SessionTerminal,
    overlays: SharedOverlay,
) -> (FrameRequester, tokio::task::JoinHandle<std::io::Result<()>>) {
    let mut terminal = EraseOnDrop::new(terminal);
    FrameScheduler::spawn(move || {
        let width = {
            let area = terminal.get_frame().area();
            overlay_interior_width(area.width)
        };
        // The standalone CLI selector has no interactive DriverState, so it
        // renders on the default (historical) palette.
        let overlay_lines = overlays.render_lines(
            width,
            &crate::modes::interactive::theme::ThemePalette::default(),
        );
        let mut stdout = std::io::stdout();
        draw_synchronized(&mut stdout, |_w| {
            terminal.draw(|frame| {
                if let Some(lines) = overlay_lines.clone() {
                    let area = frame.area();
                    draw_overlay(frame.buffer_mut(), area, lines);
                }
            })?;
            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::interactive::rt_driver::config_selector::ResourceKind;

    /// The outcome accumulation the standalone loop performs: toggles collect in
    /// order and are not treated as terminal, while Cancelled ends the loop. This
    /// drives `drain_outcomes` directly without standing up a real terminal (the full
    /// rt loop needs a TTY; the runtime validator exercises it end-to-end under tmux).
    #[test]
    fn toggles_accumulate_then_cancel_finishes() {
        let (tx, mut rx) = mpsc::unbounded_channel::<ConfigOutcome>();
        tx.send(ConfigOutcome::Toggled {
            path: PathBuf::from("/tmp/a.yaml"),
            kind: ResourceKind::Extensions,
            enabled: true,
        })
        .unwrap();
        tx.send(ConfigOutcome::Toggled {
            path: PathBuf::from("/tmp/b.yaml"),
            kind: ResourceKind::Skills,
            enabled: false,
        })
        .unwrap();
        tx.send(ConfigOutcome::Cancelled).unwrap();

        let mut outcome = ConfigSelectorOutcome::default();
        let done = drain_outcomes(&mut rx, &mut outcome).expect("cancel finishes the loop");
        assert_eq!(done.toggles.len(), 2);
        assert_eq!(done.toggles[0].path, PathBuf::from("/tmp/a.yaml"));
        assert!(done.toggles[0].enabled);
        assert_eq!(done.toggles[1].path, PathBuf::from("/tmp/b.yaml"));
        assert!(!done.toggles[1].enabled);
        assert!(!done.aborted, "Esc is a clean dismiss, not an abort");
    }

    /// Ctrl+C sets `aborted` and finishes the loop.
    #[test]
    fn exit_marks_aborted_and_finishes() {
        let (tx, mut rx) = mpsc::unbounded_channel::<ConfigOutcome>();
        tx.send(ConfigOutcome::Exit).unwrap();
        let mut outcome = ConfigSelectorOutcome::default();
        let done = drain_outcomes(&mut rx, &mut outcome).expect("exit finishes the loop");
        assert!(done.aborted);
    }

    /// With no terminal outcome buffered, the loop keeps going (returns `None`), and
    /// any toggles seen so far are retained in the running outcome.
    #[test]
    fn pending_toggles_do_not_finish_the_loop() {
        let (tx, mut rx) = mpsc::unbounded_channel::<ConfigOutcome>();
        tx.send(ConfigOutcome::Toggled {
            path: PathBuf::from("/tmp/a.yaml"),
            kind: ResourceKind::Prompts,
            enabled: true,
        })
        .unwrap();
        let mut outcome = ConfigSelectorOutcome::default();
        assert!(
            drain_outcomes(&mut rx, &mut outcome).is_none(),
            "a lone toggle does not finish the loop"
        );
        assert_eq!(outcome.toggles.len(), 1, "the toggle is retained");
    }
}
