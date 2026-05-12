//! Editor component — multi-line text editor with grapheme-aware editing,
//! viewport scrolling, paste markers, kill-ring integration, undo/redo with
//! coalescing, and an autocomplete contract.
//!
//! Behavioural parity target: `pi-mono/packages/tui/src/components/editor.ts`.
//! This is functional parity, not visual pixel parity — paste-marker format,
//! undo coalescing rules, and key dispatch mirror the TS source; rendering and
//! border chrome are the Rust port's choice.
//!
//! ## Async autocomplete contract
//!
//! The editor is fully synchronous. When the user enters a slash-command or
//! `@`-attachment context, [`EditorComponent::pending_autocomplete_request`]
//! returns a [`AutocompleteContext`] the run loop can hand to a provider.
//! After awaiting the future, the run loop calls
//! [`EditorComponent::deliver_autocomplete_results`] to feed items back. A
//! 20 ms debounce window applies before a context becomes pending —
//! mirroring `ATTACHMENT_AUTOCOMPLETE_DEBOUNCE_MS` in the TS source.
//!
//! ## IME contract
//!
//! Composition state is stored in [`EditorComponent::set_composition`]; the
//! run loop is expected to call it when it observes IME events on the wire
//! (e.g. Kitty's `CSI 27 u` style preedit reports). While composing, the
//! string is rendered inline at the cursor with an underline attribute.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use unicode_segmentation::UnicodeSegmentation;

use crate::components::autocomplete::{
    AutocompleteContext, AutocompleteItem, AutocompleteProvider, AutocompleteTrigger,
};
use crate::keybindings::{Keybinding, KeybindingsManager};
use crate::keys::{Key, KeyName, parse_key};
use crate::kill_ring::KillRing;
use crate::tui::{Component, Focusable, HandleResult, InputEvent};
use crate::utils;

/// Threshold above which a paste is stored out-of-band as a marker. Mirrors
/// the TS thresholds (>10 lines OR >1000 chars).
const PASTE_LINES_THRESHOLD: usize = 10;
const PASTE_CHARS_THRESHOLD: usize = 1000;

/// Debounce window for autocomplete queries. Matches
/// `ATTACHMENT_AUTOCOMPLETE_DEBOUNCE_MS` from the TS source.
const AUTOCOMPLETE_DEBOUNCE_MS: u64 = 20;

/// Coalescing window for typing-class undo entries. Adjacent inserts of
/// printable characters within this interval merge into a single entry.
const UNDO_COALESCE_MS: u64 = 500;

// ============================================================================
// Paste markers
// ============================================================================

/// Out-of-band content for a paste marker. The editor stores large pastes
/// here and renders a placeholder like `[paste #1 +99 lines]`; the original
/// content is substituted back when [`EditorComponent::submit_text`] is
/// called.
#[derive(Debug, Clone)]
pub struct PasteContent {
    pub id: u32,
    pub text: String,
    pub line_count: u32,
    pub char_count: u32,
}

// ============================================================================
// Undo / redo
// ============================================================================

/// One reversible edit operation.
#[derive(Debug, Clone)]
pub enum UndoOp {
    /// Insert `text` starting at byte offset `position` of the joined buffer.
    Insert { position: usize, text: String },
    /// Delete `text` starting at byte offset `position` of the joined buffer.
    Delete { position: usize, text: String },
    /// Replace `removed` with `inserted` at `position`.
    Replace {
        position: usize,
        removed: String,
        inserted: String,
    },
}

/// One entry on the undo stack. Stores cursor positions before/after the op
/// so undo and redo restore them.
#[derive(Debug, Clone)]
pub struct UndoEntry {
    pub op: UndoOp,
    pub cursor_before: (usize, usize),
    pub cursor_after: (usize, usize),
    pub timestamp: Instant,
}

// ============================================================================
// Autocomplete state
// ============================================================================

/// Internal state tracking an in-flight autocomplete query.
#[derive(Debug, Clone)]
pub struct AutocompleteState {
    pub context: AutocompleteContext,
    /// Items returned by the most recent `deliver_autocomplete_results` call.
    pub items: Vec<AutocompleteItem>,
    /// Currently selected item index.
    pub selected: usize,
    /// True once we've delivered items at least once for this context.
    pub delivered: bool,
}

// ============================================================================
// Editor component
// ============================================================================

/// Multi-line text editor.
pub struct EditorComponent {
    /// Logical lines (no trailing newlines stored).
    lines: Vec<String>,
    /// Cursor line index.
    cursor_line: usize,
    /// Cursor byte column within the current line.
    cursor_col: usize,
    /// Focus state.
    focused: bool,
    /// First visible line.
    viewport_top: usize,
    /// Number of visible rows allotted to lines (excludes border).
    viewport_height: usize,
    /// Whether to render a border around the editor.
    border: bool,
    /// Paste marker storage, keyed by id.
    paste_markers: HashMap<u32, PasteContent>,
    /// Next paste id to allocate.
    next_paste_id: u32,
    /// Undo stack (most recent at the back).
    undo_stack: Vec<UndoEntry>,
    /// Redo stack (most recent undone first).
    redo_stack: Vec<UndoEntry>,
    /// Optional kill ring for cut/yank operations.
    kill_ring: KillRing,
    /// IME composition string in progress, if any.
    composing: Option<String>,
    /// Provider for autocomplete queries.
    autocomplete_provider: Option<Arc<dyn AutocompleteProvider>>,
    /// Pending autocomplete state, if any.
    autocomplete_state: Option<AutocompleteState>,
    /// Debounce gate — no pending request is exposed before this instant.
    autocomplete_debounce_until: Option<Instant>,
    /// Cached keybindings manager (for key dispatch).
    keybindings: KeybindingsManager,
    /// Submit callback, invoked on bare Enter. Mirrors pi-mono's
    /// `editor.onSubmit`. The editor buffer is cleared *before* the callback
    /// runs, so the callback can safely mutate UI state.
    on_submit: Option<SubmitCallback>,
    /// Placeholder text shown when the buffer is empty. Rendered dim and
    /// truncated to fit the available width. `None` disables the placeholder.
    placeholder: Option<String>,
    /// ANSI SGR prefix used for the border when the editor is unfocused.
    /// `None` keeps the border uncoloured. Mirrors pi-mono's
    /// `theme.borderColor`.
    border_color: Option<String>,
    /// ANSI SGR prefix used for the border when the editor is focused.
    /// Falls back to [`Self::border_color`] when `None`.
    focused_border_color: Option<String>,
    /// Prompt history. Index 0 is the most-recent entry; older entries
    /// follow. Capped at [`HISTORY_CAP`].
    history: Vec<String>,
    /// Current position in `history`. `-1` means "not browsing", `0` is
    /// the most recent entry, `1` is the next-older, etc. Matches
    /// pi-mono's `historyIndex` semantics so Up walks back (increments).
    history_index: i32,
    /// Border style when [`Self::border`] is true. `Box` draws full
    /// `┌─┐│└─┘` chrome (legacy default). `Horizontal` draws top/bottom
    /// horizontal rules only with no side glyphs — matches pi-mono's
    /// `EditorComponent` rendering.
    border_style: BorderStyle,
}

/// Border rendering style for [`EditorComponent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderStyle {
    /// Full box with corners and side rails (`┌─┐ │ └─┘`).
    Box,
    /// Top and bottom horizontal rules only (matches pi-mono).
    Horizontal,
}

/// Maximum number of submitted prompts the editor retains for Up/Down recall.
/// Matches pi-mono's `editor.ts` cap.
const HISTORY_CAP: usize = 100;

/// Callback invoked when the user submits the editor (bare Enter). The string
/// passed is the expanded text — paste markers are substituted back to their
/// original payload before the callback runs.
pub type SubmitCallback = Box<dyn FnMut(String) + Send + 'static>;

impl EditorComponent {
    /// Construct a new empty editor.
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
            focused: true,
            viewport_top: 0,
            viewport_height: 10,
            border: true,
            paste_markers: HashMap::new(),
            next_paste_id: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            kill_ring: KillRing::default(),
            composing: None,
            autocomplete_provider: None,
            autocomplete_state: None,
            autocomplete_debounce_until: None,
            keybindings: KeybindingsManager::new(),
            on_submit: None,
            placeholder: None,
            border_color: None,
            focused_border_color: None,
            history: Vec::new(),
            history_index: -1,
            border_style: BorderStyle::Box,
        }
    }

    /// Set the border style. Defaults to [`BorderStyle::Box`].
    pub fn set_border_style(&mut self, style: BorderStyle) {
        self.border_style = style;
    }

    /// Builder form of [`Self::set_border_style`].
    pub fn with_border_style(mut self, style: BorderStyle) -> Self {
        self.set_border_style(style);
        self
    }

    /// Append a submitted prompt to the recall history. Consecutive
    /// duplicates collapse; the list is capped at [`HISTORY_CAP`].
    pub fn add_to_history(&mut self, text: impl AsRef<str>) {
        let trimmed = text.as_ref().trim();
        if trimmed.is_empty() {
            return;
        }
        if let Some(first) = self.history.first()
            && first == trimmed
        {
            return;
        }
        self.history.insert(0, trimmed.to_string());
        if self.history.len() > HISTORY_CAP {
            self.history.pop();
        }
        self.history_index = -1;
    }

    /// Replace the entire recall history (e.g. when restoring a saved
    /// session). Most-recent entry first. Truncated to [`HISTORY_CAP`].
    pub fn set_history(&mut self, items: Vec<String>) {
        self.history = items;
        self.history.truncate(HISTORY_CAP);
        self.history_index = -1;
    }

    /// Read-only snapshot of the history (most-recent first).
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Walk one step through history. `direction = -1` walks back (Up),
    /// `direction = +1` walks forward (Down). When walking past the most-
    /// recent entry the editor is restored to empty.
    pub fn navigate_history(&mut self, direction: i32) {
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

    /// Set the buffer without resetting `history_index` so the
    /// browsing-history state persists across recall steps.
    fn set_text_internal(&mut self, text: &str) {
        self.lines = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(String::from).collect()
        };
        let last = self.lines.len() - 1;
        self.cursor_line = last;
        self.cursor_col = self.lines[last].len();
        self.viewport_top = 0;
        self.ensure_cursor_visible();
        self.autocomplete_state = None;
        self.autocomplete_debounce_until = None;
    }

    /// Set the placeholder shown when the buffer is empty.
    pub fn set_placeholder(&mut self, text: impl Into<String>) {
        self.placeholder = Some(text.into());
    }

    /// Builder form of [`Self::set_placeholder`].
    pub fn with_placeholder(mut self, text: impl Into<String>) -> Self {
        self.set_placeholder(text);
        self
    }

    /// Set the ANSI SGR prefix used to paint the border in the unfocused
    /// state. The prefix is applied to every border glyph and reset is
    /// emitted automatically at the end of each line.
    pub fn set_border_color(&mut self, ansi_prefix: impl Into<String>) {
        self.border_color = Some(ansi_prefix.into());
    }

    /// Builder form of [`Self::set_border_color`].
    pub fn with_border_color(mut self, ansi_prefix: impl Into<String>) -> Self {
        self.set_border_color(ansi_prefix);
        self
    }

    /// Set the ANSI SGR prefix used to paint the border while focused.
    /// Falls back to [`Self::set_border_color`] when not set.
    pub fn set_focused_border_color(&mut self, ansi_prefix: impl Into<String>) {
        self.focused_border_color = Some(ansi_prefix.into());
    }

    /// Builder form of [`Self::set_focused_border_color`].
    pub fn with_focused_border_color(mut self, ansi_prefix: impl Into<String>) -> Self {
        self.set_focused_border_color(ansi_prefix);
        self
    }

    /// Install a callback invoked on bare Enter (no modifiers). The editor
    /// buffer is cleared before the callback runs.
    pub fn set_on_submit<F>(&mut self, cb: F)
    where
        F: FnMut(String) + Send + 'static,
    {
        self.on_submit = Some(Box::new(cb));
    }

    /// Builder form of [`Self::set_on_submit`].
    pub fn with_on_submit<F>(mut self, cb: F) -> Self
    where
        F: FnMut(String) + Send + 'static,
    {
        self.set_on_submit(cb);
        self
    }

    pub fn with_viewport_height(mut self, height: usize) -> Self {
        self.viewport_height = height;
        self
    }

    pub fn with_border(mut self, border: bool) -> Self {
        self.border = border;
        self
    }

    /// Get all text as a single string (paste markers are NOT expanded; use
    /// [`Self::submit_text`] for the expanded form intended for downstream
    /// consumers).
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Get the editor text with paste markers substituted back to their
    /// original content. Use this when reading the buffer for submission.
    pub fn submit_text(&self) -> String {
        expand_paste_markers(&self.text(), &self.paste_markers)
    }

    /// Replace the editor buffer. Clears undo/redo, paste markers, and
    /// autocomplete state.
    pub fn set_text(&mut self, text: &str) {
        self.lines = if text.is_empty() {
            vec![String::new()]
        } else {
            let mut v: Vec<String> = text.split('\n').map(String::from).collect();
            if v.is_empty() {
                v.push(String::new());
            }
            v
        };
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.viewport_top = 0;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.paste_markers.clear();
        self.next_paste_id = 0;
        self.autocomplete_state = None;
        self.autocomplete_debounce_until = None;
    }

    /// Number of logical lines.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Cursor (line, byte_col).
    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_line, self.cursor_col)
    }

    /// Set the viewport height (number of visible rows for lines, exclusive
    /// of border chrome).
    pub fn set_viewport_height(&mut self, h: usize) {
        self.viewport_height = h.max(1);
        self.ensure_cursor_visible();
    }

    /// Adjust the viewport-top offset so the cursor line is visible.
    pub fn ensure_cursor_visible(&mut self) {
        if self.cursor_line < self.viewport_top {
            self.viewport_top = self.cursor_line;
        }
        if self.viewport_height > 0 && self.cursor_line >= self.viewport_top + self.viewport_height
        {
            self.viewport_top = self.cursor_line + 1 - self.viewport_height;
        }
    }

    /// Set the autocomplete provider. The editor never drives the provider
    /// itself — see module docs for the async contract.
    pub fn set_autocomplete_provider(&mut self, provider: Arc<dyn AutocompleteProvider>) {
        self.autocomplete_provider = Some(provider);
    }

    /// If a fresh autocomplete request is pending (debounce elapsed and not
    /// yet delivered), return the context the run loop should query against.
    pub fn pending_autocomplete_request(&self) -> Option<&AutocompleteContext> {
        let state = self.autocomplete_state.as_ref()?;
        if state.delivered {
            return None;
        }
        if let Some(until) = self.autocomplete_debounce_until
            && Instant::now() < until
        {
            return None;
        }
        Some(&state.context)
    }

    /// Provide items returned by the provider. Sets `delivered = true` on
    /// the active state and resets the selection.
    pub fn deliver_autocomplete_results(&mut self, items: Vec<AutocompleteItem>) {
        if let Some(state) = self.autocomplete_state.as_mut() {
            state.items = items;
            state.selected = 0;
            state.delivered = true;
            self.autocomplete_debounce_until = None;
            if state.items.is_empty() {
                // No matches — drop the popup state to mirror TS behaviour.
                self.autocomplete_state = None;
            }
        }
    }

    /// Read-only view of the current autocomplete state.
    pub fn autocomplete_state(&self) -> Option<&AutocompleteState> {
        self.autocomplete_state.as_ref()
    }

    /// Set or clear in-progress IME composition. While composition is non-
    /// `None`, render renders the string at the cursor with underline.
    /// Submitting the composition (commit) is the run loop's job: it should
    /// call `set_composition(None)` then dispatch the committed text via
    /// [`EditorComponent::handle_input`].
    pub fn set_composition(&mut self, text: Option<String>) {
        self.composing = text;
    }

    /// View into the kill ring (useful for tests).
    pub fn kill_ring(&self) -> &KillRing {
        &self.kill_ring
    }

    /// Read-only view of stored paste markers.
    pub fn paste_markers(&self) -> &HashMap<u32, PasteContent> {
        &self.paste_markers
    }

    // -----------------------------------------------------------------
    // Insertion / paste API
    // -----------------------------------------------------------------

    /// Insert `text` at the cursor. If the text exceeds the marker
    /// threshold, store it out-of-band and insert a placeholder instead.
    pub fn paste(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let line_count = text.split('\n').count();
        let char_count = text.chars().count();
        if line_count > PASTE_LINES_THRESHOLD || char_count > PASTE_CHARS_THRESHOLD {
            self.next_paste_id += 1;
            let id = self.next_paste_id;
            let marker = if line_count > PASTE_LINES_THRESHOLD {
                format!("[paste #{} +{} lines]", id, line_count)
            } else {
                format!("[paste #{} {} chars]", id, char_count)
            };
            self.paste_markers.insert(
                id,
                PasteContent {
                    id,
                    text: text.to_string(),
                    line_count: line_count as u32,
                    char_count: char_count as u32,
                },
            );
            self.insert_text(&marker);
        } else {
            self.insert_text(text);
        }
    }

    /// Insert plain `text` at the cursor with full undo recording. Newlines
    /// split lines as you'd expect.
    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let cursor_before = (self.cursor_line, self.cursor_col);
        let position = self.flat_offset(self.cursor_line, self.cursor_col);
        self.insert_text_no_undo(text);
        let cursor_after = (self.cursor_line, self.cursor_col);
        self.push_undo(UndoEntry {
            op: UndoOp::Insert {
                position,
                text: text.to_string(),
            },
            cursor_before,
            cursor_after,
            timestamp: Instant::now(),
        });
        self.maybe_trigger_autocomplete();
    }

    fn insert_text_no_undo(&mut self, text: &str) {
        let mut iter = text.split('\n');
        let first = iter.next().unwrap_or("");
        let byte_offset = self.cursor_col;
        let line = &mut self.lines[self.cursor_line];
        line.insert_str(byte_offset, first);
        self.cursor_col = byte_offset + first.len();
        for chunk in iter {
            // Newline boundary
            let byte_offset = self.cursor_col;
            let rest: String = self.lines[self.cursor_line][byte_offset..].to_string();
            self.lines[self.cursor_line].truncate(byte_offset);
            self.cursor_line += 1;
            self.lines.insert(self.cursor_line, String::new());
            self.lines[self.cursor_line].push_str(chunk);
            self.cursor_col = chunk.len();
            self.lines[self.cursor_line].push_str(&rest);
        }
        self.ensure_cursor_visible();
    }

    // -----------------------------------------------------------------
    // Cursor helpers
    // -----------------------------------------------------------------

    fn current_line(&self) -> &str {
        &self.lines[self.cursor_line]
    }

    fn current_line_byte_len(&self) -> usize {
        self.lines[self.cursor_line].len()
    }

    /// Visual column of the cursor on its current line (counts grapheme
    /// width, not bytes). Used for the `line:col` indicator so CJK / emoji
    /// content reads as the user would expect.
    fn cursor_visual_col(&self) -> usize {
        let line = self.current_line();
        let prefix = &line[..self.cursor_col.min(line.len())];
        utils::visible_width(prefix)
    }

    /// Flatten (line, byte_col) into a single byte offset over the joined
    /// buffer (`lines.join("\n")`). Used for undo bookkeeping.
    fn flat_offset(&self, line: usize, col: usize) -> usize {
        let mut offset = 0usize;
        for l in &self.lines[..line] {
            offset += l.len() + 1; // +1 for newline
        }
        offset + col
    }

    /// Inverse of [`Self::flat_offset`].
    fn unflatten(&self, mut pos: usize) -> (usize, usize) {
        for (i, line) in self.lines.iter().enumerate() {
            if pos <= line.len() {
                return (i, pos);
            }
            pos -= line.len() + 1;
        }
        let last = self.lines.len().saturating_sub(1);
        (last, self.lines[last].len())
    }

    /// Move the cursor left by one grapheme.
    fn move_left_grapheme(&mut self) {
        if self.cursor_col > 0 {
            let line = self.current_line();
            let before = &line[..self.cursor_col];
            if let Some((_, last)) = before.grapheme_indices(true).next_back() {
                self.cursor_col -= last.len();
            } else {
                self.cursor_col = 0;
            }
        } else if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.current_line_byte_len();
            self.ensure_cursor_visible();
        }
    }

    /// Move the cursor right by one grapheme.
    fn move_right_grapheme(&mut self) {
        let line_len = self.current_line_byte_len();
        if self.cursor_col < line_len {
            let line = self.current_line();
            let after = &line[self.cursor_col..];
            if let Some((_, first)) = after.grapheme_indices(true).next() {
                self.cursor_col += first.len();
            } else {
                self.cursor_col = line_len;
            }
        } else if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = 0;
            self.ensure_cursor_visible();
        }
    }

    /// Find the byte offset of the start of the previous "word" relative to
    /// `col` on the current line. A word is a run of non-whitespace.
    fn prev_word_col(&self, line_idx: usize, col: usize) -> usize {
        let line = &self.lines[line_idx];
        if col == 0 {
            return 0;
        }
        let bytes = line.as_bytes();
        let mut i = col;
        // Skip whitespace going backwards.
        while i > 0 && (bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') {
            i -= 1;
        }
        // Skip non-whitespace.
        while i > 0 && bytes[i - 1] != b' ' && bytes[i - 1] != b'\t' {
            i -= 1;
        }
        // Snap to char boundary.
        while !line.is_char_boundary(i) && i > 0 {
            i -= 1;
        }
        i
    }

    /// Find the byte offset just past the end of the next word.
    fn next_word_col(&self, line_idx: usize, col: usize) -> usize {
        let line = &self.lines[line_idx];
        let bytes = line.as_bytes();
        let len = line.len();
        let mut i = col;
        while i < len && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        while i < len && bytes[i] != b' ' && bytes[i] != b'\t' {
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
            self.cursor_col = self.current_line_byte_len();
            self.ensure_cursor_visible();
            return;
        }
        self.cursor_col = self.prev_word_col(self.cursor_line, self.cursor_col);
    }

    fn move_word_right(&mut self) {
        if self.cursor_col == self.current_line_byte_len() {
            if self.cursor_line + 1 < self.lines.len() {
                self.cursor_line += 1;
                self.cursor_col = 0;
                self.ensure_cursor_visible();
            }
            return;
        }
        self.cursor_col = self.next_word_col(self.cursor_line, self.cursor_col);
    }

    fn clamp_cursor(&mut self) {
        self.cursor_line = self.cursor_line.min(self.lines.len() - 1);
        let max = self.current_line_byte_len();
        if self.cursor_col > max {
            self.cursor_col = max;
        }
        // Snap to char boundary if landed mid-grapheme.
        let line = &self.lines[self.cursor_line];
        while self.cursor_col > 0 && !line.is_char_boundary(self.cursor_col) {
            self.cursor_col -= 1;
        }
    }

    // -----------------------------------------------------------------
    // Edit primitives (with undo)
    // -----------------------------------------------------------------

    fn insert_char(&mut self, ch: char) {
        let cursor_before = (self.cursor_line, self.cursor_col);
        let position = self.flat_offset(self.cursor_line, self.cursor_col);
        let mut buf = [0u8; 4];
        let s = ch.encode_utf8(&mut buf);
        self.lines[self.cursor_line].insert_str(self.cursor_col, s);
        self.cursor_col += s.len();
        let cursor_after = (self.cursor_line, self.cursor_col);
        self.push_undo_coalesce(UndoEntry {
            op: UndoOp::Insert {
                position,
                text: ch.to_string(),
            },
            cursor_before,
            cursor_after,
            timestamp: Instant::now(),
        });
        self.maybe_trigger_autocomplete();
    }

    fn insert_newline(&mut self) {
        let cursor_before = (self.cursor_line, self.cursor_col);
        let position = self.flat_offset(self.cursor_line, self.cursor_col);
        let rest = self.lines[self.cursor_line][self.cursor_col..].to_string();
        self.lines[self.cursor_line].truncate(self.cursor_col);
        self.cursor_line += 1;
        self.cursor_col = 0;
        self.lines.insert(self.cursor_line, rest);
        self.ensure_cursor_visible();
        let cursor_after = (self.cursor_line, self.cursor_col);
        self.push_undo(UndoEntry {
            op: UndoOp::Insert {
                position,
                text: "\n".to_string(),
            },
            cursor_before,
            cursor_after,
            timestamp: Instant::now(),
        });
        self.cancel_autocomplete();
    }

    fn delete_back(&mut self) {
        let cursor_before = (self.cursor_line, self.cursor_col);
        if self.cursor_col > 0 {
            // Delete one grapheme backward.
            let line = self.current_line();
            let before = &line[..self.cursor_col];
            let last = before
                .grapheme_indices(true)
                .next_back()
                .map(|(_, g)| g.to_string())
                .unwrap_or_default();
            let new_col = self.cursor_col - last.len();
            let position = self.flat_offset(self.cursor_line, new_col);
            self.lines[self.cursor_line].drain(new_col..self.cursor_col);
            self.cursor_col = new_col;
            self.push_undo(UndoEntry {
                op: UndoOp::Delete {
                    position,
                    text: last,
                },
                cursor_before,
                cursor_after: (self.cursor_line, self.cursor_col),
                timestamp: Instant::now(),
            });
        } else if self.cursor_line > 0 {
            // Join with previous line.
            let current = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            self.cursor_col = self.current_line_byte_len();
            let position = self.flat_offset(self.cursor_line, self.cursor_col);
            self.lines[self.cursor_line].push_str(&current);
            self.ensure_cursor_visible();
            self.push_undo(UndoEntry {
                op: UndoOp::Delete {
                    position,
                    text: "\n".to_string(),
                },
                cursor_before,
                cursor_after: (self.cursor_line, self.cursor_col),
                timestamp: Instant::now(),
            });
        }
        self.maybe_trigger_autocomplete();
    }

    fn delete_forward(&mut self) {
        let cursor_before = (self.cursor_line, self.cursor_col);
        let line_len = self.current_line_byte_len();
        if self.cursor_col < line_len {
            let line = self.current_line();
            let after = &line[self.cursor_col..];
            let first = after
                .grapheme_indices(true)
                .next()
                .map(|(_, g)| g.to_string())
                .unwrap_or_default();
            let position = self.flat_offset(self.cursor_line, self.cursor_col);
            let end = self.cursor_col + first.len();
            self.lines[self.cursor_line].drain(self.cursor_col..end);
            self.push_undo(UndoEntry {
                op: UndoOp::Delete {
                    position,
                    text: first,
                },
                cursor_before,
                cursor_after: (self.cursor_line, self.cursor_col),
                timestamp: Instant::now(),
            });
        } else if self.cursor_line + 1 < self.lines.len() {
            let position = self.flat_offset(self.cursor_line, self.cursor_col);
            let next = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&next);
            self.push_undo(UndoEntry {
                op: UndoOp::Delete {
                    position,
                    text: "\n".to_string(),
                },
                cursor_before,
                cursor_after: (self.cursor_line, self.cursor_col),
                timestamp: Instant::now(),
            });
        }
        self.maybe_trigger_autocomplete();
    }

    fn delete_word_backward(&mut self) {
        let cursor_before = (self.cursor_line, self.cursor_col);
        if self.cursor_col == 0 {
            // Falls back to backspace behaviour (line join).
            self.delete_back();
            return;
        }
        let new_col = self.prev_word_col(self.cursor_line, self.cursor_col);
        let removed: String = self.lines[self.cursor_line][new_col..self.cursor_col].to_string();
        let position = self.flat_offset(self.cursor_line, new_col);
        self.lines[self.cursor_line].drain(new_col..self.cursor_col);
        self.cursor_col = new_col;
        self.kill_ring.push(removed.clone());
        self.push_undo(UndoEntry {
            op: UndoOp::Delete {
                position,
                text: removed,
            },
            cursor_before,
            cursor_after: (self.cursor_line, self.cursor_col),
            timestamp: Instant::now(),
        });
        self.maybe_trigger_autocomplete();
    }

    fn delete_word_forward(&mut self) {
        let cursor_before = (self.cursor_line, self.cursor_col);
        let line_len = self.current_line_byte_len();
        if self.cursor_col == line_len {
            self.delete_forward();
            return;
        }
        let end = self.next_word_col(self.cursor_line, self.cursor_col);
        let removed: String = self.lines[self.cursor_line][self.cursor_col..end].to_string();
        let position = self.flat_offset(self.cursor_line, self.cursor_col);
        self.lines[self.cursor_line].drain(self.cursor_col..end);
        self.kill_ring.push(removed.clone());
        self.push_undo(UndoEntry {
            op: UndoOp::Delete {
                position,
                text: removed,
            },
            cursor_before,
            cursor_after: (self.cursor_line, self.cursor_col),
            timestamp: Instant::now(),
        });
        self.maybe_trigger_autocomplete();
    }

    fn delete_to_line_start(&mut self) {
        if self.cursor_col == 0 {
            return;
        }
        let cursor_before = (self.cursor_line, self.cursor_col);
        let removed: String = self.lines[self.cursor_line][..self.cursor_col].to_string();
        let position = self.flat_offset(self.cursor_line, 0);
        self.lines[self.cursor_line].drain(..self.cursor_col);
        self.cursor_col = 0;
        self.kill_ring.push(removed.clone());
        self.push_undo(UndoEntry {
            op: UndoOp::Delete {
                position,
                text: removed,
            },
            cursor_before,
            cursor_after: (self.cursor_line, self.cursor_col),
            timestamp: Instant::now(),
        });
        self.maybe_trigger_autocomplete();
    }

    fn delete_to_line_end(&mut self) {
        let line_len = self.current_line_byte_len();
        if self.cursor_col == line_len {
            // Join next line.
            self.delete_forward();
            return;
        }
        let cursor_before = (self.cursor_line, self.cursor_col);
        let removed: String = self.lines[self.cursor_line][self.cursor_col..].to_string();
        let position = self.flat_offset(self.cursor_line, self.cursor_col);
        self.lines[self.cursor_line].truncate(self.cursor_col);
        self.kill_ring.push(removed.clone());
        self.push_undo(UndoEntry {
            op: UndoOp::Delete {
                position,
                text: removed,
            },
            cursor_before,
            cursor_after: (self.cursor_line, self.cursor_col),
            timestamp: Instant::now(),
        });
        self.maybe_trigger_autocomplete();
    }

    /// Yank: insert most recent kill at cursor.
    pub fn yank(&mut self) {
        let text = self.kill_ring.yank().map(str::to_string);
        if let Some(t) = text {
            self.insert_text(&t);
        }
    }

    /// Yank pop: replace previously yanked text with the next older entry.
    pub fn yank_pop(&mut self) {
        // We require the previous action to have been a yank (kill_ring tracks
        // its own yank index). Simpler: peek `yank_pop` and if Some, undo the
        // last insert and insert the new entry. We rely on undo coalescing not
        // having merged the yank insertion (yank uses `insert_text` which
        // calls `push_undo`, never coalesce).
        let next = self.kill_ring.yank_pop().map(str::to_string);
        let Some(next) = next else {
            return;
        };
        // Replace last inserted yanked text by undoing then inserting next.
        self.undo();
        self.insert_text(&next);
    }

    // -----------------------------------------------------------------
    // Undo / redo
    // -----------------------------------------------------------------

    fn push_undo(&mut self, entry: UndoEntry) {
        self.undo_stack.push(entry);
        self.redo_stack.clear();
    }

    /// Push an entry, coalescing with the previous one if it is a same-typed
    /// adjacent insert/delete within the coalescing window.
    fn push_undo_coalesce(&mut self, entry: UndoEntry) {
        let coalesce_window = Duration::from_millis(UNDO_COALESCE_MS);
        if let Some(prev) = self.undo_stack.last_mut() {
            let elapsed = entry.timestamp.duration_since(prev.timestamp);
            if elapsed <= coalesce_window {
                match (&mut prev.op, &entry.op) {
                    (
                        UndoOp::Insert {
                            position: p_pos,
                            text: p_text,
                        },
                        UndoOp::Insert {
                            position: e_pos,
                            text: e_text,
                        },
                    ) if *e_pos == *p_pos + p_text.len() => {
                        p_text.push_str(e_text);
                        prev.cursor_after = entry.cursor_after;
                        prev.timestamp = entry.timestamp;
                        self.redo_stack.clear();
                        return;
                    }
                    _ => {}
                }
            }
        }
        self.push_undo(entry);
    }

    /// Undo the most recent operation.
    pub fn undo(&mut self) {
        let Some(entry) = self.undo_stack.pop() else {
            return;
        };
        match &entry.op {
            UndoOp::Insert { position, text } => {
                self.delete_range(*position, text.len());
            }
            UndoOp::Delete { position, text } => {
                self.insert_at(*position, text);
            }
            UndoOp::Replace {
                position,
                removed,
                inserted,
            } => {
                self.delete_range(*position, inserted.len());
                self.insert_at(*position, removed);
            }
        }
        let (l, c) = entry.cursor_before;
        self.cursor_line = l;
        self.cursor_col = c;
        self.clamp_cursor();
        self.ensure_cursor_visible();
        self.redo_stack.push(entry);
    }

    /// Redo the most recently undone operation.
    pub fn redo(&mut self) {
        let Some(entry) = self.redo_stack.pop() else {
            return;
        };
        match &entry.op {
            UndoOp::Insert { position, text } => {
                self.insert_at(*position, text);
            }
            UndoOp::Delete { position, text } => {
                self.delete_range(*position, text.len());
            }
            UndoOp::Replace {
                position,
                removed,
                inserted,
            } => {
                self.delete_range(*position, removed.len());
                self.insert_at(*position, inserted);
            }
        }
        let (l, c) = entry.cursor_after;
        self.cursor_line = l;
        self.cursor_col = c;
        self.clamp_cursor();
        self.ensure_cursor_visible();
        self.undo_stack.push(entry);
    }

    /// Insert raw text at flat byte position (no undo recording).
    fn insert_at(&mut self, position: usize, text: &str) {
        let (line, col) = self.unflatten(position);
        self.cursor_line = line;
        self.cursor_col = col;
        self.insert_text_no_undo(text);
    }

    /// Delete `len` bytes starting at flat `position` (no undo recording).
    fn delete_range(&mut self, position: usize, len: usize) {
        if len == 0 {
            return;
        }
        let (start_line, start_col) = self.unflatten(position);
        let (end_line, end_col) = self.unflatten(position + len);
        if start_line == end_line {
            self.lines[start_line].drain(start_col..end_col);
        } else {
            // Take the prefix of the start line, drop intermediate lines, and
            // append the suffix of the end line.
            let suffix = self.lines[end_line][end_col..].to_string();
            self.lines[start_line].truncate(start_col);
            self.lines[start_line].push_str(&suffix);
            self.lines.drain(start_line + 1..=end_line);
        }
        self.cursor_line = start_line;
        self.cursor_col = start_col;
    }

    // -----------------------------------------------------------------
    // Autocomplete
    // -----------------------------------------------------------------

    fn cancel_autocomplete(&mut self) {
        self.autocomplete_state = None;
        self.autocomplete_debounce_until = None;
    }

    /// After the buffer mutates, decide whether autocomplete should activate.
    /// Recognises slash-command start and `@`-attachment context.
    fn maybe_trigger_autocomplete(&mut self) {
        if self.autocomplete_provider.is_none() {
            return;
        }
        let line = self.current_line();
        let before: &str = &line[..self.cursor_col];
        let trigger = detect_trigger(before);
        match trigger {
            Some((trig, query_byte_start)) => {
                let query = before[query_byte_start..].to_string();
                let ctx = AutocompleteContext {
                    text: self.text(),
                    cursor: self.flat_offset(self.cursor_line, self.cursor_col),
                    trigger: trig,
                    query,
                };
                self.autocomplete_state = Some(AutocompleteState {
                    context: ctx,
                    items: Vec::new(),
                    selected: 0,
                    delivered: false,
                });
                self.autocomplete_debounce_until =
                    Some(Instant::now() + Duration::from_millis(AUTOCOMPLETE_DEBOUNCE_MS));
            }
            None => self.cancel_autocomplete(),
        }
    }
}

impl Default for EditorComponent {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Component impl — render + input
// ============================================================================

impl Component for EditorComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let mut output = Vec::new();
        let total_width = width as usize;
        // `display_width` is the writable cell count between the borders.
        // Box style reserves 4 cells for `│ ` and ` │`. Horizontal style
        // reserves no horizontal cells — content lines span the full width
        // with a one-cell left-padding for breathing room.
        let display_width = match (self.border, self.border_style) {
            (true, BorderStyle::Box) => total_width.saturating_sub(4),
            (true, BorderStyle::Horizontal) => total_width.saturating_sub(2),
            (false, _) => total_width,
        }
        .max(1);

        // Resolve the active border color: focused first, else fall back to
        // the unfocused color, else no styling.
        let active_border = if self.focused {
            self.focused_border_color
                .as_deref()
                .or(self.border_color.as_deref())
        } else {
            self.border_color.as_deref()
        };
        let paint_border = |s: String| -> String {
            match active_border {
                Some(c) => format!("{c}{s}\x1b[0m"),
                None => s,
            }
        };

        // Render the top border, if any.
        if self.border {
            output.push(match self.border_style {
                BorderStyle::Box => paint_border(format!(
                    "┌{}┐",
                    "─".repeat(total_width.saturating_sub(2))
                )),
                BorderStyle::Horizontal => paint_border("─".repeat(total_width)),
            });
        }

        // Empty-buffer placeholder: when the buffer is truly empty (one empty
        // line) and a placeholder is configured, render the cursor marker +
        // reverse-video block followed by a dim placeholder string.
        let is_buffer_empty = self.lines.len() == 1 && self.lines[0].is_empty();
        let render_placeholder = is_buffer_empty && self.placeholder.is_some();

        // Build visual rows with grapheme-aware wrapping.
        let mut visual: Vec<String> = Vec::new();
        for (i, raw_line) in self.lines.iter().enumerate() {
            let line_for_render = if i == self.cursor_line {
                if render_placeholder {
                    self.compose_placeholder_line(self.placeholder.as_deref().unwrap_or(""))
                } else {
                    self.compose_line_for_render(raw_line)
                }
            } else {
                raw_line.clone()
            };
            for v in word_wrap_line(&line_for_render, display_width) {
                visual.push(v);
            }
        }

        let side = paint_border("│".to_string());

        let format_row = |content: &str| -> String {
            let padded = if utils::visible_width(content) >= display_width {
                utils::truncate_to_width(content, display_width)
            } else {
                utils::pad_to_width(content, display_width)
            };
            match (self.border, self.border_style) {
                (true, BorderStyle::Box) => format!("{side} {padded} {side}"),
                (true, BorderStyle::Horizontal) => format!(" {padded} "),
                (false, _) => padded,
            }
        };

        // Apply viewport.
        let view_end = (self.viewport_top + self.viewport_height).min(visual.len());
        for line in visual.iter().take(view_end).skip(self.viewport_top) {
            output.push(format_row(line));
        }
        let empty = " ".repeat(display_width);
        for _ in view_end..(self.viewport_top + self.viewport_height) {
            output.push(format_row(&empty));
        }

        // Bottom border. Box style keeps the `line:col` indicator (it's a
        // long-standing debug aid for the Box variant). Horizontal style
        // mirrors pi-mono and renders a plain rule with no indicator.
        if self.border {
            output.push(match self.border_style {
                BorderStyle::Box => {
                    let info =
                        format!(" {}:{} ", self.cursor_line + 1, self.cursor_visual_col() + 1);
                    let remaining = total_width.saturating_sub(2 + info.len());
                    paint_border(format!(
                        "└{}{info}{}┘",
                        "─".repeat(remaining / 2),
                        "─".repeat(remaining - remaining / 2)
                    ))
                }
                BorderStyle::Horizontal => paint_border("─".repeat(total_width)),
            });
        }

        output
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        if !self.focused {
            return HandleResult::Ignored;
        }
        match event {
            InputEvent::Key(key) => self.handle_key(key, ""),
            InputEvent::Raw(data) => {
                let key = parse_key(data);
                self.handle_key(&key, data)
            }
            InputEvent::Paste(data) => {
                self.paste(data);
                HandleResult::Handled
            }
            _ => HandleResult::Ignored,
        }
    }

    fn invalidate(&mut self) {}
}

impl EditorComponent {
    /// Compose the cursor's logical line with any active IME composition AND
    /// a visible reverse-video cursor at [`Self::cursor_col`] for rendering.
    ///
    /// When [`Self::focused`] is true the [`crate::tui::CURSOR_MARKER`] APC
    /// sequence is also emitted immediately before the visible cursor so the
    /// host [`crate::Tui`] can reposition the hardware cursor for IME
    /// candidate windows. The marker is zero-width and gets stripped by the
    /// Tui before the line hits the terminal.
    /// Compose the cursor line when the buffer is empty and a placeholder
    /// is set: the cursor marker + reverse-video cell sits at column 0,
    /// followed by the dim placeholder text.
    fn compose_placeholder_line(&self, placeholder: &str) -> String {
        let marker = if self.focused {
            crate::tui::CURSOR_MARKER
        } else {
            ""
        };
        let cursor_cell = if self.focused {
            "\x1b[7m \x1b[0m"
        } else {
            " "
        };
        format!("{marker}{cursor_cell}\x1b[2m{placeholder}\x1b[0m")
    }

    fn compose_line_for_render(&self, line: &str) -> String {
        // IME composition path: inline the in-progress string with an
        // underline. Composition handling already prevents a stray cursor
        // from drifting around the candidate window, so we don't add the
        // reverse-video block here.
        if let Some(comp) = &self.composing {
            let (before, after) = line.split_at(self.cursor_col.min(line.len()));
            return format!("{}\x1b[4m{}\x1b[24m{}", before, comp, after);
        }

        // Visible-cursor path: split at cursor_col (byte offset), highlight
        // exactly one grapheme (or a trailing space when the cursor is past
        // the end of the line).
        let split = self.cursor_col.min(line.len());
        let (before, after) = line.split_at(split);
        let marker = if self.focused {
            crate::tui::CURSOR_MARKER
        } else {
            ""
        };
        if after.is_empty() {
            format!("{before}{marker}\x1b[7m \x1b[0m")
        } else {
            let first_grapheme = after
                .graphemes(true)
                .next()
                .expect("non-empty after has at least one grapheme");
            let rest = &after[first_grapheme.len()..];
            format!("{before}{marker}\x1b[7m{first_grapheme}\x1b[0m{rest}")
        }
    }

    fn handle_key(&mut self, key: &Key, raw: &str) -> HandleResult {
        if key.is_release {
            return HandleResult::Ignored;
        }

        // First, try keybindings — they cover named editor actions.
        if !raw.is_empty() {
            if self.keybindings.matches(raw, Keybinding::EditorCursorUp) {
                if self.cursor_line == 0 {
                    self.navigate_history(-1);
                } else {
                    self.cursor_up();
                }
                return HandleResult::Handled;
            }
            if self.keybindings.matches(raw, Keybinding::EditorCursorDown) {
                if self.cursor_line + 1 == self.lines.len() {
                    self.navigate_history(1);
                } else {
                    self.cursor_down();
                }
                return HandleResult::Handled;
            }
            if self.keybindings.matches(raw, Keybinding::EditorCursorLeft) {
                self.move_left_grapheme();
                return HandleResult::Handled;
            }
            if self.keybindings.matches(raw, Keybinding::EditorCursorRight) {
                self.move_right_grapheme();
                return HandleResult::Handled;
            }
            if self
                .keybindings
                .matches(raw, Keybinding::EditorCursorWordLeft)
            {
                self.move_word_left();
                return HandleResult::Handled;
            }
            if self
                .keybindings
                .matches(raw, Keybinding::EditorCursorWordRight)
            {
                self.move_word_right();
                return HandleResult::Handled;
            }
            if self
                .keybindings
                .matches(raw, Keybinding::EditorCursorLineStart)
            {
                self.cursor_col = 0;
                return HandleResult::Handled;
            }
            if self
                .keybindings
                .matches(raw, Keybinding::EditorCursorLineEnd)
            {
                self.cursor_col = self.current_line_byte_len();
                return HandleResult::Handled;
            }
            if self.keybindings.matches(raw, Keybinding::EditorPageUp) {
                self.page_up();
                return HandleResult::Handled;
            }
            if self.keybindings.matches(raw, Keybinding::EditorPageDown) {
                self.page_down();
                return HandleResult::Handled;
            }
            if self
                .keybindings
                .matches(raw, Keybinding::EditorDeleteCharBackward)
            {
                self.delete_back();
                return HandleResult::Handled;
            }
            if self
                .keybindings
                .matches(raw, Keybinding::EditorDeleteCharForward)
            {
                self.delete_forward();
                return HandleResult::Handled;
            }
            if self
                .keybindings
                .matches(raw, Keybinding::EditorDeleteWordBackward)
            {
                self.delete_word_backward();
                return HandleResult::Handled;
            }
            if self
                .keybindings
                .matches(raw, Keybinding::EditorDeleteWordForward)
            {
                self.delete_word_forward();
                return HandleResult::Handled;
            }
            if self
                .keybindings
                .matches(raw, Keybinding::EditorDeleteToLineStart)
            {
                self.delete_to_line_start();
                return HandleResult::Handled;
            }
            if self
                .keybindings
                .matches(raw, Keybinding::EditorDeleteToLineEnd)
            {
                self.delete_to_line_end();
                return HandleResult::Handled;
            }
            if self.keybindings.matches(raw, Keybinding::EditorYank) {
                self.yank();
                return HandleResult::Handled;
            }
            if self.keybindings.matches(raw, Keybinding::EditorYankPop) {
                self.yank_pop();
                return HandleResult::Handled;
            }
            if self.keybindings.matches(raw, Keybinding::EditorUndo) {
                self.undo();
                return HandleResult::Handled;
            }
        }

        // Fallback: structural keys not covered by the named bindings.
        match (&key.name, &key.modifiers) {
            (KeyName::Up, _) => {
                if self.cursor_line == 0 {
                    self.navigate_history(-1);
                } else {
                    self.cursor_up();
                }
                HandleResult::Handled
            }
            (KeyName::Down, _) => {
                if self.cursor_line + 1 == self.lines.len() {
                    self.navigate_history(1);
                } else {
                    self.cursor_down();
                }
                HandleResult::Handled
            }
            (KeyName::Left, _) => {
                self.move_left_grapheme();
                HandleResult::Handled
            }
            (KeyName::Right, _) => {
                self.move_right_grapheme();
                HandleResult::Handled
            }
            (KeyName::Home, _) => {
                self.cursor_col = 0;
                HandleResult::Handled
            }
            (KeyName::End, _) => {
                self.cursor_col = self.current_line_byte_len();
                HandleResult::Handled
            }
            (KeyName::PageUp, _) => {
                self.page_up();
                HandleResult::Handled
            }
            (KeyName::PageDown, _) => {
                self.page_down();
                HandleResult::Handled
            }
            (KeyName::Enter, m) if m.shift || m.alt => {
                self.insert_newline();
                HandleResult::Handled
            }
            (KeyName::Enter, _) => {
                if self.on_submit.is_some() {
                    let text = self.submit_text();
                    self.add_to_history(&text);
                    self.set_text("");
                    if let Some(cb) = self.on_submit.as_mut() {
                        cb(text);
                    }
                } else {
                    self.insert_newline();
                }
                HandleResult::Handled
            }
            (KeyName::Backspace, _) => {
                self.delete_back();
                HandleResult::Handled
            }
            (KeyName::Delete, _) => {
                self.delete_forward();
                HandleResult::Handled
            }
            (KeyName::Char('z'), m) if m.ctrl => {
                self.undo();
                HandleResult::Handled
            }
            (KeyName::Char('y'), m) if m.ctrl => {
                self.redo();
                HandleResult::Handled
            }
            (KeyName::Char(ch), m) if !m.ctrl && !m.alt => {
                self.insert_char(*ch);
                HandleResult::Handled
            }
            _ => HandleResult::Ignored,
        }
    }

    fn cursor_up(&mut self) {
        if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.clamp_cursor();
            self.ensure_cursor_visible();
        }
    }

    fn cursor_down(&mut self) {
        if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.clamp_cursor();
            self.ensure_cursor_visible();
        }
    }

    fn page_up(&mut self) {
        self.cursor_line = self.cursor_line.saturating_sub(self.viewport_height);
        self.clamp_cursor();
        self.ensure_cursor_visible();
    }

    fn page_down(&mut self) {
        self.cursor_line =
            (self.cursor_line + self.viewport_height).min(self.lines.len().saturating_sub(1));
        self.clamp_cursor();
        self.ensure_cursor_visible();
    }
}

impl Focusable for EditorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn cursor_position(&self) -> Option<(u16, u16)> {
        if !self.focused {
            return None;
        }
        let row = self.cursor_line.saturating_sub(self.viewport_top) as u16;
        let col_visible =
            utils::visible_width(&self.lines[self.cursor_line][..self.cursor_col]) as u16;
        let offset_x = if self.border { 2 } else { 0 };
        let offset_y = if self.border { 1 } else { 0 };
        Some((col_visible + offset_x, row + offset_y))
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Wrap a single logical line at `max_width`, respecting graphemes, ANSI
/// codes, and paste-marker tokens. Delegates to
/// [`utils::wrap_text_with_ansi`] which already handles the hard cases; this
/// wrapper exists to centralise the policy and to gracefully handle the
/// degenerate `max_width == 0` case.
fn word_wrap_line(line: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![line.to_string()];
    }
    if utils::visible_width(line) <= max_width {
        return vec![line.to_string()];
    }
    // For long lines, break by graphemes first to ensure no grapheme is split.
    // `wrap_text_with_ansi` already does grapheme-aware breaking via
    // `break_long_word`; defer to it.
    utils::wrap_text_with_ansi(line, max_width)
}

/// Substitute paste markers in `text` with their full content.
fn expand_paste_markers(text: &str, markers: &HashMap<u32, PasteContent>) -> String {
    if markers.is_empty() || !text.contains("[paste #") {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("[paste #") {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        if let Some(end) = after.find(']') {
            let token = &after[..=end];
            // Parse "[paste #<id>...]" — extract the id digits.
            let id_str: String = token
                .chars()
                .skip("[paste #".len())
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(id) = id_str.parse::<u32>()
                && let Some(p) = markers.get(&id)
            {
                out.push_str(&p.text);
                rest = &after[end + 1..];
                continue;
            }
            // Unknown marker — emit literally and advance past it.
            out.push_str(token);
            rest = &after[end + 1..];
        } else {
            out.push_str(after);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

/// Detect whether the text-before-cursor on the current line ends with an
/// active autocomplete trigger context. Returns `(trigger, byte index of
/// the start of the query — i.e. just past the trigger char)`.
fn detect_trigger(before: &str) -> Option<(AutocompleteTrigger, usize)> {
    // Slash command: only at start of line, and the slash is the first char
    // followed by zero or more word characters.
    if let Some(rest) = before.strip_prefix('/') {
        // Only treat `/` as a slash command trigger if the entire prefix is
        // `/` followed by command-name characters (no spaces).
        if rest.chars().all(|c| !c.is_whitespace()) {
            return Some((AutocompleteTrigger::Slash, 1));
        }
    }
    // `@` attachment: at start, or preceded by whitespace.
    if let Some(at) = before.rfind('@') {
        let preceded_ok = at == 0
            || before[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace);
        if preceded_ok && !before[at + 1..].chars().any(char::is_whitespace) {
            return Some((AutocompleteTrigger::At, at + 1));
        }
    }
    None
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::autocomplete::{AutocompleteFuture, AutocompleteItemKind};

    #[test]
    fn editor_new_is_empty() {
        let editor = EditorComponent::new();
        assert_eq!(editor.text(), "");
        assert_eq!(editor.line_count(), 1);
        assert_eq!(editor.cursor(), (0, 0));
    }

    #[test]
    fn editor_set_text_splits_lines() {
        let mut editor = EditorComponent::new();
        editor.set_text("line1\nline2\nline3");
        assert_eq!(editor.line_count(), 3);
        assert_eq!(editor.text(), "line1\nline2\nline3");
    }

    #[test]
    fn editor_insert_chars_via_raw() {
        let mut editor = EditorComponent::new();
        editor.handle_input(&InputEvent::Raw("h".into()));
        editor.handle_input(&InputEvent::Raw("i".into()));
        assert_eq!(editor.text(), "hi");
        assert_eq!(editor.cursor(), (0, 2));
    }

    #[test]
    fn editor_newline_creates_line() {
        let mut editor = EditorComponent::new();
        editor.handle_input(&InputEvent::Raw("a".into()));
        editor.handle_input(&InputEvent::Raw("\r".into()));
        editor.handle_input(&InputEvent::Raw("b".into()));
        assert_eq!(editor.line_count(), 2);
        assert_eq!(editor.text(), "a\nb");
    }

    #[test]
    fn editor_up_arrow_walks_history_when_on_first_line() {
        let mut editor = EditorComponent::new();
        editor.add_to_history("first");
        editor.add_to_history("second");
        editor.add_to_history("third");

        // history is [third, second, first] (most-recent first).
        editor.handle_input(&InputEvent::Raw("\x1b[A".into()));
        assert_eq!(editor.text(), "third");
        editor.handle_input(&InputEvent::Raw("\x1b[A".into()));
        assert_eq!(editor.text(), "second");
        editor.handle_input(&InputEvent::Raw("\x1b[A".into()));
        assert_eq!(editor.text(), "first");
        // Past the oldest: stays put.
        editor.handle_input(&InputEvent::Raw("\x1b[A".into()));
        assert_eq!(editor.text(), "first");

        // Down walks forward; one more Down restores the empty buffer.
        editor.handle_input(&InputEvent::Raw("\x1b[B".into()));
        assert_eq!(editor.text(), "second");
        editor.handle_input(&InputEvent::Raw("\x1b[B".into()));
        assert_eq!(editor.text(), "third");
        editor.handle_input(&InputEvent::Raw("\x1b[B".into()));
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn editor_history_dedups_consecutive_duplicates_and_caps() {
        let mut editor = EditorComponent::new();
        editor.add_to_history("hi");
        editor.add_to_history("hi");
        editor.add_to_history("hi");
        assert_eq!(editor.history().len(), 1);

        for i in 0..150 {
            editor.add_to_history(format!("entry-{i}"));
        }
        assert_eq!(editor.history().len(), 100);
        assert_eq!(editor.history()[0], "entry-149");
    }

    #[test]
    fn editor_submit_pushes_to_history() {
        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let cap = Arc::clone(&captured);
        let mut editor = EditorComponent::new().with_on_submit(move |t| {
            cap.lock().unwrap().push(t);
        });
        editor.handle_input(&InputEvent::Raw("h".into()));
        editor.handle_input(&InputEvent::Raw("i".into()));
        editor.handle_input(&InputEvent::Raw("\r".into()));
        assert_eq!(editor.history(), &["hi".to_string()]);
    }

    #[test]
    fn editor_enter_invokes_on_submit_and_clears_buffer() {
        use std::sync::{Arc, Mutex};

        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let cap = Arc::clone(&captured);
        let mut editor = EditorComponent::new().with_on_submit(move |text| {
            cap.lock().unwrap().push(text);
        });
        editor.handle_input(&InputEvent::Raw("h".into()));
        editor.handle_input(&InputEvent::Raw("i".into()));
        editor.handle_input(&InputEvent::Raw("\r".into()));

        assert_eq!(captured.lock().unwrap().as_slice(), &["hi".to_string()]);
        assert_eq!(editor.text(), "");
        assert_eq!(editor.cursor(), (0, 0));
    }

    #[test]
    fn editor_backspace_removes_last_char() {
        let mut editor = EditorComponent::new();
        editor.set_text("hello");
        editor.cursor_col = 5;
        editor.handle_input(&InputEvent::Raw("\x7f".into()));
        assert_eq!(editor.text(), "hell");
    }

    #[test]
    fn editor_backspace_joins_lines() {
        let mut editor = EditorComponent::new();
        editor.set_text("line1\nline2");
        editor.cursor_line = 1;
        editor.cursor_col = 0;
        editor.handle_input(&InputEvent::Raw("\x7f".into()));
        assert_eq!(editor.text(), "line1line2");
        assert_eq!(editor.line_count(), 1);
    }

    #[test]
    fn editor_delete_forward_removes_next_char() {
        let mut editor = EditorComponent::new();
        editor.set_text("hello");
        editor.cursor_col = 0;
        editor.handle_input(&InputEvent::Raw("\x1b[3~".into()));
        assert_eq!(editor.text(), "ello");
    }

    #[test]
    fn editor_arrow_movement() {
        let mut editor = EditorComponent::new();
        editor.set_text("hello\nworld");
        editor.cursor_line = 0;
        editor.cursor_col = 2;
        editor.handle_input(&InputEvent::Raw("\x1b[B".into()));
        assert_eq!(editor.cursor(), (1, 2));
        editor.handle_input(&InputEvent::Raw("\x1b[A".into()));
        assert_eq!(editor.cursor(), (0, 2));
    }

    #[test]
    fn editor_left_grapheme_handles_multibyte() {
        let mut editor = EditorComponent::new();
        editor.set_text("a你b");
        editor.cursor_col = "a你b".len();
        editor.move_left_grapheme();
        assert_eq!(editor.cursor_col, "a你".len());
        editor.move_left_grapheme();
        assert_eq!(editor.cursor_col, "a".len());
    }

    #[test]
    fn editor_undo_redo_basic() {
        let mut editor = EditorComponent::new();
        editor.handle_input(&InputEvent::Raw("a".into()));
        editor.handle_input(&InputEvent::Raw("b".into()));
        assert_eq!(editor.text(), "ab");
        editor.undo();
        // Undo with coalescing: both inserts merge into one entry, so undo
        // empties the buffer.
        assert_eq!(editor.text(), "");
        editor.redo();
        assert_eq!(editor.text(), "ab");
    }

    #[test]
    fn editor_undo_separates_after_window() {
        let mut editor = EditorComponent::new();
        editor.insert_text("a");
        std::thread::sleep(Duration::from_millis(UNDO_COALESCE_MS + 50));
        editor.insert_text("b");
        editor.undo();
        assert_eq!(editor.text(), "a");
        editor.undo();
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn editor_undo_delete() {
        let mut editor = EditorComponent::new();
        editor.set_text("hello");
        editor.cursor_col = 5;
        editor.delete_back();
        assert_eq!(editor.text(), "hell");
        editor.undo();
        assert_eq!(editor.text(), "hello");
        editor.redo();
        assert_eq!(editor.text(), "hell");
    }

    #[test]
    fn editor_render_with_border() {
        let mut editor = EditorComponent::new()
            .with_viewport_height(3)
            .with_border(true);
        editor.set_text("line1\nline2\nline3");
        let lines = editor.render(40);
        assert!(lines[0].starts_with('┌'));
        assert!(lines[1].starts_with('│'));
        assert!(lines.last().unwrap().starts_with('└'));
    }

    #[test]
    fn editor_placeholder_renders_when_empty_and_hides_when_typing() {
        let editor = EditorComponent::new()
            .with_viewport_height(3)
            .with_border(true)
            .with_placeholder("Type a message…");
        let lines = editor.render(40);
        // Some content row contains the placeholder text.
        assert!(
            lines.iter().any(|l| utils::strip_ansi(l).contains("Type a message…")),
            "expected placeholder in {lines:?}"
        );

        let mut editor = editor;
        editor.handle_input(&InputEvent::Raw("h".into()));
        let lines = editor.render(40);
        assert!(
            lines.iter().all(|l| !utils::strip_ansi(l).contains("Type a message…")),
            "expected placeholder to be hidden after typing"
        );
    }

    #[test]
    fn editor_focused_border_color_is_applied_to_border_glyphs() {
        let editor = EditorComponent::new()
            .with_viewport_height(2)
            .with_border(true)
            .with_focused_border_color("\x1b[36m");
        let lines = editor.render(20);
        // Top border carries the SGR prefix.
        assert!(lines[0].contains("\x1b[36m"));
        // Side borders carry it too.
        assert!(lines[1].contains("\x1b[36m"));
    }

    #[test]
    fn editor_render_without_border() {
        let mut editor = EditorComponent::new()
            .with_viewport_height(3)
            .with_border(false);
        editor.set_text("hello");
        let lines = editor.render(40);
        // The cursor decoration ("\x1b[7m...\x1b[0m") splits the literal
        // "hello" with ANSI codes; assert against the visible text.
        assert!(utils::strip_ansi(&lines[0]).contains("hello"));
    }

    #[test]
    fn editor_viewport_scrolls_to_cursor() {
        let mut editor = EditorComponent::new().with_viewport_height(3);
        editor.set_text("1\n2\n3\n4\n5");
        editor.cursor_line = 4;
        editor.cursor_col = 0;
        editor.ensure_cursor_visible();
        assert!(editor.viewport_top > 0);
    }

    #[test]
    fn editor_set_viewport_height_clamps_visible() {
        let mut editor = EditorComponent::new().with_viewport_height(10);
        editor.set_text("1\n2\n3\n4\n5\n6\n7\n8\n9\n10");
        editor.cursor_line = 9;
        editor.cursor_col = 0;
        editor.set_viewport_height(2);
        assert!(editor.viewport_top >= 8);
    }

    #[test]
    fn editor_page_up_down() {
        let mut editor = EditorComponent::new().with_viewport_height(3);
        editor.set_text("1\n2\n3\n4\n5\n6\n7\n8\n9\n10");
        editor.cursor_line = 5;
        editor.cursor_col = 0;
        editor.handle_input(&InputEvent::Raw("\x1b[5~".into()));
        assert!(editor.cursor_line < 5);
        editor.cursor_line = 0;
        editor.handle_input(&InputEvent::Raw("\x1b[6~".into()));
        assert!(editor.cursor_line > 0);
    }

    #[test]
    fn editor_focus_gating() {
        let mut editor = EditorComponent::new();
        assert!(editor.focused());
        editor.set_focused(false);
        assert_eq!(
            editor.handle_input(&InputEvent::Raw("a".into())),
            HandleResult::Ignored
        );
    }

    // -- paste markers ---------------------------------------------------

    #[test]
    fn paste_short_text_inserts_inline() {
        let mut editor = EditorComponent::new();
        editor.paste("short paste");
        assert_eq!(editor.text(), "short paste");
        assert!(editor.paste_markers().is_empty());
    }

    #[test]
    fn paste_many_lines_creates_marker() {
        let mut editor = EditorComponent::new();
        let big = (0..15)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        editor.paste(&big);
        assert!(editor.text().contains("[paste #1 +15 lines]"));
        assert_eq!(editor.paste_markers().len(), 1);
        let expanded = editor.submit_text();
        assert_eq!(expanded, big);
    }

    #[test]
    fn paste_long_single_line_creates_marker() {
        let mut editor = EditorComponent::new();
        let big = "x".repeat(2000);
        editor.paste(&big);
        assert!(editor.text().contains("[paste #1 2000 chars]"));
        assert_eq!(editor.submit_text(), big);
    }

    #[test]
    fn paste_marker_round_trips_with_surrounding_text() {
        let mut editor = EditorComponent::new();
        editor.insert_text("hello ");
        let big = (0..15)
            .map(|i| format!("L{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        editor.paste(&big);
        editor.insert_text(" world");
        let submitted = editor.submit_text();
        assert!(submitted.starts_with("hello "));
        assert!(submitted.contains(&big));
        assert!(submitted.ends_with(" world"));
    }

    #[test]
    fn input_event_paste_uses_marker_path() {
        let mut editor = EditorComponent::new();
        let big = (0..20)
            .map(|i| format!("row {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        editor.handle_input(&InputEvent::Paste(big.clone()));
        assert!(editor.text().contains("[paste #1"));
        assert_eq!(editor.submit_text(), big);
    }

    // -- word wrap ------------------------------------------------------

    #[test]
    fn word_wrap_preserves_short_lines() {
        assert_eq!(word_wrap_line("hello", 10), vec!["hello".to_string()]);
    }

    #[test]
    fn word_wrap_breaks_long_lines() {
        let long = "a".repeat(100);
        let wrapped = word_wrap_line(&long, 20);
        assert!(wrapped.len() >= 5);
        for w in &wrapped {
            assert!(utils::visible_width(w) <= 20);
        }
    }

    #[test]
    fn word_wrap_handles_cjk() {
        let text = "你好世界你好世界"; // 8 graphemes, each 2 cells wide
        let wrapped = word_wrap_line(text, 6);
        assert!(wrapped.len() >= 2);
        for w in &wrapped {
            assert!(utils::visible_width(w) <= 6);
        }
    }

    #[test]
    fn word_wrap_handles_zero_width() {
        let r = word_wrap_line("hello", 0);
        assert_eq!(r, vec!["hello".to_string()]);
    }

    // -- kill ring ------------------------------------------------------

    #[test]
    fn delete_to_line_end_pushes_to_kill_ring() {
        let mut editor = EditorComponent::new();
        editor.set_text("hello world");
        editor.cursor_col = 6;
        editor.delete_to_line_end();
        assert_eq!(editor.text(), "hello ");
        assert_eq!(editor.kill_ring().len(), 1);
    }

    #[test]
    fn delete_to_line_start_pushes_to_kill_ring() {
        let mut editor = EditorComponent::new();
        editor.set_text("hello world");
        editor.cursor_col = 6;
        editor.delete_to_line_start();
        assert_eq!(editor.text(), "world");
        assert_eq!(editor.kill_ring().len(), 1);
    }

    #[test]
    fn delete_word_backward_pushes_to_kill_ring() {
        let mut editor = EditorComponent::new();
        editor.set_text("foo bar baz");
        editor.cursor_col = 7; // end of "bar"
        editor.delete_word_backward();
        assert!(editor.text().starts_with("foo "));
        assert_eq!(editor.kill_ring().len(), 1);
    }

    #[test]
    fn yank_inserts_last_kill() {
        let mut editor = EditorComponent::new();
        editor.set_text("hello world");
        editor.cursor_col = 11;
        editor.delete_to_line_start();
        assert_eq!(editor.text(), "");
        editor.yank();
        assert_eq!(editor.text(), "hello world");
    }

    #[test]
    fn yank_pop_replaces_with_older_kill() {
        let mut editor = EditorComponent::new();
        editor.set_text("aaa bbb");
        editor.cursor_col = 7;
        editor.delete_word_backward(); // kill "bbb"
        editor.cursor_col = 0;
        editor.set_text("zzz");
        editor.cursor_col = 3;
        editor.delete_word_backward(); // kill "zzz"
        editor.set_text("");
        editor.yank();
        let after_yank = editor.text();
        assert_eq!(after_yank, "zzz");
        editor.yank_pop();
        assert_eq!(editor.text(), "bbb");
    }

    // -- autocomplete ----------------------------------------------------

    struct StaticProvider {
        items: Vec<AutocompleteItem>,
    }
    impl AutocompleteProvider for StaticProvider {
        fn query<'a>(&'a self, _ctx: &'a AutocompleteContext) -> AutocompleteFuture<'a> {
            let items = self.items.clone();
            Box::pin(async move { items })
        }
    }

    #[test]
    fn slash_typing_arms_autocomplete_after_debounce() {
        let mut editor = EditorComponent::new();
        editor.set_autocomplete_provider(Arc::new(StaticProvider {
            items: vec![AutocompleteItem {
                label: "help".to_string(),
                detail: None,
                insert_text: "help".to_string(),
                kind: AutocompleteItemKind::SlashCommand,
            }],
        }));
        editor.handle_input(&InputEvent::Raw("/".into()));
        // Before debounce elapses, no pending request.
        let _ = editor.pending_autocomplete_request();
        std::thread::sleep(Duration::from_millis(AUTOCOMPLETE_DEBOUNCE_MS + 10));
        let ctx = editor.pending_autocomplete_request().cloned();
        assert!(ctx.is_some());
        assert_eq!(ctx.unwrap().trigger, AutocompleteTrigger::Slash);
    }

    #[test]
    fn at_typing_arms_attachment_autocomplete() {
        let mut editor = EditorComponent::new();
        editor.set_autocomplete_provider(Arc::new(StaticProvider { items: vec![] }));
        editor.insert_text("hi @fo");
        std::thread::sleep(Duration::from_millis(AUTOCOMPLETE_DEBOUNCE_MS + 10));
        let ctx = editor.pending_autocomplete_request().cloned();
        assert!(ctx.is_some());
        let ctx = ctx.unwrap();
        assert_eq!(ctx.trigger, AutocompleteTrigger::At);
        assert_eq!(ctx.query, "fo");
    }

    #[test]
    fn deliver_autocomplete_clears_when_empty() {
        let mut editor = EditorComponent::new();
        editor.set_autocomplete_provider(Arc::new(StaticProvider { items: vec![] }));
        editor.handle_input(&InputEvent::Raw("/".into()));
        std::thread::sleep(Duration::from_millis(AUTOCOMPLETE_DEBOUNCE_MS + 10));
        editor.deliver_autocomplete_results(vec![]);
        assert!(editor.autocomplete_state().is_none());
    }

    #[test]
    fn deliver_autocomplete_marks_delivered() {
        let mut editor = EditorComponent::new();
        editor.set_autocomplete_provider(Arc::new(StaticProvider { items: vec![] }));
        editor.handle_input(&InputEvent::Raw("/".into()));
        std::thread::sleep(Duration::from_millis(AUTOCOMPLETE_DEBOUNCE_MS + 10));
        let item = AutocompleteItem {
            label: "help".to_string(),
            detail: None,
            insert_text: "help".to_string(),
            kind: AutocompleteItemKind::SlashCommand,
        };
        editor.deliver_autocomplete_results(vec![item]);
        let state = editor.autocomplete_state().unwrap();
        assert!(state.delivered);
        assert_eq!(state.items.len(), 1);
        assert!(editor.pending_autocomplete_request().is_none());
    }

    // -- IME -------------------------------------------------------------

    #[test]
    fn composition_appears_in_render() {
        let mut editor = EditorComponent::new().with_border(false);
        editor.set_text("ab");
        editor.cursor_col = 1;
        editor.set_composition(Some("X".to_string()));
        let rendered = editor.render(40);
        let joined = rendered.join("\n");
        // Underline ANSI sequence wraps the composition string.
        assert!(joined.contains("\x1b[4mX\x1b[24m"));
    }

    #[test]
    fn composition_clears() {
        let mut editor = EditorComponent::new();
        editor.set_composition(Some("X".to_string()));
        editor.set_composition(None);
        let rendered = editor.render(40);
        let joined = rendered.join("\n");
        assert!(!joined.contains("\x1b[4m"));
    }

    // -- expand_paste_markers --------------------------------------------

    #[test]
    fn expand_paste_markers_handles_unknown_id() {
        let mut markers = HashMap::new();
        markers.insert(
            1,
            PasteContent {
                id: 1,
                text: "FULL".to_string(),
                line_count: 1,
                char_count: 4,
            },
        );
        let s = "x [paste #1 4 chars] y [paste #99 ?] z";
        let out = expand_paste_markers(s, &markers);
        assert!(out.contains("FULL"));
        assert!(out.contains("[paste #99 ?]"));
    }

    #[test]
    fn expand_paste_markers_no_markers_passthrough() {
        let markers: HashMap<u32, PasteContent> = HashMap::new();
        assert_eq!(expand_paste_markers("hello", &markers), "hello");
    }

    // -- detect_trigger --------------------------------------------------

    #[test]
    fn detect_trigger_slash() {
        assert_eq!(detect_trigger("/he"), Some((AutocompleteTrigger::Slash, 1)));
    }

    #[test]
    fn detect_trigger_at_after_word() {
        let r = detect_trigger("text @fil");
        assert!(matches!(r, Some((AutocompleteTrigger::At, _))));
    }

    #[test]
    fn detect_trigger_none() {
        assert_eq!(detect_trigger("plain text"), None);
    }
}
