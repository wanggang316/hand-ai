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
use std::io::{self, Write};

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
            // Render setup-time errors with the canonical
            // `Error: <msg>` shape (single-line, no Debug-wrapping).
            // Without this we'd surface `Error: Other("...")` or
            // `Error: Session("...")` from tokio main's default Debug
            // formatter, which breaks the contract scripts pattern-match
            // against.
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
    // A bare `--resume` (no value) lands as `Some("")` from clap; promote
    // it to `--continue` semantics so users resume the most-recent
    // session instead of seeing `Session "" not found`.
    let bare_resume = matches!(args.resume.as_deref(), Some(""));
    let continue_like = args.continue_session || bare_resume;
    let resume_session = if continue_like {
        None
    } else {
        args.resume.clone()
    };

    let base_config = setup.to_config(resume_session);

    let mut session = if continue_like {
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
                // The underlying error is always a "no sessions found" variant
                // when --continue is invoked in a fresh dir. Don't leak the
                // CodingAgentError Display prefix into the user-facing notice
                // — the message is now informational, not an error.
                let _ = e;
                eprintln!("No previous session found. Starting a new session.");
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
                // An explicit --fork <id> that can't be resolved must
                // fail with a clear error and exit 1, not silently fall
                // through to a new session that scripts wouldn't notice
                // was empty.
                return Err(format!("No session found matching '{fork_source}'").into());
            }
        }
    } else {
        AgentSession::new(base_config, setup.agent_tools)?
    };

    let json_mode = args.mode == "json";

    if json_mode {
        // Emit the JSONL session header before any agent events fire.
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
        AgentSessionEvent::SessionInfoChanged { name } => {
            // JSON mode emits a `session_info_changed` line so JSONL
            // consumers see name updates. Text mode is silent — print
            // mode is one-shot and the user already supplied the label
            // by issuing `/name`.
            if json_mode {
                let val = serde_json::json!({
                    "type": "session_info_changed",
                    "name": name,
                });
                println!("{}", val);
            }
        }
    });

    // Non-interactive: build a single initial message from piped-stdin
    // and `--prompt`, concatenating in that order. Use cases the
    // concat unblocks:
    //
    //   cat data.csv | hand --print --prompt "summarize this CSV"
    //
    // Without the concat the model only sees "summarize this CSV" and
    // has no CSV to summarize.
    //
    // Stdin is only consumed when it's piped — an interactive terminal
    // (IsTerminal) would otherwise hang waiting for Ctrl-D.
    //
    // The combined message MUST be sent before `--export` evaluates,
    // otherwise we'd export an empty session and drop the prompt.
    let piped_stdin = read_piped_stdin();
    // Positional args land in `messages()` (the @file-stripped variant).
    // Match pi's print-mode contract: `pi --print "hello there"` should
    // treat "hello there" as the prompt without needing a separate `-p`.
    let positional_msg = {
        let msgs = args.messages();
        if msgs.is_empty() {
            None
        } else {
            Some(msgs.join(" "))
        }
    };
    // pi-parity: `@<path>` tokens anywhere in argv are file references.
    // Validate each up front; missing files surface as
    // `Error: File not found: <path>` with exit 1 BEFORE we hit the
    // provider (so a typo doesn't burn an API call and stay silent).
    let file_args_block = load_file_args(&args.file_args(), &cwd)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    if let Some(initial) = build_initial_message(
        piped_stdin.as_deref(),
        args.prompt.as_deref(),
        positional_msg.as_deref(),
    ) && !initial.trim().is_empty()
    {
        let expanded = expand_at_mentions(&initial, &cwd)
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        let with_files = match file_args_block.as_deref() {
            Some(block) => format!("{block}\n\n{expanded}"),
            None => expanded,
        };
        session.send_message(&with_files).await?;
    }

    // After the prompt has run, honor `--export` by writing the now-
    // populated session out to disk. Run before the SAW_ERROR exit so a
    // partial transcript is still recoverable on error.
    if let Some(export_path) = args.export.as_deref() {
        handle_export(&session, export_path)?;
    }

    // Exit non-zero if any assistant message ended with Error/Aborted
    // so callers can pipe / `&&` against the binary reliably (`1` on
    // failure).
    if SAW_ERROR.load(std::sync::atomic::Ordering::Relaxed) {
        std::process::exit(1);
    }
    Ok(())
}

/// Process-wide flag set when any assistant message ended with an Error
/// or Aborted stop_reason. Read by `run` after the prompt completes so
/// the process exits non-zero — matching the documented `--print`
/// contract.
static SAW_ERROR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn handle_agent_event(event: &hand_agent::types::AgentEvent) {
    use hand_agent::types::AgentEvent;
    use std::sync::atomic::Ordering;
    // `--print` text mode emits ONLY the final assistant message's text
    // content blocks after the turn completes — NOT a streaming dump.
    // Multi-step tool loops therefore stay silent until the model
    // produces a `stop_reason: Stop` message with actual text;
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
                        // Write the error to stderr verbatim, no ANSI
                        // color wrap. Scripts that pipe stderr to a
                        // file or grep would otherwise see escape
                        // sequences and have to strip them. The
                        // print-mode contract is plain stderr.
                        let msg = format_assistant_error(&a.error_message, a.stop_reason);
                        eprintln!("{}", msg);
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
        // Tool execution events are silent in text mode — they belong
        // to `--mode json`, which is not wired up here yet.
        AgentEvent::ToolExecutionStart { .. } => {}
        AgentEvent::ToolExecutionEnd { .. } => {}
        AgentEvent::ToolExecutionUpdate { .. } => {}
        _ => {}
    }
}

// ============================================================================
// JSON mode (--mode json)
// ============================================================================

/// Print the per-run `session` JSONL header emitted before any agent
/// events fire: `{"type":"session","version":3,"id":<uuid>,
/// "timestamp":<iso8601>,"cwd":<path>}`.
fn emit_json_session(cwd: &std::path::Path, id: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    // Crude UTC ISO-8601 formatter. Built manually to avoid pulling in
    // chrono just for this one timestamp; the
    // `"2026-05-12T20:41:22.791Z"` shape is close enough for script
    // consumption.
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

/// JSON-mode counterpart to [`handle_agent_event`]. Emits the
/// `--mode json` event stream: `agent_start`, `turn_start`,
/// `message_start`, `message_update` (with the inner streaming event),
/// `message_end`, `turn_end`, `tool_execution_start` / `_end`,
/// `agent_end`. Payload shapes are camelCased
/// `Message` / `AssistantMessageEvent` JSON via
/// `serde rename_all = "camelCase"`.
fn handle_agent_event_json(event: &hand_agent::types::AgentEvent) {
    use hand_agent::types::AgentEvent;
    use std::sync::atomic::Ordering;

    let payload = match event {
        AgentEvent::AgentStart => serde_json::json!({"type": "agent_start"}),
        AgentEvent::AgentEnd { messages } => {
            serde_json::json!({"type": "agent_end", "messages": messages})
        }
        AgentEvent::TurnStart => serde_json::json!({"type": "turn_start"}),
        AgentEvent::TurnEnd {
            message,
            tool_results,
        } => serde_json::json!({
            "type": "turn_end",
            "message": message,
            "toolResults": tool_results,
        }),
        AgentEvent::MessageStart { message } => {
            serde_json::json!({"type": "message_start", "message": message})
        }
        AgentEvent::MessageUpdate {
            message,
            assistant_message_event,
        } => serde_json::json!({
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
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => serde_json::json!({
            "type": "tool_execution_start",
            "toolCallId": tool_call_id,
            "toolName": tool_name,
            "args": args,
        }),
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            tool_name,
            args,
            partial_result,
        } => {
            serde_json::json!({
                "type": "tool_execution_update",
                "toolCallId": tool_call_id,
                "toolName": tool_name,
                "args": args,
                "partialResult": partial_result,
            })
        }
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result,
            is_error,
        } => {
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
    // `.json` aliases to the JSONL exporter — a JSONL stream parses as
    // a sequence of JSON values, which is what most consumers want and
    // avoids shipping a second exporter for an effectively identical
    // payload.
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("html");

    match ext {
        "jsonl" | "json" => {
            // Copy the live session file verbatim. An earlier
            // implementation handed `export_to_jsonl` a fresh
            // `SessionManager::in_memory()` with no path, which always
            // failed with "Cannot export an in-memory session" — the
            // jsonl export path of `--print --export out.jsonl` was
            // completely broken. The interactive `/export` dispatcher
            // is the model: read the session file path from the live
            // AgentSession, re-open a SessionManager rooted at that
            // path, then copy.
            let Some(file_path) = session.session_file() else {
                return Err(
                    "Cannot export an in-memory session as JSONL (use --print without --no-session)"
                        .into(),
                );
            };
            let manager = SessionManager::open(file_path)
                .map_err(|e| format!("Failed to open session for export: {e}"))?;
            export::export_to_jsonl(&manager, path)?;
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

/// Expand leading whitespace-separated `@<path>` tokens in the prompt
/// into inline file attachments. Each one becomes
/// `<file name="<absolute_path>"><file content></file>` ahead of the
/// remaining prompt text. Non-existent paths produce an error string
/// scripts can pattern-match on; empty files are silently skipped.
/// Text only — image attachments need the ImageContent path and
/// resizing logic that the single-string `--prompt` surface doesn't
/// yet expose.
fn expand_at_mentions(prompt: &str, cwd: &std::path::Path) -> Result<String, String> {
    // Collect leading `@<path>` tokens, stop at the first non-@ token.
    //
    // An argv-positional CLI gets free quoting for paths with spaces;
    // hand's single --prompt string does not. To preserve the same
    // behaviour we look ahead: if `@token` doesn't resolve to an existing
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
        let chosen_found = at_path_exists(&chosen, cwd);
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
                    best_end = tokens[k].0 + next_tok.len();
                    j = k + 1;
                }
                k += 1;
            }
            // The flag is only read AFTER the loop in earlier drafts;
            // clarify that we no longer need it now that `j` is the
            // source of truth for "have we committed an extension".
            let _ = chosen_found;
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
        // read tool does (path_utils::expand_path). Without this we'd
        // join `~/file` onto cwd, producing an invalid path that
        // failed with "File not found" instead of resolving to the
        // user's home dir.
        let path = crate::tools::path_utils::expand_path(path_str);
        let abs = if path.is_absolute() {
            path.clone()
        } else {
            cwd.join(&path)
        };
        // Skip empty files silently.
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
                // Non-UTF-8 (likely a binary file e.g. image). The
                // `--prompt` API is currently text-only, so surface a
                // clean error pointing at the problem path instead of
                // the cryptic "stream did not contain valid UTF-8" raw
                // IO message. Image @-attachments via base64 + image
                // content blocks would need separate plumbing.
                return Err(format!(
                    "Cannot attach {}: binary files (e.g. images) are not yet supported by `--prompt @<path>` — text attachments only for now",
                    abs.display()
                ));
            }
            Err(e) => {
                return Err(format!("Could not read file {}: {e}", abs.display()));
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

/// Read stdin to EOF when it's been piped (not a TTY). Returns `None`
/// when stdin is interactive — reading it would block on Ctrl-D and
/// the user almost certainly meant the prompt to come from `--prompt`.
fn read_piped_stdin() -> Option<String> {
    use std::io::{IsTerminal, Read};
    let stdin = io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut buf = String::new();
    if stdin.lock().read_to_string(&mut buf).is_err() {
        return None;
    }
    if buf.is_empty() { None } else { Some(buf) }
}

/// Combine piped-stdin and `--prompt` into a single initial message.
///
/// Parts are concatenated with an EMPTY separator; the source strings
/// (especially stdin) are expected to carry their own trailing
/// newlines. Order matters: stdin comes FIRST so the user prompt
/// provides final framing — e.g.
/// `cat data | hand --print -p "summarize the preceding data"`.
///
/// Returns `None` when neither source contributes content so the caller
/// can skip the agent send entirely. An empty `--prompt` is treated as
/// missing.
/// Load every `@<path>` reference into a single concatenated block
/// suitable for prepending to the user's prompt. Each file is wrapped
/// in a `<file path="…">…</file>` envelope so the model can attribute
/// the content. Missing files surface as `File not found: <path>` —
/// pi's wording — and the caller fails fast with exit 1 rather than
/// silently sending an unsubstituted prompt.
///
/// Returns `Ok(None)` when `paths` is empty.
fn load_file_args(paths: &[String], cwd: &std::path::Path) -> Result<Option<String>, String> {
    if paths.is_empty() {
        return Ok(None);
    }
    let mut parts = Vec::with_capacity(paths.len());
    for raw in paths {
        let resolved = if std::path::Path::new(raw).is_absolute() {
            std::path::PathBuf::from(raw)
        } else {
            cwd.join(raw)
        };
        if !resolved.exists() {
            return Err(format!("File not found: {raw}"));
        }
        let body =
            std::fs::read_to_string(&resolved).map_err(|e| format!("Failed to read {raw}: {e}"))?;
        parts.push(format!("<file path=\"{raw}\">\n{body}\n</file>"));
    }
    Ok(Some(parts.join("\n")))
}

fn build_initial_message(
    stdin: Option<&str>,
    prompt: Option<&str>,
    positional: Option<&str>,
) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    if let Some(s) = stdin {
        parts.push(s);
    }
    if let Some(p) = prompt
        && !p.is_empty()
    {
        parts.push(p);
    }
    if let Some(pos) = positional
        && !pos.is_empty()
    {
        parts.push(pos);
    }
    if parts.is_empty() {
        None
    } else {
        // Empty separator. Stdin payloads typically end in `\n` already,
        // so a plain concat reads as `<stdin>\n<prompt>`; inserting our
        // own `\n\n` would double-up newlines.
        Some(parts.concat())
    }
}

/// Format an assistant-message error for stderr emission.
///
/// Output shape: `error_message` when present, otherwise the literal
/// `Request <StopReason>` — plain text, no ANSI. Scripts that pipe or
/// redirect hand's stderr can grep this without stripping escape
/// sequences.
fn format_assistant_error(
    error_message: &Option<String>,
    stop_reason: model::StopReason,
) -> String {
    match error_message {
        Some(m) if !m.is_empty() => m.clone(),
        _ => format!("Request {:?}", stop_reason),
    }
}

fn resolve_session_path(cwd: &std::path::Path, source: &str) -> std::path::PathBuf {
    SessionManager::resolve_session_source(None, cwd, source)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal on-disk AgentSession for the export tests.
    /// Lives in the test module so its dependency on the `cfg(test)`
    /// `session_manager_mut` accessor is OK.
    fn make_session_with_file(tmp: &tempfile::TempDir) -> crate::core::agent_session::AgentSession {
        use crate::core::agent_session::{AgentSession, AgentSessionConfig};
        let model = model::Model {
            id: "test-model".into(),
            name: "Test".into(),
            api: model::types::Api::AnthropicMessages,
            provider: model::types::Provider::Anthropic,
            base_url: String::new(),
            reasoning: false,
            input: vec![model::InputType::Text],
            cost: model::Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 200_000,
            max_tokens: 4096,
            headers: None,
            compat: None,
            thinking_level_map: None,
        };
        let cfg = AgentSessionConfig {
            cwd: tmp.path().to_path_buf(),
            model,
            stream_options: model::SimpleStreamOptions::default(),
            custom_system_prompt: None,
            custom_guidelines: None,
            resume_session: None,
            no_session: false,
            no_context_files: true,
            session_dir: Some(tmp.path().join(".hand").join("sessions")),
            no_skills: true,
            base_dir: None,
        };
        let mut session = AgentSession::new(cfg, vec![]).expect("session new");
        session
            .session_manager_mut()
            .append_message(model::Message::User(model::UserMessage::new_text(
                "export me",
            )))
            .expect("append");
        session
    }

    /// Regression: `handle_export` with a `.jsonl` target must copy the
    /// live session file. The previous implementation passed a
    /// `&SessionManager::in_memory()` (which has no underlying file)
    /// to `export_to_jsonl`, so the path ALWAYS errored out with
    /// "Cannot export an in-memory session." Fixed by resolving
    /// `session.session_file()` and re-opening the manager from there
    /// — matching the interactive `/export` dispatcher.
    #[tokio::test]
    async fn handle_export_jsonl_copies_live_session_file() {
        let tmp = tempfile::tempdir().unwrap();
        let session = make_session_with_file(&tmp);

        let out = tmp.path().join("dumped.jsonl");
        handle_export(&session, &out).expect("export ok");

        let exported = std::fs::read_to_string(&out).expect("exported file readable");
        assert!(
            exported.contains("\"type\":\"session\""),
            "exported jsonl must contain the session header, got: {exported}"
        );
        assert!(
            exported.contains("export me"),
            "exported jsonl must contain the appended message, got: {exported}"
        );
    }

    /// `.json` extension aliases to the JSONL exporter — a JSONL stream
    /// parses as a sequence of JSON values, which is what most
    /// consumers want.
    #[tokio::test]
    async fn handle_export_json_extension_aliases_to_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        let session = make_session_with_file(&tmp);
        let out = tmp.path().join("dumped.json");
        handle_export(&session, &out).expect("export ok");
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("\"type\":\"session\""));
    }

    /// Pin the byte shape: piped stdin and `--prompt` concatenate into
    /// a single initial message with an empty separator. The expected
    /// output is `"README contents\nSummarize the text given"` — the
    /// single `\n` comes from stdin's trailing newline, not an
    /// injected blank line. Adding our own separator would change the
    /// canonical prompt shape and could break a model tuned on it.
    #[test]
    fn build_initial_message_concatenates_stdin_and_prompt() {
        // Stdin payloads typically end in \n (line-buffered shell).
        let combined = build_initial_message(
            Some("README contents\n"),
            Some("Summarize the text given"),
            None,
        );
        assert_eq!(
            combined.as_deref(),
            Some("README contents\nSummarize the text given")
        );

        // Without a trailing newline on stdin the two strings adjoin
        // directly.
        let no_nl = build_initial_message(Some("data"), Some("summarize"), None);
        assert_eq!(no_nl.as_deref(), Some("datasummarize"));
    }

    /// Stdin alone — common with `echo "..." | hand --print` patterns
    /// where no `--prompt` is provided.
    #[test]
    fn build_initial_message_returns_stdin_alone() {
        let combined = build_initial_message(Some("just from stdin"), None, None);
        assert_eq!(combined.as_deref(), Some("just from stdin"));
    }

    /// Prompt alone — `hand --print -p "hello"`. No stdin pipe.
    #[test]
    fn build_initial_message_returns_prompt_alone() {
        let combined = build_initial_message(None, Some("hello"), None);
        assert_eq!(combined.as_deref(), Some("hello"));
    }

    /// Positional alone — `hand --print "hello there"`. pi-compat.
    #[test]
    fn build_initial_message_returns_positional_alone() {
        let combined = build_initial_message(None, None, Some("hello there"));
        assert_eq!(combined.as_deref(), Some("hello there"));
    }

    /// `--prompt` wins over positional when both supplied (positional
    /// stays appended, matching pi's "first positional message" rule
    /// for the simple case).
    #[test]
    fn build_initial_message_prompt_and_positional_both_present() {
        let combined = build_initial_message(None, Some("the prompt"), Some("trailing positional"));
        assert_eq!(combined.as_deref(), Some("the prompttrailing positional"));
    }

    /// Neither source — caller should skip the agent send entirely.
    /// `Some("")` for prompt is treated as missing.
    #[test]
    fn build_initial_message_returns_none_when_both_empty() {
        assert_eq!(build_initial_message(None, None, None), None);
        assert_eq!(build_initial_message(None, Some(""), None), None);
        assert_eq!(build_initial_message(None, None, Some("")), None);
    }

    /// Error rendering for `--print` mode emits plain stderr text, no
    /// ANSI escapes. Scripts that pipe `hand --print 2>error.log`
    /// should see the raw message, not the `\x1b[31m...` wrap.
    #[test]
    fn format_assistant_error_uses_message_verbatim_with_no_ansi() {
        let msg = format_assistant_error(
            &Some("provider returned 429: rate limit exceeded".to_string()),
            model::StopReason::Error,
        );
        assert_eq!(msg, "provider returned 429: rate limit exceeded");
        assert!(!msg.contains('\x1b'), "must not embed ANSI escapes");
    }

    /// When the assistant provides no error_message, fall back to
    /// the literal `Request <Reason>`.
    #[test]
    fn format_assistant_error_falls_back_to_request_label() {
        assert_eq!(
            format_assistant_error(&None, model::StopReason::Aborted),
            "Request Aborted"
        );
        assert_eq!(
            format_assistant_error(&Some(String::new()), model::StopReason::Error),
            "Request Error",
            "empty string is treated the same as missing"
        );
    }

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
        // Spot-check a recent timestamp: 2026-05-12T20:41:22.791Z is
        // 56 years + leap days after 1970-01-01. We just verify the
        // shape (YYYY-MM-DDTHH:MM:SS.sssZ) is right.
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

    /// An empty / whitespace-only --prompt is a no-op at expansion
    /// time. The function returns an empty (or all-whitespace) string
    /// that the call site treats as no-message.
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
        assert!(out.contains(&format!(
            "<file name=\"{}\">\nsecond\n</file>",
            p2.display()
        )));
        assert!(out.contains("compare them"));
        // Ordering: first file appears before second.
        assert!(out.find("first").unwrap() < out.find("second").unwrap());
    }

    #[test]
    fn at_mentions_only_consume_leading_tokens() {
        // A `@` in the middle of the prompt should NOT be expanded —
        // only leading tokens are file attachments, matching positional
        // shell argument semantics.
        let path = write_tmp("body");
        let prompt = format!("preamble @{} trailing", path.display());
        let out = expand_at_mentions(&prompt, &std::env::temp_dir()).unwrap();
        assert_eq!(
            out, prompt,
            "no leading @, prompt must pass through verbatim"
        );
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
    fn at_mentions_missing_file_errors_with_canonical_text() {
        let prompt = "@/tmp/definitely-not-a-real-path-xyz-12345 hi";
        let err = expand_at_mentions(prompt, &std::env::temp_dir()).unwrap_err();
        assert!(
            err.starts_with("File not found:"),
            "expected `File not found:` prefix, got: {err}"
        );
        assert!(err.contains("definitely-not-a-real-path-xyz-12345"));
    }

    /// `~/path` in @-mentions must expand against the user's HOME the
    /// same way the read tool does, not get joined onto cwd as
    /// `cwd/~/path` (which then fails to find the file).
    ///
    /// `--prompt` is a single string, so paths with spaces can't be
    /// quoted the way they would be in argv. The expander uses greedy
    /// lookahead to glue subsequent non-@ tokens onto the path until
    /// it resolves. Verifies that a path like `@/tmp/dir with space/file`
    /// is found even though it crosses two whitespace-separated tokens,
    /// AND that the trailing prompt question is preserved separately.
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
        std::fs::write(&path, [0xFF, 0xFE, 0xFD, 0x00, 0x01]).unwrap();
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
