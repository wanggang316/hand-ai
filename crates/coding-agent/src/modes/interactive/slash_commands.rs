//! Slash-command parsing and dispatch table for the interactive driver.
//!
//! pi-mono's `interactive-mode.ts` registers ~30 slash commands. This module
//! ports the framework plus the commands required by the parity brief; the
//! remainder are documented at the call site with `// TODO(parity)` markers.
//!
//! Parsing rules mirror the upstream behaviour:
//! * a leading `/` is required;
//! * the first whitespace separates the command name from the (single) argument
//!   string, which is passed through verbatim (no further tokenisation);
//! * unknown commands return [`SlashCommandResult::Unknown`] so the driver can
//!   fall back to extension-contributed commands.

use std::fmt;
use std::path::PathBuf;

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

/// Export-file format inferred from the destination extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// JSON Lines — copy the active session file verbatim.
    Jsonl,
    /// Aggregated JSON document (mirrors `.jsonl` content for now).
    Json,
    /// Standalone HTML rendering (uses [`crate::core::export::export_to_html`]).
    Html,
    /// Markdown rendering — currently routed to a TODO; M6 covers the body.
    Markdown,
}

impl ExportFormat {
    /// Infer the format from a destination path's extension. Returns `None`
    /// when the extension is missing or unrecognised — the dispatcher
    /// surfaces a friendly error for those.
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "jsonl" => Some(Self::Jsonl),
            "json" => Some(Self::Json),
            "html" | "htm" => Some(Self::Html),
            "md" | "markdown" => Some(Self::Markdown),
            _ => None,
        }
    }
}

/// Outcome of dispatching a slash command. Captures the side-effect intent so
/// the driver can act on it (open an overlay, exit the session, ...).
///
/// Overlay-opening variants (`Open*`) currently fall through to the
/// driver's print-only stub when no `OverlayMounter` is wired — see the
/// existing TODO(parity) notes in the driver.
#[derive(Debug)]
pub enum SlashCommandAction {
    /// Show a transient text message in the chat scrollback.
    ShowText(String),
    /// Open the model-selector overlay.
    OpenModelSelector,
    /// Resolve a `/model <pattern>` invocation directly — without opening the
    /// overlay — and apply the matched model. The driver delegates to
    /// [`crate::core::model_resolver::parse_model_pattern_full`].
    ModelByPattern(String),
    /// Open the thinking-level selector overlay. Optional inline level (e.g.
    /// `/thinking high`) sets the level directly without opening the overlay.
    OpenThinkingSelector { inline_level: Option<String> },
    /// Open the settings selector overlay.
    OpenSettingsSelector,
    /// Open the login dialog overlay. `provider` is the provider id to
    /// authenticate against (e.g. `"anthropic"`, `"openai"`); when `None`
    /// the dialog defaults to Anthropic.
    OpenLoginDialog { provider: Option<String> },
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
    /// Copy the last `n` assistant messages (concatenated text) to the
    /// system clipboard. `n == 0` is parsed as "fall back to copying the
    /// last one" so `/copy 0` behaves identically to `/copy`.
    CopyN(usize),
    /// Export the active session to `path`. Format is inferred from the
    /// extension via [`ExportFormat::from_path`] at parse time.
    Export(PathBuf, ExportFormat),
    /// Replace the active session with a JSONL/JSON file at `path`.
    Import(PathBuf),
    /// Fork the session at the given user-message entry id (or the latest
    /// entry when `None`).
    Fork(Option<String>),
    /// Clone the current session (full body, fresh id).
    Clone,
    /// Set the session label.
    Name(String),
    /// Open the theme selector (when arg is empty) or apply `name` directly.
    Theme(Option<String>),
    /// Render the discovered skills inline as a custom message.
    ListSkills,
    /// Render the registered Tier 1 extensions inline.
    ListExtensions,
    /// Render the agent's CHANGELOG.md inline.
    Changelog,
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
            SlashCommandAction::ModelByPattern(p) => write!(f, "[model pattern: {p}]"),
            SlashCommandAction::OpenThinkingSelector { inline_level } => match inline_level {
                Some(l) => write!(f, "[set thinking level: {l}]"),
                None => f.write_str("[open thinking selector]"),
            },
            SlashCommandAction::OpenSettingsSelector => f.write_str("[open settings selector]"),
            SlashCommandAction::OpenLoginDialog { provider } => match provider {
                Some(p) => write!(f, "[open login dialog: {p}]"),
                None => f.write_str("[open login dialog]"),
            },
            SlashCommandAction::OpenResumePicker => f.write_str("[open resume picker]"),
            SlashCommandAction::ClearChat => f.write_str("[clear chat]"),
            SlashCommandAction::Compact => f.write_str("[compact]"),
            SlashCommandAction::NewSession => f.write_str("[new session]"),
            SlashCommandAction::CopyLastAssistant => f.write_str("[copy last assistant]"),
            SlashCommandAction::CopyN(n) => write!(f, "[copy last {n}]"),
            SlashCommandAction::Export(path, fmt) => {
                write!(f, "[export {} as {fmt:?}]", path.display())
            }
            SlashCommandAction::Import(path) => write!(f, "[import {}]", path.display()),
            SlashCommandAction::Fork(entry) => match entry {
                Some(id) => write!(f, "[fork from {id}]"),
                None => f.write_str("[fork from latest]"),
            },
            SlashCommandAction::Clone => f.write_str("[clone session]"),
            SlashCommandAction::Name(n) => write!(f, "[set name: {n}]"),
            SlashCommandAction::Theme(name) => match name {
                Some(n) => write!(f, "[set theme: {n}]"),
                None => f.write_str("[open theme selector]"),
            },
            SlashCommandAction::ListSkills => f.write_str("[list skills]"),
            SlashCommandAction::ListExtensions => f.write_str("[list extensions]"),
            SlashCommandAction::Changelog => f.write_str("[changelog]"),
            SlashCommandAction::Logout => f.write_str("[logout]"),
            SlashCommandAction::ShowDiagnostics => f.write_str("[show diagnostics]"),
            SlashCommandAction::Quit => f.write_str("[quit]"),
            SlashCommandAction::Noop => Ok(()),
        }
    }
}

/// Built-in slash-command table for the interactive TUI driver.
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
                SlashCommandAction::ModelByPattern(cmd.args.clone())
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

            "copy" => SlashCommandResult::Handled(if cmd.args.is_empty() {
                SlashCommandAction::CopyLastAssistant
            } else {
                match cmd.args.parse::<usize>() {
                    Ok(0) | Ok(1) => SlashCommandAction::CopyLastAssistant,
                    Ok(n) => SlashCommandAction::CopyN(n),
                    Err(_) => SlashCommandAction::ShowText(format!(
                        "Usage: /copy [n] — argument {:?} is not a positive integer.",
                        cmd.args
                    )),
                }
            }),

            "export" => SlashCommandResult::Handled(parse_export(&cmd.args)),

            "import" => SlashCommandResult::Handled(if cmd.args.is_empty() {
                SlashCommandAction::ShowText("Usage: /import <path.jsonl>".to_string())
            } else {
                SlashCommandAction::Import(parse_path_argument(&cmd.args))
            }),

            "fork" => SlashCommandResult::Handled(SlashCommandAction::Fork(if cmd.args.is_empty() {
                None
            } else {
                Some(cmd.args.clone())
            })),

            "clone" => SlashCommandResult::Handled(SlashCommandAction::Clone),

            "name" => SlashCommandResult::Handled(if cmd.args.is_empty() {
                SlashCommandAction::ShowText("Usage: /name <new-name>".to_string())
            } else {
                SlashCommandAction::Name(cmd.args.clone())
            }),

            "theme" => SlashCommandResult::Handled(SlashCommandAction::Theme(
                if cmd.args.is_empty() {
                    None
                } else {
                    Some(cmd.args.clone())
                },
            )),

            "skills" => SlashCommandResult::Handled(SlashCommandAction::ListSkills),

            "extensions" => SlashCommandResult::Handled(SlashCommandAction::ListExtensions),

            "changelog" => SlashCommandResult::Handled(SlashCommandAction::Changelog),

            "thinking" => SlashCommandResult::Handled(SlashCommandAction::OpenThinkingSelector {
                inline_level: if cmd.args.is_empty() {
                    None
                } else {
                    Some(cmd.args.clone())
                },
            }),

            "settings" => SlashCommandResult::Handled(SlashCommandAction::OpenSettingsSelector),

            "login" => SlashCommandResult::Handled(SlashCommandAction::OpenLoginDialog {
                provider: cmd
                    .args
                    .split_whitespace()
                    .next()
                    .map(|s| s.to_string()),
            }),

            "logout" => SlashCommandResult::Handled(SlashCommandAction::Logout),

            "diagnostics" => SlashCommandResult::Handled(SlashCommandAction::ShowDiagnostics),

            _ => SlashCommandResult::Unknown,
        }
    }

    fn help_text() -> &'static str {
        "\
Commands:
  /quit, /exit, /q     Exit the session
  /help, /h            Show this help
  /model [pattern]     Open model selector / inline-resolve a pattern
  /session             Show active model
  /hotkeys             Show keyboard shortcuts
  /clear               Clear chat scrollback (history kept)
  /compact             Compact context now
  /new                 Start a fresh session
  /resume              Resume the most recent session
  /copy [n]            Copy last assistant message (or last n) to clipboard
  /export <path>       Export session (jsonl/json/html/md inferred from ext)
  /import <path>       Import a session JSONL file in place
  /fork [<entry-id>]   Fork at the given user-message entry (or latest)
  /clone               Clone the current session under a fresh id
  /name <new-name>     Set the session label
  /theme [name]        Open theme selector / apply a theme inline
  /skills              List discovered skills
  /extensions          List loaded extensions
  /changelog           Show CHANGELOG.md
  /thinking [level]    Open thinking selector / set inline level
  /settings            Open settings selector
  /login               Open login dialog
  /logout              Clear stored auth credentials
  /diagnostics         Show diagnostics report"
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

/// Strip a single layer of matched single/double quotes from `arg` and
/// return the inner string as a [`PathBuf`]. Mirrors the
/// `getPathCommandArgument` helper in pi-mono's `interactive-mode.ts`.
/// The arg is assumed to be already-trimmed of leading whitespace by the
/// slash-command parser.
fn parse_path_argument(arg: &str) -> PathBuf {
    let arg = arg.trim();
    if arg.len() >= 2 {
        let bytes = arg.as_bytes();
        let first = bytes[0];
        let last = bytes[arg.len() - 1];
        if (first == b'"' || first == b'\'') && first == last {
            return PathBuf::from(&arg[1..arg.len() - 1]);
        }
    }
    PathBuf::from(arg)
}

/// Build the [`SlashCommandAction`] for `/export <path>`. Empty argument
/// yields a usage hint; an unknown extension yields a friendly inline
/// error so the driver doesn't have to special-case it.
fn parse_export(arg: &str) -> SlashCommandAction {
    if arg.is_empty() {
        return SlashCommandAction::ShowText(
            "Usage: /export <path.jsonl|.json|.html|.md>".to_string(),
        );
    }
    let path = parse_path_argument(arg);
    match ExportFormat::from_path(&path) {
        Some(fmt) => SlashCommandAction::Export(path, fmt),
        None => SlashCommandAction::ShowText(format!(
            "/export: unsupported extension on {}. Expected .jsonl, .json, .html, or .md.",
            path.display()
        )),
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
    fn dispatches_bare_copy_returns_copy_last_assistant() {
        let parsed = ParsedSlashCommand::parse("/copy").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::CopyLastAssistant) => {}
            other => panic!("expected CopyLastAssistant, got {other:?}"),
        }
    }

    #[test]
    fn dispatches_copy_with_n_returns_copy_n() {
        let parsed = ParsedSlashCommand::parse("/copy 3").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::CopyN(3)) => {}
            other => panic!("expected CopyN(3), got {other:?}"),
        }
    }

    #[test]
    fn copy_argument_one_collapses_to_copy_last_assistant() {
        let parsed = ParsedSlashCommand::parse("/copy 1").unwrap();
        assert!(matches!(
            SlashCommandTable::dispatch(&parsed, &ctx()),
            SlashCommandResult::Handled(SlashCommandAction::CopyLastAssistant)
        ));
    }

    #[test]
    fn copy_with_invalid_argument_returns_usage_hint() {
        let parsed = ParsedSlashCommand::parse("/copy abc").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::ShowText(s)) => {
                assert!(s.contains("/copy"));
            }
            other => panic!("expected ShowText, got {other:?}"),
        }
    }

    #[test]
    fn dispatches_export_infers_format_from_extension() {
        for (input, expected) in [
            ("/export out.jsonl", ExportFormat::Jsonl),
            ("/export out.json", ExportFormat::Json),
            ("/export out.html", ExportFormat::Html),
            ("/export out.md", ExportFormat::Markdown),
        ] {
            let parsed = ParsedSlashCommand::parse(input).unwrap();
            match SlashCommandTable::dispatch(&parsed, &ctx()) {
                SlashCommandResult::Handled(SlashCommandAction::Export(_, fmt)) => {
                    assert_eq!(fmt, expected, "{input}");
                }
                other => panic!("expected Export, got {other:?}"),
            }
        }
    }

    #[test]
    fn dispatches_export_strips_quoted_path() {
        let parsed = ParsedSlashCommand::parse("/export \"my session.jsonl\"").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::Export(path, fmt)) => {
                assert_eq!(path, PathBuf::from("my session.jsonl"));
                assert_eq!(fmt, ExportFormat::Jsonl);
            }
            other => panic!("expected Export, got {other:?}"),
        }
    }

    #[test]
    fn dispatches_export_unknown_extension_yields_usage() {
        let parsed = ParsedSlashCommand::parse("/export /tmp/foo.xyz").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::ShowText(s)) => {
                assert!(s.contains("unsupported extension"));
            }
            other => panic!("expected ShowText, got {other:?}"),
        }
    }

    #[test]
    fn dispatches_export_without_argument_yields_usage() {
        let parsed = ParsedSlashCommand::parse("/export").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::ShowText(s)) => {
                assert!(s.contains("Usage"));
            }
            other => panic!("expected ShowText, got {other:?}"),
        }
    }

    #[test]
    fn dispatches_import_with_path() {
        let parsed = ParsedSlashCommand::parse("/import /tmp/session.jsonl").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::Import(path)) => {
                assert_eq!(path, PathBuf::from("/tmp/session.jsonl"));
            }
            other => panic!("expected Import, got {other:?}"),
        }
    }

    #[test]
    fn dispatches_import_without_argument_yields_usage() {
        let parsed = ParsedSlashCommand::parse("/import").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::ShowText(s)) => {
                assert!(s.contains("/import"));
            }
            other => panic!("expected ShowText, got {other:?}"),
        }
    }

    #[test]
    fn dispatches_fork_with_entry_id() {
        let parsed = ParsedSlashCommand::parse("/fork e_abc").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::Fork(Some(id))) => {
                assert_eq!(id, "e_abc");
            }
            other => panic!("expected Fork(Some), got {other:?}"),
        }
    }

    #[test]
    fn dispatches_bare_fork_yields_none() {
        let parsed = ParsedSlashCommand::parse("/fork").unwrap();
        assert!(matches!(
            SlashCommandTable::dispatch(&parsed, &ctx()),
            SlashCommandResult::Handled(SlashCommandAction::Fork(None))
        ));
    }

    #[test]
    fn dispatches_clone_and_name() {
        let parsed = ParsedSlashCommand::parse("/clone").unwrap();
        assert!(matches!(
            SlashCommandTable::dispatch(&parsed, &ctx()),
            SlashCommandResult::Handled(SlashCommandAction::Clone)
        ));

        let parsed = ParsedSlashCommand::parse("/name my session").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::Name(label)) => {
                assert_eq!(label, "my session");
            }
            other => panic!("expected Name, got {other:?}"),
        }
    }

    #[test]
    fn name_without_argument_yields_usage() {
        let parsed = ParsedSlashCommand::parse("/name").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::ShowText(s)) => {
                assert!(s.contains("/name"));
            }
            other => panic!("expected ShowText, got {other:?}"),
        }
    }

    #[test]
    fn dispatches_theme_with_and_without_arg() {
        let parsed = ParsedSlashCommand::parse("/theme").unwrap();
        assert!(matches!(
            SlashCommandTable::dispatch(&parsed, &ctx()),
            SlashCommandResult::Handled(SlashCommandAction::Theme(None))
        ));

        let parsed = ParsedSlashCommand::parse("/theme dark").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::Theme(Some(name))) => {
                assert_eq!(name, "dark");
            }
            other => panic!("expected Theme(Some), got {other:?}"),
        }
    }

    #[test]
    fn dispatches_skills_extensions_changelog() {
        for (input, expected) in [
            ("/skills", "ListSkills"),
            ("/extensions", "ListExtensions"),
            ("/changelog", "Changelog"),
        ] {
            let parsed = ParsedSlashCommand::parse(input).unwrap();
            match SlashCommandTable::dispatch(&parsed, &ctx()) {
                SlashCommandResult::Handled(action) => {
                    let actual = format!("{action:?}");
                    assert!(
                        actual.contains(expected),
                        "{input} expected {expected}, got {actual}"
                    );
                }
                other => panic!("expected Handled, got {other:?}"),
            }
        }
    }

    #[test]
    fn dispatches_model_with_pattern_returns_model_by_pattern() {
        let parsed = ParsedSlashCommand::parse("/model sonnet:high").unwrap();
        match SlashCommandTable::dispatch(&parsed, &ctx()) {
            SlashCommandResult::Handled(SlashCommandAction::ModelByPattern(p)) => {
                assert_eq!(p, "sonnet:high");
            }
            other => panic!("expected ModelByPattern, got {other:?}"),
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
