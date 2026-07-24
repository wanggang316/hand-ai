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
//!   input rows, streaming flag, running usage accumulator, pending scrollback
//!   commits) and the shared editor + footer view-model. Follow-up features add
//!   selector state here.
//! - [`footer`] — the [`FooterViewModel`](footer::FooterViewModel) and its pure
//!   two-line renderer, rebuilt from session state after each turn.
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
//! bottom area (editor + two-line footer) is laid out inside the fixed inline
//! viewport with [`bottom_area_geometry`](hand_tui::rt::view::bottom_area_geometry)
//! `.offset_y(frame.area().y)` (the M1 FIX-2 invariant: the viewport origin
//! drifts down as `insert_before` fills scrollback).

pub mod bash;
pub mod chat;
pub mod chrome;
pub mod footer;
pub mod input;
pub mod login;
pub mod login_dialog;
pub mod login_provider_picker;
pub mod messages;
pub mod model_selector;
pub mod oauth_flow;
pub mod overlay;
pub mod replay;
pub mod scoped_models_selector;
pub mod selectors;
pub mod session_picker;
pub mod settings_selector;
pub mod slash;
pub mod state;
pub mod summary;
pub mod theme_selector;
pub mod thinking_selector;
pub mod tools;
pub mod tree_selector;
pub mod user_message_selector;
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

use self::chrome::{ChangelogStartupAction, ProgressState, PromptMark};
use self::footer::build_footer_view;
use self::overlay::{DoneSignal, SharedOverlay, new_done_signal, new_shared_overlay};
use self::state::{DriverState, SharedEditor, SharedFooter, lock_editor, lock_footer, lock_state};
use self::watchdog::Watchdog;
use super::event_dispatch::{ChatUpdate, dispatch as dispatch_event};

/// The status line committed to scrollback after a Ctrl+T thinking toggle, so a
/// probe can confirm the flip and its resulting global state.
fn thinking_status_line(hidden: bool) -> String {
    let state = if hidden { "hidden" } else { "visible" };
    format!("[thinking blocks: {state}]")
}

/// Bound on the rt input event channel. A small buffer suffices for interactive
/// typing; backpressure just parks the pump, which is fine.
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// Queue the resumed session's stored transcript as ordered scrollback blocks, so a
/// resume (`--continue` / `--resume` / `--fork`, seeding messages from disk) shows
/// its prior conversation below the startup chrome before the first frame paints.
///
/// A fresh session (no messages) is a no-op — nothing is queued, so a brand-new
/// session starts on a clean transcript with no spurious `[resumed: …]` marker. The
/// replayed assistant messages are also seeded into the assistant-history so a later
/// global Ctrl+T re-render includes them.
fn queue_startup_replay(session: &AgentSession, state: &Arc<Mutex<DriverState>>) {
    let messages = session.messages();
    if messages.is_empty() {
        return;
    }
    let label = session
        .label()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| session.session_id().chars().take(8).collect());

    let mut guard = lock_state(state);
    let width = guard.size.cols;
    let hide_thinking = guard.hide_thinking;
    for block in self::replay::replay_blocks(messages, &label, hide_thinking, width) {
        guard.queue_commit(block);
    }
    for message in messages {
        if let model::Message::Assistant(a) = message {
            guard.remember_assistant(a.clone());
        }
    }
}

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
        // Footer view-model — built from session state so every field (cwd, git
        // branch, model id/provider, thinking level, context %, usage) is visible
        // at startup, before any turn. Rebuilt after each turn by the turn runner
        // so usage, branch, and thinking-level changes surface.
        let footer: SharedFooter = Arc::new(Mutex::new(build_footer_view(
            &session,
            &cwd,
            self::footer::TokenUsageSummary::default(),
        )));

        // Startup chrome: welcome header, any tmux keyboard warning, and the
        // changelog banner, committed to the top of scrollback BEFORE the first
        // frame paints. The changelog decision reads (and may write) settings, so
        // it runs while we still hold `&mut session`, before it moves into the
        // turn runner.
        let startup = collect_startup_chrome(&mut session);
        for block in startup {
            lock_state(&state).queue_commit(block);
        }

        // Startup replay: when the session was resumed (`--continue` / `--resume` /
        // `--fork` seed the transcript from disk), render its stored user /
        // assistant / tool-result messages into scrollback *in order*, closed by the
        // `[resumed: <label>]` marker — so the resumed conversation reads as one
        // continuous transcript below the chrome, and a stored `stop_reason=Error`
        // assistant surfaces its red error footnote live (VAL-CHAT-012 /
        // VAL-CHAT-029). A fresh session has no messages, so this is a no-op.
        queue_startup_replay(&session, &state);

        // Bridge agent events into the driver through the reused, hand_tui-free
        // ChatUpdate protocol. The listener forwards raw session events over an
        // unbounded channel to the agent driver task, which dispatches them.
        let (event_tx, event_rx) = mpsc::unbounded_channel::<AgentSessionEvent>();
        let forward = event_tx.clone();
        session.subscribe(move |event| {
            let _ = forward.send(event);
        });

        // The overlay runtime: a shared stack the scheduler renders over the base
        // viewport and the input loop routes keys into (modal capture), plus a
        // single shared "top overlay finished" flag every selector raises on its
        // terminal key. This is the reusable substrate for the selector family; the
        // /model selector is the first mounted on it.
        let overlays: SharedOverlay = new_shared_overlay();
        let overlay_done: DoneSignal = new_done_signal();

        // Spawn the frame scheduler: it owns the terminal and is the single place
        // the UI is painted, wrapped in synchronized-output markers.
        let (requester, scheduler) = input::spawn_scheduler(
            terminal,
            state.clone(),
            editor.clone(),
            footer.clone(),
            overlays.clone(),
        );

        // Spawn the rt input pump (crossterm EventStream → RtInputEvent channel).
        let (mut events, pump) = spawn_event_pump(EVENT_CHANNEL_CAPACITY);

        // Register the SIGHUP listener so a closing PTY master takes the same
        // clean-exit path as Ctrl+D instead of terminating the process raw.
        let mut hangup = hangup_listener().map_err(SessionError::Io)?;

        // The channel the input loop uses to hand submitted text to the turn
        // runner. Dropping it on teardown is what tells the runner to stop.
        // Unbounded so a submit *during* an in-flight turn is queued, not dropped:
        // the turn runner drains it one turn at a time, so follow-up messages
        // typed mid-turn are processed in order once the current turn ends
        // (VAL-CHAT-015).
        let (submit_tx, submit_rx) = mpsc::unbounded_channel::<String>();

        // The cancel handle: a shared handle to the session's cancellation token.
        // Grabbed here, *before* the session moves into the turn runner, so the
        // input loop can cancel an in-flight turn (Esc / Ctrl+C) from outside the
        // task that owns `&mut session`. `send_message` swaps the inner token per
        // turn but keeps this same `Arc<Mutex<…>>`, so a cancel through this handle
        // always hits the current turn's token (VAL-CHAT-013 / VAL-CHAT-014).
        let cancel = session.cancel_handle();

        // First-run onboarding gate (VAL-OVERLAY-022): with no provider credential on
        // file (stored or via env var), greet the user and auto-open the login
        // picker. Decided here, *before* the session moves into the turn runner
        // (which owns `&mut session`); the welcome banner is committed now so it
        // lands above the picker, and a `/login` submit is queued below so the turn
        // runner opens the overlay.
        let needs_onboarding = !login::any_provider_has_credentials(&session);
        if needs_onboarding {
            lock_state(&state).queue_commit(chat::status_lines_for(login::WELCOME_NO_CREDENTIALS));
        }

        // Spawn the turn runner: it owns the session and runs each submitted turn
        // to completion under the watchdog. It does NOT exit the process.
        let turns = tokio::spawn(turn_runner(TurnRunner {
            session,
            cwd: cwd.clone(),
            watchdog,
            state: state.clone(),
            editor: editor.clone(),
            footer: footer.clone(),
            requester: requester.clone(),
            submits: submit_rx,
            overlays: overlays.clone(),
            overlay_done: overlay_done.clone(),
        }));

        // Kick off first-run onboarding: queue a `/login` submit the turn runner
        // drains, which opens the provider picker on the overlay runtime. Sent after
        // the runner is spawned so the channel is live; the welcome banner committed
        // above renders first.
        if needs_onboarding {
            let _ = submit_tx.send("/login".to_string());
        }

        // Spawn the event applier as an INDEPENDENT task. It must not share a
        // `select!` with the turn runner: `send_message` emits events through
        // `event_rx`, so multiplexing the two would cancel an in-flight turn the
        // moment its own event arrived. Kept separate, a turn future is polled to
        // completion while its events render live.
        let applier = tokio::spawn(event_applier(event_rx, state.clone(), requester.clone()));

        // Best-effort async version probe: when crates.io reports a newer
        // published version, commit an "update available" banner. Offline
        // (`HAND_OFFLINE`) the fetcher returns `None` so the banner never appears,
        // and the probe never blocks startup — it runs in its own task and the
        // run loop paints immediately below regardless of when it finishes.
        let version_task = tokio::spawn(version_probe(state.clone(), requester.clone()));

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
            cancel: &cancel,
            overlays: &overlays,
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
        version_task.abort();

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
    /// Shared handle to the in-flight turn's cancellation token, used by Esc /
    /// Ctrl+C to abort a streaming turn from outside the turn runner.
    cancel: &'a Arc<Mutex<hand_agent::CancellationToken>>,
    /// The shared overlay: a modal selector, when mounted, captures every key before
    /// it can reach the editor or the turn-control paths below. The mounted selector
    /// carries its own done flag, so the input loop needs only the overlay here.
    overlays: &'a SharedOverlay,
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
        cancel,
        overlays,
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
                // A mounted modal selector owns every key first (VAL-OVERLAY-005):
                // route it through the overlay, and if one was open it consumes the
                // key — the editor, the global toggles, and the turn-control paths
                // below never see it. The dialog closes itself once the selector
                // raises its done flag on Enter/Esc.
                if overlay::dispatch_key(overlays, requester, &key) {
                    requester.request_frame();
                    continue;
                }
                // Ctrl+T is a global toggle: it flips thinking-visibility across
                // every assistant message in the transcript, so it is handled here
                // (where the requester is in scope) rather than in the editor path.
                if key.key_id.as_deref() == Some("ctrl+t") {
                    toggle_thinking_globally(state, requester);
                    requester.request_frame();
                    continue;
                }
                // Ctrl+R expands / collapses the most-recent collapsible summary
                // (compaction / branch / skill). Scrollback is immutable, so the
                // toggle re-commits the summary in its new state — this is what
                // makes the collapsed `(ctrl+r to expand)` hint *real*. Handled
                // here because the summaries live on the shared state, not the
                // session. A silent no-op when no summary has landed.
                if key.key_id.as_deref() == Some("ctrl+r") {
                    toggle_last_summary(state, requester);
                    requester.request_frame();
                    continue;
                }
                // Ctrl+X copies the last assistant message — the keyboard twin of
                // `/copy`. The copy needs the session (which the turn runner owns),
                // so it is forwarded as a `/copy` submit through the same channel
                // the turn runner drains; both paths hit the identical handler, so
                // Ctrl+X and `/copy` behave the same (VAL-CHAT-023).
                if key.key_id.as_deref() == Some("ctrl+x") {
                    let _ = submit_tx.send("/copy".to_string());
                    requester.request_frame();
                    continue;
                }
                // Turn control (VAL-CHAT-013 / VAL-CHAT-014). Esc and Ctrl+C both
                // cancel an in-flight turn; both are inert when idle. In raw mode
                // Ctrl+C is an ordinary key (SIGINT is swallowed), so the driver —
                // not the OS — decides its meaning: cancel a turn, or a visible
                // no-op. It must never exit (only Ctrl+D quits), so the terminal is
                // never left raw.
                match key.key_id.as_deref() {
                    Some("escape") => {
                        // Cancel a streaming turn; otherwise fall through so the
                        // editor can use Esc (autocomplete dismiss, etc).
                        if try_cancel_turn(state, requester, cancel, CancelSource::Esc) {
                            continue;
                        }
                    }
                    Some("ctrl+c") => {
                        // Cancel a streaming turn; idle it is a visible no-op — the
                        // app stays alive, no cancel line lands, the terminal is
                        // untouched. Either way, consume it (never reaches the
                        // editor, never quits).
                        try_cancel_turn(state, requester, cancel, CancelSource::CtrlC);
                        continue;
                    }
                    _ => {}
                }
                if handle_key(&key, editor, state, submit_tx) == KeyOutcome::Quit {
                    break;
                }
            }
            RtInputEvent::Paste(payload) => {
                // A mounted selector with a text field (the login key dialog) owns a
                // paste first: the whole payload lands in its input in one shot
                // (VAL-OVERLAY-027), and the editor beneath never sees it. Only when
                // no overlay is open does the paste reach the chat editor.
                if overlay::dispatch_paste(overlays, requester, &payload) {
                    requester.request_frame();
                    continue;
                }
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
/// Ctrl+C is *not* an exit here: it is turn control (cancel / no-op), resolved by
/// the input loop before this function is reached, so it never quits and the
/// terminal is never left raw (VAL-CHAT-014). Otherwise the key routes to the
/// editor; after dispatch a latched submit is drained and, if it is a
/// `/quit`-family command, quits — every other submit is forwarded to the agent
/// driver.
fn handle_key(
    key: &RtKey,
    editor: &SharedEditor,
    state: &Arc<Mutex<DriverState>>,
    submit_tx: &mpsc::UnboundedSender<String>,
) -> KeyOutcome {
    // Ctrl+D: unconditional clean quit. Intercepted BEFORE the editor so Ctrl+D
    // exits rather than deleting a character (exit beats delete-char). Ctrl+C is
    // handled by the input loop (cancel / no-op) and never reaches here.
    if key.key_id.as_deref() == Some("ctrl+d") {
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

/// Which key requested a turn cancellation, so the yellow status line names the
/// source the user pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancelSource {
    /// The Escape key.
    Esc,
    /// Ctrl+C (an ordinary key in raw mode, not a SIGINT).
    CtrlC,
}

impl CancelSource {
    /// The yellow status line committed to scrollback on cancel, naming the key.
    fn cancel_line(self) -> &'static str {
        match self {
            CancelSource::Esc => "[cancelled by Esc]",
            CancelSource::CtrlC => "[cancelled by ^C]",
        }
    }
}

/// Cancel the in-flight turn if one is streaming, and report whether it did.
///
/// This is the shared Esc / Ctrl+C turn-control path (VAL-CHAT-013 /
/// VAL-CHAT-014). With a turn in flight it: cancels the session token so the
/// agent loop drops its request at the next await point, clears the loader and
/// latches the cancel flag in one step (so the turn runner suppresses the
/// cancelled turn's error banner), commits the yellow `[cancelled …]` line, and
/// repaints — the user sees the loader vanish and the cancel line land at once.
///
/// Idle (no loader / no streaming turn) it does nothing and returns `false`, so
/// the caller can fall through: Esc reaches the editor, Ctrl+C is a visible
/// no-op. Cancellation is committed exactly once even if the key repeats, because
/// clearing `streaming` immediately makes a second press see no in-flight turn.
fn try_cancel_turn(
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
    cancel: &Arc<Mutex<hand_agent::CancellationToken>>,
    source: CancelSource,
) -> bool {
    if !cancel_in_flight_turn(state, cancel) {
        return false;
    }
    commit(
        state,
        requester,
        chat::status_lines_for(source.cancel_line()),
    );
    true
}

/// The side-effecting core of [`try_cancel_turn`], minus the scrollback commit
/// and repaint: mutate the shared state and cancel the session token, reporting
/// whether a turn was actually in flight.
///
/// Kept as a `FrameRequester`-free function so the cancel decision — idle is a
/// no-op, streaming clears the loader, latches the cancel flag, and cancels the
/// token exactly once — is unit-tested with a real `DriverState` and a real
/// `CancellationToken`, without a running scheduler.
fn cancel_in_flight_turn(
    state: &Arc<Mutex<DriverState>>,
    cancel: &Arc<Mutex<hand_agent::CancellationToken>>,
) -> bool {
    {
        let mut guard = lock_state(state);
        if !guard.is_streaming() {
            return false;
        }
        // Clear the loader + latch the cancel flag while holding the lock so a
        // rapid second Esc/Ctrl+C can't race a duplicate cancel line.
        guard.mark_cancelled();
    }
    // Cancel the session token so the in-flight `send_message` unwinds. The token
    // is behind its own Mutex on the session; a poisoned lock just means the turn
    // already tore down, so a failed lock is harmless here.
    if let Ok(token) = cancel.lock() {
        token.cancel();
    }
    true
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
    footer: SharedFooter,
    requester: FrameRequester,
    submits: mpsc::UnboundedReceiver<String>,
    /// The shared overlay stack the runner mounts a selector on (e.g. `/model`).
    /// Shared with the input loop, which routes keys into the mounted selector
    /// while the runner awaits its outcome.
    overlays: SharedOverlay,
    /// The shared "top overlay finished" flag handed to a mounted selector so the
    /// input loop can close the dialog once the selector emits its outcome.
    overlay_done: DoneSignal,
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
///
/// The turn is bracketed with a single, balanced OSC 133 A/B/C sequence
/// (VAL-CHAT-017 / VAL-CHAT-034): `A` (prompt start) before the user echo, `B`
/// (command start) after it, and `C` (command end) once the whole assistant
/// response — including any interleaved tool calls — has landed. The count stays
/// balanced regardless of how many tool calls the turn made, because the marks
/// live here on the turn boundary, not in the per-event apply path. OSC 9;4
/// progress goes indeterminate while the turn runs and clears (or errors) at the
/// end (VAL-CHAT-018).
async fn run_turn(runner: &mut TurnRunner, text: &str) {
    // Inline `!cmd` bash runs locally instead of going to the model: it is
    // executed through `session.run_bash`, its output committed as a bash box,
    // and the footer refreshed (a `!cd`-style change reflects on the next turn).
    // It never touches the OSC-133 turn brackets or the assistant-message path.
    if let Some(parsed) = bash::parse_inline_bash(text) {
        run_bash_inline(runner, parsed).await;
        return;
    }

    // `/compact` runs the async summarizer, so it is intercepted *before* the
    // sync slash dispatch: it shows the compaction loader, calls
    // `session.compact()` / `compact_with()` (which emits the
    // `[Compacting context...]` / `[Compaction complete]` status lines through
    // the event applier), and commits a collapsible compaction summary. It owns
    // the streaming loader for its duration, so it does not clear it up front.
    if let Some(steer) = slash::parse_compact(text) {
        run_compact(runner, steer).await;
        return;
    }

    // `/model` (bare) opens the model selector overlay, which awaits the user's
    // pick, so it is intercepted here on the async turn runner — the one task that
    // owns `&mut session` and can apply the switch. Like `/compact`, it runs
    // *before* the sync slash dispatch. `/model <pattern>` still routes through the
    // sync dispatch (it is a non-interactive switch, not an overlay).
    if slash::is_open_model_selector(text) {
        run_model_selector(runner).await;
        return;
    }

    // `/resume` opens the session picker overlay, which awaits the user's pick and
    // then switches + replays, so it is intercepted here on the async turn runner
    // (the one task that owns `&mut session`), *before* the sync slash dispatch —
    // like `/model`.
    if slash::is_open_resume_picker(text) {
        run_resume_picker(runner).await;
        return;
    }

    // The config-selector family (`/thinking`, `/theme`, `/settings`, and
    // `/model <pattern>`) is intercepted here for the same reason as `/model`: the
    // bare forms mount a modal overlay and await the pick, and the direct-arg forms
    // apply against `&mut session`. Both run on the turn runner, *before* the sync
    // slash dispatch.
    if let Some(action) = slash::config_selector_action(text) {
        run_config_selector(runner, action).await;
        return;
    }

    // The picker-selector family (`/tree`, `/scoped-models`, `/fork`) mounts a modal
    // overlay and awaits the pick, so it is intercepted here on the async turn runner
    // (the one task that owns `&mut session`), *before* the sync slash dispatch — the
    // same reason as `/model` / `/resume` / the config family.
    if let Some(action) = slash::picker_selector_action(text) {
        run_picker_selector(runner, action).await;
        return;
    }

    // The login family (`/login`, `/logout`) mounts a modal overlay (provider picker
    // → key dialog, or the OAuth flow) and awaits, or clears credentials, so it is
    // intercepted here on the async turn runner (the one task that owns
    // `&mut session`), *before* the sync slash dispatch — the same reason as
    // `/model` / `/resume` / the config + picker families.
    if let Some(action) = slash::login_action(text) {
        run_login(runner, action).await;
        return;
    }

    // Slash commands are intercepted here — the turn runner is the one place
    // that owns `&mut session`, so the session-lifecycle commands (`/new`,
    // `/clone`, `/import`, `/name`) run against it directly. The `/quit` family
    // never reaches the runner (the input loop breaks on it), and a slash
    // command never touches the model, the OSC-133 turn brackets, or the
    // streaming loader — so clear the streaming flag the input loop optimistically
    // set on submit and run the action synchronously.
    if slash::is_slash_command(text) {
        run_slash_command(runner, text);
        return;
    }

    // Clear any stale cancel flag from a prior turn so a cancellation only ever
    // suppresses *this* turn's error banner (VAL-CHAT-013 / VAL-CHAT-014).
    lock_state(&runner.state).cancel_requested = false;

    // OSC 133 A — prompt start, before the user echo.
    queue_raw(&runner.state, &runner.requester, PromptMark::PromptStart);

    // Echo the user message into scrollback immediately for input responsiveness.
    let echo = {
        let ctx = render_context(&runner.state);
        chat::render_update(
            &ChatUpdate::AppendUser {
                text: text.to_string(),
            },
            ctx,
        )
        .unwrap_or_default()
    };
    commit(&runner.state, &runner.requester, echo);

    // OSC 133 B — command start, after the user echo; OSC 9;4 indeterminate while
    // the turn is in flight.
    queue_raw(&runner.state, &runner.requester, PromptMark::CommandStart);
    queue_progress(
        &runner.state,
        &runner.requester,
        ProgressState::Indeterminate,
    );

    // Streaming: loader on + border tint while the turn runs.
    set_streaming(&runner.state, &runner.editor, &runner.requester, true);

    // Grab the cancel handle before the `&mut` send borrow: `cancel_handle`
    // borrows `&self`, `send_message` borrows `&mut self`, so they cannot overlap.
    let cancel = runner.session.cancel_handle();
    let send = runner.session.send_message(text);
    let progress_end = match run_under_watchdog(runner.watchdog, send, &cancel).await {
        TurnOutcome::Completed => ProgressState::Clear,
        TurnOutcome::Failed(e) => {
            // A turn cancelled by the user (Esc / Ctrl+C) returns an error from
            // `send_message`, but that is not a failure: the yellow `[cancelled …]`
            // line already landed from the input loop, so suppress the red banner
            // and clear (not error) the progress. A genuine failure — a missing
            // credential, a send error — was not user-cancelled, so it takes the
            // red-banner route with the loader cleared (VAL-CHAT-016).
            let cancelled = lock_state(&runner.state).take_cancel_requested();
            if failed_turn_shows_banner(cancelled) {
                commit(
                    &runner.state,
                    &runner.requester,
                    chat::error_lines(&format!("send failed: {e}")),
                );
                ProgressState::Error
            } else {
                ProgressState::Clear
            }
        }
        TurnOutcome::TimedOut => {
            commit(
                &runner.state,
                &runner.requester,
                chat::error_lines(&runner.watchdog.timeout_banner()),
            );
            ProgressState::Error
        }
    };

    // OSC 133 C — command end, closing the balanced A/B/C region; then the
    // progress terminal state (clear on success, error otherwise).
    queue_raw(&runner.state, &runner.requester, PromptMark::CommandEnd);
    queue_progress(&runner.state, &runner.requester, progress_end);

    set_streaming(&runner.state, &runner.editor, &runner.requester, false);

    // Refresh the footer from post-turn session state: the running usage
    // accumulator (bumped on each MessageEnd), the re-detected git branch (so a
    // `!git checkout -b tmp` shows on the next turn), and the current
    // thinking-level / context %. The rebuild reads the session, so it lives here
    // in the turn runner (which owns it) rather than in the event applier.
    refresh_footer(runner);
}

/// Execute an inline `!cmd` (or `!!cmd`) bash submission.
///
/// Echoes the `$ cmd` header live in the active-area preview and shows the
/// running loader while the command runs, then commits the finalized bash box
/// (header + output + exit-code footer) to scrollback once the process exits
/// (VAL-CHAT-009). A `!!cmd` renders the frame dim (excluded from the LLM
/// context). Context truncation surfaces the yellow `Output truncated. Full
/// output: <path>` footnote, the path existing on disk (VAL-CHAT-010). A bare
/// `!` / `!!` commits the yellow `[bash] empty command` notice and runs nothing.
async fn run_bash_inline(runner: &mut TurnRunner, parsed: bash::ParsedBash) {
    if parsed.command.is_empty() {
        commit(
            &runner.state,
            &runner.requester,
            vec![bash::empty_command_notice()],
        );
        return;
    }

    // Live header echo in the preview + running loader while the command runs.
    let header = bash::ParsedBash {
        command: parsed.command.clone(),
        exclude_from_context: parsed.exclude_from_context,
    };
    {
        let mut guard = lock_state(&runner.state);
        let running = bash::bash_block_lines(
            &header,
            "",
            bash::BashOutcome::Exited(None),
            None,
            true,
            guard.size.cols,
        );
        guard.set_streaming_preview(Some(running));
    }
    set_streaming(&runner.state, &runner.editor, &runner.requester, true);

    let outcome = runner.session.run_bash(&parsed.command, 0).await;

    // Clear the live preview + loader before committing the finalized box.
    lock_state(&runner.state).set_streaming_preview(None);
    set_streaming(&runner.state, &runner.editor, &runner.requester, false);

    match outcome {
        Ok(run) => {
            let bash_outcome = if run.aborted {
                bash::BashOutcome::Cancelled
            } else {
                bash::BashOutcome::Exited(run.result.exit_code)
            };
            let full_output_path = run
                .result
                .full_output_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned());
            let width = lock_state(&runner.state).size.cols;
            let lines = bash::bash_block_lines(
                &parsed,
                &run.result.output,
                bash_outcome,
                full_output_path.as_deref(),
                false,
                width,
            );
            commit(&runner.state, &runner.requester, lines);
        }
        Err(e) => {
            commit(
                &runner.state,
                &runner.requester,
                chat::error_lines(&format!("bash failed: {e}")),
            );
        }
    }

    // A `!cmd` may change the working tree (branch checkout, file writes); refresh
    // the footer so the next-turn view reflects it.
    refresh_footer(runner);
}

/// Execute a submitted slash command against the live session.
///
/// The input loop optimistically marks the state streaming on every submit
/// (so the loader shows the instant Enter is pressed, before the first event).
/// A slash command has no streaming turn, so clear that flag first, then
/// dispatch the action through the reused parsing layer. Session-lifecycle
/// commands mutate `&mut session`; the footer is rebuilt inside the handlers
/// that change session state, so no post-dispatch refresh is needed here.
fn run_slash_command(runner: &mut TurnRunner, text: &str) {
    set_streaming(&runner.state, &runner.editor, &runner.requester, false);
    let _ = slash::dispatch_slash(
        text,
        &mut runner.session,
        &runner.cwd,
        &runner.state,
        &runner.footer,
        &runner.requester,
    );
}

/// Execute `/compact` against the live session (VAL-CHAT-027).
///
/// `/compact` is the one slash command that awaits, so it runs here on the turn
/// runner (which owns `&mut session`) rather than through the sync dispatch. It:
///
/// 1. names the working-loader `Compacting context…` and turns it on;
/// 2. calls `session.compact()` / `compact_with(steer)`, whose `CompactionStart`
///    / `CompactionEnd` events land the `[Compacting context...]` /
///    `[Compaction complete]` status lines through the event applier;
/// 3. on success commits a *collapsible* compaction summary (collapsed, with the
///    `(ctrl+r to expand)` hint) and remembers it so Ctrl+R can expand it;
/// 4. on failure (e.g. no credential) commits the red `[compact failed: …]`
///    banner.
///
/// The loader is cleared and its message reset whichever way it ends.
async fn run_compact(runner: &mut TurnRunner, steer: Option<String>) {
    // Name the loader for its duration, then turn it on. The input loop marked
    // streaming on submit; naming the loader makes it read `Compacting context…`.
    lock_state(&runner.state).loader_message = Some("Compacting context…".to_string());
    set_streaming(&runner.state, &runner.editor, &runner.requester, true);

    // Snapshot the pre-compaction message count so the summary reports it.
    let tokens_before = runner.session.message_count() as u64;
    let result = match steer.as_deref() {
        Some(s) => runner.session.compact_with(s).await,
        None => runner.session.compact().await,
    };

    // Reset the loader message + clear the loader before committing the outcome.
    lock_state(&runner.state).loader_message = None;
    set_streaming(&runner.state, &runner.editor, &runner.requester, false);

    match result {
        Ok(summary) => {
            let entry = summary::CollapsibleSummary::compaction(summary, tokens_before);
            let lines = {
                let mut guard = lock_state(&runner.state);
                let width = guard.size.cols;
                let lines = summary::summary_lines(&entry, width);
                guard.remember_summary(entry);
                lines
            };
            commit(&runner.state, &runner.requester, lines);
        }
        Err(e) => {
            commit(
                &runner.state,
                &runner.requester,
                chat::error_lines(&format!("[compact failed: {e}]")),
            );
        }
    }

    // Compaction changed the transcript; refresh the footer so context % rebases.
    refresh_footer(runner);
}

/// Open the `/model` selector overlay and apply the user's pick (VAL-OVERLAY-*).
///
/// The input loop optimistically marked the state streaming on submit; a selector
/// is not a streaming turn, so clear that first. Then hand off to
/// [`selectors::open_model_selector`], which mounts the modal dialog, awaits the
/// single outcome (fed by the input loop driving the dialog), applies it
/// (`session.set_model` + footer refresh + status line) or reports the cancel, and
/// returns once the dialog closes.
async fn run_model_selector(runner: &mut TurnRunner) {
    set_streaming(&runner.state, &runner.editor, &runner.requester, false);
    selectors::open_model_selector(
        &mut runner.session,
        &runner.cwd,
        &runner.overlays,
        &runner.overlay_done,
        &runner.state,
        &runner.footer,
        &runner.requester,
    )
    .await;
}

/// Open the `/resume` session picker overlay and, on a pick, switch to and replay
/// the chosen session (VAL-OVERLAY-010 / VAL-CHAT-012 / VAL-CHAT-032).
///
/// The input loop optimistically marked the state streaming on submit; a picker is
/// not a streaming turn, so clear that first. Then hand off to
/// [`selectors::open_resume_picker`], which lists the resumable sessions, mounts the
/// modal dialog, awaits the single outcome (fed by the input loop driving the
/// dialog), switches + replays the pick (or reports the cancel), and returns once
/// the dialog closes.
async fn run_resume_picker(runner: &mut TurnRunner) {
    set_streaming(&runner.state, &runner.editor, &runner.requester, false);
    selectors::open_resume_picker(
        &mut runner.session,
        &runner.cwd,
        &runner.overlays,
        &runner.overlay_done,
        &runner.state,
        &runner.footer,
        &runner.requester,
    )
    .await;
}

/// Route a config-selector command (`/thinking`, `/theme`, `/settings`,
/// `/model <pattern>`) to its overlay open or direct-arg apply (VAL-OVERLAY-013 /
/// -014 / -017 / -018 / -025 / -026 / -036).
///
/// The input loop optimistically marked the state streaming on submit; a selector
/// (or a direct-arg apply) is not a streaming turn, so clear that first. Then:
///
/// - the **bare** forms (`/thinking`, `/theme`, `/settings`) mount a modal overlay
///   and await the pick;
/// - the **direct-arg** forms (`/thinking <level>`, `/theme <name>`,
///   `/model <pattern>`) apply immediately with a status line and no dialog.
///
/// The dialog-vs-direct-arg split is carried by the typed
/// [`SlashCommandAction`](crate::modes::interactive::slash_commands::SlashCommandAction)
/// the driver parsed before entering here.
async fn run_config_selector(
    runner: &mut TurnRunner,
    action: crate::modes::interactive::slash_commands::SlashCommandAction,
) {
    use crate::modes::interactive::slash_commands::SlashCommandAction;

    set_streaming(&runner.state, &runner.editor, &runner.requester, false);

    match action {
        // `/thinking` (bare) opens the ladder; `/thinking <level>` applies directly.
        SlashCommandAction::OpenThinkingSelector { inline_level } => match inline_level {
            None => {
                selectors::open_thinking_selector(
                    &mut runner.session,
                    &runner.cwd,
                    &runner.overlays,
                    &runner.overlay_done,
                    &runner.state,
                    &runner.footer,
                    &runner.requester,
                )
                .await;
            }
            Some(level) => selectors::apply_thinking_inline(
                &mut runner.session,
                &runner.cwd,
                &level,
                &runner.state,
                &runner.footer,
                &runner.requester,
            ),
        },
        // `/theme` (bare) opens the picker; `/theme <name>` applies directly.
        SlashCommandAction::Theme(name) => match name {
            None => {
                selectors::open_theme_selector(
                    &mut runner.session,
                    &runner.overlays,
                    &runner.overlay_done,
                    &runner.state,
                    &runner.requester,
                )
                .await;
            }
            Some(name) => selectors::apply_theme_inline(
                &mut runner.session,
                &name,
                &runner.state,
                &runner.requester,
            ),
        },
        // `/settings` opens the editable settings dialog (M2 SettingsList).
        SlashCommandAction::OpenSettingsSelector => {
            selectors::open_settings_selector(
                &mut runner.session,
                &runner.cwd,
                &runner.overlays,
                &runner.overlay_done,
                &runner.state,
                &runner.footer,
                &runner.requester,
            )
            .await;
        }
        // `/model <pattern>` is a non-interactive switch (bare `/model` is caught
        // earlier by `is_open_model_selector`).
        SlashCommandAction::ModelByPattern(pattern) => selectors::apply_model_pattern(
            &mut runner.session,
            &runner.cwd,
            &pattern,
            &runner.state,
            &runner.footer,
            &runner.requester,
        ),
        // `config_selector_action` only ever yields the four arms above; any other
        // action means the routing predicate and this match disagree — dispatch it
        // synchronously so it is never silently dropped.
        other => {
            let _ = slash::apply_slash_action(
                other,
                &mut runner.session,
                &runner.cwd,
                &runner.state,
                &runner.footer,
                &runner.requester,
            );
        }
    }
}

/// Route a picker-selector command (`/tree`, `/scoped-models`, `/fork`) to its
/// overlay open (VAL-OVERLAY-011 / -019 / -023 / -024 / -031 / -033).
///
/// The input loop optimistically marked the state streaming on submit; a picker is
/// not a streaming turn, so clear that first. Then mount the picker's modal overlay
/// and await the pick — each open owns its no-data degradation (a non-directory
/// `/tree`, an empty registry `/scoped-models`, a user-message-less `/fork` land the
/// status line without opening) and applies its result against `&mut session`.
async fn run_picker_selector(
    runner: &mut TurnRunner,
    action: crate::modes::interactive::slash_commands::SlashCommandAction,
) {
    use crate::modes::interactive::slash_commands::SlashCommandAction;

    set_streaming(&runner.state, &runner.editor, &runner.requester, false);

    match action {
        // `/tree` (bare or `<subdir>`) opens the directory picker.
        SlashCommandAction::OpenTreeSelector(arg) => {
            selectors::open_tree_selector(
                &runner.cwd,
                arg.as_deref(),
                &runner.overlays,
                &runner.overlay_done,
                &runner.state,
                &runner.requester,
            )
            .await;
        }
        // `/scoped-models` opens the session-only multi-select.
        SlashCommandAction::OpenScopedModelsSelector => {
            selectors::open_scoped_models_selector(
                &mut runner.session,
                &runner.overlays,
                &runner.overlay_done,
                &runner.state,
                &runner.requester,
            )
            .await;
        }
        // `/fork` (bare or `<entry-id>`) opens the fork-from-message picker.
        SlashCommandAction::Fork(_) => {
            selectors::open_fork_selector(
                &mut runner.session,
                &runner.cwd,
                &runner.overlays,
                &runner.overlay_done,
                &runner.state,
                &runner.footer,
                &runner.requester,
            )
            .await;
        }
        // `picker_selector_action` only ever yields the three arms above; any other
        // action means the routing predicate and this match disagree — dispatch it
        // synchronously so it is never silently dropped.
        other => {
            let _ = slash::apply_slash_action(
                other,
                &mut runner.session,
                &runner.cwd,
                &runner.state,
                &runner.footer,
                &runner.requester,
            );
        }
    }
}

/// Route a login-family command (`/login`, `/logout`) to its overlay flow
/// (VAL-OVERLAY-015 / -016 / -027 / -028 / -029 / -034).
///
/// The input loop optimistically marked the state streaming on submit; a login flow
/// is not a streaming turn, so clear that first. Then:
///
/// - `/login` (bare) opens the provider picker → the chosen provider's flow (OAuth
///   for `anthropic` / `openai-codex` / `github-copilot`, else the API-key dialog);
///   `/login <provider>` skips the picker and goes straight to that provider's flow,
///   split case-insensitively (VAL-OVERLAY-034);
/// - `/logout` clears stored credentials so the `configured` badge disappears on the
///   next `/login` (VAL-OVERLAY-029).
async fn run_login(
    runner: &mut TurnRunner,
    action: crate::modes::interactive::slash_commands::SlashCommandAction,
) {
    use crate::modes::interactive::slash_commands::SlashCommandAction;

    set_streaming(&runner.state, &runner.editor, &runner.requester, false);

    match action {
        SlashCommandAction::OpenLoginDialog { provider } => {
            login::open_login(
                &mut runner.session,
                provider.as_deref(),
                &runner.overlays,
                &runner.overlay_done,
                &runner.state,
                &runner.requester,
            )
            .await;
        }
        SlashCommandAction::Logout => {
            login::open_logout(&mut runner.session, None, &runner.state, &runner.requester).await;
        }
        // `login_action` only ever yields the two arms above; any other action means
        // the routing predicate and this match disagree — dispatch it synchronously
        // so it is never silently dropped.
        other => {
            let _ = slash::apply_slash_action(
                other,
                &mut runner.session,
                &runner.cwd,
                &runner.state,
                &runner.footer,
                &runner.requester,
            );
        }
    }
}

/// Whether a failed turn commits the red `send failed` banner.
///
/// A user cancellation (Esc / Ctrl+C) surfaces as a `send_message` error, but the
/// yellow `[cancelled …]` line already landed from the input loop, so no red
/// banner is shown. A genuine failure (a missing credential, a transport error)
/// was not user-cancelled, so it shows the banner (VAL-CHAT-016). Kept as a free
/// fn so this either/or decision is unit-tested without a running turn.
fn failed_turn_shows_banner(cancelled: bool) -> bool {
    !cancelled
}

/// Rebuild the footer view-model from current session state and the running usage
/// accumulator, then request a repaint so the new fields show.
fn refresh_footer(runner: &TurnRunner) {
    let usage = lock_state(&runner.state).usage;
    let view = build_footer_view(&runner.session, &runner.cwd, usage);
    *lock_footer(&runner.footer) = view;
    runner.requester.request_frame();
}

/// Queue an OSC 133 prompt mark for the draw closure to write, and request a
/// frame so it is flushed promptly.
fn queue_raw(state: &Arc<Mutex<DriverState>>, requester: &FrameRequester, mark: PromptMark) {
    lock_state(state).queue_raw(mark.sequence());
    requester.request_frame();
}

/// Queue an OSC 9;4 progress update for the draw closure to write, and request a
/// frame so it is flushed promptly.
fn queue_progress(
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
    progress: ProgressState,
) {
    lock_state(state).queue_raw(progress.sequence());
    requester.request_frame();
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

/// Apply a single agent session event.
///
/// Finalized blocks commit into immutable scrollback; the in-flight assistant
/// partial renders live in the active-area preview. The event handling keys off
/// message boundaries:
///
/// - A streaming `MessageUpdate` (the reused `event_dispatch`'s
///   `ReplaceLastAssistant` delta) refreshes the active-area preview via the
///   scheduler's request-frame — the M1 live-block semantics, throttled by the
///   scheduler's own frame coalescing, not committed per token.
/// - `MessageEnd` clears the preview and commits the *final* assistant snapshot
///   once, from `assistant_lines`, and remembers it so a later Ctrl+T can
///   re-render the whole transcript under the new thinking state.
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
        // Surface the failure to the terminal's progress indicator (OSC 9;4
        // error) alongside the red banner. The turn runner clears it at the turn
        // boundary; here it marks the in-flight error immediately.
        queue_progress(state, requester, ProgressState::Error);
        commit(state, requester, chat::error_lines(msg));
        return;
    }

    if let AgentSessionEvent::Agent(agent_event) = event {
        match agent_event.as_ref() {
            // Streaming delta → live active-area preview (not scrollback). The
            // scheduler coalesces the request-frames, so a fast token stream
            // repaints at the frame rate rather than once per token.
            hand_agent::types::AgentEvent::MessageUpdate {
                message: model::Message::Assistant(assistant),
                ..
            } => {
                update_streaming_preview(state, requester, assistant);
                return;
            }
            // Finalize: clear the preview, commit the final snapshot once, and
            // remember it for a global Ctrl+T re-render.
            hand_agent::types::AgentEvent::MessageEnd {
                message: model::Message::Assistant(assistant),
            } => {
                finalize_assistant(state, requester, assistant);
                return;
            }
            // A tool call begins: remember its name + args so the matching end
            // can render a complete state-tinted box. Nothing commits yet — the
            // box lands finalized on the end (the running loader rides the
            // streaming flag meanwhile).
            hand_agent::types::AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                lock_state(state).remember_tool(
                    tool_call_id.clone(),
                    tool_name.clone(),
                    args.clone(),
                );
                return;
            }
            // A tool call finished: commit its state-tinted box (name / args /
            // result), tinting success or failure by the error flag. The result
            // text is resolved through the image-parity path so a graphics
            // terminal emits no image bytes and a plain one shows a `[mime WxH]`
            // indicator (VAL-IMG-019).
            hand_agent::types::AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
            } => {
                finalize_tool(state, requester, tool_call_id, tool_name, result, *is_error);
                return;
            }
            _ => {}
        }
    }

    // Everything else (tool lifecycle, status, compaction) flows through the
    // reused dispatch → render path; streaming deltas render nothing here.
    let ctx = render_context(state);
    let mut lines = Vec::new();
    for update in dispatch_event(event) {
        if let Some(rendered) = chat::render_update(&update, ctx) {
            lines.extend(rendered);
        }
    }
    commit(state, requester, lines);
}

/// Refresh the active-area streaming preview from an in-flight assistant partial
/// and request a repaint. The preview renders the concatenated text through the
/// stream renderer, which defensively closes an unclosed code fence so the code
/// styling stays contained mid-stream (VAL-CHAT-033).
fn update_streaming_preview(
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
    assistant: &model::AssistantMessage,
) {
    let (width, partial) = {
        let guard = lock_state(state);
        (guard.size.cols, assistant_stream_text(assistant))
    };
    let preview = if partial.trim().is_empty() {
        None
    } else {
        Some(messages::stream_preview_lines(&partial, width))
    };
    lock_state(state).set_streaming_preview(preview);
    requester.request_frame();
}

/// Finalize an assistant message: clear the live preview, commit its rendered
/// lines to scrollback once, and remember the snapshot so a later global
/// thinking-toggle (Ctrl+T) can re-render the whole transcript.
fn finalize_assistant(
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
    assistant: &model::AssistantMessage,
) {
    let lines = {
        let mut guard = lock_state(state);
        guard.set_streaming_preview(None);
        // Fold this message's usage into the running total so the footer's spend
        // segment increases monotonically across the session (VAL-CHAT-005). The
        // turn runner rebuilds the footer from this accumulator at the turn
        // boundary.
        guard.accumulate_usage(&assistant.usage);
        let ctx = chat::RenderContext {
            width: guard.size.cols,
            hide_thinking: guard.hide_thinking,
        };
        let lines = messages::assistant_lines(assistant, ctx.hide_thinking, ctx.width);
        guard.remember_assistant(assistant.clone());
        lines
    };
    commit(state, requester, lines);
}

/// Commit a finished tool call's state-tinted box to scrollback.
///
/// Pairs the finishing tool with the name + args remembered at start (falling
/// back to the end event's own name when the start was missed — a defensive
/// path). The result text is resolved through the image-parity pipeline
/// ([`resolve_tool_result_text`]): a graphics-capable terminal excludes image
/// blocks entirely (zero graphics bytes reach the chat, per Decision Log ⑤),
/// while a plain terminal replaces each with a `[mime WxH]` indicator box
/// (VAL-IMG-019). The box tints success or failure by `is_error`
/// (VAL-CHAT-011), and `edit` / `write` tools render their diff with `+`/`-`
/// coloring inside the box (VAL-CHAT-039).
fn finalize_tool(
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
    tool_call_id: &str,
    tool_name: &str,
    result: &hand_agent::types::ToolResult,
    is_error: bool,
) {
    let result_text = resolve_tool_result_text(result);
    let lines = {
        let mut guard = lock_state(state);
        let pending = guard.take_tool(tool_call_id);
        let (name, args) = match pending {
            Some(p) => (p.name, p.args),
            None => (tool_name.to_string(), serde_json::Value::Null),
        };
        let state_tint = tools::ToolState::from_result(Some(is_error));
        tools::tool_box_lines(&name, &args, &result_text, state_tint, guard.size.cols)
    };
    commit(state, requester, lines);
}

/// Resolve a tool result's content to display text with image parity, using the
/// terminal's detected image capabilities.
///
/// A thin wrapper over [`tool_result_display_text`] that reads the process-global
/// detected capabilities; the pure inner function takes them explicitly so the
/// parity behaviour is unit-tested without touching the global cache.
fn resolve_tool_result_text(result: &hand_agent::types::ToolResult) -> String {
    let caps = hand_tui::get_capabilities();
    tool_result_display_text(result, &caps)
}

/// Resolve a tool result's content to display text with image parity, against an
/// explicit capability set.
///
/// Routes the result blocks through
/// [`get_text_output`](crate::tools::render_utils::get_text_output) with
/// `show_images = true`: a graphics terminal (kitty / iTerm2) drops image blocks
/// from the text (the chat shows no image — Decision Log ⑤ — so the chat emits
/// zero graphics bytes), while a plain terminal substitutes a `[mime WxH]`
/// indicator box so the presence of an image is still visible (VAL-IMG-019).
fn tool_result_display_text(
    result: &hand_agent::types::ToolResult,
    caps: &hand_tui::TerminalImageCapabilities,
) -> String {
    crate::tools::render_utils::get_text_output(&result.content, true, caps)
}

/// The concatenated text content of an in-flight assistant partial, joined with
/// newlines — the mid-stream body the preview renders.
fn assistant_stream_text(message: &model::AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            model::AssistantContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build the [`chat::RenderContext`] from the current driver state — the render
/// width and the global thinking-collapse flag — for the arms that render rich
/// message bodies.
fn render_context(state: &Arc<Mutex<DriverState>>) -> chat::RenderContext {
    let guard = lock_state(state);
    chat::RenderContext {
        width: guard.size.cols,
        hide_thinking: guard.hide_thinking,
    }
}

/// Flip the global thinking-collapse state (Ctrl+T) and re-commit the whole
/// assistant transcript under the new state, followed by a status line.
///
/// Native scrollback is immutable — already-committed lines cannot be rewritten
/// in place — so a *global* flip re-renders every remembered assistant message
/// under the new `hide_thinking` value and commits the re-rendered transcript as
/// a fresh block, then a `[thinking blocks: hidden/visible]` status line. The
/// user sees the transcript reflowed with thinking collapsed/expanded and a
/// clear marker of the current state.
fn toggle_thinking_globally(state: &Arc<Mutex<DriverState>>, requester: &FrameRequester) {
    let (blocks, status) = {
        let mut guard = lock_state(state);
        let hidden = guard.toggle_thinking();
        let width = guard.size.cols;
        let blocks: Vec<Vec<Line<'static>>> = guard
            .assistant_history
            .iter()
            .map(|msg| messages::assistant_lines(msg, hidden, width))
            .filter(|lines| !lines.is_empty())
            .collect();
        (blocks, thinking_status_line(hidden))
    };

    for block in blocks {
        commit(state, requester, block);
    }
    commit(state, requester, chat::status_lines_for(&status));
}

/// Flip the most-recent collapsible summary's expansion state (Ctrl+R) and
/// re-commit it under the new state.
///
/// Native scrollback is immutable — a committed collapsed summary cannot be
/// rewritten in place — so a toggle re-renders the summary in its new state and
/// commits it as a fresh block (the same discipline the Ctrl+T thinking toggle
/// uses). This is what makes the collapsed `(ctrl+r to expand)` hint *real*: the
/// hint promises an expand, and pressing Ctrl+R delivers the expanded block. A
/// silent no-op when no collapsible summary has landed yet.
fn toggle_last_summary(state: &Arc<Mutex<DriverState>>, requester: &FrameRequester) {
    let lines = {
        let mut guard = lock_state(state);
        match guard.toggle_last_summary() {
            Some(entry) => summary::summary_lines(&entry, guard.size.cols),
            None => return,
        }
    };
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

/// Assemble the startup-chrome scrollback blocks in order: the welcome header,
/// any tmux keyboard warning, and the changelog banner (when the session is a
/// fresh, empty one that has fallen behind the recorded version).
///
/// Each returned `Vec<Line>` is committed as a single `insert_before` block. The
/// changelog decision reads `last_changelog_version` from settings and, on
/// display or a fresh install, records the current version back — so this takes
/// `&mut session`. Empty blocks are skipped so no phantom scrollback lands.
fn collect_startup_chrome(session: &mut AgentSession) -> Vec<Vec<Line<'static>>> {
    let mut blocks: Vec<Vec<Line<'static>>> = Vec::new();

    // 1. Welcome header — always first at the top of scrollback.
    let model = session.model();
    blocks.push(chrome::welcome_header_lines(
        model.provider.as_str(),
        &model.id,
        chrome::version(),
    ));

    // 2. tmux keyboard warning — only inside a misconfigured tmux.
    if let Some(warning) = chrome::check_tmux_keyboard_setup() {
        blocks.push(chrome::warning_lines(warning));
    }

    // 3. Changelog banner — three-state, gated on an empty (non-resumed) session.
    if let Some(block) = changelog_startup_block(session) {
        blocks.push(block);
    }

    blocks.into_iter().filter(|b| !b.is_empty()).collect()
}

/// Resolve the changelog three-state and return the scrollback block to display,
/// or `None` when the action is skip / record-only. On display or a fresh
/// install it records the current version back into settings (best-effort — any
/// save failure is swallowed so startup never blocks).
fn changelog_startup_block(session: &mut AgentSession) -> Option<Vec<Line<'static>>> {
    let current_version = chrome::version();
    let messages_empty = session.messages().is_empty();
    let last_version = session.settings().current().last_changelog_version.clone();

    let path = chrome::locate_changelog_file()?;
    let entries = crate::utils::changelog::parse_changelog_file(&path).ok()?;

    let action =
        chrome::decide_changelog_startup(messages_empty, last_version.as_deref(), &entries);
    let scope = crate::core::settings::SettingsScope::Global;
    match action {
        ChangelogStartupAction::Skip => None,
        ChangelogStartupAction::RecordOnly => {
            session
                .settings_mut()
                .set_last_changelog_version(scope, Some(current_version.to_string()));
            let _ = session.settings().save(scope);
            None
        }
        ChangelogStartupAction::Display(body) => {
            session
                .settings_mut()
                .set_last_changelog_version(scope, Some(current_version.to_string()));
            let _ = session.settings().save(scope);
            Some(chrome::changelog_lines(&body))
        }
    }
}

/// Best-effort async version probe. Hits crates.io through the default fetcher
/// and, on a newer published version, commits an "update available" banner and
/// requests a repaint. Offline (`HAND_OFFLINE`) or on any fetch failure the
/// fetcher returns `None`, so nothing is committed — and because it runs in its
/// own task, an offline start never blocks the run loop (the poll budget is the
/// fetcher's own timeout, not the startup path's).
async fn version_probe(state: Arc<Mutex<DriverState>>, requester: FrameRequester) {
    let fetcher = crate::utils::version_check::HttpVersionFetcher::new();
    let current = chrome::version();
    if let Some(latest) =
        crate::utils::version_check::check_for_new_version(&fetcher, current).await
    {
        let banner = chrome::update_available_banner(current, &latest);
        commit(&state, &requester, chrome::warning_lines(&banner));
    }
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

    /// Ctrl+C is no longer a quit at the `handle_key` level: turn control resolves
    /// it in the input loop (cancel / no-op), so it must never quit and never
    /// leave the terminal raw (VAL-CHAT-014). With a non-empty buffer it also must
    /// not delete a character — the input loop consumes it before the editor.
    #[test]
    fn ctrl_c_is_not_a_quit_at_handle_key() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let editor: SharedEditor = Arc::new(Mutex::new(Editor::new()));
        lock_editor(&editor).set_text("draft");
        let state = Arc::new(Mutex::new(DriverState::new(TerminalSize::new(80, 24))));
        let (tx, _rx) = mpsc::unbounded_channel::<String>();

        let ctrl_c = RtKey {
            key_id: Some("ctrl+c".to_string()),
            raw: KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        };
        // `handle_key` never sees Ctrl+C in the real loop (the loop consumes it),
        // but even if it did it must not quit — only Ctrl+D quits.
        let outcome = handle_key(&ctrl_c, &editor, &state, &tx);
        assert_eq!(outcome, KeyOutcome::Continue, "Ctrl+C must not quit");
    }

    /// The cancel core is a no-op when idle: no loader, nothing to cancel. It
    /// reports `false` (so Esc falls to the editor / Ctrl+C is a visible no-op),
    /// leaves the state untouched, and never cancels the token (VAL-CHAT-013 /
    /// VAL-CHAT-014, the idle Ctrl+C no-op parity pin).
    #[test]
    fn cancel_is_a_noop_when_no_turn_is_streaming() {
        use hand_agent::CancellationToken;

        let state = Arc::new(Mutex::new(DriverState::new(TerminalSize::new(80, 24))));
        let cancel = Arc::new(Mutex::new(CancellationToken::new()));

        let cancelled = cancel_in_flight_turn(&state, &cancel);

        assert!(!cancelled, "idle cancel must report nothing to cancel");
        assert!(!lock_state(&state).streaming, "state stays idle");
        assert!(
            !lock_state(&state).cancel_requested,
            "no cancel flag latched when idle"
        );
        assert!(
            !cancel.lock().unwrap().is_cancelled(),
            "the token must not be cancelled by an idle no-op"
        );
    }

    /// The cancel core cancels a streaming turn: it clears the loader (streaming
    /// off), latches the cancel flag (so the turn runner suppresses the error
    /// banner), cancels the session token (so `send_message` unwinds), and reports
    /// `true`. This is the Esc / Ctrl+C cancel path (VAL-CHAT-013 / VAL-CHAT-014).
    #[test]
    fn cancel_stops_a_streaming_turn_and_clears_the_loader() {
        use hand_agent::CancellationToken;

        let state = Arc::new(Mutex::new(DriverState::new(TerminalSize::new(80, 24))));
        lock_state(&state).streaming = true; // a turn is in flight (loader shown).
        let cancel = Arc::new(Mutex::new(CancellationToken::new()));

        let cancelled = cancel_in_flight_turn(&state, &cancel);

        assert!(cancelled, "a streaming turn must be cancelled");
        assert!(
            !lock_state(&state).streaming,
            "the loader clears immediately on cancel"
        );
        assert!(
            lock_state(&state).cancel_requested,
            "the cancel flag is latched so the error banner is suppressed"
        );
        assert!(
            cancel.lock().unwrap().is_cancelled(),
            "the session token is cancelled so send_message unwinds"
        );
    }

    /// A second cancel after the first is inert: the first clears `streaming`, so
    /// the second sees no in-flight turn and reports `false` — no duplicate
    /// `[cancelled …]` line lands from a key repeat.
    #[test]
    fn a_second_cancel_after_the_first_is_inert() {
        use hand_agent::CancellationToken;

        let state = Arc::new(Mutex::new(DriverState::new(TerminalSize::new(80, 24))));
        lock_state(&state).streaming = true;
        let cancel = Arc::new(Mutex::new(CancellationToken::new()));

        assert!(
            cancel_in_flight_turn(&state, &cancel),
            "first press cancels"
        );
        assert!(
            !cancel_in_flight_turn(&state, &cancel),
            "second press is a no-op (no duplicate cancel line)"
        );
    }

    /// The cancel-source status lines name the key the user pressed, so scrollback
    /// distinguishes an Esc cancel from a Ctrl+C one.
    #[test]
    fn cancel_source_lines_name_the_key() {
        assert_eq!(CancelSource::Esc.cancel_line(), "[cancelled by Esc]");
        assert_eq!(CancelSource::CtrlC.cancel_line(), "[cancelled by ^C]");
    }

    /// `take_cancel_requested` reports and clears the latch exactly once: a
    /// cancelled turn's error is suppressed on the turn it was cancelled, and a
    /// later turn (no cancel) sees a cleared flag so its genuine error surfaces.
    #[test]
    fn take_cancel_requested_is_a_one_shot_latch() {
        let mut state = DriverState::new(TerminalSize::new(80, 24));
        state.mark_cancelled();
        assert!(state.take_cancel_requested(), "the latched cancel is taken");
        assert!(
            !state.take_cancel_requested(),
            "a second take sees a cleared flag — a later turn's error is not masked"
        );
    }

    /// A failed turn shows the red banner only when it was *not* user-cancelled: a
    /// genuine failure (missing credential, send error) surfaces the red banner
    /// with the loader cleared (VAL-CHAT-016); a cancellation suppresses it (the
    /// yellow cancel line already landed).
    #[test]
    fn failed_turn_shows_banner_only_when_not_cancelled() {
        assert!(
            failed_turn_shows_banner(false),
            "a genuine failure shows the red banner"
        );
        assert!(
            !failed_turn_shows_banner(true),
            "a user cancellation suppresses the red banner"
        );
    }

    /// Two messages submitted while a turn is in flight are both queued on the
    /// unbounded submit channel and drained in order — neither is dropped
    /// (VAL-CHAT-015). The channel *is* the queue: the turn runner drains it one
    /// turn at a time, so a submit mid-turn parks until the current turn ends and
    /// then processes in FIFO order.
    #[test]
    fn queued_submits_during_a_turn_are_preserved_in_order() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        // Simulate two follow-up messages submitted while a turn is running: they
        // are pushed to the channel back to back.
        tx.send("first follow-up".to_string()).unwrap();
        tx.send("second follow-up".to_string()).unwrap();

        // The turn runner drains them in FIFO order after the current turn ends;
        // neither is lost, and the order is preserved.
        assert_eq!(rx.try_recv().ok(), Some("first follow-up".to_string()));
        assert_eq!(rx.try_recv().ok(), Some("second follow-up".to_string()));
        assert!(
            rx.try_recv().is_err(),
            "exactly two were queued, none dropped"
        );
    }

    // --- collapsible summary Ctrl+R expand (VAL-CHAT-019) ----------------

    /// A real [`FrameRequester`] over a no-op scheduler, built under the test's
    /// tokio runtime. `request_frame` only sends on a channel and tolerates a
    /// dead scheduler, so the summary toggle repaints without a live terminal.
    fn test_requester() -> FrameRequester {
        let (requester, _handle) = hand_tui::rt::scheduler::FrameScheduler::spawn(|| Ok(()));
        requester
    }

    /// Ctrl+R with no committed summary is a silent no-op: nothing is committed,
    /// so a stray Ctrl+R before any compaction / branch / skill summary never
    /// scrolls the screen.
    #[tokio::test]
    async fn toggle_last_summary_with_no_summary_commits_nothing() {
        let state = Arc::new(Mutex::new(DriverState::new(TerminalSize::new(80, 24))));
        let requester = test_requester();

        toggle_last_summary(&state, &requester);

        assert!(
            lock_state(&state).pending_commits.is_empty(),
            "a Ctrl+R with no summary must commit nothing"
        );
    }

    /// Ctrl+R on a committed (collapsed) summary re-commits it *expanded*: the
    /// hint is real. Scrollback is immutable, so the expanded block is appended,
    /// carrying the summary body that was hidden while collapsed. A second Ctrl+R
    /// re-commits it collapsed again (the hint returns).
    #[tokio::test]
    async fn toggle_last_summary_re_commits_expanded_then_collapsed() {
        let state = Arc::new(Mutex::new(DriverState::new(TerminalSize::new(80, 24))));
        let requester = test_requester();
        // A collapsed compaction summary is on the state (as if /compact just
        // landed it). Its body is hidden while collapsed.
        lock_state(&state).remember_summary(summary::CollapsibleSummary::compaction(
            "the recovered summary body",
            2_000,
        ));

        // First Ctrl+R expands: the re-committed block reveals the body.
        toggle_last_summary(&state, &requester);
        let expanded: String = lock_state(&state)
            .pending_commits
            .iter()
            .flatten()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref().to_string())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            expanded.contains("the recovered summary body"),
            "Ctrl+R must re-commit the summary expanded: {expanded}"
        );

        // Second Ctrl+R collapses again: the fresh block carries the hint.
        lock_state(&state).take_commits();
        toggle_last_summary(&state, &requester);
        let collapsed: String = lock_state(&state)
            .pending_commits
            .iter()
            .flatten()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref().to_string())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            collapsed.contains("ctrl+r to expand"),
            "a second Ctrl+R collapses the summary again: {collapsed}"
        );
        assert!(
            !collapsed.contains("the recovered summary body"),
            "collapsed again hides the body: {collapsed}"
        );
    }

    // --- tool-result image parity (VAL-IMG-019) --------------------------

    fn plain_caps() -> hand_tui::TerminalImageCapabilities {
        hand_tui::TerminalImageCapabilities {
            kitty: false,
            iterm2: false,
            cell_dimensions: hand_tui::CellDimensions {
                width: 8,
                height: 16,
            },
        }
    }

    fn kitty_caps() -> hand_tui::TerminalImageCapabilities {
        hand_tui::TerminalImageCapabilities {
            kitty: true,
            iterm2: false,
            cell_dimensions: hand_tui::CellDimensions {
                width: 8,
                height: 16,
            },
        }
    }

    fn image_result() -> hand_agent::types::ToolResult {
        use model::types::{ImageContent, TextContent, ToolResultContent};
        hand_agent::types::ToolResult {
            content: vec![
                ToolResultContent::Text(TextContent::new("screenshot")),
                // A tiny 1x1 PNG so dimension probing has real bytes to read.
                ToolResultContent::Image(ImageContent::new(
                    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
                    "image/png",
                )),
            ],
            details: None,
            terminate: None,
        }
    }

    /// On a graphics-capable (kitty) terminal, an image block is excluded from
    /// the tool-result text entirely — the chat shows the surrounding text and
    /// no image bytes leak in (Decision Log ⑤ / VAL-IMG-019).
    #[test]
    fn tool_result_text_on_kitty_omits_the_image_with_no_indicator() {
        let text = tool_result_display_text(&image_result(), &kitty_caps());
        assert!(
            text.contains("screenshot"),
            "surrounding text kept: {text:?}"
        );
        assert!(
            !text.contains("image/png"),
            "graphics terminal must not emit an image indicator (image shown out-of-band, and per Decision Log ⑤ not in chat): {text:?}"
        );
    }

    /// On a plain terminal, the image block is replaced with a `[mime WxH]`
    /// indicator box so its presence is still visible (VAL-IMG-019 plain
    /// persona).
    #[test]
    fn tool_result_text_on_plain_shows_a_mime_indicator() {
        let text = tool_result_display_text(&image_result(), &plain_caps());
        assert!(
            text.contains("screenshot"),
            "surrounding text kept: {text:?}"
        );
        assert!(
            text.contains("image/png"),
            "plain terminal must show a [mime WxH] indicator: {text:?}"
        );
    }

    // --- inline bash routing (VAL-CHAT-009) ------------------------------

    /// A `!cmd` submission is recognised as inline bash (so the turn runner runs
    /// it locally rather than forwarding it to the model), while a plain prompt
    /// is not.
    #[test]
    fn bang_prefixed_submit_is_recognised_as_inline_bash() {
        assert!(bash::parse_inline_bash("!echo hi").is_some());
        assert!(bash::parse_inline_bash("!!git status").is_some());
        assert!(
            bash::parse_inline_bash("explain this code").is_none(),
            "a plain prompt is not inline bash"
        );
    }

    /// Two rapid submits through `handle_key` both reach the channel: the second
    /// does not overwrite the first (the single-slot drop the skeleton warned
    /// about does not happen — the channel is a queue), and both mark streaming
    /// (VAL-CHAT-015).
    #[test]
    fn rapid_submits_through_handle_key_all_reach_the_queue() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let editor: SharedEditor = Arc::new(Mutex::new(Editor::new()));
        let state = Arc::new(Mutex::new(DriverState::new(TerminalSize::new(80, 24))));
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let enter = RtKey {
            key_id: Some("enter".to_string()),
            raw: KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        };

        lock_editor(&editor).set_text("msg one");
        assert_eq!(
            handle_key(&enter, &editor, &state, &tx),
            KeyOutcome::Continue
        );
        lock_editor(&editor).set_text("msg two");
        assert_eq!(
            handle_key(&enter, &editor, &state, &tx),
            KeyOutcome::Continue
        );

        assert_eq!(rx.try_recv().ok(), Some("msg one".to_string()));
        assert_eq!(rx.try_recv().ok(), Some("msg two".to_string()));
        assert!(rx.try_recv().is_err(), "both submits queued, none dropped");
        assert!(lock_state(&state).streaming, "submit marks streaming");
    }
}
