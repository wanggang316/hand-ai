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

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use hand_tui::rt::components::Editor;
use hand_tui::rt::view::{MIN_INPUT_ROWS, TerminalSize};
use ratatui::text::Line;

use model::AssistantMessage;

use crate::modes::interactive::theme::{Theme, ThemePalette};

use super::chrome::ProgressState;
use super::footer::{FooterViewModel, TokenUsageSummary};
use super::summary::CollapsibleSummary;

/// The name + args of an in-flight tool call, remembered from `ToolStart` so the
/// matching `ToolEnd` can render a complete state-tinted box (name, args, and
/// result together). Keyed by `tool_call_id` in [`DriverState::pending_tools`].
#[derive(Debug, Clone)]
pub struct PendingTool {
    /// The tool's name (e.g. `read`, `edit`, `bash`).
    pub name: String,
    /// The tool call's arguments as raw JSON.
    pub args: serde_json::Value,
}

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
    /// An override for the working-loader's message while streaming, or `None`
    /// for the default `Working…`. `/compact` sets `Compacting context…` for its
    /// duration so the loader names the long-running operation; cleared when the
    /// operation ends.
    pub loader_message: Option<String>,
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
    /// Whether the in-flight turn was cancelled by the user (Esc / Ctrl+C) rather
    /// than failing on its own. Set by the input loop when it cancels a streaming
    /// turn (after committing the yellow `[cancelled …]` line and clearing the
    /// loader); read by the turn runner so the cancelled turn's `send_message`
    /// error is *not* re-surfaced as a red `send failed` banner — the cancel is a
    /// clean, user-initiated stop, not a failure (VAL-CHAT-013 / VAL-CHAT-014).
    /// Reset at the start of every turn so a prior cancel never masks a genuine
    /// error on the next one.
    pub cancel_requested: bool,
    /// Whether the in-flight turn saw an in-band failure — an
    /// [`AgentSessionEvent::Error`](crate::core::agent_session::AgentSessionEvent::Error)
    /// or a `MessageEnd` whose assistant message carries
    /// [`StopReason::Error`](model::StopReason::Error). The event applier task sets
    /// it as the error lands; the turn runner reads it at the turn boundary so a
    /// turn that failed *while still returning `Ok`* (a provider stream error maps
    /// to a `StopReason::Error` assistant message, not a `send_message` `Err`) ends
    /// on the red OSC 9;4 error state rather than letting the unconditional trailing
    /// `Clear` overwrite it (VAL-CHAT-018). Reset at the start of every turn so a
    /// prior failure never masks a clean turn.
    pub turn_error: bool,
    /// In-flight tool calls, keyed by `tool_call_id`, remembered from
    /// `ToolExecutionStart` so the matching `ToolExecutionEnd` can render a
    /// complete state-tinted box (the name + args from the start, the result +
    /// error flag from the end). Cleared per entry when the tool ends.
    pub pending_tools: HashMap<String, PendingTool>,
    /// Collapsible summaries (compaction / branch / skill) committed to
    /// scrollback this session, in order. Ctrl+R flips the most-recent one's
    /// expansion state and re-commits it: native scrollback is immutable, so a
    /// toggle appends the re-rendered block (the same discipline the Ctrl+T
    /// thinking toggle uses). The expand hint on a collapsed summary is *real*
    /// because this list plus the Ctrl+R listener make it so.
    pub collapsible_summaries: Vec<CollapsibleSummary>,
    /// Whether image blocks may render as graphics this session (the
    /// `terminal.show_images` setting). Seeded at launch from settings and flipped
    /// live by the `/settings` `show_images` toggle. Read by the tool-result image
    /// path per event: `false` forces the `[mime WxH]` placeholder even on a
    /// graphics-capable terminal, so flipping it off mid-session stops all
    /// subsequent image graphics bytes and flipping it back on resumes them
    /// (VAL-IMG-011). It lives here (not on the session) so the event applier task
    /// — which has no `&session` — reads the current value directly.
    pub show_images: bool,
    /// The resolved user [`Theme`] the renderers colour with, seeded at launch
    /// from the `theme` setting via
    /// [`resolve_theme_or_default`](crate::modes::interactive::theme::resolve_theme_or_default).
    /// `None` until seeded (and in the test constructors), in which case a
    /// renderer falls back to the built-in default palette. Shared as an `Arc`
    /// so the draw closure and the event applier both read it cheaply without
    /// cloning the colour maps.
    pub theme: Option<Arc<Theme>>,
}

impl DriverState {
    /// A fresh state seeded with the real terminal geometry and a single-row
    /// input body. `show_images` defaults to `true` (the settings default); the
    /// caller overrides it from the merged settings via
    /// [`set_show_images`](DriverState::set_show_images) at launch.
    #[must_use]
    pub fn new(size: TerminalSize) -> Self {
        Self {
            size,
            input_rows: MIN_INPUT_ROWS,
            show_images: true,
            ..Self::default()
        }
    }

    /// Set whether image blocks may render as graphics (the `show_images`
    /// setting). Called at launch to seed from merged settings, and by the
    /// `/settings` toggle to flip it live so the change takes effect on the next
    /// tool result without a restart (VAL-IMG-011).
    pub fn set_show_images(&mut self, on: bool) {
        self.show_images = on;
    }

    /// Whether image blocks may currently render as graphics. `false` forces the
    /// `[mime WxH]` placeholder even on a graphics-capable terminal.
    #[must_use]
    pub fn show_images(&self) -> bool {
        self.show_images
    }

    /// Seed the resolved user theme the renderers colour with. Called at launch
    /// with the theme produced by
    /// [`resolve_theme_or_default`](crate::modes::interactive::theme::resolve_theme_or_default),
    /// so a custom palette is applied and an unknown / corrupt theme has already
    /// been folded down to the default.
    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.theme = Some(theme);
    }

    /// The active user theme, if one has been seeded. A renderer with `None`
    /// falls back to the built-in default palette.
    #[must_use]
    pub fn theme(&self) -> Option<&Arc<Theme>> {
        self.theme.as_ref()
    }

    /// The render-ready colour palette derived from the active theme. A custom
    /// theme colours the UI through this; with no theme seeded (test
    /// constructors) or a built-in theme active, it is the historical default
    /// palette, so the default look is unchanged (VAL-COMPAT-004).
    #[must_use]
    pub fn palette(&self) -> ThemePalette {
        ThemePalette::from_optional(self.theme.as_deref())
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

    /// Whether a turn is currently streaming (a loader is shown). The input loop
    /// gates Esc / Ctrl+C on this: with a turn in flight the key cancels; idle it
    /// falls through (Esc → editor; Ctrl+C → visible no-op).
    #[must_use]
    pub fn is_streaming(&self) -> bool {
        self.streaming
    }

    /// Mark the in-flight turn as user-cancelled and clear the loader in one step.
    /// Called by the input loop after it cancels the session token and commits the
    /// yellow `[cancelled …]` line, so the loader vanishes immediately and the turn
    /// runner knows to suppress the cancelled turn's error banner.
    pub fn mark_cancelled(&mut self) {
        self.cancel_requested = true;
        self.streaming = false;
    }

    /// Take and clear the user-cancel flag. The turn runner calls this when a turn
    /// ends: a `true` result means the turn's error is a cancellation (the yellow
    /// line already landed), so no red `send failed` banner is committed.
    pub fn take_cancel_requested(&mut self) -> bool {
        std::mem::take(&mut self.cancel_requested)
    }

    /// Latch that the in-flight turn saw an in-band failure. Called by the event
    /// applier when an `Error` event or a `StopReason::Error` `MessageEnd` lands,
    /// so the turn runner ends the turn on the OSC 9;4 error state.
    pub fn mark_turn_error(&mut self) {
        self.turn_error = true;
    }

    /// Take and clear the in-band-error latch. The turn runner calls this at the
    /// turn boundary: a `true` result means the turn failed in-band (a provider
    /// stream error surfaced as a `StopReason::Error` assistant message while
    /// `send_message` still returned `Ok`), so the terminal progress ends on the
    /// red error state instead of a clear.
    #[must_use]
    pub fn take_turn_error(&mut self) -> bool {
        std::mem::take(&mut self.turn_error)
    }

    /// Reset the in-band-error latch at the start of a turn so a prior failure
    /// never masks a clean turn's progress-clear.
    pub fn reset_turn_error(&mut self) {
        self.turn_error = false;
    }

    /// Queue the turn's terminal OSC 9;4 progress under a single lock: the red
    /// `Error` sequence if the in-band-error latch is set (taking it), otherwise
    /// the caller-chosen `base` (a `Clear` on success, an `Error` on a
    /// `send_message` failure / timeout). Doing the take-and-queue atomically —
    /// rather than reading the latch, then queuing in a second lock — closes the
    /// race with the event applier task, which sets the latch and queues its own
    /// `Error` on a separate task: whichever of the two locked sections runs last
    /// writes the terminal state, and both agree on `Error` once the turn failed
    /// (VAL-CHAT-018).
    pub fn queue_terminal_progress(&mut self, base: ProgressState) {
        let progress = if std::mem::take(&mut self.turn_error) {
            ProgressState::Error
        } else {
            base
        };
        self.pending_raw.push(progress.sequence());
    }

    /// Remember an in-flight tool call's name + args so the matching `ToolEnd`
    /// can render a complete box. Called on `ToolExecutionStart`.
    pub fn remember_tool(&mut self, tool_call_id: String, name: String, args: serde_json::Value) {
        self.pending_tools
            .insert(tool_call_id, PendingTool { name, args });
    }

    /// Take (and remove) the remembered name + args for a finishing tool call,
    /// if it was tracked. Called on `ToolExecutionEnd` to build the final box.
    #[must_use]
    pub fn take_tool(&mut self, tool_call_id: &str) -> Option<PendingTool> {
        self.pending_tools.remove(tool_call_id)
    }

    /// Record a collapsible summary (compaction / branch / skill) so a later
    /// Ctrl+R can flip its expansion state and re-commit it. Called when the
    /// summary first lands in scrollback (collapsed).
    pub fn remember_summary(&mut self, summary: CollapsibleSummary) {
        self.collapsible_summaries.push(summary);
    }

    /// Flip the expansion state of the most-recent collapsible summary and
    /// return a clone in its new state, or `None` when no summary has landed
    /// yet. The clone is what the caller re-commits: native scrollback is
    /// immutable, so the flip appends the re-rendered block rather than
    /// rewriting the original (the Ctrl+T pattern). Ctrl+R with no summary is a
    /// silent no-op.
    #[must_use]
    pub fn toggle_last_summary(&mut self) -> Option<CollapsibleSummary> {
        let last = self.collapsible_summaries.last_mut()?;
        last.toggle();
        Some(last.clone())
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
    fn new_defaults_show_images_on_and_toggle_flips_it() {
        // The settings default is `true`; a fresh state mirrors it so images render
        // by default (VAL-IMG-011). The toggle flips it live.
        let mut state = DriverState::new(TerminalSize::new(80, 24));
        assert!(state.show_images(), "images shown by default");
        state.set_show_images(false);
        assert!(
            !state.show_images(),
            "toggling off stops graphics mid-session"
        );
        state.set_show_images(true);
        assert!(state.show_images(), "toggling back on resumes graphics");
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
    fn toggle_last_summary_flips_only_the_most_recent_and_reports_state() {
        let mut state = DriverState::new(TerminalSize::new(80, 24));
        // No summary yet → Ctrl+R is a silent no-op.
        assert!(
            state.toggle_last_summary().is_none(),
            "no summary to toggle"
        );

        state.remember_summary(CollapsibleSummary::compaction("first", 100));
        state.remember_summary(CollapsibleSummary::compaction("second", 200));

        // Toggling flips the *last* summary, returning its new (expanded) state.
        let toggled = state.toggle_last_summary().expect("a summary to toggle");
        assert!(toggled.expanded, "first toggle expands");
        assert_eq!(toggled.summary, "second", "the most-recent summary flips");
        // The earlier summary is untouched.
        assert!(!state.collapsible_summaries[0].expanded);
        assert!(state.collapsible_summaries[1].expanded);

        // A second Ctrl+R collapses it again.
        let toggled = state.toggle_last_summary().expect("a summary to toggle");
        assert!(!toggled.expanded, "second toggle collapses");
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
