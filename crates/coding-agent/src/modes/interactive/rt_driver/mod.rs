//! Interactive TUI driver on the ratatui runtime (rt stack).
//!
//! This is the **M3 strangler cutover**: `hand`'s interactive mode now runs on
//! the rt stack (`hand_tui::rt`) — a real terminal session guard, a frame
//! scheduler that owns the terminal, an async input pump, and a fixed-max inline
//! viewport — instead of the legacy `hand_tui::Tui` run loop with its background
//! tokio tasks sharing state through raw pointers.
//!
//! # What this feature is (the skeleton)
//!
//! The run-loop *skeleton*: enough to start on the new stack, type, submit,
//! stream a turn's output into scrollback, and exit cleanly on every path. The
//! two legacy exit hacks are **gone**:
//!
//! - **`std::process::exit`** — the legacy driver hard-exited the process on
//!   quit / slash-quit because a graceful teardown hung on `tokio::io::stdin`'s
//!   uncancellable blocking thread. The rt input pump drives crossterm's
//!   `EventStream` (cancellable), so every exit path now converges on one
//!   teardown that drops the scheduler, restores the terminal, and returns.
//! - **`StopHandle(*const Tui)`** — a `Send`/`Sync` newtype over a raw pointer
//!   used to call `tui.stop()` from a background task. There is no such handle
//!   here: quitting is a plain `break` out of the input loop.
//!
//! Both are replaced by the rt [`SessionGuard`](hand_tui::rt::session::SessionGuard)'s
//! deterministic restore (idempotent across explicit restore / `Drop` / panic
//! hook, and re-armed per session by the single-session guard) plus the
//! [`EraseOnDrop`](hand_tui::rt::session::EraseOnDrop) viewport wipe.
//!
//! # Structure and seams for the follow-up features
//!
//! - [`state`] — the shared, `Send` [`DriverState`](state::DriverState) (size,
//!   input rows, streaming flag, pending scrollback commits) and the shared
//!   editor. Follow-up features add footer view-model fields and selector state
//!   here.
//! - [`input`] — the editor component wiring (M2 [`Editor`](hand_tui::rt::components::Editor),
//!   borderless / no-placeholder hand-chat style) and the bottom-area draw.
//!   *Seam:* slash-command dispatch and `@`-mention autocomplete mount on the
//!   editor here.
//! - [`chat`] — [`ChatUpdate`](super::event_dispatch::ChatUpdate) → scrollback
//!   [`Line`]s. *Seam:* the message components (markdown, thinking, bash, tool
//!   cards) replace the flat text arms here without touching the commit path.
//! - [`watchdog`] — the injectable per-turn timeout (VAL-CHAT-022). *Seam:* a
//!   test or the `stall` mock-provider scenario injects a short ceiling.
//! - The **agent driver task** (below) subscribes to
//!   [`AgentSession`] events through the reused, `hand_tui`-free
//!   [`event_dispatch`](super::event_dispatch) protocol, converts them to
//!   scrollback commits, and runs each turn under the watchdog. *Seam:* turn
//!   control (steering, cancel, follow-up) and full slash/selector dispatch
//!   mount on this task.
//!
//! The chat scrollback / active-area split mirrors the rt demo
//! (`hand_tui`'s `rt_demo` example): finalized output goes to native scrollback
//! via the [`HistorySink`](hand_tui::rt::history::HistorySink), and the live
//! bottom area (editor + footer placeholder) is laid out inside the fixed inline
//! viewport with [`bottom_area_geometry`](hand_tui::rt::view::bottom_area_geometry)
//! `.offset_y(frame.area().y)` (the M1 FIX-2 invariant: the viewport origin
//! drifts down as `insert_before` fills scrollback).

pub mod chat;
pub mod input;
pub mod state;
pub mod watchdog;

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hand_tui::rt::components::{BorderTint, Editor, EditorBorder};
use hand_tui::rt::events::{RtInputEvent, RtKey, spawn_event_pump};
use hand_tui::rt::scheduler::FrameRequester;
use hand_tui::rt::session::{SessionError, SessionGuard, hangup_listener};
use hand_tui::rt::view::{RtComponent, TerminalSize};
use ratatui::text::Line;
use tokio::sync::mpsc;

use crate::core::agent_session::{AgentSession, AgentSessionEvent};
use crate::core::error::CodingAgentError;

use self::state::{DriverState, SharedEditor, SharedFooter, lock_editor, lock_state};
use self::watchdog::Watchdog;
use super::event_dispatch::{ChatUpdate, dispatch as dispatch_event};

/// Bound on the rt input event channel. A small buffer suffices for interactive
/// typing; backpressure just parks the pump, which is fine.
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// Errors raised by the interactive TUI driver.
#[derive(Debug, thiserror::Error)]
pub enum InteractiveError {
    /// An agent-layer error (session build, send failure surfaced to the caller).
    #[error("agent error: {0}")]
    Agent(#[from] CodingAgentError),

    /// A terminal-session error from the rt stack: not a TTY, a session already
    /// active, or a raw-mode / escape-write I/O failure.
    #[error("terminal session error: {0}")]
    Session(#[from] SessionError),

    /// A plain I/O error from the run loop.
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

/// The interactive TUI driver.
///
/// Built with [`InteractiveMode::new`]; [`InteractiveMode::run`] takes over the
/// terminal and runs to a clean exit. A short per-turn watchdog can be injected
/// with [`InteractiveMode::with_watchdog`] (the `stall` mock-provider scenario
/// and the timeout-banner test drive this).
pub struct InteractiveMode {
    session: AgentSession,
    cwd: PathBuf,
    watchdog: Watchdog,
}

impl InteractiveMode {
    /// Build the driver. The per-turn watchdog defaults to 5 minutes, overridable
    /// via the `HAND_TURN_TIMEOUT_MS` env (the VAL-CHAT-022 probe seam).
    pub fn new(session: AgentSession, cwd: PathBuf) -> Self {
        Self {
            session,
            cwd,
            watchdog: Watchdog::from_env_or_default(),
        }
    }

    /// Override the per-turn watchdog ceiling. Used to inject a short timeout so
    /// the timeout-banner path (VAL-CHAT-022) is probable without waiting the
    /// full default.
    #[must_use]
    pub fn with_watchdog(mut self, watchdog: Watchdog) -> Self {
        self.watchdog = watchdog;
        self
    }

    /// Run the interactive TUI to completion on the rt stack.
    ///
    /// Establishes the session guard (raw mode, bracketed paste, kitty flags,
    /// panic-restore), spawns the frame scheduler (which owns the terminal), the
    /// input pump, the SIGHUP listener, and the agent driver task, then runs the
    /// input loop until any clean-exit path fires. Every path — Ctrl+D, a
    /// `/quit` family command, event-stream EOF, or SIGHUP — converges on the
    /// single teardown at the end, which drops the scheduler (erasing the
    /// viewport) and restores the terminal. No `process::exit`, no `StopHandle`.
    pub async fn run(self) -> Result<(), InteractiveError> {
        let InteractiveMode {
            mut session,
            cwd,
            watchdog,
        } = self;

        // Establish the guard first: it verifies stdin/stdout are TTYs and claims
        // the single-session flag *before* toggling raw mode, so a non-interactive
        // or re-entrant launch leaves the shell untouched.
        let mut guard = SessionGuard::enter()?;
        let terminal = guard.terminal()?;

        // Seed the tracked size from the real terminal; resize events overwrite it.
        let (init_cols, init_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let state = Arc::new(Mutex::new(DriverState::new(TerminalSize::new(
            init_cols, init_rows,
        ))));

        // The chat editor: borderless, no placeholder — the hand chat-input style.
        // The driver's own bottom-area geometry supplies the bordered box, so the
        // editor paints text only. History seeds the recall buffer from prior
        // user turns so Up/Down recall survives a resume.
        let editor: SharedEditor = Arc::new(Mutex::new(
            Editor::new()
                .border(EditorBorder::None)
                .with_history(recall_history(&session)),
        ));
        // Footer placeholder — a single status line so the bottom chrome has its
        // shape. The full footer view-model is a follow-up feature.
        let footer: SharedFooter = Arc::new(Mutex::new(footer_line(&session, &cwd)));

        // Bridge agent events into the driver through the reused, hand_tui-free
        // ChatUpdate protocol. The listener forwards raw session events over an
        // unbounded channel to the agent driver task, which dispatches them.
        let (event_tx, event_rx) = mpsc::unbounded_channel::<AgentSessionEvent>();
        let forward = event_tx.clone();
        session.subscribe(move |event| {
            let _ = forward.send(event);
        });

        // Spawn the frame scheduler: it owns the terminal and is the single place
        // the UI is painted, wrapped in synchronized-output markers.
        let (requester, scheduler) =
            input::spawn_scheduler(terminal, state.clone(), editor.clone(), footer.clone());

        // Spawn the rt input pump (crossterm EventStream → RtInputEvent channel).
        let (mut events, pump) = spawn_event_pump(EVENT_CHANNEL_CAPACITY);

        // Register the SIGHUP listener so a closing PTY master takes the same
        // clean-exit path as Ctrl+D instead of terminating the process raw.
        let mut hangup = hangup_listener().map_err(SessionError::Io)?;

        // The channel the input loop uses to hand submitted text to the turn
        // runner. Dropping it on teardown is what tells the runner to stop.
        let (submit_tx, submit_rx) = mpsc::unbounded_channel::<String>();

        // Spawn the turn runner: it owns the session and runs each submitted turn
        // to completion under the watchdog. It does NOT exit the process.
        let turns = tokio::spawn(turn_runner(TurnRunner {
            session,
            cwd: cwd.clone(),
            watchdog,
            state: state.clone(),
            editor: editor.clone(),
            requester: requester.clone(),
            submits: submit_rx,
        }));

        // Spawn the event applier as an INDEPENDENT task. It must not share a
        // `select!` with the turn runner: `send_message` emits events through
        // `event_rx`, so multiplexing the two would cancel an in-flight turn the
        // moment its own event arrived. Kept separate, a turn future is polled to
        // completion while its events render live.
        let applier = tokio::spawn(event_applier(event_rx, state.clone(), requester.clone()));

        // Initial paint.
        requester.request_frame();

        // The input loop. It never draws directly: it mutates shared state / the
        // editor and requests a frame. Every clean-exit path breaks out of it.
        run_input_loop(RunInputArgs {
            events: &mut events,
            hangup: &mut hangup,
            editor: &editor,
            state: &state,
            requester: &requester,
            submit_tx: &submit_tx,
        })
        .await;

        // --- Single teardown for every exit path ---------------------------

        // Drop the submit channel so the turn runner wakes and returns; abort it
        // so a stalled in-flight turn (mid-stream quit) is abandoned without
        // waiting — the terminal restore below does not depend on the turn
        // unwinding. The event applier is likewise aborted.
        drop(submit_tx);
        turns.abort();
        applier.abort();

        // Drop the last requester so the scheduler drains its final frame, closes
        // its synchronized block, and stops; awaiting it releases the terminal
        // (and fires EraseOnDrop) before we restore. This is the interrupt-safe
        // ordering that guarantees a clean line even mid-stream.
        drop(requester);
        let _ = scheduler.await;

        // Stop the input pump (crossterm EventStream — cancellable, unlike the
        // legacy blocking stdin thread that forced the process::exit hack).
        pump.abort();

        // Explicit restore before returning; Drop would also do it (idempotent).
        guard.restore();
        Ok(())
    }
}

/// Arguments for the input loop, grouped to keep the signature readable.
struct RunInputArgs<'a> {
    events: &'a mut mpsc::Receiver<RtInputEvent>,
    hangup: &'a mut hand_tui::rt::session::Hangup,
    editor: &'a SharedEditor,
    state: &'a Arc<Mutex<DriverState>>,
    requester: &'a FrameRequester,
    submit_tx: &'a mpsc::UnboundedSender<String>,
}

/// The interactive input loop.
///
/// Wakes on an input event or a SIGHUP. It resolves the exit paths and the
/// submit path; everything else routes to the editor. It returns when any exit
/// path fires, leaving the caller to run the single teardown.
async fn run_input_loop(args: RunInputArgs<'_>) {
    let RunInputArgs {
        events,
        hangup,
        editor,
        state,
        requester,
        submit_tx,
    } = args;

    loop {
        // Two clean-exit signals converge here besides the explicit quit below:
        //   - the event channel closing (`None`): the pump reached EventStream
        //     EOF (stdin closed) and dropped its sender — the plain EOF exit.
        //   - a SIGHUP: a closing PTY master, routed here so it exits cleanly
        //     with the terminal restored rather than terminating raw.
        let event = tokio::select! {
            maybe = events.recv() => match maybe {
                Some(event) => event,
                None => break,
            },
            _ = hangup.recv() => break,
        };

        match event {
            RtInputEvent::Key(key) => {
                if handle_key(&key, editor, state, submit_tx) == KeyOutcome::Quit {
                    break;
                }
            }
            RtInputEvent::Paste(payload) => {
                lock_editor(editor).insert_paste(&payload);
            }
            RtInputEvent::Resize { cols, rows } => {
                let _ = lock_state(state).size.apply_resize(cols, rows);
            }
            RtInputEvent::FocusGained | RtInputEvent::FocusLost => {}
        }

        requester.request_frame();
    }
}

/// Whether a handled key requested a quit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyOutcome {
    /// Keep running.
    Continue,
    /// Break the input loop and tear down.
    Quit,
}

/// Handle one key event.
///
/// The exit-priority rule (VAL-COMPAT-013): **Ctrl+D always quits**, even with a
/// non-empty buffer — the quit wins over the editor's own `ctrl+d`
/// delete-forward binding, matching the legacy unconditional Ctrl+D listener.
/// Otherwise the key routes to the editor; after dispatch a latched submit is
/// drained and, if it is a `/quit`-family command, quits — every other submit is
/// forwarded to the agent driver.
fn handle_key(
    key: &RtKey,
    editor: &SharedEditor,
    state: &Arc<Mutex<DriverState>>,
    submit_tx: &mpsc::UnboundedSender<String>,
) -> KeyOutcome {
    // Ctrl+D / Ctrl+C: unconditional clean quit. Intercepted BEFORE the editor so
    // Ctrl+D exits rather than deleting a character (exit beats delete-char).
    if matches!(key.key_id.as_deref(), Some("ctrl+d" | "ctrl+c")) {
        return KeyOutcome::Quit;
    }

    // Route the key to the editor, then drain any latched submit.
    let submitted = {
        let mut ed = lock_editor(editor);
        ed.handle_key(key);
        ed.take_submit()
    };

    let Some(text) = submitted else {
        return KeyOutcome::Continue;
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return KeyOutcome::Continue;
    }

    // Skeleton slash handling: the `/quit` family exits; the full slash table is
    // a follow-up feature. Everything else is forwarded to the agent driver.
    if is_quit_command(trimmed) {
        return KeyOutcome::Quit;
    }

    // Mark streaming immediately so the loader/tint show before the first event,
    // then forward the turn. A send failure means the agent task is gone (we are
    // tearing down), which is harmless.
    lock_state(state).streaming = true;
    let _ = submit_tx.send(trimmed.to_string());
    KeyOutcome::Continue
}

/// Whether a submitted line is a `/quit`-family command (`/quit`, `/exit`, `/q`,
/// case-insensitive). The full slash table is a follow-up feature; the skeleton
/// only needs the exit aliases wired.
fn is_quit_command(trimmed: &str) -> bool {
    let Some(rest) = trimmed.strip_prefix('/') else {
        return false;
    };
    // Only the bare command (no args) counts as a quit.
    let name = rest.split_whitespace().next().unwrap_or("");
    matches!(name.to_ascii_lowercase().as_str(), "quit" | "exit" | "q")
}

/// State the turn-runner task owns.
struct TurnRunner {
    session: AgentSession,
    cwd: PathBuf,
    watchdog: Watchdog,
    state: Arc<Mutex<DriverState>>,
    editor: SharedEditor,
    requester: FrameRequester,
    submits: mpsc::UnboundedReceiver<String>,
}

/// The turn-runner task: drains submitted user turns and runs each to
/// completion under the watchdog.
///
/// It is deliberately **separate** from the event applier. Multiplexing a
/// running turn against the agent-event stream in one `select!` is a
/// cancellation trap: `send_message` emits events *through the same channel*, so
/// an arriving event would make the events arm ready and cancel the in-flight
/// turn future — dropping it mid-request before it ever completes. Draining
/// events in their own always-running task means a turn future is polled to
/// completion and never cancelled, while its events still render live.
///
/// The task never touches the process lifecycle. On timeout the watchdog cancels
/// the session token and the banner is committed, leaving the session usable
/// (VAL-CHAT-022).
async fn turn_runner(mut runner: TurnRunner) {
    while let Some(text) = runner.submits.recv().await {
        run_turn(&mut runner, &text).await;
    }
    // Submit channel closed — teardown.
}

/// The event applier: drains agent session events and commits their scrollback
/// representation, always running independently of the turn runner.
///
/// Because it owns only the `Send` state + requester (not the session or
/// editor), it can run as its own task without contending for the turn runner's
/// `&mut session`. It applies events as they stream in, so a turn's output
/// renders live while `send_message` is still in flight in the turn runner.
async fn event_applier(
    mut events: mpsc::UnboundedReceiver<AgentSessionEvent>,
    state: Arc<Mutex<DriverState>>,
    requester: FrameRequester,
) {
    while let Some(event) = events.recv().await {
        apply_event(&state, &requester, &event);
    }
    // Event channel closed: the session (and its listener) is gone.
}

/// Queue a finalized scrollback block and request a repaint. Empty blocks are
/// dropped by [`DriverState::queue_commit`], so a no-content update is silent.
fn commit(state: &Arc<Mutex<DriverState>>, requester: &FrameRequester, lines: Vec<Line<'static>>) {
    if lines.is_empty() {
        return;
    }
    lock_state(state).queue_commit(lines);
    requester.request_frame();
}

/// Run one submitted turn under the watchdog.
async fn run_turn(runner: &mut TurnRunner, text: &str) {
    // Echo the user message into scrollback immediately for input responsiveness.
    commit(
        &runner.state,
        &runner.requester,
        chat::render_update(&ChatUpdate::AppendUser {
            text: text.to_string(),
        })
        .unwrap_or_default(),
    );

    // Streaming: loader on + border tint while the turn runs.
    set_streaming(&runner.state, &runner.editor, &runner.requester, true);

    // Grab the cancel handle before the `&mut` send borrow: `cancel_handle`
    // borrows `&self`, `send_message` borrows `&mut self`, so they cannot overlap.
    let cancel = runner.session.cancel_handle();
    let send = runner.session.send_message(text);
    match run_under_watchdog(runner.watchdog, send, &cancel).await {
        TurnOutcome::Completed => {}
        TurnOutcome::Failed(e) => commit(
            &runner.state,
            &runner.requester,
            chat::error_lines(&format!("send failed: {e}")),
        ),
        TurnOutcome::TimedOut => commit(
            &runner.state,
            &runner.requester,
            chat::error_lines(&runner.watchdog.timeout_banner()),
        ),
    }

    set_streaming(&runner.state, &runner.editor, &runner.requester, false);
    // cwd retained for the follow-up footer view-model refresh.
    let _ = &runner.cwd;
}

/// How a turn ended under the watchdog.
#[derive(Debug)]
enum TurnOutcome {
    /// The turn completed within the ceiling.
    Completed,
    /// The turn returned an error within the ceiling.
    Failed(CodingAgentError),
    /// The turn exceeded the ceiling; the watchdog cancelled it.
    TimedOut,
}

/// Run a turn future under the watchdog ceiling, cancelling the session token on
/// elapse.
///
/// This is the watchdog-integration seam VAL-CHAT-022 probes: on timeout it
/// cancels the shared [`CancellationToken`](hand_agent::CancellationToken) so the
/// agent loop drops its in-flight future, then reports [`TurnOutcome::TimedOut`]
/// so the caller surfaces the banner — while the session (and its refreshed
/// cancel token) stays usable for the next turn. Kept as a free async fn over the
/// future + cancel handle so the timeout / cancel wiring is unit-tested with a
/// `pending()` future and a real token, without the model layer.
async fn run_under_watchdog<F>(
    watchdog: Watchdog,
    turn: F,
    cancel: &Arc<std::sync::Mutex<hand_agent::CancellationToken>>,
) -> TurnOutcome
where
    F: std::future::Future<Output = Result<Vec<model::Message>, CodingAgentError>>,
{
    match tokio::time::timeout(watchdog.turn_timeout(), turn).await {
        Ok(Ok(_)) => TurnOutcome::Completed,
        Ok(Err(e)) => TurnOutcome::Failed(e),
        Err(_) => {
            if let Ok(token) = cancel.lock() {
                token.cancel();
            }
            TurnOutcome::TimedOut
        }
    }
}

/// Apply a single agent session event, committing its scrollback representation.
///
/// The skeleton commits **finalized** blocks into immutable scrollback, so it
/// keys off message *ends* rather than streaming deltas:
///
/// - An assistant message is committed once, on its `MessageEnd`, from the final
///   snapshot — never per streaming delta (which would spam scrollback with every
///   partial). The live in-viewport streaming preview is a follow-up feature; the
///   `ReplaceLastAssistant` deltas the reused `event_dispatch` emits therefore
///   have no scrollback line here (see [`chat::render_update`]).
/// - Tool lifecycle, status, and compaction updates flow through the reused
///   `event_dispatch` → [`chat::render_update`] path; only the ones with a
///   scrollback representation (tool end, status) commit.
/// - Errors take the red-banner route.
fn apply_event(
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
    event: &AgentSessionEvent,
) {
    if let AgentSessionEvent::Error(msg) = event {
        commit(state, requester, chat::error_lines(msg));
        return;
    }

    // Commit a finalized assistant message once, on its end, from the final
    // snapshot — bypassing the streaming `ReplaceLastAssistant` deltas.
    if let AgentSessionEvent::Agent(agent_event) = event
        && let hand_agent::types::AgentEvent::MessageEnd {
            message: model::Message::Assistant(assistant),
        } = agent_event.as_ref()
    {
        let lines = chat::render_update(&ChatUpdate::AppendAssistant {
            message: Box::new(assistant.clone()),
        })
        .unwrap_or_default();
        commit(state, requester, lines);
        return;
    }

    // Everything else (tool lifecycle, status, compaction) flows through the
    // reused dispatch → render path; streaming deltas render nothing here.
    let mut lines = Vec::new();
    for update in dispatch_event(event) {
        if let Some(rendered) = chat::render_update(&update) {
            lines.extend(rendered);
        }
    }
    commit(state, requester, lines);
}

/// Toggle the streaming flag (loader + editor "thinking" tint) and repaint.
fn set_streaming(
    state: &Arc<Mutex<DriverState>>,
    editor: &SharedEditor,
    requester: &FrameRequester,
    on: bool,
) {
    lock_state(state).streaming = on;
    lock_editor(editor).set_tint(if on {
        BorderTint::Thinking
    } else {
        BorderTint::Idle
    });
    requester.request_frame();
}

/// Seed the editor recall history from the session's prior user turns (newest
/// first), so Up/Down recall survives a resume. Capped by the editor itself.
fn recall_history(session: &AgentSession) -> Vec<String> {
    let mut history: Vec<String> = session
        .messages()
        .iter()
        .filter_map(|m| match m {
            model::Message::User(u) => user_text(u),
            _ => None,
        })
        .collect();
    history.reverse(); // newest first for recall.
    history
}

/// Extract the plain text of a user message, if any.
fn user_text(message: &model::UserMessage) -> Option<String> {
    let text = match &message.content {
        model::UserContent::Text(t) => t.clone(),
        model::UserContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                model::UserContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };
    (!text.trim().is_empty()).then_some(text)
}

/// The footer placeholder line: cwd + model, so the bottom chrome has its shape.
/// The full footer view-model (git branch, token usage, context %, thinking
/// level) is a follow-up feature.
fn footer_line(session: &AgentSession, cwd: &std::path::Path) -> String {
    let model = &session.model().id;
    let label = session.label().unwrap_or("");
    let sep = if label.is_empty() { "" } else { " · " };
    format!("{}{sep}{label} · {model}", cwd.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_command_matches_aliases_case_insensitively() {
        for input in ["/quit", "/exit", "/q", "/QUIT", "/Exit", "/Q"] {
            assert!(is_quit_command(input), "{input} should be a quit command");
        }
    }

    #[test]
    fn quit_command_ignores_non_quit_slashes_and_plain_text() {
        for input in ["/help", "/model gpt", "quit", "hello", "/quitter", ""] {
            assert!(!is_quit_command(input), "{input} should not quit");
        }
    }

    #[test]
    fn quit_command_matches_on_the_name_token_ignoring_args() {
        // The command name is the first whitespace-delimited token, matching the
        // legacy `ParsedSlashCommand` parser: `/quit now` dispatches on "quit"
        // and exits, exactly as the legacy slash table does (args ignored).
        assert!(is_quit_command("/quit now"));
        assert!(is_quit_command("/exit  "));
        // But a different name that merely starts with "quit" is not the alias.
        assert!(!is_quit_command("/quitter"));
    }

    /// Ctrl+D quits even with a non-empty buffer: the exit wins over the editor's
    /// `ctrl+d` delete-forward binding (VAL-COMPAT-013). Verified at the
    /// `handle_key` level with a real editor holding text.
    #[test]
    fn ctrl_d_quits_over_delete_char_with_nonempty_buffer() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let editor: SharedEditor = Arc::new(Mutex::new(Editor::new()));
        lock_editor(&editor).set_text("unsent draft");
        let state = Arc::new(Mutex::new(DriverState::new(TerminalSize::new(80, 24))));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();

        let ctrl_d = RtKey {
            key_id: Some("ctrl+d".to_string()),
            raw: KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        };
        let outcome = handle_key(&ctrl_d, &editor, &state, &tx);

        assert_eq!(outcome, KeyOutcome::Quit, "Ctrl+D must quit");
        // The buffer is untouched — the key never reached the editor's
        // delete-forward path.
        assert_eq!(lock_editor(&editor).text(), "unsent draft");
    }

    /// A plain printable key routes to the editor and does not quit.
    #[test]
    fn printable_key_edits_and_continues() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let editor: SharedEditor = Arc::new(Mutex::new(Editor::new()));
        let state = Arc::new(Mutex::new(DriverState::new(TerminalSize::new(80, 24))));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();

        let a = RtKey {
            key_id: Some("a".to_string()),
            raw: KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        };
        let outcome = handle_key(&a, &editor, &state, &tx);

        assert_eq!(outcome, KeyOutcome::Continue);
        assert_eq!(lock_editor(&editor).text(), "a");
    }

    /// Submitting a non-quit line forwards it to the agent driver and marks the
    /// state streaming, without quitting.
    #[test]
    fn enter_submits_text_to_agent_and_marks_streaming() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let editor: SharedEditor = Arc::new(Mutex::new(Editor::new()));
        lock_editor(&editor).set_text("do a thing");
        let state = Arc::new(Mutex::new(DriverState::new(TerminalSize::new(80, 24))));
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        let enter = RtKey {
            key_id: Some("enter".to_string()),
            raw: KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        };
        let outcome = handle_key(&enter, &editor, &state, &tx);

        assert_eq!(outcome, KeyOutcome::Continue);
        assert_eq!(rx.try_recv().ok(), Some("do a thing".to_string()));
        assert!(lock_state(&state).streaming, "submit marks streaming");
    }

    /// The watchdog cancels a stalled turn on elapse and reports `TimedOut`,
    /// leaving the (freshly-cancellable) session token cancelled — the
    /// VAL-CHAT-022 timeout wiring. Driven with a `pending()` future so it never
    /// completes on its own, a near-zero ceiling so the test is instant, and a
    /// real `CancellationToken` so the cancel is observable.
    #[tokio::test]
    async fn watchdog_cancels_and_times_out_a_stalled_turn() {
        use hand_agent::CancellationToken;

        let watchdog = Watchdog::new(std::time::Duration::from_millis(1));
        let cancel = Arc::new(std::sync::Mutex::new(CancellationToken::new()));

        let stalled = std::future::pending::<Result<Vec<model::Message>, CodingAgentError>>();
        let outcome = run_under_watchdog(watchdog, stalled, &cancel).await;

        assert!(
            matches!(outcome, TurnOutcome::TimedOut),
            "a stalled turn must time out, got {outcome:?}"
        );
        assert!(
            cancel.lock().unwrap().is_cancelled(),
            "the watchdog must cancel the session token on timeout"
        );
    }

    /// A turn that completes within the ceiling reports `Completed` and never
    /// cancels the token.
    #[tokio::test]
    async fn watchdog_lets_a_fast_turn_complete_without_cancelling() {
        use hand_agent::CancellationToken;

        let watchdog = Watchdog::new(std::time::Duration::from_secs(300));
        let cancel = Arc::new(std::sync::Mutex::new(CancellationToken::new()));

        let done = std::future::ready(Ok(Vec::<model::Message>::new()));
        let outcome = run_under_watchdog(watchdog, done, &cancel).await;

        assert!(matches!(outcome, TurnOutcome::Completed), "got {outcome:?}");
        assert!(
            !cancel.lock().unwrap().is_cancelled(),
            "a completed turn must not cancel the token"
        );
    }

    /// A `/quit` submit quits and never forwards to the agent.
    #[test]
    fn slash_quit_submit_quits_without_forwarding() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let editor: SharedEditor = Arc::new(Mutex::new(Editor::new()));
        lock_editor(&editor).set_text("/quit");
        let state = Arc::new(Mutex::new(DriverState::new(TerminalSize::new(80, 24))));
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        let enter = RtKey {
            key_id: Some("enter".to_string()),
            raw: KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        };
        let outcome = handle_key(&enter, &editor, &state, &tx);

        assert_eq!(outcome, KeyOutcome::Quit);
        assert!(rx.try_recv().is_err(), "quit must not forward a turn");
    }
}
