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

use hand_tui::rt::components::{SettingEntry, SettingValue};
use hand_tui::rt::scheduler::FrameRequester;
use model::{Model, ThinkingLevel};
use tokio::sync::mpsc;

use crate::core::agent_session::AgentSession;
use crate::core::session_manager::{SessionInfo, SessionManager};
use crate::core::settings::{Settings, SettingsScope, ThemeSetting, ThinkingLevelSetting};
use crate::modes::interactive::slash_commands::SlashCommandAction;

use std::sync::atomic::Ordering;

use super::chat;
use super::footer::{TokenUsageSummary, build_footer_view};
use super::keys::NavKeys;
use super::model_selector::{ModelOutcome, ModelSelector};
use super::overlay::{self, DoneSignal, SelectorController, SharedOverlay};
use super::replay::replay_blocks;
use super::scoped_models_selector::{
    SESSION_ONLY_NOTICE, ScopedModelsOutcome, ScopedModelsSelector,
};
use super::session_picker::{SessionOutcome, SessionPicker};
use super::settings_selector::{SettingsOutcome, SettingsSelector};
use super::state::{DriverState, SharedFooter, lock_footer, lock_state};
use super::summary::{CollapsibleSummary, SummaryKind};
use super::theme_selector::{ThemeOutcome, ThemeSelector, canonical_theme};
use super::thinking_selector::{ThinkingOutcome, ThinkingSelector, level_label, parse_level_arg};
use super::tree_selector::{TreeOutcome, TreeSelector, scan_tree};
use super::user_message_selector::{ForkItem, ForkOutcome, UserMessageSelector};

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
    // Re-snapshot the registry first so a catalog hot-swapped mid-session by
    // the background rolling-release refresh shows up in this list.
    // Synchronous local reload only — never the network.
    session.refresh_model_registry();
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

/// Open the `/thinking` selector overlay and apply the user's pick
/// (VAL-OVERLAY-025 / VAL-OVERLAY-026).
///
/// Mounts the [`ThinkingSelector`] ladder (cursor seeded to the current active
/// level), then awaits its single outcome:
///
/// - **Selected(level)** — `apply_thinking_level` sets the session's reasoning
///   level, rebuilds the footer so the `thinking …` segment updates, lands the
///   `[thinking: <label>]` status line, and — when the model is **not** a reasoning
///   model and the level is not "off" — a yellow warning that the level has no
///   effect (VAL-OVERLAY-026). Picking "off" never warns.
/// - **Cancelled** — nothing changes; the yellow `[thinking selection cancelled]`
///   line lands.
pub async fn open_thinking_selector(
    session: &mut AgentSession,
    cwd: &Path,
    overlay: &SharedOverlay,
    done: &DoneSignal,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
) {
    let current = session.stream_options().reasoning;

    let (tx, mut rx) = mpsc::unbounded_channel::<ThinkingOutcome>();
    done.store(false, Ordering::SeqCst);
    let selector = ThinkingSelector::new(current, tx, done.clone());
    let controller: Arc<Mutex<dyn SelectorController>> = Arc::new(Mutex::new(selector));

    overlay::mount(overlay, requester, controller, done.clone());

    match rx.recv().await {
        Some(ThinkingOutcome::Selected(level)) => {
            apply_thinking_level(session, cwd, level, state, footer, requester);
        }
        Some(ThinkingOutcome::Cancelled) => {
            commit_status(state, requester, "[thinking selection cancelled]");
        }
        None => overlay::close(overlay, requester),
    }
}

/// Set the session's reasoning level, refresh the footer, land the
/// `[thinking: <label>]` status line, and — for a non-reasoning model asked for a
/// non-off level — the yellow warning that the level has no effect
/// (VAL-OVERLAY-026). Shared by the selector and the `/thinking <level>` direct-arg
/// path so both behave identically.
pub fn apply_thinking_level(
    session: &mut AgentSession,
    cwd: &Path,
    level: Option<ThinkingLevel>,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
) {
    let is_reasoning = session.model().reasoning;

    let mut options = session.stream_options().clone();
    options.reasoning = level;
    session.set_stream_options(options);

    refresh_footer(session, cwd, state, footer, requester);
    commit_status(
        state,
        requester,
        &format!("[thinking: {}]", level_label(level)),
    );

    // A non-reasoning model still accepts a level, but it has no effect — surface a
    // yellow warning. Picking "off" (the `None` level) is always warning-free.
    if !is_reasoning && level.is_some() {
        commit_status(
            state,
            requester,
            "warning: this model is not a reasoning model; the thinking level has no effect",
        );
    }
}

/// Open the `/theme` selector overlay and, on a pick, persist it (VAL-OVERLAY-014).
///
/// Mounts the [`ThemeSelector`] (current theme checkmarked), then awaits its single
/// outcome:
///
/// - **Selected(name)** — `apply_theme` persists the theme to settings and lands
///   the `[theme: <name>] saved; restart to apply` status line. There is **no live
///   preview** (Decision Log parity): the palette changes on the next launch.
/// - **Cancelled** — the yellow `[theme selection cancelled]` line lands.
pub async fn open_theme_selector(
    session: &mut AgentSession,
    overlay: &SharedOverlay,
    done: &DoneSignal,
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
) {
    let current = theme_setting_id(&session.settings().current().theme()).to_string();

    let (tx, mut rx) = mpsc::unbounded_channel::<ThemeOutcome>();
    done.store(false, Ordering::SeqCst);
    let selector = ThemeSelector::new(current, tx, done.clone());
    let controller: Arc<Mutex<dyn SelectorController>> = Arc::new(Mutex::new(selector));

    overlay::mount(overlay, requester, controller, done.clone());

    match rx.recv().await {
        Some(ThemeOutcome::Selected(name)) => {
            apply_theme(session, &name, state, requester);
        }
        Some(ThemeOutcome::Cancelled) => {
            commit_status(state, requester, "[theme selection cancelled]");
        }
        None => overlay::close(overlay, requester),
    }
}

/// Persist `name` as the theme setting (Global scope) and land the
/// `saved; restart to apply` status line. Shared by the selector and the
/// `/theme <name>` direct-arg path. A `name` that is not a persistable theme lands
/// the red `[theme: unknown theme …]` guidance instead (VAL-OVERLAY-018).
pub fn apply_theme(
    session: &mut AgentSession,
    name: &str,
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
) {
    let Some(canonical) = canonical_theme(name) else {
        commit_error(
            state,
            requester,
            &format!("[theme: unknown theme \"{name}\"]"),
        );
        return;
    };
    let settings = session.settings_mut();
    match settings
        .apply_setting_by_id(SettingsScope::Global, "theme", canonical)
        .and_then(|_| settings.save(SettingsScope::Global))
    {
        Ok(()) => commit_status(
            state,
            requester,
            &format!("[theme: {canonical}] saved; restart to apply"),
        ),
        Err(e) => commit_error(state, requester, &format!("[theme failed: {e}]")),
    }
}

/// Open the `/settings` selector overlay (M2 [`SettingsList`]) and, on the first
/// change, persist it and close (VAL-OVERLAY-013 / VAL-OVERLAY-036 /
/// VAL-OVERLAY-004 exception).
///
/// Builds the entries from the **merged effective settings** — the three
/// `default_*` rows first so a project override is visible — mounts the
/// [`SettingsSelector`], then awaits its single outcome:
///
/// - **Changed{id, value}** — `apply_setting_by_id` + `save` persist the change to
///   the Global layer, the footer rebuilds (a thinking-level change reflects), and
///   the `[settings: <id> = <value>]` status line lands. The dialog is already
///   closed (the first change closes it).
/// - **Closed** — Esc lands the specific `[/settings closed]` line (not a generic
///   cancel — the VAL-OVERLAY-004 exception).
// One over the lint's ceiling: the resolved `nav` snapshot joins the existing
// overlay-mount + session-apply parameter set. Grouping them into a context struct
// would ripple through the whole selector-open family for no readability gain here.
#[allow(clippy::too_many_arguments)]
pub async fn open_settings_selector(
    session: &mut AgentSession,
    cwd: &Path,
    overlay: &SharedOverlay,
    done: &DoneSignal,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
    nav: NavKeys,
) {
    let entries = build_settings_entries(session.settings().current());

    let (tx, mut rx) = mpsc::unbounded_channel::<SettingsOutcome>();
    done.store(false, Ordering::SeqCst);
    let selector = SettingsSelector::with_nav(entries, tx, done.clone(), nav);
    let controller: Arc<Mutex<dyn SelectorController>> = Arc::new(Mutex::new(selector));

    overlay::mount(overlay, requester, controller, done.clone());

    match rx.recv().await {
        Some(SettingsOutcome::Changed { id, value }) => {
            apply_settings_change(session, cwd, &id, &value, state, footer, requester);
        }
        Some(SettingsOutcome::Closed) => {
            commit_status(state, requester, "[/settings closed]");
        }
        None => overlay::close(overlay, requester),
    }
}

/// Persist one settings change (Global scope), rebuild the footer, and land the
/// status line. A rejected write takes the red-banner route.
fn apply_settings_change(
    session: &mut AgentSession,
    cwd: &Path,
    id: &str,
    value: &str,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
) {
    let settings = session.settings_mut();
    let result = settings
        .apply_setting_by_id(SettingsScope::Global, id, value)
        .and_then(|_| settings.save(SettingsScope::Global));
    match result {
        Ok(()) => {
            // `show_images` flips the driver-side render gate live so the next tool
            // result honours the new value without a restart: off forces the
            // `[mime WxH]` placeholder even on a graphics terminal, on resumes the
            // graphics path (VAL-IMG-011).
            if id == "show_images" {
                lock_state(state)
                    .set_show_images(session.settings().current().terminal.show_images());
            }
            refresh_footer(session, cwd, state, footer, requester);
            commit_status(state, requester, &format!("[settings: {id} = {value}]"));
        }
        Err(e) => commit_error(state, requester, &format!("[settings failed: {e}]")),
    }
}

/// Build the `/settings` dialog entries from the **merged effective** settings.
///
/// The three `default_*` rows come first, rendered as their effective string values
/// (`(unset)` when unset) so a project override is visible (VAL-OVERLAY-036, the
/// issue #16 UAT regression); the editable toggles/enums follow. Every id here is
/// one [`apply_setting_by_id`](crate::core::settings::SettingsManager::apply_setting_by_id)
/// accepts, so a change always round-trips.
///
/// [`apply_setting_by_id`]: crate::core::settings::SettingsManager::apply_setting_by_id
fn build_settings_entries(merged: &Settings) -> Vec<SettingEntry> {
    let provider = merged
        .default_provider
        .clone()
        .unwrap_or_else(|| "(unset)".to_string());
    let model = merged
        .default_model
        .clone()
        .unwrap_or_else(|| "(unset)".to_string());
    let thinking = merged
        .default_thinking_level
        .map_or_else(|| "(unset)".to_string(), thinking_setting_id);

    vec![
        // The merged effective defaults, first, so a project override is visible.
        SettingEntry::new(
            "default_provider",
            SettingValue::String(provider),
            "Effective default provider (global + project merged).",
        ),
        SettingEntry::new(
            "default_model",
            SettingValue::String(model),
            "Effective default model (global + project merged).",
        ),
        SettingEntry::new(
            "default_thinking_level",
            SettingValue::String(thinking),
            "Effective default thinking level (global + project merged).",
        ),
        // Editable toggles / enums.
        SettingEntry::new(
            "theme",
            SettingValue::Enum {
                choices: super::theme_selector::THEME_NAMES
                    .iter()
                    .map(|t| (*t).to_string())
                    .collect(),
                selected: theme_setting_index(&merged.theme()),
            },
            "Color theme (applied on next launch).",
        ),
        SettingEntry::new(
            "auto_compact",
            SettingValue::Bool(merged.compaction.enabled.unwrap_or(true)),
            "Automatically compact context when it grows large.",
        ),
        SettingEntry::new(
            "hide_thinking_block",
            SettingValue::Bool(merged.hide_thinking_block.unwrap_or(false)),
            "Hide thinking blocks in the transcript.",
        ),
        SettingEntry::new(
            "show_images",
            SettingValue::Bool(merged.terminal.show_images()),
            "Render images inline (off forces text placeholders).",
        ),
        SettingEntry::new(
            "quiet_startup",
            SettingValue::Bool(merged.quiet_startup.unwrap_or(false)),
            "Suppress the startup chrome.",
        ),
    ]
}

/// The persistable id string for a [`ThemeSetting`] — the four built-in kebab
/// tags (`dark` / `light` / `high-contrast` / `system`) or a custom theme's
/// bare name.
fn theme_setting_id(theme: &ThemeSetting) -> &str {
    theme.as_tag()
}

/// The index of a [`ThemeSetting`] within
/// [`THEME_NAMES`](super::theme_selector::THEME_NAMES). A custom theme is not
/// in the built-in list, so it falls back to the first row.
fn theme_setting_index(theme: &ThemeSetting) -> usize {
    let id = theme_setting_id(theme);
    super::theme_selector::THEME_NAMES
        .iter()
        .position(|t| *t == id)
        .unwrap_or(0)
}

/// The persistable id string for a [`ThinkingLevelSetting`].
fn thinking_setting_id(level: ThinkingLevelSetting) -> String {
    match level {
        ThinkingLevelSetting::Off => "off",
        ThinkingLevelSetting::Minimal => "minimal",
        ThinkingLevelSetting::Low => "low",
        ThinkingLevelSetting::Medium => "medium",
        ThinkingLevelSetting::High => "high",
        ThinkingLevelSetting::Xhigh => "xhigh",
        ThinkingLevelSetting::Max => "max",
    }
    .to_string()
}

/// Apply a `/thinking <arg>` direct-argument submission (VAL-OVERLAY-017 /
/// VAL-OVERLAY-018): no dialog opens — a valid level (or an off variant) is applied
/// with the status line + non-reasoning warning; anything else lands the yellow
/// `[/thinking: unknown level …]` guidance.
pub fn apply_thinking_inline(
    session: &mut AgentSession,
    cwd: &Path,
    arg: &str,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
) {
    match parse_level_arg(arg) {
        Some(level) => apply_thinking_level(session, cwd, level, state, footer, requester),
        None => commit_status(
            state,
            requester,
            &format!(
                "[/thinking: unknown level \"{arg}\"; try off/minimal/low/medium/high/xhigh/max]"
            ),
        ),
    }
}

/// Apply a `/model <pattern>` direct-argument submission (VAL-OVERLAY-017 /
/// VAL-OVERLAY-018): resolve the pattern against the registry and switch without a
/// dialog. No match lands the yellow `[/model: no match for "<pattern>"]` guidance;
/// an ambiguous / invalid-thinking-suffix pattern surfaces the resolver's warning.
pub fn apply_model_pattern(
    session: &mut AgentSession,
    cwd: &Path,
    pattern: &str,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
) {
    use crate::core::model_resolver::{ParseModelPatternOptions, parse_model_pattern_full};

    // Same re-snapshot as the `/model` dialog: a pattern typed mid-session
    // must resolve against the freshest catalog the background refresh
    // installed, not the construction-time snapshot.
    session.refresh_model_registry();
    let all_models = session.model_registry().all().to_vec();
    let parsed =
        parse_model_pattern_full(pattern, &all_models, ParseModelPatternOptions::permissive());

    let Some(model) = parsed.model else {
        commit_status(
            state,
            requester,
            &format!("[/model: no match for \"{pattern}\"]"),
        );
        return;
    };

    let id = model.id.clone();
    session.set_model(model);
    // A `<model>:<level>` pattern carries an explicit thinking level; apply it too.
    if let Some(level) = parsed.thinking_level {
        let mut options = session.stream_options().clone();
        options.reasoning = Some(level);
        session.set_stream_options(options);
    }
    refresh_footer(session, cwd, state, footer, requester);
    if let Some(warning) = parsed.warning {
        commit_status(state, requester, &format!("[/model: {warning}]"));
    }
    commit_status(state, requester, &format!("[model set to {id}]"));
}

/// Apply a `/theme <name>` direct-argument submission (VAL-OVERLAY-017 /
/// VAL-OVERLAY-018): no dialog — a persistable theme name is saved with the
/// `saved; restart to apply` line; anything else lands the red
/// `[theme: unknown theme …]` guidance (handled inside [`apply_theme`]).
pub fn apply_theme_inline(
    session: &mut AgentSession,
    name: &str,
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
) {
    apply_theme(session, name, state, requester);
}

/// The [`SlashCommandAction`] variants this feature's selector family owns, so the
/// driver can route them to the async overlay opens / direct-arg helpers *before*
/// the sync slash dispatch. Kept here (next to the handlers) so the routing set is
/// a single source of truth.
#[must_use]
pub fn is_config_selector_action(action: &SlashCommandAction) -> bool {
    matches!(
        action,
        SlashCommandAction::OpenThinkingSelector { .. }
            | SlashCommandAction::OpenSettingsSelector
            | SlashCommandAction::Theme(_)
            | SlashCommandAction::ModelByPattern(_)
    )
}

/// The [`SlashCommandAction`] variants the **picker** selector family owns (`/tree`,
/// `/scoped-models`, `/fork`), so the driver can route them to the async overlay
/// opens *before* the sync slash dispatch. Kept here (next to the handlers) so the
/// routing set is a single source of truth — the same shape as
/// [`is_config_selector_action`].
#[must_use]
pub fn is_picker_selector_action(action: &SlashCommandAction) -> bool {
    matches!(
        action,
        SlashCommandAction::OpenTreeSelector(_)
            | SlashCommandAction::OpenScopedModelsSelector
            | SlashCommandAction::Fork(_)
    )
}

/// Open the `/tree` directory picker overlay and, on a pick, land the
/// `[/tree picked: <relative-path>]` status line (VAL-OVERLAY-024).
///
/// Resolves the scan root from `arg` (a path relative to `cwd`, or `cwd` itself for a
/// bare `/tree`), scans it dirs-first with the noise directories skipped, mounts the
/// [`TreeSelector`], then awaits its single outcome:
///
/// - **Selected(path)** — the `[/tree picked: <path>]` status line lands.
/// - **Cancelled** — the yellow `[/tree cancelled]` line lands.
///
/// A `<subdir>` argument that does not resolve to a directory takes the no-data
/// degradation: no overlay opens and the `[/tree: not a directory …]` status line
/// lands (VAL-OVERLAY-019).
pub async fn open_tree_selector(
    cwd: &Path,
    arg: Option<&str>,
    overlay: &SharedOverlay,
    done: &DoneSignal,
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
    nav: NavKeys,
) {
    // Resolve the scan root: a bare `/tree` scans cwd; `/tree <subdir>` scans that
    // subtree (joined onto cwd, so a relative arg stays inside the project).
    let (root, title) = match arg.map(str::trim).filter(|s| !s.is_empty()) {
        Some(sub) => (cwd.join(sub), format!("Tree: {sub}")),
        None => (cwd.to_path_buf(), "Tree: .".to_string()),
    };

    // No-data degradation: a non-directory target never opens the picker.
    if !root.is_dir() {
        let shown = arg.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(".");
        commit_status(
            state,
            requester,
            &format!("[/tree: not a directory \"{shown}\"]"),
        );
        return;
    }

    let rows = scan_tree(&root);

    let (tx, mut rx) = mpsc::unbounded_channel::<TreeOutcome>();
    done.store(false, Ordering::SeqCst);
    let selector = TreeSelector::with_nav(rows, title, tx, done.clone(), nav);
    let controller: Arc<Mutex<dyn SelectorController>> = Arc::new(Mutex::new(selector));

    overlay::mount(overlay, requester, controller, done.clone());

    match rx.recv().await {
        Some(TreeOutcome::Selected(path)) => {
            commit_status(state, requester, &format!("[/tree picked: {path}]"));
        }
        Some(TreeOutcome::Cancelled) => {
            commit_status(state, requester, "[/tree cancelled]");
        }
        None => overlay::close(overlay, requester),
    }
}

/// Open the `/scoped-models` multi-select overlay and apply the user's session-only
/// pick (VAL-OVERLAY-011 / -031 / -033).
///
/// Seeds the selector from `settings.enabled_models` (`None` = all enabled), mounts
/// the [`ScopedModelsSelector`], then awaits its single outcome:
///
/// - **Saved(_)** — the change is **session-only** (the parity nail): nothing is
///   written to settings, so a reopen shows the unchanged on-disk config. The honest
///   `[scoped-models: session-only — persist not yet wired]` notice lands
///   (VAL-OVERLAY-031).
/// - **Cancelled** — the yellow `[scoped-models cancelled]` line lands.
///
/// A registry with **no models** takes the no-data degradation: no overlay opens and
/// the `[scoped-models: no models available]` status line lands (VAL-OVERLAY-019).
pub async fn open_scoped_models_selector(
    session: &mut AgentSession,
    overlay: &SharedOverlay,
    done: &DoneSignal,
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
) {
    let all_models = session.model_registry().all().to_vec();

    // No-data degradation: an empty registry never opens the picker.
    if all_models.is_empty() {
        commit_status(state, requester, "[scoped-models: no models available]");
        return;
    }

    let enabled_ids = session.settings().current().enabled_models.clone();

    let (tx, mut rx) = mpsc::unbounded_channel::<ScopedModelsOutcome>();
    done.store(false, Ordering::SeqCst);
    let selector = ScopedModelsSelector::new(all_models, enabled_ids, tx, done.clone());
    let controller: Arc<Mutex<dyn SelectorController>> = Arc::new(Mutex::new(selector));

    overlay::mount(overlay, requester, controller, done.clone());

    match rx.recv().await {
        Some(ScopedModelsOutcome::Saved(_)) => {
            // Session-only (parity nail VAL-OVERLAY-031): do NOT persist to settings.
            // The subset is not wired to durable storage yet, so a reopen shows the
            // unchanged on-disk config — surface the honest notice.
            commit_status(
                state,
                requester,
                &format!("[scoped-models: {SESSION_ONLY_NOTICE}]"),
            );
        }
        Some(ScopedModelsOutcome::Cancelled) => {
            commit_status(state, requester, "[scoped-models cancelled]");
        }
        None => overlay::close(overlay, requester),
    }
}

/// Open the `/fork` picker overlay and, on a pick, fork the session at the chosen
/// user message (VAL-OVERLAY-023).
///
/// Lists the session's forkable user messages, mounts the [`UserMessageSelector`]
/// (titled "Fork from Message", latest preselected), then awaits its single outcome:
///
/// - **Selected(entry_id)** — `session.fork(entry_id)` branches the session; a
///   `[branch]` collapsible summary lands (reusing info-commands'
///   [`SummaryKind::Branch`] + its Ctrl+R expand listener) and the
///   `[forked at: <preview>]` status line lands.
/// - **Cancelled** — the yellow `[fork cancelled]` line lands.
///
/// A session with **no user messages** takes the no-data degradation: no overlay
/// opens and the `[fork: no user messages to fork from]` status line lands
/// (VAL-OVERLAY-019).
// One over the lint's ceiling: `nav` joins the existing overlay-mount + session-apply
// parameter set; see the note on `open_settings_selector`.
#[allow(clippy::too_many_arguments)]
pub async fn open_fork_selector(
    session: &mut AgentSession,
    cwd: &Path,
    overlay: &SharedOverlay,
    done: &DoneSignal,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
    nav: NavKeys,
) {
    let entries = session.fork_messages();

    // No-data degradation: nothing to fork from → no overlay, just the status line.
    if entries.is_empty() {
        commit_status(state, requester, "[fork: no user messages to fork from]");
        return;
    }

    let messages: Vec<ForkItem> = entries
        .into_iter()
        .map(|e| ForkItem {
            entry_id: e.entry_id,
            text: e.text,
        })
        .collect();

    let (tx, mut rx) = mpsc::unbounded_channel::<ForkOutcome>();
    done.store(false, Ordering::SeqCst);
    let selector = UserMessageSelector::with_nav(messages, tx, done.clone(), nav);
    let controller: Arc<Mutex<dyn SelectorController>> = Arc::new(Mutex::new(selector));

    overlay::mount(overlay, requester, controller, done.clone());

    match rx.recv().await {
        Some(ForkOutcome::Selected(entry_id)) => {
            apply_fork(session, cwd, &entry_id, state, footer, requester);
        }
        Some(ForkOutcome::Cancelled) => {
            commit_status(state, requester, "[fork cancelled]");
        }
        None => overlay::close(overlay, requester),
    }
}

/// Fork the session at `entry_id`, land a `[branch]` collapsible summary + the
/// `[forked at: <preview>]` status line, and refresh the footer so the branched
/// session's context surfaces. A fork failure takes the yellow status route.
fn apply_fork(
    session: &mut AgentSession,
    cwd: &Path,
    entry_id: &str,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
) {
    match session.fork(entry_id) {
        Ok(text) => {
            let preview = super::user_message_selector::fold_single_line(&text);
            let preview = truncate_preview(&preview, 60);
            // A `[branch]` collapsible summary, reusing info-commands' SummaryKind +
            // its Ctrl+R expand listener (the summary is remembered so Ctrl+R flips
            // it). The expanded body carries the forked-from message.
            let summary = CollapsibleSummary {
                kind: SummaryKind::Branch,
                summary: format!("Forked a new session from: {preview}"),
                expanded: false,
            };
            commit_summary(state, requester, summary);
            refresh_footer(session, cwd, state, footer, requester);
            commit_status(state, requester, &format!("[forked at: {preview}]"));
        }
        Err(e) => {
            commit_status(state, requester, &format!("[fork failed: {e}]"));
        }
    }
}

/// Truncate a single-line preview to `max` chars, appending an ellipsis when cut.
fn truncate_preview(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let head: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

/// Commit a collapsible summary block to scrollback and remember it so the driver's
/// global Ctrl+R listener can expand/collapse it. Mirrors the `/compact` summary
/// path so the branch summary inherits the same expand behaviour for free.
fn commit_summary(
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
    summary: CollapsibleSummary,
) {
    let mut guard = lock_state(state);
    let width = guard.size.cols;
    let palette = guard.palette();
    let lines = super::summary::summary_lines(&summary, width, &palette);
    guard.queue_commit(lines);
    guard.remember_summary(summary);
    drop(guard);
    requester.request_frame();
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
// One over the lint's ceiling: `nav` joins the existing overlay-mount + session-apply
// parameter set; see the note on `open_settings_selector`.
#[allow(clippy::too_many_arguments)]
pub async fn open_resume_picker(
    session: &mut AgentSession,
    cwd: &Path,
    overlay: &SharedOverlay,
    done: &DoneSignal,
    state: &Arc<Mutex<DriverState>>,
    footer: &SharedFooter,
    requester: &FrameRequester,
    nav: NavKeys,
) {
    let sessions = list_resumable_sessions(session, cwd);

    let (tx, mut rx) = mpsc::unbounded_channel::<SessionOutcome>();
    // Reset the shared done flag before mounting (the runtime's "overlay finished"
    // latch, cleared per open so a prior selector's raise never leaks in).
    done.store(false, Ordering::SeqCst);
    let picker = SessionPicker::with_nav(sessions, tx, done.clone(), nav);
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
    let palette = guard.palette();
    let blocks = replay_blocks(&messages, &label, hide_thinking, width, &palette);
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

/// Commit a red error block to scrollback and request a repaint. Used by the
/// direct-arg guidance for an unknown theme (VAL-OVERLAY-018) and a rejected
/// settings write.
fn commit_error(state: &Arc<Mutex<DriverState>>, requester: &FrameRequester, text: &str) {
    let lines = chat::error_lines(text);
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

    // === Driver-side apply tests (thinking / model-pattern / settings entries) ===

    use hand_tui::rt::scheduler::{FrameRequester, FrameScheduler};
    use hand_tui::rt::view::TerminalSize;

    /// A model with the given reasoning flag and a real context window (so the
    /// footer's context % computes).
    fn model_with_reasoning(reasoning: bool) -> Model {
        let mut m = make_model(Provider::Anthropic, "test-model", "Test");
        m.reasoning = reasoning;
        m.context_window = 200_000;
        m
    }

    fn test_session(reasoning: bool) -> AgentSession {
        AgentSession::in_memory_with_client(
            model_with_reasoning(reasoning),
            vec![],
            model::Client::new(),
        )
    }

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

    fn committed_text(state: &Arc<Mutex<DriverState>>) -> String {
        lock_state(state)
            .pending_commits
            .iter()
            .flatten()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // --- /thinking apply + warning (VAL-OVERLAY-025 / VAL-OVERLAY-026) -----

    #[tokio::test]
    async fn thinking_apply_sets_the_level_and_lands_the_status_line() {
        let mut session = test_session(true);
        let cwd = Path::new("/tmp");
        let (state, footer, req) = (state(), footer_of(&session, cwd), test_requester());

        apply_thinking_level(
            &mut session,
            cwd,
            Some(ThinkingLevel::High),
            &state,
            &footer,
            &req,
        );

        assert_eq!(
            session.stream_options().reasoning,
            Some(ThinkingLevel::High)
        );
        let out = committed_text(&state);
        assert!(
            out.contains("[thinking: high]"),
            "status line missing: {out}"
        );
        // A reasoning model gets no warning.
        assert!(
            !out.contains("not a reasoning model"),
            "reasoning model must not warn: {out}"
        );
        // The footer's thinking segment reflects the new level.
        assert_eq!(lock_footer(&footer).thinking_level, "high");
    }

    #[tokio::test]
    async fn thinking_apply_on_a_non_reasoning_model_warns_for_a_level() {
        // VAL-OVERLAY-026: setting a non-off level on a non-reasoning model lands
        // the yellow warning that it has no effect.
        let mut session = test_session(false);
        let cwd = Path::new("/tmp");
        let (state, footer, req) = (state(), footer_of(&session, cwd), test_requester());

        apply_thinking_level(
            &mut session,
            cwd,
            Some(ThinkingLevel::Medium),
            &state,
            &footer,
            &req,
        );

        let out = committed_text(&state);
        assert!(out.contains("[thinking: medium]"), "status missing: {out}");
        assert!(
            out.contains("not a reasoning model"),
            "non-reasoning warning missing: {out}"
        );
    }

    #[tokio::test]
    async fn thinking_off_on_a_non_reasoning_model_does_not_warn() {
        // VAL-OVERLAY-026: `/thinking off` never warns, even on a non-reasoning
        // model.
        let mut session = test_session(false);
        let cwd = Path::new("/tmp");
        let (state, footer, req) = (state(), footer_of(&session, cwd), test_requester());

        apply_thinking_level(&mut session, cwd, None, &state, &footer, &req);

        let out = committed_text(&state);
        assert!(out.contains("[thinking: off]"), "off status missing: {out}");
        assert!(
            !out.contains("not a reasoning model"),
            "off must never warn: {out}"
        );
    }

    // --- /thinking direct-arg + invalid arg (VAL-OVERLAY-017 / -018) ------

    #[tokio::test]
    async fn thinking_inline_unknown_level_lands_yellow_guidance() {
        let mut session = test_session(true);
        let cwd = Path::new("/tmp");
        let (state, footer, req) = (state(), footer_of(&session, cwd), test_requester());

        apply_thinking_inline(&mut session, cwd, "bogus", &state, &footer, &req);

        // The level is unchanged and the yellow unknown-level guidance lands.
        assert_eq!(session.stream_options().reasoning, None);
        let out = committed_text(&state);
        assert!(
            out.contains("unknown level") && out.contains("bogus"),
            "unknown-level guidance missing: {out}"
        );
    }

    #[tokio::test]
    async fn thinking_inline_valid_level_applies_without_a_dialog() {
        let mut session = test_session(true);
        let cwd = Path::new("/tmp");
        let (state, footer, req) = (state(), footer_of(&session, cwd), test_requester());

        apply_thinking_inline(&mut session, cwd, "high", &state, &footer, &req);

        assert_eq!(
            session.stream_options().reasoning,
            Some(ThinkingLevel::High)
        );
        assert!(committed_text(&state).contains("[thinking: high]"));
    }

    // --- /model <pattern> direct-arg + no match (VAL-OVERLAY-017 / -018) --

    #[tokio::test]
    async fn model_pattern_no_match_lands_yellow_no_match_guidance() {
        let mut session = test_session(true);
        let cwd = Path::new("/tmp");
        let (state, footer, req) = (state(), footer_of(&session, cwd), test_requester());
        let original = session.model().id.clone();

        apply_model_pattern(
            &mut session,
            cwd,
            "definitely-no-such-model-xyz",
            &state,
            &footer,
            &req,
        );

        // The model is unchanged and the `[/model: no match …]` guidance lands.
        assert_eq!(session.model().id, original, "no switch on no match");
        let out = committed_text(&state);
        assert!(
            out.contains("no match") && out.contains("definitely-no-such-model-xyz"),
            "no-match guidance missing: {out}"
        );
    }

    // --- /settings merged effective defaults (VAL-OVERLAY-036) ------------

    #[test]
    fn build_settings_entries_surfaces_merged_defaults_including_project_override() {
        use crate::core::settings::Settings;

        // A merged view where the project override supplies the model — it must be
        // visible in the dialog (issue #16 UAT regression).
        let mut merged = Settings::defaults();
        merged.default_provider = Some("anthropic".to_string());
        merged.default_model = Some("claude-opus-4-7".to_string());
        merged.default_thinking_level = Some(ThinkingLevelSetting::High);

        let entries = build_settings_entries(&merged);
        // The three default-* rows come first, in order.
        assert_eq!(entries[0].key, "default_provider");
        assert_eq!(entries[1].key, "default_model");
        assert_eq!(entries[2].key, "default_thinking_level");
        // Their effective (merged) string values are visible.
        assert_eq!(entries[0].value.to_string(), "anthropic");
        assert_eq!(entries[1].value.to_string(), "claude-opus-4-7");
        assert_eq!(entries[2].value.to_string(), "high");
    }

    #[test]
    fn build_settings_entries_shows_unset_placeholder_for_missing_defaults() {
        use crate::core::settings::Settings;

        // A truly empty merged view (no provider/model/thinking set) shows the
        // `(unset)` placeholder for each default row.
        let merged = Settings::default();
        let entries = build_settings_entries(&merged);
        assert_eq!(entries[0].value.to_string(), "(unset)");
        assert_eq!(entries[1].value.to_string(), "(unset)");
        assert_eq!(entries[2].value.to_string(), "(unset)");
        // Every id round-trips through apply_setting_by_id (so a change persists).
        for e in &entries {
            assert!(
                matches!(
                    e.key.as_str(),
                    "default_provider"
                        | "default_model"
                        | "default_thinking_level"
                        | "theme"
                        | "auto_compact"
                        | "hide_thinking_block"
                        | "show_images"
                        | "quiet_startup"
                ),
                "unexpected settings id: {}",
                e.key
            );
        }
    }

    /// The `/settings` dialog surfaces `show_images` as a bool toggle defaulting to
    /// the merged effective value (`true`), so the mid-session toggle is reachable
    /// from the dialog (VAL-IMG-011).
    #[test]
    fn build_settings_entries_includes_show_images_toggle() {
        use crate::core::settings::Settings;

        let merged = Settings::defaults();
        let entries = build_settings_entries(&merged);
        let show = entries
            .iter()
            .find(|e| e.key == "show_images")
            .expect("show_images entry present in /settings");
        assert_eq!(
            show.value.to_string(),
            "true",
            "show_images defaults to the effective value (true)"
        );
    }

    /// Applying `show_images = false` through the `/settings` change path persists
    /// the setting *and* flips the live driver-state gate, so the next tool result
    /// honours it without a restart (VAL-IMG-011). Toggling back to `true` restores
    /// the gate. Driven with a settings manager backed by a temp global path so the
    /// `save` in `apply_settings_change` succeeds and the flip runs.
    #[tokio::test]
    async fn apply_show_images_change_flips_the_live_driver_gate() {
        use crate::core::settings::{Settings, SettingsManager};

        let dir = tempfile::TempDir::new().unwrap();
        let global_path = dir.path().join("settings.yaml");
        let mgr = SettingsManager::from_layers_for_test(
            Settings::default(),
            Settings::default(),
            Some(global_path),
            None,
        );
        let mut session = test_session(true);
        *session.settings_mut() = mgr;

        let (state, req) = (state(), test_requester());
        let footer = footer_of(&session, Path::new("."));
        // Sanity: the driver gate starts on (the DriverState default).
        assert!(lock_state(&state).show_images(), "gate starts on");

        apply_settings_change(
            &mut session,
            Path::new("."),
            "show_images",
            "false",
            &state,
            &footer,
            &req,
        );
        assert!(
            !lock_state(&state).show_images(),
            "applying show_images=false flips the live gate off mid-session"
        );

        apply_settings_change(
            &mut session,
            Path::new("."),
            "show_images",
            "true",
            &state,
            &footer,
            &req,
        );
        assert!(
            lock_state(&state).show_images(),
            "applying show_images=true restores the live gate"
        );
    }

    // --- /theme unknown-arg guidance (VAL-OVERLAY-018) --------------------

    #[tokio::test]
    async fn theme_inline_unknown_name_lands_red_guidance() {
        // An unknown theme name never persists — it takes the red guidance route
        // before touching settings, so an in-memory session suffices.
        let mut session = test_session(true);
        let (state, req) = (state(), test_requester());

        apply_theme_inline(&mut session, "nosuch", &state, &req);

        let out = committed_text(&state);
        assert!(
            out.contains("unknown theme") && out.contains("nosuch"),
            "unknown-theme guidance missing: {out}"
        );
    }

    // --- theme + settings persistence round-trip (VAL-OVERLAY-013 / -014) --

    #[test]
    fn theme_setting_persists_and_reloads() {
        use crate::core::settings::{Settings, SettingsManager, SettingsScope, ThemeSetting};

        // The persistence contract `apply_theme` relies on: apply_setting_by_id +
        // save writes YAML that a fresh load reads back. Driver-level tmux proves
        // the end-to-end restart-palette path; this pins the disk round-trip.
        let dir = tempfile::TempDir::new().unwrap();
        let global_path = dir.path().join("settings.yaml");
        // Empty raw layers so the global change is not shadowed by a populated
        // project layer.
        let mut mgr = SettingsManager::from_layers_for_test(
            Settings::default(),
            Settings::default(),
            Some(global_path.clone()),
            None,
        );

        mgr.apply_setting_by_id(SettingsScope::Global, "theme", "light")
            .unwrap();
        mgr.save(SettingsScope::Global).unwrap();

        // The persisted YAML carries the theme, so the next launch reads it.
        let written = std::fs::read_to_string(&global_path).unwrap();
        assert!(
            written.contains("light"),
            "persisted YAML must carry the theme: {written}"
        );
        assert_eq!(mgr.current().theme(), ThemeSetting::Light);
    }

    #[test]
    fn settings_change_persists_and_reloads() {
        use crate::core::settings::{Settings, SettingsManager, SettingsScope};

        // The first-change persistence path `apply_settings_change` relies on.
        let dir = tempfile::TempDir::new().unwrap();
        let global_path = dir.path().join("settings.yaml");
        let mut mgr = SettingsManager::from_layers_for_test(
            Settings::default(),
            Settings::default(),
            Some(global_path.clone()),
            None,
        );

        mgr.apply_setting_by_id(SettingsScope::Global, "auto_compact", "false")
            .unwrap();
        mgr.save(SettingsScope::Global).unwrap();

        assert_eq!(mgr.current().compaction.enabled, Some(false));
        let written = std::fs::read_to_string(&global_path).unwrap();
        assert!(
            written.contains("false"),
            "persisted YAML must carry the toggle: {written}"
        );
    }

    // --- routing predicate ------------------------------------------------

    #[test]
    fn is_config_selector_action_matches_the_family() {
        assert!(is_config_selector_action(
            &SlashCommandAction::OpenThinkingSelector { inline_level: None }
        ));
        assert!(is_config_selector_action(&SlashCommandAction::Theme(None)));
        assert!(is_config_selector_action(
            &SlashCommandAction::OpenSettingsSelector
        ));
        assert!(is_config_selector_action(
            &SlashCommandAction::ModelByPattern("sonnet".into())
        ));
        // Bare /model opens the (separately-routed) model selector — not this family.
        assert!(!is_config_selector_action(
            &SlashCommandAction::OpenModelSelector
        ));
        assert!(!is_config_selector_action(&SlashCommandAction::ClearChat));
    }

    #[test]
    fn is_picker_selector_action_matches_the_family() {
        assert!(is_picker_selector_action(
            &SlashCommandAction::OpenTreeSelector(None)
        ));
        assert!(is_picker_selector_action(
            &SlashCommandAction::OpenScopedModelsSelector
        ));
        assert!(is_picker_selector_action(&SlashCommandAction::Fork(None)));
        assert!(is_picker_selector_action(&SlashCommandAction::Fork(Some(
            "e1".into()
        ))));
        // The config family is not the picker family.
        assert!(!is_picker_selector_action(&SlashCommandAction::Theme(None)));
        assert!(!is_picker_selector_action(
            &SlashCommandAction::OpenModelSelector
        ));
    }

    // --- picker apply-side (fork branch summary + no-data helpers) ---------

    #[tokio::test]
    async fn fork_of_a_missing_entry_lands_the_yellow_fork_failed_line() {
        // The `apply_fork` error route: forking a non-existent entry id never
        // branches — it lands the yellow `[fork failed: …]` status (the happy-path
        // branch summary + `[forked at: …]` is proved end-to-end in the fixtures,
        // which journal a real user message).
        let mut session = test_session(false);
        let cwd = Path::new("/tmp");
        let (state, footer, req) = (state(), footer_of(&session, cwd), test_requester());

        apply_fork(&mut session, cwd, "no-such-entry-id", &state, &footer, &req);

        let out = committed_text(&state);
        assert!(out.contains("[fork failed:"), "fork error missing: {out}");
        // No branch summary is remembered on the failure path.
        assert!(
            lock_state(&state).collapsible_summaries.is_empty(),
            "a failed fork remembers no summary"
        );
    }

    #[test]
    fn truncate_preview_caps_long_text_with_an_ellipsis() {
        assert_eq!(truncate_preview("short", 60), "short");
        let long = "x".repeat(100);
        let out = truncate_preview(&long, 10);
        assert_eq!(
            out.chars().count(),
            10,
            "capped to max including the ellipsis"
        );
        assert!(out.ends_with('…'));
    }

    // === Per-selector wrap-vs-clamp navigation nail (VAL-OVERLAY-002) ======
    //
    // Now that every selector exists, pin each one's navigation semantics in a
    // single table-driven walk. `wrap` selectors bring Up-on-the-first back to the
    // last row (and Down-on-the-last back to the first); `clamp` selectors treat
    // both ends as no-ops. This is the cross-selector nail the follow-up validator
    // probes from outside — here it is unit-pinned so a regression fails the build.
    //
    // wrap:  model / fork / scoped-models / thinking / theme
    // clamp: tree / resume / login / settings
    //
    // (login is owned by m3-login-auth; settings is a first-change-closes dialog
    // with no cursor wrap; both are noted for the table's completeness. This walk
    // pins the selectors this feature and its siblings own directly.)

    mod nav_nail {
        use super::super::*;
        use crate::core::session_manager::SessionInfo;
        use crate::modes::interactive::rt_driver::model_selector::ModelSelector;
        use crate::modes::interactive::rt_driver::overlay::{SelectorController, new_done_signal};
        use crate::modes::interactive::rt_driver::scoped_models_selector::ScopedModelsSelector;
        use crate::modes::interactive::rt_driver::session_picker::SessionPicker;
        use crate::modes::interactive::rt_driver::theme_selector::ThemeSelector;
        use crate::modes::interactive::rt_driver::thinking_selector::ThinkingSelector;
        use crate::modes::interactive::rt_driver::tree_selector::{TreeRow, TreeSelector};
        use crate::modes::interactive::rt_driver::user_message_selector::{
            ForkItem, UserMessageSelector,
        };

        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use hand_tui::rt::events::RtKey;
        use model::types::Provider;
        use model::{Api, Cost, InputType, Model};

        fn key(id: &str) -> RtKey {
            RtKey {
                key_id: Some(id.to_string()),
                raw: KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            }
        }

        fn model(id: &str) -> Model {
            Model {
                id: id.to_string(),
                name: id.to_string(),
                api: Api::AnthropicMessages,
                provider: Provider::Anthropic,
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

        /// Drive a fresh-at-top selector: Up first (a wrap moves off row 0, a clamp
        /// stays), reporting the resulting body text's first `→` marker position via
        /// a probe closure. To keep this generic across selectors we assert on the
        /// concrete cursor index each exposes.
        ///
        /// Returns `(after_up_from_top_moved, after_down_from_bottom_moved)`.
        fn walk<C: SelectorController>(
            sel: &mut C,
            len: usize,
            index: impl Fn(&C) -> usize,
        ) -> (bool, bool) {
            // Start at the top.
            let top = index(sel);
            sel.handle_key(&key("up"));
            let moved_up = index(sel) != top;
            // Drive to the bottom, then one more down.
            // Reset to top first (Up may have wrapped to the bottom).
            for _ in 0..len {
                sel.handle_key(&key("up"));
            }
            // Now walk to the last row.
            for _ in 0..len.saturating_sub(1) {
                sel.handle_key(&key("down"));
            }
            let bottom = index(sel);
            sel.handle_key(&key("down"));
            let moved_down = index(sel) != bottom;
            (moved_up, moved_down)
        }

        #[test]
        fn model_selector_wraps() {
            let (tx, _rx) = mpsc::unbounded_channel();
            let mut sel = ModelSelector::new(
                None,
                vec![model("a"), model("b"), model("c")],
                vec![],
                tx,
                new_done_signal(),
            );
            let (up, down) = walk(&mut sel, 3, |s| {
                s.highlighted().map_or(0, |m| {
                    // Map the highlighted model back to its filtered index is awkward;
                    // instead assert on the wrap directly.
                    ["a", "b", "c"].iter().position(|x| *x == m.id).unwrap_or(0)
                })
            });
            assert!(up, "model: Up on the first row wraps");
            assert!(down, "model: Down on the last row wraps");
        }

        #[test]
        fn fork_selector_wraps() {
            let (tx, _rx) = mpsc::unbounded_channel();
            let mut sel = UserMessageSelector::new(
                vec![
                    ForkItem {
                        entry_id: "a".into(),
                        text: "1".into(),
                    },
                    ForkItem {
                        entry_id: "b".into(),
                        text: "2".into(),
                    },
                    ForkItem {
                        entry_id: "c".into(),
                        text: "3".into(),
                    },
                ],
                tx,
                new_done_signal(),
            );
            // Fork preselects the LAST row, so reset to the top first.
            for _ in 0..3 {
                sel.handle_key(&key("up"));
            }
            let (up, down) = walk(&mut sel, 3, UserMessageSelector::selected_index);
            assert!(up, "fork: Up on the first row wraps to last");
            assert!(down, "fork: Down on the last row wraps to first");
        }

        #[test]
        fn scoped_models_selector_wraps() {
            let (tx, _rx) = mpsc::unbounded_channel();
            let mut sel = ScopedModelsSelector::new(
                vec![model("a"), model("b"), model("c")],
                None,
                tx,
                new_done_signal(),
            );
            let (up, down) = walk(&mut sel, 3, ScopedModelsSelector::selected_index);
            assert!(up, "scoped-models: Up on the first row wraps");
            assert!(down, "scoped-models: Down on the last row wraps");
        }

        #[test]
        fn thinking_selector_wraps() {
            let (tx, _rx) = mpsc::unbounded_channel();
            let mut sel = ThinkingSelector::new(None, tx, new_done_signal());
            let len = 7; // off + 6 levels
            let (up, down) = walk(&mut sel, len, ThinkingSelector::selected_index);
            assert!(up, "thinking: Up on the first row wraps");
            assert!(down, "thinking: Down on the last row wraps");
        }

        #[test]
        fn theme_selector_wraps() {
            let (tx, _rx) = mpsc::unbounded_channel();
            let mut sel = ThemeSelector::new("dark", tx, new_done_signal());
            let len = 4;
            let (up, down) = walk(&mut sel, len, ThemeSelector::selected_index);
            assert!(up, "theme: Up on the first row wraps");
            assert!(down, "theme: Down on the last row wraps");
        }

        #[test]
        fn tree_selector_clamps() {
            let (tx, _rx) = mpsc::unbounded_channel();
            let rows = vec![
                TreeRow {
                    rel_path: "a".into(),
                    label: "a".into(),
                    depth: 0,
                    is_dir: false,
                },
                TreeRow {
                    rel_path: "b".into(),
                    label: "b".into(),
                    depth: 0,
                    is_dir: false,
                },
                TreeRow {
                    rel_path: "c".into(),
                    label: "c".into(),
                    depth: 0,
                    is_dir: false,
                },
            ];
            let mut sel = TreeSelector::new(rows, "t", tx, new_done_signal());
            let (up, down) = walk(&mut sel, 3, TreeSelector::selected_index);
            assert!(!up, "tree: Up on the first row clamps (no wrap)");
            assert!(!down, "tree: Down on the last row clamps (no wrap)");
        }

        #[test]
        fn resume_picker_clamps() {
            let (tx, _rx) = mpsc::unbounded_channel();
            let sessions: Vec<SessionInfo> = (0..3)
                .map(|i| SessionInfo {
                    path: std::path::PathBuf::from(format!("/tmp/{i}.jsonl")),
                    id: format!("id{i}"),
                    cwd: "/tmp".into(),
                    timestamp: 0,
                    modified: 0,
                    message_count: 1,
                    name: Some(format!("s{i}")),
                    parent_session_path: None,
                    first_message: "hi".into(),
                    all_messages_text: String::new(),
                })
                .collect();
            let mut sel = SessionPicker::new(sessions, tx, new_done_signal());
            // SessionPicker exposes `highlighted`, not a raw index; probe via id.
            let index = |s: &SessionPicker| {
                s.highlighted().map_or(0, |info| {
                    info.id
                        .trim_start_matches("id")
                        .parse::<usize>()
                        .unwrap_or(0)
                })
            };
            let (up, down) = walk(&mut sel, 3, index);
            assert!(!up, "resume: Up on the first row clamps (no wrap)");
            assert!(!down, "resume: Down on the last row clamps (no wrap)");
        }
    }
}
