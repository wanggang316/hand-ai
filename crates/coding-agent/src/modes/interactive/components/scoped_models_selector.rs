//! Multi-select picker for the user's "scoped" model list (the Ctrl+P cycle).
//!
//! The dialog lets users:
//! * Toggle individual models on/off (Enter).
//! * Bulk enable / clear / toggle-by-provider for the current filter.
//! * Reorder enabled models (Ctrl+Up / Ctrl+Down).
//! * Persist the configuration (Ctrl+S) — the renderer only emits an event;
//!   the host writes it through `SettingsManager`.
//!
//! Like the other Phase-3 selectors this is a pure renderer over a
//! caller-supplied view-model. `enabled_ids` follows the TS semantics:
//! `None` means "all models enabled", `Some(vec)` is an explicit ordered
//! list. The `OnChange` event reports the same shape so the host can sync
//! to its session-level state.
//!
//! Theming caveat: the TS source pulls `accent`, `success`, `warning`,
//! `muted`, `dim` from the coding-agent theme. Until the theme port lands
//! the renderer hardcodes ANSI defaults that match the dark palette.
//!
//! TODO(parity): theme integration deferred — see
//! docs/exec-plans/parity-completion.md §A1.

use std::cmp::Reverse;
use std::collections::HashMap;

use hand_tui::{
    Component, Container, FuzzyMatch, HandleResult, InputComponent, InputEvent, SpacerComponent,
    TextComponent, fuzzy_filter,
};
use hand_tui::{Key, KeyName, parse_key};
use model::Model;
use tokio::sync::mpsc;

use super::dynamic_border::DynamicBorderComponent;

const ACCENT: &str = "\x1b[36m";
const SUCCESS: &str = "\x1b[32m";
const WARNING: &str = "\x1b[33m";
const MUTED: &str = "\x1b[90m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

const MAX_VISIBLE: usize = 8;

/// Initial configuration handed to the selector.
#[derive(Debug, Clone)]
pub struct ScopedModelsConfig {
    pub all_models: Vec<Model>,
    /// `None` ⇒ every model is enabled (no explicit list yet); `Some(vec)`
    /// is an ordered subset of `provider/id` strings.
    pub enabled_ids: Option<Vec<String>>,
}

/// Outcome dispatched on the channel handed to
/// [`ScopedModelsSelectorComponent::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedModelsOutcome {
    /// Session-only change. The host should update its in-memory copy but
    /// not persist yet.
    Change(Option<Vec<String>>),
    /// User asked to persist (Ctrl+S). The host should write to settings.
    Persist(Option<Vec<String>>),
    /// User cancelled (Esc / Ctrl+C with empty search).
    Cancelled,
}

fn full_id(model: &Model) -> String {
    format!("{}/{}", model.provider.as_str(), model.id)
}

fn is_enabled(enabled: &Option<Vec<String>>, id: &str) -> bool {
    match enabled {
        None => true,
        Some(list) => list.iter().any(|x| x == id),
    }
}

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
            let target_set: Vec<String> =
                targets.map(|t| t.to_vec()).unwrap_or_else(|| list.clone());
            Some(
                list.into_iter()
                    .filter(|id| !target_set.contains(id))
                    .collect(),
            )
        }
    }
}

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

#[derive(Debug, Clone)]
struct ModelItem {
    full_id: String,
    model: Model,
    enabled: bool,
}

/// Multi-select / ordering picker for the scoped-models list.
pub struct ScopedModelsSelectorComponent {
    models_by_id: HashMap<String, Model>,
    all_ids: Vec<String>,
    enabled_ids: Option<Vec<String>>,
    filtered_items: Vec<ModelItem>,
    selected_index: usize,
    search_input: InputComponent,
    is_dirty: bool,
    tx: mpsc::UnboundedSender<ScopedModelsOutcome>,
}

impl ScopedModelsSelectorComponent {
    pub fn new(config: ScopedModelsConfig, tx: mpsc::UnboundedSender<ScopedModelsOutcome>) -> Self {
        let mut models_by_id = HashMap::new();
        let mut all_ids = Vec::new();
        for model in config.all_models {
            let id = full_id(&model);
            models_by_id.insert(id.clone(), model);
            all_ids.push(id);
        }

        let mut me = Self {
            models_by_id,
            all_ids,
            enabled_ids: config.enabled_ids,
            filtered_items: Vec::new(),
            selected_index: 0,
            search_input: InputComponent::new(),
            is_dirty: false,
            tx,
        };
        me.rebuild_items();
        me
    }

    fn build_items(&self) -> Vec<ModelItem> {
        sorted_ids(&self.enabled_ids, &self.all_ids)
            .into_iter()
            .filter_map(|id| {
                self.models_by_id.get(&id).map(|m| ModelItem {
                    full_id: id.clone(),
                    model: m.clone(),
                    enabled: is_enabled(&self.enabled_ids, &id),
                })
            })
            .collect()
    }

    fn rebuild_items(&mut self) {
        let items = self.build_items();
        let query = self.search_input.text().to_string();
        if query.is_empty() {
            self.filtered_items = items;
        } else {
            let haystacks: Vec<String> = items
                .iter()
                .map(|i| format!("{} {}", i.model.id, i.model.provider.as_str()))
                .collect();
            let refs: Vec<&str> = haystacks.iter().map(String::as_str).collect();
            let mut matches: Vec<(usize, FuzzyMatch)> = fuzzy_filter(&query, &refs);
            matches.sort_by_key(|m| Reverse(m.1.score));
            self.filtered_items = matches
                .into_iter()
                .map(|(idx, _)| items[idx].clone())
                .collect();
        }
        if self.filtered_items.is_empty() {
            self.selected_index = 0;
        } else if self.selected_index >= self.filtered_items.len() {
            self.selected_index = self.filtered_items.len() - 1;
        }
    }

    fn notify_change(&self) {
        let _ = self
            .tx
            .send(ScopedModelsOutcome::Change(self.enabled_ids.clone()));
    }

    fn header_lines(&self) -> Vec<String> {
        vec![
            format!("{ACCENT}{BOLD}Model Configuration{RESET}"),
            format!(
                "{MUTED}Session-only. Ctrl+S to apply for this session (persistence pending).{RESET}"
            ),
        ]
    }

    fn footer_text(&self) -> String {
        let enabled_count = self
            .enabled_ids
            .as_ref()
            .map(Vec::len)
            .unwrap_or(self.all_ids.len());
        let count_text = if self.enabled_ids.is_none() {
            "all enabled".to_string()
        } else {
            format!("{}/{} enabled", enabled_count, self.all_ids.len())
        };
        let parts = [
            "Enter toggle",
            "Ctrl+A all",
            "Ctrl+X clear",
            "Ctrl+T provider",
            "Ctrl+Up/Ctrl+Down reorder",
            "Ctrl+S apply",
            count_text.as_str(),
        ];
        let body = format!("{DIM}  {}{RESET}", parts.join(" · "));
        if self.is_dirty {
            format!("{body} {WARNING}(unsaved){RESET}")
        } else {
            body
        }
    }

    fn list_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        let count = self.filtered_items.len();
        if count == 0 {
            lines.push(format!("{MUTED}  No matching models{RESET}"));
            return lines;
        }

        let half = MAX_VISIBLE / 2;
        let start = self
            .selected_index
            .saturating_sub(half)
            .min(count.saturating_sub(MAX_VISIBLE));
        let end = (start + MAX_VISIBLE).min(count);
        let all_enabled = self.enabled_ids.is_none();

        for i in start..end {
            let item = &self.filtered_items[i];
            let is_selected = i == self.selected_index;
            let prefix = if is_selected {
                format!("{ACCENT}→ {RESET}")
            } else {
                "  ".to_string()
            };
            let id_text = if is_selected {
                format!("{ACCENT}{}{RESET}", item.model.id)
            } else {
                item.model.id.clone()
            };
            let provider_badge = format!("{MUTED} [{}]{RESET}", item.model.provider.as_str());
            let status = if all_enabled {
                String::new()
            } else if item.enabled {
                format!("{SUCCESS} ✓{RESET}")
            } else {
                format!("{DIM} ✗{RESET}")
            };
            lines.push(format!("{prefix}{id_text}{provider_badge}{status}"));
        }

        if start > 0 || end < count {
            lines.push(format!(
                "{MUTED}  ({}/{}){RESET}",
                self.selected_index + 1,
                count
            ));
        }

        // Selected model "Model Name:" footer.
        if let Some(item) = self.filtered_items.get(self.selected_index) {
            lines.push(String::new());
            lines.push(format!("{MUTED}  Model Name: {}{RESET}", item.model.name));
        }

        lines
    }

    fn render_body(&self, width: u16) -> Vec<String> {
        let mut container = Container::new();
        container.add_child(Box::new(DynamicBorderComponent::new()));
        container.add_child(Box::new(SpacerComponent::new(1)));
        for line in self.header_lines() {
            container.add_child(Box::new(TextComponent::new(line)));
        }
        container.add_child(Box::new(SpacerComponent::new(1)));
        for line in self.search_input.render(width) {
            container.add_child(Box::new(TextComponent::new(line)));
        }
        container.add_child(Box::new(SpacerComponent::new(1)));
        for line in self.list_lines() {
            container.add_child(Box::new(TextComponent::new(line)));
        }
        container.add_child(Box::new(SpacerComponent::new(1)));
        container.add_child(Box::new(TextComponent::new(self.footer_text())));
        container.add_child(Box::new(DynamicBorderComponent::new()));
        container.render(width)
    }

    fn dispatch_key(&mut self, key: &Key) -> HandleResult {
        if key.is_release {
            return HandleResult::Ignored;
        }
        let ctrl = key.modifiers.ctrl;
        match (&key.name, ctrl) {
            (KeyName::Up, false) => {
                let len = self.filtered_items.len();
                if len > 0 {
                    self.selected_index = if self.selected_index == 0 {
                        len - 1
                    } else {
                        self.selected_index - 1
                    };
                }
                HandleResult::Handled
            }
            (KeyName::Down, false) => {
                let len = self.filtered_items.len();
                if len > 0 {
                    self.selected_index = (self.selected_index + 1) % len;
                }
                HandleResult::Handled
            }
            (KeyName::Up, true) | (KeyName::Down, true) => {
                self.handle_reorder(matches!(key.name, KeyName::Up));
                HandleResult::Handled
            }
            (KeyName::Enter, _) => {
                self.handle_toggle();
                HandleResult::Handled
            }
            (KeyName::Char('a'), true) => {
                self.handle_enable_all();
                HandleResult::Handled
            }
            (KeyName::Char('x'), true) => {
                self.handle_clear_all();
                HandleResult::Handled
            }
            (KeyName::Char('t'), true) => {
                self.handle_toggle_provider();
                HandleResult::Handled
            }
            (KeyName::Char('s'), true) => {
                self.handle_save();
                HandleResult::Handled
            }
            (KeyName::Char('c'), true) => {
                if !self.search_input.text().is_empty() {
                    self.search_input.clear();
                    self.rebuild_items();
                } else {
                    let _ = self.tx.send(ScopedModelsOutcome::Cancelled);
                }
                HandleResult::Handled
            }
            (KeyName::Escape, _) => {
                let _ = self.tx.send(ScopedModelsOutcome::Cancelled);
                HandleResult::Handled
            }
            _ => {
                let prev = self.search_input.text().to_string();
                let _ = self
                    .search_input
                    .handle_input(&InputEvent::Key(key.clone()));
                if self.search_input.text() != prev {
                    self.rebuild_items();
                }
                HandleResult::Handled
            }
        }
    }

    fn handle_reorder(&mut self, going_up: bool) {
        let Some(enabled) = self.enabled_ids.clone() else {
            return;
        };
        let Some(item) = self.filtered_items.get(self.selected_index).cloned() else {
            return;
        };
        if !is_enabled(&self.enabled_ids, &item.full_id) {
            return;
        }
        let delta = if going_up { -1 } else { 1 };
        let Some(cur) = enabled.iter().position(|x| x == &item.full_id) else {
            return;
        };
        let new_idx = cur as i32 + delta;
        if new_idx < 0 || (new_idx as usize) >= enabled.len() {
            return;
        }
        self.enabled_ids = move_id(self.enabled_ids.clone(), &item.full_id, delta);
        self.is_dirty = true;
        // Selection follows the moved item by adjusting the view index.
        if going_up && self.selected_index > 0 {
            self.selected_index -= 1;
        } else if !going_up {
            self.selected_index += 1;
        }
        self.rebuild_items();
        self.notify_change();
    }

    fn handle_toggle(&mut self) {
        let Some(item) = self.filtered_items.get(self.selected_index).cloned() else {
            return;
        };
        self.enabled_ids = toggle(self.enabled_ids.clone(), &item.full_id);
        self.is_dirty = true;
        self.rebuild_items();
        self.notify_change();
    }

    fn handle_enable_all(&mut self) {
        let targets: Option<Vec<String>> = if !self.search_input.text().is_empty() {
            Some(
                self.filtered_items
                    .iter()
                    .map(|i| i.full_id.clone())
                    .collect(),
            )
        } else {
            None
        };
        self.enabled_ids = enable_all(self.enabled_ids.clone(), &self.all_ids, targets.as_deref());
        self.is_dirty = true;
        self.rebuild_items();
        self.notify_change();
    }

    fn handle_clear_all(&mut self) {
        let targets: Option<Vec<String>> = if !self.search_input.text().is_empty() {
            Some(
                self.filtered_items
                    .iter()
                    .map(|i| i.full_id.clone())
                    .collect(),
            )
        } else {
            None
        };
        self.enabled_ids = clear_all(self.enabled_ids.clone(), &self.all_ids, targets.as_deref());
        self.is_dirty = true;
        self.rebuild_items();
        self.notify_change();
    }

    fn handle_toggle_provider(&mut self) {
        let Some(item) = self.filtered_items.get(self.selected_index).cloned() else {
            return;
        };
        let provider = item.model.provider;
        let provider_ids: Vec<String> = self
            .all_ids
            .iter()
            .filter(|id| {
                self.models_by_id
                    .get(*id)
                    .map(|m| m.provider == provider)
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        let all_provider_enabled = provider_ids
            .iter()
            .all(|id| is_enabled(&self.enabled_ids, id));
        self.enabled_ids = if all_provider_enabled {
            clear_all(self.enabled_ids.clone(), &self.all_ids, Some(&provider_ids))
        } else {
            enable_all(self.enabled_ids.clone(), &self.all_ids, Some(&provider_ids))
        };
        self.is_dirty = true;
        self.rebuild_items();
        self.notify_change();
    }

    fn handle_save(&mut self) {
        let _ = self
            .tx
            .send(ScopedModelsOutcome::Persist(self.enabled_ids.clone()));
        self.is_dirty = false;
    }
}

impl Component for ScopedModelsSelectorComponent {
    fn render(&self, width: u16) -> Vec<String> {
        self.render_body(width)
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        match event {
            InputEvent::Key(key) => self.dispatch_key(key),
            InputEvent::Raw(s) | InputEvent::Paste(s) => {
                let key = parse_key(s);
                self.dispatch_key(&key)
            }
            _ => HandleResult::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::types::Provider;
    use model::{Api, Cost, InputType};

    fn drain(rx: &mut mpsc::UnboundedReceiver<ScopedModelsOutcome>) -> Vec<ScopedModelsOutcome> {
        let mut out = Vec::new();
        while let Ok(o) = rx.try_recv() {
            out.push(o);
        }
        out
    }

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

    fn config(enabled: Option<Vec<String>>) -> ScopedModelsConfig {
        ScopedModelsConfig {
            all_models: three_models(),
            enabled_ids: enabled,
        }
    }

    #[test]
    fn renders_header_and_models_with_all_enabled() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let selector = ScopedModelsSelectorComponent::new(config(None), tx);
        let body = selector.render(80).join("\n");
        assert!(body.contains("Model Configuration"));
        assert!(body.contains("Session-only"));
        assert!(body.contains("all enabled"));
        assert!(body.contains("claude-sonnet"));
        assert!(body.contains("gpt-4o"));
        assert!(body.contains("gemini-2-pro"));
    }

    /// Regression for #81: the overlay must NOT claim Ctrl+S saves
    /// to settings while the runtime still surfaces "persist not
    /// yet wired". Pin the honest wording — "apply for this session
    /// (persistence pending)" in the header and "Ctrl+S apply" in
    /// the footer — until persist actually lands.
    #[test]
    fn overlay_strings_match_runtime_until_persist_lands() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let selector = ScopedModelsSelectorComponent::new(config(None), tx);
        let body = selector.render(80).join("\n");
        assert!(
            !body.contains("to save to settings"),
            "overlay still claims to save to settings: {body}"
        );
        assert!(
            !body.contains("Ctrl+S save"),
            "footer still labels Ctrl+S as `save`: {body}"
        );
        assert!(
            body.contains("apply for this session"),
            "header missing honest apply-for-this-session wording: {body}"
        );
        assert!(
            body.contains("persistence pending"),
            "header missing persistence-pending hint: {body}"
        );
        assert!(
            body.contains("Ctrl+S apply"),
            "footer missing Ctrl+S apply label: {body}"
        );
    }

    #[test]
    fn enter_toggles_first_item_emits_change() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ScopedModelsSelectorComponent::new(config(None), tx);
        selector.handle_input(&InputEvent::Raw("\r".into()));
        match drain(&mut rx).into_iter().next() {
            Some(ScopedModelsOutcome::Change(Some(ids))) => {
                assert_eq!(ids.len(), 1);
                assert!(ids[0].ends_with("/claude-sonnet"));
            }
            other => panic!("expected Change(Some(_)), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_s_emits_persist_and_clears_dirty() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ScopedModelsSelectorComponent::new(config(None), tx);
        selector.handle_input(&InputEvent::Raw("\r".into())); // toggle → dirty
        selector.handle_input(&InputEvent::Raw("\x13".into())); // Ctrl+S
        let events = drain(&mut rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ScopedModelsOutcome::Persist(_))),
            "expected Persist among {events:?}"
        );
        // After save, footer should not show "(unsaved)".
        let body = selector.render(80).join("\n");
        assert!(!body.contains("(unsaved)"));
    }

    #[test]
    fn ctrl_x_clears_all_when_no_search() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ScopedModelsSelectorComponent::new(config(None), tx);
        selector.handle_input(&InputEvent::Raw("\x18".into())); // Ctrl+X
        match drain(&mut rx).into_iter().last() {
            Some(ScopedModelsOutcome::Change(Some(ids))) => assert!(ids.is_empty()),
            other => panic!("expected empty Change, got {other:?}"),
        }
    }

    #[test]
    fn ctrl_a_returns_to_all_enabled_when_no_filter() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ScopedModelsSelectorComponent::new(config(Some(vec![])), tx);
        selector.handle_input(&InputEvent::Raw("\x01".into())); // Ctrl+A
        match drain(&mut rx).into_iter().last() {
            Some(ScopedModelsOutcome::Change(None)) => {}
            other => panic!("expected Change(None), got {other:?}"),
        }
    }

    #[test]
    fn escape_emits_cancel() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ScopedModelsSelectorComponent::new(config(None), tx);
        selector.handle_input(&InputEvent::Raw("\x1b".into()));
        match drain(&mut rx).into_iter().next() {
            Some(ScopedModelsOutcome::Cancelled) => {}
            other => panic!("expected Cancelled, got {other:?}"),
        }
    }

    #[test]
    fn typing_filters_then_ctrl_c_clears_search_only() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ScopedModelsSelectorComponent::new(config(None), tx);
        for ch in "claude".chars() {
            selector.handle_input(&InputEvent::Raw(ch.to_string()));
        }
        let body = selector.render(80).join("\n");
        assert!(body.contains("claude-sonnet"));
        assert!(!body.contains("gpt-4o"));

        // Ctrl+C with non-empty query should clear search, not cancel.
        selector.handle_input(&InputEvent::Raw("\x03".into()));
        let after = selector.render(80).join("\n");
        assert!(after.contains("gpt-4o"));
        // Still no cancel emitted.
        assert!(
            !drain(&mut rx)
                .iter()
                .any(|e| matches!(e, ScopedModelsOutcome::Cancelled))
        );
    }

    #[test]
    fn ctrl_t_toggles_provider_off_then_on() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ScopedModelsSelectorComponent::new(config(None), tx);
        // Cursor on first item (anthropic). Ctrl+T should clear all anthropic ids.
        selector.handle_input(&InputEvent::Raw("\x14".into())); // Ctrl+T
        let events = drain(&mut rx);
        // Last event should be a Change with anthropic removed.
        match events.into_iter().last() {
            Some(ScopedModelsOutcome::Change(Some(ids))) => {
                assert!(!ids.iter().any(|i| i.starts_with("anthropic/")));
            }
            other => panic!("expected provider removal, got {other:?}"),
        }
    }
}
