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
#[derive(Debug)]
pub enum SlashCommandAction {
    /// Show a transient text message in the chat scrollback.
    ShowText(String),
    /// Open the model-selector overlay.
    OpenModelSelector,
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

            // TODO(parity): /compact, /resume, /export, /copy, /name, /thinking,
            // /settings, /new, /import, /fork, /clone, /login, /logout, /theme,
            // /skills, /extensions, /diagnostics, /changelog, ... — see
            // pi-mono's interactive-mode.ts for the full list.
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
    fn unknown_command_yields_unknown() {
        let parsed = ParsedSlashCommand::parse("/totally-unknown-thing").unwrap();
        assert!(matches!(
            SlashCommandTable::dispatch(&parsed, &ctx()),
            SlashCommandResult::Unknown
        ));
    }
}
