//! Shared, `Send` state for the rt interactive driver.
//!
//! The rt scheduler's draw closure runs in a spawned tokio task that requires
//! `Send`, and the agent driver task streams updates in concurrently, so the
//! pieces both sides touch live behind `Arc<Mutex<…>>` — the same concurrency
//! model the legacy driver used (`Arc<Mutex<ChatList>>` + friends) and the rt
//! demo mirrors.
//!
//! Two things are shared:
//!
//! - [`DriverState`] — the plain, `Send` fields the draw closure reads and the
//!   input / agent tasks mutate: the tracked terminal size, the input-body row
//!   count (auto-grow), the streaming flag (drives the loader + border tint),
//!   and the queue of finalized scrollback blocks awaiting a single
//!   `insert_before`.
//! - The **editor** ([`hand_tui::rt::components::Editor`]) lives behind its own
//!   `Arc<Mutex<…>>` (see [`SharedEditor`]) because it is both a key sink (input
//!   loop, `&mut`) and a renderer (draw closure, `&`). It is a component, not a
//!   data field, so it is not folded into `DriverState`.

use std::sync::{Arc, Mutex, MutexGuard};

use hand_tui::rt::components::Editor;
use hand_tui::rt::view::{MIN_INPUT_ROWS, TerminalSize};
use ratatui::text::Line;

use model::AssistantMessage;

use super::footer::{FooterViewModel, TokenUsageSummary};

/// The editor shared between the input loop (which dispatches keys to it) and the
/// draw closure (which renders it). Behind a blocking `Mutex`: every critical
/// section is a tiny, non-awaiting call (`handle_key`, `render`, `take_submit`),
/// so a blocking mutex is correct inside the async runtime and simplest.
pub type SharedEditor = Arc<Mutex<Editor>>;

/// The footer view-model shared into the draw closure. Rebuilt from session state
/// after each turn and session action (branch / usage / thinking-level refresh)
/// and read every frame to render the two-line bottom summary.
pub type SharedFooter = Arc<Mutex<FooterViewModel>>;

/// Mutable, `Send` driver state read by the scheduler's draw closure and mutated
/// by the input and agent tasks.
///
/// A plain `std::sync::Mutex`: the draw closure the scheduler runs is
/// synchronous, and every critical section is a small field access with no
/// `.await`, so a blocking mutex avoids the `blocking_lock`-in-async footgun and
/// is simplest.
#[derive(Debug, Default)]
pub struct DriverState {
    /// The current terminal geometry, tracked from `RtInputEvent::Resize`. Seeded
    /// at launch from the real size and overwritten whole on every resize; the
    /// draw closure lays the fixed bottom-area viewport out against it.
    pub size: TerminalSize,
    /// How many rows the input body currently occupies (1..=8). The draw closure
    /// recomputes it from the editor each frame; kept here so a resize handler
    /// and the geometry read agree on one value.
    pub input_rows: u16,
    /// Whether a turn is in flight — drives the loader row and the editor's
    /// "thinking" border tint. Set when a submit dispatches, cleared on turn end
    /// (or watchdog timeout).
    pub streaming: bool,
    /// Spinner animation phase, advanced each frame while streaming.
    pub spinner_phase: u64,
    /// Finalized chat blocks awaiting a single `insert_before` into scrollback.
    /// The draw closure drains this (via the [`HistorySink`]) *before* it redraws
    /// the viewport, honouring the "insert_before between draws" ordering.
    ///
    /// [`HistorySink`]: hand_tui::rt::history::HistorySink
    pub pending_commits: Vec<Vec<Line<'static>>>,
    /// Raw terminal control sequences awaiting a write — the OSC 133 prompt
    /// marks and OSC 9;4 progress updates that cannot ride a ratatui `Buffer`
    /// cell (they are out-of-band escapes, like the M2 image / OSC 8 channel).
    /// The draw closure drains and writes these on the terminal-owning task,
    /// inside the synchronized-output block, so invariant #1 (the scheduler owns
    /// the terminal) holds. Each entry is a complete, self-contained escape.
    pub pending_raw: Vec<&'static str>,
    /// Whether thinking blocks render collapsed (the static `Thinking...` label)
    /// rather than their full dim-italic body. Flipped globally by Ctrl+T; the
    /// draw closure reads it for the streaming preview and the commit path reads
    /// it when finalizing an assistant message.
    pub hide_thinking: bool,
    /// Raw snapshots of every assistant message committed to scrollback this
    /// session, in order. Ctrl+T re-renders all of them under the new
    /// [`hide_thinking`](DriverState::hide_thinking) state and re-commits them so
    /// the toggle takes effect globally (native scrollback is immutable, so a
    /// global flip appends the re-rendered transcript rather than rewriting it).
    pub assistant_history: Vec<AssistantMessage>,
    /// The in-flight assistant partial rendered live in the active-area preview,
    /// or `None` when no turn is streaming. Updated per streaming delta and
    /// cleared when the final snapshot commits to scrollback on `MessageEnd`.
    pub streaming_preview: Option<Vec<Line<'static>>>,
    /// The running token/cost accumulator, bumped on every `MessageEnd` and read
    /// when the footer view-model is rebuilt. It only ever grows within a session,
    /// so the footer's spend segment is monotonic across turns (VAL-CHAT-005).
    pub usage: TokenUsageSummary,
}

impl DriverState {
    /// A fresh state seeded with the real terminal geometry and a single-row
    /// input body.
    #[must_use]
    pub fn new(size: TerminalSize) -> Self {
        Self {
            size,
            input_rows: MIN_INPUT_ROWS,
            ..Self::default()
        }
    }

    /// Queue a finalized block for a single scrollback commit. Empty blocks are
    /// dropped so a no-content update never scrolls the terminal.
    pub fn queue_commit(&mut self, lines: Vec<Line<'static>>) {
        if !lines.is_empty() {
            self.pending_commits.push(lines);
        }
    }

    /// Take every queued block, clearing the queue, so each block commits exactly
    /// once.
    pub fn take_commits(&mut self) -> Vec<Vec<Line<'static>>> {
        std::mem::take(&mut self.pending_commits)
    }

    /// Queue a raw terminal control sequence (an OSC 133 mark or OSC 9;4 progress
    /// update) for a single write by the draw closure. The draw path drains it
    /// exactly once.
    pub fn queue_raw(&mut self, sequence: &'static str) {
        self.pending_raw.push(sequence);
    }

    /// Take every queued raw sequence, clearing the queue, so each is written
    /// exactly once.
    pub fn take_raw(&mut self) -> Vec<&'static str> {
        std::mem::take(&mut self.pending_raw)
    }

    /// Record an assistant message snapshot so a later global thinking-toggle
    /// (Ctrl+T) can re-render it. Called when the message finalizes into
    /// scrollback.
    pub fn remember_assistant(&mut self, message: AssistantMessage) {
        self.assistant_history.push(message);
    }

    /// Flip the global thinking-collapse state and report the new value, so the
    /// caller can re-render the transcript and emit the matching status line.
    pub fn toggle_thinking(&mut self) -> bool {
        self.hide_thinking = !self.hide_thinking;
        self.hide_thinking
    }

    /// Replace the active-area streaming preview (or clear it with `None`).
    pub fn set_streaming_preview(&mut self, preview: Option<Vec<Line<'static>>>) {
        self.streaming_preview = preview;
    }

    /// Fold one assistant message's usage into the running accumulator, returning
    /// the new total. Because it only adds, the footer's spend segment never
    /// decreases across a session.
    pub fn accumulate_usage(&mut self, usage: &model::Usage) -> TokenUsageSummary {
        super::footer::accumulate_usage(&mut self.usage, usage);
        self.usage
    }
}

/// Lock the shared driver state, treating poisoning as fatal — a poisoned lock
/// means a panic already tore through the driver and continuing would paint or
/// commit garbage.
pub fn lock_state(state: &Arc<Mutex<DriverState>>) -> MutexGuard<'_, DriverState> {
    state.lock().expect("driver state mutex poisoned")
}

/// Lock the shared editor, treating poisoning as fatal.
pub fn lock_editor(editor: &SharedEditor) -> MutexGuard<'_, Editor> {
    editor.lock().expect("editor mutex poisoned")
}

/// Lock the shared footer view-model, treating poisoning as fatal.
pub fn lock_footer(footer: &SharedFooter) -> MutexGuard<'_, FooterViewModel> {
    footer.lock().expect("footer mutex poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_commit_drops_empty_blocks() {
        let mut state = DriverState::new(TerminalSize::new(80, 24));
        state.queue_commit(Vec::new());
        assert!(state.pending_commits.is_empty());
        state.queue_commit(vec![Line::from("x")]);
        assert_eq!(state.pending_commits.len(), 1);
    }

    #[test]
    fn take_commits_drains_the_queue() {
        let mut state = DriverState::new(TerminalSize::new(80, 24));
        state.queue_commit(vec![Line::from("a")]);
        state.queue_commit(vec![Line::from("b")]);
        let taken = state.take_commits();
        assert_eq!(taken.len(), 2);
        assert!(state.pending_commits.is_empty());
    }

    #[test]
    fn queue_and_take_raw_sequences_drain_once() {
        let mut state = DriverState::new(TerminalSize::new(80, 24));
        state.queue_raw("\x1b]133;A\x07");
        state.queue_raw("\x1b]9;4;3;0\x07");
        let taken = state.take_raw();
        assert_eq!(taken, vec!["\x1b]133;A\x07", "\x1b]9;4;3;0\x07"]);
        assert!(state.pending_raw.is_empty(), "queue drained after take");
    }

    #[test]
    fn new_seeds_single_row_input_and_size() {
        let state = DriverState::new(TerminalSize::new(120, 40));
        assert_eq!(state.input_rows, MIN_INPUT_ROWS);
        assert_eq!(state.size, TerminalSize::new(120, 40));
        assert!(!state.streaming);
    }

    #[test]
    fn toggle_thinking_flips_and_reports_new_state() {
        let mut state = DriverState::new(TerminalSize::new(80, 24));
        assert!(!state.hide_thinking, "thinking starts expanded");
        assert!(state.toggle_thinking(), "first flip hides");
        assert!(state.hide_thinking);
        assert!(!state.toggle_thinking(), "second flip shows");
        assert!(!state.hide_thinking);
    }

    #[test]
    fn remember_assistant_accumulates_snapshots_in_order() {
        use model::types::{
            Api, AssistantContentBlock, AssistantMessage, Provider, StopReason, TextContent, Usage,
        };
        let make = |t: &str| AssistantMessage {
            role: "assistant".to_string(),
            content: vec![AssistantContentBlock::Text(TextContent::new(t))],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: "m".to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        };
        let mut state = DriverState::new(TerminalSize::new(80, 24));
        state.remember_assistant(make("first"));
        state.remember_assistant(make("second"));
        assert_eq!(state.assistant_history.len(), 2);
    }

    #[test]
    fn set_streaming_preview_stores_and_clears() {
        let mut state = DriverState::new(TerminalSize::new(80, 24));
        assert!(state.streaming_preview.is_none());
        state.set_streaming_preview(Some(vec![Line::from("live")]));
        assert!(state.streaming_preview.is_some());
        state.set_streaming_preview(None);
        assert!(state.streaming_preview.is_none());
    }

    #[test]
    fn accumulate_usage_grows_monotonically_across_turns() {
        use model::types::{Usage, UsageCost};
        let turn = Usage {
            input: 50,
            output: 80,
            cache_read: 5,
            cache_write: 3,
            total_tokens: 138,
            cost: UsageCost {
                total: 0.25,
                ..Default::default()
            },
        };
        let mut state = DriverState::new(TerminalSize::new(80, 24));
        let first = state.accumulate_usage(&turn);
        assert_eq!(first.input, 50);
        assert_eq!(first.output, 80);
        let second = state.accumulate_usage(&turn);
        // A second turn only adds — the totals never decrease.
        assert_eq!(second.input, 100);
        assert_eq!(second.output, 160);
        assert_eq!(second.cache_read, 10);
        assert_eq!(state.usage.input, 100, "accumulator persists on the state");
    }
}
