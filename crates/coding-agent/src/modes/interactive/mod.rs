//! Interactive TUI mode for the coding agent.
//!
//! The interactive driver runs on the **ratatui runtime** (`hand_tui::rt`) — see
//! [`rt_driver`], the M3 strangler cutover. It wires the rt session guard, frame
//! scheduler, input pump, and fixed-max inline viewport into a run loop that
//! starts, types, submits, streams a turn into scrollback, and exits cleanly on
//! every path (no `process::exit`, no raw-pointer stop handle).
//!
//! The [`event_dispatch`] protocol (`AgentSessionEvent` → `ChatUpdate`) is reused
//! unchanged — it carries zero `hand_tui` dependency and is the bridge the rt
//! driver commits agent output through.
//!
//! # Follow-up features build on this skeleton
//!
//! Full startup chrome, the rich message components (markdown / thinking / bash /
//! tool cards), the complete slash-command table, the selectors, and the full
//! footer view-model land in later M3 features on the seams the [`rt_driver`]
//! documents. The legacy `hand_tui::Tui` components (`components/`, `theme/`) are
//! retained as migration source for those features.

pub mod components;
pub mod event_dispatch;
pub mod rt_driver;
pub mod slash_commands;
pub mod syntax_highlight;
pub mod theme;

pub use rt_driver::watchdog::{DEFAULT_TURN_TIMEOUT, Watchdog};
pub use rt_driver::{InteractiveError, InteractiveMode};
pub use slash_commands::{
    ExportFormat, ParsedSlashCommand, SlashCommandAction, SlashCommandContext, SlashCommandResult,
    SlashCommandTable,
};

use std::path::{Path, PathBuf};

use crate::cli::Args;
use crate::core::agent_session::{AgentSession, AgentSessionConfig};
use crate::core::settings::SettingsManager;
use crate::modes::session_setup::SessionSetup;
use crate::{SessionBackend, SessionManager};

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

    // A bare `--resume` (no value following) lands as `Some("")` from clap. In the
    // rt TUI it mounts the one-shot session picker *before* the driver starts
    // (VAL-CHAT-036): the user lists their sessions and picks one to resume, and a
    // clean Esc cancel falls back to a fresh session with the terminal restored (no
    // orphan UI). An explicit `--continue` keeps its "resume most-recent" semantics.
    let bare_resume = matches!(args.resume.as_deref(), Some(""));
    let picked = if bare_resume {
        // Mount the picker *before* consuming the tools / building the session, so a
        // cancel leaves the terminal cooked and only then do we build a fresh session.
        Some(pick_resume_session(&cwd).await?)
    } else {
        None
    };

    // Resolve the session config *before* moving the tools out of `setup`
    // (`to_config` borrows `&setup`). `continue_like` is only meaningful on the
    // no-picker path; the picker resolves resume directly to a key (or a fresh
    // session on cancel).
    let continue_like = picked.is_none() && args.continue_session;
    let base_config = match &picked {
        // Bare `--resume` resolved the picker: a `Some(path)` resumes that session by
        // its resolved key; a `None` (clean cancel) falls back to a fresh session.
        Some(Some(path)) => setup.to_config(Some(path.to_string_lossy().to_string())),
        Some(None) => setup.to_config(None),
        // No picker: the ordinary `--continue` / `--resume <id>` / fresh path.
        None if continue_like => setup.to_config(None),
        None => setup.to_config(args.resume.clone()),
    };

    let agent_tools = setup.agent_tools;

    // The picker's `Some(Some(path))` resumes a specific session directly; every
    // other case flows through `build_session` (which honours `--continue` /
    // `--fork` / `--resume <id>`).
    let session = match picked {
        Some(Some(_)) => AgentSession::new(base_config, agent_tools)?,
        _ => build_session(&args, continue_like, base_config, agent_tools, &cwd)?,
    };

    InteractiveMode::new(session, cwd).run().await?;
    Ok(())
}

/// Mount the one-shot `--resume` session picker and return the chosen session's
/// path, or `None` when the user cancelled (Esc) or no sessions exist.
///
/// Lists the resumable sessions in `cwd` (backend-aware) and shows the rt picker.
/// A listing failure or an empty list surfaces the picker's `(no sessions)` empty
/// state (which the user leaves with Esc → `None`), so the resume flow degrades to
/// a fresh session rather than erroring.
async fn pick_resume_session(cwd: &Path) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    let backend = SettingsManager::session_backend_for_cwd(cwd);
    let sessions = SessionManager::list_with_backend(backend, cwd).unwrap_or_default();
    let picked = crate::cli::select_session(sessions).await?;
    Ok(picked)
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
    let backend = SettingsManager::session_backend_for_cwd(cwd);
    let session = if continue_like {
        // Discovery only header-scans candidates (jsonl) or reads the
        // store's session table (sqlite); the resolved key is handed
        // to AgentSession::new so the session body is read exactly
        // once, by the open inside it.
        match SessionManager::most_recent_session_key_with_backend(
            backend,
            cwd,
            base_config.session_dir.as_deref(),
        ) {
            Some(key) => {
                let config = AgentSessionConfig {
                    resume_session: Some(key),
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
        let forked = if backend == SessionBackend::Sqlite {
            SessionManager::fork_in_sqlite(cwd, base_config.session_dir.as_deref(), fork_source)
        } else {
            let fork_path =
                resolve_session_path_in(base_config.session_dir.as_deref(), cwd, fork_source);
            SessionManager::fork_from_in(&fork_path, cwd, base_config.session_dir.as_deref())
        };
        match forked {
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
