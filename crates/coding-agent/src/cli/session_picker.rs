//! One-shot TUI session picker, used by `--resume`.
//!
//! Constructs a self-contained [`Tui`], mounts a [`SessionSelectorComponent`]
//! as a centred overlay on top of an empty root, waits for the user's
//! choice on the component's events channel, then tears the loop down and
//! returns the selected session path (or `None` for cancellation).
//!
//! Unlike the driver-side `/resume` overlay, this helper owns its own Tui
//! and is the sole foreground consumer — it is intended to run during
//! CLI startup, before the main interactive driver is constructed.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use hand_tui::{OverlayOptions, ProcessTerminal, Tui};
use tokio::sync::mpsc;

use crate::core::error::CodingAgentError;
use crate::core::session_manager::SessionInfo;
use crate::modes::interactive::components::{SessionSelectorComponent, SessionSelectorEvent};

/// Failure modes for [`select_session`].
#[derive(Debug, thiserror::Error)]
pub enum SessionPickerError {
    #[error("session listing failed: {0}")]
    Listing(#[from] CodingAgentError),

    #[error("tui error: {0}")]
    Tui(#[from] hand_tui::TuiError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("overlay mount failed: {0}")]
    Overlay(#[from] hand_tui::OverlayMountError),

    #[error("run loop join failed")]
    Join,
}

/// Show the TUI session picker. Returns the on-disk path of the chosen
/// session, or `None` if the user cancelled.
///
/// `sessions` is supplied by the caller (typically
/// [`crate::core::session_manager::SessionManager::list`]) so this helper
/// stays decoupled from the loader's exact source.
pub async fn select_session(
    sessions: Vec<SessionInfo>,
) -> Result<Option<PathBuf>, SessionPickerError> {
    let mut tui = Tui::new(Box::new(ProcessTerminal::new()?));
    let mounter = tui.overlay_mounter();
    let running = tui.running_handle();
    let run_handle = tokio::spawn(async move {
        let _ = tui.run().await;
    });
    select_session_inner(sessions, mounter, running, run_handle).await
}

/// Test-friendly variant: accepts a pre-built [`Tui`] event channel so the
/// run loop never reaches `std::io::stdin`.
#[cfg(test)]
async fn select_session_with_events(
    sessions: Vec<SessionInfo>,
    terminal: Box<dyn hand_tui::Terminal>,
    events: mpsc::UnboundedReceiver<hand_tui::StdinBufferEvent>,
) -> Result<Option<PathBuf>, SessionPickerError> {
    let mut tui = Tui::new(terminal);
    let mounter = tui.overlay_mounter();
    let running = tui.running_handle();
    let run_handle = tokio::spawn(async move {
        let _ = tui.run_with_events(events).await;
    });
    select_session_inner(sessions, mounter, running, run_handle).await
}

async fn select_session_inner(
    sessions: Vec<SessionInfo>,
    mounter: hand_tui::OverlayMounter,
    running: Arc<AtomicBool>,
    run_handle: tokio::task::JoinHandle<()>,
) -> Result<Option<PathBuf>, SessionPickerError> {
    let (tx, mut rx) = mpsc::unbounded_channel::<SessionSelectorEvent>();
    let component = SessionSelectorComponent::new(sessions, tx);
    let handle = mounter
        .show(Box::new(component), OverlayOptions::default())
        .await?;

    // Await the user's choice. Cancellation (or EOF — the run loop closes
    // the channel when it stops) maps to `None`.
    let selection = match rx.recv().await {
        Some(SessionSelectorEvent::Selected { path, .. }) => Some(path),
        Some(SessionSelectorEvent::Cancelled) | None => None,
    };

    // Best-effort overlay teardown; errors are non-fatal because we're
    // about to stop the loop anyway.
    let _ = mounter.hide(handle);
    running.store(false, Ordering::Relaxed);
    run_handle.await.map_err(|_| SessionPickerError::Join)?;
    Ok(selection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hand_tui::TestTerminal;

    fn make_session(id: &str, name: &str) -> SessionInfo {
        SessionInfo {
            path: PathBuf::from(format!("/tmp/{id}.session")),
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

    /// Happy path: when the run loop closes (stdin EOF) before any user
    /// action, the picker returns `Ok(None)`. This drives the helper
    /// end-to-end with the test entry point.
    #[tokio::test]
    async fn select_session_returns_none_when_run_loop_closes() {
        let sessions = vec![make_session("abc", "first"), make_session("xyz", "second")];
        let (tx, rx) = mpsc::unbounded_channel::<hand_tui::StdinBufferEvent>();
        // Drop tx — stdin EOF, so the run loop will exit and the picker's
        // outcome channel will close, surfacing `None`.
        drop(tx);

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            select_session_with_events(sessions, Box::new(TestTerminal::new(80, 24)), rx),
        )
        .await
        .expect("helper must finish within 500ms");

        assert!(
            matches!(result, Ok(None)),
            "expected Ok(None), got {result:?}"
        );
    }

    /// Component-level smoke test: the constructor preserves the supplied
    /// sessions in display order with the cursor on the first row, and
    /// pressing Enter pushes a `Selected` event with that session's path.
    /// This guards the contract `select_session_inner` relies on.
    #[tokio::test]
    async fn session_selector_component_emits_selected_on_enter() {
        use hand_tui::Component;
        use hand_tui::tui::InputEvent;

        let sessions = vec![make_session("abc", "first")];
        let (tx, mut rx) = mpsc::unbounded_channel::<SessionSelectorEvent>();
        let mut component = SessionSelectorComponent::new(sessions.clone(), tx);

        // The component dispatches on raw `\r` (the Enter keybinding's
        // default sequence), mirroring how stdin delivers it.
        let _ = component.handle_input(&InputEvent::Raw("\r".into()));

        let event = rx.recv().await.expect("Enter must emit an event");
        match event {
            SessionSelectorEvent::Selected { path: p, .. } => {
                assert_eq!(p, sessions[0].path);
            }
            other => panic!("expected Selected, got {other:?}"),
        }
    }
}
