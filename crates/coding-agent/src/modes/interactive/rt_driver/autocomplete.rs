//! Editor autocomplete wiring — the slash-command source and the composite
//! provider that serves both triggers through the editor's single slot.
//!
//! The editor ([`hand_tui::rt::components::Editor`]) carries one provider slot
//! and a trigger-aware caret seam
//! ([`context_at_cursor`](hand_tui::rt::components::Editor::context_at_cursor))
//! that already distinguishes a `/` at line start from an `@` mention. This
//! module fills the slot with a [`CombinedProvider`] routing by trigger char:
//!
//! - `/` → [`SlashCommandProvider`] (this module): completes the registered
//!   slash commands. Candidates are snapshotted from
//!   [`SlashCommandRegistry`] — the same single source the `/help` listing is
//!   pinned against — so the popup never drifts from the dispatchable set.
//! - `@` → the cwd path source from [`mention`]: untouched — the composite
//!   only routes, it never re-filters, so `@` completion behaves exactly as it
//!   did when the mention provider was installed directly.
//!
//! Slash matching is a **case-insensitive prefix** on the primary command
//! name; a bare `/` lists every command, bounded by [`MAX_CANDIDATES`] (the
//! popup separately caps *visible* rows). An accepted candidate splices
//! `/name ` — with a trailing space — when the command accepts arguments, so
//! the user can keep typing the argument; a no-argument command splices just
//! `/name`, ready to submit.

use std::path::Path;
use std::sync::Arc;

use hand_tui::rt::components::{AutocompleteItem, AutocompleteProvider, CombinedProvider};

use crate::core::slash_commands::SlashCommandRegistry;

use super::mention;

/// Bound on the candidates one slash query returns. The built-in registry is
/// well under this, but the snapshot is capped anyway so a future registry
/// growth (e.g. extension commands) can never flood the popup's candidate
/// list or the per-keystroke prefix scan.
pub const MAX_CANDIDATES: usize = 50;

/// One completable command, snapshotted from the registry at build time.
struct CommandEntry {
    /// Primary name, without the leading slash.
    name: String,
    /// The one-line registry description, shown after the name in the popup row.
    description: String,
    /// Whether the command takes an argument (drives the trailing-space splice).
    accepts_args: bool,
}

/// Completes `/command` from the built-in slash-command registry.
///
/// Answers the `/` trigger only, so `@`-mention completion and ordinary typing
/// are unaffected. Matching is a case-insensitive prefix on the primary name
/// (aliases are not listed — one row per command, same as `/help`); the
/// accepted `insert_text` keeps the leading slash and appends a trailing space
/// for argument-taking commands.
pub struct SlashCommandProvider {
    commands: Vec<CommandEntry>,
}

impl SlashCommandProvider {
    /// Snapshot the built-in registry — the same source `/help` is pinned
    /// against — capped at [`MAX_CANDIDATES`] entries.
    #[must_use]
    pub fn from_registry() -> Self {
        let registry = SlashCommandRegistry::new();
        let commands = registry
            .commands()
            .iter()
            .take(MAX_CANDIDATES)
            .map(|cmd| CommandEntry {
                name: cmd.name.clone(),
                description: cmd.description.clone(),
                accepts_args: cmd.accepts_args,
            })
            .collect();
        Self { commands }
    }

    /// The popup row for one command: `/name  description` (the same shape
    /// hand-tui's own slash source renders), inserting `/name ` when the
    /// command accepts an argument and `/name` when it does not.
    fn item_for(entry: &CommandEntry) -> AutocompleteItem {
        let label = if entry.description.is_empty() {
            format!("/{}", entry.name)
        } else {
            format!("/{}  {}", entry.name, entry.description)
        };
        let insert = if entry.accepts_args {
            format!("/{} ", entry.name)
        } else {
            format!("/{}", entry.name)
        };
        AutocompleteItem::with_insert(label, insert)
    }
}

impl AutocompleteProvider for SlashCommandProvider {
    fn handles(&self, trigger: char) -> bool {
        trigger == '/'
    }

    fn query(&self, trigger: char, prefix: &str) -> Vec<AutocompleteItem> {
        if trigger != '/' {
            return Vec::new();
        }
        let prefix_lower = prefix.to_ascii_lowercase();
        self.commands
            .iter()
            .filter(|cmd| cmd.name.to_ascii_lowercase().starts_with(&prefix_lower))
            .map(Self::item_for)
            .collect()
    }
}

/// Adapter letting the `Arc`-shared mention provider sit inside hand-tui's
/// boxed [`CombinedProvider`] without changing
/// [`mention::build_mention_provider`]'s signature (its direct users keep the
/// `Arc` form). Pure delegation — no behaviour of its own.
struct SharedProvider(Arc<dyn AutocompleteProvider>);

impl AutocompleteProvider for SharedProvider {
    fn handles(&self, trigger: char) -> bool {
        self.0.handles(trigger)
    }

    fn query(&self, trigger: char, prefix: &str) -> Vec<AutocompleteItem> {
        self.0.query(trigger, prefix)
    }
}

/// Build the chat editor's composite autocomplete provider: `/` routes to the
/// slash-command source, `@` routes to the cwd path source from [`mention`].
/// Any other context yields nothing, so normal typing and submit are
/// unaffected.
#[must_use]
pub fn build_editor_provider(cwd: &Path) -> Arc<dyn AutocompleteProvider> {
    Arc::new(CombinedProvider::new(vec![
        Box::new(SlashCommandProvider::from_registry()),
        Box::new(SharedProvider(mention::build_mention_provider(cwd))),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[AutocompleteItem]) -> Vec<String> {
        items.iter().map(|i| i.label.clone()).collect()
    }

    fn inserts(items: &[AutocompleteItem]) -> Vec<String> {
        items.iter().map(|i| i.insert_text.clone()).collect()
    }

    // --- SlashCommandProvider ---------------------------------------------

    #[test]
    fn a_mo_prefix_surfaces_model_from_the_registry() {
        let provider = SlashCommandProvider::from_registry();
        let items = provider.query('/', "mo");
        let got = inserts(&items);
        // `/model` accepts an argument, so its splice carries a trailing space.
        assert!(got.contains(&"/model ".to_string()), "got: {got:?}");
        assert!(
            !got.iter().any(|i| i.starts_with("/help")),
            "prefix `mo` must not surface /help: {got:?}"
        );
    }

    #[test]
    fn prefix_match_is_case_insensitive() {
        let provider = SlashCommandProvider::from_registry();
        let got = inserts(&provider.query('/', "MO"));
        assert!(got.contains(&"/model ".to_string()), "got: {got:?}");
    }

    #[test]
    fn a_bare_slash_lists_a_bounded_command_set() {
        let provider = SlashCommandProvider::from_registry();
        let items = provider.query('/', "");
        assert!(!items.is_empty(), "bare `/` lists the commands");
        assert!(
            items.len() <= MAX_CANDIDATES,
            "candidate list is bounded: {} > {MAX_CANDIDATES}",
            items.len()
        );
    }

    #[test]
    fn names_and_descriptions_come_from_the_real_registry() {
        let provider = SlashCommandProvider::from_registry();
        let items = provider.query('/', "");
        let got = labels(&items);
        // Known built-ins are present, each carrying its non-empty registry
        // description after the name (`/name  description`).
        for name in ["/help", "/model", "/quit"] {
            let row = got
                .iter()
                .find(|l| *l == name || l.starts_with(&format!("{name}  ")))
                .unwrap_or_else(|| panic!("{name} missing from bare `/`: {got:?}"));
            assert!(
                row.len() > name.len() + 2,
                "{name} row carries a description: {row:?}"
            );
        }
    }

    #[test]
    fn a_no_argument_command_splices_without_a_trailing_space() {
        let provider = SlashCommandProvider::from_registry();
        // `/help` takes no argument → splice is submit-ready, no trailing space.
        let help = inserts(&provider.query('/', "help"));
        assert!(help.contains(&"/help".to_string()), "got: {help:?}");
        // `/name <new-name>` takes one → splice keeps the argument seam open.
        let name = inserts(&provider.query('/', "name"));
        assert!(name.contains(&"/name ".to_string()), "got: {name:?}");
    }

    #[test]
    fn a_no_match_prefix_returns_empty() {
        let provider = SlashCommandProvider::from_registry();
        assert!(provider.query('/', "zzz-no-such-command").is_empty());
    }

    #[test]
    fn is_inert_on_the_at_trigger() {
        let provider = SlashCommandProvider::from_registry();
        assert!(!provider.handles('@'));
        assert!(provider.query('@', "READ").is_empty());
    }

    // --- the composite ------------------------------------------------------

    #[test]
    fn composite_serves_both_triggers_and_nothing_else() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();

        let provider = build_editor_provider(dir.path());
        assert!(provider.handles('/'));
        assert!(provider.handles('@'));
        assert!(!provider.handles('#'), "no third trigger");

        // `/` routes to the slash source only.
        let slash = inserts(&provider.query('/', "mo"));
        assert!(slash.contains(&"/model ".to_string()), "got: {slash:?}");
        assert!(
            !labels(&provider.query('/', "READ")).contains(&"README.md".to_string()),
            "a `/` query never reaches the path source"
        );

        // `@` routes to the path source only — byte-identical to the direct
        // mention install (same labels, same sigil-carrying inserts).
        let at = provider.query('@', "READM");
        assert!(
            labels(&at).contains(&"README.md".to_string()),
            "got: {:?}",
            labels(&at)
        );
        assert_eq!(
            at,
            mention::build_mention_provider(dir.path()).query('@', "READM"),
            "the composite must not alter the mention provider's answers"
        );
        assert!(
            !inserts(&at).iter().any(|i| i.starts_with('/')),
            "an `@` query never reaches the slash source"
        );
    }
}
