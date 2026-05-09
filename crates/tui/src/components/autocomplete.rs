//! Autocomplete component — suggestion dropdown for input fields.
//!
//! This module provides two layers:
//!
//! 1. The dropdown view: [`AutocompleteComponent`] renders a vertical list of
//!    [`Suggestion`]s. It is purely presentational.
//! 2. The provider abstraction: [`AutocompleteProvider`] is the source of
//!    completion items. The editor (M3.T2) drives a provider, then feeds the
//!    resulting [`AutocompleteItem`]s into the dropdown.
//!
//! Providers are async to support filesystem / API lookups. A
//! [`CombinedAutocompleteProvider`] composes multiple sources (e.g. a static
//! slash-command list plus a file picker) and merges results in provider
//! order. Debouncing happens in the editor, not here.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::tui::Component;

/// A single autocomplete suggestion.
#[derive(Debug, Clone)]
pub struct Suggestion {
    /// The text to insert on selection.
    pub value: String,
    /// Display label (may differ from value).
    pub label: String,
    /// Optional description/annotation.
    pub description: Option<String>,
}

impl Suggestion {
    pub fn new(value: impl Into<String>) -> Self {
        let v: String = value.into();
        Self {
            label: v.clone(),
            value: v,
            description: None,
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

/// Autocomplete dropdown component.
pub struct AutocompleteComponent {
    suggestions: Vec<Suggestion>,
    selected_index: usize,
    visible: bool,
    max_visible: usize,
    scroll_offset: usize,
}

impl AutocompleteComponent {
    pub fn new() -> Self {
        Self {
            suggestions: Vec::new(),
            selected_index: 0,
            visible: false,
            max_visible: 8,
            scroll_offset: 0,
        }
    }

    /// Update the suggestions list.
    pub fn set_suggestions(&mut self, suggestions: Vec<Suggestion>) {
        self.suggestions = suggestions;
        self.selected_index = 0;
        self.scroll_offset = 0;
        self.visible = !self.suggestions.is_empty();
    }

    /// Get the currently selected suggestion.
    pub fn selected(&self) -> Option<&Suggestion> {
        self.suggestions.get(self.selected_index)
    }

    /// Move selection down.
    pub fn select_next(&mut self) {
        if !self.suggestions.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.suggestions.len();
            self.ensure_visible();
        }
    }

    /// Move selection up.
    pub fn select_prev(&mut self) {
        if !self.suggestions.is_empty() {
            if self.selected_index == 0 {
                self.selected_index = self.suggestions.len() - 1;
            } else {
                self.selected_index -= 1;
            }
            self.ensure_visible();
        }
    }

    /// Show the dropdown.
    pub fn show(&mut self) {
        self.visible = !self.suggestions.is_empty();
    }

    /// Hide the dropdown.
    pub fn hide(&mut self) {
        self.visible = false;
    }

    /// Whether the dropdown is visible.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Number of suggestions.
    pub fn count(&self) -> usize {
        self.suggestions.len()
    }

    /// Set max visible items.
    pub fn set_max_visible(&mut self, max: usize) {
        self.max_visible = max;
    }

    fn ensure_visible(&mut self) {
        if self.selected_index < self.scroll_offset {
            self.scroll_offset = self.selected_index;
        }
        if self.selected_index >= self.scroll_offset + self.max_visible {
            self.scroll_offset = self.selected_index + 1 - self.max_visible;
        }
    }
}

impl Default for AutocompleteComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for AutocompleteComponent {
    fn render(&self, width: u16) -> Vec<String> {
        if !self.visible || self.suggestions.is_empty() {
            return vec![];
        }

        let w = width as usize;
        let end = (self.scroll_offset + self.max_visible).min(self.suggestions.len());
        let visible = &self.suggestions[self.scroll_offset..end];

        visible
            .iter()
            .enumerate()
            .map(|(i, suggestion)| {
                let abs_index = self.scroll_offset + i;
                let is_selected = abs_index == self.selected_index;
                let indicator = if is_selected { ">" } else { " " };

                let desc = suggestion
                    .description
                    .as_deref()
                    .map(|d| format!("  \x1b[2m{}\x1b[0m", d))
                    .unwrap_or_default();

                let max_label = w.saturating_sub(4 + crate::utils::visible_width(&desc));
                let label = if suggestion.label.len() > max_label {
                    format!("{}...", &suggestion.label[..max_label.saturating_sub(3)])
                } else {
                    suggestion.label.clone()
                };

                if is_selected {
                    format!(" \x1b[7m{} {}\x1b[0m{}", indicator, label, desc)
                } else {
                    format!(" {} {}{}", indicator, label, desc)
                }
            })
            .collect()
    }
}

/// What triggered the autocomplete request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutocompleteTrigger {
    /// User typed `/` at start of word.
    Slash,
    /// User typed `@` (file/attachment).
    At,
    /// User pressed a manual trigger key.
    Manual,
}

/// Context delivered to providers.
#[derive(Debug, Clone)]
pub struct AutocompleteContext {
    /// Full input text.
    pub text: String,
    /// Cursor position (byte offset).
    pub cursor: usize,
    /// Trigger that initiated this query.
    pub trigger: AutocompleteTrigger,
    /// Active query: the text after the trigger char up to cursor.
    pub query: String,
}

/// Kind of completion item — informs how the editor inserts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutocompleteItemKind {
    SlashCommand,
    File,
    Custom,
}

/// Item returned by a provider.
#[derive(Debug, Clone)]
pub struct AutocompleteItem {
    pub label: String,
    pub detail: Option<String>,
    /// Text inserted on accept (replaces from trigger to cursor).
    pub insert_text: String,
    pub kind: AutocompleteItemKind,
}

/// Boxed future returned by [`AutocompleteProvider::query`].
pub type AutocompleteFuture<'a> = Pin<Box<dyn Future<Output = Vec<AutocompleteItem>> + Send + 'a>>;

/// Provider trait. Async to support filesystem / API lookups.
pub trait AutocompleteProvider: Send + Sync {
    fn query<'a>(&'a self, ctx: &'a AutocompleteContext) -> AutocompleteFuture<'a>;
}

/// Combine multiple providers. Queries run sequentially in provider order and
/// results are concatenated. The TS reference does not deduplicate, so neither
/// do we — callers that need dedup should layer it on top.
///
/// Sequential is intentional: typical providers (in-memory tables, single
/// `readdir` call) finish in microseconds, and avoiding a `futures` dependency
/// keeps the surface small. If a provider becomes I/O-heavy, it should manage
/// its own internal concurrency.
pub struct CombinedAutocompleteProvider {
    providers: Vec<Arc<dyn AutocompleteProvider>>,
}

impl CombinedAutocompleteProvider {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    pub fn with_providers(providers: Vec<Arc<dyn AutocompleteProvider>>) -> Self {
        Self { providers }
    }

    pub fn add_provider(&mut self, provider: Arc<dyn AutocompleteProvider>) {
        self.providers.push(provider);
    }
}

impl Default for CombinedAutocompleteProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AutocompleteProvider for CombinedAutocompleteProvider {
    fn query<'a>(&'a self, ctx: &'a AutocompleteContext) -> AutocompleteFuture<'a> {
        Box::pin(async move {
            let mut all = Vec::new();
            for provider in &self.providers {
                all.extend(provider.query(ctx).await);
            }
            all
        })
    }
}

/// Slash command definition (used by [`SlashCommandProvider`]).
#[derive(Debug, Clone)]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    pub arguments: Option<String>,
}

impl SlashCommand {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            arguments: None,
        }
    }

    pub fn with_arguments(mut self, arguments: impl Into<String>) -> Self {
        self.arguments = Some(arguments.into());
        self
    }
}

/// Provider that yields a static list of slash commands, filtered by prefix
/// match against the command `name`.
pub struct SlashCommandProvider {
    commands: Vec<SlashCommand>,
}

impl SlashCommandProvider {
    pub fn new(commands: Vec<SlashCommand>) -> Self {
        Self { commands }
    }

    pub fn commands(&self) -> &[SlashCommand] {
        &self.commands
    }
}

impl AutocompleteProvider for SlashCommandProvider {
    fn query<'a>(&'a self, ctx: &'a AutocompleteContext) -> AutocompleteFuture<'a> {
        Box::pin(async move {
            let query = ctx.query.as_str();
            self.commands
                .iter()
                .filter(|cmd| cmd.name.starts_with(query))
                .map(|cmd| {
                    let detail = match &cmd.arguments {
                        Some(args) if !cmd.description.is_empty() => {
                            Some(format!("{} — {}", args, cmd.description))
                        }
                        Some(args) => Some(args.clone()),
                        None if !cmd.description.is_empty() => Some(cmd.description.clone()),
                        None => None,
                    };
                    AutocompleteItem {
                        label: cmd.name.clone(),
                        detail,
                        insert_text: cmd.name.clone(),
                        kind: AutocompleteItemKind::SlashCommand,
                    }
                })
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_autocomplete_empty() {
        let ac = AutocompleteComponent::new();
        assert!(!ac.is_visible());
        assert_eq!(ac.render(80).len(), 0);
    }

    #[test]
    fn test_autocomplete_with_suggestions() {
        let mut ac = AutocompleteComponent::new();
        ac.set_suggestions(vec![
            Suggestion::new("/help"),
            Suggestion::new("/model"),
            Suggestion::new("/quit"),
        ]);
        assert!(ac.is_visible());
        assert_eq!(ac.count(), 3);

        let lines = ac.render(80);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn test_autocomplete_navigation() {
        let mut ac = AutocompleteComponent::new();
        ac.set_suggestions(vec![
            Suggestion::new("a"),
            Suggestion::new("b"),
            Suggestion::new("c"),
        ]);

        assert_eq!(ac.selected().unwrap().value, "a");
        ac.select_next();
        assert_eq!(ac.selected().unwrap().value, "b");
        ac.select_next();
        assert_eq!(ac.selected().unwrap().value, "c");
        ac.select_next(); // wraps
        assert_eq!(ac.selected().unwrap().value, "a");
    }

    #[test]
    fn test_autocomplete_prev_wraps() {
        let mut ac = AutocompleteComponent::new();
        ac.set_suggestions(vec![Suggestion::new("a"), Suggestion::new("b")]);

        ac.select_prev(); // wraps to end
        assert_eq!(ac.selected().unwrap().value, "b");
    }

    #[test]
    fn test_autocomplete_hide_show() {
        let mut ac = AutocompleteComponent::new();
        ac.set_suggestions(vec![Suggestion::new("x")]);
        assert!(ac.is_visible());
        ac.hide();
        assert!(!ac.is_visible());
        ac.show();
        assert!(ac.is_visible());
    }

    #[test]
    fn test_autocomplete_max_visible() {
        let mut ac = AutocompleteComponent::new();
        ac.set_max_visible(2);
        ac.set_suggestions(vec![
            Suggestion::new("a"),
            Suggestion::new("b"),
            Suggestion::new("c"),
            Suggestion::new("d"),
        ]);

        let lines = ac.render(80);
        assert_eq!(lines.len(), 2); // Only 2 shown
    }

    #[test]
    fn test_suggestion_builder() {
        let s = Suggestion::new("cmd")
            .with_label("Command")
            .with_description("Do something");
        assert_eq!(s.value, "cmd");
        assert_eq!(s.label, "Command");
        assert_eq!(s.description.as_deref(), Some("Do something"));
    }

    // ---- Provider abstraction tests ----

    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ctx(query: &str, trigger: AutocompleteTrigger) -> AutocompleteContext {
        AutocompleteContext {
            text: query.to_string(),
            cursor: query.len(),
            trigger,
            query: query.to_string(),
        }
    }

    fn cmds(names: &[&str]) -> Vec<SlashCommand> {
        names
            .iter()
            .map(|n| SlashCommand::new(*n, format!("desc {n}")))
            .collect()
    }

    #[tokio::test]
    async fn test_slash_command_provider_filters_by_prefix() {
        let provider = SlashCommandProvider::new(cmds(&["help", "history", "model", "quit"]));
        let items = provider.query(&ctx("h", AutocompleteTrigger::Slash)).await;
        let names: Vec<_> = items.iter().map(|i| i.insert_text.as_str()).collect();
        assert_eq!(names, vec!["help", "history"]);
    }

    #[tokio::test]
    async fn test_slash_command_provider_empty_query_returns_all() {
        let provider = SlashCommandProvider::new(cmds(&["help", "model", "quit"]));
        let items = provider.query(&ctx("", AutocompleteTrigger::Slash)).await;
        assert_eq!(items.len(), 3);
    }

    #[tokio::test]
    async fn test_slash_command_provider_no_match() {
        let provider = SlashCommandProvider::new(cmds(&["help", "model"]));
        let items = provider
            .query(&ctx("zzz", AutocompleteTrigger::Slash))
            .await;
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn test_slash_command_provider_item_kind_and_detail() {
        let provider = SlashCommandProvider::new(vec![
            SlashCommand::new("model", "Switch model").with_arguments("<id>"),
        ]);
        let items = provider.query(&ctx("m", AutocompleteTrigger::Slash)).await;
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.kind, AutocompleteItemKind::SlashCommand);
        assert_eq!(item.label, "model");
        assert_eq!(item.insert_text, "model");
        assert_eq!(item.detail.as_deref(), Some("<id> — Switch model"));
    }

    // A provider that returns a fixed list, used to compose tests.
    struct StaticProvider {
        items: Vec<AutocompleteItem>,
    }

    impl AutocompleteProvider for StaticProvider {
        fn query<'a>(&'a self, _ctx: &'a AutocompleteContext) -> AutocompleteFuture<'a> {
            let items = self.items.clone();
            Box::pin(async move { items })
        }
    }

    fn item(label: &str, kind: AutocompleteItemKind) -> AutocompleteItem {
        AutocompleteItem {
            label: label.to_string(),
            detail: None,
            insert_text: label.to_string(),
            kind,
        }
    }

    #[tokio::test]
    async fn test_combined_provider_merges_in_order() {
        let p0 = Arc::new(StaticProvider {
            items: vec![
                item("a", AutocompleteItemKind::SlashCommand),
                item("b", AutocompleteItemKind::SlashCommand),
            ],
        });
        let p1 = Arc::new(StaticProvider {
            items: vec![
                item("c", AutocompleteItemKind::File),
                item("d", AutocompleteItemKind::File),
            ],
        });
        let combined = CombinedAutocompleteProvider::with_providers(vec![p0, p1]);
        let items = combined.query(&ctx("", AutocompleteTrigger::Manual)).await;
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["a", "b", "c", "d"]);
    }

    #[tokio::test]
    async fn test_combined_provider_empty() {
        let combined = CombinedAutocompleteProvider::new();
        let items = combined.query(&ctx("", AutocompleteTrigger::Manual)).await;
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn test_combined_provider_add_provider() {
        let mut combined = CombinedAutocompleteProvider::new();
        combined.add_provider(Arc::new(StaticProvider {
            items: vec![item("x", AutocompleteItemKind::Custom)],
        }));
        let items = combined.query(&ctx("", AutocompleteTrigger::Manual)).await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "x");
    }

    // Provider that records when it has been polled — used to verify both
    // providers run in a combined query.
    struct CountingProvider {
        counter: Arc<AtomicUsize>,
        label: &'static str,
    }

    impl AutocompleteProvider for CountingProvider {
        fn query<'a>(&'a self, _ctx: &'a AutocompleteContext) -> AutocompleteFuture<'a> {
            let counter = Arc::clone(&self.counter);
            let label = self.label;
            Box::pin(async move {
                // Yield once so the executor genuinely drives both futures.
                tokio::task::yield_now().await;
                counter.fetch_add(1, Ordering::SeqCst);
                vec![item(label, AutocompleteItemKind::Custom)]
            })
        }
    }

    #[tokio::test]
    async fn test_combined_provider_async_concurrency() {
        let counter = Arc::new(AtomicUsize::new(0));
        let combined = CombinedAutocompleteProvider::with_providers(vec![
            Arc::new(CountingProvider {
                counter: Arc::clone(&counter),
                label: "p0",
            }),
            Arc::new(CountingProvider {
                counter: Arc::clone(&counter),
                label: "p1",
            }),
        ]);
        let items = combined.query(&ctx("", AutocompleteTrigger::Manual)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 2, "both providers must run");
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["p0", "p1"]);
    }

    #[test]
    fn test_autocomplete_context_query_extraction() {
        // Editor builds the context; here we only assert the struct holds what
        // a caller would put in.
        let c = AutocompleteContext {
            text: "/hel".to_string(),
            cursor: 4,
            trigger: AutocompleteTrigger::Slash,
            query: "hel".to_string(),
        };
        assert_eq!(c.text, "/hel");
        assert_eq!(c.cursor, 4);
        assert_eq!(c.trigger, AutocompleteTrigger::Slash);
        assert_eq!(c.query, "hel");
    }

    #[test]
    fn test_autocomplete_item_construction() {
        let it = AutocompleteItem {
            label: "src/main.rs".to_string(),
            detail: Some("file".to_string()),
            insert_text: "src/main.rs".to_string(),
            kind: AutocompleteItemKind::File,
        };
        assert_eq!(it.label, "src/main.rs");
        assert_eq!(it.detail.as_deref(), Some("file"));
        assert_eq!(it.kind, AutocompleteItemKind::File);
        let cloned = it.clone();
        assert_eq!(cloned.insert_text, "src/main.rs");
    }

    #[test]
    fn test_autocomplete_trigger_equality() {
        assert_eq!(AutocompleteTrigger::Slash, AutocompleteTrigger::Slash);
        assert_ne!(AutocompleteTrigger::Slash, AutocompleteTrigger::At);
        assert_ne!(AutocompleteTrigger::At, AutocompleteTrigger::Manual);
    }

    // Verifies a downstream user can implement `AutocompleteProvider` and plug
    // it into `CombinedAutocompleteProvider` through the public surface alone.
    struct UserProvider;

    impl AutocompleteProvider for UserProvider {
        fn query<'a>(&'a self, _ctx: &'a AutocompleteContext) -> AutocompleteFuture<'a> {
            Box::pin(async {
                vec![AutocompleteItem {
                    label: "user".to_string(),
                    detail: None,
                    insert_text: "user".to_string(),
                    kind: AutocompleteItemKind::Custom,
                }]
            })
        }
    }

    #[tokio::test]
    async fn test_autocomplete_provider_trait_implementable_in_user_code() {
        let combined =
            CombinedAutocompleteProvider::with_providers(vec![Arc::new(UserProvider) as Arc<_>]);
        let items = combined.query(&ctx("", AutocompleteTrigger::Manual)).await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "user");
        assert_eq!(items[0].kind, AutocompleteItemKind::Custom);
    }
}
