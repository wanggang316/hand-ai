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
    match run_inner(args).await {
        Ok(()) => Ok(()),
        Err(e) => {
            // Render setup-time errors with pi-mono's `Error: <msg>` shape
            // (single-line, no Debug-wrapping). Without this we'd surface
            // `Error: Other("...")` or `Error: Session("...")` from
            // tokio main's default Debug formatter, which breaks the
            // contract scripts pattern-match against.
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

async fn run_inner(args: Args) -> Result<(), Box<dyn std::error::Error>> {
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

    let json_mode = args.mode == "json";

    if json_mode {
        // Emit pi-mono-style session header before any agent events fire.
        emit_json_session(&cwd, session.session_id());
    }

    session.subscribe(move |event| match event {
        AgentSessionEvent::Agent(agent_event) => {
            if json_mode {
                handle_agent_event_json(&agent_event);
            } else {
                handle_agent_event(&agent_event);
            }
        }
        AgentSessionEvent::CompactionStart => {
            if !json_mode {
                eprintln!("\x1b[33m[Compacting context...]\x1b[0m");
            }
        }
        AgentSessionEvent::CompactionEnd { .. } => {
            if !json_mode {
                eprintln!("\x1b[33m[Compaction complete]\x1b[0m");
            }
        }
        AgentSessionEvent::Error(err) => {
            if json_mode {
                // Emit as a JSON error event so JSONL consumers parse it
                // uniformly with other events; still set SAW_ERROR so the
                // process exits 1.
                let val = serde_json::json!({
                    "type": "error",
                    "error": err.to_string(),
                });
                println!("{}", val);
            } else {
                eprintln!("\x1b[31mError: {}\x1b[0m", err);
            }
            // Session-level errors (e.g. "No API key found", network
            // failures) must propagate to the exit code. Without this
            // hand exits 0 on a red error, masking failures from
            // scripts that `&&`-chain it.
            SAW_ERROR.store(true, std::sync::atomic::Ordering::Relaxed);
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
    use std::sync::atomic::Ordering;
    // Pi-mono's `pi --print` text mode emits ONLY the final assistant
    // message's text content blocks after the turn completes — NOT a
    // streaming dump. Multi-step tool loops therefore stay silent until
    // the model produces a `stop_reason: Stop` message with actual text;
    // intermediate tool-call rounds are invisible to scripts.
    //
    // Approach: drop MessageUpdate / streaming events entirely. At
    // MessageEnd with `StopReason::Stop`, walk the message's text content
    // blocks and write each one to stdout terminated by `\n`. Error /
    // Aborted messages surface their error_message on stderr.
    match event {
        AgentEvent::MessageUpdate { .. } => {
            // Suppressed — see contract above.
        }
        AgentEvent::MessageEnd { message } => {
            use model::{AssistantContentBlock, Message as ModelMessage, StopReason};
            if let ModelMessage::Assistant(a) = message {
                match a.stop_reason {
                    StopReason::Error | StopReason::Aborted => {
                        let msg = a
                            .error_message
                            .clone()
                            .unwrap_or_else(|| format!("Request {:?}", a.stop_reason));
                        eprintln!("\x1b[31m{}\x1b[0m", msg);
                        SAW_ERROR.store(true, Ordering::Relaxed);
                    }
                    StopReason::Stop => {
                        // Final assistant turn: emit only the text content
                        // blocks. Skips thinking / tool_call blocks so the
                        // output stays grep-friendly.
                        for block in &a.content {
                            if let AssistantContentBlock::Text(t) = block {
                                println!("{}", t.text);
                            }
                        }
                        let _ = io::stdout().flush();
                    }
                    _ => {
                        // ToolUse / Continue / other intermediate stops:
                        // silent. The agent loop will keep going and
                        // eventually produce a Stop or Error message.
                    }
                }
            }
        }
        // Tool execution events are silent in text mode — reserved for
        // `pi --mode json` upstream; hand has no JSON mode wired yet.
        AgentEvent::ToolExecutionStart { .. } => {}
        AgentEvent::ToolExecutionEnd { .. } => {}
        AgentEvent::ToolExecutionUpdate { .. } => {}
        _ => {}
    }
}

// ============================================================================
// JSON mode (pi-mono `--mode json` parity)
// ============================================================================

/// Print the per-run `session` JSONL header pi-mono emits before any events
/// fire. Mirrors `{"type":"session","version":3,"id":<uuid>,
/// "timestamp":<iso8601>,"cwd":<path>}`.
fn emit_json_session(cwd: &std::path::Path, id: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    // Crude UTC ISO-8601 formatter — depends only on chrono via serde_json's
    // ecosystem? No, just format manually to avoid pulling in chrono just
    // for this one timestamp. Matches pi's "2026-05-12T20:41:22.791Z" shape
    // closely enough for script consumption.
    let dt = format_iso8601(secs, now.subsec_millis());
    let val = serde_json::json!({
        "type": "session",
        "version": 3,
        "id": id,
        "timestamp": dt,
        "cwd": cwd.display().to_string(),
    });
    println!("{}", val);
}

/// Minimal UTC ISO-8601 (`YYYY-MM-DDTHH:MM:SS.sssZ`) from a Unix-seconds
/// value. Good enough for JSONL consumers; not calendar-correct beyond
/// the proleptic Gregorian boundaries.
fn format_iso8601(secs: u64, millis: u32) -> String {
    let days = secs / 86400;
    let rem = secs % 86400;
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;
    // Convert days-since-epoch (1970-01-01) to civil date via Howard
    // Hinnant's algorithm; integer arithmetic only.
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, m, d, hour, minute, second, millis
    )
}

/// JSON-mode counterpart to [`handle_agent_event`]. Mirrors pi-mono's
/// `--mode json` event types: `agent_start`, `turn_start`, `message_start`,
/// `message_update` (with the inner streaming event), `message_end`,
/// `turn_end`, `tool_execution_start`/`_end`, `agent_end`. The exact
/// payload shapes are camelCased Message/AssistantMessageEvent JSON which
/// matches the TS reference because both crates serialize the same
/// underlying types with serde rename_all = "camelCase".
fn handle_agent_event_json(event: &hand_agent::types::AgentEvent) {
    use hand_agent::types::AgentEvent;
    use std::sync::atomic::Ordering;

    let payload = match event {
        AgentEvent::AgentStart => serde_json::json!({"type": "agent_start"}),
        AgentEvent::AgentEnd { messages } => {
            serde_json::json!({"type": "agent_end", "messages": messages})
        }
        AgentEvent::TurnStart => serde_json::json!({"type": "turn_start"}),
        AgentEvent::TurnEnd { message, tool_results } => serde_json::json!({
            "type": "turn_end",
            "message": message,
            "toolResults": tool_results,
        }),
        AgentEvent::MessageStart { message } => {
            serde_json::json!({"type": "message_start", "message": message})
        }
        AgentEvent::MessageUpdate { message, assistant_message_event } => serde_json::json!({
            "type": "message_update",
            "assistantMessageEvent": assistant_message_event,
            "message": message,
        }),
        AgentEvent::MessageEnd { message } => {
            // Mirror text-mode SAW_ERROR semantics so JSON consumers also
            // get a non-zero exit when the model errored out.
            use model::{Message as ModelMessage, StopReason};
            if let ModelMessage::Assistant(a) = message
                && matches!(a.stop_reason, StopReason::Error | StopReason::Aborted)
            {
                SAW_ERROR.store(true, Ordering::Relaxed);
            }
            serde_json::json!({"type": "message_end", "message": message})
        }
        AgentEvent::ToolExecutionStart { tool_call_id, tool_name, args } => serde_json::json!({
            "type": "tool_execution_start",
            "toolCallId": tool_call_id,
            "toolName": tool_name,
            "args": args,
        }),
        AgentEvent::ToolExecutionUpdate { tool_call_id, tool_name, args, partial_result } => {
            serde_json::json!({
                "type": "tool_execution_update",
                "toolCallId": tool_call_id,
                "toolName": tool_name,
                "args": args,
                "partialResult": partial_result,
            })
        }
        AgentEvent::ToolExecutionEnd { tool_call_id, tool_name, result, is_error } => {
            serde_json::json!({
                "type": "tool_execution_end",
                "toolCallId": tool_call_id,
                "toolName": tool_name,
                "result": result,
                "isError": is_error,
            })
        }
    };
    println!("{}", payload);
    let _ = io::stdout().flush();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_formatter_round_trip_at_known_timestamps() {
        // 1970-01-01T00:00:00.000Z (Unix epoch)
        assert_eq!(format_iso8601(0, 0), "1970-01-01T00:00:00.000Z");
        // 2024-01-01T00:00:00.000Z (post-leap-year)
        assert_eq!(format_iso8601(1_704_067_200, 0), "2024-01-01T00:00:00.000Z");
        // Mid-day check, with millis.
        assert_eq!(
            format_iso8601(1_704_067_200 + 12 * 3600 + 34 * 60 + 56, 789),
            "2024-01-01T12:34:56.789Z"
        );
        // Pi sample used 2026-05-12T20:41:22.791Z — compute its epoch.
        // 2026-05-12 is 56 years + leap days after 1970-01-01.
        // We just verify the shape (YYYY-MM-DDTHH:MM:SS.sssZ) is right.
        let s = format_iso8601(1_778_618_482, 791);
        assert!(s.starts_with("2026-05-12T"));
        assert!(s.ends_with("Z"));
        assert_eq!(s.len(), 24);
    }
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
