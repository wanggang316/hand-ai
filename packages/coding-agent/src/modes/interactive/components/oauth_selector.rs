//! Provider picker used by `/login` and `/logout`.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/oauth-selector.ts`.
//!
//! Unlike the simpler picker selectors, this component renders a custom
//! provider list (with per-provider status indicators) instead of wrapping
//! `SelectListComponent`. It owns an embedded
//! [`hand_tui::InputComponent`] for the search box and dispatches to it for
//! any key it doesn't itself handle. Fuzzy filtering uses
//! [`hand_tui::fuzzy_filter`].
//!
//! The TS source resolves provider auth status by calling `authStorage.get(id)`
//! and an optional `getAuthStatus(id)` callback. The Rust port avoids tying
//! the renderer to [`AuthStorage`] / [`ModelRegistry`] directly; instead the
//! constructor accepts a fully-resolved
//! [`AuthSelectorProvider::status`] string per provider, computed by the
//! driver from the same APIs. This keeps the component pure-render and easy
//! to test, matching the view-model pattern used by `footer`.
//!
//! Theming caveat: the TS source pulls `accent`, `success`, `warning`,
//! `muted`, `error` from the coding-agent theme. Until the theme port lands
//! the renderer hardcodes ANSI defaults that match the dark theme palette.
//!
//! TODO(parity): theme integration deferred — see
//! docs/exec-plans/parity-completion.md §A1.

use hand_tui::utils::truncate_to_width;
use hand_tui::{
    Component, Container, FuzzyMatch, HandleResult, InputComponent, InputEvent, SpacerComponent,
    TextComponent, fuzzy_filter,
};
use hand_tui::{Key, KeyName, parse_key};
use tokio::sync::mpsc;

use super::dynamic_border::DynamicBorderComponent;

/// ANSI prefixes used while the theme system is deferred. The host-supplied
/// `provider.status` strings are expected to embed their own colors (success
/// / warning / muted), so the selector itself only needs accent / muted /
/// bold for the title and indicator.
const ACCENT: &str = "\x1b[36m"; // cyan
const MUTED: &str = "\x1b[90m"; // bright black
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

/// Mode the selector is shown in (login vs logout). Mirrors the TS
/// `"login" | "logout"` literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthSelectorMode {
    Login,
    Logout,
}

/// Per-provider view-model. The `status` text (already styled) is rendered
/// to the right of the provider name; the host computes it once via
/// `AuthStorage::get` + `ModelRegistry::provider_auth_status` and hands the
/// fully formatted segment in.
#[derive(Debug, Clone)]
pub struct AuthSelectorProvider {
    pub id: String,
    pub name: String,
    /// Pre-formatted status text (with ANSI). May be empty.
    pub status: String,
}

/// Outcome dispatched on the channel handed to
/// [`OAuthSelectorComponent::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthOutcome {
    /// User confirmed the highlighted provider.
    Selected(String),
    /// User cancelled (Esc).
    Cancelled,
}

/// Provider picker for `/login` and `/logout`.
pub struct OAuthSelectorComponent {
    mode: AuthSelectorMode,
    all_providers: Vec<AuthSelectorProvider>,
    filtered_indices: Vec<usize>,
    selected_index: usize,
    search_input: InputComponent,
    tx: mpsc::UnboundedSender<OAuthOutcome>,
    max_visible: usize,
}

impl OAuthSelectorComponent {
    pub fn new(
        mode: AuthSelectorMode,
        providers: Vec<AuthSelectorProvider>,
        tx: mpsc::UnboundedSender<OAuthOutcome>,
    ) -> Self {
        let mut me = Self {
            mode,
            filtered_indices: (0..providers.len()).collect(),
            all_providers: providers,
            selected_index: 0,
            search_input: InputComponent::new(),
            tx,
            max_visible: 8,
        };
        me.refilter();
        me
    }

    fn refilter(&mut self) {
        let query = self.search_input.text();
        if query.is_empty() {
            self.filtered_indices = (0..self.all_providers.len()).collect();
        } else {
            let haystacks: Vec<String> = self
                .all_providers
                .iter()
                .map(|p| format!("{} {}", p.name, p.id))
                .collect();
            let refs: Vec<&str> = haystacks.iter().map(String::as_str).collect();
            let mut matches: Vec<(usize, FuzzyMatch)> = fuzzy_filter(query, &refs);
            // Already sorted by score (highest first) per fuzzy_filter contract.
            matches.sort_by(|a, b| b.1.score.cmp(&a.1.score));
            self.filtered_indices = matches.into_iter().map(|(idx, _)| idx).collect();
        }
        if self.filtered_indices.is_empty() {
            self.selected_index = 0;
        } else if self.selected_index >= self.filtered_indices.len() {
            self.selected_index = self.filtered_indices.len() - 1;
        }
    }

    fn confirm(&self) {
        if let Some(&idx) = self.filtered_indices.get(self.selected_index) {
            let _ = self
                .tx
                .send(OAuthOutcome::Selected(self.all_providers[idx].id.clone()));
        }
    }

    fn cancel(&self) {
        let _ = self.tx.send(OAuthOutcome::Cancelled);
    }

    /// Build the "title + search + list" body, then wrap it in dynamic borders.
    fn render_body(&self, width: u16) -> Vec<String> {
        let mut container = Container::new();
        container.add_child(Box::new(DynamicBorderComponent::new()));
        container.add_child(Box::new(SpacerComponent::new(1)));

        let title = match self.mode {
            AuthSelectorMode::Login => "Select provider to configure:",
            AuthSelectorMode::Logout => "Select provider to logout:",
        };
        container.add_child(Box::new(TextComponent::new(format!(
            "{ACCENT}{BOLD}{title}{RESET}"
        ))));
        container.add_child(Box::new(SpacerComponent::new(1)));

        // Render the search input in-place. We can't move the embedded input
        // into the container here without losing its mutable state across
        // renders, so we render its lines directly instead.
        let search_lines = self.search_input.render(width);
        for line in &search_lines {
            container.add_child(Box::new(TextComponent::new(line.clone())));
        }
        container.add_child(Box::new(SpacerComponent::new(1)));

        for line in self.list_lines(width as usize) {
            container.add_child(Box::new(TextComponent::new(line)));
        }

        container.add_child(Box::new(SpacerComponent::new(1)));
        container.add_child(Box::new(DynamicBorderComponent::new()));
        container.render(width)
    }

    fn list_lines(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let count = self.filtered_indices.len();

        if count == 0 {
            let message = if self.all_providers.is_empty() {
                match self.mode {
                    AuthSelectorMode::Login => "No providers available",
                    AuthSelectorMode::Logout => "No providers logged in. Use /login first.",
                }
            } else {
                "No matching providers"
            };
            lines.push(format!(
                "{MUTED}  {}{RESET}",
                truncate_to_width(message, width.saturating_sub(2))
            ));
            return lines;
        }

        let half = self.max_visible / 2;
        let start = self
            .selected_index
            .saturating_sub(half)
            .min(count.saturating_sub(self.max_visible));
        let end = (start + self.max_visible).min(count);

        for i in start..end {
            let provider = &self.all_providers[self.filtered_indices[i]];
            let is_selected = i == self.selected_index;
            let line = if is_selected {
                format!(
                    "{ACCENT}→ {RESET}{ACCENT}{}{RESET}{}",
                    provider.name, provider.status
                )
            } else {
                format!("  {}{}", provider.name, provider.status)
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

        lines
    }

    fn dispatch_key(&mut self, key: &Key) -> HandleResult {
        if key.is_release {
            return HandleResult::Ignored;
        }

        match &key.name {
            KeyName::Up => {
                if !self.filtered_indices.is_empty() {
                    self.selected_index = self.selected_index.saturating_sub(1);
                }
                HandleResult::Handled
            }
            KeyName::Down => {
                let len = self.filtered_indices.len();
                if len > 0 && self.selected_index + 1 < len {
                    self.selected_index += 1;
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
                // Forward to the search input and refilter.
                let prev_text = self.search_input.text().to_string();
                let _ = self
                    .search_input
                    .handle_input(&InputEvent::Key(key.clone()));
                if self.search_input.text() != prev_text {
                    self.refilter();
                }
                HandleResult::Handled
            }
        }
    }
}

impl Component for OAuthSelectorComponent {
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

    fn drain(rx: &mut mpsc::UnboundedReceiver<OAuthOutcome>) -> Vec<OAuthOutcome> {
        let mut out = Vec::new();
        while let Ok(o) = rx.try_recv() {
            out.push(o);
        }
        out
    }

    fn providers() -> Vec<AuthSelectorProvider> {
        vec![
            AuthSelectorProvider {
                id: "anthropic".into(),
                name: "Anthropic".into(),
                status: " ✓ configured".into(),
            },
            AuthSelectorProvider {
                id: "openai".into(),
                name: "OpenAI".into(),
                status: " • unconfigured".into(),
            },
            AuthSelectorProvider {
                id: "google".into(),
                name: "Google".into(),
                status: "".into(),
            },
        ]
    }

    #[test]
    fn renders_title_and_each_provider() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let selector = OAuthSelectorComponent::new(AuthSelectorMode::Login, providers(), tx);
        let body = selector.render(60).join("\n");
        assert!(body.contains("Select provider to configure"));
        assert!(body.contains("Anthropic"));
        assert!(body.contains("OpenAI"));
        assert!(body.contains("Google"));
    }

    #[test]
    fn logout_mode_uses_logout_title() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let selector = OAuthSelectorComponent::new(AuthSelectorMode::Logout, providers(), tx);
        let body = selector.render(60).join("\n");
        assert!(body.contains("Select provider to logout"));
    }

    #[test]
    fn enter_emits_first_provider() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = OAuthSelectorComponent::new(AuthSelectorMode::Login, providers(), tx);
        selector.handle_input(&InputEvent::Raw("\r".into()));
        assert_eq!(
            drain(&mut rx),
            vec![OAuthOutcome::Selected("anthropic".into())]
        );
    }

    #[test]
    fn down_then_enter_selects_second_provider() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = OAuthSelectorComponent::new(AuthSelectorMode::Login, providers(), tx);
        selector.handle_input(&InputEvent::Raw("\x1b[B".into())); // Down
        selector.handle_input(&InputEvent::Raw("\r".into()));
        assert_eq!(
            drain(&mut rx),
            vec![OAuthOutcome::Selected("openai".into())]
        );
    }

    #[test]
    fn escape_emits_cancel() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut selector = OAuthSelectorComponent::new(AuthSelectorMode::Login, providers(), tx);
        selector.handle_input(&InputEvent::Raw("\x1b".into()));
        assert_eq!(drain(&mut rx), vec![OAuthOutcome::Cancelled]);
    }

    #[test]
    fn typing_filters_provider_list() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut selector = OAuthSelectorComponent::new(AuthSelectorMode::Login, providers(), tx);
        // "anth" should match "Anthropic" only (id `anthropic` and name
        // `Anthropic` both contain it; neither `OpenAI`/`openai` nor
        // `Google`/`google` does).
        selector.handle_input(&InputEvent::Raw("a".into()));
        selector.handle_input(&InputEvent::Raw("n".into()));
        selector.handle_input(&InputEvent::Raw("t".into()));
        selector.handle_input(&InputEvent::Raw("h".into()));
        let body = selector.render(60).join("\n");
        assert!(body.contains("Anthropic"));
        assert!(!body.contains("OpenAI"));
        assert!(!body.contains("Google"));
    }

    #[test]
    fn empty_providers_renders_login_hint() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let selector = OAuthSelectorComponent::new(AuthSelectorMode::Login, vec![], tx);
        let body = selector.render(60).join("\n");
        assert!(body.contains("No providers available"));
    }

    #[test]
    fn empty_providers_logout_hints_use_login_first() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let selector = OAuthSelectorComponent::new(AuthSelectorMode::Logout, vec![], tx);
        let body = selector.render(60).join("\n");
        assert!(body.contains("/login first"));
    }
}
