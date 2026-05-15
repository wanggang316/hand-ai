//! Login dialog used by `/login` to drive an OAuth-style authentication flow.
//!
//! [`LoginDialogComponent`] owns an [`InputComponent`] and a typed
//! `Stage` enum capturing what to show. The OAuth provider transitions
//! stages by calling the `show_*` mutator methods (`show_auth`,
//! `show_manual_input`, `show_prompt`, `show_info`, `show_waiting`,
//! `show_progress`) — each one clears or appends content and requests a
//! re-render.
//!
//! Events ([`LoginDialogEvent::Submit`] / `Cancel`) flow through an
//! [`std::sync::mpsc::Sender`] supplied at construction. The input's
//! `on_submit` / `on_escape` callbacks forward to that channel so the
//! manual-input and prompt stages dispatch user-supplied strings
//! without a host-owned future.
//!
//! Provider lookup: until a shared OAuth provider registry lands, the
//! constructor accepts an explicit `providers` slice (the same pattern
//! `oauth_selector` uses). Callers without a provider list pass an
//! empty slice and the dialog falls back to the raw provider id.

use std::sync::mpsc::Sender;

use hand_tui::components::input::InputComponent;
use hand_tui::tui::{Component, Focusable, HandleResult, InputEvent};
use hand_tui::utils::visible_width;

use super::dynamic_border::DynamicBorderComponent;
use super::keybinding_hints::key_hint_for;

/// View-model for an OAuth provider — mirrors the subset of `getOAuthProviders()`
/// that the dialog actually consumes.
#[derive(Debug, Clone)]
pub struct LoginProvider {
    /// Stable provider id (e.g. `"github-copilot"`).
    pub id: String,
    /// Display name (e.g. `"GitHub Copilot"`).
    pub name: String,
}

/// Events surfaced by [`LoginDialogComponent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginDialogEvent {
    /// User submitted the manual-input or prompt input field.
    Submit(String),
    /// User pressed `tui.select.cancel` or the OAuth flow was aborted.
    Cancel,
}

/// What the dialog is currently showing. Each variant is driven by one of
/// the `show_*` methods.
#[derive(Debug, Clone)]
enum Stage {
    /// Initial empty state, just the title.
    Idle,
    /// `show_auth`: present a clickable auth URL with optional warning.
    Auth {
        url: String,
        instructions: Option<String>,
    },
    /// `show_manual_input`: prompt for a code/url after the auth panel.
    ManualInput {
        url: Option<String>,
        instructions: Option<String>,
        prompt: String,
    },
    /// `show_prompt`: ask for a value (e.g. enterprise hostname). Preserves
    /// any preceding auth panel.
    Prompt {
        url: Option<String>,
        instructions: Option<String>,
        message: String,
        placeholder: Option<String>,
    },
    /// `show_info`: read-only multi-line message.
    Info { lines: Vec<String> },
    /// `show_waiting`: text + cancel hint while polling.
    Waiting {
        url: Option<String>,
        instructions: Option<String>,
        message: String,
    },
    /// `show_progress`: append-only progress lines.
    Progress {
        url: Option<String>,
        instructions: Option<String>,
        messages: Vec<String>,
    },
}

/// Login dialog. Renders a bordered panel with a title and a stage-specific
/// content area. The dialog owns an [`InputComponent`] and routes its
/// `Submit` / `Escape` events into the supplied channel.
pub struct LoginDialogComponent {
    title: String,
    border: DynamicBorderComponent,
    input: InputComponent,
    stage: Stage,
    events: Sender<LoginDialogEvent>,
    focused: bool,
}

impl LoginDialogComponent {
    /// Construct a dialog for `provider_id`. `providers` is consulted for a
    /// display name; if the id is missing the raw id is used. `name_override`
    /// and `title_override` mirror the optional ctor args in the TS source.
    pub fn new(
        provider_id: &str,
        providers: &[LoginProvider],
        name_override: Option<&str>,
        title_override: Option<&str>,
        events: Sender<LoginDialogEvent>,
    ) -> Self {
        let provider_name = name_override
            .map(str::to_string)
            .or_else(|| {
                providers
                    .iter()
                    .find(|p| p.id == provider_id)
                    .map(|p| p.name.clone())
            })
            .unwrap_or_else(|| provider_id.to_string());

        let title = title_override
            .map(str::to_string)
            .unwrap_or_else(|| format!("Login to {provider_name}"));

        let mut input = InputComponent::new();
        // Forward Enter / Escape from the inner input via the events channel.
        let submit_tx = events.clone();
        input.set_on_submit(Box::new(move |text: &str| {
            let _ = submit_tx.send(LoginDialogEvent::Submit(text.to_string()));
        }));
        let escape_tx = events.clone();
        input.set_on_escape(Box::new(move || {
            let _ = escape_tx.send(LoginDialogEvent::Cancel);
        }));

        Self {
            title,
            border: DynamicBorderComponent::new(),
            input,
            stage: Stage::Idle,
            events,
            focused: false,
        }
    }

    /// Mirror `showAuth`: replace the content with an auth URL + optional
    /// warning.
    pub fn show_auth(&mut self, url: impl Into<String>, instructions: Option<String>) {
        self.stage = Stage::Auth {
            url: url.into(),
            instructions,
        };
        self.input.clear();
    }

    /// Mirror `showManualInput`: append a prompt + input, preserving any auth
    /// panel previously shown.
    pub fn show_manual_input(&mut self, prompt: impl Into<String>) {
        let (url, instructions) = self.preserved_auth_panel();
        self.stage = Stage::ManualInput {
            url,
            instructions,
            prompt: prompt.into(),
        };
        self.input.clear();
    }

    /// Mirror `showPrompt`: ask for a value with an optional placeholder.
    pub fn show_prompt(&mut self, message: impl Into<String>, placeholder: Option<String>) {
        let (url, instructions) = self.preserved_auth_panel();
        self.stage = Stage::Prompt {
            url,
            instructions,
            message: message.into(),
            placeholder,
        };
        self.input.clear();
    }

    /// Mirror `showInfo`: read-only multi-line message.
    pub fn show_info(&mut self, lines: Vec<String>) {
        self.stage = Stage::Info { lines };
        self.input.clear();
    }

    /// Mirror `showWaiting`: dim status + cancel hint while polling.
    pub fn show_waiting(&mut self, message: impl Into<String>) {
        let (url, instructions) = self.preserved_auth_panel();
        self.stage = Stage::Waiting {
            url,
            instructions,
            message: message.into(),
        };
    }

    /// Mirror `showProgress`: append a progress line. If the dialog is not
    /// already in `Progress` mode, transition into it (preserving any auth
    /// panel) and seed it with this message.
    pub fn show_progress(&mut self, message: impl Into<String>) {
        let msg = message.into();
        if let Stage::Progress { messages, .. } = &mut self.stage {
            messages.push(msg);
            return;
        }
        let (url, instructions) = self.preserved_auth_panel();
        self.stage = Stage::Progress {
            url,
            instructions,
            messages: vec![msg],
        };
    }

    /// Return the dialog title. Useful for tests.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Send a `Cancel` event without changing the stage. Mirrors the TS
    /// `cancel()` private helper exposed via the abort signal.
    pub fn cancel(&self) {
        let _ = self.events.send(LoginDialogEvent::Cancel);
    }

    /// Capture URL / instructions from the current stage so transitions that
    /// "append" (manual input, prompt, waiting, progress) can keep showing
    /// them, matching the TS behaviour.
    fn preserved_auth_panel(&self) -> (Option<String>, Option<String>) {
        match &self.stage {
            Stage::Auth { url, instructions } => (Some(url.clone()), instructions.clone()),
            Stage::ManualInput {
                url, instructions, ..
            }
            | Stage::Prompt {
                url, instructions, ..
            }
            | Stage::Waiting {
                url, instructions, ..
            }
            | Stage::Progress {
                url, instructions, ..
            } => (url.clone(), instructions.clone()),
            _ => (None, None),
        }
    }
}

// Theming caveat: the component expects `accent`, `dim`, `warning`,
// `text` slots. Until the theme system surfaces them we hardcode
// dark-theme defaults.
const ACCENT: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const WARNING: &str = "\x1b[33m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

fn osc8_link(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x07{text}\x1b]8;;\x07")
}

fn click_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd+click to open"
    } else {
        "Ctrl+click to open"
    }
}

fn pad_line(line: &str, width: u16) -> String {
    let target = width as usize;
    let current = visible_width(line);
    if current >= target {
        line.to_string()
    } else {
        format!("{line}{}", " ".repeat(target - current))
    }
}

impl Component for LoginDialogComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let mut out = Vec::new();
        out.extend(self.border.render(width));

        // Title (bold accent).
        out.push(pad_line(
            &format!("{ACCENT}{BOLD}{}{RESET}", self.title),
            width,
        ));

        match &self.stage {
            Stage::Idle => {
                out.push(pad_line("", width));
            }
            Stage::Auth { url, instructions } => {
                render_auth_panel(&mut out, width, url, instructions.as_deref());
            }
            Stage::ManualInput {
                url,
                instructions,
                prompt,
            } => {
                if let Some(u) = url {
                    render_auth_panel(&mut out, width, u, instructions.as_deref());
                }
                out.push(pad_line("", width));
                out.push(pad_line(&format!("{DIM}{prompt}{RESET}"), width));
                out.extend(self.input.render(width));
                out.push(pad_line(
                    &format!("({})", key_hint_for("tui.select.cancel", "to cancel")),
                    width,
                ));
            }
            Stage::Prompt {
                url,
                instructions,
                message,
                placeholder,
            } => {
                if let Some(u) = url {
                    render_auth_panel(&mut out, width, u, instructions.as_deref());
                }
                out.push(pad_line("", width));
                out.push(pad_line(message, width));
                if let Some(ph) = placeholder {
                    out.push(pad_line(&format!("{DIM}e.g., {ph}{RESET}"), width));
                }
                out.extend(self.input.render(width));
                out.push(pad_line(
                    &format!(
                        "({} {})",
                        key_hint_for("tui.select.cancel", "to cancel,"),
                        key_hint_for("tui.select.confirm", "to submit"),
                    ),
                    width,
                ));
            }
            Stage::Info { lines } => {
                out.push(pad_line("", width));
                for line in lines {
                    out.push(pad_line(line, width));
                }
                out.push(pad_line("", width));
                out.push(pad_line(
                    &format!("({})", key_hint_for("tui.select.cancel", "to close")),
                    width,
                ));
            }
            Stage::Waiting {
                url,
                instructions,
                message,
            } => {
                if let Some(u) = url {
                    render_auth_panel(&mut out, width, u, instructions.as_deref());
                }
                out.push(pad_line("", width));
                out.push(pad_line(&format!("{DIM}{message}{RESET}"), width));
                out.push(pad_line(
                    &format!("({})", key_hint_for("tui.select.cancel", "to cancel")),
                    width,
                ));
            }
            Stage::Progress {
                url,
                instructions,
                messages,
            } => {
                if let Some(u) = url {
                    render_auth_panel(&mut out, width, u, instructions.as_deref());
                }
                out.push(pad_line("", width));
                for m in messages {
                    out.push(pad_line(&format!("{DIM}{m}{RESET}"), width));
                }
            }
        }

        out.extend(self.border.render(width));
        out
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        // Stages that have an active input: ManualInput, Prompt.
        if matches!(self.stage, Stage::ManualInput { .. } | Stage::Prompt { .. }) {
            return self.input.handle_input(event);
        }
        HandleResult::Ignored
    }

    fn invalidate(&mut self) {
        self.input.invalidate();
    }
}

fn render_auth_panel(out: &mut Vec<String>, width: u16, url: &str, instructions: Option<&str>) {
    out.push(pad_line("", width));
    out.push(pad_line(
        &format!("{ACCENT}{}{RESET}", osc8_link(url, url)),
        width,
    ));
    out.push(pad_line(
        &format!("{DIM}{}{RESET}", osc8_link(url, click_hint())),
        width,
    ));
    if let Some(text) = instructions {
        out.push(pad_line("", width));
        out.push(pad_line(&format!("{WARNING}{text}{RESET}"), width));
    }
}

impl Focusable for LoginDialogComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.input.set_focused(focused);
    }

    /// The dialog defers cursor positioning to the embedded input. Drivers
    /// using this dialog inside a container should add the dialog's vertical
    /// offset onto the row component before painting the cursor.
    fn cursor_position(&self) -> Option<(u16, u16)> {
        if matches!(self.stage, Stage::ManualInput { .. } | Stage::Prompt { .. }) {
            self.input.cursor_position()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn make_dialog() -> (LoginDialogComponent, mpsc::Receiver<LoginDialogEvent>) {
        let (tx, rx) = mpsc::channel();
        let providers = vec![LoginProvider {
            id: "github-copilot".into(),
            name: "GitHub Copilot".into(),
        }];
        let dialog = LoginDialogComponent::new("github-copilot", &providers, None, None, tx);
        (dialog, rx)
    }

    #[test]
    fn resolves_provider_name_for_title() {
        let (d, _rx) = make_dialog();
        assert_eq!(d.title(), "Login to GitHub Copilot");
    }

    #[test]
    fn falls_back_to_raw_id_when_provider_unknown() {
        let (tx, _rx) = mpsc::channel();
        let d = LoginDialogComponent::new("unknown-provider", &[], None, None, tx);
        assert_eq!(d.title(), "Login to unknown-provider");
    }

    #[test]
    fn name_override_wins_over_provider_lookup() {
        let (tx, _rx) = mpsc::channel();
        let providers = vec![LoginProvider {
            id: "p1".into(),
            name: "Provider One".into(),
        }];
        let d = LoginDialogComponent::new("p1", &providers, Some("Custom Name"), None, tx);
        assert_eq!(d.title(), "Login to Custom Name");
    }

    #[test]
    fn title_override_wins_outright() {
        let (tx, _rx) = mpsc::channel();
        let d = LoginDialogComponent::new("p1", &[], None, Some("Special"), tx);
        assert_eq!(d.title(), "Special");
    }

    #[test]
    fn show_auth_renders_url_and_instructions() {
        let (mut d, _rx) = make_dialog();
        d.show_auth("https://example.test/auth", Some("scan QR".into()));
        let lines = d.render(60);
        let blob = lines.join("\n");
        assert!(blob.contains("https://example.test/auth"));
        assert!(blob.contains("scan QR"));
    }

    #[test]
    fn show_manual_input_preserves_auth_url() {
        let (mut d, _rx) = make_dialog();
        d.show_auth("https://example.test/auth", None);
        d.show_manual_input("Paste code:");
        let blob = d.render(60).join("\n");
        // Auth URL still visible.
        assert!(blob.contains("https://example.test/auth"));
        assert!(blob.contains("Paste code:"));
    }

    #[test]
    fn show_progress_appends_messages() {
        let (mut d, _rx) = make_dialog();
        d.show_progress("step 1");
        d.show_progress("step 2");
        let blob = d.render(60).join("\n");
        assert!(blob.contains("step 1"));
        assert!(blob.contains("step 2"));
    }

    #[test]
    fn show_info_renders_each_line() {
        let (mut d, _rx) = make_dialog();
        d.show_info(vec!["line a".into(), "line b".into()]);
        let blob = d.render(60).join("\n");
        assert!(blob.contains("line a"));
        assert!(blob.contains("line b"));
    }

    #[test]
    fn cancel_sends_event() {
        let (d, rx) = make_dialog();
        d.cancel();
        match rx.try_recv() {
            Ok(LoginDialogEvent::Cancel) => {}
            other => panic!("expected Cancel, got {other:?}"),
        }
    }
}
