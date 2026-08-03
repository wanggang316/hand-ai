//! The rt-native **login provider picker** — the `/login` provider list migrated
//! onto the [overlay runtime](super::overlay).
//!
//! Like the [`ModelSelector`](super::model_selector::ModelSelector) it is a
//! [`SelectorController`]: it produces rt-native styled [`Line`]s and consumes keys
//! while it is the mounted modal overlay. Its behaviour is the `/login` picker's:
//!
//! - one row per provider, each tagged with a **status badge** — green
//!   `configured` when a credential is on file, yellow `env detected` when only an
//!   environment variable is present (VAL-OVERLAY-029 badge removal is the driver's
//!   remit: it rebuilds this list after `/logout`);
//! - the auth **method** each provider uses (`oauth` / `api key`) so the user knows
//!   which flow the pick opens;
//! - **type-to-filter** (fuzzy) narrows the list live; a query with no matches shows
//!   a hint;
//! - **↑/↓ navigate** (no wrap — a bounded list reads more naturally clamped);
//! - **Enter** confirms the highlighted provider, **Esc** cancels — each emits
//!   exactly one [`LoginProviderOutcome`] and raises the
//!   [`DoneSignal`](super::overlay::DoneSignal) so the runtime unmounts it.
//!
//! The driver owns provider-list construction (badges, method) and the flow the
//! pick opens (OAuth vs key dialog); this component is pure UI + pick logic over its
//! constructor inputs — the same construct-in / channel-out selector shape the rest
//! of the family uses.

use std::cmp::Reverse;
use std::sync::atomic::Ordering;

use hand_tui::fuzzy::{FuzzyMatch, fuzzy_filter};
use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::HandleOutcome;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::mpsc;

use super::overlay::{DoneSignal, SelectorController};
use crate::modes::interactive::theme::ThemePalette;

/// The most provider rows shown at once; the window scrolls to keep the selection
/// visible.
const MAX_VISIBLE: usize = 10;

/// Which auth method a provider uses — surfaced as a row hint so the user knows the
/// pick opens the OAuth flow versus the API-key dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    /// The provider authenticates through an OAuth flow (auth-URL / device-code).
    Oauth,
    /// The provider authenticates with a pasted API key.
    ApiKey,
}

impl AuthMethod {
    fn label(self) -> &'static str {
        match self {
            AuthMethod::Oauth => "oauth",
            AuthMethod::ApiKey => "api key",
        }
    }
}

/// The credential state of a provider, rendered as a colored badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderBadge {
    /// A credential is on file (green `configured`).
    Configured,
    /// Only an environment variable is present (yellow `env detected`).
    EnvDetected,
    /// No credential (no badge).
    None,
}

/// One provider row in the picker.
#[derive(Debug, Clone)]
pub struct LoginProviderRow {
    /// Stable provider id (the storage key, e.g. `"anthropic"`).
    pub id: String,
    /// Display name (e.g. `"Anthropic"`).
    pub name: String,
    /// The credential badge shown to the right of the name.
    pub badge: ProviderBadge,
    /// The auth method the pick opens.
    pub method: AuthMethod,
}

/// The outcome the picker emits on its channel — exactly one per open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginProviderOutcome {
    /// The user confirmed a provider (Enter) — carries its stable id.
    Selected(String),
    /// The user cancelled (Esc).
    Cancelled,
}

/// The rt-native `/login` (and `/logout`) provider picker component.
pub struct LoginProviderPicker {
    /// The dialog title (login vs logout wording).
    title: String,
    /// The providers, in the order the caller supplied.
    providers: Vec<LoginProviderRow>,
    /// Indices into `providers` that survive the current filter, best-match first.
    filtered: Vec<usize>,
    /// The highlighted row, as an index into `filtered`.
    selected: usize,
    /// The live filter query typed by the user.
    query: String,
    /// The outcome channel; exactly one [`LoginProviderOutcome`] is sent.
    tx: mpsc::UnboundedSender<LoginProviderOutcome>,
    /// Raised on the terminal key (Enter/Esc) so the overlay runtime unmounts this.
    done: DoneSignal,
}

impl LoginProviderPicker {
    /// Build a picker over `providers` with the given `title`.
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        providers: Vec<LoginProviderRow>,
        tx: mpsc::UnboundedSender<LoginProviderOutcome>,
        done: DoneSignal,
    ) -> Self {
        let mut me = Self {
            title: title.into(),
            filtered: (0..providers.len()).collect(),
            providers,
            selected: 0,
            query: String::new(),
            tx,
            done,
        };
        me.refilter();
        me
    }

    /// The highlighted provider, if the filtered view is non-empty.
    #[must_use]
    pub fn highlighted(&self) -> Option<&LoginProviderRow> {
        let idx = *self.filtered.get(self.selected)?;
        self.providers.get(idx)
    }

    /// Re-derive the filtered view from the query, keeping the cursor in range. An
    /// empty query shows the whole list in its natural order; a non-empty query
    /// fuzzy-matches over `name` + `id`.
    fn refilter(&mut self) {
        if self.query.is_empty() {
            self.filtered = (0..self.providers.len()).collect();
        } else {
            let haystacks: Vec<String> = self
                .providers
                .iter()
                .map(|p| format!("{} {}", p.name, p.id))
                .collect();
            let refs: Vec<&str> = haystacks.iter().map(String::as_str).collect();
            let mut matches: Vec<(usize, FuzzyMatch)> = fuzzy_filter(&self.query, &refs);
            matches.sort_by_key(|m| Reverse(m.1.score));
            self.filtered = matches.into_iter().map(|(idx, _)| idx).collect();
        }
        if self.filtered.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
        }
    }

    /// Move the cursor up one row (clamped at the top).
    fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move the cursor down one row (clamped at the bottom).
    fn move_down(&mut self) {
        if self.selected + 1 < self.filtered.len() {
            self.selected += 1;
        }
    }

    /// Emit the highlighted provider as the confirmed outcome (Enter). Returns
    /// whether anything was emitted — a no-op when the filtered view is empty.
    fn confirm(&self) -> bool {
        if let Some(row) = self.highlighted() {
            let _ = self.tx.send(LoginProviderOutcome::Selected(row.id.clone()));
            true
        } else {
            false
        }
    }

    /// Emit the cancel outcome (Esc).
    fn cancel(&self) {
        let _ = self.tx.send(LoginProviderOutcome::Cancelled);
    }

    /// Whether `key` is a printable character to append to the filter query.
    fn typed_char(key: &RtKey) -> Option<char> {
        use crossterm::event::{KeyCode, KeyModifiers};
        if key
            .raw
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return None;
        }
        match key.raw.code {
            KeyCode::Char(c) => Some(c),
            _ => None,
        }
    }

    /// The visible slice `[start, end)` of the filtered view, windowed so the
    /// selection stays on screen.
    fn visible_window(&self) -> (usize, usize) {
        let count = self.filtered.len();
        if count <= MAX_VISIBLE {
            return (0, count);
        }
        let half = MAX_VISIBLE / 2;
        let start = self
            .selected
            .saturating_sub(half)
            .min(count.saturating_sub(MAX_VISIBLE));
        (start, (start + MAX_VISIBLE).min(count))
    }

    /// The picker body rendered as styled lines (title, query, rows or the empty
    /// hint, and the key hint), wrapped to `width`.
    fn body_lines(&self, width: u16, palette: &ThemePalette) -> Vec<Line<'static>> {
        let muted = Style::default().fg(palette.dim);
        let accent = Style::default().fg(palette.accent);

        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(Line::from(Span::styled(
            self.title.clone(),
            accent.add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(vec![
            Span::styled("Search: ", muted),
            Span::raw(self.query.clone()),
        ]));
        lines.push(Line::from(String::new()));

        if self.filtered.is_empty() {
            lines.push(Line::styled("  No matching providers".to_string(), muted));
        } else {
            let (start, end) = self.visible_window();
            for i in start..end {
                lines.push(provider_row(
                    &self.providers[self.filtered[i]],
                    i == self.selected,
                    palette,
                ));
            }
            let count = self.filtered.len();
            if end - start < count {
                lines.push(Line::from(Span::styled(
                    format!("  ({}/{})", self.selected + 1, count),
                    muted,
                )));
            }
        }

        lines.push(Line::from(String::new()));
        lines.push(Line::from(Span::styled(
            "↑/↓ navigate   Enter select   Esc cancel".to_string(),
            muted,
        )));

        let _ = width;
        lines
    }
}

/// Render one provider row: a cursor mark, the name, its credential badge, and the
/// auth-method hint. The highlighted row is accent-bold.
fn provider_row(row: &LoginProviderRow, selected: bool, palette: &ThemePalette) -> Line<'static> {
    let accent = Style::default().fg(palette.accent);
    let muted = Style::default().fg(palette.dim);
    let success = Style::default().fg(palette.success);
    let warning = Style::default().fg(palette.warning);

    let mut spans: Vec<Span<'static>> = Vec::new();
    if selected {
        spans.push(Span::styled("→ ".to_string(), accent));
        spans.push(Span::styled(
            row.name.clone(),
            accent.add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::raw("  ".to_string()));
        spans.push(Span::raw(row.name.clone()));
    }

    match row.badge {
        ProviderBadge::Configured => {
            spans.push(Span::styled("  configured".to_string(), success));
        }
        ProviderBadge::EnvDetected => {
            spans.push(Span::styled("  env detected".to_string(), warning));
        }
        ProviderBadge::None => {}
    }

    spans.push(Span::styled(format!("  [{}]", row.method.label()), muted));

    Line::from(spans)
}

impl SelectorController for LoginProviderPicker {
    fn render_lines(&self, width: u16, palette: &ThemePalette) -> Vec<Line<'static>> {
        self.body_lines(width, palette)
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        match key.key_id.as_deref() {
            Some("up") => {
                self.move_up();
                HandleOutcome::Consumed
            }
            Some("down") => {
                self.move_down();
                HandleOutcome::Consumed
            }
            Some("enter") => {
                // Enter on an empty filtered view is inert: nothing to confirm, so the
                // picker stays open and the done flag is not raised.
                if self.confirm() {
                    self.done.store(true, Ordering::SeqCst);
                }
                HandleOutcome::Consumed
            }
            Some("escape") => {
                self.cancel();
                self.done.store(true, Ordering::SeqCst);
                HandleOutcome::Consumed
            }
            Some("backspace") => {
                self.query.pop();
                self.refilter();
                HandleOutcome::Consumed
            }
            _ => {
                if let Some(c) = Self::typed_char(key) {
                    self.query.push(c);
                    self.refilter();
                }
                // A modal selector owns every key so it never reaches the editor
                // beneath (VAL-OVERLAY-005).
                HandleOutcome::Consumed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn row(id: &str, name: &str, badge: ProviderBadge, method: AuthMethod) -> LoginProviderRow {
        LoginProviderRow {
            id: id.to_string(),
            name: name.to_string(),
            badge,
            method,
        }
    }

    fn picker(
        rows: Vec<LoginProviderRow>,
    ) -> (
        LoginProviderPicker,
        mpsc::UnboundedReceiver<LoginProviderOutcome>,
        DoneSignal,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let done = super::super::overlay::new_done_signal();
        let picker = LoginProviderPicker::new("Login", rows, tx, done.clone());
        (picker, rx, done)
    }

    fn key_id(id: &str) -> RtKey {
        RtKey {
            key_id: Some(id.to_string()),
            raw: KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        }
    }

    fn char_key(c: char) -> RtKey {
        RtKey {
            key_id: Some(c.to_string()),
            raw: KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        }
    }

    fn body_text(p: &LoginProviderPicker) -> String {
        p.body_lines(80, &ThemePalette::default())
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

    fn drain(
        rx: &mut mpsc::UnboundedReceiver<LoginProviderOutcome>,
    ) -> Option<LoginProviderOutcome> {
        rx.try_recv().ok()
    }

    fn sample() -> Vec<LoginProviderRow> {
        vec![
            row(
                "anthropic",
                "Anthropic",
                ProviderBadge::Configured,
                AuthMethod::Oauth,
            ),
            row(
                "openai",
                "OpenAI",
                ProviderBadge::EnvDetected,
                AuthMethod::ApiKey,
            ),
            row(
                "google",
                "Google Gemini",
                ProviderBadge::None,
                AuthMethod::ApiKey,
            ),
        ]
    }

    // --- badges (VAL-OVERLAY badge rendering) -----------------------------

    #[test]
    fn renders_each_provider_with_its_badge_and_method() {
        let (p, _rx, _done) = picker(sample());
        let body = body_text(&p);
        assert!(body.contains("Anthropic"), "{body}");
        assert!(
            body.contains("configured"),
            "green configured badge: {body}"
        );
        assert!(body.contains("env detected"), "yellow env badge: {body}");
        assert!(body.contains("[oauth]"), "oauth method hint: {body}");
        assert!(body.contains("[api key]"), "api-key method hint: {body}");
        // A provider with no credential shows no badge word.
        let google_line = p
            .body_lines(80, &ThemePalette::default())
            .into_iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .find(|l| l.contains("Google"))
            .unwrap();
        assert!(
            !google_line.contains("configured") && !google_line.contains("env detected"),
            "unconfigured provider has no badge: {google_line}"
        );
    }

    // --- filter -----------------------------------------------------------

    #[test]
    fn typing_filters_the_provider_list() {
        let (mut p, _rx, _done) = picker(sample());
        for c in "anth".chars() {
            p.handle_key(&char_key(c));
        }
        let body = body_text(&p);
        assert!(body.contains("Anthropic"), "{body}");
        assert!(!body.contains("OpenAI"), "{body}");
        assert!(!body.contains("Google"), "{body}");
        // Backspace restores.
        for _ in 0..4 {
            p.handle_key(&key_id("backspace"));
        }
        assert!(body_text(&p).contains("OpenAI"));
    }

    #[test]
    fn no_match_shows_the_hint() {
        let (mut p, _rx, _done) = picker(sample());
        for c in "zzzzz".chars() {
            p.handle_key(&char_key(c));
        }
        assert!(body_text(&p).contains("No matching providers"));
    }

    // --- navigation (clamped) + Enter/Esc outcomes ------------------------

    #[test]
    fn enter_confirms_the_highlighted_provider_by_id() {
        let (mut p, mut rx, done) = picker(sample());
        p.handle_key(&key_id("down"));
        p.handle_key(&key_id("enter"));
        assert!(done.load(Ordering::SeqCst), "enter raises done");
        assert_eq!(
            drain(&mut rx),
            Some(LoginProviderOutcome::Selected("openai".to_string()))
        );
    }

    #[test]
    fn arrows_clamp_at_the_ends() {
        let (mut p, _rx, _done) = picker(sample());
        p.handle_key(&key_id("up")); // clamp at top
        assert_eq!(p.highlighted().map(|r| r.id.as_str()), Some("anthropic"));
        for _ in 0..5 {
            p.handle_key(&key_id("down")); // clamp at bottom
        }
        assert_eq!(p.highlighted().map(|r| r.id.as_str()), Some("google"));
    }

    #[test]
    fn escape_emits_cancelled_and_raises_done() {
        let (mut p, mut rx, done) = picker(sample());
        p.handle_key(&key_id("escape"));
        assert!(done.load(Ordering::SeqCst));
        assert_eq!(drain(&mut rx), Some(LoginProviderOutcome::Cancelled));
    }

    #[test]
    fn enter_on_an_empty_filtered_view_is_inert() {
        let (mut p, mut rx, done) = picker(sample());
        for c in "zzzzz".chars() {
            p.handle_key(&char_key(c));
        }
        p.handle_key(&key_id("enter"));
        assert!(!done.load(Ordering::SeqCst), "no confirm on empty view");
        assert!(drain(&mut rx).is_none());
    }
}
