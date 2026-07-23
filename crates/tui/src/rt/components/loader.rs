//! Loader family: a basic animated [`Loader`] and a [`CancellableLoader`] with a
//! progress bar and an Escape-to-cancel affordance.
//!
//! The rt-native counterparts to the legacy `LoaderComponent` and
//! `CancellableLoaderComponent`. Where the legacy widgets render to `Vec<String>`
//! of ANSI-coded lines and consume raw byte events, these implement
//! [`RtComponent`]: they paint into a ratatui [`Buffer`] and consume structured
//! [`RtKey`]s.
//!
//! # Pinned behaviour
//!
//! - **Static message, animated spinner.** Both loaders show a spinner glyph
//!   followed by a *static* message ("Working…"-style). [`tick`](Loader::tick)
//!   advances the spinner frame; the message is unchanged by ticking. Tests pin
//!   the presence/absence of the *static message text*, never a specific spinner
//!   glyph or the frame timing (Decision Log: spinner cadence is host-driven and
//!   an informed exclusion).
//! - **Active toggle.** A [`Loader`] renders its row only while
//!   [`active`](Loader::is_active); an inactive loader paints nothing, so the
//!   static text is present while working and gone once the work finishes — no
//!   ghost row left behind.
//! - **Cancellable chrome.** [`CancellableLoader`] adds, below the spinner line,
//!   an optional elapsed suffix `(3.2s)`, a `█`/`░` progress bar with a
//!   right-aligned percentage, and a "Press Escape to cancel" hint. Escape latches
//!   a [`CancelOutcome::Cancelled`] (polled via
//!   [`take_outcome`](CancellableLoader::take_outcome)) and sets
//!   [`is_cancelled`](CancellableLoader::is_cancelled).
//! - **CJK/emoji safety.** The spinner line is truncated to the area width on a
//!   *grapheme* boundary via [`truncate_graphemes_with_ellipsis`], measured in
//!   display columns — a two-column glyph is kept or dropped whole, and the
//!   narrow-terminal case never byte-slices a multibyte grapheme or underflows the
//!   width (the legacy `&msg[..n]` panic and `width - 8` underflow this avoids).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::truncate_graphemes_with_ellipsis;
use crate::rt::events::RtKey;
use crate::rt::view::{HandleOutcome, RtComponent};

/// Default braille spinner frames (mirrors the legacy loader's default set).
///
/// Exposed so a host can drive an animation timer, but the *glyphs* are not part
/// of any behavioural contract — only the static message text is.
pub const DEFAULT_SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Default cancel hint shown by [`CancellableLoader`].
pub const DEFAULT_CANCEL_HINT: &str = "Press Escape to cancel";

/// Maximum width, in columns, of the cancellable loader's progress bar interior
/// (between the brackets), matching the legacy cap.
const MAX_PROGRESS_BAR_WIDTH: usize = 40;

/// A basic animated loading spinner with a static message.
///
/// Renders a single row: a spinner glyph, a space, then the message. The message
/// is static — [`tick`](Loader::tick) only advances the spinner. When
/// [`inactive`](Loader::set_active), it paints nothing.
pub struct Loader {
    /// The static message shown beside the spinner.
    message: String,
    /// Spinner animation frames; empty hides the spinner glyph entirely.
    frames: Vec<String>,
    /// Current frame index into `frames` (kept in range by `tick`).
    frame: usize,
    /// Whether the loader is showing this frame. An inactive loader paints
    /// nothing — the static text is gone once the work finishes.
    active: bool,
    /// Style applied to the whole row.
    style: Style,
}

impl Loader {
    /// A new, **active** loader with the given static message and the default
    /// spinner frames.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            frames: DEFAULT_SPINNER_FRAMES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            frame: 0,
            active: true,
            style: Style::default().add_modifier(Modifier::DIM),
        }
    }

    /// Replace the spinner frames. An empty set hides the spinner glyph, leaving
    /// just the message.
    #[must_use]
    pub fn frames(mut self, frames: Vec<String>) -> Self {
        self.set_frames(frames);
        self
    }

    /// Set the row style.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Replace the spinner frames at runtime, resetting the frame index so the
    /// animation restarts cleanly.
    pub fn set_frames(&mut self, frames: Vec<String>) {
        self.frames = frames;
        self.frame = 0;
    }

    /// Replace the static message.
    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = message.into();
    }

    /// The current static message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Show or hide the loader. Hidden, it paints nothing.
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    /// Whether the loader is currently showing.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Advance to the next spinner frame. A no-op when there are no frames. Call
    /// this on a host-driven timer; it never changes the message.
    pub fn tick(&mut self) {
        if !self.frames.is_empty() {
            self.frame = (self.frame + 1) % self.frames.len();
        }
    }

    /// The current spinner frame index (for a host inspecting the animation).
    pub fn frame_index(&self) -> usize {
        self.frame
    }

    /// The spinner glyph for the current frame, or an empty string when the
    /// spinner is hidden.
    fn spinner(&self) -> &str {
        if self.frames.is_empty() {
            ""
        } else {
            &self.frames[self.frame % self.frames.len()]
        }
    }

    /// The full spinner line text ("`⠋ message`", or just the message when the
    /// spinner is hidden), before width truncation.
    fn line(&self) -> String {
        let spinner = self.spinner();
        if spinner.is_empty() {
            self.message.clone()
        } else {
            format!("{spinner} {}", self.message)
        }
    }
}

impl RtComponent for Loader {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() || !self.active {
            return;
        }
        let width = area.width as usize;
        let shown = truncate_graphemes_with_ellipsis(&self.line(), width);
        buf.set_stringn(area.x, area.y, &shown, width, self.style);
    }

    fn handle_key(&mut self, _key: &RtKey) -> HandleOutcome {
        HandleOutcome::Ignored
    }
}

/// The outcome of a terminal key on a [`CancellableLoader`], for a host that
/// wants to react without wiring a callback.
///
/// Escape latches [`CancelOutcome::Cancelled`], polled via
/// [`take_outcome`](CancellableLoader::take_outcome). A one-shot latch — reading
/// it clears it — so a host polls it once per frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// Escape was pressed; the host should abort the underlying task.
    Cancelled,
}

/// A loading spinner with an elapsed-time suffix, a progress bar, a percentage,
/// and an Escape-to-cancel affordance.
///
/// Renders up to three rows: the spinner line (spinner + message + optional
/// `(elapsed)` suffix), an optional `[███░░░]  50%` progress bar, and the cancel
/// hint. Escape sets [`is_cancelled`](CancellableLoader::is_cancelled) and latches
/// a [`CancelOutcome::Cancelled`].
pub struct CancellableLoader {
    /// The animated spinner and static message (reused wholesale).
    inner: Loader,
    /// Optional elapsed-time label, e.g. "3.2s", shown as a `(…)` suffix.
    elapsed: Option<String>,
    /// Optional progress ratio in `[0.0, 1.0]`; `None` hides the bar.
    progress: Option<f64>,
    /// The cancel hint; empty hides the hint row.
    cancel_hint: String,
    /// Whether Escape has cancelled this loader.
    cancelled: bool,
    /// Latched terminal outcome (Escape), cleared on read.
    outcome: Option<CancelOutcome>,
}

impl CancellableLoader {
    /// A new cancellable loader with the given static message, no progress, no
    /// elapsed suffix, and the default cancel hint.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            inner: Loader::new(message).style(Style::default()),
            elapsed: None,
            progress: None,
            cancel_hint: DEFAULT_CANCEL_HINT.to_string(),
            cancelled: false,
            outcome: None,
        }
    }

    /// Set the static message.
    pub fn set_message(&mut self, message: impl Into<String>) {
        self.inner.set_message(message);
    }

    /// The current static message.
    pub fn message(&self) -> &str {
        self.inner.message()
    }

    /// Set the elapsed-time suffix (e.g. `Some("3.2s")`), or `None` to hide it.
    pub fn set_elapsed(&mut self, elapsed: Option<String>) {
        self.elapsed = elapsed;
    }

    /// Set the progress ratio, clamped into `[0.0, 1.0]`; `None` hides the bar. A
    /// `NaN` is treated as no progress so a bad computation never leaves the bar
    /// in an undefined fill.
    pub fn set_progress(&mut self, progress: Option<f64>) {
        self.progress = progress.and_then(|p| {
            if p.is_nan() {
                None
            } else {
                Some(p.clamp(0.0, 1.0))
            }
        });
    }

    /// Set the cancel-hint text; empty hides the hint row.
    pub fn set_cancel_hint(&mut self, hint: impl Into<String>) {
        self.cancel_hint = hint.into();
    }

    /// Advance the spinner animation (host-driven timer). Never changes the
    /// message.
    pub fn tick(&mut self) {
        self.inner.tick();
    }

    /// The current spinner frame index.
    pub fn frame_index(&self) -> usize {
        self.inner.frame_index()
    }

    /// The number of rows this loader would like to occupy given its current
    /// state: the spinner line, plus the progress bar when there is progress,
    /// plus the cancel-hint row when the hint is non-empty (1..=3).
    ///
    /// A host laying the loader out in a vertical stack uses this to size the rect
    /// it hands to [`render`](RtComponent::render); a shorter rect simply clips
    /// the lower rows (they are painted top-down), which is what a tiny pane wants.
    pub fn desired_rows(&self) -> u16 {
        let mut rows = 1u16; // spinner line always present
        if self.progress.is_some() {
            rows += 1;
        }
        if !self.cancel_hint.is_empty() {
            rows += 1;
        }
        rows
    }

    /// Whether Escape has cancelled this loader.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Clear the cancelled state and any pending outcome (e.g. to reuse the loader
    /// for a fresh task).
    pub fn reset(&mut self) {
        self.cancelled = false;
        self.outcome = None;
    }

    /// Take the latched terminal outcome (Escape), clearing it. A host polls this
    /// once per frame to learn whether the user cancelled since the last poll.
    pub fn take_outcome(&mut self) -> Option<CancelOutcome> {
        self.outcome.take()
    }

    /// The spinner line text with the optional `(elapsed)` suffix appended, before
    /// width truncation.
    fn spinner_line(&self) -> String {
        let base = self.inner.line();
        match &self.elapsed {
            Some(elapsed) => format!("{base} ({elapsed})"),
            None => base,
        }
    }

    /// The progress-bar row for `width` columns, or `None` when there is no
    /// progress or no room. Built with a checked interior width so a terminal
    /// narrower than the fixed chrome never underflows (the legacy `width - 8`
    /// panic this avoids).
    fn progress_row(&self, width: usize) -> Option<String> {
        let progress = self.progress?;
        // Reserve room for the two-space indent, the brackets, the space, and the
        // "100%" readout. `saturating_sub` keeps this at zero on a narrow pane
        // rather than wrapping to a huge value.
        const CHROME: usize = 2 + 2 + 1 + 4; // "  [" .. "] 100%"
        let interior = width.saturating_sub(CHROME).min(MAX_PROGRESS_BAR_WIDTH);
        if interior == 0 {
            return None;
        }
        let filled = ((progress * interior as f64).round() as usize).min(interior);
        let empty = interior - filled;
        let pct = (progress * 100.0).round() as u32;
        Some(format!(
            "  [{}{}] {pct:>3}%",
            "█".repeat(filled),
            "░".repeat(empty),
        ))
    }
}

impl RtComponent for CancellableLoader {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let width = area.width as usize;
        let mut y = area.y;
        let bottom = area.y.saturating_add(area.height);

        // Row 1: spinner + message + optional elapsed suffix.
        if y < bottom {
            let shown = truncate_graphemes_with_ellipsis(&self.spinner_line(), width);
            buf.set_stringn(area.x, y, &shown, width, Style::default());
            y = y.saturating_add(1);
        }

        // Row 2: progress bar, when there is progress and room for it.
        if y < bottom
            && let Some(bar) = self.progress_row(width)
        {
            let shown = truncate_graphemes_with_ellipsis(&bar, width);
            buf.set_stringn(area.x, y, &shown, width, Style::default());
            y = y.saturating_add(1);
        }

        // Row 3: cancel hint.
        if y < bottom && !self.cancel_hint.is_empty() {
            let hint = format!("  {}", self.cancel_hint);
            let shown = truncate_graphemes_with_ellipsis(&hint, width);
            buf.set_stringn(
                area.x,
                y,
                &shown,
                width,
                Style::default().add_modifier(Modifier::DIM),
            );
        }
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        if key.key_id.as_deref() == Some("escape") {
            self.cancelled = true;
            self.outcome = Some(CancelOutcome::Cancelled);
            HandleOutcome::Consumed
        } else {
            HandleOutcome::Ignored
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt::components::display_width;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Rect;

    fn key(id: &str, code: KeyCode) -> RtKey {
        RtKey {
            key_id: Some(id.to_string()),
            raw: KeyEvent::new(code, KeyModifiers::NONE),
        }
    }

    fn render<C: RtComponent>(comp: &C, width: u16, height: u16) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        comp.render(area, &mut buf);
        buf
    }

    fn row(buf: &Buffer, y: u16) -> String {
        let area = buf.area;
        let mut s = String::new();
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell((x, y)) {
                s.push_str(cell.symbol());
            }
        }
        s.trim_end().to_string()
    }

    #[test]
    fn loader_shows_static_message_while_active() {
        let loader = Loader::new("Working…");
        let buf = render(&loader, 40, 1);
        assert!(row(&buf, 0).contains("Working…"));
    }

    #[test]
    fn loader_hidden_paints_nothing() {
        let mut loader = Loader::new("Working…");
        loader.set_active(false);
        let buf = render(&loader, 40, 1);
        assert!(
            row(&buf, 0).is_empty(),
            "inactive loader leaves no ghost row"
        );
    }

    #[test]
    fn loader_tick_keeps_message_static() {
        let mut loader = Loader::new("Loading data");
        loader.tick();
        loader.tick();
        let buf = render(&loader, 40, 1);
        assert!(row(&buf, 0).contains("Loading data"));
        assert_eq!(loader.message(), "Loading data");
    }

    #[test]
    fn cancellable_shows_progress_and_cancel_hint() {
        let mut loader = CancellableLoader::new("Fetching");
        loader.set_progress(Some(0.5));
        let buf = render(&loader, 60, 3);
        assert!(row(&buf, 0).contains("Fetching"));
        assert!(row(&buf, 1).contains('█'), "filled progress glyph present");
        assert!(row(&buf, 1).contains("50%"));
        assert!(row(&buf, 2).contains("Escape"), "cancel hint present");
    }

    #[test]
    fn cancellable_elapsed_suffix_rendered() {
        let mut loader = CancellableLoader::new("Working");
        loader.set_elapsed(Some("3.2s".to_string()));
        let buf = render(&loader, 60, 3);
        assert!(row(&buf, 0).contains("(3.2s)"));
    }

    #[test]
    fn cancellable_escape_cancels() {
        let mut loader = CancellableLoader::new("Working");
        assert!(!loader.is_cancelled());
        let outcome = loader.handle_key(&key("escape", KeyCode::Esc));
        assert!(outcome.is_consumed());
        assert!(loader.is_cancelled());
        assert_eq!(loader.take_outcome(), Some(CancelOutcome::Cancelled));
        // One-shot latch: a second poll is empty.
        assert_eq!(loader.take_outcome(), None);
    }

    #[test]
    fn cancellable_reset_clears_state() {
        let mut loader = CancellableLoader::new("Working");
        loader.handle_key(&key("escape", KeyCode::Esc));
        loader.reset();
        assert!(!loader.is_cancelled());
        assert_eq!(loader.take_outcome(), None);
    }

    #[test]
    fn cancellable_non_escape_ignored() {
        let mut loader = CancellableLoader::new("Working");
        let outcome = loader.handle_key(&key("enter", KeyCode::Enter));
        assert_eq!(outcome, HandleOutcome::Ignored);
        assert!(!loader.is_cancelled());
    }

    // --- CJK / underflow safety --------------------------------------------

    /// The display width the *helper* produced for the spinner line at `width` —
    /// measured from the source string, not the painted grid (a wide glyph's
    /// reserved continuation cell reads back as a blank, which would double-count).
    fn truncated_spinner_width(line: &str, width: u16) -> usize {
        display_width(&truncate_graphemes_with_ellipsis(line, width as usize))
    }

    #[test]
    fn narrow_cjk_loader_truncates_without_panic() {
        // A CJK message far wider than a <8-column pane: the legacy byte-slice
        // would panic here. Grapheme-aware truncation must survive, stay
        // single-row, and never exceed the pane width in display columns.
        let loader = Loader::new("你好世界你好世界");
        for w in 1u16..=8 {
            let buf = render(&loader, w, 2);
            // Confined to the single loader row — nothing spills to row 1.
            assert!(
                row(&buf, 1).is_empty(),
                "width {w}: spilled to a second row"
            );
            // The truncated line fits the pane (measured at the helper, so a wide
            // glyph is not double-counted by a reserved continuation cell).
            assert!(
                truncated_spinner_width(&loader.line(), w) <= w as usize,
                "width {w}: line wider than pane"
            );
        }
    }

    #[test]
    fn narrow_cjk_cancellable_no_width_underflow() {
        // The legacy `width - 8` underflowed usize on a <8-column pane. Here a
        // narrow pane must simply drop the bar, never panic.
        let mut loader = CancellableLoader::new("处理中");
        loader.set_progress(Some(0.5));
        for w in 1u16..=8 {
            // Rendering must not panic at any narrow width.
            let _ = render(&loader, w, 3);
            // The bar is dropped below its fixed chrome width rather than
            // underflowing usize.
            assert!(loader.progress_row(w as usize).is_none(), "width {w}");
            // The spinner line still fits the pane in display columns.
            assert!(
                truncated_spinner_width(&loader.spinner_line(), w) <= w as usize,
                "width {w}: spinner line wider than pane"
            );
        }
    }

    #[test]
    fn progress_row_absent_when_no_room() {
        let mut loader = CancellableLoader::new("x");
        loader.set_progress(Some(0.5));
        // Below the fixed chrome width, the bar is dropped rather than underflowing.
        assert!(loader.progress_row(5).is_none());
        // With room, it appears with a percentage.
        assert!(loader.progress_row(60).unwrap().contains("50%"));
    }

    #[test]
    fn progress_clamps_out_of_range() {
        let mut loader = CancellableLoader::new("x");
        loader.set_progress(Some(1.5));
        let bar = loader.progress_row(60).unwrap();
        assert!(bar.contains("100%"));
        assert!(!bar.contains('░'), "fully filled bar has no empty cells");
        loader.set_progress(Some(f64::NAN));
        assert!(
            loader.progress_row(60).is_none(),
            "NaN progress is treated as no progress"
        );
    }

    #[test]
    fn desired_rows_reflects_state() {
        let mut loader = CancellableLoader::new("x");
        assert_eq!(loader.desired_rows(), 2); // spinner + hint
        loader.set_progress(Some(0.1));
        assert_eq!(loader.desired_rows(), 3); // + bar
        loader.set_cancel_hint("");
        assert_eq!(loader.desired_rows(), 2); // spinner + bar, no hint
    }
}
