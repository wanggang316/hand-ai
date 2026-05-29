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

    /// Synchronous fast path. Implementations that can answer the query
    /// without awaiting (in-memory tables, bounded directory walks)
    /// should return `Some(items)`; the editor delivers them inline
    /// from `maybe_trigger_autocomplete` so suggestions appear on the
    /// same keystroke they were triggered by, with no run-loop wiring.
    ///
    /// Returning `None` defers to the async path (`query`) which a
    /// driver task is expected to drain via `pending_autocomplete_request`
    /// → `deliver_autocomplete_results`. Until that wiring lands the
    /// async-only providers stay silent — sync is the production path.
    fn query_sync(&self, _ctx: &AutocompleteContext) -> Option<Vec<AutocompleteItem>> {
        None
    }
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

    fn query_sync(&self, ctx: &AutocompleteContext) -> Option<Vec<AutocompleteItem>> {
        // Fan out to each provider's sync path. The first provider that
        // returns `Some` wins — if it returns an empty `Vec`, that's
        // still a definitive "I'm the one that handles this trigger but
        // nothing matches", which is different from `None` (defer).
        // Providers that don't claim the trigger return `None` and the
        // loop continues.
        let mut produced = false;
        let mut out = Vec::new();
        for provider in &self.providers {
            if let Some(items) = provider.query_sync(ctx) {
                produced = true;
                out.extend(items);
            }
        }
        if produced { Some(out) } else { None }
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

impl SlashCommandProvider {
    fn matches(&self, ctx: &AutocompleteContext) -> Vec<AutocompleteItem> {
        // Filter is a prefix match against the command name — `/he`
        // matches `help` and `hotkeys`. Args completion happens after
        // a space (which we currently leave to extension providers).
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
                    // Include the `/` sigil in the insertion text so
                    // `accept_autocomplete` (which deletes from the
                    // trigger char through the cursor) lands a
                    // well-formed slash command. The path provider
                    // already follows this convention with `@<label>`;
                    // mismatching here would strip the `/` and demote
                    // the command to a plain-text message (issue #59).
                    insert_text: format!("/{}", cmd.name),
                    kind: AutocompleteItemKind::SlashCommand,
                }
            })
            .collect()
    }
}

impl AutocompleteProvider for SlashCommandProvider {
    fn query<'a>(&'a self, ctx: &'a AutocompleteContext) -> AutocompleteFuture<'a> {
        let items = self.matches(ctx);
        Box::pin(async move { items })
    }

    fn query_sync(&self, ctx: &AutocompleteContext) -> Option<Vec<AutocompleteItem>> {
        // Slash trigger only — `@path` queries are routed to the path
        // provider in the combined chain. Returning `None` lets the
        // combined provider fall through to its other members.
        if !matches!(ctx.trigger, AutocompleteTrigger::Slash) {
            return None;
        }
        Some(self.matches(ctx))
    }
}

// ============================================================================
// PathAutocompleteProvider — `@path` completion
// ============================================================================

/// Default max BFS depth from the project root. A tool like `fd
/// --max-depth=∞` could rely on its own gitignore handling for
/// pruning; this autocomplete does a manual walk so it caps at 3
/// levels — enough to surface typical `src/...`, `crates/.../*` etc.
/// paths without scanning node_modules / target / .git.
const DEFAULT_PATH_MAX_DEPTH: usize = 3;

/// Default cap on returned entries. Above this the popup becomes unusable
/// anyway; 200 is generous enough to cover repos where the user prefixes
/// with a directory without making the result list unwieldy.
const DEFAULT_PATH_MAX_ENTRIES: usize = 200;

/// Walks the project root looking for files / dirs whose path matches the
/// user's `@<query>` prefix. Uses `std::fs::read_dir` synchronously — paths
/// of interest sit on hot disk, the walk is bounded by depth and count, and
/// the editor calls us inline from `maybe_trigger_autocomplete`, so a
/// tokio runtime is not required.
pub struct PathAutocompleteProvider {
    root: std::path::PathBuf,
    max_depth: usize,
    max_entries: usize,
}

impl PathAutocompleteProvider {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            root: root.into(),
            max_depth: DEFAULT_PATH_MAX_DEPTH,
            max_entries: DEFAULT_PATH_MAX_ENTRIES,
        }
    }

    pub fn with_max_depth(mut self, depth: usize) -> Self {
        self.max_depth = depth;
        self
    }

    pub fn with_max_entries(mut self, n: usize) -> Self {
        self.max_entries = n;
        self
    }

    fn walk(&self, raw_query: &str) -> Vec<AutocompleteItem> {
        // BFS from root. Depth 0 = direct children of root. Skip the usual
        // suspects (`.git`, `target`, `node_modules`, `.venv`) — they
        // dominate the entry budget without ever being something the user
        // wants to `@`-attach.
        let skip: &[&str] = &[".git", "target", "node_modules", ".venv", ".cache"];

        // Quote-aware query: a leading `"` means the caller is mid-typing a
        // quoted path (`@"my doc`) and wants completion *inside* the
        // quoted span. Strip the opening `"` for matching, remember the
        // mode, and emit insert_text that splices into the open quote
        // without duplicating it.
        let (quoted_input, stripped) = match raw_query.strip_prefix('"') {
            Some(rest) => (true, rest),
            None => (false, raw_query),
        };
        let query_lower = stripped.to_lowercase();

        let mut out = Vec::new();
        // No global "visited" set: the old shape (canon-keyed across the
        // whole walk) silently blocked sibling-alias directories like
        // `link-to-pkg → pkg` because `pkg` had already been descended
        // by the time the iteration order reached `link-to-pkg`. The
        // user expects to see both paths surface. Recursion is bounded
        // by `max_depth` + `max_entries`, which is enough to handle the
        // pathological `link-to-self → self` and ancestor-loop cases
        // without losing sibling aliases. The dir-iteration order on
        // ext4 (no sort) used to hide this on macOS HFS+/APFS (which
        // sorts) and surface only in CI.
        let mut queue: std::collections::VecDeque<(std::path::PathBuf, usize)> =
            std::collections::VecDeque::new();
        queue.push_back((self.root.clone(), 0));

        while let Some((dir, depth)) = queue.pop_front() {
            if out.len() >= self.max_entries {
                break;
            }
            let entries = match std::fs::read_dir(&dir) {
                Ok(it) => it,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                if out.len() >= self.max_entries {
                    break;
                }
                let path = entry.path();
                let file_name = match path.file_name().and_then(|s| s.to_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                if skip.contains(&file_name.as_str()) {
                    continue;
                }
                let display = match path.strip_prefix(&self.root) {
                    Ok(rel) => rel.to_string_lossy().into_owned(),
                    Err(_) => path.to_string_lossy().into_owned(),
                };
                // Symlink-aware kind: use file_type() to detect the entry
                // is a symlink, then resolve the target's metadata to
                // decide whether to treat it as a directory for both
                // suggestion-rendering and recursion. We also include
                // plain symlinks-to-files in the result list.
                let file_type = entry.file_type().ok();
                let is_symlink = file_type
                    .as_ref()
                    .map(|ft| ft.is_symlink())
                    .unwrap_or(false);
                let is_dir_direct = file_type.as_ref().map(|ft| ft.is_dir()).unwrap_or(false);
                let target_is_dir = if is_symlink {
                    std::fs::metadata(&path)
                        .map(|m| m.is_dir())
                        .unwrap_or(false)
                } else {
                    false
                };
                let treat_as_dir = is_dir_direct || target_is_dir;

                // Filter scope depends on whether the query carries a
                // path component:
                //   - `@src/` (contains `/`) → match the full relative
                //     path as a substring; lets users scope to a
                //     subdirectory.
                //   - `@RE`   (no `/`) → basename prefix match. This
                //     matches `cd <tab>` / `ls <tab>` muscle memory
                //     and stops `.gitignore` (basename ends in "re"),
                //     `.claude/worktrees/` (intermediate component
                //     contains "re"), and similar false positives
                //     from polluting the popup (#60).
                let basename_lower = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_lowercase())
                    .unwrap_or_default();
                // Smarter filter — query shape determines the match:
                //   `src/`       → full-path substring (subdirectory scope)
                //   `.rs`        → basename ends_with (extension completion)
                //   `RE`         → basename starts_with (the common case)
                // Pre-fix, the filter was `display.to_lowercase()
                // .contains(query)`, which matched intermediate path
                // components — typing `@RE` brought in
                // `.claude/worktrees/` because "worktrees" contains
                // "re", which the user didn't expect (#60).
                let matches = if query_lower.is_empty() {
                    true
                } else if query_lower.contains('/') {
                    display.to_lowercase().contains(&query_lower)
                } else if query_lower.starts_with('.') {
                    basename_lower.ends_with(&query_lower)
                } else {
                    basename_lower.starts_with(&query_lower)
                };
                if matches {
                    let label = if treat_as_dir {
                        format!("{display}/")
                    } else {
                        display.clone()
                    };
                    // Quote-wrap when the path contains spaces — the
                    // editor needs an unambiguous span to splice. When
                    // the caller is already inside an open quote
                    // (`quoted_input` true), emit `@"…"` so the splice
                    // closes the open quote without duplicating the
                    // opening one. Both branches produce the same
                    // wrapped form, so collapse the conditional.
                    let needs_quotes = label.contains(' ') || quoted_input;
                    let insert = if needs_quotes {
                        format!("@\"{label}\"")
                    } else {
                        format!("@{label}")
                    };
                    out.push(AutocompleteItem {
                        label,
                        detail: None,
                        insert_text: insert,
                        kind: AutocompleteItemKind::File,
                    });
                }

                if treat_as_dir && depth + 1 < self.max_depth {
                    queue.push_back((path, depth + 1));
                }
            }
        }

        out
    }
}

impl AutocompleteProvider for PathAutocompleteProvider {
    fn query<'a>(&'a self, ctx: &'a AutocompleteContext) -> AutocompleteFuture<'a> {
        let items = self.query_sync(ctx).unwrap_or_default();
        Box::pin(async move { items })
    }

    fn query_sync(&self, ctx: &AutocompleteContext) -> Option<Vec<AutocompleteItem>> {
        if !matches!(ctx.trigger, AutocompleteTrigger::At) {
            return None;
        }
        Some(self.walk(&ctx.query))
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
        // `insert_text` carries the `/` sigil so that
        // `EditorComponent::accept_autocomplete` (which deletes
        // trigger+query and inserts insert_text) lands a well-formed
        // slash command. See #59.
        assert_eq!(names, vec!["/help", "/history"]);
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

    /// Regression for #59: end-to-end through the real provider —
    /// the user types `/he`, the popup item's `insert_text` must be
    /// `/help` (with sigil), not `help`. Without the sigil,
    /// `EditorComponent::accept_autocomplete` deletes the user's
    /// `/he` then inserts `help`, producing a plain-text message
    /// that the LLM gets instead of running the slash command.
    #[tokio::test]
    async fn test_slash_command_provider_insert_text_includes_sigil() {
        let provider = SlashCommandProvider::new(cmds(&["help"]));
        let items = provider.query(&ctx("he", AutocompleteTrigger::Slash)).await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].insert_text, "/help");
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
        assert_eq!(item.insert_text, "/model");
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

    // ====================================================================
    // query_sync — sync fast path covered by SlashCommandProvider and
    // PathAutocompleteProvider; combined provider fans out
    // ====================================================================

    #[test]
    fn slash_provider_query_sync_only_matches_slash_trigger() {
        let provider = SlashCommandProvider::new(vec![SlashCommand::new("help", "Show help")]);
        // Slash trigger → Some(...) with the match.
        let slash = ctx("he", AutocompleteTrigger::Slash);
        let items = provider.query_sync(&slash).expect("slash returns Some");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "help");
        // @ trigger → None (defer to other providers in a combined chain).
        let at = ctx("he", AutocompleteTrigger::At);
        assert!(provider.query_sync(&at).is_none());
    }

    #[test]
    fn path_provider_query_sync_only_matches_at_trigger() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("hello.rs"), "// hi").unwrap();
        let provider = PathAutocompleteProvider::new(tmp.path().to_path_buf());
        // @ trigger → Some(...) with the match.
        let at = ctx("hello", AutocompleteTrigger::At);
        let items = provider.query_sync(&at).expect("at returns Some");
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"hello.rs"), "got: {labels:?}");
        // Slash trigger → None.
        let slash = ctx("hello", AutocompleteTrigger::Slash);
        assert!(provider.query_sync(&slash).is_none());
    }

    #[test]
    fn path_provider_descends_into_subdirectories_up_to_max_depth() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src/inner")).unwrap();
        std::fs::write(tmp.path().join("src/inner/deep.rs"), "").unwrap();
        let provider = PathAutocompleteProvider::new(tmp.path().to_path_buf());
        let at = ctx("deep", AutocompleteTrigger::At);
        let items = provider.query_sync(&at).expect("at returns Some");
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(
            labels.iter().any(|l| l.ends_with("deep.rs")),
            "depth 2 file not found: {labels:?}"
        );
    }

    #[test]
    fn path_provider_skips_well_known_noise_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        std::fs::write(tmp.path().join(".git/config"), "").unwrap();
        std::fs::create_dir_all(tmp.path().join("target")).unwrap();
        std::fs::write(tmp.path().join("target/debug-binary"), "").unwrap();
        let provider = PathAutocompleteProvider::new(tmp.path().to_path_buf());
        let at = ctx("", AutocompleteTrigger::At);
        let items = provider.query_sync(&at).expect("at returns Some");
        let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
        // Neither `.git/config` nor `target/debug-binary` should leak.
        for forbidden in [".git", "target", "config", "debug-binary"] {
            assert!(
                !labels.iter().any(|l| l.contains(forbidden)),
                "noise dir leaked: {forbidden} in {labels:?}"
            );
        }
    }

    #[test]
    fn combined_provider_sync_routes_by_trigger() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("alpha.txt"), "").unwrap();
        let mut combined = CombinedAutocompleteProvider::new();
        combined.add_provider(Arc::new(SlashCommandProvider::new(vec![
            SlashCommand::new("alpha", "the alpha command"),
        ])));
        combined.add_provider(Arc::new(PathAutocompleteProvider::new(
            tmp.path().to_path_buf(),
        )));

        // `/alpha` → slash provider answers, path provider deferred.
        let slash_items = combined
            .query_sync(&ctx("alpha", AutocompleteTrigger::Slash))
            .expect("slash trigger yields Some");
        let slash_labels: Vec<&str> = slash_items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(slash_labels, vec!["alpha"]);

        // `@alpha` → path provider answers with the file, slash deferred.
        let at_items = combined
            .query_sync(&ctx("alpha", AutocompleteTrigger::At))
            .expect("at trigger yields Some");
        let at_labels: Vec<&str> = at_items.iter().map(|i| i.label.as_str()).collect();
        assert!(at_labels.contains(&"alpha.txt"), "got: {at_labels:?}");
    }

    /// Hidden files (dotfiles like `.gitignore`, `.env.local`) are
    /// surfaced by the path provider — the user may legitimately want
    /// to `@`-attach them. The `.git` directory itself is still
    /// dropped: it's noise that never adds value to a prompt.
    #[tokio::test]
    async fn test_path_provider_includes_dotfiles_excludes_git() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored").unwrap();
        std::fs::write(dir.path().join("visible.txt"), "visible").unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git").join("HEAD"), "ref").unwrap();

        let provider = PathAutocompleteProvider::new(dir.path());
        let items = provider
            .query_sync(&ctx("", AutocompleteTrigger::At))
            .expect("at trigger yields Some");
        let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
        assert!(
            labels.iter().any(|l| l == ".gitignore"),
            "dotfile must surface, got: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l == "visible.txt"),
            "regular file must surface, got: {labels:?}"
        );
        assert!(
            !labels.iter().any(|l| l.starts_with(".git/")),
            ".git/ contents must be dropped, got: {labels:?}"
        );
    }

    /// UC-ac-013 — a path containing spaces is emitted with the entire
    /// path wrapped in double quotes so the editor sees an unambiguous
    /// span. The label stays unquoted so the dropdown shows the natural
    /// name; only `insert_text` carries the quotes.
    #[test]
    fn path_provider_quotes_paths_with_spaces() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("my documents")).unwrap();
        std::fs::write(tmp.path().join("my documents/report.txt"), "").unwrap();

        let provider = PathAutocompleteProvider::new(tmp.path().to_path_buf());
        let items = provider
            .query_sync(&ctx("report", AutocompleteTrigger::At))
            .expect("at returns Some");
        let item = items
            .iter()
            .find(|i| i.label.ends_with("report.txt"))
            .expect("report.txt must be present");
        assert!(
            item.insert_text.starts_with("@\"") && item.insert_text.ends_with('"'),
            "spaced path must be quote-wrapped, got: {:?}",
            item.insert_text
        );
        assert!(
            !item.label.contains('"'),
            "label stays unquoted, got: {:?}",
            item.label
        );
    }

    /// UC-ac-019/020 — when the user is already inside a quoted `@`
    /// completion (`@"my doc`), the provider sees a leading `"` in the
    /// query, strips it for matching, and emits an insert_text that
    /// terminates the quote without duplicating the opening one.
    #[test]
    fn path_provider_continues_inside_open_quote() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("my documents")).unwrap();
        std::fs::write(tmp.path().join("my documents/report.txt"), "").unwrap();

        let provider = PathAutocompleteProvider::new(tmp.path().to_path_buf());
        let items = provider
            .query_sync(&ctx("\"my doc", AutocompleteTrigger::At))
            .expect("at returns Some");
        let item = items
            .iter()
            .find(|i| i.label.contains("my documents"))
            .expect("my documents/ must surface from inside open quote");
        let insert = &item.insert_text;
        assert!(
            insert.starts_with("@\""),
            "insert must keep the open `@\"`, got: {insert:?}"
        );
        assert!(
            insert.ends_with('"'),
            "insert must close the quote exactly once, got: {insert:?}"
        );
        // No double-quoting either side.
        assert_eq!(
            insert.matches('"').count(),
            2,
            "exactly two quotes (open + close), got: {insert:?}"
        );
    }

    /// UC-ac-015/016/017 — the path BFS follows symlinks: symlinked
    /// directories are descended into, symlinked files appear in the
    /// result list, and a self-referential symlink cycle does not hang
    /// or duplicate entries.
    #[cfg(unix)]
    #[test]
    fn path_provider_follows_symlinks_without_cycling() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        // Real tree:  pkg/inner/file.rs
        // Symlink:    link-to-pkg → pkg (directory symlink)
        // Symlink:    file-link    → pkg/inner/file.rs (file symlink)
        std::fs::create_dir_all(tmp.path().join("pkg/inner")).unwrap();
        std::fs::write(tmp.path().join("pkg/inner/file.rs"), "x").unwrap();
        symlink(tmp.path().join("pkg"), tmp.path().join("link-to-pkg")).unwrap();
        symlink(
            tmp.path().join("pkg/inner/file.rs"),
            tmp.path().join("file-link"),
        )
        .unwrap();
        // Self-cycle: cycle → cycle (would loop forever without guard).
        symlink(tmp.path().join("cycle"), tmp.path().join("cycle")).unwrap();

        let provider = PathAutocompleteProvider::new(tmp.path().to_path_buf());
        let items = provider
            .query_sync(&ctx("file.rs", AutocompleteTrigger::At))
            .expect("at returns Some");
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        // Real path surfaces.
        assert!(
            labels.iter().any(|l| l.contains("pkg/inner/file.rs")),
            "real path must surface, got: {labels:?}"
        );
        // Via the directory symlink — the walker descended into link-to-pkg.
        assert!(
            labels
                .iter()
                .any(|l| l.contains("link-to-pkg/inner/file.rs")),
            "symlinked dir must be descended, got: {labels:?}"
        );

        // File-symlink shows up as itself when queried by basename.
        let by_basename = provider
            .query_sync(&ctx("file-link", AutocompleteTrigger::At))
            .expect("at returns Some");
        let bn_labels: Vec<&str> = by_basename.iter().map(|i| i.label.as_str()).collect();
        assert!(
            bn_labels.iter().any(|l| l.contains("file-link")),
            "symlinked file must surface, got: {bn_labels:?}"
        );
    }

    // ---------- UC-ac pending closures (provider-side) ----------

    /// UC-ac-005 — an empty `@` query returns every file and folder
    /// under the project root (modulo the auto-ignore list). The
    /// dropdown shows everything available, not a slice keyed by some
    /// hidden prefix.
    #[test]
    fn path_provider_empty_query_returns_all_entries() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "").unwrap();
        std::fs::write(tmp.path().join("b.md"), "").unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("sub/c.rs"), "").unwrap();

        let provider = PathAutocompleteProvider::new(tmp.path().to_path_buf());
        let items = provider
            .query_sync(&ctx("", AutocompleteTrigger::At))
            .expect("empty @ trigger yields Some");
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"a.txt"), "got: {labels:?}");
        assert!(labels.contains(&"b.md"), "got: {labels:?}");
        assert!(labels.contains(&"sub/"), "got: {labels:?}");
        assert!(labels.contains(&"sub/c.rs"), "nested file too: {labels:?}");
    }

    /// UC-ac-006 — matching by extension (`.rs`) finds every Rust file
    /// regardless of stem. The walk is substring-matched on the full
    /// relative path so an extension query lands on every entry whose
    /// path contains that string.
    #[test]
    fn path_provider_matches_by_extension_in_query() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "").unwrap();
        std::fs::write(tmp.path().join("lib.rs"), "").unwrap();
        std::fs::write(tmp.path().join("readme.md"), "").unwrap();

        let provider = PathAutocompleteProvider::new(tmp.path().to_path_buf());
        let items = provider
            .query_sync(&ctx(".rs", AutocompleteTrigger::At))
            .expect("at returns Some");
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"main.rs"));
        assert!(labels.contains(&"lib.rs"));
        assert!(
            !labels.contains(&"readme.md"),
            "non-matching extension must be filtered out, got: {labels:?}"
        );
    }

    /// UC-ac-007 — matching is case-insensitive. `MAIN` finds
    /// `main.rs`; `Sub` finds `subdir/`. Models often mismatch case
    /// when paraphrasing what the user typed.
    #[test]
    fn path_provider_query_is_case_insensitive() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "").unwrap();
        std::fs::create_dir(tmp.path().join("subdir")).unwrap();

        let provider = PathAutocompleteProvider::new(tmp.path().to_path_buf());
        let upper_main = provider
            .query_sync(&ctx("MAIN", AutocompleteTrigger::At))
            .expect("at returns Some");
        assert!(
            upper_main.iter().any(|i| i.label == "main.rs"),
            "MAIN must match main.rs case-insensitively"
        );

        let mixed_sub = provider
            .query_sync(&ctx("Sub", AutocompleteTrigger::At))
            .expect("at returns Some");
        assert!(
            mixed_sub.iter().any(|i| i.label == "subdir/"),
            "Sub must match subdir/ case-insensitively"
        );
    }

    /// UC-ac-009/010 — nested and deeply-nested file paths surface via
    /// the BFS walk, up to `DEFAULT_PATH_MAX_DEPTH = 3`. A `depth-3`
    /// file is reachable; a `depth-4` file is intentionally outside
    /// the budget.
    #[test]
    fn path_provider_returns_nested_paths_within_depth_budget() {
        let tmp = tempfile::tempdir().unwrap();
        // depth 1: a/
        // depth 2: a/b/
        // depth 3: a/b/c.rs (reachable — depth_cap=3 == file at depth 3)
        // depth 4: a/b/d/deep.rs (beyond budget)
        std::fs::create_dir_all(tmp.path().join("a/b/d")).unwrap();
        std::fs::write(tmp.path().join("a/b/c.rs"), "").unwrap();
        std::fs::write(tmp.path().join("a/b/d/deep.rs"), "").unwrap();

        let provider = PathAutocompleteProvider::new(tmp.path().to_path_buf());
        let items = provider
            .query_sync(&ctx("", AutocompleteTrigger::At))
            .expect("at returns Some");
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(
            labels.contains(&"a/b/c.rs"),
            "depth-3 file should be reachable, got: {labels:?}"
        );
        assert!(
            !labels.iter().any(|l| l.ends_with("deep.rs")),
            "depth-4 file is beyond the default 3-level budget, got: {labels:?}"
        );
    }

    /// Issue #60: typing `@RE` must not pull in entries whose only
    /// "match" was in an intermediate path component. Pre-fix, the
    /// filter ran `display.contains(query)` on the full relative
    /// path, so `.claude/worktrees/` matched `re` because "worktrees"
    /// happens to contain it. Now the basename-only branch keeps
    /// extension completions working (`@.rs` → `main.rs`) without
    /// hauling in intermediate-component matches.
    #[test]
    fn path_provider_basename_filter_excludes_intermediate_path_matches() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("README.md"), "").unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "").unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude/worktrees")).unwrap();
        std::fs::write(tmp.path().join(".claude/worktrees/cache.txt"), "").unwrap();

        let provider = PathAutocompleteProvider::new(tmp.path().to_path_buf());
        let items = provider
            .query_sync(&ctx("RE", AutocompleteTrigger::At))
            .expect("at returns Some");
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(
            labels.contains(&"README.md"),
            "README.md should match @RE, got: {labels:?}"
        );
        for unwanted in [
            ".gitignore",
            ".claude/worktrees/",
            ".claude/worktrees/cache.txt",
        ] {
            assert!(
                !labels.contains(&unwanted),
                "@RE should not match {unwanted:?}, got: {labels:?}"
            );
        }
    }

    /// UC-ac-012 — querying with a relative directory prefix (`src/`)
    /// scopes results to that subtree and recurses inside it.
    #[test]
    fn path_provider_scopes_to_relative_dir_and_recurses() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src/inner")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "").unwrap();
        std::fs::write(tmp.path().join("src/inner/util.rs"), "").unwrap();
        std::fs::write(tmp.path().join("other.txt"), "").unwrap();

        let provider = PathAutocompleteProvider::new(tmp.path().to_path_buf());
        let items = provider
            .query_sync(&ctx("src/", AutocompleteTrigger::At))
            .expect("at returns Some");
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"src/main.rs"));
        assert!(labels.contains(&"src/inner/util.rs"));
        // `other.txt` doesn't contain `src/` so it's filtered out.
        assert!(
            !labels.contains(&"other.txt"),
            "out-of-scope entry leaked, got: {labels:?}"
        );
    }

    /// UC-ac-018 — when the project root's own path string happens to
    /// contain the query (e.g. cwd is `/tmp/foo-test/...` and the
    /// query is `test`), the returned suggestions stay anchored on
    /// entries under the root — the cwd basename is not treated as a
    /// match itself.
    #[test]
    fn path_provider_root_basename_does_not_pollute_results() {
        // Force a tempdir whose name contains the query keyword.
        let tmp = tempfile::Builder::new()
            .prefix("match-keyword-")
            .tempdir()
            .unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
        std::fs::write(tmp.path().join("beta.rs"), "").unwrap();

        let provider = PathAutocompleteProvider::new(tmp.path().to_path_buf());
        let items = provider
            .query_sync(&ctx("alpha", AutocompleteTrigger::At))
            .expect("at returns Some");
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        // The root itself is not a child entry — we never emit a label
        // for the project root, only for entries inside it. So even
        // when the cwd basename matches the keyword, only the
        // child file `alpha.rs` surfaces.
        assert!(labels.contains(&"alpha.rs"));
        assert!(
            !labels.iter().any(|l| l.contains("match-keyword-")),
            "root basename leaked into results, got: {labels:?}"
        );
    }
}
