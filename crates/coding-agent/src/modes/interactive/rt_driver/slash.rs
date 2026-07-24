//! Slash-command dispatch and action handlers for the rt interactive driver.
//!
//! This is the M3 slash seam: the reused parsing layer
//! ([`ParsedSlashCommand`] + [`SlashCommandTable`]) is unchanged, and this
//! module is the *driver-side* half — it turns a submitted line into a typed
//! [`SlashCommandAction`] and executes the action against the live
//! [`AgentSession`] and the shared [`DriverState`].
//!
//! # Where dispatch runs (concurrency)
//!
//! The `/quit` family is resolved in the input loop (it needs the loop's
//! `break` to tear down). Everything else is forwarded through the same
//! `submit_tx` channel the model turns use, so it runs on the **turn runner
//! task** — the one place that owns `&mut AgentSession`. That is why
//! [`apply_slash_action`] takes `&mut AgentSession`: the session-lifecycle
//! commands (`/new`, `/clone`, `/import`, `/name`) mutate it, and `/session`
//! / `/export` read it.
//!
//! # The dispatch table is the extension point
//!
//! [`apply_slash_action`] matches every [`SlashCommandAction`] variant, so the
//! table is exhaustive at compile time: adding a new command to the parsing
//! layer forces a new arm here. This feature implements the *session-lifecycle*
//! arms (`ClearChat` / `NewSession` / `ShowSessionInfo` / `Export` / `Import` /
//! `Clone` / `Name`) and the always-safe leaves (`ShowText` / `Quit` / `Noop`).
//! Every other arm routes through [`unsupported`], which commits a single
//! yellow "not yet available" status line — the seam the follow-up info-command
//! and selector features replace, one arm at a time, without touching the
//! dispatch wiring or the commit path.

use std::path::Path;
use std::sync::{Arc, Mutex};

use hand_tui::rt::scheduler::FrameRequester;
use ratatui::text::Line;

use crate::core::agent_session::AgentSession;
use crate::modes::interactive::slash_commands::{
    ExportFormat, ParsedSlashCommand, SlashCommandAction, SlashCommandContext, SlashCommandResult,
    SlashCommandTable,
};

use super::chat;
use super::footer::{TokenUsageSummary, build_footer_view, thinking_level_label};
use super::state::{DriverState, SharedFooter, lock_footer, lock_state};
use super::summary::labelled_box_lines;

/// Whether a dispatched slash command asked the driver to exit.
///
/// The quit family is caught earlier, in the input loop (it needs the loop's
/// `break`), so in practice the turn-runner path only ever sees
/// [`SlashOutcome::Continue`]; the `Quit` variant keeps the handler total so
/// the exhaustive match compiles and a future caller that runs dispatch from a
/// different seam still gets a correct answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashOutcome {
    /// The command was handled; keep the session running.
    Continue,
    /// The user asked to quit.
    Quit,
}

/// The escape that clears the whole screen *and* the scrollback buffer, then
/// homes the cursor: `ESC[3J` (erase scrollback) + `ESC[2J` (erase display) +
/// `ESC[H` (cursor home). Queued as a raw sequence so it rides the same
/// terminal-owning draw path the OSC 133 marks use — the only place that is
/// allowed to write to the terminal (invariant #1: the scheduler owns it).
const CLEAR_SCREEN_AND_SCROLLBACK: &str = "\x1b[3J\x1b[2J\x1b[H";

/// Whether a submitted line is a slash command the driver should intercept
/// rather than send to the model.
///
/// A plain `/` with no name (or a leading-space-only body) is not a command —
/// [`ParsedSlashCommand::parse`] returns `None`, matching the parsing layer's
/// own contract — so such input falls through to the model like ordinary text.
#[must_use]
pub fn is_slash_command(trimmed: &str) -> bool {
    ParsedSlashCommand::parse(trimmed).is_some()
}

/// Whether a submission is a bare `/model` (or its `/models` alias) with no
/// pattern — the case that opens the interactive model selector overlay.
///
/// A `/model <pattern>` submission (a non-interactive switch) is *not* matched: it
/// carries args and routes through the sync slash dispatch's
/// [`SlashCommandAction::ModelByPattern`] arm instead. The driver uses this to
/// intercept the bare form on the async turn runner (it mounts an overlay and
/// awaits the pick), *before* the sync slash dispatch — mirroring how `/compact` is
/// intercepted.
#[must_use]
pub fn is_open_model_selector(line: &str) -> bool {
    let Some(parsed) = ParsedSlashCommand::parse(line) else {
        return false;
    };
    matches!(parsed.name.as_str(), "model" | "models") && parsed.args.is_empty()
}

/// The typed [`SlashCommandAction`] for a submission if it belongs to the config
/// selector family (`/thinking`, `/theme`, `/settings`, `/model <pattern>`) — the
/// commands the driver intercepts on the async turn runner (they may open an
/// overlay and await, or apply a direct argument), *before* the sync slash
/// dispatch. `None` for anything else (including bare `/model`, which
/// [`is_open_model_selector`] already routes).
///
/// Returning the parsed action (rather than a bare predicate) lets the driver route
/// the dialog-vs-direct-arg split off the same parse the sync dispatch would do, so
/// the two paths never disagree.
#[must_use]
pub fn config_selector_action(line: &str) -> Option<SlashCommandAction> {
    let parsed = ParsedSlashCommand::parse(line)?;
    let ctx = SlashCommandContext {
        // The context is only read by commands that echo the current model; the
        // config-selector commands do not, so placeholder values are fine here —
        // the driver re-dispatches against the live session when it applies.
        model_id: String::new(),
        provider: String::new(),
    };
    let action = match SlashCommandTable::dispatch(&parsed, &ctx) {
        SlashCommandResult::Handled(action) => action,
        SlashCommandResult::Unknown => return None,
    };
    super::selectors::is_config_selector_action(&action).then_some(action)
}

/// The typed [`SlashCommandAction`] for a submission if it belongs to the *picker*
/// selector family (`/tree`, `/scoped-models`, `/fork`) — the commands the driver
/// intercepts on the async turn runner because they mount a modal overlay and await
/// the pick (`/tree` and `/scoped-models` unconditionally; `/fork` when it opens the
/// picker — a bare `/fork` or `/fork <entry-id>` that resolves interactively). `None`
/// for anything else.
///
/// Returning the parsed action (rather than a bare predicate) lets the driver route
/// the three pickers off the same parse the sync dispatch would do, so the two paths
/// never disagree — the same shape as [`config_selector_action`].
#[must_use]
pub fn picker_selector_action(line: &str) -> Option<SlashCommandAction> {
    let parsed = ParsedSlashCommand::parse(line)?;
    let ctx = SlashCommandContext {
        // The picker commands do not echo the current model, so placeholders are
        // fine — the driver re-reads the live session when it opens the overlay.
        model_id: String::new(),
        provider: String::new(),
    };
    let action = match SlashCommandTable::dispatch(&parsed, &ctx) {
        SlashCommandResult::Handled(action) => action,
        SlashCommandResult::Unknown => return None,
    };
    super::selectors::is_picker_selector_action(&action).then_some(action)
}

/// Whether a submission is a `/resume` command — the case that opens the session
/// picker overlay.
///
/// `/resume` takes no argument in the rt driver: it always opens the picker (the
/// legacy "resume most recent" fallback is `--continue` / `hand --resume`). The
/// driver uses this to intercept it on the async turn runner (it mounts an overlay,
/// awaits the pick, then switches + replays), *before* the sync slash dispatch —
/// mirroring how `/model` is intercepted.
#[must_use]
pub fn is_open_resume_picker(line: &str) -> bool {
    ParsedSlashCommand::parse(line).is_some_and(|parsed| parsed.name == "resume")
}

/// Recognise a `/compact` submission and pull its optional steering text.
///
/// Returns `Some(None)` for a bare `/compact`, `Some(Some(steer))` for
/// `/compact <steer>`, and `None` for anything that is not a `/compact`
/// command. The driver uses this to intercept `/compact` on the async turn
/// runner (it calls `session.compact()`), *before* the sync slash dispatch —
/// the one command in the table that needs to await.
#[must_use]
pub fn parse_compact(line: &str) -> Option<Option<String>> {
    let parsed = ParsedSlashCommand::parse(line)?;
    if parsed.name != "compact" {
        return None;
    }
    Some((!parsed.args.is_empty()).then(|| parsed.args.clone()))
}

/// Dispatch a submitted slash line: parse it, resolve the typed action through
/// the reused [`SlashCommandTable`], and execute it against the session and the
/// shared driver state. Returns whether the command asked to quit.
///
/// An unparseable line (bare `/`) or an unrecognised command commits a yellow
/// hint and returns [`SlashOutcome::Continue`] — the driver never silently
/// swallows a mistyped command.
pub fn dispatch_slash(
    line: &str,
    session: &mut AgentSession,
    cwd: &Path,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
) -> SlashOutcome {
    let Some(parsed) = ParsedSlashCommand::parse(line) else {
        commit_status(state, requester, "[unknown command]");
        return SlashOutcome::Continue;
    };

    let ctx = SlashCommandContext {
        model_id: session.model().id.clone(),
        provider: session.model().provider.as_str().to_string(),
    };

    match SlashCommandTable::dispatch(&parsed, &ctx) {
        SlashCommandResult::Handled(action) => {
            apply_slash_action(action, session, cwd, state, footer, requester)
        }
        SlashCommandResult::Unknown => {
            // The unknown-command prompt (VAL-CHAT-006): name the mistyped
            // command and point the user at /help. Committed yellow, so a typo is
            // an obvious, recoverable notice rather than a silent swallow.
            commit_status(
                state,
                requester,
                &format!("Unknown command: /{}. Type /help for a list.", parsed.name),
            );
            SlashOutcome::Continue
        }
    }
}

/// Execute a typed [`SlashCommandAction`] against the driver.
///
/// The match is exhaustive: every variant is handled so the table stays a
/// compile-time-checked source of truth. The arms this feature owns are the
/// session-lifecycle commands; the rest route through [`unsupported`] until a
/// follow-up feature replaces them.
pub fn apply_slash_action(
    action: SlashCommandAction,
    session: &mut AgentSession,
    cwd: &Path,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
) -> SlashOutcome {
    match action {
        // --- Session-lifecycle commands (this feature) --------------------
        SlashCommandAction::ClearChat => {
            apply_clear(state, requester);
        }
        SlashCommandAction::NewSession => {
            apply_new_session(session, cwd, state, footer, requester);
        }
        SlashCommandAction::ShowSessionInfo => {
            let usage = lock_state(state).usage;
            let text = render_session_info(session, &usage);
            commit_status(state, requester, &text);
        }
        SlashCommandAction::Export(path, fmt) => {
            apply_export(session, &path, fmt, state, requester);
        }
        SlashCommandAction::Import(path) => {
            apply_import(session, &path, cwd, state, footer, requester);
        }
        SlashCommandAction::Clone => {
            apply_clone(session, state, requester);
        }
        SlashCommandAction::Name(label) => {
            apply_name(session, &label, state, footer, requester);
        }

        // --- Info commands (this feature) ---------------------------------
        // `/help` renders through the ShowText leaf below (the parsing layer
        // builds its response text). `/copy` + Ctrl+X, `/copy N`, `/skills`,
        // `/extensions`, `/diagnostics`, and `/changelog` render inline here.
        SlashCommandAction::CopyLastAssistant => {
            apply_copy_last_assistant(session, state, requester);
        }
        SlashCommandAction::CopyN(n) => {
            apply_copy_n(session, n, state, requester);
        }
        SlashCommandAction::ListSkills => {
            let body = skills_body(session);
            commit(
                state,
                requester,
                labelled_box_lines("skills", &body, box_width(state)),
            );
        }
        SlashCommandAction::ListExtensions => {
            let body = extensions_body(session);
            commit(
                state,
                requester,
                labelled_box_lines("extensions", &body, box_width(state)),
            );
        }
        SlashCommandAction::ShowDiagnostics => {
            let body = diagnostics_body();
            commit(
                state,
                requester,
                labelled_box_lines("diagnostics", &body, box_width(state)),
            );
        }
        SlashCommandAction::Changelog => {
            let body = changelog_body();
            commit(
                state,
                requester,
                labelled_box_lines("changelog", &body, box_width(state)),
            );
        }

        // --- Always-safe leaves -------------------------------------------
        // ShowText carries a usage / error string the parsing layer already
        // built (e.g. `/import` with no arg, `/export` unknown extension, and
        // the `/help` response text).
        SlashCommandAction::ShowText(text) => commit_status(state, requester, &text),
        SlashCommandAction::Quit => return SlashOutcome::Quit,
        SlashCommandAction::Noop => {}

        // `/compact` runs the async summarizer, so it is intercepted on the
        // turn-runner task *before* this sync dispatch (see the driver's
        // `run_turn`). Reaching this arm means a caller dispatched `/compact`
        // outside that path; surface the seam line rather than silently
        // dropping it.
        compact @ SlashCommandAction::Compact(_) => unsupported(&compact, state, requester),

        // Bare `/model` opens the interactive selector overlay, which awaits the
        // user's pick, so it is intercepted on the async turn runner *before* this
        // sync dispatch (see the driver's `run_turn` + `run_model_selector`).
        // Reaching this arm means a caller dispatched it outside that path; surface
        // the seam line rather than silently dropping it. (`/model <pattern>` is a
        // separate `ModelByPattern` action, handled by the follow-up feature.)
        open @ SlashCommandAction::OpenModelSelector => unsupported(&open, state, requester),

        // `/resume` opens the interactive session picker overlay, which awaits the
        // user's pick then switches + replays, so it is intercepted on the async
        // turn runner *before* this sync dispatch (see the driver's `run_turn` +
        // `run_resume_picker`). Reaching this arm means a caller dispatched it
        // outside that path; surface the seam line rather than silently dropping it.
        open @ SlashCommandAction::OpenResumePicker => unsupported(&open, state, requester),

        // The config-selector family (`/thinking`, `/theme`, `/settings`, and
        // `/model <pattern>`) is intercepted on the async turn runner *before* this
        // sync dispatch (see `run_turn` + `run_config_selector`): the bare forms
        // mount a modal overlay and await, and the direct-arg forms apply against
        // `&mut session`. Reaching these arms means a caller dispatched one outside
        // that path; surface the seam line rather than silently dropping it — the
        // same contract as `/model` / `/resume` above.
        thinking @ SlashCommandAction::OpenThinkingSelector { .. } => {
            unsupported(&thinking, state, requester)
        }
        settings @ SlashCommandAction::OpenSettingsSelector => {
            unsupported(&settings, state, requester)
        }
        theme @ SlashCommandAction::Theme(_) => unsupported(&theme, state, requester),
        model @ SlashCommandAction::ModelByPattern(_) => unsupported(&model, state, requester),

        // The picker-selector family (`/tree`, `/scoped-models`, `/fork`) is
        // intercepted on the async turn runner *before* this sync dispatch (see
        // `run_turn` + `run_picker_selector`): each mounts a modal overlay and awaits
        // the pick, then applies it against `&mut session`. Reaching these arms means
        // a caller dispatched one outside that path; surface the seam line rather than
        // silently dropping it — the same contract as `/model` / `/resume` above.
        tree @ SlashCommandAction::OpenTreeSelector(_) => unsupported(&tree, state, requester),
        scoped @ SlashCommandAction::OpenScopedModelsSelector => {
            unsupported(&scoped, state, requester)
        }
        fork @ SlashCommandAction::Fork(_) => unsupported(&fork, state, requester),

        // --- Follow-up feature seam ---------------------------------------
        // The remaining selector commands (/login, …) land here as their features
        // arrive; each replaces one arm without touching the dispatch wiring.
        other => unsupported(&other, state, requester),
    }
    SlashOutcome::Continue
}

/// `/clear` — wipe the visible scrollback and the terminal's scroll buffer,
/// leaving the session **context intact**. Only the screen is cleared: the
/// message history and the running usage accumulator are untouched, so the
/// footer's context % / usage segments do not move (VAL-CHAT-024).
///
/// Native scrollback is immutable through the [`HistorySink`] commit path, so
/// the wipe is a raw `ESC[3J ESC[2J ESC[H` queued for the terminal-owning draw
/// closure — the same out-of-band raw channel the OSC 133 marks use. The
/// `[chat cleared]` status line is committed *after* the wipe so it lands as
/// the first line on the freshly cleared screen.
///
/// [`HistorySink`]: hand_tui::rt::history::HistorySink
fn apply_clear(state: &Arc<Mutex<DriverState>>, requester: &FrameRequester) {
    lock_state(state).queue_raw(CLEAR_SCREEN_AND_SCROLLBACK);
    requester.request_frame();
    commit_status(state, requester, "[chat cleared]");
}

/// `/new` — reset the session: drop the transcript and start a fresh session
/// (a new on-disk file when the session is on-disk, else a fresh in-memory
/// one), then rebuild the footer so its context % falls back to the
/// empty-session baseline (VAL-CHAT-025). On failure the red `[/new failed: …]`
/// banner lands and nothing is reset.
///
/// The scrollback is cleared too so the fresh session starts on a clean screen,
/// matching the legacy driver's "clear the chat list on new session" behaviour.
fn apply_new_session(
    session: &mut AgentSession,
    cwd: &Path,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
) {
    match session.reset_session() {
        Ok(()) => {
            // Reset the running usage accumulator: the new session starts with
            // no spend, so the footer's usage / context segments fall to the
            // empty-session baseline rather than carrying the prior session's
            // totals.
            lock_state(state).usage = TokenUsageSummary::default();
            lock_state(state).queue_raw(CLEAR_SCREEN_AND_SCROLLBACK);
            refresh_footer(session, cwd, state, footer);
            commit_status(state, requester, "[new session started]");
        }
        Err(e) => commit_error(state, requester, &format!("[/new failed: {e}]")),
    }
}

/// `/export <path>` — write the session to `path` in the format inferred from
/// the extension (VAL-CHAT-040). Refuses to overwrite an existing file. The
/// unknown-extension and missing-argument cases never reach here: the parsing
/// layer resolves them to a [`SlashCommandAction::ShowText`] usage hint, so an
/// unsupported extension writes nothing.
fn apply_export(
    session: &AgentSession,
    path: &Path,
    fmt: ExportFormat,
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
) {
    use crate::core::export::{export_to_html, export_to_json, export_to_jsonl};
    use crate::core::session_manager::SessionManager;

    if path.exists() {
        commit_error(
            state,
            requester,
            &format!(
                "[/export: {} already exists. Delete it or choose a different path.]",
                path.display()
            ),
        );
        return;
    }

    let result = match fmt {
        ExportFormat::Jsonl | ExportFormat::Json => {
            // `.jsonl` / `.json` copy the on-disk session file; an in-memory
            // session has no file to serialize, so surface that explicitly
            // rather than writing an empty document.
            let Some(session_path) = session.session_file() else {
                commit_error(
                    state,
                    requester,
                    "[/export: cannot export an in-memory session as JSON/JSONL]",
                );
                return;
            };
            let manager = match SessionManager::open(session_path) {
                Ok(m) => m,
                Err(e) => {
                    commit_error(state, requester, &format!("[/export failed: {e}]"));
                    return;
                }
            };
            if matches!(fmt, ExportFormat::Jsonl) {
                export_to_jsonl(&manager, path)
            } else {
                export_to_json(&manager, path)
            }
        }
        ExportFormat::Html => {
            let session_id = session.session_id().to_string();
            let model_id = session.model().id.clone();
            export_to_html(session.messages(), &session_id, &model_id, path)
        }
    };

    match result {
        Ok(()) => commit_status(
            state,
            requester,
            &format!("[exported to {}]", path.display()),
        ),
        Err(e) => commit_error(state, requester, &format!("[/export failed: {e}]")),
    }
}

/// `/import <path>` — replace the active session in place with the JSONL/JSON
/// file at `path` and rebuild the footer so the imported session's context %
/// and label surface (VAL-CHAT-041). A missing file or a malformed session
/// takes the red-banner route. The bare-`/import` usage hint is produced by the
/// parsing layer as a [`SlashCommandAction::ShowText`], so it never reaches
/// here.
fn apply_import(
    session: &mut AgentSession,
    path: &Path,
    cwd: &Path,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
) {
    if !path.exists() {
        commit_error(
            state,
            requester,
            &format!("[/import: file not found: {}]", path.display()),
        );
        return;
    }
    match session.switch_session(path) {
        Ok(()) => {
            lock_state(state).usage = TokenUsageSummary::default();
            lock_state(state).queue_raw(CLEAR_SCREEN_AND_SCROLLBACK);
            refresh_footer(session, cwd, state, footer);
            commit_status(
                state,
                requester,
                &format!("[imported session from {}]", path.display()),
            );
        }
        Err(e) => commit_error(state, requester, &format!("[/import failed: {e}]")),
    }
}

/// `/clone` — duplicate the current session under a fresh id, writing a new
/// session file to the store (VAL-CHAT-042). The status line reports the new
/// id; on failure the red `[/clone failed: …]` banner lands.
fn apply_clone(
    session: &mut AgentSession,
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
) {
    match session.clone_session() {
        Ok(()) => commit_status(
            state,
            requester,
            &format!("[cloned session: new id {}]", session.session_id()),
        ),
        Err(e) => commit_error(state, requester, &format!("[/clone failed: {e}]")),
    }
}

/// `/name <label>` — set the session label, update the footer so the new name
/// shows, then confirm (VAL-CHAT-043). A subsequent `/session` reflects the new
/// label because it reads `session.label()` live. The bare-`/name` usage hint
/// is produced by the parsing layer, so it never reaches here.
fn apply_name(
    session: &mut AgentSession,
    label: &str,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
) {
    match session.set_label(label) {
        Ok(()) => {
            lock_footer(footer).session_name = Some(label.to_string());
            commit_status(state, requester, &format!("[session name set: {label}]"));
        }
        Err(e) => commit_error(state, requester, &format!("[/name failed: {e}]")),
    }
}

/// Render the `/session` fact block: id, label (when set), message count,
/// model + provider, thinking level, and token usage (when any accrued) with
/// the session duration (VAL-CHAT-026). Returns the text so the caller commits
/// it as a status block.
fn render_session_info(session: &AgentSession, usage: &TokenUsageSummary) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    let _ = writeln!(out, "Session: {}", session.session_id());
    if let Some(label) = session.label() {
        let _ = writeln!(out, "Label: {label}");
    }
    let _ = writeln!(out, "Messages: {}", session.message_count());
    let model = session.model();
    let _ = writeln!(out, "Model: {} ({})", model.id, model.provider.as_str());
    let thinking = thinking_level_label(session.stream_options().reasoning);
    let _ = writeln!(out, "Thinking: {thinking}");
    if usage.input > 0 || usage.output > 0 || usage.cache_read > 0 || usage.cache_write > 0 {
        let _ = writeln!(
            out,
            "Tokens: {} in / {} out (cache read {} / write {})",
            usage.input, usage.output, usage.cache_read, usage.cache_write,
        );
        if usage.cost_usd > 0.0 {
            let _ = writeln!(out, "Cost: ${:.4}", usage.cost_usd);
        }
    }
    if let Some(started_ms) = session_started_at_ms(session) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let elapsed_ms = (now_ms - started_ms).max(0) as u64;
        let _ = writeln!(out, "Duration: {}", format_duration_ms(elapsed_ms));
    }
    // Trim the trailing newline so the status block renders flush.
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

/// The session-start timestamp in epoch millis, read from the first entry that
/// carries one (the session header). `None` for an in-memory session with no
/// timestamped entries.
fn session_started_at_ms(session: &AgentSession) -> Option<i64> {
    session
        .session_manager()
        .entries()
        .iter()
        .find_map(|e| e.timestamp())
}

/// Format an elapsed duration in millis as `Hh Mm Ss` (dropping empty leading
/// units), matching the legacy `/session` duration rendering.
fn format_duration_ms(ms: u64) -> String {
    let total_s = ms / 1000;
    let h = total_s / 3600;
    let m = (total_s % 3600) / 60;
    let s = total_s % 60;
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// Rebuild the footer view-model from current session state (context %, label,
/// usage) and request a repaint. Called after a session-mutating command so the
/// footer reflects the new state on the next frame.
fn refresh_footer(
    session: &AgentSession,
    cwd: &Path,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
) {
    let usage = lock_state(state).usage;
    *lock_footer(footer) = build_footer_view(session, cwd, usage);
}

/// The render width the info-command tinted boxes wrap to — the tracked
/// terminal columns.
fn box_width(state: &Arc<Mutex<DriverState>>) -> u16 {
    lock_state(state).size.cols
}

/// `/copy` (and its Ctrl+X shortcut) — copy the last assistant message's text to
/// the system clipboard and commit a status line describing the outcome
/// (VAL-CHAT-023). No assistant message yet → the yellow
/// `[no assistant message to copy]` state; a copy failure → the red
/// `[copy failed: …]` banner. Shared verbatim by the slash path and the Ctrl+X
/// key path so both behave identically.
pub fn apply_copy_last_assistant(
    session: &AgentSession,
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
) {
    match last_assistant_text(session) {
        Some(body) => match crate::utils::clipboard::copy_to_clipboard(&body) {
            Ok(()) => commit_status(state, requester, "[copied to clipboard]"),
            Err(e) => commit_error(state, requester, &format!("[copy failed: {e}]")),
        },
        None => commit_status(state, requester, "[no assistant message to copy]"),
    }
}

/// `/copy N` — concatenate the text of the trailing `n` assistant messages
/// (chronological order) and copy them to the clipboard (VAL-CHAT-023). No
/// assistant messages → the yellow `[no assistant messages to copy]` state.
fn apply_copy_n(
    session: &AgentSession,
    n: usize,
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
) {
    let texts = last_n_assistant_texts(session, n);
    if texts.is_empty() {
        commit_status(state, requester, "[no assistant messages to copy]");
        return;
    }
    let body = texts.join("\n\n");
    match crate::utils::clipboard::copy_to_clipboard(&body) {
        Ok(()) => commit_status(
            state,
            requester,
            &format!(
                "[copied last {} assistant message(s) to clipboard]",
                texts.len()
            ),
        ),
        Err(e) => commit_error(state, requester, &format!("[copy failed: {e}]")),
    }
}

/// The trailing assistant message's textual body, joining its text blocks with
/// newlines. `None` when there is no assistant message, or the last one carries
/// only non-text content (image-only) — both are "nothing to copy".
fn last_assistant_text(session: &AgentSession) -> Option<String> {
    for msg in session.messages().iter().rev() {
        if let model::Message::Assistant(a) = msg {
            let parts: Vec<String> = a
                .content
                .iter()
                .filter_map(|block| match block {
                    model::AssistantContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect();
            if parts.is_empty() {
                return None;
            }
            return Some(parts.join("\n"));
        }
    }
    None
}

/// Up to `n` trailing assistant messages' text content, oldest-first so callers
/// join them chronologically. Image-only messages are skipped (same contract as
/// [`last_assistant_text`]).
fn last_n_assistant_texts(session: &AgentSession, n: usize) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    let mut collected: Vec<String> = Vec::new();
    for msg in session.messages().iter().rev() {
        if collected.len() >= n {
            break;
        }
        if let model::Message::Assistant(a) = msg {
            let parts: Vec<String> = a
                .content
                .iter()
                .filter_map(|block| match block {
                    model::AssistantContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect();
            if !parts.is_empty() {
                collected.push(parts.join("\n"));
            }
        }
    }
    collected.reverse();
    collected
}

/// `/skills` body — a markdown bullet list of discovered skills, or a sensible
/// empty form when none are installed (VAL-CHAT-044 empty-state).
fn skills_body(session: &AgentSession) -> String {
    let skills = session.skills();
    if skills.is_empty() {
        return "_(no skills discovered)_".to_string();
    }
    let mut out = String::new();
    for skill in skills {
        out.push_str(&format!("- **{}** — {}\n", skill.name, skill.description));
    }
    out.trim_end().to_string()
}

/// `/extensions` body — a markdown bullet list of loaded extensions, or a
/// sensible empty form when none are loaded (VAL-CHAT-044 empty-state).
fn extensions_body(session: &AgentSession) -> String {
    let exts = session.extensions();
    if exts.is_empty() {
        return "_(no extensions loaded)_".to_string();
    }
    let mut out = String::new();
    for ext in exts {
        let manifest = ext.manifest();
        let desc = manifest.description.as_deref().unwrap_or("");
        if desc.is_empty() {
            out.push_str(&format!("- **{}** ({})\n", manifest.name, manifest.version));
        } else {
            out.push_str(&format!(
                "- **{}** ({}) — {desc}\n",
                manifest.name, manifest.version
            ));
        }
    }
    out.trim_end().to_string()
}

/// `/diagnostics` body — the diagnostics report rendered as a compact text block
/// (VAL-CHAT-044). Runs the same checks the RPC diagnostics handler does.
fn diagnostics_body() -> String {
    use crate::core::diagnostics::{DiagStatus, run_diagnostics};
    use std::fmt::Write;

    let report = run_diagnostics();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "ok={} warn={} error={}",
        report.ok_count(),
        report.warn_count(),
        report.error_count()
    );
    for check in &report.checks {
        let (status, detail) = match &check.status {
            DiagStatus::Ok => ("OK", String::new()),
            DiagStatus::Warn(msg) => ("WARN", msg.clone()),
            DiagStatus::Error(msg) => ("ERR", msg.clone()),
        };
        if detail.is_empty() {
            let _ = writeln!(out, "[{status}] {}", check.name);
        } else {
            let _ = writeln!(out, "[{status}] {} — {detail}", check.name);
        }
    }
    out.trim_end().to_string()
}

/// `/changelog` body — the parsed CHANGELOG.md (newest-first), or a sensible
/// empty form when no changelog is found (VAL-CHAT-030).
fn changelog_body() -> String {
    use crate::utils::changelog::parse_changelog_file;

    let entries = super::chrome::locate_changelog_file()
        .and_then(|p| parse_changelog_file(p).ok())
        .unwrap_or_default();
    if entries.is_empty() {
        return "_(no changelog entries found)_".to_string();
    }
    entries
        .iter()
        .map(|e| e.content.clone())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The yellow status line for a slash command whose handler is not yet on this
/// driver. This is the follow-up-feature seam: each arriving feature replaces
/// the matching [`apply_slash_action`] arm, shrinking what routes here.
fn unsupported(
    action: &SlashCommandAction,
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
) {
    commit_status(
        state,
        requester,
        &format!("[{action} is not available on this driver yet]"),
    );
}

/// Commit a yellow status block to scrollback and request a repaint.
fn commit_status(state: &Arc<Mutex<DriverState>>, requester: &FrameRequester, text: &str) {
    commit(state, requester, chat::status_lines_for(text));
}

/// Commit a red error block to scrollback and request a repaint.
fn commit_error(state: &Arc<Mutex<DriverState>>, requester: &FrameRequester, text: &str) {
    commit(state, requester, chat::error_lines(text));
}

/// Queue a finalized block and request a repaint. Empty blocks are dropped by
/// [`DriverState::queue_commit`], so a no-content status is silent.
fn commit(state: &Arc<Mutex<DriverState>>, requester: &FrameRequester, lines: Vec<Line<'static>>) {
    if lines.is_empty() {
        return;
    }
    lock_state(state).queue_commit(lines);
    requester.request_frame();
}

#[cfg(test)]
mod tests {
    use super::*;

    use hand_tui::rt::scheduler::FrameScheduler;
    use hand_tui::rt::view::TerminalSize;

    use crate::core::agent_session::AgentSession;

    // --- Test harness ------------------------------------------------------

    /// A test model with a real context window so the footer's context % is
    /// computed (a zero window yields `None`).
    fn test_model() -> model::Model {
        model::Model {
            id: "test-model".to_string(),
            name: "Test".to_string(),
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
        }
    }

    /// An in-memory session with the test model. The session-lifecycle commands
    /// operate on it exactly as they do on an on-disk session (id advances on
    /// clone, label sticks, reset drops the transcript); the on-disk file
    /// side-effects (`/export` JSONL, `/clone` store file) are the tmux
    /// validator's remit and are exercised there.
    fn test_session() -> AgentSession {
        AgentSession::in_memory_with_client(test_model(), vec![], model::Client::new())
    }

    /// A real [`FrameRequester`] over a no-op scheduler. `request_frame` only
    /// sends on a channel and silently tolerates a dead scheduler, so the
    /// handlers repaint without a running terminal. Built under the test's
    /// tokio runtime.
    fn test_requester() -> FrameRequester {
        let (requester, _handle) = FrameScheduler::spawn(|| Ok(()));
        requester
    }

    fn state() -> Arc<Mutex<DriverState>> {
        Arc::new(Mutex::new(DriverState::new(TerminalSize::new(80, 24))))
    }

    fn footer_of(session: &AgentSession, cwd: &Path) -> SharedFooter {
        Arc::new(Mutex::new(build_footer_view(
            session,
            cwd,
            TokenUsageSummary::default(),
        )))
    }

    /// The text of a committed line, concatenating its spans.
    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Every queued scrollback line, joined — a simple `contains` check for the
    /// status/error lines a handler committed.
    fn committed_text(state: &Arc<Mutex<DriverState>>) -> String {
        lock_state(state)
            .pending_commits
            .iter()
            .flatten()
            .map(text_of)
            .collect::<Vec<_>>()
            .join("\n")
    }

    // --- /clear (VAL-CHAT-024) --------------------------------------------

    #[tokio::test]
    async fn clear_wipes_screen_and_commits_status_without_touching_context() {
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        // Seed a usage total so we can prove /clear leaves it untouched (the
        // footer context %/usage must not move — only the screen clears).
        lock_state(&state).usage = TokenUsageSummary {
            input: 1234,
            output: 567,
            ..TokenUsageSummary::default()
        };
        let before_ctx = lock_footer(&footer).context_percent;

        let outcome = dispatch_slash("/clear", &mut session, cwd, &state, &footer, &requester);

        assert_eq!(outcome, SlashOutcome::Continue);
        // A raw screen+scrollback wipe was queued.
        assert!(
            lock_state(&state)
                .pending_raw
                .contains(&CLEAR_SCREEN_AND_SCROLLBACK),
            "clear must queue the screen+scrollback wipe escape"
        );
        // The status line landed.
        assert!(
            committed_text(&state).contains("[chat cleared]"),
            "expected [chat cleared], got: {}",
            committed_text(&state)
        );
        // Context is untouched: the usage accumulator and the footer % are the
        // same as before (only the screen cleared, not the session).
        assert_eq!(
            lock_state(&state).usage.input,
            1234,
            "usage must survive /clear"
        );
        assert_eq!(
            lock_footer(&footer).context_percent,
            before_ctx,
            "footer context % must not move on /clear"
        );
    }

    // --- /new (VAL-CHAT-025) ----------------------------------------------

    #[tokio::test]
    async fn new_session_resets_transcript_and_rebases_footer() {
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        // Seed a message so message_count is non-zero before /new.
        session
            .session_manager_mut()
            .append_message(model::Message::User(model::UserMessage::new_text("hi")))
            .unwrap();
        assert_eq!(session.message_count(), 1);

        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();
        // Pretend prior spend accrued so we can prove it resets to baseline.
        lock_state(&state).usage = TokenUsageSummary {
            input: 9_999,
            ..TokenUsageSummary::default()
        };

        let outcome = dispatch_slash("/new", &mut session, cwd, &state, &footer, &requester);

        assert_eq!(outcome, SlashOutcome::Continue);
        assert_eq!(session.message_count(), 0, "/new drops the transcript");
        assert!(
            committed_text(&state).contains("[new session started]"),
            "got: {}",
            committed_text(&state)
        );
        // Footer rebased to the empty-session baseline: usage reset, context %
        // reflects an empty transcript.
        assert_eq!(lock_state(&state).usage.input, 0, "usage resets on /new");
        assert_eq!(
            lock_footer(&footer).context_percent,
            Some(0.0),
            "empty session's context % is the baseline"
        );
        // A screen wipe was queued so the fresh session starts clean.
        assert!(
            lock_state(&state)
                .pending_raw
                .contains(&CLEAR_SCREEN_AND_SCROLLBACK)
        );
    }

    // --- /session (VAL-CHAT-026) ------------------------------------------

    #[tokio::test]
    async fn session_info_reports_id_model_and_thinking() {
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        dispatch_slash("/session", &mut session, cwd, &state, &footer, &requester);

        let out = committed_text(&state);
        assert!(out.contains("Session:"), "missing id line: {out}");
        assert!(out.contains("Model: test-model"), "missing model: {out}");
        assert!(out.contains("(anthropic)"), "missing provider: {out}");
        assert!(
            out.contains("Thinking: off"),
            "missing thinking level: {out}"
        );
        assert!(out.contains("Messages: 0"), "missing message count: {out}");
    }

    #[tokio::test]
    async fn session_info_reflects_a_prior_name() {
        // VAL-CHAT-043: after /name, /session shows the new label.
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        dispatch_slash(
            "/name my-project",
            &mut session,
            cwd,
            &state,
            &footer,
            &requester,
        );
        // Fresh commit buffer for the /session read.
        lock_state(&state).take_commits();
        dispatch_slash("/session", &mut session, cwd, &state, &footer, &requester);

        assert!(
            committed_text(&state).contains("Label: my-project"),
            "/session must reflect the /name label, got: {}",
            committed_text(&state)
        );
    }

    // --- /name (VAL-CHAT-043) ---------------------------------------------

    #[tokio::test]
    async fn name_sets_label_and_updates_footer() {
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        dispatch_slash(
            "/name my label",
            &mut session,
            cwd,
            &state,
            &footer,
            &requester,
        );

        assert_eq!(session.label(), Some("my label"));
        assert_eq!(
            lock_footer(&footer).session_name.as_deref(),
            Some("my label")
        );
        assert!(
            committed_text(&state).contains("[session name set: my label]"),
            "got: {}",
            committed_text(&state)
        );
    }

    #[tokio::test]
    async fn bare_name_yields_usage_hint_from_parsing_layer() {
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        dispatch_slash("/name", &mut session, cwd, &state, &footer, &requester);

        assert_eq!(session.label(), None, "bare /name must not set a label");
        assert!(
            committed_text(&state).contains("/name"),
            "expected a usage hint, got: {}",
            committed_text(&state)
        );
    }

    // --- /clone (VAL-CHAT-042) --------------------------------------------

    #[tokio::test]
    async fn clone_advances_session_id_and_confirms() {
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();
        let original_id = session.session_id().to_string();

        dispatch_slash("/clone", &mut session, cwd, &state, &footer, &requester);

        assert_ne!(
            session.session_id(),
            original_id,
            "/clone must fork a fresh id"
        );
        let out = committed_text(&state);
        assert!(out.contains("cloned session"), "got: {out}");
        assert!(
            out.contains(session.session_id()),
            "must name the new id: {out}"
        );
    }

    // --- /export (VAL-CHAT-040) -------------------------------------------

    #[tokio::test]
    async fn export_html_writes_the_file_and_confirms() {
        let dir = tempfile::TempDir::new().unwrap();
        let output = dir.path().join("session.html");
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        apply_slash_action(
            SlashCommandAction::Export(output.clone(), ExportFormat::Html),
            &mut session,
            cwd,
            &state,
            &footer,
            &requester,
        );

        assert!(output.exists(), "HTML export must write the file");
        assert!(
            committed_text(&state).contains("exported to"),
            "got: {}",
            committed_text(&state)
        );
    }

    #[tokio::test]
    async fn export_refuses_to_overwrite_an_existing_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let output = dir.path().join("existing.html");
        std::fs::write(&output, "KEEP-ME").unwrap();
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        apply_slash_action(
            SlashCommandAction::Export(output.clone(), ExportFormat::Html),
            &mut session,
            cwd,
            &state,
            &footer,
            &requester,
        );

        assert_eq!(std::fs::read_to_string(&output).unwrap(), "KEEP-ME");
        assert!(
            committed_text(&state).contains("already exists"),
            "got: {}",
            committed_text(&state)
        );
    }

    #[tokio::test]
    async fn export_jsonl_on_in_memory_session_writes_nothing_and_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let output = dir.path().join("session.jsonl");
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        apply_slash_action(
            SlashCommandAction::Export(output.clone(), ExportFormat::Jsonl),
            &mut session,
            cwd,
            &state,
            &footer,
            &requester,
        );

        assert!(!output.exists(), "no file for an in-memory JSONL export");
        assert!(
            committed_text(&state).contains("in-memory"),
            "got: {}",
            committed_text(&state)
        );
    }

    #[tokio::test]
    async fn export_unknown_extension_writes_nothing() {
        // The parsing layer resolves an unknown extension to a ShowText usage
        // hint, so dispatching `/export foo.xyz` writes no file.
        let dir = tempfile::TempDir::new().unwrap();
        let output = dir.path().join("session.xyz");
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        let line = format!("/export {}", output.display());
        dispatch_slash(&line, &mut session, cwd, &state, &footer, &requester);

        assert!(!output.exists(), "an unknown extension must write nothing");
        assert!(
            committed_text(&state).contains("unsupported extension"),
            "got: {}",
            committed_text(&state)
        );
    }

    // --- /import (VAL-CHAT-041) -------------------------------------------

    #[tokio::test]
    async fn import_missing_file_errors_without_switching() {
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();
        let original_id = session.session_id().to_string();

        dispatch_slash(
            "/import /tmp/definitely-not-here-xyz.jsonl",
            &mut session,
            cwd,
            &state,
            &footer,
            &requester,
        );

        assert_eq!(
            session.session_id(),
            original_id,
            "no switch on a missing file"
        );
        assert!(
            committed_text(&state).contains("not found"),
            "got: {}",
            committed_text(&state)
        );
    }

    #[tokio::test]
    async fn bare_import_yields_usage_hint() {
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        dispatch_slash("/import", &mut session, cwd, &state, &footer, &requester);

        assert!(
            committed_text(&state).contains("/import"),
            "expected a usage hint, got: {}",
            committed_text(&state)
        );
    }

    // --- Dispatch plumbing -------------------------------------------------

    #[test]
    fn is_slash_command_matches_commands_but_not_bare_slash_or_text() {
        assert!(is_slash_command("/clear"));
        assert!(is_slash_command("/name foo"));
        assert!(!is_slash_command("hello"));
        assert!(!is_slash_command("/"));
        assert!(!is_slash_command("/   "));
    }

    #[tokio::test]
    async fn unknown_command_commits_a_hint_and_continues() {
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        let outcome = dispatch_slash(
            "/totally-unknown-xyz",
            &mut session,
            cwd,
            &state,
            &footer,
            &requester,
        );

        assert_eq!(outcome, SlashOutcome::Continue);
        // The prompt names the mistyped command and points at /help
        // (VAL-CHAT-006).
        let out = committed_text(&state);
        assert!(
            out.contains("Unknown command: /totally-unknown-xyz"),
            "must name the mistyped command: {out}"
        );
        assert!(out.contains("/help"), "must point the user at /help: {out}");
    }

    #[tokio::test]
    async fn unimplemented_command_routes_through_the_unsupported_seam() {
        // A recognised command whose handler is not yet on this driver (e.g.
        // /model → OpenModelSelector) commits the "not available yet" seam line
        // rather than panicking.
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        dispatch_slash("/settings", &mut session, cwd, &state, &footer, &requester);

        assert!(
            committed_text(&state).contains("not available on this driver yet"),
            "got: {}",
            committed_text(&state)
        );
    }

    // --- Test helpers for the info commands -------------------------------

    /// An assistant message carrying `text`.
    fn assistant_message(text: &str) -> model::Message {
        use model::types::{
            Api, AssistantContentBlock, AssistantMessage, Provider, StopReason, TextContent, Usage,
        };
        model::Message::Assistant(AssistantMessage {
            role: "assistant".to_string(),
            content: vec![AssistantContentBlock::Text(TextContent::new(text))],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: "test-model".to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        })
    }

    /// Seed the session's context messages with the given assistant texts (in
    /// order), so the copy commands — which read `session.messages()` — have
    /// something to read.
    fn seed_assistants(session: &mut AgentSession, texts: &[&str]) {
        let msgs: Vec<model::Message> = texts.iter().map(|t| assistant_message(t)).collect();
        session.set_messages(msgs);
    }

    // --- /help (VAL-CHAT-006) ---------------------------------------------

    #[tokio::test]
    async fn help_commits_the_command_list_text() {
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        dispatch_slash("/help", &mut session, cwd, &state, &footer, &requester);

        let out = committed_text(&state);
        assert!(out.contains("/quit"), "help must list /quit: {out}");
        assert!(out.contains("/help"), "help must list /help: {out}");
        assert!(out.contains("/copy"), "help must list /copy: {out}");
        assert!(out.contains("/compact"), "help must list /compact: {out}");
    }

    // --- /copy + Ctrl+X two-state + /copy N (VAL-CHAT-023) ----------------

    #[tokio::test]
    async fn copy_with_no_assistant_message_reports_the_empty_state() {
        // Headless copy contract: with no assistant message, the yellow
        // "[no assistant message to copy]" state lands regardless of whether a
        // clipboard exists — the empty-state check runs before any transport.
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        dispatch_slash("/copy", &mut session, cwd, &state, &footer, &requester);

        assert!(
            committed_text(&state).contains("[no assistant message to copy]"),
            "got: {}",
            committed_text(&state)
        );
    }

    /// With an assistant message present, the copy handler reads its text and
    /// hands it to the transport — it is *not* the empty state. The actual
    /// clipboard round-trip (native / OSC 52) is exercised by the tmux validator
    /// under HOME/HAND_HOME isolation; a unit test must not touch the real
    /// system clipboard (`arboard` off the main thread is a known macOS flake /
    /// SIGSEGV risk in the parallel test runner), so we assert the *pre-transport*
    /// decision through the pure collector instead.
    #[tokio::test]
    async fn copy_with_an_assistant_message_is_not_the_empty_state() {
        let mut session = test_session();
        seed_assistants(&mut session, &["the answer is 42"]);
        // The copy handler branches on this: `Some` → copy attempt, `None` →
        // the yellow empty state. A seeded message resolves to `Some`, so the
        // empty state is never taken.
        assert_eq!(
            last_assistant_text(&session).as_deref(),
            Some("the answer is 42"),
            "a seeded assistant message must resolve to copyable text"
        );
    }

    #[tokio::test]
    async fn copy_n_with_no_messages_reports_the_empty_state() {
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        dispatch_slash("/copy 3", &mut session, cwd, &state, &footer, &requester);

        assert!(
            committed_text(&state).contains("[no assistant messages to copy]"),
            "got: {}",
            committed_text(&state)
        );
    }

    /// `/copy N` collects the trailing N assistant texts oldest-first, capped at
    /// N — the pure decision the copy handler feeds to the clipboard. Unit-tested
    /// through the collector so no real clipboard is touched (see the note on
    /// `copy_with_an_assistant_message_is_not_the_empty_state`).
    #[test]
    fn copy_n_collects_the_trailing_n_assistant_texts_oldest_first() {
        let mut session = test_session();
        seed_assistants(&mut session, &["one", "two", "three"]);

        // /copy 2 → the last two, chronological.
        assert_eq!(
            last_n_assistant_texts(&session, 2),
            vec!["two".to_string(), "three".to_string()]
        );
        // /copy N with N larger than the count collects everything available.
        assert_eq!(
            last_n_assistant_texts(&session, 10),
            vec!["one".to_string(), "two".to_string(), "three".to_string()]
        );
        // Bare /copy resolves to the last one.
        assert_eq!(last_assistant_text(&session).as_deref(), Some("three"));
        // /copy 0 collects nothing (the parsing layer folds it to bare /copy).
        assert!(last_n_assistant_texts(&session, 0).is_empty());
    }

    // --- /skills /extensions /diagnostics /changelog (VAL-CHAT-044/030) ---

    #[tokio::test]
    async fn skills_renders_a_labelled_box_with_an_empty_form() {
        // The test session discovers no skills, so the box shows the empty form.
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        dispatch_slash("/skills", &mut session, cwd, &state, &footer, &requester);

        let out = committed_text(&state);
        assert!(out.contains("[skills]"), "skills box label: {out}");
        assert!(
            out.contains("no skills discovered"),
            "empty-state form: {out}"
        );
    }

    #[tokio::test]
    async fn extensions_renders_a_labelled_box_with_an_empty_form() {
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        dispatch_slash(
            "/extensions",
            &mut session,
            cwd,
            &state,
            &footer,
            &requester,
        );

        let out = committed_text(&state);
        assert!(out.contains("[extensions]"), "extensions box label: {out}");
        assert!(
            out.contains("no extensions loaded"),
            "empty-state form: {out}"
        );
    }

    #[tokio::test]
    async fn diagnostics_renders_a_labelled_box_with_the_report() {
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        dispatch_slash(
            "/diagnostics",
            &mut session,
            cwd,
            &state,
            &footer,
            &requester,
        );

        let out = committed_text(&state);
        assert!(
            out.contains("[diagnostics]"),
            "diagnostics box label: {out}"
        );
        // The report always carries the ok/warn/error counts line.
        assert!(out.contains("ok="), "diagnostics counts line: {out}");
    }

    #[tokio::test]
    async fn changelog_renders_a_labelled_box() {
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        dispatch_slash("/changelog", &mut session, cwd, &state, &footer, &requester);

        // The box label is present regardless of whether a CHANGELOG.md is
        // found (the empty form still renders a `[changelog]` box).
        assert!(
            committed_text(&state).contains("[changelog]"),
            "changelog box label: {}",
            committed_text(&state)
        );
    }

    // --- /compact interception (VAL-CHAT-027) -----------------------------

    // --- /model selector interception (VAL-OVERLAY-*) ---------------------

    #[test]
    fn is_open_model_selector_matches_bare_model_and_models_only() {
        // Bare `/model` (and its `/models` alias) opens the selector overlay.
        assert!(is_open_model_selector("/model"));
        assert!(is_open_model_selector("/models"));
        assert!(is_open_model_selector("/model   "));
        // `/model <pattern>` carries args → a non-interactive switch, not the
        // selector; it routes through the sync ModelByPattern dispatch instead.
        assert!(!is_open_model_selector("/model sonnet"));
        assert!(!is_open_model_selector("/model anthropic/claude"));
        // Unrelated commands and plain text never open the selector.
        assert!(!is_open_model_selector("/help"));
        assert!(!is_open_model_selector("model"));
        assert!(!is_open_model_selector("/"));
    }

    // --- /resume picker interception (VAL-OVERLAY-010) --------------------

    #[test]
    fn is_open_resume_picker_matches_resume_only() {
        assert!(is_open_resume_picker("/resume"));
        assert!(is_open_resume_picker("/resume   "));
        // Unrelated commands and plain text never open the picker.
        assert!(!is_open_resume_picker("/help"));
        assert!(!is_open_resume_picker("/model"));
        assert!(!is_open_resume_picker("resume"));
        assert!(!is_open_resume_picker("/"));
    }

    // --- picker-selector interception (/tree /scoped-models /fork) --------

    #[test]
    fn picker_selector_action_routes_the_three_pickers() {
        use crate::modes::interactive::slash_commands::SlashCommandAction;

        // `/tree` (bare and with a subdir) routes to the tree picker.
        assert!(matches!(
            picker_selector_action("/tree"),
            Some(SlashCommandAction::OpenTreeSelector(None))
        ));
        assert!(matches!(
            picker_selector_action("/tree src"),
            Some(SlashCommandAction::OpenTreeSelector(Some(_)))
        ));
        // `/scoped-models` (and its underscore alias) routes to the multi-select.
        assert!(matches!(
            picker_selector_action("/scoped-models"),
            Some(SlashCommandAction::OpenScopedModelsSelector)
        ));
        assert!(matches!(
            picker_selector_action("/scoped_models"),
            Some(SlashCommandAction::OpenScopedModelsSelector)
        ));
        // `/fork` (bare and with an entry id) routes to the fork picker.
        assert!(matches!(
            picker_selector_action("/fork"),
            Some(SlashCommandAction::Fork(None))
        ));
        assert!(matches!(
            picker_selector_action("/fork e123"),
            Some(SlashCommandAction::Fork(Some(_)))
        ));
        // Unrelated commands never route to the picker family.
        assert!(picker_selector_action("/help").is_none());
        assert!(picker_selector_action("/model").is_none());
        assert!(picker_selector_action("plain text").is_none());
    }

    #[test]
    fn parse_compact_recognises_the_command_and_its_steering_text() {
        assert_eq!(parse_compact("/compact"), Some(None));
        assert_eq!(
            parse_compact("/compact focus on the schema"),
            Some(Some("focus on the schema".to_string()))
        );
        // Not a compact command.
        assert_eq!(parse_compact("/help"), None);
        assert_eq!(parse_compact("compact"), None);
    }
}
