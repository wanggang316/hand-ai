//! Interactive TUI mode for the coding agent.
//!
//! Components live in `components/` and the theme system in `theme/`.
//! This module wires them together via [`InteractiveMode`], covering
//! the happy path: chat scrollback, editor input, agent dispatch, a
//! small slash-command table, and a model-selector overlay. Features
//! still in flight are marked with `// TODO` notes.
//!
//! # Theming
//!
//! Interactive components are styled through semantic color slots
//! (`user_message_bg`, `custom_message_label`, etc.) provided by
//! [`theme`]. The driver currently uses the components' built-in
//! defaults rather than reading the theme directly — the
//! theme-to-component wiring is a follow-up task.

pub mod components;
pub mod driver;
pub mod event_dispatch;
pub mod slash_commands;
pub mod syntax_highlight;
pub mod theme;

pub use driver::{InteractiveError, InteractiveMode};
pub use slash_commands::{
    ExportFormat, ParsedSlashCommand, SlashCommandAction, SlashCommandContext, SlashCommandResult,
    SlashCommandTable,
};

use std::path::{Path, PathBuf};

use crate::SessionManager;
use crate::cli::Args;
use crate::core::agent_session::{AgentSession, AgentSessionConfig};
use crate::modes::session_setup::SessionSetup;

/// High-level entry point for the interactive TUI mode. Mirrors
/// `run_interactive` in `main.rs` but launches the [`InteractiveMode`] driver
/// instead of the line-based REPL.
///
/// On a non-tty stdin (e.g. piped input), this falls through to the legacy
/// line REPL because [`Tui`](hand_tui::Tui) needs a real terminal. The caller
/// is expected to detect the tty itself via [`std::io::IsTerminal`] and dispatch
/// accordingly.
pub async fn run_interactive(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let setup = SessionSetup::resolve(&args)?;
    let cwd = setup.cwd.clone();

    // A bare `--resume` (no value following) lands as `Some("")` from
    // clap; promote it to `--continue` semantics so the user resumes
    // the most-recent session in cwd instead of getting a confusing
    // 'Session "" not found' error.
    let bare_resume = matches!(args.resume.as_deref(), Some(""));
    let continue_like = args.continue_session || bare_resume;
    let resume_session = if continue_like {
        None
    } else {
        args.resume.clone()
    };

    let base_config = setup.to_config(resume_session);
    let agent_tools = setup.agent_tools;

    let session = build_session(&args, continue_like, base_config, agent_tools, &cwd)?;

    InteractiveMode::new(session, cwd).run().await?;
    Ok(())
}

/// Build the [`AgentSession`] honouring `--continue` / `--fork` / `--resume`
/// in the same way as the legacy `main.rs` flow.
fn build_session(
    args: &Args,
    continue_like: bool,
    base_config: AgentSessionConfig,
    agent_tools: Vec<hand_agent::types::AgentTool>,
    cwd: &Path,
) -> Result<AgentSession, Box<dyn std::error::Error>> {
    let session = if continue_like {
        // Discovery only header-scans candidates; the resolved path is
        // handed to AgentSession::new so the session body is read
        // exactly once, by the open inside it.
        match SessionManager::most_recent_session_path(cwd, base_config.session_dir.as_deref()) {
            Some(path) => {
                let config = AgentSessionConfig {
                    resume_session: Some(path.to_string_lossy().into_owned()),
                    ..base_config.clone()
                };
                AgentSession::new(config, agent_tools)?
            }
            None => {
                eprintln!("No previous session found. Starting a new session.");
                AgentSession::new(base_config, agent_tools)?
            }
        }
    } else if let Some(ref fork_source) = args.fork {
        let fork_path =
            resolve_session_path_in(base_config.session_dir.as_deref(), cwd, fork_source);
        match SessionManager::fork_from_in(&fork_path, cwd, base_config.session_dir.as_deref()) {
            Ok(sm) => {
                let config = AgentSessionConfig {
                    resume_session: Some(sm.id().to_string()),
                    ..base_config.clone()
                };
                drop(sm);
                AgentSession::new(config, agent_tools)?
            }
            Err(e) => {
                eprintln!("Failed to fork session: {}. Starting new session.", e);
                AgentSession::new(base_config, agent_tools)?
            }
        }
    } else {
        AgentSession::new(base_config, agent_tools)?
    };
    Ok(session)
}

/// Resolve a `--fork <source>` argument to an on-disk path. Probes
/// `--session-dir <X>` (when set) before the home-based default so
/// `--fork <id> --session-dir <X>` matches the plumbing
/// `--continue` / `--resume` already have (#77).
fn resolve_session_path_in(session_dir: Option<&Path>, cwd: &Path, source: &str) -> PathBuf {
    SessionManager::resolve_session_source_in(session_dir, None, cwd, source)
}
