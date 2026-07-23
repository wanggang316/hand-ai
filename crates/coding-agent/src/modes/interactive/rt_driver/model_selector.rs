//! The rt-native `/model` selector — the first selector migrated onto the
//! [overlay runtime](super::overlay).
//!
//! It is a [`SelectorController`]: it produces rt-native styled [`Line`]s (not the
//! legacy `Vec<String>` renderer) and consumes keys while it is the mounted modal
//! overlay. Its behaviour is the model picker's:
//!
//! - **↑/↓ navigate with wrap** across the filtered list;
//! - **type-to-filter** (fuzzy) narrows the list live; a query with no matches
//!   shows a "no matching models" hint, and clearing the query restores the full
//!   list (VAL-OVERLAY-006);
//! - **Tab** toggles the active list between the user's *scoped* subset and *all*
//!   models when a scoped subset exists, re-filtering against the new list and
//!   changing the visible count (VAL-OVERLAY-012);
//! - the **current** model is sorted to the top and pre-selected on open, so a bare
//!   Enter keeps the current model (VAL-OVERLAY-035);
//! - **Enter** confirms the highlighted model, **Esc** cancels — each emits exactly
//!   one [`ModelOutcome`] on the outcome channel and raises the
//!   [`DoneSignal`](super::overlay::DoneSignal) so the runtime unmounts it
//!   (VAL-OVERLAY-003).
//!
//! The driver owns registry access and persistence: it builds the `all`/`scoped`
//! lists, mounts this component, and applies the picked model. This component is
//! pure UI + pick logic over its constructor inputs — the reusable
//! construct-in / channel-out selector shape.

use std::cmp::Reverse;
use std::sync::atomic::Ordering;

use hand_tui::fuzzy::{FuzzyMatch, fuzzy_filter};
use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::HandleOutcome;
use model::Model;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::mpsc;

use super::overlay::{DoneSignal, SelectorController};

/// The most rows of the model list shown at once; the window scrolls to keep the
/// selection visible.
const MAX_VISIBLE: usize = 10;

/// Which list the selector currently surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelScope {
    /// Every model in the registry.
    All,
    /// The user's configured (`enabled_models`) subset.
    Scoped,
}

/// The outcome the selector emits on its channel — exactly one per open.
///
/// `Model` is large, so the success payload is boxed to keep the enum compact
/// (clippy::large_enum_variant), matching the legacy `ModelOutcome`.
#[derive(Debug, Clone)]
pub enum ModelOutcome {
    /// The user confirmed this model (Enter).
    Selected(Box<Model>),
    /// The user cancelled (Esc) — the current model is kept.
    Cancelled,
}

/// The rt-native `/model` picker component.
pub struct ModelSelector {
    /// The full registry list, with the current model sorted to the top.
    all_models: Vec<Model>,
    /// The user's scoped subset (`enabled_models`); empty disables the Tab toggle.
    scoped_models: Vec<Model>,
    /// The current model's `(provider, id)`, used for the checkmark + preselect.
    current: Option<(String, String)>,
    /// Which list is active right now.
    scope: ModelScope,
    /// Indices into the active list that survive the current filter, best-match
    /// first. This is the view order the list renders and the cursor indexes.
    filtered: Vec<usize>,
    /// The highlighted row, as an index into `filtered`.
    selected: usize,
    /// The live filter query typed by the user.
    query: String,
    /// The outcome channel; exactly one [`ModelOutcome`] is sent on confirm/cancel.
    tx: mpsc::UnboundedSender<ModelOutcome>,
    /// Raised on the terminal key (Enter/Esc) so the overlay runtime unmounts this.
    done: DoneSignal,
}

impl ModelSelector {
    /// Build a selector.
    ///
    /// * `current` — pre-selects (and checkmarks) the matching entry; sorted to the
    ///   top of `all_models` so a bare Enter keeps it (VAL-OVERLAY-035).
    /// * `all_models` — the full registry list.
    /// * `scoped_models` — the user's configured subset; empty means the Tab scope
    ///   toggle is disabled and the selector opens on `All`.
    /// * `tx` / `done` — the outcome channel and the runtime's unmount signal.
    #[must_use]
    pub fn new(
        current: Option<Model>,
        mut all_models: Vec<Model>,
        scoped_models: Vec<Model>,
        tx: mpsc::UnboundedSender<ModelOutcome>,
        done: DoneSignal,
    ) -> Self {
        // Current model first, then a stable provider/id order — so the current
        // model is the pre-selected top row.
        if let Some(cur) = current.as_ref() {
            all_models.sort_by(|a, b| {
                let a_cur = models_equal(a, cur);
                let b_cur = models_equal(b, cur);
                match (a_cur, b_cur) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a
                        .provider
                        .as_str()
                        .cmp(b.provider.as_str())
                        .then(a.id.cmp(&b.id)),
                }
            });
        }
        let current = current
            .as_ref()
            .map(|m| (m.provider.as_str().to_string(), m.id.clone()));
        let scope = if scoped_models.is_empty() {
            ModelScope::All
        } else {
            ModelScope::Scoped
        };

        let mut me = Self {
            all_models,
            scoped_models,
            current,
            scope,
            filtered: Vec::new(),
            selected: 0,
            query: String::new(),
            tx,
            done,
        };
        me.refilter();
        me.preselect_current();
        me
    }

    /// The list backing the current scope.
    fn active_models(&self) -> &[Model] {
        match self.scope {
            ModelScope::All => &self.all_models,
            ModelScope::Scoped => &self.scoped_models,
        }
    }

    /// Whether the Tab scope toggle is available (a scoped subset exists).
    #[must_use]
    pub fn has_scope_toggle(&self) -> bool {
        !self.scoped_models.is_empty()
    }

    /// The active scope (test/introspection aid).
    #[must_use]
    pub fn scope(&self) -> ModelScope {
        self.scope
    }

    /// The number of models currently visible after filtering (test aid).
    #[must_use]
    pub fn visible_count(&self) -> usize {
        self.filtered.len()
    }

    /// The highlighted model, if any is visible.
    #[must_use]
    pub fn highlighted(&self) -> Option<&Model> {
        let idx = *self.filtered.get(self.selected)?;
        self.active_models().get(idx)
    }

    /// Re-derive the filtered view from the query against the active list, keeping
    /// the cursor in range. An empty query shows the whole list in its natural
    /// order; a non-empty query fuzzy-matches `provider/id`, `id`, and `name`,
    /// best-score first.
    fn refilter(&mut self) {
        let query = self.query.clone();
        let active = self.active_models();
        if query.is_empty() {
            self.filtered = (0..active.len()).collect();
        } else {
            let haystacks: Vec<String> = active
                .iter()
                .map(|m| {
                    let p = m.provider.as_str();
                    format!("{id} {name} {p} {p}/{id}", id = m.id, name = m.name)
                })
                .collect();
            let refs: Vec<&str> = haystacks.iter().map(String::as_str).collect();
            let mut matches: Vec<(usize, FuzzyMatch)> = fuzzy_filter(&query, &refs);
            matches.sort_by_key(|m| Reverse(m.1.score));
            self.filtered = matches.into_iter().map(|(idx, _)| idx).collect();
        }
        // Keep the cursor valid: clamp into range, or reset when nothing matches.
        if self.filtered.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
        }
    }

    /// Move the cursor to the current model's row within the filtered view, so a
    /// bare Enter keeps the current model (VAL-OVERLAY-035).
    fn preselect_current(&mut self) {
        let Some((p, id)) = self.current.clone() else {
            return;
        };
        let active = self.active_models();
        if let Some(view_idx) = self.filtered.iter().position(|&i| {
            let m = &active[i];
            m.provider.as_str() == p && m.id == id
        }) {
            self.selected = view_idx;
        }
    }

    /// Toggle the active list between scoped and all (Tab), re-filtering and
    /// re-preselecting against the new list. A no-op when no scoped subset exists.
    fn toggle_scope(&mut self) {
        if self.scoped_models.is_empty() {
            return;
        }
        self.scope = match self.scope {
            ModelScope::All => ModelScope::Scoped,
            ModelScope::Scoped => ModelScope::All,
        };
        self.selected = 0;
        self.refilter();
        self.preselect_current();
    }

    /// Move the cursor up one row, wrapping to the bottom.
    fn move_up(&mut self) {
        let len = self.filtered.len();
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
        let len = self.filtered.len();
        if len == 0 {
            return;
        }
        self.selected = (self.selected + 1) % len;
    }

    /// Emit the highlighted model as the confirmed outcome (Enter). A no-op when the
    /// filtered view is empty — there is nothing to confirm.
    fn confirm(&self) {
        if let Some(model) = self.highlighted() {
            let _ = self
                .tx
                .send(ModelOutcome::Selected(Box::new(model.clone())));
        }
    }

    /// Emit the cancel outcome (Esc) — the current model is kept.
    fn cancel(&self) {
        let _ = self.tx.send(ModelOutcome::Cancelled);
    }

    /// Whether `id` is a printable character to append to the filter query. Uses the
    /// raw crossterm event so the exact typed character (with its case) is kept.
    fn typed_char(key: &RtKey) -> Option<char> {
        use crossterm::event::{KeyCode, KeyModifiers};
        // Ignore control-modified keys (Ctrl/Alt/Super chords are not filter text).
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

    /// The list body rendered as styled lines (the header, the query line, the
    /// windowed rows, and the footer), wrapped to `width`.
    fn body_lines(&self, width: u16) -> Vec<Line<'static>> {
        let muted = Style::default().fg(Color::DarkGray);
        let accent = Style::default().fg(Color::Cyan);
        let success = Style::default().fg(Color::Green);
        let warning = Style::default().fg(Color::Yellow);

        let mut lines: Vec<Line<'static>> = Vec::new();

        // Header: scope indicator + Tab hint, or the login hint when no scoped
        // subset is configured.
        if self.scoped_models.is_empty() {
            lines.push(Line::styled(
                "Only showing models from configured providers. Use /login to add providers."
                    .to_string(),
                warning,
            ));
        } else {
            let (all_style, scoped_style) = match self.scope {
                ModelScope::All => (accent, muted),
                ModelScope::Scoped => (muted, accent),
            };
            lines.push(Line::from(vec![
                Span::styled("Scope: ", muted),
                Span::styled("all", all_style),
                Span::styled(" | ", muted),
                Span::styled("scoped", scoped_style),
                Span::styled("   (Tab to toggle)", muted),
            ]));
        }

        // The live query line.
        lines.push(Line::from(vec![
            Span::styled("Search: ", muted),
            Span::raw(self.query.clone()),
        ]));
        lines.push(Line::from(String::new()));

        // The windowed list, or the zero-match hint.
        if self.filtered.is_empty() {
            lines.push(Line::styled("  No matching models".to_string(), muted));
        } else {
            let (start, end) = self.visible_window();
            let active = self.active_models();
            for i in start..end {
                let model = &active[self.filtered[i]];
                let is_selected = i == self.selected;
                let is_current = self
                    .current
                    .as_ref()
                    .is_some_and(|(p, id)| model.provider.as_str() == p && &model.id == id);

                let mut spans: Vec<Span<'static>> = Vec::new();
                if is_selected {
                    spans.push(Span::styled("→ ".to_string(), accent));
                    spans.push(Span::styled(
                        model.id.clone(),
                        accent.add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::raw("  ".to_string()));
                    spans.push(Span::raw(model.id.clone()));
                }
                spans.push(Span::raw(" ".to_string()));
                spans.push(Span::styled(
                    format!("[{}]", model.provider.as_str()),
                    muted,
                ));
                if is_current {
                    spans.push(Span::styled(" ✓".to_string(), success));
                }
                lines.push(Line::from(spans));
            }

            // A position footnote when the list is windowed.
            let count = self.filtered.len();
            if end - start < count {
                lines.push(Line::styled(
                    format!("  ({}/{})", self.selected + 1, count),
                    muted,
                ));
            }

            // The friendly model-name footer for the highlighted row.
            if let Some(model) = self.highlighted() {
                lines.push(Line::from(String::new()));
                lines.push(Line::styled(format!("  {}", model.name), muted));
            }
        }

        let _ = width;
        lines
    }
}

impl SelectorController for ModelSelector {
    fn render_lines(&self, width: u16) -> Vec<Line<'static>> {
        // The scheduler paints these inside the anchored, bordered, dimmed overlay
        // rect; it clips rows/cols wider than the interior so a small terminal never
        // spills past the border.
        self.body_lines(width)
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
            Some("tab") | Some("shift+tab") => {
                self.toggle_scope();
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
                // A modal selector owns every key: even one it does not act on is
                // consumed so it never reaches the editor beneath (VAL-OVERLAY-005).
                HandleOutcome::Consumed
            }
        }
    }
}

/// Whether two models are the same `(provider, id)` pair.
fn models_equal(a: &Model, b: &Model) -> bool {
    a.id == b.id && a.provider == b.provider
}

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use model::types::Provider;
    use model::{Api, Cost, InputType};

    fn make_model(provider: Provider, id: &str, name: &str) -> Model {
        Model {
            id: id.to_string(),
            name: name.to_string(),
            api: Api::AnthropicMessages,
            provider,
            base_url: String::new(),
            reasoning: false,
            input: vec![InputType::Text],
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 0,
            max_tokens: 0,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    fn three_models() -> Vec<Model> {
        vec![
            make_model(Provider::Anthropic, "claude-sonnet", "Claude Sonnet"),
            make_model(Provider::OpenAI, "gpt-4o", "GPT-4o"),
            make_model(Provider::Google, "gemini-2-pro", "Gemini 2 Pro"),
        ]
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

    fn selector(
        current: Option<Model>,
        all: Vec<Model>,
        scoped: Vec<Model>,
    ) -> (
        ModelSelector,
        mpsc::UnboundedReceiver<ModelOutcome>,
        DoneSignal,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let done = super::super::overlay::new_done_signal();
        let sel = ModelSelector::new(current, all, scoped, tx, done.clone());
        (sel, rx, done)
    }

    fn body_text(sel: &ModelSelector) -> String {
        sel.body_lines(80)
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

    fn drain(rx: &mut mpsc::UnboundedReceiver<ModelOutcome>) -> Option<ModelOutcome> {
        rx.try_recv().ok()
    }

    // --- rendering + preselect (VAL-OVERLAY-035) --------------------------

    #[test]
    fn renders_models_and_provider_badges() {
        let (sel, _rx, _done) = selector(None, three_models(), vec![]);
        let body = body_text(&sel);
        assert!(body.contains("claude-sonnet"), "{body}");
        assert!(body.contains("gpt-4o"), "{body}");
        assert!(body.contains("gemini-2-pro"), "{body}");
        assert!(body.contains("[anthropic]"), "{body}");
    }

    #[test]
    fn current_model_is_checkmarked_preselected_and_kept_by_bare_enter() {
        // VAL-OVERLAY-035: the current model sorts to the top, is checkmarked and
        // pre-selected, so an immediate Enter re-picks it unchanged.
        let models = three_models();
        let current = models[1].clone(); // gpt-4o
        let (mut sel, mut rx, done) = selector(Some(current.clone()), models, vec![]);

        assert!(
            body_text(&sel).contains("✓"),
            "current model is checkmarked"
        );
        assert_eq!(
            sel.highlighted().map(|m| m.id.as_str()),
            Some("gpt-4o"),
            "current model is pre-selected"
        );

        sel.handle_key(&key_id("enter"));
        assert!(done.load(Ordering::SeqCst), "enter raises the done flag");
        match drain(&mut rx) {
            Some(ModelOutcome::Selected(m)) => {
                assert_eq!(m.id, current.id);
                assert_eq!(m.provider, current.provider);
            }
            other => panic!("expected Selected(current), got {other:?}"),
        }
    }

    // --- navigation wrap ---------------------------------------------------

    #[test]
    fn down_wraps_at_the_bottom_and_up_wraps_at_the_top() {
        let (mut sel, _rx, _done) = selector(None, three_models(), vec![]);
        assert_eq!(sel.selected, 0);
        // Three downs wrap back to the top.
        for _ in 0..3 {
            sel.handle_key(&key_id("down"));
        }
        assert_eq!(sel.selected, 0, "down wraps at the bottom");
        // Up from the top wraps to the last row.
        sel.handle_key(&key_id("up"));
        assert_eq!(sel.selected, 2, "up wraps at the top");
    }

    // --- type-to-filter narrow / clear / zero-match (VAL-OVERLAY-006) -----

    #[test]
    fn typing_narrows_the_list_then_clearing_restores_it() {
        let (mut sel, _rx, _done) = selector(None, three_models(), vec![]);
        for c in "claude".chars() {
            sel.handle_key(&char_key(c));
        }
        let narrowed = body_text(&sel);
        assert!(narrowed.contains("claude-sonnet"), "{narrowed}");
        assert!(!narrowed.contains("gpt-4o"), "{narrowed}");
        assert_eq!(sel.visible_count(), 1, "filter narrowed to one match");

        // Backspace the whole query: the full list returns.
        for _ in 0.."claude".len() {
            sel.handle_key(&key_id("backspace"));
        }
        assert!(sel.query.is_empty(), "query cleared");
        assert_eq!(sel.visible_count(), 3, "clearing restores the full list");
    }

    #[test]
    fn a_query_with_no_matches_shows_the_zero_match_hint() {
        let (mut sel, _rx, _done) = selector(None, three_models(), vec![]);
        for c in "zzzzz".chars() {
            sel.handle_key(&char_key(c));
        }
        assert_eq!(sel.visible_count(), 0, "no models match");
        assert!(
            body_text(&sel).contains("No matching models"),
            "zero-match hint expected: {}",
            body_text(&sel)
        );
    }

    // --- Tab scope toggle (VAL-OVERLAY-012) -------------------------------

    #[test]
    fn tab_toggles_scope_and_changes_the_visible_count() {
        let models = three_models();
        let scoped = vec![models[0].clone()]; // one scoped model
        let (mut sel, _rx, _done) = selector(None, models, scoped);

        // With a scoped subset the selector opens scoped: one model visible.
        assert_eq!(sel.scope(), ModelScope::Scoped);
        assert_eq!(
            sel.visible_count(),
            1,
            "scoped shows the one configured model"
        );

        // Tab → all: the count grows to the full registry.
        sel.handle_key(&key_id("tab"));
        assert_eq!(sel.scope(), ModelScope::All);
        assert_eq!(sel.visible_count(), 3, "all shows every model");

        // Tab again → back to scoped.
        sel.handle_key(&key_id("tab"));
        assert_eq!(sel.scope(), ModelScope::Scoped);
        assert_eq!(sel.visible_count(), 1);
    }

    #[test]
    fn without_a_scoped_subset_tab_is_inert_and_the_login_hint_shows() {
        let (mut sel, _rx, _done) = selector(None, three_models(), vec![]);
        assert!(!sel.has_scope_toggle(), "no scoped subset → no toggle");
        assert_eq!(sel.scope(), ModelScope::All);
        sel.handle_key(&key_id("tab"));
        assert_eq!(
            sel.scope(),
            ModelScope::All,
            "tab is inert without a subset"
        );
        assert!(
            body_text(&sel).contains("Use /login to add providers"),
            "the login hint is shown when no scoped subset is configured"
        );
    }

    // --- Enter / Esc outcomes (VAL-OVERLAY-003) ---------------------------

    #[test]
    fn escape_emits_cancelled_and_raises_done() {
        let (mut sel, mut rx, done) = selector(None, three_models(), vec![]);
        sel.handle_key(&key_id("escape"));
        assert!(done.load(Ordering::SeqCst), "escape raises the done flag");
        assert!(matches!(drain(&mut rx), Some(ModelOutcome::Cancelled)));
    }

    #[test]
    fn enter_confirms_the_highlighted_model_after_navigation() {
        let (mut sel, mut rx, _done) = selector(None, three_models(), vec![]);
        // Move to the second row and confirm it.
        sel.handle_key(&key_id("down"));
        let expected = sel.highlighted().unwrap().id.clone();
        sel.handle_key(&key_id("enter"));
        match drain(&mut rx) {
            Some(ModelOutcome::Selected(m)) => assert_eq!(m.id, expected),
            other => panic!("expected Selected, got {other:?}"),
        }
    }

    #[test]
    fn a_ctrl_chord_is_consumed_but_not_treated_as_filter_text() {
        // A modal selector consumes Ctrl chords (they never reach the editor) but
        // must not append them to the filter query.
        let (mut sel, _rx, _done) = selector(None, three_models(), vec![]);
        let ctrl_a = RtKey {
            key_id: Some("ctrl+a".to_string()),
            raw: KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        };
        let outcome = sel.handle_key(&ctrl_a);
        assert_eq!(
            outcome,
            HandleOutcome::Consumed,
            "modal: every key consumed"
        );
        assert!(sel.query.is_empty(), "a Ctrl chord is not filter text");
    }
}
