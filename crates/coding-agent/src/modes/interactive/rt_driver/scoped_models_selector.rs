//! The rt-native `/scoped-models` selector — the filtered-set batch multi-select
//! built on the [overlay runtime](super::overlay).
//!
//! It is a [`SelectorController`]: it produces rt-native styled [`Line`]s and
//! consumes keys while it is the mounted modal overlay. Its behaviour is the
//! scoped-models multi-select's (VAL-OVERLAY-011 / -031 / -033):
//!
//! - the list is every registry model with an enabled ✓ / disabled ✗ marker;
//!   `enabled_ids == None` means "all enabled" (no explicit subset yet), `Some(vec)`
//!   is an explicit ordered subset of `provider/id` strings;
//! - **type-to-filter** narrows the list; the batch gestures below act on the
//!   **current filtered set** when a query is active, else on the whole list;
//! - **Enter** toggles the highlighted model;
//! - **Ctrl+A** enables every model in the current filter, **Ctrl+X** clears every
//!   model in the current filter (the filtered-set batch, VAL-OVERLAY-011);
//! - **Ctrl+T** toggles the highlighted model's whole provider on/off in bulk;
//! - **Ctrl+↑ / Ctrl+↓** reorder the highlighted (enabled) model within the subset;
//! - **Ctrl+S** "saves" — but this is **session-only**: it lands the
//!   `session-only — persist not yet wired` notice and does *not* write settings
//!   (the parity nail VAL-OVERLAY-031; a reopen shows the unchanged on-disk config);
//! - **Ctrl+C is two-stage** — with a non-empty query it clears the search only; with
//!   an empty query it cancels (VAL-OVERLAY-033). Esc always cancels;
//! - **↑/↓ navigate with wrap** (the per-selector nav nail VAL-OVERLAY-002 pins
//!   `/scoped-models` as *wrap*).
//!
//! The driver owns applying the session-only change (in-memory only) and the
//! `no models` no-data degradation (VAL-OVERLAY-019); this component is pure UI +
//! pick logic over its constructor inputs — the reusable construct-in / channel-out
//! selector shape.

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
use crate::modes::interactive::theme::ThemePalette;

/// The most rows shown at once; the window scrolls to keep the selection visible.
const MAX_VISIBLE: usize = 8;

/// The honest Ctrl+S notice: the scoped set is a **session-only** change until
/// persistence is wired (VAL-OVERLAY-031). Pinned as a constant so the driver's
/// status line and the footer hint agree.
pub const SESSION_ONLY_NOTICE: &str = "session-only — persist not yet wired";

/// The `provider/id` string for a model — the stable identity used in `enabled_ids`.
#[must_use]
pub fn full_id(model: &Model) -> String {
    format!("{}/{}", model.provider.as_str(), model.id)
}

/// Whether `id` is enabled given the current `enabled` view (`None` = all enabled).
fn is_enabled(enabled: &Option<Vec<String>>, id: &str) -> bool {
    match enabled {
        None => true,
        Some(list) => list.iter().any(|x| x == id),
    }
}

/// Toggle one id in the enabled view. `None` (all enabled) becomes the singleton
/// `Some([id])` — TS parity: the first explicit toggle seeds the subset.
fn toggle(enabled: Option<Vec<String>>, id: &str) -> Option<Vec<String>> {
    match enabled {
        None => Some(vec![id.to_string()]),
        Some(mut list) => {
            if let Some(pos) = list.iter().position(|x| x == id) {
                list.remove(pos);
            } else {
                list.push(id.to_string());
            }
            Some(list)
        }
    }
}

/// Enable every id in `targets` (or every id when `targets` is `None`). When the
/// result covers the whole registry it collapses back to `None` (all enabled).
fn enable_all(
    enabled: Option<Vec<String>>,
    all_ids: &[String],
    targets: Option<&[String]>,
) -> Option<Vec<String>> {
    match enabled {
        None => None,
        Some(mut list) => {
            let pool: Vec<String> = match targets {
                Some(t) => t.to_vec(),
                None => all_ids.to_vec(),
            };
            for id in pool {
                if !list.contains(&id) {
                    list.push(id);
                }
            }
            if list.len() == all_ids.len() {
                None
            } else {
                Some(list)
            }
        }
    }
}

/// Clear (disable) every id in `targets` (or every id when `targets` is `None`).
fn clear_all(
    enabled: Option<Vec<String>>,
    all_ids: &[String],
    targets: Option<&[String]>,
) -> Option<Vec<String>> {
    match enabled {
        None => match targets {
            Some(t) => Some(
                all_ids
                    .iter()
                    .filter(|id| !t.contains(id))
                    .cloned()
                    .collect(),
            ),
            None => Some(Vec::new()),
        },
        Some(list) => {
            let target_set: Vec<String> = targets
                .map(<[String]>::to_vec)
                .unwrap_or_else(|| list.clone());
            Some(
                list.into_iter()
                    .filter(|id| !target_set.contains(id))
                    .collect(),
            )
        }
    }
}

/// Move one id `delta` positions within the enabled subset (bounded), leaving a
/// `None` (all-enabled) view untouched.
fn move_id(enabled: Option<Vec<String>>, id: &str, delta: i32) -> Option<Vec<String>> {
    let mut list = enabled?;
    let Some(idx) = list.iter().position(|x| x == id) else {
        return Some(list);
    };
    let new_idx = idx as i32 + delta;
    if new_idx < 0 || (new_idx as usize) >= list.len() {
        return Some(list);
    }
    list.swap(idx, new_idx as usize);
    Some(list)
}

/// The display order of all ids given the enabled view: the enabled subset first (in
/// its stored order), then the remaining registry ids.
fn sorted_ids(enabled: &Option<Vec<String>>, all_ids: &[String]) -> Vec<String> {
    match enabled {
        None => all_ids.to_vec(),
        Some(list) => {
            let mut out = list.clone();
            for id in all_ids {
                if !out.contains(id) {
                    out.push(id.clone());
                }
            }
            out
        }
    }
}

/// One rendered/model item after ordering + filtering.
#[derive(Debug, Clone)]
struct ModelItem {
    full_id: String,
    id: String,
    provider: model::types::Provider,
    enabled: bool,
}

/// The outcome the selector emits on its channel — exactly one per open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedModelsOutcome {
    /// The user "saved" (Ctrl+S). Session-only: the driver keeps the subset in memory
    /// and lands the persist-not-wired notice (VAL-OVERLAY-031). Carries the subset
    /// (`None` = all enabled).
    Saved(Option<Vec<String>>),
    /// The user cancelled (Esc, or Ctrl+C with an empty query) — nothing changes.
    Cancelled,
}

/// The rt-native `/scoped-models` multi-select component.
pub struct ScopedModelsSelector {
    /// Every registry id, in registry order.
    all_ids: Vec<String>,
    /// The registry models keyed by `full_id` (for provider batch + display).
    models: Vec<(String, Model)>,
    /// The current enabled view (`None` = all enabled).
    enabled_ids: Option<Vec<String>>,
    /// The current filter query.
    query: String,
    /// The filtered, ordered items the list renders and the cursor indexes.
    filtered: Vec<ModelItem>,
    /// The highlighted row (index into `filtered`).
    selected: usize,
    /// The outcome channel; exactly one [`ScopedModelsOutcome`] is sent on save/cancel.
    tx: mpsc::UnboundedSender<ScopedModelsOutcome>,
    /// Raised on the terminal key (Ctrl+S / Esc / Ctrl+C-when-empty) so the runtime
    /// unmounts this.
    done: DoneSignal,
}

impl ScopedModelsSelector {
    /// Build a selector over `all_models` seeded with `enabled_ids` (`None` = all
    /// enabled).
    #[must_use]
    pub fn new(
        all_models: Vec<Model>,
        enabled_ids: Option<Vec<String>>,
        tx: mpsc::UnboundedSender<ScopedModelsOutcome>,
        done: DoneSignal,
    ) -> Self {
        let mut all_ids = Vec::with_capacity(all_models.len());
        let mut models = Vec::with_capacity(all_models.len());
        for model in all_models {
            let id = full_id(&model);
            all_ids.push(id.clone());
            models.push((id, model));
        }
        let mut me = Self {
            all_ids,
            models,
            enabled_ids,
            query: String::new(),
            filtered: Vec::new(),
            selected: 0,
            tx,
            done,
        };
        me.rebuild();
        me
    }

    /// The current enabled subset (`None` = all enabled) — test/introspection aid.
    #[must_use]
    pub fn enabled_ids(&self) -> Option<&[String]> {
        self.enabled_ids.as_deref()
    }

    /// The count of currently-visible (filtered) rows (test aid).
    #[must_use]
    pub fn visible_count(&self) -> usize {
        self.filtered.len()
    }

    /// The highlighted row index (test aid).
    #[must_use]
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// The current query (test aid).
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Look up a registry model by its `full_id`.
    fn model_of(&self, full_id: &str) -> Option<&Model> {
        self.models
            .iter()
            .find(|(id, _)| id == full_id)
            .map(|(_, m)| m)
    }

    /// The `full_id`s of the currently-filtered rows (the batch-gesture target set
    /// when a query is active).
    fn filtered_ids(&self) -> Vec<String> {
        self.filtered.iter().map(|i| i.full_id.clone()).collect()
    }

    /// Re-derive the ordered + filtered items from the enabled view and the query,
    /// keeping the cursor in range.
    fn rebuild(&mut self) {
        let ordered = sorted_ids(&self.enabled_ids, &self.all_ids);
        let items: Vec<ModelItem> = ordered
            .into_iter()
            .filter_map(|id| {
                self.model_of(&id).map(|m| ModelItem {
                    full_id: id.clone(),
                    id: m.id.clone(),
                    provider: m.provider,
                    enabled: is_enabled(&self.enabled_ids, &id),
                })
            })
            .collect();

        if self.query.is_empty() {
            self.filtered = items;
        } else {
            let haystacks: Vec<String> = items
                .iter()
                .map(|i| format!("{} {}", i.id, i.provider.as_str()))
                .collect();
            let refs: Vec<&str> = haystacks.iter().map(String::as_str).collect();
            let mut matches: Vec<(usize, FuzzyMatch)> = fuzzy_filter(&self.query, &refs);
            matches.sort_by_key(|m| Reverse(m.1.score));
            self.filtered = matches
                .into_iter()
                .map(|(idx, _)| items[idx].clone())
                .collect();
        }

        if self.filtered.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
        }
    }

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

    fn move_down(&mut self) {
        let len = self.filtered.len();
        if len == 0 {
            return;
        }
        self.selected = (self.selected + 1) % len;
    }

    fn toggle_selected(&mut self) {
        let Some(item) = self.filtered.get(self.selected).cloned() else {
            return;
        };
        self.enabled_ids = toggle(self.enabled_ids.take(), &item.full_id);
        self.rebuild();
    }

    /// The batch-gesture target: the filtered set when a query is active, else the
    /// whole list (`None`).
    fn batch_targets(&self) -> Option<Vec<String>> {
        if self.query.is_empty() {
            None
        } else {
            Some(self.filtered_ids())
        }
    }

    fn enable_all_gesture(&mut self) {
        let targets = self.batch_targets();
        self.enabled_ids = enable_all(self.enabled_ids.take(), &self.all_ids, targets.as_deref());
        self.rebuild();
    }

    fn clear_all_gesture(&mut self) {
        let targets = self.batch_targets();
        self.enabled_ids = clear_all(self.enabled_ids.take(), &self.all_ids, targets.as_deref());
        self.rebuild();
    }

    fn toggle_provider_gesture(&mut self) {
        let Some(item) = self.filtered.get(self.selected).cloned() else {
            return;
        };
        let provider = item.provider;
        let provider_ids: Vec<String> = self
            .all_ids
            .iter()
            .filter(|id| self.model_of(id).is_some_and(|m| m.provider == provider))
            .cloned()
            .collect();
        let all_on = provider_ids
            .iter()
            .all(|id| is_enabled(&self.enabled_ids, id));
        self.enabled_ids = if all_on {
            clear_all(self.enabled_ids.take(), &self.all_ids, Some(&provider_ids))
        } else {
            enable_all(self.enabled_ids.take(), &self.all_ids, Some(&provider_ids))
        };
        self.rebuild();
    }

    fn reorder(&mut self, going_up: bool) {
        let Some(item) = self.filtered.get(self.selected).cloned() else {
            return;
        };
        // Reorder only applies to an explicit, enabled subset.
        if self.enabled_ids.is_none() || !is_enabled(&self.enabled_ids, &item.full_id) {
            return;
        }
        let delta = if going_up { -1 } else { 1 };
        self.enabled_ids = move_id(self.enabled_ids.take(), &item.full_id, delta);
        // The selection follows the moved item.
        if going_up && self.selected > 0 {
            self.selected -= 1;
        } else if !going_up {
            self.selected += 1;
        }
        self.rebuild();
    }

    fn save(&self) {
        let _ = self
            .tx
            .send(ScopedModelsOutcome::Saved(self.enabled_ids.clone()));
    }

    fn cancel(&self) {
        let _ = self.tx.send(ScopedModelsOutcome::Cancelled);
    }

    /// Ctrl+C: two-stage. A non-empty query clears the search (returns `false`, so
    /// the dialog stays open); an empty query cancels (returns `true`, terminal).
    fn ctrl_c(&mut self) -> bool {
        if self.query.is_empty() {
            self.cancel();
            true
        } else {
            self.query.clear();
            self.rebuild();
            false
        }
    }

    /// Whether `key` is a printable char to append to the filter query (Ctrl/Alt/Super
    /// chords are never filter text).
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

    fn body_lines(&self, _width: u16, palette: &ThemePalette) -> Vec<Line<'static>> {
        let muted = Style::default().fg(palette.dim);
        let accent = Style::default().fg(palette.accent);
        let success = Style::default().fg(palette.success);
        let dim = Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM);

        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(Span::styled(
            "Model Configuration".to_string(),
            accent.add_modifier(Modifier::BOLD),
        )));
        // The honest session-only wording (parity nail VAL-OVERLAY-031): the overlay
        // must not claim it saves to settings.
        lines.push(Line::from(Span::styled(
            format!("Session-only. Ctrl+S applies for this session ({SESSION_ONLY_NOTICE})."),
            muted,
        )));
        lines.push(Line::from(vec![
            Span::styled("Search: ", muted),
            Span::raw(self.query.clone()),
        ]));
        lines.push(Line::from(String::new()));

        let all_enabled = self.enabled_ids.is_none();
        if self.filtered.is_empty() {
            lines.push(Line::from(Span::styled(
                "  No matching models".to_string(),
                muted,
            )));
        } else {
            let (start, end) = self.visible_window();
            for i in start..end {
                let item = &self.filtered[i];
                let is_selected = i == self.selected;

                let mut spans: Vec<Span<'static>> = Vec::new();
                if is_selected {
                    spans.push(Span::styled("→ ".to_string(), accent));
                    spans.push(Span::styled(
                        item.id.clone(),
                        accent.add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::raw("  ".to_string()));
                    spans.push(Span::raw(item.id.clone()));
                }
                spans.push(Span::styled(
                    format!(" [{}]", item.provider.as_str()),
                    muted,
                ));
                if all_enabled {
                    // Nothing: an all-enabled view shows no per-row marker.
                } else if item.enabled {
                    spans.push(Span::styled(" ✓".to_string(), success));
                } else {
                    spans.push(Span::styled(" ✗".to_string(), dim));
                }
                lines.push(Line::from(spans));
            }

            let count = self.filtered.len();
            if end - start < count {
                lines.push(Line::from(Span::styled(
                    format!("  ({}/{count})", self.selected + 1),
                    muted,
                )));
            }
        }

        lines.push(Line::from(String::new()));
        let enabled_summary = if all_enabled {
            "all enabled".to_string()
        } else {
            let n = self.enabled_ids.as_ref().map_or(0, Vec::len);
            format!("{n}/{} enabled", self.all_ids.len())
        };
        lines.push(Line::from(Span::styled(
            format!(
                "  Enter toggle · Ctrl+A all · Ctrl+X clear · Ctrl+T provider · Ctrl+↑/↓ reorder · Ctrl+S apply · {enabled_summary}"
            ),
            dim,
        )));
        lines
    }
}

impl SelectorController for ScopedModelsSelector {
    fn render_lines(&self, width: u16, palette: &ThemePalette) -> Vec<Line<'static>> {
        self.body_lines(width, palette)
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        match key.key_id.as_deref() {
            Some("up") => self.move_up(),
            Some("down") => self.move_down(),
            Some("ctrl+up") => self.reorder(true),
            Some("ctrl+down") => self.reorder(false),
            Some("enter") => self.toggle_selected(),
            Some("ctrl+a") => self.enable_all_gesture(),
            Some("ctrl+x") => self.clear_all_gesture(),
            Some("ctrl+t") => self.toggle_provider_gesture(),
            Some("ctrl+s") => {
                self.save();
                self.done.store(true, Ordering::SeqCst);
            }
            Some("ctrl+c") => {
                if self.ctrl_c() {
                    self.done.store(true, Ordering::SeqCst);
                }
            }
            Some("escape") => {
                self.cancel();
                self.done.store(true, Ordering::SeqCst);
            }
            Some("backspace") => {
                self.query.pop();
                self.rebuild();
            }
            _ => {
                if let Some(c) = Self::typed_char(key) {
                    self.query.push(c);
                    self.rebuild();
                }
            }
        }
        // A modal selector owns every key so none reaches the editor beneath
        // (VAL-OVERLAY-005), even keys it does not act on.
        HandleOutcome::Consumed
    }
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

    fn catalog() -> Vec<Model> {
        vec![
            make_model(Provider::Anthropic, "claude-sonnet", "Claude Sonnet"),
            make_model(Provider::Anthropic, "claude-haiku", "Claude Haiku"),
            make_model(Provider::OpenAI, "gpt-4o", "GPT-4o"),
            make_model(Provider::Google, "gemini-2-pro", "Gemini 2 Pro"),
        ]
    }

    fn key_id(id: &str) -> RtKey {
        let (code, mods) = match id {
            "ctrl+a" => (KeyCode::Char('a'), KeyModifiers::CONTROL),
            "ctrl+x" => (KeyCode::Char('x'), KeyModifiers::CONTROL),
            "ctrl+t" => (KeyCode::Char('t'), KeyModifiers::CONTROL),
            "ctrl+s" => (KeyCode::Char('s'), KeyModifiers::CONTROL),
            "ctrl+c" => (KeyCode::Char('c'), KeyModifiers::CONTROL),
            "ctrl+up" => (KeyCode::Up, KeyModifiers::CONTROL),
            "ctrl+down" => (KeyCode::Down, KeyModifiers::CONTROL),
            _ => (KeyCode::Esc, KeyModifiers::NONE),
        };
        RtKey {
            key_id: Some(id.to_string()),
            raw: KeyEvent::new(code, mods),
        }
    }

    fn char_key(c: char) -> RtKey {
        RtKey {
            key_id: Some(c.to_string()),
            raw: KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        }
    }

    fn selector(
        enabled: Option<Vec<String>>,
    ) -> (
        ScopedModelsSelector,
        mpsc::UnboundedReceiver<ScopedModelsOutcome>,
        DoneSignal,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let done = super::super::overlay::new_done_signal();
        (
            ScopedModelsSelector::new(catalog(), enabled, tx, done.clone()),
            rx,
            done,
        )
    }

    fn body_text(sel: &ScopedModelsSelector) -> String {
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

    fn drain(rx: &mut mpsc::UnboundedReceiver<ScopedModelsOutcome>) -> Vec<ScopedModelsOutcome> {
        let mut out = Vec::new();
        while let Ok(o) = rx.try_recv() {
            out.push(o);
        }
        out
    }

    // --- Enter toggle (VAL-OVERLAY-011) ----------------------------------------

    #[test]
    fn enter_toggles_the_highlighted_model_off_from_all_enabled() {
        // From `None` (all enabled), toggling the first model seeds an explicit
        // subset with just that id (TS parity).
        let (mut sel, _rx, _done) = selector(None);
        sel.handle_key(&key_id("enter"));
        let ids = sel.enabled_ids().unwrap();
        assert_eq!(ids.len(), 1);
        assert!(ids[0].ends_with("/claude-sonnet"), "{ids:?}");
    }

    // --- Ctrl+A / Ctrl+X on the whole list (no filter) -------------------------

    #[test]
    fn ctrl_x_with_no_filter_clears_every_model() {
        let (mut sel, _rx, _done) = selector(None);
        sel.handle_key(&key_id("ctrl+x"));
        assert_eq!(sel.enabled_ids(), Some([].as_slice()), "all cleared");
    }

    #[test]
    fn ctrl_a_with_no_filter_returns_to_all_enabled() {
        let (mut sel, _rx, _done) = selector(Some(vec![]));
        sel.handle_key(&key_id("ctrl+a"));
        assert_eq!(sel.enabled_ids(), None, "all enabled collapses to None");
    }

    // --- Ctrl+A / Ctrl+X on the FILTERED set (VAL-OVERLAY-011) ------------------

    #[test]
    fn ctrl_x_with_a_filter_clears_only_the_filtered_set() {
        // Start all-enabled, filter to "claude" (2 anthropic models), Ctrl+X clears
        // only those — gpt-4o / gemini stay enabled.
        let (mut sel, _rx, _done) = selector(None);
        for c in "claude".chars() {
            sel.handle_key(&char_key(c));
        }
        assert_eq!(
            sel.visible_count(),
            2,
            "filter narrowed to the 2 claude models"
        );
        sel.handle_key(&key_id("ctrl+x"));
        let ids = sel.enabled_ids().unwrap();
        // The two claude ids are gone; the other two remain.
        assert!(!ids.iter().any(|i| i.contains("claude")), "{ids:?}");
        assert!(ids.iter().any(|i| i.ends_with("/gpt-4o")), "{ids:?}");
        assert!(ids.iter().any(|i| i.ends_with("/gemini-2-pro")), "{ids:?}");
    }

    #[test]
    fn ctrl_a_with_a_filter_enables_only_the_filtered_set() {
        // Start with nothing enabled, filter to "gpt", Ctrl+A enables only gpt-4o.
        let (mut sel, _rx, _done) = selector(Some(vec![]));
        for c in "gpt".chars() {
            sel.handle_key(&char_key(c));
        }
        sel.handle_key(&key_id("ctrl+a"));
        let ids = sel.enabled_ids().unwrap();
        assert_eq!(ids.len(), 1, "only the filtered model enabled: {ids:?}");
        assert!(ids[0].ends_with("/gpt-4o"), "{ids:?}");
    }

    // --- Ctrl+T provider batch -------------------------------------------------

    #[test]
    fn ctrl_t_toggles_the_whole_provider_off_then_on() {
        let (mut sel, _rx, _done) = selector(None);
        // Cursor on the first anthropic model; Ctrl+T disables ALL anthropic ids.
        sel.handle_key(&key_id("ctrl+t"));
        let ids = sel.enabled_ids().unwrap();
        assert!(
            !ids.iter().any(|i| i.starts_with("anthropic/")),
            "anthropic removed in bulk: {ids:?}"
        );
        assert!(ids.iter().any(|i| i.starts_with("openai/")), "{ids:?}");
    }

    // --- Ctrl+↑/↓ reorder ------------------------------------------------------

    #[test]
    fn ctrl_down_reorders_an_enabled_model_within_the_subset() {
        let a = "anthropic/claude-sonnet".to_string();
        let b = "openai/gpt-4o".to_string();
        let (mut sel, _rx, _done) = selector(Some(vec![a.clone(), b.clone()]));
        // Cursor on the first enabled item (a). Ctrl+↓ swaps it below b.
        sel.handle_key(&key_id("ctrl+down"));
        assert_eq!(sel.enabled_ids(), Some([b, a].as_slice()), "reordered");
    }

    // --- Ctrl+S session-only save (VAL-OVERLAY-031) ----------------------------

    #[test]
    fn ctrl_s_emits_saved_and_closes() {
        let (mut sel, mut rx, done) = selector(None);
        sel.handle_key(&key_id("enter")); // seed a subset
        sel.handle_key(&key_id("ctrl+s"));
        assert!(done.load(Ordering::SeqCst), "ctrl+s closes the dialog");
        assert!(
            drain(&mut rx)
                .iter()
                .any(|o| matches!(o, ScopedModelsOutcome::Saved(_))),
            "ctrl+s emits Saved"
        );
    }

    #[test]
    fn overlay_wording_is_honest_about_session_only_persistence() {
        // Parity nail VAL-OVERLAY-031: the overlay must not claim it saves to
        // settings while persistence is not wired.
        let (sel, _rx, _done) = selector(None);
        let body = body_text(&sel);
        assert!(body.contains(SESSION_ONLY_NOTICE), "honest notice: {body}");
        assert!(
            !body.contains("saved to settings"),
            "no false claim: {body}"
        );
    }

    // --- Ctrl+C two-stage (VAL-OVERLAY-033) ------------------------------------

    #[test]
    fn ctrl_c_two_stage_clears_search_first_then_cancels() {
        let (mut sel, mut rx, done) = selector(None);
        for c in "claude".chars() {
            sel.handle_key(&char_key(c));
        }
        assert_eq!(sel.query(), "claude");
        // First Ctrl+C: clears the search, does NOT cancel or close.
        sel.handle_key(&key_id("ctrl+c"));
        assert_eq!(sel.query(), "", "first ctrl+c clears the search");
        assert!(!done.load(Ordering::SeqCst), "first ctrl+c does not close");
        assert!(
            !drain(&mut rx)
                .iter()
                .any(|o| matches!(o, ScopedModelsOutcome::Cancelled)),
            "first ctrl+c must not cancel"
        );
        // Second Ctrl+C (empty query): cancels + closes.
        sel.handle_key(&key_id("ctrl+c"));
        assert!(done.load(Ordering::SeqCst), "second ctrl+c closes");
        assert!(
            drain(&mut rx)
                .iter()
                .any(|o| matches!(o, ScopedModelsOutcome::Cancelled)),
            "second ctrl+c cancels"
        );
    }

    // --- filter narrows then clearing restores ---------------------------------

    #[test]
    fn typing_narrows_then_backspace_restores() {
        let (mut sel, _rx, _done) = selector(None);
        for c in "gemini".chars() {
            sel.handle_key(&char_key(c));
        }
        assert_eq!(sel.visible_count(), 1);
        for _ in 0.."gemini".len() {
            sel.handle_key(&key_id("backspace"));
        }
        assert_eq!(sel.visible_count(), 4, "cleared restores the full list");
    }

    // --- navigation wrap (VAL-OVERLAY-002) -------------------------------------

    #[test]
    fn navigation_wraps() {
        let (mut sel, _rx, _done) = selector(None);
        assert_eq!(sel.selected_index(), 0);
        sel.handle_key(&key_id("up"));
        assert_eq!(sel.selected_index(), 3, "up on the first row wraps to last");
        sel.handle_key(&key_id("down"));
        assert_eq!(
            sel.selected_index(),
            0,
            "down on the last row wraps to first"
        );
    }

    #[test]
    fn escape_always_cancels() {
        let (mut sel, mut rx, done) = selector(None);
        sel.handle_key(&key_id("escape"));
        assert!(done.load(Ordering::SeqCst));
        assert!(
            drain(&mut rx)
                .iter()
                .any(|o| matches!(o, ScopedModelsOutcome::Cancelled))
        );
    }
}
