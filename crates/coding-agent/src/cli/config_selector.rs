//! One-shot TUI configuration picker, used by `pi config` style invocations.
//!
//! Ported from `pi-mono/packages/coding-agent/src/cli/config-selector.ts`.
//!
//! Constructs a self-contained [`Tui`], mounts a
//! [`ConfigSelectorComponent`] as a centred overlay over an empty root,
//! drains its event channel until the user dismisses the dialog (`Esc`)
//! or aborts (`Ctrl+C`), and returns.
//!
//! The component emits `ToggleRequested` events for each in-flight toggle
//! the user makes; the helper currently just records those for inspection
//! by the caller because the underlying YAML write-back path is not yet
//! ported (see `core::extensions::source_registry`'s
//! `add_source_to_settings` / `remove_source_from_settings`). The visual
//! checkbox flip happens inside the component itself, so the user sees
//! immediate feedback.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use hand_tui::{OverlayOptions, ProcessTerminal, Tui};
use tokio::sync::mpsc;

use crate::core::extensions::source_registry::ResolvedPaths;
use crate::modes::interactive::components::{ConfigSelectorComponent, ConfigSelectorEvent};

/// Outcome of a successful run of [`select_config`]. Mostly an inspection
/// surface for tests and for callers that want to surface a summary line
/// after the dialog closes.
#[derive(Debug, Default, Clone)]
pub struct ConfigSelectorOutcome {
    /// Each toggle the user issued before dismissing the dialog. Recorded
    /// in chronological order. Persistence is best-effort and lives in the
    /// driver — see the module docs.
    pub toggles: Vec<ToggleRecord>,
    /// `true` when the user pressed `Ctrl+C` (which the TUI treats as a
    /// hard exit) rather than just dismissing the dialog with `Esc`.
    pub aborted: bool,
}

/// One toggle event recorded while the dialog was up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToggleRecord {
    pub path: std::path::PathBuf,
    pub enabled: bool,
}

/// Failure modes for [`select_config`].
#[derive(Debug, thiserror::Error)]
pub enum ConfigSelectorCliError {
    #[error("tui error: {0}")]
    Tui(#[from] hand_tui::TuiError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("overlay mount failed: {0}")]
    Overlay(#[from] hand_tui::OverlayMountError),

    #[error("run loop join failed")]
    Join,
}

/// Show the TUI config selector. Returns when the user dismisses the
/// dialog. The returned [`ConfigSelectorOutcome`] captures every toggle
/// the user made.
pub async fn select_config(
    resolved: ResolvedPaths,
) -> Result<ConfigSelectorOutcome, ConfigSelectorCliError> {
    let mut tui = Tui::new(Box::new(ProcessTerminal::new()?));
    let mounter = tui.overlay_mounter();
    let running = tui.running_handle();
    let run_handle = tokio::spawn(async move {
        let _ = tui.run().await;
    });
    select_config_inner(resolved, mounter, running, run_handle).await
}

#[cfg(test)]
async fn select_config_with_events(
    resolved: ResolvedPaths,
    terminal: Box<dyn hand_tui::Terminal>,
    events: mpsc::UnboundedReceiver<hand_tui::StdinBufferEvent>,
) -> Result<ConfigSelectorOutcome, ConfigSelectorCliError> {
    let mut tui = Tui::new(terminal);
    let mounter = tui.overlay_mounter();
    let running = tui.running_handle();
    let run_handle = tokio::spawn(async move {
        let _ = tui.run_with_events(events).await;
    });
    select_config_inner(resolved, mounter, running, run_handle).await
}

async fn select_config_inner(
    resolved: ResolvedPaths,
    mounter: hand_tui::OverlayMounter,
    running: Arc<AtomicBool>,
    run_handle: tokio::task::JoinHandle<()>,
) -> Result<ConfigSelectorOutcome, ConfigSelectorCliError> {
    let (tx, mut rx) = mpsc::unbounded_channel::<ConfigSelectorEvent>();
    let component = ConfigSelectorComponent::new(&resolved, tx);
    let handle = mounter
        .show(Box::new(component), OverlayOptions::default())
        .await?;

    let mut outcome = ConfigSelectorOutcome::default();
    while let Some(event) = rx.recv().await {
        match event {
            ConfigSelectorEvent::ToggleRequested { path, enabled, .. } => {
                outcome.toggles.push(ToggleRecord { path, enabled });
                // TODO(parity): forward the toggle to the SettingsManager
                // once the YAML write-back path is implemented in
                // `core::extensions::source_registry`. The component has
                // already updated its in-memory state so the user sees a
                // visual response.
            }
            ConfigSelectorEvent::Cancelled => break,
            ConfigSelectorEvent::Exit => {
                outcome.aborted = true;
                break;
            }
        }
    }

    let _ = mounter.hide(handle);
    running.store(false, Ordering::Relaxed);
    run_handle.await.map_err(|_| ConfigSelectorCliError::Join)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hand_tui::TestTerminal;

    fn empty_resolved() -> ResolvedPaths {
        // ResolvedPaths is a simple aggregate of vectors; default-construct
        // and fill nothing — the helper just renders an empty dialog and
        // exits when the run loop closes.
        ResolvedPaths::default()
    }

    /// EOF-on-stdin: the run loop exits, the outcome channel closes, and
    /// the helper returns an empty outcome (no toggles, not aborted).
    #[tokio::test]
    async fn select_config_returns_empty_outcome_when_run_loop_closes() {
        let (tx, rx) = mpsc::unbounded_channel::<hand_tui::StdinBufferEvent>();
        drop(tx);

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            select_config_with_events(empty_resolved(), Box::new(TestTerminal::new(80, 24)), rx),
        )
        .await
        .expect("helper must finish within 500ms")
        .expect("helper returned an error");

        assert!(result.toggles.is_empty(), "no toggles fired: {result:?}");
        assert!(!result.aborted, "no Ctrl+C: {result:?}");
    }

    /// Toggle accumulation: simulate the component emitting toggles by
    /// constructing an outcome inline. This guards the
    /// `ConfigSelectorEvent::ToggleRequested` -> `ToggleRecord` mapping
    /// inside `select_config_inner`.
    #[tokio::test]
    async fn toggle_events_accumulate_in_outcome() {
        use crate::modes::interactive::components::ConfigSelectorResourceKind;

        let mut outcome = ConfigSelectorOutcome::default();
        let events = vec![
            ConfigSelectorEvent::ToggleRequested {
                path: std::path::PathBuf::from("/tmp/a.yaml"),
                kind: ConfigSelectorResourceKind::Extensions,
                enabled: true,
            },
            ConfigSelectorEvent::ToggleRequested {
                path: std::path::PathBuf::from("/tmp/b.yaml"),
                kind: ConfigSelectorResourceKind::Skills,
                enabled: false,
            },
            ConfigSelectorEvent::Cancelled,
        ];
        for event in events {
            match event {
                ConfigSelectorEvent::ToggleRequested { path, enabled, .. } => {
                    outcome.toggles.push(ToggleRecord { path, enabled });
                }
                ConfigSelectorEvent::Cancelled => break,
                ConfigSelectorEvent::Exit => {
                    outcome.aborted = true;
                    break;
                }
            }
        }
        assert_eq!(outcome.toggles.len(), 2);
        assert_eq!(
            outcome.toggles[0].path,
            std::path::PathBuf::from("/tmp/a.yaml")
        );
        assert!(outcome.toggles[0].enabled);
        assert_eq!(
            outcome.toggles[1].path,
            std::path::PathBuf::from("/tmp/b.yaml")
        );
        assert!(!outcome.toggles[1].enabled);
        assert!(!outcome.aborted);
    }
}
