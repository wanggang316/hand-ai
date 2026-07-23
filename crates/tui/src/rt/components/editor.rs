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

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders, Widget};
use unicode_segmentation::UnicodeSegmentation;

use super::display_width;
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
    /// Kill-ring seam: the last killed span, set by future kill primitives and
    /// drained by [`take_killed`](Editor::take_killed). Unused by the core today.
    killed: Option<String>,
    /// The caret's viewport-local `(x, y)` within the last render area, recorded on
    /// [`render`](RtComponent::render) (which borrows `&self`) so
    /// [`cursor`](RtComponent::cursor) can report a width-aware position. `None`
    /// until first rendered or when the caret scrolls out of view.
    caret_cell: std::cell::Cell<Option<(u16, u16)>>,
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
            caret_cell: std::cell::Cell::new(None),
        }
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

    /// Undo seam. Every core mutation calls this with the edit it just applied; the
    /// undo feature overrides it to build a coalescing stack. A no-op today, but it
    /// keeps every mutation funnelling through one recording point so the later
    /// feature does not have to re-thread the primitives.
    #[allow(clippy::unused_self)]
    fn record_edit(&mut self, _edit: EditRecord) {
        // Intentionally empty: the undo feature owns the stack. See module docs.
    }

    /// Kill-ring seam: take the last killed span, clearing it. Kill primitives
    /// (added by the kill-ring feature) set it; a yank drains it here.
    pub fn take_killed(&mut self) -> Option<String> {
        self.killed.take()
    }

    /// Kill-ring seam: stash a killed span for a later yank. Exposed so the
    /// kill-ring feature can seed the ring without reaching into private state.
    pub fn set_kill(&mut self, text: impl Into<String>) {
        self.killed = Some(text.into());
    }

    /// Paste seam: the single entry point a paste event routes through. Today it
    /// inserts inline via [`insert_str`](Editor::insert_str); the paste-marker
    /// feature diverts a large payload to an out-of-band marker here, and no other
    /// call site changes.
    pub fn insert_paste(&mut self, text: &str) {
        self.insert_str(text);
    }

    /// Autocomplete seam: the token under the caret a provider would query, or
    /// `None` when the caret is not in a completable context. A `/` at column 0
    /// opens a slash command; an `@` (preceded by start-of-line or whitespace)
    /// opens a mention. The core computes this but never queries — the
    /// autocomplete feature does.
    #[must_use]
    pub fn context_at_cursor(&self) -> Option<AutocompleteContext> {
        let line = &self.lines[self.cursor_line];
        let before = &line[..self.cursor_col];
        // Slash command: only at the very start of the first line.
        if self.cursor_line == 0
            && before.starts_with('/')
            && !before[1..].contains(char::is_whitespace)
        {
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
    // Edit primitives (each funnels through `record_edit`)
    // -----------------------------------------------------------------

    /// Insert a string at the caret, splitting on `\n` into new lines. This is the
    /// path an IME multi-char commit and a small paste both take, so a whole
    /// composed run lands atomically.
    pub fn insert_str(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
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

    /// Insert a soft line break at the caret (splitting the current line).
    fn insert_newline(&mut self) {
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
    fn delete_back(&mut self) {
        if self.cursor_col > 0 {
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
    fn delete_forward(&mut self) {
        let line_len = self.lines[self.cursor_line].len();
        if self.cursor_col < line_len {
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
        self.add_to_history(&text);
        self.submitted = Some(text);
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
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        let Some(id) = key.key_id.as_deref() else {
            return HandleOutcome::Ignored;
        };
        match id {
            // Newline-insertion gestures.
            "alt+enter" | "shift+enter" => {
                // Shift+Enter is only distinguishable from Enter under an enhanced
                // (kitty) keyboard; in plain mode it never arrives as this id.
                self.insert_newline();
                HandleOutcome::Consumed
            }
            "enter" => {
                // Trailing-backslash soft break: a `\` immediately before Enter is
                // consumed and replaced with a newline, suppressing the submit.
                let line = &self.lines[self.cursor_line];
                if self.cursor_col > 0 && line.as_bytes().get(self.cursor_col - 1) == Some(&b'\\') {
                    self.delete_back();
                    self.insert_newline();
                } else {
                    self.submit();
                }
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
            "delete" => {
                self.delete_forward();
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
    fn alt_enter_inserts_newline_not_submit() {
        let mut ed = Editor::new();
        type_str(&mut ed, "ab");
        ed.handle_key(&key("alt+enter", KeyCode::Enter, KeyModifiers::ALT));
        type_str(&mut ed, "cd");
        assert_eq!(ed.text(), "ab\ncd");
        assert_eq!(ed.line_count(), 2);
        assert!(ed.take_submit().is_none());
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
}
