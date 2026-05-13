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
            Err(_) => {
                // Pi-mono parity: an explicit --fork <id> that can't be
                // resolved must fail with a clear error and exit 1, not
                // silently fall through to a new session that scripts
                // wouldn't notice was empty.
                return Err(format!(
                    "No session found matching '{fork_source}'"
                )
                .into());
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

    // Non-interactive: process a single prompt, either from --prompt or
    // stdin. The prompt MUST run before `--export` evaluates, otherwise
    // we'd export an empty session and silently drop the user's prompt
    // on the floor.
    if let Some(prompt) = args.prompt.as_deref() {
        let expanded = expand_at_mentions(prompt, &cwd).map_err(|e| -> Box<dyn std::error::Error> {
            e.into()
        })?;
        // Match pi-mono: an empty (or whitespace-only) --prompt is a
        // no-op, not an empty turn sent to the model. Without this guard
        // hand sends "" to the upstream which hallucinates wildly.
        if !expanded.trim().is_empty() {
            session.send_message(&expanded).await?;
        }
    } else {
        let stdin = io::stdin();
        let input: String = stdin
            .lock()
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<_>>()
            .join("\n");
        if !input.is_empty() {
            let expanded = expand_at_mentions(&input, &cwd).map_err(
                |e| -> Box<dyn std::error::Error> { e.into() },
            )?;
            session.send_message(&expanded).await?;
        }
    }

    // After the prompt has run, honor `--export` by writing the now-
    // populated session out to disk. Run before the SAW_ERROR exit so a
    // partial transcript is still recoverable on error.
    if let Some(export_path) = args.export.as_deref() {
        handle_export(&session, export_path)?;
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

/// Pi-mono parity: leading whitespace-separated `@<path>` tokens in the
/// prompt are treated as file attachments. Each one is expanded inline as
///
///     <file name="<absolute_path>">
///     <file content>
///     </file>
///
/// before the remaining prompt text. Non-existent paths return an error
/// matching pi's `Error: File not found: <path>` text so script consumers
/// can pattern-match on it. Empty files are silently skipped (pi's
/// behavior). This intentionally only handles TEXT files for now —
/// image attachments require the ImageContent path and resizing logic
/// that hand's --prompt single-string interface doesn't yet expose.
/// Split `s` on ASCII whitespace and return each token alongside its byte
/// offset in the original string. Mirrors `str::split_whitespace` but
/// preserves the position info we need to reconstruct the rest of the
/// prompt after the leading @-tokens.
fn split_whitespace_with_offsets(s: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip whitespace.
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        while i < bytes.len() && !(bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        out.push((start, &s[start..i]));
    }
    out
}

/// Returns `true` when the @-path candidate (after `~` / cwd expansion)
/// points at a file the process can stat. Used by [`expand_at_mentions`]
/// to disambiguate paths with spaces in them.
fn at_path_exists(path_str: &str, cwd: &std::path::Path) -> bool {
    let expanded = crate::tools::path_utils::expand_path(path_str);
    let abs = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    };
    std::fs::metadata(&abs).is_ok()
}

fn expand_at_mentions(prompt: &str, cwd: &std::path::Path) -> Result<String, String> {
    // Collect leading `@<path>` tokens, stop at the first non-@ token.
    //
    // Pi's argv-positional flow gives it free quoting for paths with
    // spaces; hand's single --prompt string does not. To preserve the
    // behavior we look ahead: if `@token` doesn't resolve to an existing
    // file, greedily glue subsequent whitespace-separated tokens onto
    // the path until we find one that does. The greedy match stops at
    // either the first existing-file candidate OR the first remaining
    // token that itself starts with `@` (next attachment) — that way
    // multiple @attachments can coexist with spaced paths.
    let tokens: Vec<(usize, &str)> = split_whitespace_with_offsets(prompt);
    let mut attachments: Vec<String> = Vec::new();
    let mut rest_start = 0usize;
    let mut i = 0usize;
    while i < tokens.len() {
        let (tok_off, tok) = tokens[i];
        let Some(initial_path) = tok.strip_prefix('@') else {
            break;
        };
        // Greedy lookahead: try the bare candidate first, then grow it
        // by appending more tokens (joined by a single space). If the
        // bare candidate or any extension resolves to an existing file,
        // use the longest match. Otherwise — the user-typed token
        // is broken in some way — keep the BARE initial path and let
        // downstream report the missing file with the cleanest path
        // (not "missing.txt summarize"). The trailing tokens stay in
        // the prompt rest so users still see what they meant.
        let bare_end = tok_off + tok.len();
        let mut chosen = initial_path.to_string();
        let mut chosen_found = at_path_exists(&chosen, cwd);
        let mut best_end = bare_end;
        let mut j = i + 1;
        if !chosen_found {
            // Grow the candidate only when it might become a real path.
            // Stop at the next `@` (next attachment) — never swallow it.
            let mut trial = chosen.clone();
            let mut k = j;
            while k < tokens.len() {
                let (_, next_tok) = tokens[k];
                if next_tok.starts_with('@') {
                    break;
                }
                trial = format!("{trial} {next_tok}");
                if at_path_exists(&trial, cwd) {
                    chosen = trial.clone();
                    chosen_found = true;
                    best_end = tokens[k].0 + next_tok.len();
                    j = k + 1;
                }
                k += 1;
            }
        }
        attachments.push(chosen);
        rest_start = best_end;
        i = j;
    }
    if attachments.is_empty() {
        return Ok(prompt.to_string());
    }

    let mut prefix = String::new();
    for path_str in &attachments {
        // Expand `~` and macOS Unicode-space variants the same way the
        // read tool does (path_utils::expand_path). Without this hand
        // joined `~/file` onto cwd, producing an invalid path that
        // failed with "File not found" instead of resolving to the
        // user's home dir as pi does.
        let path = crate::tools::path_utils::expand_path(path_str);
        let abs = if path.is_absolute() {
            path.clone()
        } else {
            cwd.join(&path)
        };
        // Skip empty files silently — pi's behavior.
        match std::fs::metadata(&abs) {
            Ok(m) if m.len() == 0 => continue,
            Ok(_) => {}
            Err(_) => {
                return Err(format!("File not found: {}", abs.display()));
            }
        }
        match std::fs::read_to_string(&abs) {
            Ok(content) => {
                use std::fmt::Write;
                let _ = write!(
                    &mut prefix,
                    "<file name=\"{}\">\n{}\n</file>\n",
                    abs.display(),
                    content,
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                // Non-UTF-8 (likely a binary file e.g. image). Pi-mono
                // supports image @file attachments via base64 + image
                // content blocks; hand's --prompt API is currently
                // text-only, so surface a clean error pointing at the
                // problem path instead of the cryptic
                // "stream did not contain valid UTF-8" raw IO message.
                return Err(format!(
                    "Cannot attach {}: binary files (e.g. images) are not yet supported by `--prompt @<path>` — text attachments only for now",
                    abs.display()
                ));
            }
            Err(e) => {
                return Err(format!(
                    "Could not read file {}: {e}",
                    abs.display()
                ));
            }
        }
    }
    let rest = prompt[rest_start..].trim_start();
    if rest.is_empty() {
        Ok(prefix.trim_end().to_string())
    } else {
        Ok(format!("{prefix}{rest}"))
    }
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

    fn write_tmp(contents: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hand-at-mention-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            seq,
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("attach.txt");
        std::fs::write(&path, contents).unwrap();
        path
    }

    /// Pi-mono parity: an empty / whitespace-only --prompt is a no-op
    /// at expansion time too. The function returns an empty (or all-
    /// whitespace) string that the call site treats as no-message.
    #[test]
    fn at_mentions_passthrough_preserves_empty_input() {
        let cwd = std::env::temp_dir();
        assert_eq!(expand_at_mentions("", &cwd).unwrap(), "");
        assert_eq!(expand_at_mentions("   ", &cwd).unwrap().trim(), "");
    }

    #[test]
    fn at_mentions_passthrough_when_absent() {
        let cwd = std::env::temp_dir();
        let out = expand_at_mentions("plain prompt", &cwd).unwrap();
        assert_eq!(out, "plain prompt");
    }

    #[test]
    fn at_mentions_expand_single_absolute_path() {
        let path = write_tmp("file body line 1\nfile body line 2\n");
        let prompt = format!("@{} please summarize", path.display());
        let out = expand_at_mentions(&prompt, &std::env::temp_dir()).unwrap();
        let expected = format!(
            "<file name=\"{}\">\nfile body line 1\nfile body line 2\n\n</file>\nplease summarize",
            path.display()
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn at_mentions_expand_multiple_paths_in_order() {
        let p1 = write_tmp("first");
        let p2 = write_tmp("second");
        let prompt = format!("@{} @{} compare them", p1.display(), p2.display());
        let out = expand_at_mentions(&prompt, &std::env::temp_dir()).unwrap();
        assert!(out.contains(&format!("<file name=\"{}\">\nfirst\n</file>", p1.display())));
        assert!(out.contains(&format!("<file name=\"{}\">\nsecond\n</file>", p2.display())));
        assert!(out.contains("compare them"));
        // Ordering: first file appears before second.
        assert!(out.find("first").unwrap() < out.find("second").unwrap());
    }

    #[test]
    fn at_mentions_only_consume_leading_tokens() {
        // A `@` in the middle of the prompt should NOT be expanded — only
        // leading tokens are file attachments, matching pi's positional
        // shell semantics.
        let path = write_tmp("body");
        let prompt = format!("preamble @{} trailing", path.display());
        let out = expand_at_mentions(&prompt, &std::env::temp_dir()).unwrap();
        assert_eq!(out, prompt, "no leading @, prompt must pass through verbatim");
    }

    /// The greedy-lookahead added for spaced paths must not swallow
    /// trailing prompt tokens into the missing-file error message. A
    /// non-existent @file followed by question text should error with
    /// the bare path, never "missing.txt summarize".
    #[test]
    fn at_mentions_missing_file_error_excludes_trailing_prompt_tokens() {
        let prompt = "@/tmp/this-path-does-not-exist-xyz123.txt summarize this";
        let err = expand_at_mentions(prompt, &std::env::temp_dir()).unwrap_err();
        assert!(
            err.contains("/tmp/this-path-does-not-exist-xyz123.txt"),
            "expected the bare path in error, got: {err}"
        );
        assert!(
            !err.contains("summarize"),
            "trailing prompt tokens must NOT leak into the path error, got: {err}"
        );
    }

    #[test]
    fn at_mentions_missing_file_errors_with_pi_text() {
        let prompt = "@/tmp/definitely-not-a-real-path-xyz-12345 hi";
        let err = expand_at_mentions(prompt, &std::env::temp_dir()).unwrap_err();
        assert!(
            err.starts_with("File not found:"),
            "expected pi-mono-style error, got: {err}"
        );
        assert!(err.contains("definitely-not-a-real-path-xyz-12345"));
    }

    /// `~/path` in @-mentions must expand against the user's HOME the
    /// same way the read tool does, not get joined onto cwd as
    /// `cwd/~/path` (which then fails to find the file). Pi-mono
    /// resolves these. Test writes into HOME, expands, and removes.
    /// Pi-mono accepts paths with spaces as a single argv positional —
    /// hand's single `--prompt` string can't preserve that grouping
    /// directly. The expander uses greedy lookahead to glue subsequent
    /// non-@ tokens onto the path until it resolves. Verifies that a
    /// path like `@/tmp/dir with space/file` is found even though it
    /// crosses two whitespace-separated tokens, AND that the trailing
    /// prompt question is preserved separately.
    #[test]
    fn at_mentions_handle_paths_with_spaces() {
        let dir = std::env::temp_dir().join(format!(
            "hand-spc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let with_space = dir.join("subdir with space");
        std::fs::create_dir_all(&with_space).unwrap();
        let path = with_space.join("attach.txt");
        std::fs::write(&path, "SPACE_BODY").unwrap();
        // Prompt has the path with a literal space + a trailing question.
        let prompt = format!("@{} describe", path.display());
        let out = expand_at_mentions(&prompt, &std::env::temp_dir()).unwrap();
        assert!(
            out.contains("SPACE_BODY"),
            "spaced path must resolve and inline the body; got: {out}"
        );
        assert!(
            out.contains(&path.display().to_string()),
            "spaced absolute path must appear in <file name=...>, got: {out}"
        );
        assert!(
            out.trim_end().ends_with("describe"),
            "trailing question must survive lookahead, got: {out}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn at_mentions_expand_tilde_against_home() {
        let home = match std::env::var("HOME") {
            Ok(h) => std::path::PathBuf::from(h),
            Err(_) => {
                // No HOME → can't test tilde expansion meaningfully.
                return;
            }
        };
        let name = format!(
            "hand-tilde-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let path = home.join(&name);
        std::fs::write(&path, "TILDE_BODY").unwrap();
        let prompt = format!("@~/{name} describe");
        let out = expand_at_mentions(&prompt, &std::env::temp_dir()).unwrap();
        assert!(
            out.contains("TILDE_BODY"),
            "tilde must expand to HOME, expected body inlined; got: {out}"
        );
        assert!(
            out.contains(&path.display().to_string()),
            "absolute resolved path must appear in <file name=...>, got: {out}"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn at_mentions_binary_file_returns_clear_error() {
        // Write a non-UTF-8 byte sequence (invalid lone high surrogate
        // start, etc.). std::fs::read_to_string fails with
        // ErrorKind::InvalidData which our handler routes to a clear
        // "binary not supported" message instead of leaking the raw
        // "stream did not contain valid UTF-8" text.
        let dir = std::env::temp_dir().join(format!(
            "hand-bin-at-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("binary.bin");
        std::fs::write(&path, &[0xFF, 0xFE, 0xFD, 0x00, 0x01]).unwrap();
        let prompt = format!("@{} describe", path.display());
        let err = expand_at_mentions(&prompt, &std::env::temp_dir()).unwrap_err();
        assert!(
            err.contains("binary files"),
            "expected binary-files hint, got: {err}"
        );
        assert!(err.contains(&path.display().to_string()));
    }

    #[test]
    fn at_mentions_empty_file_silently_skipped() {
        let path = write_tmp("");
        let prompt = format!("@{} hi", path.display());
        let out = expand_at_mentions(&prompt, &std::env::temp_dir()).unwrap();
        // Empty file produces no <file> block; rest of prompt remains.
        assert_eq!(out, "hi");
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
