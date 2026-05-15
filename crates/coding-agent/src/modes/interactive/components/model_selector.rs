//! Searchable picker for the active model.
//!
//! The renderer is intentionally pure: it takes the model list (and a
//! "scoped" subset) as constructor inputs and emits the chosen
//! [`model::Model`] on a channel. The driver remains responsible for
//! pulling the list from the registry, applying the user's scoping rules,
//! and persisting the picked model.
//!
//! The selector supports:
//! * fuzzy filtering against `provider/id` and `id provider`,
//! * Tab to toggle between the "scoped" and "all" lists when both are
//!   non-empty,
//! * up/down with wrap-around,
//! * Enter to confirm, Esc to cancel.
//!
//! Theming caveat: until the coding-agent theme port lands the renderer
//! hardcodes ANSI defaults that match the dark palette (`accent`, `muted`,
//! `success`, `warning`, `error`).
//!
//! TODO: theme integration deferred — see
//! docs/exec-plans/parity-completion.md §A1.

use std::cmp::Reverse;

use hand_tui::{
    Component, Container, FuzzyMatch, HandleResult, InputComponent, InputEvent, SpacerComponent,
    TextComponent, fuzzy_filter,
};
use hand_tui::{Key, KeyName, parse_key};
use model::Model;
use model::types::Provider;
use tokio::sync::mpsc;

use super::dynamic_border::DynamicBorderComponent;

const ACCENT: &str = "\x1b[36m";
const MUTED: &str = "\x1b[90m";
const SUCCESS: &str = "\x1b[32m";
const WARNING: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

const MAX_VISIBLE: usize = 10;

/// Which list is currently surfaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelScope {
    All,
    Scoped,
}

/// Outcome dispatched on the channel handed to
/// [`ModelSelectorComponent::new`].
///
/// `Model` is large (~260 bytes) so the success variant is boxed to keep
/// the enum compact (clippy::large_enum_variant).
#[derive(Debug, Clone)]
pub enum ModelOutcome {
    Selected(Box<Model>),
    Cancelled,
}

/// Renders the searchable model picker.
pub struct ModelSelectorComponent {
    all_models: Vec<Model>,
    scoped_models: Vec<Model>,
    current_provider_id: Option<(String, String)>,
    scope: ModelScope,
    filtered_indices: Vec<usize>,
    selected_index: usize,
    search_input: InputComponent,
    tx: mpsc::UnboundedSender<ModelOutcome>,
}

fn models_equal(a: &Model, b: &Model) -> bool {
    a.id == b.id && a.provider == b.provider
}

fn provider_label(p: Provider) -> &'static str {
    p.as_str()
}

impl ModelSelectorComponent {
    /// Build a new selector.
    ///
    /// * `current_model` — pre-selects the matching entry when present.
    /// * `all_models` — the fully-loaded provider list.
    /// * `scoped_models` — user's "scoped" subset; empty means scope toggle is
    ///   disabled and the renderer behaves like the TS code's `scopedModels.length === 0` branch.
    pub fn new(
        current_model: Option<Model>,
        all_models: Vec<Model>,
        scoped_models: Vec<Model>,
        tx: mpsc::UnboundedSender<ModelOutcome>,
    ) -> Self {
        let scope = if scoped_models.is_empty() {
            ModelScope::All
        } else {
            ModelScope::Scoped
        };
        let mut all_models = all_models;
        if let Some(cur) = current_model.as_ref() {
            // Sort: current model first, then by provider name.
            all_models.sort_by(|a, b| {
                let a_cur = models_equal(a, cur);
                let b_cur = models_equal(b, cur);
                match (a_cur, b_cur) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => provider_label(a.provider).cmp(provider_label(b.provider)),
                }
            });
        }
        let current_provider_id = current_model
            .as_ref()
            .map(|m| (provider_label(m.provider).to_string(), m.id.clone()));

        let mut me = Self {
            all_models,
            scoped_models,
            current_provider_id,
            scope,
            filtered_indices: Vec::new(),
            selected_index: 0,
            search_input: InputComponent::new(),
            tx,
        };
        me.refilter();
        me.preselect_current();
        me
    }

    fn active_models(&self) -> &[Model] {
        match self.scope {
            ModelScope::All => &self.all_models,
            ModelScope::Scoped => &self.scoped_models,
        }
    }

    fn refilter(&mut self) {
        let query = self.search_input.text().to_string();
        let active = self.active_models().to_vec();
        if query.is_empty() {
            self.filtered_indices = (0..active.len()).collect();
        } else {
            let haystacks: Vec<String> = active
                .iter()
                .map(|m| {
                    let provider = provider_label(m.provider);
                    format!(
                        "{id} {provider} {provider}/{id}",
                        id = m.id,
                        provider = provider
                    )
                })
                .collect();
            let refs: Vec<&str> = haystacks.iter().map(String::as_str).collect();
            let mut matches: Vec<(usize, FuzzyMatch)> = fuzzy_filter(&query, &refs);
            matches.sort_by_key(|m| Reverse(m.1.score));
            self.filtered_indices = matches.into_iter().map(|(idx, _)| idx).collect();
        }
        if self.filtered_indices.is_empty() {
            self.selected_index = 0;
        } else if self.selected_index >= self.filtered_indices.len() {
            self.selected_index = self.filtered_indices.len() - 1;
        }
    }

    fn preselect_current(&mut self) {
        let Some((p, id)) = self.current_provider_id.clone() else {
            return;
        };
        let active = self.active_models();
        if let Some(view_idx) = self.filtered_indices.iter().position(|&i| {
            let m = &active[i];
            provider_label(m.provider) == p && m.id == id
        }) {
            self.selected_index = view_idx;
        }
    }

    fn confirm(&self) {
        let Some(&model_idx) = self.filtered_indices.get(self.selected_index) else {
            return;
        };
        let model = self.active_models()[model_idx].clone();
        let _ = self.tx.send(ModelOutcome::Selected(Box::new(model)));
    }

    fn cancel(&self) {
        let _ = self.tx.send(ModelOutcome::Cancelled);
    }

    fn toggle_scope(&mut self) {
        if self.scoped_models.is_empty() {
            return;
        }
        self.scope = match self.scope {
            ModelScope::All => ModelScope::Scoped,
            ModelScope::Scoped => ModelScope::All,
        };
        // Reset cursor and re-filter against the new active list.
        self.selected_index = 0;
        self.refilter();
        self.preselect_current();
    }

    fn render_body(&self, width: u16) -> Vec<String> {
        let mut container = Container::new();
        container.add_child(Box::new(DynamicBorderComponent::new()));
        container.add_child(Box::new(SpacerComponent::new(1)));

        if self.scoped_models.is_empty() {
            let hint =
                "Only showing models from configured providers. Use /login to add providers.";
            container.add_child(Box::new(TextComponent::new(format!(
                "{WARNING}{hint}{RESET}"
            ))));
        } else {
            container.add_child(Box::new(TextComponent::new(self.scope_text())));
            container.add_child(Box::new(TextComponent::new(self.scope_hint_text())));
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
        container.add_child(Box::new(DynamicBorderComponent::new()));
        container.render(width)
    }

    fn scope_text(&self) -> String {
        let (all_color, scoped_color) = match self.scope {
            ModelScope::All => (ACCENT, MUTED),
            ModelScope::Scoped => (MUTED, ACCENT),
        };
        format!(
            "{MUTED}Scope: {RESET}{all_color}all{RESET}{MUTED} | {RESET}{scoped_color}scoped{RESET}"
        )
    }

    fn scope_hint_text(&self) -> String {
        format!("{MUTED}Tab to toggle scope (all/scoped){RESET}")
    }

    fn list_lines(&self) -> Vec<String> {
        let active = self.active_models();
        let count = self.filtered_indices.len();
        let mut lines = Vec::new();

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

        for i in start..end {
            let model = &active[self.filtered_indices[i]];
            let is_selected = i == self.selected_index;
            let is_current = self
                .current_provider_id
                .as_ref()
                .map(|(p, id)| provider_label(model.provider) == p && &model.id == id)
                .unwrap_or(false);

            let provider_badge = format!("{MUTED}[{}]{RESET}", provider_label(model.provider));
            let checkmark = if is_current {
                format!("{SUCCESS} ✓{RESET}")
            } else {
                String::new()
            };

            let line = if is_selected {
                format!(
                    "{ACCENT}→ {RESET}{ACCENT}{}{RESET} {} {}",
                    model.id, provider_badge, checkmark
                )
            } else {
                format!("  {} {} {}", model.id, provider_badge, checkmark)
            };
            lines.push(line);
        }

        if start > 0 || end < count {
            lines.push(format!(
                "{MUTED}  ({}/{}){RESET}",
                self.selected_index + 1,
                count
            ));
        }

        // Append the "Model Name" footer (TS shows the full friendly name).
        if let Some(&model_idx) = self.filtered_indices.get(self.selected_index) {
            let m = &active[model_idx];
            lines.push(String::new());
            lines.push(format!("{MUTED}  Model Name: {}{RESET}", m.name));
        }

        lines
    }

    fn dispatch_key(&mut self, key: &Key) -> HandleResult {
        if key.is_release {
            return HandleResult::Ignored;
        }
        match &key.name {
            KeyName::Tab => {
                self.toggle_scope();
                HandleResult::Handled
            }
            KeyName::Up => {
                let len = self.filtered_indices.len();
                if len > 0 {
                    self.selected_index = if self.selected_index == 0 {
                        len - 1
                    } else {
                        self.selected_index - 1
                    };
                }
                HandleResult::Handled
            }
            KeyName::Down => {
                let len = self.filtered_indices.len();
                if len > 0 {
                    self.selected_index = (self.selected_index + 1) % len;
                }
                HandleResult::Handled
            }
            KeyName::Enter => {
                self.confirm();
                HandleResult::Handled
            }
            KeyName::Escape => {
                self.cancel();
                HandleResult::Handled
            }
            _ => {
                let prev = self.search_input.text().to_string();
                let _ = self
                    .search_input
                    .handle_input(&InputEvent::Key(key.clone()));
                if self.search_input.text() != prev {
                    self.refilter();
                }
                HandleResult::Handled
            }
        }
    }
}

impl Component for ModelSelectorComponent {
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

    fn drain(rx: &mut mpsc::UnboundedReceiver<ModelOutcome>) -> Vec<ModelOutcome> {
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

    #[test]
    fn renders_models_and_provider_badges() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let selector = ModelSelectorComponent::new(None, three_models(), vec![], tx);
        let body = selector.render(80).join("\n");
        assert!(body.contains("claude-sonnet"));
        assert!(body.contains("gpt-4o"));
        assert!(body.contains("gemini-2-pro"));
        assert!(body.contains("[anthropic]"));
        assert!(body.contains("Model Name:"));
    }

    #[test]
    fn current_model_gets_checkmark_and_is_preselected() {
        let models = three_models();
        let current = models[1].clone();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ModelSelectorComponent::new(Some(current.clone()), models, vec![], tx);

        let body = selector.render(80).join("\n");
        assert!(body.contains("✓"));

        // Enter immediately should pick the current model (sorted to the top).
        selector.handle_input(&InputEvent::Raw("\r".into()));
        match drain(&mut rx).into_iter().next() {
            Some(ModelOutcome::Selected(m)) => {
                assert_eq!(m.id, current.id);
                assert_eq!(m.provider, current.provider);
            }
            other => panic!("expected Selected, got {other:?}"),
        }
    }

    #[test]
    fn fuzzy_filter_typing_narrows_list() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut selector = ModelSelectorComponent::new(None, three_models(), vec![], tx);
        // Typing "claude" should keep only the Anthropic claude-sonnet entry.
        for ch in "claude".chars() {
            selector.handle_input(&InputEvent::Raw(ch.to_string()));
        }
        let body = selector.render(80).join("\n");
        assert!(body.contains("claude-sonnet"));
        assert!(!body.contains("gpt-4o"));
        assert!(!body.contains("gemini-2-pro"));
    }

    #[test]
    fn down_wraps_around_at_bottom() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut selector = ModelSelectorComponent::new(None, three_models(), vec![], tx);
        for _ in 0..3 {
            selector.handle_input(&InputEvent::Raw("\x1b[B".into()));
        }
        // Selected index should wrap to 0.
        assert_eq!(selector.selected_index, 0);
    }

    #[test]
    fn escape_cancels() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = ModelSelectorComponent::new(None, three_models(), vec![], tx);
        selector.handle_input(&InputEvent::Raw("\x1b".into()));
        match drain(&mut rx).into_iter().next() {
            Some(ModelOutcome::Cancelled) => {}
            other => panic!("expected Cancelled, got {other:?}"),
        }
    }

    #[test]
    fn tab_toggles_scope_when_scoped_non_empty() {
        let models = three_models();
        let scoped = vec![models[0].clone()];
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut selector = ModelSelectorComponent::new(None, models, scoped, tx);
        // Initial scope is Scoped.
        assert_eq!(selector.scope, ModelScope::Scoped);
        selector.handle_input(&InputEvent::Raw("\t".into()));
        assert_eq!(selector.scope, ModelScope::All);
        selector.handle_input(&InputEvent::Raw("\t".into()));
        assert_eq!(selector.scope, ModelScope::Scoped);
    }

    #[test]
    fn empty_scoped_list_renders_login_hint() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let selector = ModelSelectorComponent::new(None, three_models(), vec![], tx);
        let body = selector.render(80).join("\n");
        assert!(body.contains("Use /login to add providers"));
    }

    #[test]
    fn no_matches_renders_hint() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut selector = ModelSelectorComponent::new(None, three_models(), vec![], tx);
        for ch in "zzzzz".chars() {
            selector.handle_input(&InputEvent::Raw(ch.to_string()));
        }
        let body = selector.render(80).join("\n");
        assert!(body.contains("No matching models"));
    }
}
