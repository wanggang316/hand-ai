//! Driver-side glue that opens selector overlays against the live session.
//!
//! The [overlay runtime](super::overlay) supplies the generic mount/dispatch/close
//! machinery; this module supplies the *session-aware* half for each selector: it
//! reads the inputs the selector needs off the [`AgentSession`], mounts the
//! component, awaits its single outcome, and applies the result. The `/model`
//! selector is the first; the follow-up selector family adds one `open_*` function
//! here per command, all reusing the same runtime.
//!
//! # Concurrency
//!
//! An `open_*` runs on the **turn-runner task** (the one place that owns
//! `&mut AgentSession`), so it can `await` the outcome channel and then apply the
//! pick (`session.set_model`) directly. While it awaits, the **input loop** — a
//! separate task sharing the [`SharedOverlay`] — routes keys into the mounted
//! selector, so the user drives the dialog and the runner wakes on the outcome. The
//! turn runner is otherwise blocked here, which is correct: a modal selector owns
//! the interaction until it closes, and any streaming turn keeps running on its own
//! task underneath (VAL-OVERLAY-009).

use std::path::Path;
use std::sync::{Arc, Mutex};

use hand_tui::rt::scheduler::FrameRequester;
use model::Model;
use tokio::sync::mpsc;

use crate::core::agent_session::AgentSession;
use crate::core::session_manager::{SessionInfo, SessionManager};

use std::sync::atomic::Ordering;

use super::chat;
use super::footer::{TokenUsageSummary, build_footer_view};
use super::model_selector::{ModelOutcome, ModelSelector};
use super::overlay::{self, DoneSignal, SelectorController, SharedOverlay};
use super::replay::replay_blocks;
use super::session_picker::{SessionOutcome, SessionPicker};
use super::state::{DriverState, SharedFooter, lock_footer, lock_state};

/// Open the `/model` selector overlay and apply the user's pick.
///
/// Builds the registry's full model list and the user's scoped subset (from
/// `enabled_models`), mounts the [`ModelSelector`] as a centered modal dialog, then
/// awaits its single outcome:
///
/// - **Selected** — `session.set_model(model)` switches the model (and journals the
///   change so a resume keeps it), the footer rebuilds so the model segment
///   updates, and the `[model set to <id>]` status line lands (VAL-OVERLAY-003).
/// - **Cancelled** — nothing changes; the `[model selection cancelled]` status line
///   lands so the cancel is visible.
///
/// The await resolves as soon as the input loop feeds the selector its Enter/Esc;
/// if the channel closes without an outcome (a teardown mid-dialog), it returns
/// quietly, leaving the model unchanged.
pub async fn open_model_selector(
    session: &mut AgentSession,
    cwd: &Path,
    overlay: &SharedOverlay,
    done: &DoneSignal,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
) {
    let all_models = session.model_registry().all().to_vec();
    let scoped_models = resolve_scoped_models(session, &all_models);
    let current = session.model().clone();

    let (tx, mut rx) = mpsc::unbounded_channel::<ModelOutcome>();
    // Reset the shared done flag before mounting: it is the runtime's "overlay
    // finished" latch, cleared per open so a prior selector's raise never leaks into
    // this one. The selector raises it on its terminal key; the input loop reads
    // this same flag to close the overlay.
    done.store(false, Ordering::SeqCst);
    let selector = ModelSelector::new(Some(current), all_models, scoped_models, tx, done.clone());
    let controller: Arc<Mutex<dyn SelectorController>> = Arc::new(Mutex::new(selector));

    overlay::mount(overlay, requester, controller, done.clone());

    // Await the selector's single outcome. The input loop drives the dialog and
    // closes it (pops the overlay) once the user confirms/cancels; here we react to
    // the value it emitted on the way out.
    match rx.recv().await {
        Some(ModelOutcome::Selected(model)) => {
            let id = model.id.clone();
            session.set_model(*model);
            refresh_footer(session, cwd, state, footer, requester);
            commit_status(state, requester, &format!("[model set to {id}]"));
        }
        Some(ModelOutcome::Cancelled) => {
            commit_status(state, requester, "[model selection cancelled]");
        }
        // Channel closed with no outcome (teardown mid-dialog): leave the model as
        // is and make sure any lingering overlay is cleared.
        None => overlay::close(overlay, requester),
    }
}

/// Open the `/resume` session picker overlay and, on a pick, switch to and replay
/// the chosen session (VAL-OVERLAY-010 / VAL-CHAT-012 / VAL-CHAT-032).
///
/// Lists the resumable sessions in `cwd` (backend-aware), mounts the
/// [`SessionPicker`] as a centered modal dialog, then awaits its single outcome:
///
/// - **Selected** — resolve the session (`switch_session` by path under jsonl,
///   `switch_session_by_id` under sqlite where every session shares one database
///   path), clear the screen so the replayed transcript starts clean, replay the
///   loaded messages into scrollback in order (closed by the `[resumed: …]` marker),
///   and refresh the footer so the resumed session's context %/label surface.
/// - **Cancelled** — nothing is resumed; the yellow `[resume cancelled]` status
///   line lands so the cancel is visible (VAL-CHAT-032).
///
/// An empty list still mounts the picker (showing `(no sessions)`); it stays open
/// until the user presses Esc, which cancels here.
pub async fn open_resume_picker(
    session: &mut AgentSession,
    cwd: &Path,
    overlay: &SharedOverlay,
    done: &DoneSignal,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
) {
    let sessions = list_resumable_sessions(session, cwd);

    let (tx, mut rx) = mpsc::unbounded_channel::<SessionOutcome>();
    // Reset the shared done flag before mounting (the runtime's "overlay finished"
    // latch, cleared per open so a prior selector's raise never leaks in).
    done.store(false, Ordering::SeqCst);
    let picker = SessionPicker::new(sessions, tx, done.clone());
    let controller: Arc<Mutex<dyn SelectorController>> = Arc::new(Mutex::new(picker));

    overlay::mount(overlay, requester, controller, done.clone());

    match rx.recv().await {
        Some(SessionOutcome::Selected { id, path }) => {
            resume_selected(session, cwd, &id, &path, state, footer, requester);
        }
        Some(SessionOutcome::Cancelled) => {
            commit_status(state, requester, "[resume cancelled]");
        }
        // Channel closed with no outcome (teardown mid-dialog): leave the session
        // as is and clear any lingering overlay.
        None => overlay::close(overlay, requester),
    }
}

/// Switch to the picked session and replay its transcript into scrollback.
///
/// Under sqlite every session shares one database path, so the id is the selector;
/// under jsonl the path addresses the session file. On success the screen is
/// cleared, the loaded messages are replayed in order (each as one scrollback
/// block, closed by the `[resumed: …]` marker), and the footer is rebuilt. A switch
/// failure takes the red-banner route and nothing is replayed.
fn resume_selected(
    session: &mut AgentSession,
    cwd: &Path,
    id: &str,
    path: &Path,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
) {
    use crate::core::session_manager::SessionBackend;

    let result = match session.session_backend() {
        SessionBackend::Sqlite => session.switch_session_by_id(id),
        SessionBackend::Jsonl => session.switch_session(path),
    };
    match result {
        Ok(()) => {
            // Clear the screen so the replayed transcript starts on a fresh screen,
            // matching the legacy driver's "clear the chat list on resume".
            lock_state(state).queue_raw("\x1b[3J\x1b[2J\x1b[H");
            // Reset the running usage accumulator: the resumed session's spend is
            // rebuilt from its own footer, not the prior session's totals.
            lock_state(state).usage = TokenUsageSummary::default();
            replay_into_scrollback(session, id, state, requester);
            refresh_footer(session, cwd, state, footer, requester);
        }
        Err(e) => {
            commit_status(state, requester, &format!("[resume failed: {e}]"));
        }
    }
}

/// Replay the active session's transcript into scrollback in order, closed by the
/// `[resumed: <label>]` marker. Each message becomes one queued scrollback block, so
/// the replayed transcript lands in message order and the marker last. Also seeds
/// the assistant-history so a later global Ctrl+T re-render includes the resumed
/// messages.
fn replay_into_scrollback(
    session: &AgentSession,
    fallback_label: &str,
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
) {
    let label = session
        .label()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| short_id(fallback_label));
    let messages = session.messages().to_vec();

    let mut guard = lock_state(state);
    let width = guard.size.cols;
    let hide_thinking = guard.hide_thinking;
    let blocks = replay_blocks(&messages, &label, hide_thinking, width);
    for block in blocks {
        guard.queue_commit(block);
    }
    // Seed assistant history so Ctrl+T re-renders the resumed assistant messages too.
    for message in &messages {
        if let model::Message::Assistant(a) = message {
            guard.remember_assistant(a.clone());
        }
    }
    drop(guard);
    requester.request_frame();
}

/// The first 8 chars of a session id, used as a compact resume label when the
/// session carries no name.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// List the resumable sessions in `cwd`, backend-aware, tolerating a listing
/// failure with an empty list (the picker then shows `(no sessions)` rather than
/// aborting the resume flow).
#[must_use]
pub fn list_resumable_sessions(session: &AgentSession, cwd: &Path) -> Vec<SessionInfo> {
    SessionManager::list_with_backend(session.session_backend(), cwd).unwrap_or_default()
}

/// Resolve the user's scoped model subset from `settings.enabled_models`, matching
/// each configured pattern against the registry.
///
/// `enabled_models` is a list of patterns (`provider/id`, a bare id, or a name
/// fragment); each pattern selects the registry models it matches, de-duplicated
/// and kept in registry order. An unset (or empty-after-resolution) `enabled_models`
/// yields an empty subset — the selector then disables the Tab scope toggle and
/// opens on the full list. Kept as a pure function over `(session settings, full
/// list)` so the scoping rule is unit-testable without a running overlay.
#[must_use]
pub fn resolve_scoped_models(session: &AgentSession, all_models: &[Model]) -> Vec<Model> {
    let Some(patterns) = session.settings().current().enabled_models.clone() else {
        return Vec::new();
    };
    scoped_from_patterns(&patterns, all_models)
}

/// Select the registry models matching any of `patterns` (case-insensitive over
/// `provider/id`, `id`, and `name`), de-duplicated and kept in `all_models` order.
///
/// Pulled out from [`resolve_scoped_models`] so the pattern-matching rule is tested
/// directly against a fixed list, without touching settings.
#[must_use]
pub fn scoped_from_patterns(patterns: &[String], all_models: &[Model]) -> Vec<Model> {
    let needles: Vec<String> = patterns
        .iter()
        .map(|p| p.trim().to_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    if needles.is_empty() {
        return Vec::new();
    }
    all_models
        .iter()
        .filter(|m| {
            let provider = m.provider.as_str().to_lowercase();
            let id = m.id.to_lowercase();
            let name = m.name.to_lowercase();
            let qualified = format!("{provider}/{id}");
            needles.iter().any(|needle| {
                qualified == *needle
                    || id == *needle
                    || id.contains(needle.as_str())
                    || name.contains(needle.as_str())
            })
        })
        .cloned()
        .collect()
}

/// Rebuild the footer view-model from current session state (model, context %,
/// usage) and request a repaint so the new fields show.
fn refresh_footer(
    session: &AgentSession,
    cwd: &Path,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
) {
    let usage: TokenUsageSummary = lock_state(state).usage;
    *lock_footer(footer) = build_footer_view(session, cwd, usage);
    requester.request_frame();
}

/// Commit a yellow status block to scrollback and request a repaint.
fn commit_status(state: &Arc<Mutex<DriverState>>, requester: &FrameRequester, text: &str) {
    let lines = chat::status_lines_for(text);
    if lines.is_empty() {
        return;
    }
    lock_state(state).queue_commit(lines);
    requester.request_frame();
}

#[cfg(test)]
mod tests {
    use super::*;

    use model::types::Provider;
    use model::{Api, Cost, InputType};

    fn make_model(provider: Provider, id: &str, name: &str) -> Model {
        Model {
            id: id.to_string(),
            name: name.to_string(),
            api: Api::AnthropicMessages,
            provider,
            base_url: String::new(),
            reasoning: false,
            input: vec![InputType::Text],
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 0,
            max_tokens: 0,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    fn catalog() -> Vec<Model> {
        vec![
            make_model(Provider::Anthropic, "claude-sonnet", "Claude Sonnet"),
            make_model(Provider::Anthropic, "claude-haiku", "Claude Haiku"),
            make_model(Provider::OpenAI, "gpt-4o", "GPT-4o"),
            make_model(Provider::Google, "gemini-2-pro", "Gemini 2 Pro"),
        ]
    }

    #[test]
    fn no_patterns_yields_an_empty_scope() {
        assert!(scoped_from_patterns(&[], &catalog()).is_empty());
        // Whitespace-only patterns are dropped, so they also yield an empty scope.
        assert!(scoped_from_patterns(&["  ".to_string()], &catalog()).is_empty());
    }

    #[test]
    fn a_qualified_pattern_selects_exactly_that_model() {
        let scoped = scoped_from_patterns(&["openai/gpt-4o".to_string()], &catalog());
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].id, "gpt-4o");
    }

    #[test]
    fn a_substring_pattern_selects_every_match_in_registry_order() {
        // "claude" matches both Anthropic models, kept in catalog order.
        let scoped = scoped_from_patterns(&["claude".to_string()], &catalog());
        assert_eq!(scoped.len(), 2);
        assert_eq!(scoped[0].id, "claude-sonnet");
        assert_eq!(scoped[1].id, "claude-haiku");
    }

    #[test]
    fn multiple_patterns_union_without_duplicates() {
        let scoped = scoped_from_patterns(
            &["claude-sonnet".to_string(), "gpt-4o".to_string()],
            &catalog(),
        );
        assert_eq!(scoped.len(), 2);
        let ids: Vec<&str> = scoped.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["claude-sonnet", "gpt-4o"]);
    }

    #[test]
    fn matching_is_case_insensitive() {
        let scoped = scoped_from_patterns(&["GPT-4O".to_string()], &catalog());
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].id, "gpt-4o");
    }
}
