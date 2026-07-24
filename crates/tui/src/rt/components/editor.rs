//! Editor component — the rt-native multi-line text editor core.
//!
//! The rt counterpart to the legacy `EditorComponent` (`crate::components::editor`,
//! 3121 lines). Where the legacy widget renders to `Vec<String>` of ANSI-coded
//! lines and consumes raw byte events, this implements [`RtComponent`]: it paints
//! into a ratatui [`Buffer`], consumes structured [`RtKey`]s, and reports its
//! caret as a viewport-local [`Position`] so the view drives the hardware cursor
//! to the insertion point.
//!
//! # Scope — *core* only
//!
//! This feature implements the editing core and simple recall history. The
//! richer subsystems the legacy editor carries — a coalescing undo/redo stack, a
//! kill-ring, paste markers, `@`-mention / slash autocomplete — are **later
//! features**. They are not implemented here, but the seams they mount on *are*:
//!
//! - **Undo/redo.** Every mutation funnels through the small edit primitives
//!   ([`insert_str`](Editor::insert_str), [`delete_back`](Editor::delete_back),
//!   [`delete_forward`](Editor::delete_forward),
//!   [`insert_newline`](Editor::insert_newline)) and each one calls the
//!   [`record_edit`](Editor::record_edit) hook with the flat byte range and the
//!   text that changed. Today that hook is a no-op; the undo feature fills it in
//!   without touching the primitives.
//! - **Kill-ring.** Line/word kill operations will land as new primitives that
//!   push the removed span onto a ring; the [`take_killed`](Editor::take_killed)
//!   / [`set_kill`](Editor::set_kill) accessor pair is the mount point so the
//!   ring lives outside the core.
//! - **Paste markers.** [`insert_paste`](Editor::insert_paste) is the single
//!   entry point a paste event routes through; today it inserts inline, but a
//!   large paste can be diverted to an out-of-band marker there without any other
//!   call site changing.
//! - **Autocomplete.** [`context_at_cursor`](Editor::context_at_cursor) reports
//!   the token under the caret (its trigger char and prefix) so the autocomplete
//!   feature can query a provider off it; the core never blocks on a provider.
//!
//! # Editing model
//!
//! Content is a `Vec<String>` of logical lines (no trailing newlines stored); the
//! caret is a `(line, byte_col)` pair. All motion and deletion are **grapheme
//! cluster** aware via [`unicode-segmentation`](unicode_segmentation): Left /
//! Right / Backspace / Delete each move or remove exactly one extended grapheme
//! cluster, so a CJK ideograph, an emoji ZWJ sequence, a regional-indicator flag,
//! or a combining sequence is always edited as one visual unit and never sliced
//! mid-codepoint. Column widths are measured with
//! [`unicode-width`](unicode_width) so a two-cell glyph counts as two columns and
//! wrapping never leaves a half-occupied cell.
//!
//! # Wrapping, growth, and the cursor
//!
//! Each logical line is soft-wrapped to the interior width by display columns,
//! never splitting a grapheme. The bordered box **auto-grows** from
//! [`MIN_INPUT_ROWS`] to [`MAX_INPUT_ROWS`] visual rows with the content, then
//! stops and scrolls internally (the caret row is kept visible). A submit shrinks
//! it back to one row. Because the caret is tracked by walking the *same* wrap the
//! renderer uses, a resize that reflows the content leaves the caret pinned to its
//! grapheme rather than drifting — the narrow-resize stability the validator
//! probes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders, Widget};
use unicode_segmentation::UnicodeSegmentation;

use super::display_width;
use super::{Autocomplete, AutocompleteProvider};
use crate::rt::events::RtKey;
use crate::rt::view::{HandleOutcome, RtComponent};

/// The minimum number of interior text rows the box shows — a single line, so the
/// caret always has a home even on an empty buffer.
pub const MIN_INPUT_ROWS: usize = 1;

/// The maximum number of interior text rows the box auto-grows to before it stops
/// growing and scrolls its content internally. Matches the legacy 1→8 cap.
pub const MAX_INPUT_ROWS: usize = 8;

/// Maximum number of submitted prompts retained for Up/Down recall. Mirrors the
/// legacy `HISTORY_CAP`.
const HISTORY_CAP: usize = 100;

/// A paste with more than this many logical lines is folded to a
/// `[paste #N +M lines]` marker instead of landing inline. Mirrors the legacy
/// `PASTE_LINES_THRESHOLD`.
const PASTE_LINES_THRESHOLD: usize = 10;

/// A single-line paste longer than this many characters is folded to a
/// `[paste #N M chars]` marker. Mirrors the legacy `PASTE_CHARS_THRESHOLD`.
const PASTE_CHARS_THRESHOLD: usize = 1000;

/// The literal prefix every fold marker opens with. Marker detection, dense
/// renumbering, and expansion all key off this.
const PASTE_MARKER_PREFIX: &str = "[paste #";

/// Border chrome for the editor box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorBorder {
    /// A full rounded box (`╭─╮ │ ╰─╯`) with a `line:col` indicator woven into the
    /// bottom rail. This is the gallery / demo style.
    Box,
    /// No border at all: the text rows span the full area. This is the chat-input
    /// style the host wires up in a later milestone.
    None,
}

/// The visual tint applied to the box border, a hook the host drives from focus
/// and streaming state. The core never *derives* these values — it just paints
/// the one it is told to — so a later milestone can map real focus / thinking
/// state onto them without this component reaching into the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderTint {
    /// The resting, unfocused border colour.
    #[default]
    Idle,
    /// The border colour while the editor holds focus.
    Focused,
    /// The border colour while a background task is "thinking" — a distinct
    /// accent the host pulses during streaming.
    Thinking,
}

/// One reversible edit, handed to the undo seam. Carries the flat byte offset into
/// the joined buffer (`lines.join("\n")`) and the text inserted and/or removed at
/// that offset, which is everything an undo stack needs to invert the edit.
///
/// The core produces these on every mutation and passes them to
/// [`Editor::record_edit`]; the undo feature will consume them. Kept public so the
/// later feature can build its stack against this shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditRecord {
    /// Flat byte offset of the edit into `lines.join("\n")`.
    pub position: usize,
    /// Text removed at `position` (empty for a pure insert).
    pub removed: String,
    /// Text inserted at `position` (empty for a pure delete).
    pub inserted: String,
}

/// A per-editor circular buffer of killed (cut) spans, the rt-native counterpart
/// to the legacy kill ring. Scoped to a single [`Editor`] — there is no shared,
/// cross-editor ring (an informed exclusion: the hand UI never cuts from one
/// editor into another).
///
/// A kill pushes the removed span; a [`yank`](KillRing::yank) reads the most
/// recent; a [`yank_pop`](KillRing::yank_pop) walks to progressively older entries
/// and wraps around the ring. Empty spans are dropped so an empty kill never
/// pollutes the ring or shifts the yank cursor.
#[derive(Debug, Clone)]
pub struct KillRing {
    /// Killed spans, oldest first; the most recent kill is at the back.
    entries: Vec<String>,
    /// Retained-entry cap; the oldest entry is evicted past this.
    max_size: usize,
    /// The index the last yank / yank-pop landed on, or `None` when the caller has
    /// not yanked since the last kill (so a bare yank-pop is inert).
    yank_index: Option<usize>,
}

impl Default for KillRing {
    fn default() -> Self {
        Self::new(32)
    }
}

impl KillRing {
    /// A new empty ring retaining at most `max_size` (clamped to `>= 1`) entries.
    #[must_use]
    pub fn new(max_size: usize) -> Self {
        Self {
            entries: Vec::new(),
            max_size: max_size.max(1),
            yank_index: None,
        }
    }

    /// Push a killed span onto the ring, evicting the oldest entry past the cap.
    /// An empty span is ignored. Resets the yank cursor: the next yank starts from
    /// this newest entry.
    pub fn push(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.entries.push(text);
        if self.entries.len() > self.max_size {
            self.entries.remove(0);
        }
        self.yank_index = None;
    }

    /// Yank the most recent kill, arming the yank cursor for a following yank-pop.
    /// `None` when the ring is empty.
    pub fn yank(&mut self) -> Option<&str> {
        if self.entries.is_empty() {
            return None;
        }
        let idx = self.entries.len() - 1;
        self.yank_index = Some(idx);
        Some(&self.entries[idx])
    }

    /// Walk the yank cursor one entry older, wrapping around the ring. Must follow a
    /// [`yank`](KillRing::yank); `None` when the ring is empty or no yank has armed
    /// the cursor.
    pub fn yank_pop(&mut self) -> Option<&str> {
        let idx = self.yank_index?;
        if self.entries.is_empty() {
            return None;
        }
        let new_idx = if idx == 0 {
            self.entries.len() - 1
        } else {
            idx - 1
        };
        self.yank_index = Some(new_idx);
        Some(&self.entries[new_idx])
    }

    /// Reset the yank cursor so a following yank-pop is inert until the next yank.
    pub fn reset(&mut self) {
        self.yank_index = None;
    }

    /// Peek the most recent kill without arming the yank cursor. `None` on an empty
    /// ring.
    #[must_use]
    pub fn newest(&self) -> Option<&str> {
        self.entries.last().map(String::as_str)
    }

    /// Whether the ring holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The number of retained entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// One coalescable unit on the undo stack: a reversible edit whose `inserted` may
/// grow as an open typing burst absorbs adjacent single-grapheme inserts. Carries
/// the caret positions before and after the edit so undo and redo restore the
/// caret to where the user expects.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UndoUnit {
    /// Flat byte offset of the edit into `lines.join("\n")`.
    position: usize,
    /// Text removed at `position` (empty for a pure insert).
    removed: String,
    /// Text inserted at `position` (empty for a pure delete). Grows while the unit
    /// stays an open typing burst.
    inserted: String,
    /// Caret `(line, col)` before the edit — undo restores this.
    cursor_before: (usize, usize),
    /// Caret `(line, col)` after the edit — redo restores this.
    cursor_after: (usize, usize),
    /// True while this unit is an open typing burst that a following adjacent
    /// single-grapheme insert may extend. Sealed by a pause, newline, paste,
    /// delete, undo/redo, or any non-typing edit.
    open: bool,
    /// The paste registry state on the *other* side of this unit's edit, for
    /// units that mutate the registry (fold-marker creation, marker deletion).
    /// Undo and redo swap it with the live registry so text and payloads restore
    /// together. `None` for plain edits that leave the registry untouched.
    paste_registry: Option<PasteRegistrySnapshot>,
}

/// The token under the caret that an autocomplete provider would query, reported
/// by [`Editor::context_at_cursor`]. Purely descriptive — the core computes it but
/// never acts on it, leaving the query/debounce/popup to the autocomplete feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteContext {
    /// The trigger character that opened the token (`'/'` for a slash command at
    /// column 0, `'@'` for a mention).
    pub trigger: char,
    /// The text typed after the trigger, up to the caret.
    pub prefix: String,
    /// Byte column on the caret's line where the trigger sits.
    pub start_col: usize,
}

/// The out-of-band payload a fold marker stands in for. A large paste is
/// diverted here and the buffer shows only the compact `[paste #N …]` token; the
/// full text is spliced back in on submit / recall so the marker never leaks to
/// the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteContent {
    /// The marker id (1-based, dense after deletions).
    pub id: u32,
    /// The full original paste text.
    pub text: String,
    /// Logical line count of the payload (drives the `+M lines` form).
    pub line_count: usize,
    /// Character count of the payload (drives the `M chars` form).
    pub char_count: usize,
}

/// The paste registry state (markers + next id) captured before an operation
/// that mutates it, so undo/redo restore text and registry together. Swapped
/// with the live registry in either direction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PasteRegistrySnapshot {
    markers: HashMap<u32, PasteContent>,
    next_id: u32,
}

/// A transform applied to a paste payload before the fold/insert decision.
/// Returning `Some(new)` substitutes the payload; `None` keeps the original
/// verbatim. The host installs one to rewrite terminal file-drop pastes (quoted
/// / `file://` paths) into `@mention` form.
///
/// Kept `Arc`-wrapped and `Send + Sync` so a host can share one transform across
/// editors; the core only ever calls it.
pub type PasteTransform = Arc<dyn Fn(&str) -> Option<String> + Send + Sync + 'static>;

/// Build a paste transform that rewrites a dropped-file path into an `@mention`.
///
/// A drop-like paste is a single line, optionally wrapped in matching quotes and
/// optionally `file://`-prefixed. When the decoded path exists (per the injected
/// `exists` predicate, resolved against `cwd` for relative paths) it is rewritten
/// to `@<relative-or-absolute>`; otherwise the transform returns `None` and the
/// text is inserted verbatim.
///
/// Both `cwd` and the existence predicate are injected so a test drives the
/// transform deterministically without touching the real filesystem.
#[must_use]
pub fn dropped_file_mention_transform(
    cwd: PathBuf,
    exists: Arc<dyn Fn(&Path) -> bool + Send + Sync + 'static>,
) -> PasteTransform {
    Arc::new(move |raw: &str| transform_dropped_file_paste(raw, &cwd, exists.as_ref()))
}

/// A multi-line, grapheme-aware text editor painted into a ratatui buffer.
///
/// See the module docs for the editing model, growth behaviour, and the seams
/// reserved for undo / kill-ring / paste / autocomplete.
pub struct Editor {
    /// Logical lines, no trailing newlines stored. Always non-empty (one empty
    /// line for an empty buffer) so the caret always has a line.
    lines: Vec<String>,
    /// Caret line index into `lines`.
    cursor_line: usize,
    /// Caret byte column within the caret line, always on a grapheme boundary.
    cursor_col: usize,
    /// Border chrome.
    border: EditorBorder,
    /// The tint the host asked for; painted onto the border.
    tint: BorderTint,
    /// Recall history, newest first. Capped at [`HISTORY_CAP`].
    history: Vec<String>,
    /// Position in `history` while browsing: `-1` means "not browsing", `0` the
    /// newest entry, higher indices older ones.
    history_index: i32,
    /// IME preedit string shown inline at the caret while composing. Rendered
    /// underlined and excluded from the committed buffer until the platform
    /// commits it (as one [`RtInputEvent::Paste`](crate::rt::events::RtInputEvent)
    /// / multi-char string routed through [`insert_str`](Editor::insert_str)).
    composing: Option<String>,
    /// Latched submitted text, drained by [`take_submit`](Editor::take_submit).
    submitted: Option<String>,
    /// Kill-ring seam: the last killed span, set by the kill primitives and
    /// drained by [`take_killed`](Editor::take_killed). Mirrors the newest ring
    /// entry so the host can observe the last kill without reaching into the ring.
    killed: Option<String>,
    /// Per-editor kill ring backing yank / yank-pop. Not shared across editors.
    kill_ring: KillRing,
    /// Undo stack, oldest first; the most recent unit is at the back and may be an
    /// open typing burst.
    undo_stack: Vec<UndoUnit>,
    /// Redo stack, most-recently-undone first. Discarded when a fresh edit lands
    /// (typing-after-undo drops the redo branch).
    redo_stack: Vec<UndoUnit>,
    /// Caret `(line, col)` captured before the in-flight primitive mutates it, so
    /// `record_edit` can stamp the unit's `cursor_before`.
    edit_cursor_before: (usize, usize),
    /// When set, the next recorded edit is forced to seal into its own unit (never
    /// coalescing with the previous burst). Used to make a paste one atomic unit
    /// and to honour an explicit pause.
    break_coalesce: bool,
    /// Guards against `record_edit` re-entrancy while undo/redo replay a primitive:
    /// a replayed edit must not push a fresh unit.
    replaying: bool,
    /// The caret's viewport-local `(x, y)` within the last render area, recorded on
    /// [`render`](RtComponent::render) (which borrows `&self`) so
    /// [`cursor`](RtComponent::cursor) can report a width-aware position. `None`
    /// until first rendered or when the caret scrolls out of view.
    caret_cell: std::cell::Cell<Option<(u16, u16)>>,
    /// Out-of-band payloads for the fold markers currently in the buffer, keyed
    /// by marker id. Empty until a paste crosses the fold threshold.
    paste_markers: HashMap<u32, PasteContent>,
    /// The highest marker id allocated so far; the next fold takes `+ 1`.
    /// Decremented (and remaining ids densely renumbered) when a marker is
    /// deleted so ids never gap.
    next_paste_id: u32,
    /// Optional paste-payload transform, run before the fold/insert decision.
    /// The host installs one (e.g. dropped-path → `@mention`); the core only
    /// calls it.
    paste_transform: Option<PasteTransform>,
    /// The autocomplete provider the editor queries off its
    /// [`context_at_cursor`](Editor::context_at_cursor) seam. `None` until a
    /// host installs one — the editor never completes without a data source.
    autocomplete_provider: Option<Arc<dyn AutocompleteProvider>>,
    /// The live suggestion popup. Closed (empty) until a completable context
    /// under the caret yields candidates; refreshed after every buffer mutation.
    autocomplete: Autocomplete,
    /// The canonical key id that submits the buffer (default `"enter"`). The host
    /// binds this from the app-layer `Submit` action so a project `submit:
    /// alt+enter` fires and bare Enter falls back to inserting a newline. Any
    /// enter-family chord that is *not* the submit key inserts a newline instead.
    submit_key: String,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    /// A new empty editor with the box border and idle tint.
    #[must_use]
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
            border: EditorBorder::Box,
            tint: BorderTint::Idle,
            history: Vec::new(),
            history_index: -1,
            composing: None,
            submitted: None,
            killed: None,
            kill_ring: KillRing::default(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            edit_cursor_before: (0, 0),
            break_coalesce: false,
            replaying: false,
            caret_cell: std::cell::Cell::new(None),
            paste_markers: HashMap::new(),
            next_paste_id: 0,
            paste_transform: None,
            autocomplete_provider: None,
            autocomplete: Autocomplete::new(),
            submit_key: "enter".to_string(),
        }
    }

    /// Install a paste-payload transform, run before the fold/insert decision on
    /// every [`insert_paste`](Editor::insert_paste). Builder form of
    /// [`set_paste_transform`](Editor::set_paste_transform).
    #[must_use]
    pub fn with_paste_transform(mut self, transform: PasteTransform) -> Self {
        self.paste_transform = Some(transform);
        self
    }

    /// Install (or replace) the paste-payload transform.
    pub fn set_paste_transform(&mut self, transform: PasteTransform) {
        self.paste_transform = Some(transform);
    }

    /// Set the border chrome.
    #[must_use]
    pub fn border(mut self, border: EditorBorder) -> Self {
        self.border = border;
        self
    }

    /// Seed the recall history (newest first), e.g. when restoring a session.
    /// Truncated to [`HISTORY_CAP`].
    #[must_use]
    pub fn with_history(mut self, history: Vec<String>) -> Self {
        self.history = history;
        self.history.truncate(HISTORY_CAP);
        self
    }

    /// Bind the canonical key id that submits the buffer (default `"enter"`).
    /// Builder form of [`set_submit_key`](Editor::set_submit_key).
    #[must_use]
    pub fn with_submit_key(mut self, id: &str) -> Self {
        self.set_submit_key(id);
        self
    }

    /// Bind the canonical key id that submits the buffer.
    ///
    /// The host resolves this from the app-layer `Submit` action (default
    /// `"enter"`). With a custom chord (`"alt+enter"`), that chord submits and
    /// bare Enter inserts a newline instead — the single submit decision point,
    /// so the driver never has to second-guess the editor's Enter handling. Any
    /// enter-family chord that is not the submit key inserts a newline.
    pub fn set_submit_key(&mut self, id: &str) {
        self.submit_key = id.to_string();
    }

    // -----------------------------------------------------------------
    // Host-facing state (focus / thinking tint hook, submit, IME)
    // -----------------------------------------------------------------

    /// Set the border tint the host wants painted. The core does not derive this
    /// from focus/streaming itself — a later milestone maps real state onto it.
    pub fn set_tint(&mut self, tint: BorderTint) {
        self.tint = tint;
    }

    /// The tint currently painted on the border.
    #[must_use]
    pub fn tint(&self) -> BorderTint {
        self.tint
    }

    /// Set (or clear, with `None`) the IME preedit string shown inline at the
    /// caret. It renders underlined and is *not* part of the committed buffer; the
    /// platform commits it later as a multi-char string through
    /// [`insert_str`](Editor::insert_str).
    pub fn set_composition(&mut self, preedit: Option<String>) {
        self.composing = preedit.filter(|s| !s.is_empty());
    }

    /// Take the latched submitted text, clearing it. A host polls this once per
    /// frame; `None` means nothing was submitted since the last poll.
    pub fn take_submit(&mut self) -> Option<String> {
        self.submitted.take()
    }

    // -----------------------------------------------------------------
    // Content accessors
    // -----------------------------------------------------------------

    /// The full buffer text, lines joined by `\n`.
    #[must_use]
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// The caret as `(line, byte_col)`.
    #[must_use]
    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_line, self.cursor_col)
    }

    /// The number of logical lines.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Replace the buffer text, moving the caret to the end. Resets browsing state
    /// and IME. Records nothing on the undo seam — this is a programmatic replace,
    /// the counterpart the undo feature will special-case if it wants it undoable.
    pub fn set_text(&mut self, text: &str) {
        self.set_text_internal(text);
        self.history_index = -1;
    }

    /// Replace the buffer without touching `history_index`, so recall browsing
    /// survives across steps.
    fn set_text_internal(&mut self, text: &str) {
        self.lines = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(String::from).collect()
        };
        let last = self.lines.len() - 1;
        self.cursor_line = last;
        self.cursor_col = self.lines[last].len();
        self.composing = None;
    }

    // -----------------------------------------------------------------
    // History
    // -----------------------------------------------------------------

    /// Append a submitted prompt to the recall history. The text is trimmed;
    /// empty/blank text is dropped, and a consecutive duplicate of the newest
    /// entry is collapsed. Capped at [`HISTORY_CAP`], newest first.
    pub fn add_to_history(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.first().map(String::as_str) == Some(trimmed) {
            return;
        }
        self.history.insert(0, trimmed.to_string());
        self.history.truncate(HISTORY_CAP);
        self.history_index = -1;
    }

    /// Read-only view of the recall history (newest first).
    #[must_use]
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Walk one step through recall history. `direction = -1` walks to older
    /// entries (Up), `+1` back toward newer (Down). Walking forward past the newest
    /// entry restores the buffer to empty. A no-op with no history.
    fn navigate_history(&mut self, direction: i32) {
        if self.history.is_empty() {
            return;
        }
        let new_index = self.history_index - direction;
        if !(-1..self.history.len() as i32).contains(&new_index) {
            return;
        }
        self.history_index = new_index;
        if new_index == -1 {
            self.set_text_internal("");
        } else {
            let text = self.history[new_index as usize].clone();
            self.set_text_internal(&text);
        }
    }

    // -----------------------------------------------------------------
    // Seams for later features (undo / kill-ring / paste / autocomplete)
    // -----------------------------------------------------------------

    /// Undo seam. Every core mutation funnels its [`EditRecord`] here, and this
    /// builds the coalescing undo stack from it — the primitives never change.
    ///
    /// Coalescing rule, derived from the record shape plus the `break_coalesce`
    /// latch, pins the unit boundaries the contract requires:
    /// - A **single-grapheme insert** (empty `removed`, one cluster, no newline)
    ///   extends the open typing burst at the top of the stack when it sits
    ///   adjacent to it — one undo unit per burst.
    /// - A **newline**, a **paste / multi-grapheme insert**, and any **delete** or
    ///   **replace** seal a unit on their own, so undo peels them off one at a time.
    /// - An explicit **pause** ([`pause`](Editor::pause)) latches `break_coalesce`,
    ///   so the next insert starts a fresh burst even though it is a single glyph.
    ///
    /// Every recorded edit discards the redo branch: typing-after-undo cannot be
    /// redone into.
    fn record_edit(&mut self, edit: EditRecord) {
        if self.replaying {
            // An undo/redo replay drives the primitives; it must not record.
            return;
        }
        let cursor_before = self.edit_cursor_before;
        let cursor_after = (self.cursor_line, self.cursor_col);
        // A fresh edit always invalidates the redo branch.
        self.redo_stack.clear();

        let is_insert = edit.removed.is_empty() && !edit.inserted.is_empty();
        // A single-grapheme insert is typing — it opens (or extends) a burst. A
        // newline, a multi-grapheme paste, and any delete/replace are never typing.
        let is_typing =
            is_insert && edit.inserted != "\n" && edit.inserted.graphemes(true).count() == 1;
        // The break latch only forbids *merging into the previous unit*; the new
        // typing unit is still opened so the rest of the burst coalesces into it.
        let may_merge = is_typing && !self.break_coalesce;
        self.break_coalesce = false;

        if may_merge
            && let Some(prev) = self.undo_stack.last_mut()
            && prev.open
            && prev.removed.is_empty()
            && edit.position == prev.position + prev.inserted.len()
        {
            // Extend the open typing burst in place.
            prev.inserted.push_str(&edit.inserted);
            prev.cursor_after = cursor_after;
            return;
        }

        self.undo_stack.push(UndoUnit {
            position: edit.position,
            removed: edit.removed,
            inserted: edit.inserted,
            cursor_before,
            cursor_after,
            open: is_typing,
            paste_registry: None,
        });
    }

    /// Break the current typing burst so the next insert starts a new undo unit.
    /// The host calls this on an idle pause (mirroring the legacy time-window
    /// coalescing) and tests call it to pin the pause boundary deterministically.
    pub fn pause(&mut self) {
        self.break_coalesce = true;
        if let Some(unit) = self.undo_stack.last_mut() {
            unit.open = false;
        }
    }

    /// Kill-ring seam: take the last killed span, clearing it. Kill primitives set
    /// it; the host drains it here to observe the most recent kill.
    pub fn take_killed(&mut self) -> Option<String> {
        self.killed.take()
    }

    /// Kill-ring seam: stash a killed span for a later yank. Pushes onto the ring
    /// and mirrors it into the `killed` latch, so seeding the ring and observing the
    /// last kill both work.
    pub fn set_kill(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.killed = Some(text.clone());
        self.kill_ring.push(text);
    }

    /// A read-only view of the kill ring (for tests and host inspection).
    #[must_use]
    pub fn kill_ring(&self) -> &KillRing {
        &self.kill_ring
    }

    /// Paste seam: the single entry point a paste event routes through. Runs the
    /// paste pipeline and lands the result as one atomic undo unit — a paste
    /// breaks the typing burst on both sides, so it undoes in one step and the
    /// next keystroke starts fresh.
    ///
    /// The pipeline, in order:
    /// 1. **Transform.** An installed [`PasteTransform`] may rewrite the payload
    ///    (e.g. a dropped-file path → `@mention`); `None` keeps it verbatim.
    /// 2. **Defuse.** Bare ESC / CSI / other C0 control bytes are stripped so a
    ///    pasted escape sequence can never re-colour the terminal, move the
    ///    hardware cursor, or corrupt the buffer — the payload lands as inert
    ///    text.
    /// 3. **Fold decision.** A payload over [`PASTE_LINES_THRESHOLD`] lines, or a
    ///    single line over [`PASTE_CHARS_THRESHOLD`] chars, is diverted to the
    ///    registry and only a compact `[paste #N …]` marker lands inline; the
    ///    payload is spliced back on submit / recall. Otherwise the text lands
    ///    inline unchanged.
    pub fn insert_paste(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        // 1. Transform (dropped-path → @mention, etc.). A transform sees the raw
        //    payload; only the *result* is defused/folded.
        let transformed = self
            .paste_transform
            .as_ref()
            .and_then(|t| t(text))
            .unwrap_or_else(|| text.to_string());
        // 2. Defuse escape / control bytes.
        let payload = defuse_control_bytes(&transformed);
        if payload.is_empty() {
            return;
        }

        // A paste is atomic: break the burst so it is its own unit, and seal it
        // afterwards so the next single-glyph insert cannot extend it.
        self.break_coalesce = true;

        let line_count = payload.split('\n').count();
        let char_count = payload.chars().count();
        if line_count > PASTE_LINES_THRESHOLD || char_count > PASTE_CHARS_THRESHOLD {
            self.insert_fold_marker(&payload, line_count, char_count);
        } else {
            self.insert_str(&payload);
        }
        if let Some(unit) = self.undo_stack.last_mut() {
            unit.open = false;
        }
    }

    /// Divert a large paste to the registry and insert only its compact marker.
    /// The marker's undo unit is tagged with the pre-fold registry snapshot so a
    /// single undo removes the marker *and* drops the hidden payload together.
    fn insert_fold_marker(&mut self, payload: &str, line_count: usize, char_count: usize) {
        let snapshot = self.paste_registry_snapshot();
        self.next_paste_id += 1;
        let id = self.next_paste_id;
        // Line-count form wins when both thresholds trip, matching legacy.
        let marker = if line_count > PASTE_LINES_THRESHOLD {
            format!("{PASTE_MARKER_PREFIX}{id} +{line_count} lines]")
        } else {
            format!("{PASTE_MARKER_PREFIX}{id} {char_count} chars]")
        };
        self.paste_markers.insert(
            id,
            PasteContent {
                id,
                text: payload.to_string(),
                line_count,
                char_count,
            },
        );
        self.insert_str(&marker);
        // `insert_str` pushed the marker's undo unit; tag it with the pre-fold
        // registry so undoing the paste unwinds the payload and the id counter.
        if let Some(unit) = self.undo_stack.last_mut() {
            unit.paste_registry = Some(snapshot);
        }
    }

    /// A snapshot of the live paste registry (markers + next id).
    fn paste_registry_snapshot(&self) -> PasteRegistrySnapshot {
        PasteRegistrySnapshot {
            markers: self.paste_markers.clone(),
            next_id: self.next_paste_id,
        }
    }

    /// Exchange the live paste registry with `snapshot`. Undo and redo both call
    /// this: the unit holds the registry state on the *other* side of its edit, so
    /// a swap moves it in either direction.
    fn swap_paste_registry(&mut self, snapshot: &mut PasteRegistrySnapshot) {
        std::mem::swap(&mut self.paste_markers, &mut snapshot.markers);
        std::mem::swap(&mut self.next_paste_id, &mut snapshot.next_id);
    }

    /// Substitute every fold marker in `text` with its full payload. Unknown
    /// markers (no registry entry) pass through literally.
    fn expand_markers(&self, text: &str) -> String {
        expand_paste_markers(text, &self.paste_markers)
    }

    /// Read-only view of the fold-marker registry (tests / host inspection).
    #[must_use]
    pub fn paste_markers(&self) -> &HashMap<u32, PasteContent> {
        &self.paste_markers
    }

    /// The buffer with fold markers expanded to their full payloads — the form a
    /// host reads for submission. Plain [`text`](Editor::text) leaves markers
    /// compact.
    #[must_use]
    pub fn expanded_text(&self) -> String {
        self.expand_markers(&self.text())
    }

    /// Remove the fold-marker token spanning `start_col..cursor_col` on the caret
    /// line, drop its registry entry, decrement the id counter, and densely
    /// renumber the survivors (`#3` → `#2` once `#1` is gone) in both the registry
    /// and the buffer text. Recorded as one whole-buffer replace so a single undo
    /// restores text and registry together.
    fn delete_fold_marker(&mut self, start_col: usize, id: u32) {
        let removed_text = self.text();
        let snapshot = self.paste_registry_snapshot();
        let cursor_before = self.edit_cursor_before;

        // Drop the deleted marker, then shift every higher id down by one in the
        // registry (ascending order so keys never collide mid-shift).
        self.paste_markers.remove(&id);
        self.next_paste_id = self.next_paste_id.saturating_sub(1);
        let mut higher: Vec<u32> = self
            .paste_markers
            .keys()
            .copied()
            .filter(|&key| key > id)
            .collect();
        higher.sort_unstable();
        for old_id in higher {
            if let Some(mut content) = self.paste_markers.remove(&old_id) {
                content.id = old_id - 1;
                self.paste_markers.insert(old_id - 1, content);
            }
        }

        // Excise the token text, then rewrite marker ids in the buffer to match.
        self.lines[self.cursor_line].drain(start_col..self.cursor_col);
        self.cursor_col = start_col;
        for line in &mut self.lines {
            *line = renumber_paste_markers(line, id);
        }
        self.clamp_col();

        let inserted = self.text();
        let cursor_after = (self.cursor_line, self.cursor_col);
        self.redo_stack.clear();
        self.break_coalesce = true;
        self.undo_stack.push(UndoUnit {
            position: 0,
            removed: removed_text,
            inserted,
            cursor_before,
            cursor_after,
            open: false,
            paste_registry: Some(snapshot),
        });
    }

    /// The id of the live fold marker whose token the caret sits at the open
    /// bracket of, or inside of, on the caret line — for the forward-delete
    /// downgrade. `None` when the caret is not on a marker token.
    fn fold_marker_covering_forward(&self) -> Option<u32> {
        let line = &self.lines[self.cursor_line];
        marker_covering(line, self.cursor_col)
    }

    /// Downgrade a live fold marker to literal text: drop its payload from the
    /// registry (so it no longer expands on submit) while leaving the token
    /// characters in place. The buffer text is untouched, so no undo unit is
    /// pushed here — the caller's forward-delete records the actual character
    /// removal.
    fn downgrade_fold_marker(&mut self, id: u32) {
        self.paste_markers.remove(&id);
    }

    /// Autocomplete seam: the token under the caret a provider would query, or
    /// `None` when the caret is not in a completable context. A `/` at the start
    /// of any line (per-line start) opens a slash command; an `@` (preceded by
    /// start-of-line or whitespace) opens a mention. The core computes this but
    /// never queries — the autocomplete feature does.
    #[must_use]
    pub fn context_at_cursor(&self) -> Option<AutocompleteContext> {
        let line = &self.lines[self.cursor_line];
        let before = &line[..self.cursor_col];
        // Slash command: at the very start of the caret's line (per-line start).
        // A `/` mid-line is not a command — only column 0 of a line triggers it.
        if before.starts_with('/') && !before[1..].contains(char::is_whitespace) {
            return Some(AutocompleteContext {
                trigger: '/',
                prefix: before[1..].to_string(),
                start_col: 0,
            });
        }
        // Mention: an `@` at line start or after whitespace, with no whitespace in
        // the token that follows.
        if let Some(at) = before.rfind('@') {
            let boundary_ok = at == 0
                || before[..at]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_whitespace);
            let token = &before[at + 1..];
            if boundary_ok && !token.contains(char::is_whitespace) {
                return Some(AutocompleteContext {
                    trigger: '@',
                    prefix: token.to_string(),
                    start_col: at,
                });
            }
        }
        None
    }

    // -----------------------------------------------------------------
    // Autocomplete popup integration
    // -----------------------------------------------------------------

    /// Install (or replace) the autocomplete provider and refresh the popup for
    /// the current caret context. Builder form of
    /// [`set_autocomplete_provider`](Editor::set_autocomplete_provider).
    #[must_use]
    pub fn with_autocomplete_provider(mut self, provider: Arc<dyn AutocompleteProvider>) -> Self {
        self.autocomplete_provider = Some(provider);
        self.refresh_autocomplete();
        self
    }

    /// Install (or replace) the autocomplete provider and refresh the popup.
    pub fn set_autocomplete_provider(&mut self, provider: Arc<dyn AutocompleteProvider>) {
        self.autocomplete_provider = Some(provider);
        self.refresh_autocomplete();
    }

    /// A read-only view of the live suggestion popup.
    #[must_use]
    pub fn autocomplete(&self) -> &Autocomplete {
        &self.autocomplete
    }

    /// Whether the suggestion popup is currently open.
    #[must_use]
    pub fn autocomplete_visible(&self) -> bool {
        self.autocomplete.is_visible()
    }

    /// The number of candidate rows the suggestion popup wants to paint (0 when it
    /// is closed). A host that owns the surrounding box geometry — like the rt
    /// driver — reserves this many rows for the popup band so it is not clipped.
    #[must_use]
    pub fn popup_row_count(&self) -> u16 {
        if self.autocomplete.is_visible() {
            self.autocomplete.visible_rows() as u16
        } else {
            0
        }
    }

    /// Re-query the provider off the caret context and repopulate the popup.
    ///
    /// Called after every buffer mutation. When the caret is not in a
    /// completable context (no trigger, or the provider does not claim the
    /// trigger), or when the query yields no candidates, the popup is closed —
    /// so a zero-match query never leaves an empty frame, and typing past a
    /// query (e.g. a space) dismisses it.
    fn refresh_autocomplete(&mut self) {
        let Some(provider) = self.autocomplete_provider.as_ref() else {
            self.autocomplete.close();
            return;
        };
        match self.context_at_cursor() {
            Some(ctx) if provider.handles(ctx.trigger) => {
                let items = provider.query(ctx.trigger, &ctx.prefix);
                // An empty result closes the popup — no empty frame.
                self.autocomplete.set_items(items);
            }
            _ => self.autocomplete.close(),
        }
    }

    /// Accept the selected candidate: splice its insertion text over the trigger
    /// token under the caret as one undo unit, then close the popup.
    ///
    /// Tab is the *only* accept gesture (Enter submits the buffer verbatim). A
    /// no-op when the popup is closed or the caret is no longer in a completable
    /// context. Returns whether a candidate was accepted.
    fn accept_autocomplete(&mut self) -> bool {
        let Some(item) = self.autocomplete.selected().cloned() else {
            return false;
        };
        let Some(ctx) = self.context_at_cursor() else {
            self.autocomplete.close();
            return false;
        };
        // Replace the trigger token `[start_col, cursor_col)` on the caret line
        // with the candidate's insertion text, recorded as one atomic unit.
        self.replace_span(ctx.start_col, self.cursor_col, &item.insert_text);
        self.autocomplete.close();
        true
    }

    // -----------------------------------------------------------------
    // Kill-ring operations (kill / yank / yank-pop)
    // -----------------------------------------------------------------

    /// Kill the word before the caret onto the ring (Emacs `C-w`). Removes the span
    /// from the previous word start to the caret; a no-op at column 0.
    fn kill_word_backward(&mut self) {
        if self.cursor_col == 0 {
            return;
        }
        let start = self.prev_word_col();
        self.kill_span(start, self.cursor_col);
    }

    /// Kill the word after the caret onto the ring (Emacs `M-d`). Removes the span
    /// from the caret to the next word end; a no-op at end of line.
    fn kill_word_forward(&mut self) {
        let end = self.next_word_col();
        if end <= self.cursor_col {
            return;
        }
        self.kill_span(self.cursor_col, end);
    }

    /// Kill from the caret back to the line start onto the ring (Emacs `C-u`). A
    /// no-op at column 0.
    fn kill_to_line_start(&mut self) {
        if self.cursor_col == 0 {
            return;
        }
        self.kill_span(0, self.cursor_col);
    }

    /// Kill from the caret to the line end onto the ring (Emacs `C-k`). A no-op at
    /// end of line.
    fn kill_to_line_end(&mut self) {
        let line_len = self.lines[self.cursor_line].len();
        if self.cursor_col >= line_len {
            return;
        }
        self.kill_span(self.cursor_col, line_len);
    }

    /// Remove `[start, end)` on the caret line, push it onto the ring, and record a
    /// delete so the kill is undoable as one unit. Leaves the caret at `start`.
    fn kill_span(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        self.edit_cursor_before = (self.cursor_line, self.cursor_col);
        let removed: String = self.lines[self.cursor_line][start..end].to_string();
        let position = self.flat_offset(self.cursor_line, start);
        self.lines[self.cursor_line].drain(start..end);
        self.cursor_col = start;
        self.killed = Some(removed.clone());
        self.kill_ring.push(removed.clone());
        // A kill seals a fresh unit and breaks any adjacent typing burst.
        self.break_coalesce = true;
        self.record_edit(EditRecord {
            position,
            removed,
            inserted: String::new(),
        });
    }

    /// Yank the most recent kill at the caret (Emacs `C-y`). A no-op on an empty
    /// ring. The insert is one atomic undo unit so a following yank-pop can peel it.
    fn yank(&mut self) {
        let text = self.kill_ring.yank().map(str::to_string);
        if let Some(text) = text {
            self.insert_paste(&text);
        }
    }

    /// Yank-pop: replace the just-yanked span with the next-older ring entry,
    /// wrapping around (Emacs `M-y`). Requires a preceding yank; a no-op otherwise.
    /// Undoes the last yank's insert, then inserts the older entry as one unit.
    fn yank_pop(&mut self) {
        let next = self.kill_ring.yank_pop().map(str::to_string);
        let Some(next) = next else {
            return;
        };
        // Peel the previous yank's insert, then lay down the older entry. The undo
        // leaves the ring untouched (undo/redo never touch the ring), so the yank
        // cursor the pop advanced stays valid.
        self.undo();
        self.insert_paste(&next);
    }

    // -----------------------------------------------------------------
    // Undo / redo (unit-level, coalescing)
    // -----------------------------------------------------------------

    /// Undo the most recent unit, restoring the caret to where it sat before that
    /// edit. A calm no-op when the undo stack is empty. The undone unit moves to the
    /// redo stack so a redo can replay it.
    pub fn undo(&mut self) {
        let Some(mut unit) = self.undo_stack.pop() else {
            return;
        };
        self.replaying = true;
        // Invert the edit: remove what was inserted, restore what was removed.
        if !unit.inserted.is_empty() {
            self.delete_flat(unit.position, unit.inserted.len());
        }
        if !unit.removed.is_empty() {
            self.insert_flat(unit.position, &unit.removed);
        }
        self.replaying = false;
        // Restore the registry that matched the pre-edit text; the swap leaves the
        // post-edit registry in the unit so a redo can put it back.
        if let Some(snapshot) = unit.paste_registry.as_mut() {
            self.swap_paste_registry(snapshot);
        }
        let (line, col) = unit.cursor_before;
        self.cursor_line = line.min(self.lines.len() - 1);
        self.cursor_col = col.min(self.lines[self.cursor_line].len());
        self.redo_stack.push(unit);
        // Any further typing must start a fresh burst, not extend a reopened one.
        self.break_coalesce = true;
    }

    /// Redo the most recently undone unit, restoring the caret to where it sat after
    /// that edit. A calm no-op when the redo stack is empty.
    ///
    /// The hand UI binds no key to redo — it is pinned at the unit layer so the
    /// coalescing contract is fully testable without introducing a new keystroke.
    pub fn redo(&mut self) {
        let Some(mut unit) = self.redo_stack.pop() else {
            return;
        };
        self.replaying = true;
        // Replay the edit: restore what was removed's counterpart — remove the
        // original `removed`, then insert the original `inserted`.
        if !unit.removed.is_empty() {
            self.delete_flat(unit.position, unit.removed.len());
        }
        if !unit.inserted.is_empty() {
            self.insert_flat(unit.position, &unit.inserted);
        }
        self.replaying = false;
        // Re-apply the registry that matched the post-edit text; the swap leaves
        // the pre-edit registry in the unit so a later undo can restore it.
        if let Some(snapshot) = unit.paste_registry.as_mut() {
            self.swap_paste_registry(snapshot);
        }
        let (line, col) = unit.cursor_after;
        self.cursor_line = line.min(self.lines.len() - 1);
        self.cursor_col = col.min(self.lines[self.cursor_line].len());
        self.undo_stack.push(unit);
        self.break_coalesce = true;
    }

    /// Insert `text` at flat byte offset `position` without recording (undo/redo
    /// replay). Splits on `\n` into lines and leaves the caret past the insert.
    fn insert_flat(&mut self, position: usize, text: &str) {
        let (line, col) = self.unflatten(position);
        self.cursor_line = line;
        self.cursor_col = col;
        self.insert_str(text);
    }

    /// Delete `len` bytes starting at flat `position` without recording (undo/redo
    /// replay). Handles a span that crosses logical-line boundaries.
    fn delete_flat(&mut self, position: usize, len: usize) {
        if len == 0 {
            return;
        }
        let (start_line, start_col) = self.unflatten(position);
        let (end_line, end_col) = self.unflatten(position + len);
        if start_line == end_line {
            self.lines[start_line].drain(start_col..end_col);
        } else {
            let suffix = self.lines[end_line][end_col..].to_string();
            self.lines[start_line].truncate(start_col);
            self.lines[start_line].push_str(&suffix);
            self.lines.drain(start_line + 1..=end_line);
        }
        self.cursor_line = start_line;
        self.cursor_col = start_col;
    }

    /// Map a flat byte offset over `lines.join("\n")` back to `(line, col)`,
    /// clamping past-the-end offsets to the buffer end.
    fn unflatten(&self, position: usize) -> (usize, usize) {
        let mut remaining = position;
        for (i, line) in self.lines.iter().enumerate() {
            let line_len = line.len();
            if remaining <= line_len {
                return (i, remaining);
            }
            remaining -= line_len + 1; // +1 for the joining newline
        }
        let last = self.lines.len() - 1;
        (last, self.lines[last].len())
    }

    // -----------------------------------------------------------------
    // Edit primitives (each funnels through `record_edit`)
    // -----------------------------------------------------------------

    /// Insert a string at the caret, splitting on `\n` into new lines. This is the
    /// path an IME multi-char commit and a small paste both take, so a whole
    /// composed run lands atomically.
    pub fn insert_str(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.edit_cursor_before = (self.cursor_line, self.cursor_col);
        let position = self.flat_offset(self.cursor_line, self.cursor_col);
        let mut chunks = text.split('\n');
        let first = chunks.next().unwrap_or("");
        self.lines[self.cursor_line].insert_str(self.cursor_col, first);
        self.cursor_col += first.len();
        for chunk in chunks {
            let rest = self.lines[self.cursor_line][self.cursor_col..].to_string();
            self.lines[self.cursor_line].truncate(self.cursor_col);
            self.cursor_line += 1;
            self.lines
                .insert(self.cursor_line, format!("{chunk}{rest}"));
            self.cursor_col = chunk.len();
        }
        self.record_edit(EditRecord {
            position,
            removed: String::new(),
            inserted: text.to_string(),
        });
    }

    /// Replace `[start, end)` on the caret line with `text` as one atomic undo
    /// unit, leaving the caret past the inserted text.
    ///
    /// This is the autocomplete-accept primitive: it removes the trigger token
    /// (`/query` or `@query`) the caret sits in and splices the candidate's
    /// insertion text in its place. Recording the whole swap as a single
    /// replace unit is the migration fix — one undo cleanly reverts the accept
    /// (the legacy accept split it across delete+insert and corrupted the
    /// buffer). A `break_coalesce` latch on both sides seals it so a following
    /// keystroke starts a fresh burst.
    fn replace_span(&mut self, start: usize, end: usize, text: &str) {
        let line_len = self.lines[self.cursor_line].len();
        let start = start.min(line_len);
        let end = end.clamp(start, line_len);
        self.edit_cursor_before = (self.cursor_line, self.cursor_col);
        let removed: String = self.lines[self.cursor_line][start..end].to_string();
        let position = self.flat_offset(self.cursor_line, start);
        self.lines[self.cursor_line].replace_range(start..end, text);
        self.cursor_col = start + text.len();
        // An accept seals its own unit and breaks any adjacent typing burst.
        self.break_coalesce = true;
        self.record_edit(EditRecord {
            position,
            removed,
            inserted: text.to_string(),
        });
        self.break_coalesce = true;
    }

    /// Insert a soft line break at the caret (splitting the current line).
    fn insert_newline(&mut self) {
        self.edit_cursor_before = (self.cursor_line, self.cursor_col);
        let position = self.flat_offset(self.cursor_line, self.cursor_col);
        let rest = self.lines[self.cursor_line][self.cursor_col..].to_string();
        self.lines[self.cursor_line].truncate(self.cursor_col);
        self.cursor_line += 1;
        self.cursor_col = 0;
        self.lines.insert(self.cursor_line, rest);
        self.record_edit(EditRecord {
            position,
            removed: String::new(),
            inserted: "\n".to_string(),
        });
    }

    /// Delete one grapheme cluster before the caret, or join with the previous line
    /// when at column 0.
    ///
    /// Backspacing over the **closing bracket** of a live fold marker removes the
    /// whole token atomically: its payload is dropped, the remaining markers are
    /// densely renumbered so ids never gap, and a single undo restores the token
    /// and the hidden payload together.
    fn delete_back(&mut self) {
        self.edit_cursor_before = (self.cursor_line, self.cursor_col);
        if self.cursor_col > 0 {
            let before = &self.lines[self.cursor_line][..self.cursor_col];
            if let Some((start_col, id)) = paste_marker_ending_at(before)
                && self.paste_markers.contains_key(&id)
            {
                self.delete_fold_marker(start_col, id);
                return;
            }
            let line = &self.lines[self.cursor_line];
            let before = &line[..self.cursor_col];
            let cluster = before
                .grapheme_indices(true)
                .next_back()
                .map(|(_, g)| g.to_string())
                .unwrap_or_default();
            let new_col = self.cursor_col - cluster.len();
            let position = self.flat_offset(self.cursor_line, new_col);
            self.lines[self.cursor_line].drain(new_col..self.cursor_col);
            self.cursor_col = new_col;
            self.record_edit(EditRecord {
                position,
                removed: cluster,
                inserted: String::new(),
            });
        } else if self.cursor_line > 0 {
            let current = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
            let position = self.flat_offset(self.cursor_line, self.cursor_col);
            self.lines[self.cursor_line].push_str(&current);
            self.record_edit(EditRecord {
                position,
                removed: "\n".to_string(),
                inserted: String::new(),
            });
        }
    }

    /// Delete one grapheme cluster after the caret, or pull up the next line when
    /// at end of line.
    ///
    /// A forward-Delete that lands on the **open bracket** of a live fold marker,
    /// or anywhere inside its token, *downgrades* the marker to literal text
    /// rather than deleting it atomically: the payload is dropped from the
    /// registry and the `[paste #N …]` text stays in the buffer as plain
    /// characters (it no longer expands on submit). This intentional asymmetry
    /// with backspace-over-`]` is a Decision Log parity pin — forward-delete is
    /// "edit the token", not "remove the token".
    fn delete_forward(&mut self) {
        self.edit_cursor_before = (self.cursor_line, self.cursor_col);
        let line_len = self.lines[self.cursor_line].len();
        if self.cursor_col < line_len {
            if let Some(id) = self.fold_marker_covering_forward()
                && self.paste_markers.contains_key(&id)
            {
                self.downgrade_fold_marker(id);
            }
            let line = &self.lines[self.cursor_line];
            let after = &line[self.cursor_col..];
            let cluster = after
                .grapheme_indices(true)
                .next()
                .map(|(_, g)| g.to_string())
                .unwrap_or_default();
            let position = self.flat_offset(self.cursor_line, self.cursor_col);
            let end = self.cursor_col + cluster.len();
            self.lines[self.cursor_line].drain(self.cursor_col..end);
            self.record_edit(EditRecord {
                position,
                removed: cluster,
                inserted: String::new(),
            });
        } else if self.cursor_line + 1 < self.lines.len() {
            let position = self.flat_offset(self.cursor_line, self.cursor_col);
            let next = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&next);
            self.record_edit(EditRecord {
                position,
                removed: "\n".to_string(),
                inserted: String::new(),
            });
        }
    }

    // -----------------------------------------------------------------
    // Cursor motion (grapheme + word aware)
    // -----------------------------------------------------------------

    /// Move left one grapheme, wrapping to the end of the previous line at column 0.
    fn move_left(&mut self) {
        if self.cursor_col > 0 {
            let before = &self.lines[self.cursor_line][..self.cursor_col];
            if let Some((_, cluster)) = before.grapheme_indices(true).next_back() {
                self.cursor_col -= cluster.len();
            } else {
                self.cursor_col = 0;
            }
        } else if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
        }
    }

    /// Move right one grapheme, wrapping to the start of the next line at end of
    /// line.
    fn move_right(&mut self) {
        let line_len = self.lines[self.cursor_line].len();
        if self.cursor_col < line_len {
            let after = &self.lines[self.cursor_line][self.cursor_col..];
            if let Some((_, cluster)) = after.grapheme_indices(true).next() {
                self.cursor_col += cluster.len();
            } else {
                self.cursor_col = line_len;
            }
        } else if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = 0;
        }
    }

    /// Byte column at the start of the previous whitespace-delimited word.
    fn prev_word_col(&self) -> usize {
        let line = &self.lines[self.cursor_line];
        let bytes = line.as_bytes();
        let mut i = self.cursor_col;
        while i > 0 && matches!(bytes[i - 1], b' ' | b'\t') {
            i -= 1;
        }
        while i > 0 && !matches!(bytes[i - 1], b' ' | b'\t') {
            i -= 1;
        }
        while i > 0 && !line.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    /// Byte column just past the end of the next whitespace-delimited word.
    fn next_word_col(&self) -> usize {
        let line = &self.lines[self.cursor_line];
        let bytes = line.as_bytes();
        let len = line.len();
        let mut i = self.cursor_col;
        while i < len && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        while i < len && !matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        while i < len && !line.is_char_boundary(i) {
            i += 1;
        }
        i
    }

    fn move_word_left(&mut self) {
        if self.cursor_col == 0 && self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
        } else {
            self.cursor_col = self.prev_word_col();
        }
    }

    fn move_word_right(&mut self) {
        if self.cursor_col == self.lines[self.cursor_line].len() {
            if self.cursor_line + 1 < self.lines.len() {
                self.cursor_line += 1;
                self.cursor_col = 0;
            }
        } else {
            self.cursor_col = self.next_word_col();
        }
    }

    /// Move the caret up one logical line, clamping the column to a grapheme
    /// boundary on the shorter line.
    fn move_up(&mut self) {
        if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.clamp_col();
        }
    }

    /// Move the caret down one logical line, clamping the column.
    fn move_down(&mut self) {
        if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.clamp_col();
        }
    }

    /// Clamp `cursor_col` to the current line's length and snap it back to the
    /// nearest grapheme boundary if it landed mid-cluster.
    fn clamp_col(&mut self) {
        let line = &self.lines[self.cursor_line];
        if self.cursor_col > line.len() {
            self.cursor_col = line.len();
        }
        while self.cursor_col > 0 && !line.is_char_boundary(self.cursor_col) {
            self.cursor_col -= 1;
        }
    }

    /// Flatten a `(line, byte_col)` into a single byte offset over `lines.join("\n")`.
    fn flat_offset(&self, line: usize, col: usize) -> usize {
        let mut offset = 0;
        for l in &self.lines[..line] {
            offset += l.len() + 1; // +1 for the joining newline
        }
        offset + col
    }

    // -----------------------------------------------------------------
    // Submit
    // -----------------------------------------------------------------

    /// Handle a bare Enter: submit the buffer, or no-op on empty/blank content.
    ///
    /// A non-blank buffer is latched for [`take_submit`](Editor::take_submit), its
    /// trimmed form is recalled into history, and the buffer is cleared. A blank
    /// buffer is cleared without submitting or recording history (the empty-submit
    /// no-op).
    fn submit(&mut self) {
        let text = self.text();
        if text.trim().is_empty() {
            // Empty submit: clear, do not submit or record history.
            self.set_text("");
            return;
        }
        // Recall must return the *full* payload, never an orphan marker, so the
        // expanded form is what enters the history ring.
        let expanded = self.expand_markers(&text);
        self.add_to_history(&expanded);
        // Record the clear as one undoable unit so undo-after-submit restores the
        // sent text. The buffer is emptied at position 0; undo re-inserts it and
        // parks the caret at its end (cursor_before). The redo branch is discarded,
        // matching every other fresh edit.
        let cursor_before = (self.cursor_line, self.cursor_col);
        self.redo_stack.clear();
        // Capture the pre-clear registry so undo-after-submit restores the fold
        // markers *and* their payloads alongside the re-inserted text.
        let snapshot = self.paste_registry_snapshot();
        self.undo_stack.push(UndoUnit {
            position: 0,
            removed: text.clone(),
            inserted: String::new(),
            cursor_before,
            cursor_after: (0, 0),
            open: false,
            paste_registry: Some(snapshot),
        });
        self.break_coalesce = true;
        // The submitted text is the *expanded* payload — fold markers are
        // substituted back so the marker token never reaches the host / agent.
        self.submitted = Some(self.expand_markers(&text));
        self.paste_markers.clear();
        self.next_paste_id = 0;
        self.set_text("");
    }

    // -----------------------------------------------------------------
    // Wrapping and geometry (shared by render and cursor)
    // -----------------------------------------------------------------

    /// The interior text width (columns available for content) inside `area`.
    fn interior_width(&self, area: Rect) -> usize {
        match self.border {
            // Box reserves the two side rails plus a one-column pad on each side.
            EditorBorder::Box => (area.width as usize).saturating_sub(4).max(1),
            EditorBorder::None => (area.width as usize).max(1),
        }
    }

    /// Soft-wrap the whole buffer to `width` columns, producing the visual rows and
    /// the caret's visual `(row, col)`.
    ///
    /// The caret position is derived from the *same* wrap the renderer paints, so a
    /// reflow never desynchronises the two — the key to narrow-resize stability.
    fn wrap(&self, width: usize) -> WrapResult {
        let width = width.max(1);
        let mut rows: Vec<String> = Vec::new();
        let mut caret = (0usize, 0usize);
        for (li, line) in self.lines.iter().enumerate() {
            let is_caret_line = li == self.cursor_line;
            let start_row = rows.len();
            let wrapped = wrap_line_graphemes(line, width);
            // Locate the caret within this logical line's wrapped rows.
            if is_caret_line {
                caret = locate_caret(&wrapped, self.cursor_col, start_row, width);
            }
            for (sym_row, _) in wrapped {
                rows.push(sym_row);
            }
            // A logical line always contributes at least one visual row.
            if rows.len() == start_row {
                rows.push(String::new());
            }
        }
        WrapResult { rows, caret }
    }

    /// Compute the interior row count the box wants for `content_rows` wrapped rows:
    /// the content clamped into `[MIN_INPUT_ROWS, MAX_INPUT_ROWS]`.
    fn desired_rows(content_rows: usize) -> usize {
        content_rows.clamp(MIN_INPUT_ROWS, MAX_INPUT_ROWS)
    }

    /// The total row count the editor wants to occupy inside `area`: the
    /// auto-grown interior rows plus the border chrome (2 rows for a box, 0 for the
    /// borderless variant). A host uses this to size the editor's slot so the box
    /// grows and shrinks with the content; the render clamps it to `area.height`.
    #[must_use]
    pub fn desired_height(&self, area: Rect) -> u16 {
        let width = self.interior_width(area);
        let content_rows = self.wrap(width).rows.len();
        let interior = Self::desired_rows(content_rows) as u16;
        let border = match self.border {
            EditorBorder::Box => 2,
            EditorBorder::None => 0,
        };
        interior.saturating_add(border)
    }
}

/// The result of wrapping the buffer: the visual rows and the caret's `(row, col)`.
struct WrapResult {
    rows: Vec<String>,
    caret: (usize, usize),
}

/// Wrap one logical line to `width` display columns on grapheme boundaries,
/// returning each visual row's text paired with the byte offset (into the logical
/// line) at which that row starts.
fn wrap_line_graphemes(line: &str, width: usize) -> Vec<(String, usize)> {
    if line.is_empty() {
        return vec![(String::new(), 0)];
    }
    let mut rows: Vec<(String, usize)> = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    let mut row_start = 0usize;
    let mut byte = 0usize;
    for cluster in line.graphemes(true) {
        let w = display_width(cluster).max(1);
        if current_width > 0 && current_width + w > width {
            rows.push((std::mem::take(&mut current), row_start));
            current_width = 0;
            row_start = byte;
        }
        current.push_str(cluster);
        current_width += w;
        byte += cluster.len();
    }
    rows.push((current, row_start));
    rows
}

/// Locate the caret's visual `(row, col)` within one logical line's wrapped rows.
/// `caret_byte` is the caret's byte column on the logical line; `start_row` is the
/// visual-row index where this logical line begins.
fn locate_caret(
    wrapped: &[(String, usize)],
    caret_byte: usize,
    start_row: usize,
    width: usize,
) -> (usize, usize) {
    // Find the last wrapped row whose start byte is <= the caret byte; the caret
    // lives on that row, at the display width of the text between the row start and
    // the caret.
    let mut row_idx = 0usize;
    for (i, (_, row_start)) in wrapped.iter().enumerate() {
        if *row_start <= caret_byte {
            row_idx = i;
        } else {
            break;
        }
    }
    let (row_text, row_start) = &wrapped[row_idx];
    let within = caret_byte.saturating_sub(*row_start);
    let prefix = &row_text[..within.min(row_text.len())];
    let col = display_width(prefix).min(width.saturating_sub(1));
    (start_row + row_idx, col)
}

impl RtComponent for Editor {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.caret_cell.set(None);
        if area.is_empty() {
            return;
        }
        let width = self.interior_width(area);
        let WrapResult { rows, caret } = self.wrap(width);
        let interior_rows = Self::desired_rows(rows.len());

        // Auto-grow: the box occupies only the rows it needs (interior + border),
        // anchored at the top of `area`, clamped to the area height. The rows below
        // it are left untouched — under the M1 rt invariant the scheduler blanks the
        // vacated band, so a shrink leaves no ghost border in the viewport or
        // scrollback.
        let box_area = Rect {
            height: self.desired_height(area).min(area.height),
            ..area
        };

        // Scroll window: keep the caret row visible. Derived fresh from the caret
        // each frame (render is `&self`), so a resize that reflows never leaves a
        // stale offset — the freed rows repaint blank rather than ghosting.
        let scroll = compute_scroll(0, caret.0, rows.len(), interior_rows);

        let tint_style = self.tint_style();

        // Paint the border chrome and get the interior rect to lay text into.
        let interior = match self.border {
            EditorBorder::Box => {
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(tint_style);
                let inner = block.inner(box_area);
                block.render(box_area, buf);
                self.paint_indicator(box_area, buf, caret, tint_style);
                // Pad one column inside each rail (matches `interior_width`).
                Rect {
                    x: inner.x.saturating_add(1),
                    width: inner.width.saturating_sub(2),
                    ..inner
                }
            }
            EditorBorder::None => box_area,
        };

        // Paint the visible window of text rows into the interior.
        let visible = interior_rows.min(interior.height as usize);
        for r in 0..visible {
            let Some(row) = rows.get(scroll + r) else {
                break;
            };
            let y = interior.y + r as u16;
            let mut painted = row.clone();
            // Weave in the IME preedit inline at the caret row (informed exclusion:
            // the preedit is not in the buffer, so it is only *shown*, never stored).
            if let Some(preedit) = &self.composing
                && scroll + r == caret.0
            {
                painted = splice_preedit(row, caret.1, preedit);
            }
            buf.set_stringn(
                interior.x,
                y,
                &painted,
                interior.width as usize,
                Style::default(),
            );
        }

        // Record the caret position for `cursor()`, in the render area's local
        // coordinate space, when it is inside the visible window. A caret scrolled
        // out of view is left `None` (the hardware cursor hides rather than
        // stranding).
        if caret.0 >= scroll && caret.0 < scroll + visible {
            let rel_row = (caret.0 - scroll) as u16;
            // Offset the preedit width so the caret sits after any inline preedit
            // it precedes (informed exclusion: the caret follows the composition).
            let preedit_w = self.composing.as_deref().map_or(0, display_width);
            let x = interior.x + caret.1 as u16 + preedit_w as u16;
            let y = interior.y + rel_row;
            // Clamp inside the interior so a wide-glyph caret never lands off-box.
            let max_x = interior.x + interior.width.saturating_sub(1);
            self.caret_cell.set(Some((x.min(max_x), y)));
        }

        // Paint the suggestion popup in the band below the box, clamped to the
        // rows `area` actually has. When the box has grown to the cap and `area`
        // leaves no room below it, the popup gets zero rows and paints nothing —
        // every painted row stays in bounds and never overwrites a history line.
        if self.autocomplete.is_visible() {
            let below_y = box_area.y.saturating_add(box_area.height);
            let area_bottom = area.y.saturating_add(area.height);
            let avail = area_bottom.saturating_sub(below_y);
            if avail > 0 {
                let rows = (self.autocomplete.visible_rows() as u16).min(avail);
                let popup_area = Rect {
                    x: box_area.x,
                    y: below_y,
                    width: box_area.width,
                    height: rows,
                };
                self.autocomplete.render(popup_area, buf);
            }
        }
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        // When the suggestion popup is open, it captures its navigation gestures
        // before the buffer sees them: Up/Down move the indicator (never the
        // buffer caret, never recall history), Tab accepts the selection (the
        // *only* accept gesture — Enter still submits the buffer verbatim), and
        // Esc closes the popup leaving the buffer untouched. Every other key
        // falls through to the buffer, then the popup refreshes off the new
        // context.
        if let Some(id) = key.key_id.as_deref()
            && self.autocomplete.is_visible()
        {
            match id {
                "up" => {
                    self.autocomplete.select_prev();
                    return HandleOutcome::Consumed;
                }
                "down" => {
                    self.autocomplete.select_next();
                    return HandleOutcome::Consumed;
                }
                "tab" => {
                    if self.accept_autocomplete() {
                        return HandleOutcome::Consumed;
                    }
                    // Fall through only if there was nothing to accept.
                }
                "escape" => {
                    self.autocomplete.close();
                    return HandleOutcome::Consumed;
                }
                _ => {}
            }
        }
        let outcome = self.handle_key_inner(key);
        // Refresh the popup off the caret context after any buffer-mutating key.
        // Pure navigation / non-mutating keys leave the popup as-is unless they
        // moved the caret out of a completable context, which `refresh` detects.
        if self.autocomplete_provider.is_some() {
            self.refresh_autocomplete();
        }
        outcome
    }

    fn cursor(&self) -> Option<Position> {
        // The caret was recorded on the last `render` (which mirrors the wrap the
        // renderer painted), in the render area's local coordinate space; the view
        // translates it into viewport coordinates. Reporting it from the recorded
        // cell — rather than recomputing without a width — is what keeps the
        // hardware cursor on the caret's grapheme across a reflow.
        self.caret_cell.get().map(|(x, y)| Position::new(x, y))
    }
}

impl Editor {
    /// The buffer's own key handling, driven by [`handle_key`](RtComponent::handle_key)
    /// after the autocomplete popup has had first refusal on its gestures.
    fn handle_key_inner(&mut self, key: &RtKey) -> HandleOutcome {
        let Some(id) = key.key_id.as_deref() else {
            return HandleOutcome::Ignored;
        };
        match id {
            // Enter-family chords: the configured submit key submits; every other
            // enter variant inserts a newline. With the default binding
            // (`submit == enter`) bare Enter submits and alt/shift+Enter insert a
            // newline — the historical behaviour. With a custom `submit: alt+enter`
            // that chord submits and bare Enter inserts a newline instead.
            // Shift+Enter is only distinguishable from Enter under an enhanced
            // (kitty) keyboard; in plain mode it never arrives as this id.
            "alt+enter" | "shift+enter" | "enter" if id == self.submit_key => {
                // Trailing-backslash soft break: a `\` immediately before the submit
                // key is consumed and replaced with a newline, suppressing the submit.
                let line = &self.lines[self.cursor_line];
                if self.cursor_col > 0 && line.as_bytes().get(self.cursor_col - 1) == Some(&b'\\') {
                    self.delete_back();
                    self.insert_newline();
                } else {
                    self.submit();
                }
                HandleOutcome::Consumed
            }
            "alt+enter" | "shift+enter" | "enter" => {
                self.insert_newline();
                HandleOutcome::Consumed
            }
            // History navigation only at the buffer's logical edges; interior lines
            // move the caret instead.
            "up" => {
                if self.cursor_line == 0 {
                    self.navigate_history(-1);
                } else {
                    self.move_up();
                }
                HandleOutcome::Consumed
            }
            "down" => {
                if self.cursor_line + 1 == self.lines.len() {
                    self.navigate_history(1);
                } else {
                    self.move_down();
                }
                HandleOutcome::Consumed
            }
            "left" => {
                self.move_left();
                HandleOutcome::Consumed
            }
            "right" => {
                self.move_right();
                HandleOutcome::Consumed
            }
            "alt+left" | "ctrl+left" => {
                self.move_word_left();
                HandleOutcome::Consumed
            }
            "alt+right" | "ctrl+right" => {
                self.move_word_right();
                HandleOutcome::Consumed
            }
            "home" | "ctrl+a" => {
                self.cursor_col = 0;
                HandleOutcome::Consumed
            }
            "end" | "ctrl+e" => {
                self.cursor_col = self.lines[self.cursor_line].len();
                HandleOutcome::Consumed
            }
            "backspace" => {
                self.delete_back();
                HandleOutcome::Consumed
            }
            "delete" | "ctrl+d" => {
                self.delete_forward();
                HandleOutcome::Consumed
            }
            // Kill-ring: kill word / to-line-start / to-line-end, yank, yank-pop.
            "ctrl+w" | "alt+backspace" => {
                self.kill_word_backward();
                HandleOutcome::Consumed
            }
            "alt+d" | "alt+delete" => {
                self.kill_word_forward();
                HandleOutcome::Consumed
            }
            "ctrl+u" => {
                self.kill_to_line_start();
                HandleOutcome::Consumed
            }
            "ctrl+k" => {
                self.kill_to_line_end();
                HandleOutcome::Consumed
            }
            "ctrl+y" => {
                self.yank();
                HandleOutcome::Consumed
            }
            "alt+y" => {
                self.yank_pop();
                HandleOutcome::Consumed
            }
            "ctrl+z" => {
                self.undo();
                HandleOutcome::Consumed
            }
            "space" => {
                self.insert_str(" ");
                HandleOutcome::Consumed
            }
            "tab" => {
                self.insert_str("\t");
                HandleOutcome::Consumed
            }
            _ => {
                // A bare printable char (possibly with shift) inserts. A multi-char
                // id, or any modifier chord (ctrl/alt/super), is not text — let it
                // bubble so the view can act on it.
                if let Some(ch) = printable_char(id) {
                    let mut s = ch.to_string();
                    // Preserve the caret's typed case for a shifted letter.
                    if let Some(raw_ch) = raw_char(key) {
                        s = raw_ch.to_string();
                    }
                    self.insert_str(&s);
                    HandleOutcome::Consumed
                } else {
                    HandleOutcome::Ignored
                }
            }
        }
    }
}

impl Editor {
    /// The border style for the current tint.
    fn tint_style(&self) -> Style {
        match self.tint {
            BorderTint::Idle => Style::default().fg(Color::DarkGray),
            BorderTint::Focused => Style::default().fg(Color::Cyan),
            BorderTint::Thinking => Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        }
    }

    /// Weave the `line:col` indicator into the bottom border rail of a box.
    fn paint_indicator(&self, area: Rect, buf: &mut Buffer, caret: (usize, usize), style: Style) {
        if area.height < 2 || area.width < 6 {
            return;
        }
        // 1-based logical line:col for the indicator (visual col is the caret's
        // wrapped column, which reads as the user expects for CJK/emoji).
        let info = format!(" {}:{} ", self.cursor_line + 1, caret.1 + 1);
        let info_w = display_width(&info);
        let bottom_y = area.y + area.height - 1;
        // Place the indicator centred on the bottom rail, inside the corners.
        let inner_w = area.width as usize - 2;
        if info_w >= inner_w {
            return;
        }
        let left_pad = (inner_w - info_w) / 2;
        let x = area.x + 1 + left_pad as u16;
        buf.set_stringn(x, bottom_y, &info, info_w, style);
    }
}

/// Compute the scroll window top so the caret row stays visible within `visible`
/// interior rows. Starts from `prev` and only moves as far as needed, then clamps.
fn compute_scroll(prev: usize, caret_row: usize, total_rows: usize, visible: usize) -> usize {
    let mut top = prev;
    if caret_row < top {
        top = caret_row;
    } else if visible > 0 && caret_row >= top + visible {
        top = caret_row + 1 - visible;
    }
    // Never scroll past the last full window.
    let max_top = total_rows.saturating_sub(visible);
    top.min(max_top)
}

/// Splice an IME preedit into `row` at display column `col`, so it renders inline
/// at the caret without being part of the committed buffer.
fn splice_preedit(row: &str, col: usize, preedit: &str) -> String {
    // Find the byte offset in `row` at display column `col`.
    let mut used = 0usize;
    let mut byte = row.len();
    for (i, cluster) in row.grapheme_indices(true) {
        if used >= col {
            byte = i;
            break;
        }
        used += display_width(cluster).max(1);
    }
    let mut out = String::with_capacity(row.len() + preedit.len());
    out.push_str(&row[..byte]);
    out.push_str(preedit);
    out.push_str(&row[byte..]);
    out
}

/// The single printable character a key id denotes, or `None` for named keys,
/// modifier chords, or multi-char ids. A shift-only prefix is stripped (the char
/// still carries its case via [`raw_char`]).
fn printable_char(id: &str) -> Option<char> {
    let bare = id.strip_prefix("shift+").unwrap_or(id);
    // A modifier chord that survived the strip (ctrl/alt/super) is not text.
    if bare.contains('+') {
        return None;
    }
    let mut chars = bare.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        // A multi-char id (a named key like "left", "home") is not text.
        return None;
    }
    Some(ch)
}

/// The literal character crossterm reported for a key, when it is a `Char`. Used to
/// preserve the exact case/glyph the user typed (a shifted letter arrives uppercase).
fn raw_char(key: &RtKey) -> Option<char> {
    match key.raw.code {
        crossterm::event::KeyCode::Char(c) => Some(c),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Paste pipeline free functions (escape defusing, marker parsing/renumber/expand,
// dropped-file transform). Kept free so the pure logic is unit-testable in
// isolation from an `Editor`.
// ---------------------------------------------------------------------------

/// Strip bare escape / control bytes from a pasted payload so a pasted terminal
/// sequence cannot re-colour the screen, move the hardware cursor, or corrupt the
/// buffer — it lands as inert text instead.
///
/// Dropped: every C0 control (`0x00..=0x1F`) and DEL (`0x7F`) *except* the two
/// whitespace controls the editor treats as real content — `\n` (line breaks a
/// multi-line paste keeps) and `\t` (a literal tab). ESC (`0x1B`), the CSI/OSC
/// introducers built on it, and stray carriage returns all vanish. A lone `\r` is
/// dropped rather than kept so a CRLF payload does not leave dangling carriage
/// returns; `\r\n` collapses to `\n`.
fn defuse_control_bytes(text: &str) -> String {
    text.chars()
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect()
}

/// If the text-before-cursor ends with a complete fold-marker token, return the
/// token's start byte column on the line and its id. `None` when the char before
/// the cursor is not a marker's closing bracket.
fn paste_marker_ending_at(before: &str) -> Option<(usize, u32)> {
    if !before.ends_with(']') {
        return None;
    }
    let start = before.rfind(PASTE_MARKER_PREFIX)?;
    let token = &before[start..];
    // An embedded ']' means the trailing bracket closes something else.
    if token[..token.len() - 1].contains(']') {
        return None;
    }
    let body = &token[PASTE_MARKER_PREFIX.len()..token.len() - 1];
    let digit_end = body
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(body.len());
    body[..digit_end].parse().ok().map(|id| (start, id))
}

/// The id of the fold-marker token on `line` that the byte column `col` falls
/// on the open bracket of, or strictly inside of. `None` when `col` is not on a
/// marker token. Used for the forward-delete downgrade: deleting at `[` or inside
/// the token turns it literal.
fn marker_covering(line: &str, col: usize) -> Option<u32> {
    let mut search_from = 0;
    while let Some(rel) = line[search_from..].find(PASTE_MARKER_PREFIX) {
        let start = search_from + rel;
        let after = &line[start..];
        let rel_end = after.find(']')?;
        let end = start + rel_end; // byte index of the ']'
        // The caret covers the token when it sits at the open bracket or strictly
        // inside (up to and including the char just before ']'); at or past the
        // ']' the token is already whole behind the caret.
        if col >= start && col <= end {
            let body = &after[PASTE_MARKER_PREFIX.len()..rel_end];
            let digit_end = body
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(body.len());
            if let Ok(id) = body[..digit_end].parse::<u32>() {
                return Some(id);
            }
        }
        search_from = end + 1;
    }
    None
}

/// Rewrite marker tokens in `line`, decrementing every id greater than
/// `removed_id` by one. Non-marker text and markers at or below `removed_id` pass
/// through untouched.
fn renumber_paste_markers(line: &str, removed_id: u32) -> String {
    if !line.contains(PASTE_MARKER_PREFIX) {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find(PASTE_MARKER_PREFIX) {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let Some(end) = after.find(']') else {
            out.push_str(after);
            return out;
        };
        let token = &after[..=end];
        let body = &token[PASTE_MARKER_PREFIX.len()..];
        let digit_end = body
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(body.len());
        match body[..digit_end].parse::<u32>() {
            Ok(id) if id > removed_id => {
                out.push_str(PASTE_MARKER_PREFIX);
                out.push_str(&(id - 1).to_string());
                out.push_str(&body[digit_end..]);
            }
            _ => out.push_str(token),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Substitute every fold-marker token in `text` with its full payload. An
/// unknown marker (no registry entry) is emitted literally.
fn expand_paste_markers(text: &str, markers: &HashMap<u32, PasteContent>) -> String {
    if markers.is_empty() || !text.contains(PASTE_MARKER_PREFIX) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(PASTE_MARKER_PREFIX) {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let Some(end) = after.find(']') else {
            out.push_str(after);
            return out;
        };
        let token = &after[..=end];
        let id_str: String = token
            .chars()
            .skip(PASTE_MARKER_PREFIX.chars().count())
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(id) = id_str.parse::<u32>()
            && let Some(content) = markers.get(&id)
        {
            out.push_str(&content.text);
        } else {
            // Unknown marker — emit it literally.
            out.push_str(token);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Rewrite a single-line dropped-file paste into an `@mention`, or `None` to
/// insert it verbatim.
///
/// Mirrors the coding-agent driver's `transform_dropped_file_paste` but takes the
/// cwd and an existence predicate as parameters so it stays pure and testable. A
/// drop-like paste is a single non-empty line, optionally in matching outer
/// quotes and optionally `file://`-prefixed (percent-decoded). When the resolved
/// path exists, the mention prefers the cwd-relative form, falling back to the
/// absolute path.
fn transform_dropped_file_paste(
    raw: &str,
    cwd: &Path,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<String> {
    // Drop-like pastes are single-line; multi-line pastes are bracketed and
    // never come from a drag-drop.
    if raw.contains('\n') {
        return None;
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let stripped = strip_matching_quotes(trimmed);
    let no_scheme = stripped.strip_prefix("file://").unwrap_or(stripped);
    let decoded = percent_decode(no_scheme);
    let candidate = PathBuf::from(decoded.as_ref());
    let resolved = if candidate.is_absolute() {
        candidate.clone()
    } else {
        cwd.join(&candidate)
    };
    if !exists(&resolved) {
        return None;
    }
    let mention = if let Ok(rel) = resolved.strip_prefix(cwd) {
        rel.to_string_lossy().to_string()
    } else {
        resolved.to_string_lossy().to_string()
    };
    Some(format!("@{mention}"))
}

/// Strip one layer of matching outer quotes (`"…"` or `'…'`) from `s`.
fn strip_matching_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Best-effort percent-decode (`%20` → space), for `file://`-encoded paths.
fn percent_decode(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains('%') {
        return std::borrow::Cow::Borrowed(s);
    }
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &s[i + 1..i + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(id: &str, code: KeyCode, mods: KeyModifiers) -> RtKey {
        RtKey {
            key_id: Some(id.to_string()),
            raw: KeyEvent::new(code, mods),
        }
    }

    fn ch(c: char) -> RtKey {
        key(&c.to_string(), KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn type_str(ed: &mut Editor, s: &str) {
        for c in s.chars() {
            ed.handle_key(&ch(c));
        }
    }

    #[test]
    fn typing_appends_chars() {
        let mut ed = Editor::new();
        type_str(&mut ed, "hello");
        assert_eq!(ed.text(), "hello");
        assert_eq!(ed.cursor(), (0, 5));
    }

    #[test]
    fn default_submit_key_alt_enter_inserts_newline_not_submit() {
        // Default binding (submit == enter): alt+enter inserts a newline and does
        // not submit, so a multi-line message can be composed before Enter sends it.
        let mut ed = Editor::new();
        type_str(&mut ed, "ab");
        ed.handle_key(&key("alt+enter", KeyCode::Enter, KeyModifiers::ALT));
        type_str(&mut ed, "cd");
        assert_eq!(ed.text(), "ab\ncd");
        assert_eq!(ed.line_count(), 2);
        assert!(ed.take_submit().is_none());
    }

    #[test]
    fn custom_submit_key_alt_enter_submits() {
        // With `submit: alt+enter`, that chord submits the buffer verbatim.
        let mut ed = Editor::new().with_submit_key("alt+enter");
        type_str(&mut ed, "ab");
        ed.handle_key(&key("alt+enter", KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(ed.take_submit().as_deref(), Some("ab"));
        assert_eq!(ed.text(), "", "buffer cleared after submit");
    }

    #[test]
    fn custom_submit_key_bare_enter_inserts_newline() {
        // When submit is bound to alt+enter, bare Enter stops submitting and
        // inserts a newline instead (the non-submit enter variant).
        let mut ed = Editor::new().with_submit_key("alt+enter");
        type_str(&mut ed, "ab");
        ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
        type_str(&mut ed, "cd");
        assert_eq!(ed.text(), "ab\ncd");
        assert_eq!(ed.line_count(), 2);
        assert!(ed.take_submit().is_none());
    }

    #[test]
    fn set_submit_key_reapplies_binding() {
        // The setter mirrors the builder so `/reload` can re-point the submit key
        // on the live editor: bare Enter stops submitting (inserts a newline) and
        // the new chord submits.
        let mut ed = Editor::new();
        ed.set_submit_key("alt+enter");
        type_str(&mut ed, "a");
        ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
        assert!(ed.take_submit().is_none(), "bare Enter no longer submits");
        type_str(&mut ed, "b");
        ed.handle_key(&key("alt+enter", KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(ed.take_submit().as_deref(), Some("a\nb"));
    }

    /// Render `area` into a fresh buffer and collect each row as a trimmed String.
    fn render_rows(ed: &Editor, area: Rect) -> Vec<String> {
        let mut buf = Buffer::empty(area);
        RtComponent::render(ed, area, &mut buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(area.x + x, area.y + y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    fn mention_provider(paths: &[&str]) -> Arc<dyn AutocompleteProvider> {
        use crate::rt::components::{PathEntry, PathProvider};
        Arc::new(PathProvider::new(
            paths.iter().map(|p| PathEntry::file(*p)).collect(),
        ))
    }

    #[test]
    fn popup_paints_candidate_rows_when_given_room() {
        // The band below the box has rows to spare, so an active @-context paints
        // its candidate labels — the driver-facing accessors report the popup and
        // its row count for reserving the band.
        let mut ed = Editor::new()
            .border(EditorBorder::None)
            .with_autocomplete_provider(mention_provider(&["README.md", "main.rs"]));
        type_str(&mut ed, "@RE");
        assert!(ed.autocomplete_visible(), "the @-context opens the popup");
        assert!(ed.popup_row_count() >= 1, "at least one candidate row");

        // A box of one editor row plus rows to spare below paints the popup band.
        let area = Rect::new(0, 0, 40, 6);
        let rows = render_rows(&ed, area);
        assert!(
            rows.iter().any(|r| r.contains("README.md")),
            "popup paints the candidate: {rows:?}"
        );
    }

    #[test]
    fn popup_paints_nothing_when_area_leaves_no_room_below_box() {
        // A one-row area is entirely the editor box; the below-box band is empty,
        // so the self-render paints no popup rows (the defect the driver fixes by
        // reserving the band itself).
        let mut ed = Editor::new()
            .border(EditorBorder::None)
            .with_autocomplete_provider(mention_provider(&["README.md"]));
        type_str(&mut ed, "@RE");
        assert!(ed.autocomplete_visible());

        let area = Rect::new(0, 0, 40, 1);
        let rows = render_rows(&ed, area);
        assert!(
            !rows.iter().any(|r| r.contains("README.md")),
            "no room below the box → no popup painted: {rows:?}"
        );
    }

    #[test]
    fn shift_enter_inserts_newline_under_enhanced() {
        let mut ed = Editor::new();
        type_str(&mut ed, "ab");
        ed.handle_key(&key("shift+enter", KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(ed.text(), "ab\n");
        assert!(ed.take_submit().is_none());
    }

    #[test]
    fn trailing_backslash_then_enter_inserts_newline() {
        let mut ed = Editor::new();
        type_str(&mut ed, "ab\\");
        ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(ed.text(), "ab\n", "backslash consumed, newline inserted");
        assert!(ed.take_submit().is_none(), "submit suppressed");
    }

    #[test]
    fn enter_submits_and_clears_and_recalls() {
        let mut ed = Editor::new();
        type_str(&mut ed, "  hello world  ");
        ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(ed.take_submit().as_deref(), Some("  hello world  "));
        assert_eq!(ed.text(), "", "buffer cleared after submit");
        assert_eq!(
            ed.history(),
            &["hello world".to_string()],
            "trimmed form recalled"
        );
    }

    #[test]
    fn empty_submit_is_noop() {
        let mut ed = Editor::new();
        type_str(&mut ed, "   ");
        ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
        assert!(ed.take_submit().is_none(), "blank does not submit");
        assert!(ed.history().is_empty(), "blank not recalled");
        assert_eq!(ed.text(), "", "buffer cleared");
    }

    #[test]
    fn history_dedups_consecutive_and_caps() {
        let mut ed = Editor::new();
        for _ in 0..2 {
            type_str(&mut ed, "same");
            ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
        }
        assert_eq!(ed.history().len(), 1, "consecutive duplicate stored once");
    }

    #[test]
    fn grapheme_backspace_deletes_whole_cluster() {
        // A ZWJ family emoji is one cluster.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let mut ed = Editor::new();
        ed.insert_str(&format!("a{family}b"));
        // Caret at end; delete 'b', then the whole family cluster.
        ed.handle_key(&key("backspace", KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(ed.text(), format!("a{family}"));
        ed.handle_key(&key("backspace", KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(ed.text(), "a", "whole ZWJ cluster removed as a unit");
    }

    #[test]
    fn grapheme_left_right_move_by_cluster() {
        let flag = "\u{1F1EF}\u{1F1F5}"; // regional-indicator flag (JP), one cluster
        let mut ed = Editor::new();
        ed.insert_str(&format!("x{flag}y"));
        ed.cursor_col = 0;
        ed.cursor_line = 0;
        ed.handle_key(&key("right", KeyCode::Right, KeyModifiers::NONE)); // past 'x'
        assert_eq!(ed.cursor(), (0, 1));
        ed.handle_key(&key("right", KeyCode::Right, KeyModifiers::NONE)); // past flag
        assert_eq!(
            ed.cursor(),
            (0, 1 + flag.len()),
            "flag jumped as one cluster"
        );
        ed.handle_key(&key("left", KeyCode::Left, KeyModifiers::NONE)); // back over flag
        assert_eq!(ed.cursor(), (0, 1));
    }

    #[test]
    fn left_at_col_zero_wraps_to_prev_line_end() {
        let mut ed = Editor::new();
        ed.insert_str("ab\ncd");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.handle_key(&key("left", KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(ed.cursor(), (0, 2), "wrapped to end of previous line");
    }

    #[test]
    fn cjk_multichar_ime_commit_lands_whole() {
        let mut ed = Editor::new();
        // A multi-char IME commit routed as one insert.
        ed.insert_str("你好世界");
        assert_eq!(ed.text(), "你好世界");
        assert_eq!(ed.cursor(), (0, "你好世界".len()));
    }

    #[test]
    fn history_up_recalls_on_first_line_only() {
        let mut ed = Editor::new();
        type_str(&mut ed, "first");
        ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
        ed.take_submit();
        // Empty buffer, first line: Up recalls history.
        ed.handle_key(&key("up", KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(ed.text(), "first");
    }

    #[test]
    fn interior_up_down_moves_cursor_not_history() {
        let mut ed = Editor::new();
        type_str(&mut ed, "recalled");
        ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
        ed.take_submit();
        // A multi-line draft.
        ed.insert_str("l1\nl2\nl3");
        // Caret on last line; Up moves the caret to interior lines, not history.
        ed.cursor_line = 2;
        ed.cursor_col = 2;
        ed.handle_key(&key("up", KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(ed.cursor().0, 1, "interior Up moved caret, not history");
        assert_eq!(ed.text(), "l1\nl2\nl3", "draft untouched");
        // On the first line, Up recalls history and replaces the draft.
        ed.cursor_line = 0;
        ed.handle_key(&key("up", KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(ed.text(), "recalled");
    }

    #[test]
    fn down_past_newest_restores_empty() {
        let mut ed = Editor::new();
        type_str(&mut ed, "entry");
        ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
        ed.take_submit();
        ed.handle_key(&key("up", KeyCode::Up, KeyModifiers::NONE)); // recall "entry"
        assert_eq!(ed.text(), "entry");
        ed.handle_key(&key("down", KeyCode::Down, KeyModifiers::NONE)); // past newest
        assert_eq!(ed.text(), "", "Down past newest restores empty buffer");
    }

    #[test]
    fn auto_grow_row_count() {
        // desired_rows clamps content rows into [1, 8].
        assert_eq!(Editor::desired_rows(0), 1, "empty still shows one row");
        assert_eq!(Editor::desired_rows(1), 1);
        assert_eq!(Editor::desired_rows(5), 5);
        assert_eq!(Editor::desired_rows(8), 8);
        assert_eq!(Editor::desired_rows(20), 8, "capped at 8");
    }

    #[test]
    fn submit_shrinks_back_to_one_row() {
        let mut ed = Editor::new();
        ed.insert_str("a\nb\nc\nd");
        let width = 40;
        assert!(ed.wrap(width).rows.len() >= 4);
        ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            Editor::desired_rows(ed.wrap(width).rows.len()),
            1,
            "shrinks to one row after submit"
        );
    }

    #[test]
    fn long_line_wraps_within_cap_and_caret_visible() {
        let mut ed = Editor::new();
        let long = "x".repeat(5000);
        ed.insert_str(&long);
        let width = 40;
        let WrapResult { rows, caret } = ed.wrap(width);
        assert_eq!(rows.len(), (5000usize).div_ceil(40), "wrapped by width");
        // Caret at end sits on the last wrapped row.
        assert_eq!(caret.0, rows.len() - 1);
        // The auto-grow row count is capped at 8, the box does not blow up.
        assert_eq!(Editor::desired_rows(rows.len()), MAX_INPUT_ROWS);
    }

    #[test]
    fn wide_char_wrap_does_not_split_cell() {
        // A line of CJK (2 cols each) into a width of 5: each row holds two glyphs
        // (4 cols) — the 5th col cannot fit a third 2-col glyph, so no half cell.
        let mut ed = Editor::new();
        ed.insert_str("字字字字");
        let WrapResult { rows, .. } = ed.wrap(5);
        for row in &rows {
            assert!(display_width(row) <= 5, "row never overflows the width");
        }
        assert_eq!(rows.len(), 2, "two glyphs per 5-col row");
    }

    #[test]
    fn narrow_resize_keeps_caret_on_its_grapheme() {
        let mut ed = Editor::new();
        ed.insert_str("hello world this is a long line");
        // Put the caret in the middle, on a known grapheme boundary.
        ed.cursor_col = "hello world ".len();
        let wide = ed.wrap(100);
        let narrow = ed.wrap(10);
        // At width 100 the whole line is one row; at 10 it reflows to several.
        assert_eq!(wide.rows.len(), 1);
        assert!(narrow.rows.len() > 1, "narrow reflows to multiple rows");
        // The caret still points at the same grapheme in both wraps: the char at
        // its byte column is unchanged (`this` begins right after `hello world `).
        let line = &ed.lines[0];
        assert_eq!(&line[ed.cursor_col..ed.cursor_col + 1], "t");
    }

    #[test]
    fn word_jump_moves_by_word() {
        let mut ed = Editor::new();
        ed.insert_str("alpha beta gamma");
        ed.cursor_col = 0;
        ed.handle_key(&key("alt+right", KeyCode::Right, KeyModifiers::ALT));
        assert_eq!(ed.cursor().1, "alpha".len());
        ed.handle_key(&key("alt+right", KeyCode::Right, KeyModifiers::ALT));
        assert_eq!(ed.cursor().1, "alpha beta".len());
        ed.handle_key(&key("alt+left", KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(ed.cursor().1, "alpha ".len());
    }

    #[test]
    fn home_end_move_to_line_edges() {
        let mut ed = Editor::new();
        ed.insert_str("abcdef");
        ed.handle_key(&key("home", KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(ed.cursor().1, 0);
        ed.handle_key(&key("end", KeyCode::End, KeyModifiers::NONE));
        assert_eq!(ed.cursor().1, 6);
    }

    #[test]
    fn delete_forward_and_line_join() {
        let mut ed = Editor::new();
        ed.insert_str("ab\ncd");
        ed.cursor_line = 0;
        ed.cursor_col = 2; // end of "ab"
        ed.handle_key(&key("delete", KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(ed.text(), "abcd", "delete at eol joins next line");
    }

    #[test]
    fn tint_hook_roundtrips() {
        let mut ed = Editor::new();
        assert_eq!(ed.tint(), BorderTint::Idle);
        ed.set_tint(BorderTint::Focused);
        assert_eq!(ed.tint(), BorderTint::Focused);
        ed.set_tint(BorderTint::Thinking);
        assert_eq!(ed.tint(), BorderTint::Thinking);
    }

    #[test]
    fn autocomplete_context_slash_and_mention() {
        let mut ed = Editor::new();
        ed.insert_str("/hel");
        let ctx = ed.context_at_cursor().expect("slash context");
        assert_eq!(ctx.trigger, '/');
        assert_eq!(ctx.prefix, "hel");

        let mut ed2 = Editor::new();
        ed2.insert_str("see @fil");
        let ctx2 = ed2.context_at_cursor().expect("mention context");
        assert_eq!(ctx2.trigger, '@');
        assert_eq!(ctx2.prefix, "fil");
    }

    #[test]
    fn seams_are_reachable() {
        let mut ed = Editor::new();
        ed.set_kill("cut");
        assert_eq!(ed.take_killed().as_deref(), Some("cut"));
        assert!(ed.take_killed().is_none());
        ed.insert_paste("pasted");
        assert_eq!(ed.text(), "pasted");
        ed.set_composition(Some("preedit".to_string()));
        // A composition does not enter the buffer.
        assert_eq!(ed.text(), "pasted");
    }

    // --- KillRing pure logic -------------------------------------------------

    #[test]
    fn kill_ring_push_yank_and_yank_pop_wrap() {
        let mut ring = KillRing::new(10);
        assert!(ring.is_empty());
        ring.push("first".to_string());
        ring.push("second".to_string());
        ring.push("third".to_string());
        assert_eq!(ring.len(), 3);
        // Yank reads the newest, then yank-pop walks older and wraps around.
        assert_eq!(ring.yank(), Some("third"));
        assert_eq!(ring.yank_pop(), Some("second"));
        assert_eq!(ring.yank_pop(), Some("first"));
        assert_eq!(ring.yank_pop(), Some("third"), "wraps back to newest");
    }

    #[test]
    fn kill_ring_empty_push_ignored_and_bare_pop_inert() {
        let mut ring = KillRing::new(10);
        ring.push(String::new());
        assert!(ring.is_empty(), "empty span never enters the ring");
        assert!(ring.yank().is_none(), "yank on empty ring is None");
        ring.push("x".to_string());
        // A push resets the yank cursor, so a bare yank-pop (no preceding yank)
        // is inert.
        assert!(ring.yank_pop().is_none(), "pop without yank is inert");
    }

    #[test]
    fn kill_ring_evicts_oldest_past_cap() {
        let mut ring = KillRing::new(2);
        ring.push("a".to_string());
        ring.push("b".to_string());
        ring.push("c".to_string());
        assert_eq!(ring.len(), 2, "capped");
        assert_eq!(ring.yank(), Some("c"));
        assert_eq!(ring.yank_pop(), Some("b"), "oldest 'a' was evicted");
    }

    // --- Editor kill / yank / yank-pop --------------------------------------

    #[test]
    fn kill_word_backward_pushes_and_yank_reinserts() {
        let mut ed = Editor::new();
        ed.insert_str("alpha beta");
        ed.handle_key(&key("ctrl+w", KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(ed.text(), "alpha ", "word killed to caret");
        assert_eq!(
            ed.kill_ring().newest(),
            Some("beta"),
            "kill pushed onto ring"
        );
        // Yank re-inserts the killed span at the caret.
        ed.handle_key(&key("ctrl+y", KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(ed.text(), "alpha beta", "yank restored the killed word");
    }

    #[test]
    fn kill_to_line_start_and_end() {
        let mut ed = Editor::new();
        ed.insert_str("hello world");
        // Caret at end; kill to line start removes everything.
        ed.handle_key(&key("ctrl+u", KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(ed.text(), "");
        assert_eq!(ed.kill_ring().newest(), Some("hello world"));

        let mut ed2 = Editor::new();
        ed2.insert_str("hello world");
        ed2.cursor_col = "hello ".len();
        ed2.handle_key(&key("ctrl+k", KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(ed2.text(), "hello ", "killed to line end");
        assert_eq!(ed2.kill_ring().newest(), Some("world"));
    }

    #[test]
    fn yank_pop_cycles_through_ring() {
        let mut ed = Editor::new();
        // Kill two words so the ring has [beta, gamma] (gamma newest).
        ed.insert_str("beta");
        ed.handle_key(&key("ctrl+u", KeyCode::Char('u'), KeyModifiers::CONTROL));
        ed.insert_str("gamma");
        ed.handle_key(&key("ctrl+u", KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(ed.text(), "");
        // Yank newest, then yank-pop replaces it with the older entry.
        ed.handle_key(&key("ctrl+y", KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(ed.text(), "gamma");
        ed.handle_key(&key("alt+y", KeyCode::Char('y'), KeyModifiers::ALT));
        assert_eq!(ed.text(), "beta", "yank-pop swapped in the older kill");
        ed.handle_key(&key("alt+y", KeyCode::Char('y'), KeyModifiers::ALT));
        assert_eq!(ed.text(), "gamma", "yank-pop wrapped back to newest");
    }

    // --- Undo coalescing + boundaries ---------------------------------------

    #[test]
    fn typing_burst_is_one_undo_unit() {
        let mut ed = Editor::new();
        type_str(&mut ed, "hello");
        assert_eq!(ed.undo_stack.len(), 1, "a typing burst is one unit");
        ed.undo();
        assert_eq!(ed.text(), "", "one undo removed the whole burst");
    }

    #[test]
    fn pause_starts_a_new_undo_unit() {
        let mut ed = Editor::new();
        type_str(&mut ed, "abc");
        ed.pause();
        type_str(&mut ed, "def");
        assert_eq!(ed.undo_stack.len(), 2, "pause split the burst");
        ed.undo();
        assert_eq!(ed.text(), "abc", "first undo removed the post-pause burst");
        ed.undo();
        assert_eq!(ed.text(), "");
    }

    #[test]
    fn newline_paste_delete_each_start_new_units() {
        // Newline seals a unit.
        let mut ed = Editor::new();
        type_str(&mut ed, "ab");
        ed.handle_key(&key("alt+enter", KeyCode::Enter, KeyModifiers::ALT));
        type_str(&mut ed, "cd");
        // Three units: "ab", "\n", "cd".
        assert_eq!(ed.undo_stack.len(), 3);
        ed.undo();
        assert_eq!(ed.text(), "ab\n");
        ed.undo();
        assert_eq!(ed.text(), "ab");

        // Paste is its own atomic unit and breaks the burst on both sides.
        let mut ed2 = Editor::new();
        type_str(&mut ed2, "x");
        ed2.insert_paste("PASTED");
        type_str(&mut ed2, "y");
        assert_eq!(ed2.undo_stack.len(), 3, "x | PASTED | y");
        ed2.undo();
        assert_eq!(ed2.text(), "xPASTED");
        ed2.undo();
        assert_eq!(ed2.text(), "x", "paste undoes in one step");

        // Delete starts a new unit (not merged with the preceding typing burst).
        let mut ed3 = Editor::new();
        type_str(&mut ed3, "abc");
        ed3.handle_key(&key("backspace", KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(ed3.text(), "ab");
        assert_eq!(ed3.undo_stack.len(), 2, "delete is its own unit");
        ed3.undo();
        assert_eq!(ed3.text(), "abc", "undo restored the deleted char");
    }

    #[test]
    fn undo_after_submit_restores_sent_text() {
        let mut ed = Editor::new();
        type_str(&mut ed, "send me");
        ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(ed.take_submit().as_deref(), Some("send me"));
        assert_eq!(ed.text(), "", "buffer cleared on submit");
        ed.undo();
        assert_eq!(ed.text(), "send me", "undo restored the submitted text");
    }

    #[test]
    fn undo_redo_are_calm_noops_at_boundaries() {
        let mut ed = Editor::new();
        // Undo/redo on an empty stack do nothing and do not panic.
        ed.undo();
        ed.redo();
        assert_eq!(ed.text(), "");
        type_str(&mut ed, "hi");
        ed.undo();
        assert_eq!(ed.text(), "");
        // Extra undo past the bottom is a no-op.
        ed.undo();
        assert_eq!(ed.text(), "");
        ed.redo();
        assert_eq!(ed.text(), "hi", "redo replays the undone unit");
        // Extra redo past the top is a no-op.
        ed.redo();
        assert_eq!(ed.text(), "hi");
    }

    #[test]
    fn typing_after_undo_discards_redo_branch() {
        let mut ed = Editor::new();
        type_str(&mut ed, "abc");
        ed.pause();
        type_str(&mut ed, "def");
        ed.undo(); // drop "def"
        assert_eq!(ed.text(), "abc");
        // A fresh edit discards the redo branch.
        type_str(&mut ed, "X");
        assert_eq!(ed.text(), "abcX");
        ed.redo();
        assert_eq!(
            ed.text(),
            "abcX",
            "redo branch was discarded, redo is inert"
        );
    }

    #[test]
    fn redo_replays_at_unit_granularity() {
        let mut ed = Editor::new();
        type_str(&mut ed, "one");
        ed.pause();
        type_str(&mut ed, "two");
        ed.undo();
        ed.undo();
        assert_eq!(ed.text(), "");
        ed.redo();
        assert_eq!(ed.text(), "one", "redo replays one unit");
        ed.redo();
        assert_eq!(ed.text(), "onetwo", "redo replays the next unit");
    }

    // --- Paste pipeline pure logic ------------------------------------------

    #[test]
    fn defuse_strips_escape_and_control_keeps_newline_tab() {
        // A pasted CSI colour sequence plus a bell — all defused; the visible
        // text survives, and the real whitespace controls (\n, \t) are kept.
        let payload = "red\x1b[31mtext\x07\tafter\nline2\r\n";
        let out = defuse_control_bytes(payload);
        assert!(!out.contains('\x1b'), "ESC removed");
        assert!(!out.contains('\x07'), "BEL removed");
        assert!(!out.contains('\r'), "carriage return removed");
        assert_eq!(
            out, "red[31mtext\tafter\nline2\n",
            "printable + \\n + \\t survive; CRLF collapses to LF"
        );
    }

    #[test]
    fn marker_ending_at_detects_complete_token() {
        let before = "see [paste #2 +40 lines]";
        assert_eq!(paste_marker_ending_at(before), Some((4, 2)));
        // A cursor not on a closing bracket is not a marker end.
        assert_eq!(paste_marker_ending_at("no marker here"), None);
        // A nested/incomplete bracket is rejected.
        assert_eq!(paste_marker_ending_at("[paste #1 [x]]"), None);
    }

    #[test]
    fn marker_covering_finds_token_at_open_and_inside_only() {
        let line = "a [paste #1 +99 lines] b";
        let open = line.find('[').unwrap();
        let close = line.find(']').unwrap();
        assert_eq!(
            marker_covering(line, open),
            Some(1),
            "covers the open bracket"
        );
        assert_eq!(
            marker_covering(line, open + 3),
            Some(1),
            "covers strictly inside the token"
        );
        assert_eq!(
            marker_covering(line, close),
            Some(1),
            "covers the close bracket"
        );
        assert_eq!(
            marker_covering(line, 0),
            None,
            "before the token: not covered"
        );
        assert_eq!(
            marker_covering(line, close + 1),
            None,
            "past the close bracket: not covered"
        );
    }

    #[test]
    fn renumber_decrements_only_higher_ids() {
        let line = "[paste #1 x] mid [paste #3 y] end [paste #2 z]";
        // Removing id 1 shifts 2→1 and 3→2, leaves any at-or-below untouched.
        let out = renumber_paste_markers(line, 1);
        assert_eq!(out, "[paste #1 x] mid [paste #2 y] end [paste #1 z]");
    }

    #[test]
    fn expand_substitutes_known_and_passes_unknown() {
        let mut markers = HashMap::new();
        markers.insert(
            1,
            PasteContent {
                id: 1,
                text: "FULL\nPAYLOAD".to_string(),
                line_count: 2,
                char_count: 12,
            },
        );
        let expanded = expand_paste_markers("a [paste #1 +2 lines] b [paste #9 x] c", &markers);
        assert_eq!(
            expanded, "a FULL\nPAYLOAD b [paste #9 x] c",
            "known marker expands, unknown passes through literally"
        );
    }

    #[test]
    fn transform_rewrites_existing_path_and_skips_missing() {
        let cwd = PathBuf::from("/work");
        // Existence predicate: only /work/src/main.rs "exists".
        let exists = |p: &Path| p == Path::new("/work/src/main.rs");
        // Quoted relative path inside cwd → cwd-relative @mention.
        assert_eq!(
            transform_dropped_file_paste("'src/main.rs'", &cwd, &exists).as_deref(),
            Some("@src/main.rs")
        );
        // file:// prefix, percent-encoded, absolute inside cwd → still relative.
        assert_eq!(
            transform_dropped_file_paste("file:///work/src/main.rs", &cwd, &exists).as_deref(),
            Some("@src/main.rs")
        );
        // A non-existent path is left verbatim (None).
        assert_eq!(
            transform_dropped_file_paste("src/missing.rs", &cwd, &exists),
            None
        );
        // A multi-line payload is never a drop.
        assert_eq!(
            transform_dropped_file_paste("src/main.rs\nextra", &cwd, &exists),
            None
        );
    }
}
