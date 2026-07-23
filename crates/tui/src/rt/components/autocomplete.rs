//! Autocomplete — the provider contract and suggestion popup for the rt editor.
//!
//! The rt-native counterpart to the legacy `crate::components::autocomplete`.
//! Where the legacy layer is async and walks the real filesystem, this is
//! **synchronous and data-injected**: a provider is handed its data source
//! (a slash-command table, a file list) at construction, so the matching
//! semantics are exercised without a tokio runtime or a tempdir. The editor
//! (see [`Editor`](super::Editor)) drives a provider off its
//! [`context_at_cursor`](super::Editor::context_at_cursor) seam and feeds the
//! results into the [`Autocomplete`] popup.
//!
//! # The two providers and the trigger router
//!
//! - [`SlashProvider`] answers the `/` trigger only. A `/` opens it at the
//!   **start of a line** (per-line start); matching is a **case-sensitive
//!   prefix** on the command name, and the accepted `insert_text` keeps the
//!   leading slash (`/help`) so the buffer lands a well-formed command.
//! - [`PathProvider`] answers the `@` trigger only. Its match scope is pinned
//!   by the query shape (Decision Log): a query **without** `/` is a **basename
//!   prefix** match, plus a dot-prefixed query (`.rs`) is an **extension**
//!   (basename suffix) match — it never matches an intermediate path component;
//!   a query **containing** `/` (`src/`) is a **relative-path substring** match,
//!   so it scopes into a subtree. Matching is case-insensitive; the accepted
//!   `insert_text` is `@path`, with the whole span double-quoted (`@"a b"`) when
//!   the path contains a space.
//! - [`CombinedProvider`] routes by trigger: it forwards to whichever member
//!   claims the trigger char, so the slash provider never eats an `@` query and
//!   the path provider never eats a `/` query.
//!
//! The path matching is deliberately **prefix / extension / substring**, not
//! fuzzy — the legacy `fuzzy` selector filter is a different surface and does
//! not apply here.
//!
//! # The popup
//!
//! [`Autocomplete`] is the popup state machine: it holds the current candidate
//! list, the selection index, and a scroll window capped at
//! [`MAX_VISIBLE`] rows. Up/Down move the indicator (and scroll the window),
//! never the buffer caret. An empty candidate list means the popup is not
//! visible at all — there is never an empty frame.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Widget;

use super::truncate_with_ellipsis;

/// The most candidate rows the popup shows before it scrolls its window. Mirrors
/// the legacy 8-row cap.
pub const MAX_VISIBLE: usize = 8;

/// A single completion candidate produced by a provider.
///
/// `label` is what the popup shows; `insert_text` is what the editor splices in
/// on Tab-accept, replacing the trigger token (`/query` or `@query`) under the
/// caret. Keeping the two distinct lets a path label read as the bare name while
/// the insertion carries the sigil (and quotes for spaced paths).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteItem {
    /// The text shown in the popup row.
    pub label: String,
    /// The text spliced into the buffer on accept, replacing the trigger token.
    pub insert_text: String,
}

impl AutocompleteItem {
    /// A candidate whose label and insertion text share the same string.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            label: text.clone(),
            insert_text: text,
        }
    }

    /// A candidate with a distinct display label and insertion text.
    pub fn with_insert(label: impl Into<String>, insert_text: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            insert_text: insert_text.into(),
        }
    }
}

/// A source of completion candidates for one trigger char.
///
/// A provider is queried with the trigger char that opened the token and the
/// prefix typed after it. It returns candidates only when it *claims* the
/// trigger; a provider that does not handle the trigger returns an empty vec so
/// the [`CombinedProvider`] router can tell "no matches for my trigger" from
/// "not my trigger" (the router keys off [`AutocompleteProvider::handles`]).
pub trait AutocompleteProvider: Send + Sync {
    /// Whether this provider answers the given trigger char.
    fn handles(&self, trigger: char) -> bool;

    /// The candidates for `prefix` under `trigger`. Only called when
    /// [`handles`](AutocompleteProvider::handles) returned `true`.
    fn query(&self, trigger: char, prefix: &str) -> Vec<AutocompleteItem>;
}

// ============================================================================
// SlashProvider — `/command` completion
// ============================================================================

/// A slash command: the name (without the leading `/`) and an optional
/// description shown dimmed after the label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    /// The command name, without the leading slash.
    pub name: String,
    /// An optional one-line description.
    pub description: Option<String>,
}

impl SlashCommand {
    /// A command with just a name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
        }
    }

    /// Attach a description shown dimmed in the popup row.
    #[must_use]
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

/// Completes `/command` from a static, injected command table.
///
/// Answers the `/` trigger only. Matching is a **case-sensitive prefix** on the
/// command name (`/He` does not match `help`); the accepted `insert_text`
/// carries the leading slash so the buffer lands `/help`, not a plain-text
/// `help` message.
pub struct SlashProvider {
    commands: Vec<SlashCommand>,
}

impl SlashProvider {
    /// A provider over the given command table.
    pub fn new(commands: Vec<SlashCommand>) -> Self {
        Self { commands }
    }
}

impl AutocompleteProvider for SlashProvider {
    fn handles(&self, trigger: char) -> bool {
        trigger == '/'
    }

    fn query(&self, trigger: char, prefix: &str) -> Vec<AutocompleteItem> {
        if trigger != '/' {
            return Vec::new();
        }
        self.commands
            .iter()
            .filter(|cmd| cmd.name.starts_with(prefix))
            .map(|cmd| {
                let label = match &cmd.description {
                    Some(desc) if !desc.is_empty() => format!("/{}  {}", cmd.name, desc),
                    _ => format!("/{}", cmd.name),
                };
                AutocompleteItem::with_insert(label, format!("/{}", cmd.name))
            })
            .collect()
    }
}

// ============================================================================
// PathProvider — `@path` completion
// ============================================================================

/// One entry in the [`PathProvider`]'s injected file list: a project-relative
/// path and whether it is a directory (rendered with a trailing `/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathEntry {
    /// The path relative to the project root, using `/` separators.
    pub rel_path: String,
    /// Whether this entry is a directory.
    pub is_dir: bool,
}

impl PathEntry {
    /// A file entry.
    pub fn file(rel_path: impl Into<String>) -> Self {
        Self {
            rel_path: rel_path.into(),
            is_dir: false,
        }
    }

    /// A directory entry (shown with a trailing `/`).
    pub fn dir(rel_path: impl Into<String>) -> Self {
        Self {
            rel_path: rel_path.into(),
            is_dir: true,
        }
    }
}

/// Completes `@path` from an injected list of project-relative paths.
///
/// Answers the `@` trigger only. The match scope is pinned by the query shape:
/// - no `/` in the query → **basename prefix** (`RE` → `README.md`), or, for a
///   dot-prefixed query (`.rs`), **basename extension** (suffix); it never
///   matches an intermediate path component (`@RE` never pulls in
///   `.claude/worktrees/` just because "worktrees" contains "re");
/// - a `/` in the query → **relative-path substring** (`src/` also surfaces
///   `vendor/src/x.rs`), scoping into a subtree.
///
/// Matching is case-insensitive. The accepted `insert_text` is `@path`; when the
/// path contains a space the whole span is double-quoted (`@"my file.txt"`).
pub struct PathProvider {
    entries: Vec<PathEntry>,
}

impl PathProvider {
    /// A provider over the given file list.
    pub fn new(entries: Vec<PathEntry>) -> Self {
        Self { entries }
    }

    /// Whether `entry` matches `query` under the shape-driven rules.
    fn entry_matches(entry: &PathEntry, query_lower: &str) -> bool {
        if query_lower.is_empty() {
            return true;
        }
        let rel_lower = entry.rel_path.to_lowercase();
        if query_lower.contains('/') {
            // Path-scoped query → substring on the full relative path.
            rel_lower.contains(query_lower)
        } else {
            let basename_lower = entry
                .rel_path
                .rsplit('/')
                .next()
                .unwrap_or(&entry.rel_path)
                .to_lowercase();
            if let Some(stripped) = query_lower.strip_prefix('.') {
                // Extension completion: `.rs` → basename ends with `.rs`.
                // Guard against a bare `.` (matches every basename with a dot).
                if stripped.is_empty() {
                    basename_lower.starts_with('.')
                } else {
                    basename_lower.ends_with(query_lower)
                }
            } else {
                basename_lower.starts_with(query_lower)
            }
        }
    }

    /// The display label and insertion text for a matching entry.
    fn item_for(entry: &PathEntry) -> AutocompleteItem {
        let label = if entry.is_dir {
            format!("{}/", entry.rel_path)
        } else {
            entry.rel_path.clone()
        };
        let insert = if label.contains(' ') {
            format!("@\"{label}\"")
        } else {
            format!("@{label}")
        };
        AutocompleteItem::with_insert(label, insert)
    }
}

impl AutocompleteProvider for PathProvider {
    fn handles(&self, trigger: char) -> bool {
        trigger == '@'
    }

    fn query(&self, trigger: char, prefix: &str) -> Vec<AutocompleteItem> {
        if trigger != '@' {
            return Vec::new();
        }
        let query_lower = prefix.to_lowercase();
        self.entries
            .iter()
            .filter(|entry| Self::entry_matches(entry, &query_lower))
            .map(Self::item_for)
            .collect()
    }
}

// ============================================================================
// CombinedProvider — trigger router
// ============================================================================

/// Routes a query to whichever member provider claims the trigger char.
///
/// The router is what keeps the two triggers disjoint: a `/` query goes to the
/// slash provider (the path provider is skipped), and an `@` query goes to the
/// path provider (the slash provider is skipped). Members are consulted in
/// order; the first that [`handles`](AutocompleteProvider::handles) the trigger
/// answers.
pub struct CombinedProvider {
    providers: Vec<Box<dyn AutocompleteProvider>>,
}

impl CombinedProvider {
    /// A router over the given member providers, in priority order.
    pub fn new(providers: Vec<Box<dyn AutocompleteProvider>>) -> Self {
        Self { providers }
    }
}

impl AutocompleteProvider for CombinedProvider {
    fn handles(&self, trigger: char) -> bool {
        self.providers.iter().any(|p| p.handles(trigger))
    }

    fn query(&self, trigger: char, prefix: &str) -> Vec<AutocompleteItem> {
        for provider in &self.providers {
            if provider.handles(trigger) {
                return provider.query(trigger, prefix);
            }
        }
        Vec::new()
    }
}

// ============================================================================
// Autocomplete — the suggestion popup
// ============================================================================

/// The suggestion popup: a candidate list with a moving selection indicator and
/// a scroll window capped at [`MAX_VISIBLE`] rows.
///
/// The popup is **visible iff it holds at least one candidate** — setting an
/// empty candidate list closes it, so a zero-match query never leaves an empty
/// frame. Up/Down move the indicator and scroll the window to keep the selection
/// visible; they never touch the buffer caret.
#[derive(Debug, Clone, Default)]
pub struct Autocomplete {
    items: Vec<AutocompleteItem>,
    selected: usize,
    scroll: usize,
}

impl Autocomplete {
    /// An empty, closed popup.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the candidate list, resetting the selection to the first row.
    ///
    /// An empty list closes the popup. A non-empty list opens it with the first
    /// candidate selected and the window scrolled to the top.
    pub fn set_items(&mut self, items: Vec<AutocompleteItem>) {
        self.items = items;
        self.selected = 0;
        self.scroll = 0;
    }

    /// Close the popup, dropping its candidates.
    pub fn close(&mut self) {
        self.items.clear();
        self.selected = 0;
        self.scroll = 0;
    }

    /// Whether the popup is visible — true exactly when it holds a candidate.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        !self.items.is_empty()
    }

    /// The candidate count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the popup holds no candidates.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The selection index within the candidate list.
    #[must_use]
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// The top of the visible scroll window within the candidate list.
    #[must_use]
    pub fn scroll_offset(&self) -> usize {
        self.scroll
    }

    /// The currently selected candidate, or `None` when the popup is closed.
    #[must_use]
    pub fn selected(&self) -> Option<&AutocompleteItem> {
        self.items.get(self.selected)
    }

    /// A read-only view of the candidate list.
    #[must_use]
    pub fn items(&self) -> &[AutocompleteItem] {
        &self.items
    }

    /// Move the selection down one row, wrapping past the end. A no-op when
    /// closed. Scrolls the window to keep the selection visible.
    pub fn select_next(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.items.len();
        self.clamp_scroll();
    }

    /// Move the selection up one row, wrapping past the start. A no-op when
    /// closed. Scrolls the window to keep the selection visible.
    pub fn select_prev(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.items.len() - 1
        } else {
            self.selected - 1
        };
        self.clamp_scroll();
    }

    /// The number of candidate rows the popup paints for the current list: the
    /// candidate count clamped to [`MAX_VISIBLE`].
    #[must_use]
    pub fn visible_rows(&self) -> usize {
        self.items.len().min(MAX_VISIBLE)
    }

    /// Keep the [`MAX_VISIBLE`]-row window over the selection.
    fn clamp_scroll(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + MAX_VISIBLE {
            self.scroll = self.selected + 1 - MAX_VISIBLE;
        }
    }

    /// Paint the popup into `area`, top-aligned, showing at most [`MAX_VISIBLE`]
    /// rows of the scroll window. The selected row is painted reversed; other
    /// rows carry a leading space where the indicator sits. Nothing is painted
    /// when the popup is closed or the area is empty.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if self.items.is_empty() || area.is_empty() {
            return;
        }
        let width = area.width as usize;
        let rows = (area.height as usize).min(self.visible_rows());
        let end = (self.scroll + rows).min(self.items.len());
        let mut lines: Vec<Line> = Vec::with_capacity(rows);
        for (offset, item) in self.items[self.scroll..end].iter().enumerate() {
            let abs = self.scroll + offset;
            let selected = abs == self.selected;
            let indicator = if selected { "▸ " } else { "  " };
            // Reserve the indicator columns before truncating the label.
            let budget = width.saturating_sub(2);
            let label = truncate_with_ellipsis(&item.label, budget);
            let text = format!("{indicator}{label}");
            let style = if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(text, style)));
        }
        Text::from(lines).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slash(names: &[&str]) -> SlashProvider {
        SlashProvider::new(names.iter().map(|n| SlashCommand::new(*n)).collect())
    }

    fn labels(items: &[AutocompleteItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    fn inserts(items: &[AutocompleteItem]) -> Vec<&str> {
        items.iter().map(|i| i.insert_text.as_str()).collect()
    }

    // --- SlashProvider ------------------------------------------------------

    #[test]
    fn slash_prefix_match_is_case_sensitive_and_keeps_sigil() {
        let p = slash(&["help", "history", "model", "quit"]);
        let items = p.query('/', "h");
        assert_eq!(inserts(&items), vec!["/help", "/history"]);
        // Case-sensitive: uppercase prefix matches nothing.
        assert!(p.query('/', "H").is_empty());
    }

    #[test]
    fn slash_empty_query_returns_all() {
        let p = slash(&["help", "model", "quit"]);
        assert_eq!(p.query('/', "").len(), 3);
    }

    #[test]
    fn slash_ignores_at_trigger() {
        let p = slash(&["help"]);
        assert!(!p.handles('@'));
        assert!(p.query('@', "h").is_empty());
    }

    // --- PathProvider -------------------------------------------------------

    fn paths() -> PathProvider {
        PathProvider::new(vec![
            PathEntry::file("README.md"),
            PathEntry::file(".gitignore"),
            PathEntry::file("main.rs"),
            PathEntry::file("lib.rs"),
            PathEntry::dir("src"),
            PathEntry::file("src/main.rs"),
            PathEntry::file("src/inner/util.rs"),
            PathEntry::file("vendor/src/x.rs"),
            PathEntry::dir(".claude/worktrees"),
            PathEntry::file(".claude/worktrees/cache.txt"),
        ])
    }

    #[test]
    fn path_basename_prefix_excludes_intermediate_components() {
        // `@RE` → README.md by basename prefix; must NOT pull in entries that
        // only match on an intermediate path component (worktrees contains "re")
        // nor `.gitignore` (basename does not start with "re").
        let items = paths().query('@', "RE");
        let got = labels(&items);
        assert!(got.contains(&"README.md"), "got: {got:?}");
        for unwanted in [
            ".gitignore",
            ".claude/worktrees/",
            ".claude/worktrees/cache.txt",
        ] {
            assert!(!got.contains(&unwanted), "@RE leaked {unwanted}: {got:?}");
        }
    }

    #[test]
    fn path_extension_query_matches_basename_suffix() {
        let items = paths().query('@', ".rs");
        let got = labels(&items);
        assert!(got.contains(&"main.rs"));
        assert!(got.contains(&"lib.rs"));
        assert!(got.contains(&"src/main.rs"));
        assert!(!got.contains(&"README.md"), "got: {got:?}");
    }

    #[test]
    fn path_slash_query_is_relative_path_substring() {
        // `@src/` scopes into every path whose relative path contains `src/` —
        // including `vendor/src/x.rs` (substring, not just top-level prefix).
        let items = paths().query('@', "src/");
        let got = labels(&items);
        assert!(got.contains(&"src/main.rs"), "got: {got:?}");
        assert!(got.contains(&"src/inner/util.rs"), "got: {got:?}");
        assert!(got.contains(&"vendor/src/x.rs"), "got: {got:?}");
        assert!(!got.contains(&"README.md"), "got: {got:?}");
    }

    #[test]
    fn path_match_is_case_insensitive() {
        let items = paths().query('@', "main");
        let got = labels(&items);
        assert!(got.contains(&"main.rs"));
        let upper = paths().query('@', "MAIN");
        assert!(labels(&upper).contains(&"main.rs"), "case-insensitive");
    }

    #[test]
    fn path_empty_query_returns_all() {
        assert_eq!(paths().query('@', "").len(), 10);
    }

    #[test]
    fn path_dir_label_has_trailing_slash_and_insert_is_at_path() {
        let items = PathProvider::new(vec![PathEntry::dir("src")]).query('@', "sr");
        assert_eq!(labels(&items), vec!["src/"]);
        assert_eq!(inserts(&items), vec!["@src/"]);
    }

    #[test]
    fn path_spaced_path_is_double_quoted_in_insert() {
        let items = PathProvider::new(vec![PathEntry::file("my file.txt")]).query('@', "my");
        assert_eq!(inserts(&items), vec!["@\"my file.txt\""]);
        // The label stays unquoted.
        assert_eq!(labels(&items), vec!["my file.txt"]);
    }

    #[test]
    fn path_ignores_slash_trigger() {
        let p = PathProvider::new(vec![PathEntry::file("main.rs")]);
        assert!(!p.handles('/'));
        assert!(p.query('/', "m").is_empty());
    }

    // --- CombinedProvider routing ------------------------------------------

    fn combined() -> CombinedProvider {
        CombinedProvider::new(vec![
            Box::new(slash(&["alpha", "beta"])),
            Box::new(PathProvider::new(vec![
                PathEntry::file("alpha.txt"),
                PathEntry::file("main.rs"),
            ])),
        ])
    }

    #[test]
    fn combined_routes_slash_to_slash_provider_only() {
        let items = combined().query('/', "alpha");
        assert_eq!(labels(&items), vec!["/alpha"]);
    }

    #[test]
    fn combined_routes_at_to_path_provider_only() {
        let items = combined().query('@', "alpha");
        // Only the path member answers `@` — the slash `/alpha` never leaks.
        assert_eq!(labels(&items), vec!["alpha.txt"]);
    }

    #[test]
    fn combined_handles_reports_both_triggers() {
        let c = combined();
        assert!(c.handles('/'));
        assert!(c.handles('@'));
        assert!(!c.handles('#'));
    }

    // --- Autocomplete popup -------------------------------------------------

    fn popup(n: usize) -> Autocomplete {
        let mut ac = Autocomplete::new();
        ac.set_items(
            (0..n)
                .map(|i| AutocompleteItem::new(format!("item{i}")))
                .collect(),
        );
        ac
    }

    #[test]
    fn popup_empty_items_is_not_visible() {
        let mut ac = Autocomplete::new();
        assert!(!ac.is_visible());
        ac.set_items(vec![AutocompleteItem::new("x")]);
        assert!(ac.is_visible());
        // A zero-match refresh closes it — no empty frame.
        ac.set_items(vec![]);
        assert!(!ac.is_visible());
    }

    #[test]
    fn popup_navigation_moves_indicator_and_wraps() {
        let mut ac = popup(3);
        assert_eq!(ac.selected_index(), 0);
        ac.select_next();
        assert_eq!(ac.selected_index(), 1);
        ac.select_next();
        ac.select_next(); // wraps
        assert_eq!(ac.selected_index(), 0);
        ac.select_prev(); // wraps to end
        assert_eq!(ac.selected_index(), 2);
    }

    #[test]
    fn popup_window_caps_at_eight_and_scrolls() {
        let mut ac = popup(20);
        assert_eq!(ac.visible_rows(), MAX_VISIBLE);
        // Walk down to index 8 — the window must scroll so the selection stays
        // visible (offset advances past 0).
        for _ in 0..MAX_VISIBLE {
            ac.select_next();
        }
        assert_eq!(ac.selected_index(), MAX_VISIBLE);
        assert!(ac.scroll_offset() > 0, "window scrolled to keep selection");
        assert_eq!(ac.scroll_offset() + MAX_VISIBLE, ac.selected_index() + 1);
    }

    #[test]
    fn popup_close_drops_candidates() {
        let mut ac = popup(3);
        ac.close();
        assert!(!ac.is_visible());
        assert!(ac.selected().is_none());
    }
}
