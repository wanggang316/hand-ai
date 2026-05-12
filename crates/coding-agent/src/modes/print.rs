//! Non-interactive `--print` mode.
//!
//! Reads the prompt from `--prompt` or stdin, drives a single send through
//! the [`AgentSession`], and exits. Streamed output is rendered to stdout/
//! stderr exactly as in the interactive flow, so callers piping output into
//! a file see the same bytes either way.

use crate::SessionManager;
use crate::cli::Args;
use crate::core::agent_session::{AgentSession, AgentSessionConfig, AgentSessionEvent};
use crate::core::export;
use crate::modes::session_setup::SessionSetup;
use std::io::{self, BufRead, Write};

/// Run the agent in non-interactive print mode.
///
/// Mirrors the original `if cli.print { ... }` branch in `main.rs`:
/// 1. Resolve shared values from `args` via [`SessionSetup`].
/// 2. Build (or resume / continue / fork) an [`AgentSession`].
/// 3. Subscribe a printing event handler.
/// 4. Honour `--export` (early-return) before sending any prompt.
/// 5. Send the prompt from `--prompt`, or stdin if none was given.
pub async fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let setup = SessionSetup::resolve(&args)?;
    let cwd = setup.cwd.clone();

    // Determine the resume id, mirroring main.rs: --continue defers to
    // SessionManager::continue_recent below, otherwise --resume wins.
    let resume_session = if args.continue_session {
        None
    } else {
        args.resume.clone()
    };

    let base_config = setup.to_config(resume_session);

    let mut session = if args.continue_session {
        match SessionManager::continue_recent(&cwd) {
            Ok(sm) => {
                let config = AgentSessionConfig {
                    resume_session: Some(sm.id().to_string()),
                    ..base_config.clone()
                };
                drop(sm);
                AgentSession::new(config, setup.agent_tools)?
            }
            Err(e) => {
                eprintln!("No session to continue: {}. Starting new session.", e);
                AgentSession::new(base_config, setup.agent_tools)?
            }
        }
    } else if let Some(ref fork_source) = args.fork {
        let fork_path = resolve_session_path(&cwd, fork_source);
        match SessionManager::fork_from(&fork_path, &cwd) {
            Ok(sm) => {
                let config = AgentSessionConfig {
                    resume_session: Some(sm.id().to_string()),
                    ..base_config.clone()
                };
                drop(sm);
                AgentSession::new(config, setup.agent_tools)?
            }
            Err(e) => {
                eprintln!("Failed to fork session: {}. Starting new session.", e);
                AgentSession::new(base_config, setup.agent_tools)?
            }
        }
    } else {
        AgentSession::new(base_config, setup.agent_tools)?
    };

    session.subscribe(|event| match event {
        AgentSessionEvent::Agent(agent_event) => handle_agent_event(&agent_event),
        AgentSessionEvent::CompactionStart => {
            eprintln!("\x1b[33m[Compacting context...]\x1b[0m");
        }
        AgentSessionEvent::CompactionEnd { .. } => {
            eprintln!("\x1b[33m[Compaction complete]\x1b[0m");
        }
        AgentSessionEvent::Error(err) => {
            eprintln!("\x1b[31mError: {}\x1b[0m", err);
        }
    });

    if let Some(export_path) = args.export {
        return handle_export(&session, &export_path);
    }

    // Non-interactive: process a single prompt, either from --prompt or stdin.
    if let Some(prompt) = args.prompt {
        session.send_message(&prompt).await?;
    } else {
        let stdin = io::stdin();
        let input: String = stdin
            .lock()
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<_>>()
            .join("\n");
        if !input.is_empty() {
            session.send_message(&input).await?;
        }
    }

    // Exit non-zero if any assistant message ended with Error/Aborted —
    // matches pi-mono's `pi --print` exit-code contract (1 on failure)
    // so callers can pipe / `&&` against the binary reliably.
    if SAW_ERROR.load(std::sync::atomic::Ordering::Relaxed) {
        std::process::exit(1);
    }
    Ok(())
}

/// Process-wide flag set when any assistant message ended with an Error
/// or Aborted stop_reason. Read by `run` after the prompt completes so
/// the process exits non-zero — matching pi's `pi --print` contract.
static SAW_ERROR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn handle_agent_event(event: &hand_agent::types::AgentEvent) {
    use hand_agent::types::AgentEvent;
    // Track whether the CURRENT assistant message has emitted any text
    // to stdout. The agent may produce multiple assistant messages per
    // turn (e.g. a thinking-only message followed by a text-bearing one),
    // and emitting a trailing newline at MessageEnd for the empty / dropped
    // ones produces gratuitous blank lines that diverge from pi-mono's
    // text-only print contract (the TS impl emits ONLY the final assistant
    // message's text blocks).
    static TEXT_EMITTED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    use std::sync::atomic::Ordering;
    match event {
        AgentEvent::MessageUpdate {
            assistant_message_event,
            ..
        } => {
            use model::AssistantMessageEvent;
            match assistant_message_event.as_ref() {
                AssistantMessageEvent::TextDelta { delta, .. } => {
                    print!("{}", delta);
                    let _ = io::stdout().flush();
                    TEXT_EMITTED.store(true, Ordering::Relaxed);
                }
                AssistantMessageEvent::ThinkingDelta { .. } => {
                    // Pi-mono's `--print` / `-p` text mode emits only the
                    // final assistant text content blocks — thinking is
                    // deliberately suppressed so the output is scriptable.
                    // Match that contract here: drop thinking deltas
                    // entirely from stdout. (The interactive TUI still
                    // renders thinking; this branch is print-only.)
                }
                _ => {}
            }
        }
        AgentEvent::MessageEnd { message } => {
            // Surface API-level errors to stderr the same way `pi --print`
            // does — otherwise an invalid model id / 400 / 401 silently
            // exits with code 0 and an empty stdout.
            use model::{Message as ModelMessage, StopReason};
            if let ModelMessage::Assistant(a) = message
                && matches!(a.stop_reason, StopReason::Error | StopReason::Aborted)
            {
                let msg = a
                    .error_message
                    .clone()
                    .unwrap_or_else(|| format!("Request {:?}", a.stop_reason));
                eprintln!("\x1b[31m{}\x1b[0m", msg);
                SAW_ERROR.store(true, Ordering::Relaxed);
            }
            // Only terminate with a newline when this message actually
            // wrote text — thinking-only messages (e.g. an intermediate
            // assistant turn that emits a tool call after reasoning) would
            // otherwise inject a blank line.
            if TEXT_EMITTED.swap(false, Ordering::Relaxed) {
                println!();
            }
        }
        AgentEvent::ToolExecutionStart { tool_name, .. } => {
            eprintln!("\x1b[36m[{}]\x1b[0m", tool_name);
        }
        AgentEvent::ToolExecutionEnd {
            tool_name,
            is_error: true,
            ..
        } => {
            eprintln!("\x1b[31m[{} failed]\x1b[0m", tool_name);
        }
        AgentEvent::ToolExecutionEnd { .. } => {}
        AgentEvent::ToolExecutionUpdate { partial_result, .. } => {
            // Origin renamed `update: serde_json::Value` to a typed
            // `partial_result: ToolResult`. Render any text content
            // blocks as dim output to keep the prior progress UX.
            for block in &partial_result.content {
                if let model::ToolResultContent::Text(t) = block {
                    eprint!("\x1b[2m{}\x1b[0m", t.text);
                }
            }
            let _ = io::stderr().flush();
        }
        _ => {}
    }
}

fn handle_export(
    session: &AgentSession,
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("html");

    match ext {
        "jsonl" => {
            export::export_to_jsonl(&SessionManager::in_memory(), path)
                .map_err(|e| format!("JSONL export not available for active session: {}", e))?;
        }
        _ => {
            export::export_to_html(
                session.messages(),
                session.session_id(),
                &session.model().id,
                path,
            )?;
        }
    }
    Ok(())
}

fn resolve_session_path(cwd: &std::path::Path, source: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(source);
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
