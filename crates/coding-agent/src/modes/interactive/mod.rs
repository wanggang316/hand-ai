//! Interactive TUI mode for the coding agent.
//!
//! Phase-1 components (in `components/`) and the theme system (in `theme/`)
//! are fully ported. This module wires them together via [`InteractiveMode`]
//! — a skeleton port of pi-mono's `interactive-mode.ts` covering the happy
//! path: chat scrollback, editor input, agent dispatch, a small slash-command
//! table, and a model-selector overlay. Many features are deferred behind
//! `// TODO(parity)` markers; see the parity-completion plan for the queue.
//!
//! # Theming
//!
//! pi-mono's interactive components consume a coding-agent–specific `theme`
//! object (semantic color slots like `userMessageBg`, `customMessageLabel`,
//! etc.). The Phase-1 theme port is in [`theme`]; the driver currently uses
//! the components' built-in defaults rather than reading the theme directly,
//! since the theme→component wiring is a follow-up task.

pub mod components;
pub mod driver;
pub mod event_dispatch;
pub mod slash_commands;
pub mod syntax_highlight;
pub mod theme;

pub use driver::{InteractiveError, InteractiveMode};
pub use slash_commands::{
    ParsedSlashCommand, SlashCommandAction, SlashCommandContext, SlashCommandResult,
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

    let resume_session = if args.continue_session {
        None
    } else {
        args.resume.clone()
    };

    let base_config = setup.to_config(resume_session);
    let agent_tools = setup.agent_tools;

    let session = build_session(&args, base_config, agent_tools, &cwd)?;

    InteractiveMode::new(session, cwd).run().await?;
    Ok(())
}

/// Build the [`AgentSession`] honouring `--continue` / `--fork` / `--resume`
/// in the same way as the legacy `main.rs` flow.
fn build_session(
    args: &Args,
    base_config: AgentSessionConfig,
    agent_tools: Vec<hand_agent::types::AgentTool>,
    cwd: &Path,
) -> Result<AgentSession, Box<dyn std::error::Error>> {
    let session = if args.continue_session {
        match SessionManager::continue_recent(cwd) {
            Ok(sm) => {
                let config = AgentSessionConfig {
                    resume_session: Some(sm.id().to_string()),
                    ..base_config.clone()
                };
                drop(sm);
                AgentSession::new(config, agent_tools)?
            }
            Err(e) => {
                let _ = e;
                eprintln!("No previous session found. Starting a new session.");
                AgentSession::new(base_config, agent_tools)?
            }
        }
    } else if let Some(ref fork_source) = args.fork {
        let fork_path = resolve_session_path(cwd, fork_source);
        match SessionManager::fork_from(&fork_path, cwd) {
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

fn resolve_session_path(cwd: &Path, source: &str) -> PathBuf {
    let path = PathBuf::from(source);
    if path.exists() {
        return path;
    }
    let session_dir = cwd.join(".hand").join("sessions");
    let candidate = session_dir.join(format!("{}.jsonl", source));
    if candidate.exists() {
        return candidate;
    }
    if let Ok(entries) = std::fs::read_dir(&session_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if let Some(name_str) = name.to_str()
                && name_str.starts_with(source)
                && name_str.ends_with(".jsonl")
            {
                return entry.path();
            }
        }
    }
    path
}
