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
            commit_status(
                state,
                requester,
                &format!("[unknown command: /{}]", parsed.name),
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

        // --- Always-safe leaves -------------------------------------------
        // ShowText carries a usage / error string the parsing layer already
        // built (e.g. `/import` with no arg, `/export` unknown extension).
        SlashCommandAction::ShowText(text) => commit_status(state, requester, &text),
        SlashCommandAction::Quit => return SlashOutcome::Quit,
        SlashCommandAction::Noop => {}

        // --- Follow-up feature seam ---------------------------------------
        // Info commands (/help, /copy, /compact, /skills, …) and selector
        // commands (/model, /theme, /thinking, …) land here as their features
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
        assert!(
            committed_text(&state).contains("unknown command"),
            "got: {}",
            committed_text(&state)
        );
    }

    #[tokio::test]
    async fn unimplemented_command_routes_through_the_unsupported_seam() {
        // A recognised command whose handler is not yet on this driver (e.g.
        // /help → ShowText is implemented, but /skills → ListSkills is not)
        // commits the "not available yet" seam line rather than panicking.
        let mut session = test_session();
        let cwd = Path::new("/tmp");
        let state = state();
        let footer = footer_of(&session, cwd);
        let requester = test_requester();

        dispatch_slash("/skills", &mut session, cwd, &state, &footer, &requester);

        assert!(
            committed_text(&state).contains("not available on this driver yet"),
            "got: {}",
            committed_text(&state)
        );
    }
}
