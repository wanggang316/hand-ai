//! The rt-native `/thinking` selector — the reasoning-level ladder built on the
//! [overlay runtime](super::overlay).
//!
//! It is a [`SelectorController`]: it produces rt-native styled [`Line`]s and
//! consumes keys while it is the mounted modal overlay. Its behaviour is the
//! thinking-ladder picker's:
//!
//! - the **ladder** is `off` plus every [`ThinkingLevel`] the model exposes, each
//!   row carrying a token-budget description (VAL-OVERLAY-025);
//! - **↑/↓ navigate with wrap** across the ladder;
//! - the cursor **starts on the current active level** so a bare Enter keeps it
//!   (VAL-OVERLAY-025);
//! - **Enter** confirms the highlighted level, **Esc** cancels — each emits exactly
//!   one [`ThinkingOutcome`] on the outcome channel and raises the
//!   [`DoneSignal`](super::overlay::DoneSignal) so the runtime unmounts it.
//!
//! The driver owns applying the pick (`set_stream_options`), the footer refresh,
//! the `[thinking: <label>]` status line, and the non-reasoning-model warning
//! (VAL-OVERLAY-026). This component is pure UI + pick logic over its constructor
//! inputs — the reusable construct-in / channel-out selector shape.

use std::sync::atomic::Ordering;

use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::HandleOutcome;
use model::ThinkingLevel;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::mpsc;

use super::overlay::{DoneSignal, SelectorController};
use crate::modes::interactive::theme::ThemePalette;

/// The ordered thinking ladder: `off` (represented as `None`) plus every
/// [`ThinkingLevel`] variant, top-to-bottom from least to most reasoning.
///
/// The `/thinking` selector always offers the full ladder — a model that is not a
/// reasoning model still lets the user pick a level, and the driver surfaces the
/// yellow "not a reasoning model" warning after the fact (VAL-OVERLAY-026), rather
/// than hiding the ladder.
#[must_use]
pub fn thinking_ladder() -> Vec<Option<ThinkingLevel>> {
    vec![
        None,
        Some(ThinkingLevel::Minimal),
        Some(ThinkingLevel::Low),
        Some(ThinkingLevel::Medium),
        Some(ThinkingLevel::High),
        Some(ThinkingLevel::Xhigh),
        Some(ThinkingLevel::Max),
    ]
}

/// The user-visible label for a ladder entry (`off` / `minimal` / …). Shared with
/// the footer's `thinking_level_label` so both surfaces agree on the wording.
#[must_use]
pub fn level_label(level: Option<ThinkingLevel>) -> &'static str {
    super::footer::thinking_level_label(level)
}

/// The token-budget description shown next to each ladder entry (mirrors the legacy
/// `LEVEL_DESCRIPTIONS`).
#[must_use]
pub fn level_description(level: Option<ThinkingLevel>) -> &'static str {
    match level {
        None => "No reasoning",
        Some(ThinkingLevel::Minimal) => "Very brief reasoning (~1k tokens)",
        Some(ThinkingLevel::Low) => "Light reasoning (~2k tokens)",
        Some(ThinkingLevel::Medium) => "Moderate reasoning (~8k tokens)",
        Some(ThinkingLevel::High) => "Deep reasoning (~16k tokens)",
        Some(ThinkingLevel::Xhigh) => "Extra-high reasoning (~32k tokens)",
        Some(ThinkingLevel::Max) => "Maximum reasoning",
    }
}

/// Parse a `/thinking <arg>` literal into a ladder entry, if it names one.
///
/// `off` / `none` / `clear` all resolve to `Some(None)` — the "off" entry — so the
/// off variants (VAL-OVERLAY-026) are accepted. A named level resolves to
/// `Some(Some(level))`. Anything else is `None`, which the driver surfaces as the
/// yellow "unknown level" guidance (VAL-OVERLAY-018). Matching is
/// case-insensitive.
#[must_use]
pub fn parse_level_arg(arg: &str) -> Option<Option<ThinkingLevel>> {
    match arg.trim().to_ascii_lowercase().as_str() {
        "off" | "none" | "clear" => Some(None),
        "minimal" => Some(Some(ThinkingLevel::Minimal)),
        "low" => Some(Some(ThinkingLevel::Low)),
        "medium" => Some(Some(ThinkingLevel::Medium)),
        "high" => Some(Some(ThinkingLevel::High)),
        "xhigh" => Some(Some(ThinkingLevel::Xhigh)),
        "max" => Some(Some(ThinkingLevel::Max)),
        _ => None,
    }
}

/// The outcome the selector emits on its channel — exactly one per open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingOutcome {
    /// The user confirmed this level (Enter). `None` means "off".
    Selected(Option<ThinkingLevel>),
    /// The user cancelled (Esc) — the current level is kept.
    Cancelled,
}

/// The rt-native `/thinking` ladder component.
pub struct ThinkingSelector {
    /// The full ladder (off + every level), in ascending order.
    ladder: Vec<Option<ThinkingLevel>>,
    /// The current active level, checkmarked and pre-selected.
    current: Option<ThinkingLevel>,
    /// The highlighted row, as an index into `ladder`.
    selected: usize,
    /// The outcome channel; exactly one [`ThinkingOutcome`] is sent on
    /// confirm/cancel.
    tx: mpsc::UnboundedSender<ThinkingOutcome>,
    /// Raised on the terminal key (Enter/Esc) so the overlay runtime unmounts this.
    done: DoneSignal,
}

impl ThinkingSelector {
    /// Build a selector over the full ladder, pre-selecting `current` (so a bare
    /// Enter keeps the active level).
    #[must_use]
    pub fn new(
        current: Option<ThinkingLevel>,
        tx: mpsc::UnboundedSender<ThinkingOutcome>,
        done: DoneSignal,
    ) -> Self {
        let ladder = thinking_ladder();
        let selected = ladder.iter().position(|l| *l == current).unwrap_or(0);
        Self {
            ladder,
            current,
            selected,
            tx,
            done,
        }
    }

    /// The highlighted ladder entry (test/introspection aid).
    #[must_use]
    pub fn highlighted(&self) -> Option<ThinkingLevel> {
        self.ladder.get(self.selected).copied().flatten()
    }

    /// The highlighted row index (test aid).
    #[must_use]
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Move the cursor up one row, wrapping to the bottom.
    fn move_up(&mut self) {
        let len = self.ladder.len();
        if len == 0 {
            return;
        }
        self.selected = if self.selected == 0 {
            len - 1
        } else {
            self.selected - 1
        };
    }

    /// Move the cursor down one row, wrapping to the top.
    fn move_down(&mut self) {
        let len = self.ladder.len();
        if len == 0 {
            return;
        }
        self.selected = (self.selected + 1) % len;
    }

    /// Emit the highlighted level as the confirmed outcome (Enter).
    fn confirm(&self) {
        let chosen = self.ladder.get(self.selected).copied().flatten();
        let _ = self.tx.send(ThinkingOutcome::Selected(chosen));
    }

    /// Emit the cancel outcome (Esc) — the current level is kept.
    fn cancel(&self) {
        let _ = self.tx.send(ThinkingOutcome::Cancelled);
    }

    /// The ladder body rendered as styled lines (a header, then one row per level
    /// with its budget description and a checkmark on the current level).
    fn body_lines(&self, _width: u16, palette: &ThemePalette) -> Vec<Line<'static>> {
        let muted = Style::default().fg(palette.dim);
        let accent = Style::default().fg(palette.accent);
        let success = Style::default().fg(palette.success);

        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::styled("Thinking level".to_string(), muted));
        lines.push(Line::from(String::new()));

        for (i, level) in self.ladder.iter().enumerate() {
            let is_selected = i == self.selected;
            let is_current = *level == self.current;
            let label = level_label(*level);
            let desc = level_description(*level);

            let mut spans: Vec<Span<'static>> = Vec::new();
            if is_selected {
                spans.push(Span::styled("→ ".to_string(), accent));
                spans.push(Span::styled(
                    label.to_string(),
                    accent.add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::raw("  ".to_string()));
                spans.push(Span::raw(label.to_string()));
            }
            spans.push(Span::styled(format!("  {desc}"), muted));
            if is_current {
                spans.push(Span::styled(" ✓".to_string(), success));
            }
            lines.push(Line::from(spans));
        }

        lines
    }
}

impl SelectorController for ThinkingSelector {
    fn render_lines(&self, width: u16, palette: &ThemePalette) -> Vec<Line<'static>> {
        self.body_lines(width, palette)
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        match key.key_id.as_deref() {
            Some("up") | Some("ctrl+k") => {
                self.move_up();
                HandleOutcome::Consumed
            }
            Some("down") | Some("ctrl+j") => {
                self.move_down();
                HandleOutcome::Consumed
            }
            Some("enter") => {
                self.confirm();
                self.done.store(true, Ordering::SeqCst);
                HandleOutcome::Consumed
            }
            Some("escape") => {
                self.cancel();
                self.done.store(true, Ordering::SeqCst);
                HandleOutcome::Consumed
            }
            // A modal selector owns every key so none reaches the editor beneath
            // (VAL-OVERLAY-005), even keys it does not act on.
            _ => HandleOutcome::Consumed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key_id(id: &str) -> RtKey {
        RtKey {
            key_id: Some(id.to_string()),
            raw: KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        }
    }

    fn selector(
        current: Option<ThinkingLevel>,
    ) -> (
        ThinkingSelector,
        mpsc::UnboundedReceiver<ThinkingOutcome>,
        DoneSignal,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let done = super::super::overlay::new_done_signal();
        (ThinkingSelector::new(current, tx, done.clone()), rx, done)
    }

    fn body_text(sel: &ThinkingSelector) -> String {
        sel.body_lines(80, &ThemePalette::default())
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<ThinkingOutcome>) -> Option<ThinkingOutcome> {
        rx.try_recv().ok()
    }

    // --- ladder rendering + budget descriptions (VAL-OVERLAY-025) ----------

    #[test]
    fn renders_the_full_ladder_with_token_budgets() {
        let (sel, _rx, _done) = selector(Some(ThinkingLevel::Medium));
        let body = body_text(&sel);
        for label in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
            assert!(body.contains(label), "ladder missing {label}: {body}");
        }
        // Token-budget descriptions accompany each level.
        assert!(body.contains("~8k tokens"), "medium budget missing: {body}");
        assert!(
            body.contains("No reasoning"),
            "off description missing: {body}"
        );
    }

    // --- cursor seeds to the current active level (VAL-OVERLAY-025) --------

    #[test]
    fn cursor_starts_on_the_current_level_and_bare_enter_keeps_it() {
        let (mut sel, mut rx, done) = selector(Some(ThinkingLevel::High));
        assert_eq!(
            sel.highlighted(),
            Some(ThinkingLevel::High),
            "cursor seeds to the current level"
        );
        assert!(
            body_text(&sel).contains("✓"),
            "current level is checkmarked"
        );

        sel.handle_key(&key_id("enter"));
        assert!(done.load(Ordering::SeqCst), "enter raises the done flag");
        assert_eq!(
            drain(&mut rx),
            Some(ThinkingOutcome::Selected(Some(ThinkingLevel::High)))
        );
    }

    #[test]
    fn cursor_starts_at_off_when_no_level_is_active() {
        let (sel, _rx, _done) = selector(None);
        assert_eq!(sel.selected_index(), 0, "off is the top row");
        assert_eq!(sel.highlighted(), None, "off = None");
    }

    // --- navigation wrap ---------------------------------------------------

    #[test]
    fn down_wraps_at_the_bottom_and_up_wraps_at_the_top() {
        let (mut sel, _rx, _done) = selector(None);
        assert_eq!(sel.selected_index(), 0);
        // Seven downs (ladder length) wrap back to the top.
        for _ in 0..7 {
            sel.handle_key(&key_id("down"));
        }
        assert_eq!(sel.selected_index(), 0, "down wraps at the bottom");
        sel.handle_key(&key_id("up"));
        assert_eq!(sel.selected_index(), 6, "up wraps at the top");
    }

    // --- Enter after navigation / Esc cancel ------------------------------

    #[test]
    fn down_then_enter_advances_one_step() {
        let (mut sel, mut rx, _done) = selector(None);
        sel.handle_key(&key_id("down")); // off -> minimal
        sel.handle_key(&key_id("enter"));
        assert_eq!(
            drain(&mut rx),
            Some(ThinkingOutcome::Selected(Some(ThinkingLevel::Minimal)))
        );
    }

    #[test]
    fn enter_at_off_emits_none() {
        let (mut sel, mut rx, _done) = selector(Some(ThinkingLevel::Low));
        sel.handle_key(&key_id("up")); // low -> minimal
        sel.handle_key(&key_id("up")); // minimal -> off
        sel.handle_key(&key_id("enter"));
        assert_eq!(drain(&mut rx), Some(ThinkingOutcome::Selected(None)));
    }

    #[test]
    fn escape_emits_cancelled_and_raises_done() {
        let (mut sel, mut rx, done) = selector(Some(ThinkingLevel::Medium));
        sel.handle_key(&key_id("escape"));
        assert!(done.load(Ordering::SeqCst), "escape raises the done flag");
        assert_eq!(drain(&mut rx), Some(ThinkingOutcome::Cancelled));
    }

    // --- argument parsing (direct-arg + off variants) ---------------------

    #[test]
    fn parse_level_arg_accepts_levels_and_off_variants() {
        assert_eq!(parse_level_arg("high"), Some(Some(ThinkingLevel::High)));
        assert_eq!(parse_level_arg("MAX"), Some(Some(ThinkingLevel::Max)));
        // off + its aliases all resolve to the off entry (no warning path).
        assert_eq!(parse_level_arg("off"), Some(None));
        assert_eq!(parse_level_arg("none"), Some(None));
        assert_eq!(parse_level_arg("clear"), Some(None));
        // Anything else is unknown (the yellow guidance path).
        assert_eq!(parse_level_arg("bogus"), None);
        assert_eq!(parse_level_arg(""), None);
    }
}
