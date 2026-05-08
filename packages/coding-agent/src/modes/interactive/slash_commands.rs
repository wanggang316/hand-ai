//! Slash-command parsing and dispatch table for the interactive driver.
//!
//! pi-mono's `interactive-mode.ts` registers ~30 slash commands. This module
//! ports the framework + the four commands required by the parity brief
//! (`/quit`, `/exit`, `/help`, `/model`); everything else is left for follow-up
//! batches and is documented at the call site with `// TODO(parity)` markers.
//!
//! Parsing rules mirror the upstream behaviour:
//! * a leading `/` is required;
//! * the first whitespace separates the command name from the (single) argument
//!   string, which is passed through verbatim (no further tokenisation);
//! * unknown commands return [`SlashCommandResult::Unknown`] so the driver can
//!   fall back to extension-contributed commands.

use std::fmt;

/// Parsed slash-command form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSlashCommand {
    /// Bare command name with the leading `/` stripped (e.g. `"model"`).
    pub name: String,
    /// Raw argument string with no further tokenisation. Empty when the user
    /// typed only `/foo` with no argument.
    pub args: String,
}

impl ParsedSlashCommand {
    /// Try to parse `input` as a slash command. Returns `None` when the input
    /// does not start with `/` or is just `/` with no name.
    pub fn parse(input: &str) -> Option<Self> {
        let stripped = input.strip_prefix('/')?;
        let trimmed = stripped.trim_start();
        if trimmed.is_empty() {
            return None;
        }
        let mut iter = trimmed.splitn(2, char::is_whitespace);
        let name = iter.next().unwrap_or("").to_string();
        let args = iter.next().unwrap_or("").trim().to_string();
        if name.is_empty() {
            return None;
        }
        Some(Self { name, args })
    }
}

/// Outcome of dispatching a slash command. Captures the side-effect intent so
/// the driver can act on it (open an overlay, exit the session, ...).
///
/// Overlay-opening variants (`Open*`) currently fall through to the
/// driver's print-only stub because mounting overlays from the background
/// agent task requires shared `Tui` access we don't yet have. The same
/// blocker applies to `OpenModelSelector` — see the existing TODO(parity)
/// note in the driver.
#[derive(Debug)]
pub enum SlashCommandAction {
    /// Show a transient text message in the chat scrollback.
    ShowText(String),
    /// Open the model-selector overlay.
    OpenModelSelector,
    /// Open the thinking-level selector overlay. Optional inline level (e.g.
    /// `/thinking high`) sets the level directly without opening the overlay.
    OpenThinkingSelector { inline_level: Option<String> },
    /// Open the settings selector overlay.
    OpenSettingsSelector,
    /// Open the login dialog overlay.
    OpenLoginDialog,
    /// Open the session-resume picker (most-recent fallback).
    OpenResumePicker,
    /// Clear the chat scrollback (visual only — session history is kept).
    ClearChat,
    /// Trigger compaction on the active session and inject a summary message.
    Compact,
    /// Start a fresh session via `SessionManager::create()`.
    NewSession,
    /// Copy the most recent assistant message to the system clipboard.
    CopyLastAssistant,
    /// Clear stored auth credentials.
    Logout,
    /// Run diagnostics and dump the report into the chat.
    ShowDiagnostics,
    /// Quit the interactive session.
    Quit,
    /// No-op acknowledgement (the handler did its work without producing any
    /// surfaceable output).
    Noop,
}

/// Result of [`SlashCommandTable::dispatch`].
#[derive(Debug)]
pub enum SlashCommandResult {
    /// The command was recognised; the driver should execute the action.
    Handled(SlashCommandAction),
    /// The command was not recognised. The driver may fall back to
    /// extension-contributed commands or show an "unknown command" hint.
    Unknown,
}

/// Minimum context a slash-command handler needs to render a response without
/// depending on the full driver / session.
///
/// More fields will be added as additional commands are ported (see the
/// `TODO(parity)` markers in the dispatch table).
#[derive(Debug, Clone)]
pub struct SlashCommandContext {
    /// Active model id (e.g. `"claude-sonnet-4-5"`).
    pub model_id: String,
    /// Active provider label (e.g. `"anthropic"`).
    pub provider: String,
}

impl fmt::Display for SlashCommandAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SlashCommandAction::ShowText(s) => f.write_str(s),
            SlashCommandAction::OpenModelSelector => f.write_str("[open model selector]"),
            SlashCommandAction::OpenThinkingSelector { inline_level } => match inline_level {
                Some(l) => write!(f, "[set thinking level: {l}]"),
                None => f.write_str("[open thinking selector]"),
            },
            SlashCommandAction::OpenSettingsSelector => f.write_str("[open settings selector]"),
            SlashCommandAction::OpenLoginDialog => f.write_str("[open login dialog]"),
            SlashCommandAction::OpenResumePicker => f.write_str("[open resume picker]"),
            SlashCommandAction::ClearChat => f.write_str("[clear chat]"),
            SlashCommandAction::Compact => f.write_str("[compact]"),
            SlashCommandAction::NewSession => f.write_str("[new session]"),
            SlashCommandAction::CopyLastAssistant => f.write_str("[copy last assistant]"),
            SlashCommandAction::Logout => f.write_str("[logout]"),
            SlashCommandAction::ShowDiagnostics => f.write_str("[show diagnostics]"),
            SlashCommandAction::Quit => f.write_str("[quit]"),
            SlashCommandAction::Noop => Ok(()),
        }
    }
}

/// Built-in slash-command table for the interactive TUI driver.
///
/// The table is intentionally small in this batch — see the module docstring
/// and the `TODO(parity)` markers below for the queue of follow-up commands.
pub struct SlashCommandTable;

impl SlashCommandTable {
    /// Dispatch a parsed slash command.
    pub fn dispatch(cmd: &ParsedSlashCommand, ctx: &SlashCommandContext) -> SlashCommandResult {
        match cmd.name.as_str() {
            "quit" | "exit" | "q" => SlashCommandResult::Handled(SlashCommandAction::Quit),

            "help" | "h" => SlashCommandResult::Handled(SlashCommandAction::ShowText(
                Self::help_text().to_string(),
            )),

            "model" => SlashCommandResult::Handled(if cmd.args.is_empty() {
                SlashCommandAction::OpenModelSelector
            } else {
                // Inline form `/model <pattern>` is left to a follow-up batch
                // alongside the rest of the command surface; for the skeleton
                // we redirect to the picker.
                //
                // TODO(parity): honour `/model <pattern>` directly without
                // opening the overlay.
                SlashCommandAction::OpenModelSelector
            }),

            // Show keybinding hints inline. Mirrors the simple branch in the
            // legacy line REPL until the dedicated overlay component is wired.
            "hotkeys" | "keybindings" => SlashCommandResult::Handled(SlashCommandAction::ShowText(
                Self::hotkeys_text().to_string(),
            )),

            // Show the active model + provider. Stop short of the rich
            // "session" panel — that needs `SessionManager` access (TODO).
            "session" => SlashCommandResult::Handled(SlashCommandAction::ShowText(format!(
                "Model: {}\nProvider: {}",
                ctx.model_id, ctx.provider
            ))),

            "clear" => SlashCommandResult::Handled(SlashCommandAction::ClearChat),

            "compact" => SlashCommandResult::Handled(SlashCommandAction::Compact),

            "new" => SlashCommandResult::Handled(SlashCommandAction::NewSession),

            "resume" => SlashCommandResult::Handled(SlashCommandAction::OpenResumePicker),

            "copy" => SlashCommandResult::Handled(SlashCommandAction::CopyLastAssistant),

            "thinking" => SlashCommandResult::Handled(SlashCommandAction::OpenThinkingSelector {
                inline_level: if cmd.args.is_empty() {
                    None
                } else {
                    Some(cmd.args.clone())
                },
            }),

            "settings" => SlashCommandResult::Handled(SlashCommandAction::OpenSettingsSelector),

            "login" => SlashCommandResult::Handled(SlashCommandAction::OpenLoginDialog),

            "logout" => SlashCommandResult::Handled(SlashCommandAction::Logout),

            "diagnostics" => SlashCommandResult::Handled(SlashCommandAction::ShowDiagnostics),

            // TODO(parity): /export, /name, /import, /fork, /clone, /theme,
            // /skills, /extensions, /changelog, ... — see pi-mono's
            // interactive-mode.ts for the full list.
            _ => SlashCommandResult::Unknown,
        }
    }

    fn help_text() -> &'static str {
        "\
Commands:
  /quit, /exit, /q     Exit the session
  /help, /h            Show this help
  /model               Open model selector
  /session             Show active model
  /hotkeys             Show keyboard shortcuts
  /clear               Clear chat scrollback (history kept)
  /compact             Compact context now
  /new                 Start a fresh session
  /resume              Resume the most recent session
  /copy                Copy last assistant message to clipboard
  /thinking [level]    Open thinking selector / set inline level
  /settings            Open settings selector
  /login               Open login dialog
  /logout              Clear stored auth credentials
  /diagnostics         Show diagnostics report

(Other commands are still being ported — type the legacy CLI command via
 `--print` for full slash-command coverage in the meantime.)"
    }

    fn hotkeys_text() -> &'static str {
        "\
Keyboard shortcuts:
  Enter      Send message
  Ctrl+C     Cancel current operation / clear input
  Ctrl+D     Exit the session
  Esc        Close overlay (model selector etc.)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> SlashCommandContext {
        SlashCommandContext {
            model_id: "claude-sonnet-4".to_string(),
            provider: "anthropic".to_string(),
        }
    }

    #[test]
    fn parses_bare_command() {
        let parsed = ParsedSlashCommand::parse("/help").expect("valid");
        assert_eq!(parsed.name, "help");
        assert_eq!(parsed.args, "");
    }

    #[test]
    fn parses_command_with_args() {
        let parsed = ParsedSlashCommand::parse("/model sonnet:high").expect("valid");
        assert_eq!(parsed.name, "model");
        assert_eq!(parsed.args, "sonnet:high");
    }

    #[test]
    fn rejects_non_slash_input() {
        assert!(ParsedSlashCommand::parse("hello").is_none());
        assert!(ParsedSlashCommand::parse("").is_none());
        assert!(ParsedSlashCommand::parse("/").is_none());
        assert!(ParsedSlashCommand::parse("/   ").is_none());
    }

    #[test]
    fn dispatches_quit_aliases() {
        for input in ["/quit", "/exit", "/q"] {
            let parsed = ParsedSlashCommand::parse(input).unwrap();
            match SlashCommandTable::dispatch(&parsed, &ctx()) {
                SlashCommandResult::Handled(SlashCommandAction::Quit) => {}
                other => panic!("expected Quit, got {:?}", other),
            }
        }
    }

    #[test]
    fn dispatches_help_returns_text() {
        let parsed = ParsedSlashCommand::parse("/help").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::ShowText(s)) => {
                assert!(s.contains("/quit"));
                assert!(s.contains("/help"));
            }
            other => panic!("expected ShowText, got {:?}", other),
        }
    }

    #[test]
    fn dispatches_bare_model_opens_selector() {
        let parsed = ParsedSlashCommand::parse("/model").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::OpenModelSelector) => {}
            other => panic!("expected OpenModelSelector, got {:?}", other),
        }
    }

    #[test]
    fn dispatches_session_shows_model() {
        let parsed = ParsedSlashCommand::parse("/session").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::ShowText(s)) => {
                assert!(s.contains("claude-sonnet-4"));
                assert!(s.contains("anthropic"));
            }
            other => panic!("expected ShowText, got {:?}", other),
        }
    }

    #[test]
    fn dispatches_clear_compact_new() {
        for (input, expected) in [
            ("/clear", "ClearChat"),
            ("/compact", "Compact"),
            ("/new", "NewSession"),
        ] {
            let parsed = ParsedSlashCommand::parse(input).unwrap();
            match SlashCommandTable::dispatch(&parsed, &ctx()) {
                SlashCommandResult::Handled(action) => {
                    let actual = format!("{:?}", action);
                    assert!(
                        actual.contains(expected),
                        "{input} expected variant {expected}, got {actual}"
                    );
                }
                other => panic!("expected Handled, got {:?}", other),
            }
        }
    }

    #[test]
    fn dispatches_thinking_with_inline_level() {
        let parsed = ParsedSlashCommand::parse("/thinking medium").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::OpenThinkingSelector {
                inline_level,
            }) => {
                assert_eq!(inline_level.as_deref(), Some("medium"));
            }
            other => panic!("expected OpenThinkingSelector, got {:?}", other),
        }
    }

    #[test]
    fn dispatches_thinking_without_args_opens_selector() {
        let parsed = ParsedSlashCommand::parse("/thinking").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::OpenThinkingSelector {
                inline_level,
            }) => {
                assert!(inline_level.is_none());
            }
            other => panic!("expected OpenThinkingSelector, got {:?}", other),
        }
    }

    #[test]
    fn dispatches_login_logout_diagnostics_settings_resume_copy() {
        for (input, expected) in [
            ("/login", "OpenLoginDialog"),
            ("/logout", "Logout"),
            ("/diagnostics", "ShowDiagnostics"),
            ("/settings", "OpenSettingsSelector"),
            ("/resume", "OpenResumePicker"),
            ("/copy", "CopyLastAssistant"),
        ] {
            let parsed = ParsedSlashCommand::parse(input).unwrap();
            match SlashCommandTable::dispatch(&parsed, &ctx()) {
                SlashCommandResult::Handled(action) => {
                    let actual = format!("{:?}", action);
                    assert!(
                        actual.contains(expected),
                        "{input} expected {expected}, got {actual}"
                    );
                }
                other => panic!("expected Handled, got {:?}", other),
            }
        }
    }

    #[test]
    fn unknown_command_yields_unknown() {
        let parsed = ParsedSlashCommand::parse("/totally-unknown-thing").unwrap();
        assert!(matches!(
            SlashCommandTable::dispatch(&parsed, &ctx()),
            SlashCommandResult::Unknown
        ));
    }
}
