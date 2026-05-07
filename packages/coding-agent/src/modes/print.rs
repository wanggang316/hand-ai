//! Non-interactive `--print` mode.
//!
//! Reads the prompt from `--prompt` or stdin, drives a single send through
//! the [`AgentSession`], and exits. Streamed output is rendered to stdout/
//! stderr exactly as in the interactive flow, so callers piping output into
//! a file see the same bytes either way.

use crate::cli::Args;
use crate::core::agent_session::{AgentSession, AgentSessionConfig, AgentSessionEvent};
use crate::core::export;
use crate::modes::session_setup::SessionSetup;
use crate::SessionManager;
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

    Ok(())
}

fn handle_agent_event(event: &hand_agent::types::AgentEvent) {
    use hand_agent::types::AgentEvent;
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
                }
                AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                    print!("\x1b[2m{}\x1b[0m", delta);
                    let _ = io::stdout().flush();
                }
                _ => {}
            }
        }
        AgentEvent::MessageEnd { .. } => {
            println!();
        }
        AgentEvent::ToolExecutionStart { tool_name, .. } => {
            eprintln!("\x1b[36m[{}]\x1b[0m", tool_name);
        }
        AgentEvent::ToolExecutionEnd {
            tool_name,
            is_error,
            ..
        } => {
            if *is_error {
                eprintln!("\x1b[31m[{} failed]\x1b[0m", tool_name);
            }
        }
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
