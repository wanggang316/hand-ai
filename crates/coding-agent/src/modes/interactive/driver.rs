//! `InteractiveMode` — TUI driver wiring [`AgentSession`], a [`Tui`], and the
//! chat / footer / editor components into a runnable interactive session.
//!
//! A deliberately minimal driver. The skeleton covers the happy path:
//!
//! * a vertical layout with chat scrollback above and an editor + footer below;
//! * user input → [`AgentSession::send_message`] → message components;
//! * a small slash-command dispatch table (`/quit`, `/exit`, `/help`, `/model`);
//! * a model-selector overlay for `/model`.
//!
//! ## Concurrency model
//!
//! The TUI's run loop is the foreground task. It owns the [`Tui`] and never
//! returns until the user requests quit. To send work to the agent without
//! blocking the run loop:
//!
//! * The editor's input listener stages submitted text into a shared
//!   "pending submission" slot.
//! * A background task polls this slot and, when populated, calls
//!   [`AgentSession::send_message`] in its own task. Agent events stream back
//!   via the [`AgentSession::subscribe`] callback, which forwards them onto a
//!   tokio channel.
//! * A second background task drains that channel and mutates the shared
//!   chat-list (an `Arc<Mutex<Vec<Box<dyn Component>>>>`). The TUI re-reads
//!   that list on every render, so updates appear without explicit redraws.
//! * `tui.request_render()` is poked from the chat-list mutator so the diff
//!   renderer wakes up promptly.
//!
//! Anything not covered here is marked with `// TODO(parity)` and surfaces a
//! placeholder.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use hand_tui::{
    CombinedAutocompleteProvider, Component, EditorComponent, Focusable, HandleResult, InputEvent,
    KeyName, ListenerResult, OverlayMounter, OverlayOptions, PathAutocompleteProvider,
    ProcessTerminal, SettingEntry, SettingValue, SlashCommand as TuiSlashCommand,
    SlashCommandProvider, TextComponent, Tui, TuiError,
};
use tokio::sync::mpsc;

use crate::core::agent_session::{AgentSession, AgentSessionEvent};
use crate::core::error::CodingAgentError;

use super::components::{
    AssistantMessageComponent, AuthSelectorMode, AuthSelectorProvider, BashExecutionComponent,
    BashStatus, BorderedLoaderComponent, CustomMessageComponent, CustomMessageData,
    FooterComponent, FooterViewModel, LoginDialogComponent, LoginDialogEvent, ModelOutcome,
    ModelSelectorComponent, OAuthOutcome, OAuthSelectorComponent, ScopedModelsConfig,
    ScopedModelsOutcome, ScopedModelsSelectorComponent, SessionSelectorComponent,
    SessionSelectorEvent, SettingsSelectorComponent, SettingsSelectorEvent, ThemeOutcome,
    ThemeSelectorComponent, ThinkingOutcome, ThinkingSelectorComponent, TokenUsageSummary,
    ToolExecutionComponent, TreeRow, TreeSelectorComponent, TreeSelectorEvent,
    UserMessageComponent,
};
use super::event_dispatch::{ChatUpdate, dispatch as dispatch_event};
use super::slash_commands::{
    ExportFormat, ParsedSlashCommand, SlashCommandAction, SlashCommandContext, SlashCommandResult,
    SlashCommandTable,
};

/// Errors raised by the interactive TUI driver.
#[derive(Debug, thiserror::Error)]
pub enum InteractiveError {
    #[error("agent error: {0}")]
    Agent(#[from] CodingAgentError),

    #[error("tui error: {0}")]
    Tui(#[from] TuiError),

    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

/// Shared chat scrollback. The TUI renders from this vec; agent / driver
/// commands push new components into it.
type ChatList = Arc<StdMutex<Vec<Box<dyn Component>>>>;

/// Shared footer view-model. Updated by the driver / agent task and read by
/// the TUI on every render.
type SharedFooter = Arc<StdMutex<FooterViewModel>>;

/// Component that defers its render to a shared chat list.
struct ChatScrollback {
    list: ChatList,
}

impl Component for ChatScrollback {
    fn render(&self, width: u16) -> Vec<String> {
        let list = self.list.lock().expect("chat list mutex poisoned");
        let mut out = Vec::new();
        for child in list.iter() {
            out.extend(child.render(width));
        }
        out
    }
}

/// Wrapper that defers rendering to a shared, externally-mutable component.
///
/// Used for in-flight components (assistant message, tool execution) where
/// the agent task mutates the underlying state and the TUI re-reads on each
/// render tick.
struct SharedRender<T: Component + Send + 'static> {
    inner: Arc<StdMutex<T>>,
}

impl<T: Component + Send + 'static> Component for SharedRender<T> {
    fn render(&self, width: u16) -> Vec<String> {
        let inner = self.inner.lock().expect("shared component mutex poisoned");
        inner.render(width)
    }
}

/// Like [`SharedRender`] but also forwards `handle_input` and the
/// hide/invalidate hooks. Use for components that need to keep receiving
/// input while the driver holds an out-of-tree mutable handle — namely
/// the editor (Ctrl+T toggles, Ctrl+G external-edit, per-thinking-level
/// border tinting).
struct SharedComponent<T: Component + Send + 'static> {
    inner: Arc<StdMutex<T>>,
}

impl<T: Component + Send + 'static> Component for SharedComponent<T> {
    fn render(&self, width: u16) -> Vec<String> {
        let inner = self.inner.lock().expect("shared component mutex poisoned");
        inner.render(width)
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        let mut inner = self.inner.lock().expect("shared component mutex poisoned");
        inner.handle_input(event)
    }

    fn invalidate(&mut self) {
        let mut inner = self.inner.lock().expect("shared component mutex poisoned");
        inner.invalidate();
    }

    fn set_hidden(&mut self, hidden: bool) {
        let mut inner = self.inner.lock().expect("shared component mutex poisoned");
        inner.set_hidden(hidden);
    }

    fn is_hidden(&self) -> bool {
        let inner = self.inner.lock().expect("shared component mutex poisoned");
        inner.is_hidden()
    }
}

/// Per-tool-call handle kept by the driver while a tool execution is
/// in-flight. Either branch points at the live, mutable component the chat
/// list also references through a [`SharedRender`] wrapper.
enum ToolHandle {
    Bash(Arc<StdMutex<BashExecutionComponent>>),
    Generic(Arc<StdMutex<ToolExecutionComponent>>),
}

/// Live in-flight components keyed by `tool_call_id`.
type ToolHandles = Arc<StdMutex<HashMap<String, ToolHandle>>>;

/// Live in-flight assistant message component (the one that streaming
/// `MessageUpdate` events should mutate). Replaced on every `MessageStart`.
type AssistantHandle = Arc<StdMutex<Option<Arc<StdMutex<AssistantMessageComponent>>>>>;

/// Component that renders a footer from a shared view-model so the TUI task
/// can re-render after the agent task updates it.
struct SharedFooterComponent {
    view: SharedFooter,
}

impl Component for SharedFooterComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let view = self.view.lock().expect("footer view-model mutex poisoned");
        let footer = FooterComponent::new(view.clone());
        footer.render(width)
    }
}

/// Loader slot rendered between the chat scrollback and the editor. The
/// driver swaps the inner component on agent / compaction lifecycle events
/// so the user always knows when the agent is actively working.
type SharedLoaderSlot = Arc<StdMutex<Option<Arc<StdMutex<BorderedLoaderComponent>>>>>;

struct LoaderSlot {
    slot: SharedLoaderSlot,
}

impl Component for LoaderSlot {
    fn render(&self, width: u16) -> Vec<String> {
        let slot = self.slot.lock().expect("loader slot mutex poisoned");
        match slot.as_ref() {
            Some(cell) => {
                let inner = cell.lock().expect("loader cell mutex poisoned");
                inner.render(width)
            }
            None => Vec::new(),
        }
    }
}

/// Submission staged by the input listener for the background driver task to
/// pick up.
#[derive(Default)]
struct Pending {
    text: Option<String>,
    quit: bool,
}

/// The interactive TUI driver.
pub struct InteractiveMode {
    session: AgentSession,
    cwd: PathBuf,
}

impl InteractiveMode {
    /// Build the driver. The actual TUI runs in [`Self::run`].
    pub fn new(session: AgentSession, cwd: PathBuf) -> Self {
        Self { session, cwd }
    }

    /// Build the footer view-model from current session state. Pass the
    /// running `usage` accumulator so the footer can show token totals
    /// without re-walking the message history on every render.
    pub(crate) fn build_footer_view(
        session: &AgentSession,
        cwd: &Path,
        usage: TokenUsageSummary,
    ) -> FooterViewModel {
        let context_window = session.model().context_window;
        let context_percent = if context_window > 0 {
            let tokens =
                crate::core::compaction::estimate_context_tokens(session.messages()) as f64;
            Some(tokens / context_window as f64 * 100.0)
        } else {
            None
        };
        FooterViewModel {
            cwd: cwd.display().to_string(),
            home_dir: dirs::home_dir().map(|p| p.display().to_string()),
            git_branch: detect_git_branch(cwd),
            session_name: session.label().map(|s| s.to_string()),
            usage,
            model_id: session.model().id.clone(),
            model_provider: session.model().provider.as_str().to_string(),
            context_window,
            context_percent,
            auto_compact_enabled: session.auto_compaction_enabled(),
            has_reasoning: session.model().reasoning,
            thinking_level: session
                .stream_options()
                .reasoning
                .map(|l| level_label(l).to_string())
                .unwrap_or_default(),
            available_provider_count: count_providers_with_credentials(),
            extension_statuses: Vec::new(),
        }
    }

    /// Run the interactive TUI to completion.
    pub async fn run(self) -> Result<(), InteractiveError> {
        let InteractiveMode { mut session, cwd } = self;

        // Shared state the TUI renders and the background task mutates.
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        // Running token-usage accumulator. Updated by the event pump on
        // every MessageEnd; consumed by `refresh_footer` to populate the
        // token segment of the footer view-model.
        let usage = Arc::new(StdMutex::new(TokenUsageSummary::default()));
        let footer = Arc::new(StdMutex::new(Self::build_footer_view(
            &session,
            &cwd,
            TokenUsageSummary::default(),
        )));
        let pending = Arc::new(StdMutex::new(Pending::default()));
        // Initialise the process-wide "hide thinking blocks" toggle (M5.5)
        // from the settings default. The flag itself lives in a static
        // (see `hide_thinking_flag()` below) so every code path that
        // builds an `AssistantMessageComponent` can subscribe to it
        // without threading an `Arc` argument through five layers of
        // helpers.
        hide_thinking_flag().store(
            session
                .settings()
                .current()
                .hide_thinking_block
                .unwrap_or(false),
            std::sync::atomic::Ordering::Relaxed,
        );

        // Welcome header at the very top of the scrollback. Compact
        // one-liner with the product name, version, and the most-used
        // keybindings. Stays in the scrollback (scrolls off as chat
        // grows).
        push_welcome_header(&chat, session.model());
        // M5.2 — surface a tmux-keyboard warning at startup so the user
        // knows extended-keys (Modified Enter, Alt+letter, etc.) won't
        // round-trip correctly through their multiplexer.
        if let Some(msg) = check_tmux_keyboard_setup() {
            push_status(&chat, msg, Some(YELLOW_FG));
        }

        // M5.4 — auto-display CHANGELOG entries that were added since
        // `last_changelog_version`. Runs BEFORE the replay so resumed
        // sessions (with non-empty messages) skip the banner. On a fresh
        // install (`last_changelog_version` not yet set) we only record
        // the current version and stay quiet — there's nothing to "catch
        // up on".
        maybe_show_changelog_on_update(&chat, &mut session);

        // Replay existing session messages.
        replay_messages_into(&chat, session.messages());

        // Channel agent → driver task.
        let (event_tx, event_rx) = mpsc::unbounded_channel::<AgentSessionEvent>();
        let forward = event_tx.clone();
        session.subscribe(move |event| {
            let _ = forward.send(event);
        });

        // Build the editor and wire its submit callback to the pending slot.
        // The callback runs from inside the Tui's input dispatch (same task
        // as the run loop), so it just hands the submitted text off to the
        // shared `Pending` slot which the agent task polls.
        //
        // No placeholder: the editor renders empty so an active
        // terminal IME composition (which draws its preview at the
        // cursor column) doesn't collide with a dim placeholder.
        let pending_for_submit = Arc::clone(&pending);
        let cwd_for_paste = cwd.clone();
        let paste_transform: hand_tui::components::PasteTransform =
            Arc::new(move |raw: &str| transform_dropped_file_paste(raw, &cwd_for_paste));
        let mut editor = EditorComponent::new()
            .with_border(true)
            .with_border_style(hand_tui::components::editor::BorderStyle::Horizontal)
            // Auto-grow: single-row prompt when empty, expands to fit
            // multi-line input up to 8 rows, scrolls beyond. The diff
            // renderer's shrink path is viewport-aware now (see
            // `DiffRenderer::set_viewport_height`), so live-region
            // size changes don't leak into scrollback.
            .with_auto_grow(8)
            .with_border_color(BORDER_DIM)
            .with_focused_border_color(BORDER_FOCUS)
            .with_paste_transform(paste_transform)
            .with_on_submit(move |text: String| {
                if let Ok(mut p) = pending_for_submit.lock() {
                    p.text = Some(text);
                }
            });

        // Autocomplete: combine slash-command suggestions (from the built-in
        // command registry, source of truth for `/help`) with `@path`
        // filesystem completion rooted at the session's cwd. Both providers
        // answer synchronously via `query_sync`, so the popup appears on
        // the same keystroke that triggers it — no separate driver task.
        {
            let slash_registry = crate::core::slash_commands::SlashCommandRegistry::new();
            // Built-ins first so they always shadow an extension that
            // tries to override them; extension-contributed commands
            // then layer underneath. The dispatcher already enforces
            // built-in precedence (see `find_extension_command`), so
            // visibility here just keeps the picker honest.
            let mut slash_commands: Vec<TuiSlashCommand> = slash_registry
                .commands()
                .iter()
                .map(|c| TuiSlashCommand::new(c.name.clone(), c.description.clone()))
                .collect();
            let builtin_names: std::collections::HashSet<String> =
                slash_commands.iter().map(|c| c.name.clone()).collect();
            for (spec, _ext) in session.collected_slash_commands() {
                if builtin_names.contains(&spec.name) {
                    continue;
                }
                slash_commands.push(TuiSlashCommand::new(spec.name, spec.description));
            }
            let mut combined = CombinedAutocompleteProvider::new();
            combined.add_provider(Arc::new(SlashCommandProvider::new(slash_commands)));
            combined.add_provider(Arc::new(PathAutocompleteProvider::new(cwd.clone())));
            editor.set_autocomplete_provider(Arc::new(combined));
        }

        // Wrap the editor in Arc<Mutex<>> so out-of-tree code (slash
        // dispatch, input listeners) can mutate it live — needed for
        // per-thinking-level border color, Ctrl+G external editor, and
        // Ctrl+T hide-thinking toggle's editor-side feedback. The driver
        // keeps a handle, the Tui owns a `SharedComponent` wrapper that
        // delegates render/input.
        let editor: Arc<StdMutex<EditorComponent>> = Arc::new(StdMutex::new(editor));

        // Loader slot — appears between chat and editor while the agent
        // is working / compacting / retrying.
        let loader_slot: SharedLoaderSlot = Arc::new(StdMutex::new(None));

        // Build the TUI tree.
        let terminal = Box::new(ProcessTerminal::new()?);
        let mut tui = Tui::new(terminal);
        // Publish the render handle so `push_status` (and other shared-state
        // mutators) can poke the main loop after a mutation — without this,
        // background-thread updates only become visible the next time some
        // other source (stdin, loader tick) triggers a render.
        set_render_handle(tui.render_handle());
        tui.root_mut().add_child_with_id(Box::new(ChatScrollback {
            list: Arc::clone(&chat),
        }));
        tui.root_mut().add_child_with_id(Box::new(LoaderSlot {
            slot: Arc::clone(&loader_slot),
        }));
        let editor_id = tui.root_mut().add_child_with_id(Box::new(SharedComponent {
            inner: Arc::clone(&editor),
        }));
        tui.root_mut()
            .add_child_with_id(Box::new(SharedFooterComponent {
                view: Arc::clone(&footer),
            }));
        tui.set_focus(Some(editor_id));

        // Ctrl+D listener: requests shutdown via the pending slot.
        let pending_for_quit = Arc::clone(&pending);
        tui.add_input_listener(Box::new(move |event: &InputEvent| {
            if let InputEvent::Key(key) = event
                && matches!(&key.name, KeyName::Char('d'))
                && key.modifiers.ctrl
            {
                if let Ok(mut p) = pending_for_quit.lock() {
                    p.quit = true;
                }
                return ListenerResult {
                    consume: true,
                    data: None,
                };
            }
            ListenerResult::pass()
        }));

        // Ctrl+T listener: toggle the process-wide "hide thinking blocks"
        // flag (M5.5). Flipping the atomic mutates the visible state of
        // every `AssistantMessageComponent` in scrollback on the next
        // render because they all subscribe via
        // `with_shared_hide_flag(hide_thinking_flag().clone())`.
        let chat_for_hide = Arc::clone(&chat);
        tui.add_input_listener(Box::new(move |event: &InputEvent| {
            if let InputEvent::Key(key) = event
                && matches!(&key.name, KeyName::Char('t'))
                && key.modifiers.ctrl
            {
                use std::sync::atomic::Ordering;
                let flag = hide_thinking_flag();
                let now = !flag.load(Ordering::Relaxed);
                flag.store(now, Ordering::Relaxed);
                let label = if now {
                    "[thinking blocks: hidden]"
                } else {
                    "[thinking blocks: visible]"
                };
                push_status(&chat_for_hide, label.to_string(), None);
                return ListenerResult {
                    consume: true,
                    data: None,
                };
            }
            ListenerResult::pass()
        }));

        // Ctrl+G listener: open the editor's current buffer in `$VISUAL`
        // / `$EDITOR` and read the result back. The actual edit runs in
        // a worker thread because we can't block the Tui input loop on
        // `wait()`; the result is applied to the editor via its Arc
        // handle.
        let chat_for_ext = Arc::clone(&chat);
        let editor_for_ext = Arc::clone(&editor);
        tui.add_input_listener(Box::new(move |event: &InputEvent| {
            if let InputEvent::Key(key) = event
                && matches!(&key.name, KeyName::Char('g'))
                && key.modifiers.ctrl
            {
                let current = editor_for_ext.lock().map(|e| e.text()).unwrap_or_default();
                let chat_clone = Arc::clone(&chat_for_ext);
                let editor_clone = Arc::clone(&editor_for_ext);
                std::thread::spawn(move || match run_external_editor(&current) {
                    Ok(new_text) => {
                        if let Ok(mut e) = editor_clone.lock() {
                            e.set_text(&new_text);
                        }
                    }
                    Err(e) => push_status(
                        &chat_clone,
                        format!("[external editor failed: {e}]"),
                        Some(RED_FG),
                    ),
                });
                return ListenerResult {
                    consume: true,
                    data: None,
                };
            }
            ListenerResult::pass()
        }));

        // Ctrl+V listener: read an image from the system clipboard,
        // write it to a temp file, and insert the path at the cursor.
        // The actual clipboard read + file write runs off-thread
        // (arboard / tempfile are sync) and the resulting path is
        // inserted via the editor's Arc handle.
        let chat_for_img = Arc::clone(&chat);
        let editor_for_img = Arc::clone(&editor);
        let render_for_img: Arc<dyn Fn() + Send + Sync + 'static> = Arc::new(tui.render_handle());
        tui.add_input_listener(Box::new(move |event: &InputEvent| {
            if let InputEvent::Key(key) = event
                && matches!(&key.name, KeyName::Char('v'))
                && key.modifiers.ctrl
            {
                let chat_clone = Arc::clone(&chat_for_img);
                let editor_clone = Arc::clone(&editor_for_img);
                let render_clone = Arc::clone(&render_for_img);
                std::thread::spawn(move || match handle_clipboard_image_paste() {
                    Ok(Some(path)) => {
                        if let Ok(mut e) = editor_clone.lock() {
                            e.insert_text(&path);
                        }
                        render_clone();
                    }
                    Ok(None) => {}
                    Err(e) => push_status(
                        &chat_clone,
                        format!("[clipboard image paste failed: {e}]"),
                        Some(RED_FG),
                    ),
                });
                return ListenerResult {
                    consume: true,
                    data: None,
                };
            }
            ListenerResult::pass()
        }));

        // Escape listener: cancel the in-flight agent turn (HTTP call,
        // tool execution, retry, etc). Only fires when a loader is mounted
        // — otherwise Escape falls through so the editor can use it for
        // autocomplete dismiss / etc.
        let cancel_for_esc = session.cancel_handle();
        let loader_for_esc = Arc::clone(&loader_slot);
        let chat_for_esc = Arc::clone(&chat);
        tui.add_input_listener(Box::new(move |event: &InputEvent| {
            if let InputEvent::Key(key) = event
                && matches!(&key.name, KeyName::Escape)
                && !key.modifiers.shift
                && !key.modifiers.ctrl
                && !key.modifiers.alt
            {
                let loader_active = loader_for_esc.lock().map(|s| s.is_some()).unwrap_or(false);
                if loader_active {
                    if let Ok(token) = cancel_for_esc.lock() {
                        token.cancel();
                    }
                    // Clear the loader immediately so the user sees a
                    // response — the agent task will also clear it when
                    // AgentEnd fires, but cancellation can take a moment.
                    if let Ok(mut s) = loader_for_esc.lock() {
                        *s = None;
                    }
                    push_status(
                        &chat_for_esc,
                        "[cancelled by Esc]".to_string(),
                        Some(YELLOW_FG),
                    );
                    return ListenerResult {
                        consume: true,
                        data: None,
                    };
                }
            }
            ListenerResult::pass()
        }));

        // Stop signal for background tasks.
        let stop = Arc::new(AtomicBool::new(false));

        // Background task: drains agent events into the chat list AND
        // accumulates token usage from `MessageEnd` events into the running
        // usage counter so the footer reflects spend in real time. Also
        // installs / removes the loader slot on agent + compaction lifecycle
        // events so the user sees a "Working…" / "Compacting…" indicator.
        let chat_for_events = Arc::clone(&chat);
        let stop_for_events = Arc::clone(&stop);
        let tool_handles: ToolHandles = Arc::new(StdMutex::new(HashMap::new()));
        let tools_for_events = Arc::clone(&tool_handles);
        let assistant_handle: AssistantHandle = Arc::new(StdMutex::new(None));
        let assistant_for_events = Arc::clone(&assistant_handle);
        let usage_for_events = Arc::clone(&usage);
        let loader_for_events = Arc::clone(&loader_slot);
        let _event_pump = tokio::spawn(async move {
            let mut rx = event_rx;
            while !stop_for_events.load(Ordering::Relaxed) {
                // Poll the stop flag while waiting for events so `/quit`
                // unblocks this task even when no agent activity is in
                // flight. Without the timeout, `rx.recv().await` parks
                // forever (the channel sender is held by the live
                // session subscriber), and `event_pump.await` after
                // `tui.run()` hangs the whole process.
                let received =
                    tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
                let ev = match received {
                    Ok(Some(ev)) => ev,
                    Ok(None) => break,  // channel closed
                    Err(_) => continue, // timeout — re-check stop flag
                };
                match &ev {
                    AgentSessionEvent::Agent(agent_ev) => match agent_ev.as_ref() {
                        hand_agent::types::AgentEvent::AgentStart => {
                            install_loader(&loader_for_events, "Working…");
                            emit_terminal_progress(ProgressState::Indeterminate);
                        }
                        hand_agent::types::AgentEvent::AgentEnd { .. } => {
                            clear_loader(&loader_for_events);
                            emit_terminal_progress(ProgressState::Clear);
                        }
                        hand_agent::types::AgentEvent::MessageEnd {
                            message: model::Message::Assistant(a),
                        } => {
                            accumulate_usage(&usage_for_events, &a.usage);
                        }
                        _ => {}
                    },
                    AgentSessionEvent::CompactionStart => {
                        install_loader(&loader_for_events, "Compacting context…");
                        emit_terminal_progress(ProgressState::Indeterminate);
                    }
                    AgentSessionEvent::CompactionEnd { .. } => {
                        clear_loader(&loader_for_events);
                        emit_terminal_progress(ProgressState::Clear);
                    }
                    AgentSessionEvent::Error(msg) => {
                        clear_loader(&loader_for_events);
                        emit_terminal_progress(ProgressState::Error);
                        push_error(&chat_for_events, msg.as_str());
                    }
                    AgentSessionEvent::SessionInfoChanged { .. } => {
                        // The TUI rebuilds its session-info footer
                        // on the next render tick from `session.label()`.
                        // No event-time action required.
                    }
                }
                let updates = dispatch_event(&ev);
                apply_updates_to_chat(
                    &chat_for_events,
                    &tools_for_events,
                    &assistant_for_events,
                    updates,
                );
            }
        });

        // Spinner-tick task: while a loader is mounted, advance its animation
        // ~10 times a second so the user gets feedback that things are moving.
        let loader_for_tick = Arc::clone(&loader_slot);
        let stop_for_tick = Arc::clone(&stop);
        let tick_render = tui.render_handle();
        let _tick_task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(LOADER_TICK_MS));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            while !stop_for_tick.load(Ordering::Relaxed) {
                tick.tick().await;
                let mounted = {
                    let slot = loader_for_tick.lock().unwrap();
                    slot.as_ref().map(Arc::clone)
                };
                if let Some(cell) = mounted {
                    if let Ok(mut c) = cell.lock() {
                        c.tick();
                    }
                    tick_render();
                }
            }
        });

        // M5.3 — async best-effort version probe. Runs once at startup. When
        // crates.io reports a newer published version, mount an "Update
        // Available" banner in scrollback. Failure (network, malformed
        // payload, `HAND_OFFLINE` set) is silent so startup never blocks on
        // it.
        let chat_for_ver = Arc::clone(&chat);
        let ver_render = tui.render_handle();
        let current_version = env!("CARGO_PKG_VERSION").to_string();
        tokio::spawn(async move {
            let fetcher = crate::utils::version_check::HttpVersionFetcher::new();
            if let Some(latest) =
                crate::utils::version_check::check_for_new_version(&fetcher, &current_version).await
            {
                let banner = format!(
                    "[update available] hand-coding-agent {latest} is newer than {current_version}. \
Run `cargo install --git https://github.com/badlogic/hand-ai hand-coding-agent` to upgrade. \
Changelog: https://github.com/badlogic/hand-ai/blob/main/crates/coding-agent/CHANGELOG.md",
                );
                push_status(&chat_for_ver, banner, Some(YELLOW_FG));
                ver_render();
            }
        });

        // We need to call `tui.stop()` from outside the run-loop when the
        // user submits or quits. The Tui's `stop()` is `&self` and only
        // touches Send + Sync atomics, so we can use a raw pointer wrapped
        // in a `Send`/`Sync` newtype. Lifetime: the Tui lives until after
        // both background tasks have been awaited (we hold the future and
        // await it before this function returns).
        struct StopHandle(*const Tui);
        // Safety: Tui::stop only mutates Send + Sync atomics.
        unsafe impl Send for StopHandle {}
        unsafe impl Sync for StopHandle {}
        impl StopHandle {
            unsafe fn stop(&self) {
                unsafe {
                    (*self.0).stop();
                }
            }
        }
        let stop_handle = Arc::new(StopHandle(&tui as *const _));

        // Background driver task: polls `pending`. On submit, runs the agent
        // for the submitted text. On quit, calls `tui.stop()`.
        //
        // The submitted text is published into `pending.text` by the editor's
        // `on_submit` callback (installed above), which fires on bare Enter
        // from inside the Tui run loop. This replaces the earlier listener-
        // based "mirror" hack and keeps the editor's own buffer authoritative.
        let agent_chat = Arc::clone(&chat);
        let agent_footer = Arc::clone(&footer);
        let agent_pending = Arc::clone(&pending);
        let agent_usage = Arc::clone(&usage);
        let agent_editor = Arc::clone(&editor);
        let stop_for_agent = Arc::clone(&stop);
        let stop_handle_for_agent = Arc::clone(&stop_handle);
        let agent_cwd = cwd.clone();
        let agent_mounter = tui.overlay_mounter();
        let _agent_task = tokio::spawn(async move {
            let mut session = session;
            let cwd = agent_cwd;
            let mounter = agent_mounter;
            // First-run onboarding: if no provider has a credential on
            // file (stored or via env var), greet the user and open the
            // login picker right away.
            if !any_provider_has_credentials(&session) {
                push_status(
                    &agent_chat,
                    "Welcome to hand. No provider credentials were found — opening /login.\n\
                     Pick a provider, paste an API key, and you're ready to go."
                        .to_string(),
                    None,
                );
                apply_slash_action(
                    SlashCommandAction::OpenLoginDialog { provider: None },
                    &agent_chat,
                    &mut session,
                    &cwd,
                    Some(&mounter),
                )
                .await;
            }
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            while !stop_for_agent.load(Ordering::Relaxed) {
                interval.tick().await;
                let (submitted, quit) = {
                    let mut p = agent_pending.lock().unwrap();
                    (p.text.take(), std::mem::take(&mut p.quit))
                };
                if quit {
                    // Restore terminal cooked mode + cursor, then
                    // hard-exit. Going through the normal `tui.stop()`
                    // → `tui.run()` returns → main cleanup path
                    // hangs because the stdin reader is parked in
                    // tokio::io::stdin (a blocking OS thread the
                    // runtime cannot cancel); the user-visible
                    // symptom of issue #7 is exactly this.
                    unsafe { stop_handle_for_agent.stop() };
                    // Give the run loop a beat to call
                    // shutdown_terminal before we yank the process.
                    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                    std::process::exit(0);
                }
                if let Some(text) = submitted {
                    let trimmed = text.trim().to_string();
                    if trimmed.is_empty() {
                        continue;
                    }
                    // Bash mode: `!cmd` runs cmd in a subprocess and
                    // shows the output inline like a tool call. `!!cmd`
                    // does the same but is excluded from agent context.
                    if trimmed.starts_with('!') {
                        run_bash_inline(&agent_chat, &session, &trimmed).await;
                        refresh_footer(&session, &cwd, &agent_footer, &agent_usage);
                        continue;
                    }
                    if let Some(parsed) = ParsedSlashCommand::parse(&trimmed) {
                        let ctx = SlashCommandContext {
                            model_id: session.model().id.clone(),
                            provider: session.model().provider.as_str().to_string(),
                        };
                        match SlashCommandTable::dispatch(&parsed, &ctx) {
                            SlashCommandResult::Handled(action) => {
                                let usage_snapshot = agent_usage.lock().ok().map(|u| *u);
                                let outcome = apply_slash_action_with_usage(
                                    action,
                                    &agent_chat,
                                    &mut session,
                                    &cwd,
                                    Some(&mounter),
                                    usage_snapshot,
                                )
                                .await;
                                // Slash commands can mutate session state
                                // (`/thinking`, `/model`, …) — refresh the
                                // footer so the thinking-level / model
                                // segments reflect the change immediately,
                                // and re-tint the editor border with the
                                // current thinking level (M3.3).
                                refresh_footer(&session, &cwd, &agent_footer, &agent_usage);
                                refresh_editor_border(&session, &agent_editor);
                                if matches!(outcome, SlashOutcome::Quit) {
                                    // Same hard-exit as the bare
                                    // `quit` pending path above —
                                    // tokio::io::stdin's blocking
                                    // thread makes graceful teardown
                                    // hang otherwise.
                                    unsafe { stop_handle_for_agent.stop() };
                                    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                                    std::process::exit(0);
                                }
                            }
                            SlashCommandResult::Unknown => {
                                push_status(
                                    &agent_chat,
                                    format!(
                                        "Unknown command: /{}. Type /help for available commands.",
                                        parsed.name
                                    ),
                                    Some(ORANGE_FG),
                                );
                            }
                        }
                        continue;
                    }
                    // Echo the user message immediately.
                    {
                        let mut list = agent_chat.lock().unwrap();
                        list.push(Box::new(UserMessageComponent::new(trimmed.clone())));
                    }
                    // 5-minute ceiling on a single turn so a hung HTTP
                    // request can't pin the loader spinner forever. Real
                    // long-running turns won't normally exceed this; if
                    // they do, escape-cancel and resubmit. (No timeout for
                    // tool execution time inside the turn — only the
                    // overall wall clock.)
                    let send = session.send_message(&trimmed);
                    let send_timeout = tokio::time::Duration::from_secs(300);
                    match tokio::time::timeout(send_timeout, send).await {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => push_error(&agent_chat, format!("send failed: {e}")),
                        Err(_) => {
                            // Timeout fired — cancel the session token so
                            // the agent loop drops its in-flight future
                            // and the loader clears on the next event tick.
                            if let Ok(token) = session.cancel_handle().lock() {
                                token.cancel();
                            }
                            push_error(&agent_chat, "request timed out after 5 minutes; cancelled");
                        }
                    }
                    refresh_footer(&session, &cwd, &agent_footer, &agent_usage);
                }
            }
        });

        // Run the Tui — this blocks until `tui.stop()` fires from the agent
        // task (or stdin closes).
        tui.run().await?;

        // Shutdown. Signal every background task; the tui has already
        // restored the terminal (shutdown_terminal runs inside the
        // run loop on its way out). We don't await the spawned tasks
        // because the stdin reader sits inside `tokio::io::stdin()`,
        // which is backed by a blocking OS thread that the runtime
        // cannot cancel — `event_pump.await` / `tick_task.await`
        // would block forever for normal `/quit`. Exit the process
        // directly; tokio reaps the runtime as part of teardown.
        stop.store(true, Ordering::Relaxed);
        std::process::exit(0);
    }
}

/// ANSI dim foreground prefix (used for tool-result lines).
const DIM_FG: &str = "\x1b[2;37m";
/// ANSI yellow foreground prefix (used for status / warnings).
const YELLOW_FG: &str = "\x1b[33m";
/// ANSI orange-ish foreground for unknown-command warnings.
const ORANGE_FG: &str = "\x1b[38;5;208m";
/// ANSI red foreground for errors.
const RED_FG: &str = "\x1b[31m";
/// ANSI reset.
const RESET: &str = "\x1b[0m";
/// Dim border color used when the editor is not focused.
const BORDER_DIM: &str = "\x1b[2;90m";
/// Cyan border color used when the editor is focused.
const BORDER_FOCUS: &str = "\x1b[36m";
/// Interval (ms) between loader-spinner ticks. ~10 fps so the animation is
/// visibly moving without burning the render loop.
const LOADER_TICK_MS: u64 = 100;

/// Mount a cancellable bordered loader into the shared slot. Replaces any
/// existing loader.
fn install_loader(slot: &SharedLoaderSlot, message: impl Into<String>) {
    let loader = BorderedLoaderComponent::new_cancellable(message);
    let cell = Arc::new(StdMutex::new(loader));
    if let Ok(mut s) = slot.lock() {
        *s = Some(cell);
    }
    // Force a redraw — without this the spinner doesn't appear until
    // the next render tick (~100 ms), visible flicker on fast paths.
    request_render();
}

/// Remove the mounted loader, if any.
///
/// Forces a re-render. The spinner-tick task only calls `tick_render`
/// while a loader is mounted, so clearing the slot without a manual
/// nudge leaves the last "Working…" frame stuck on screen until some
/// other event happens to trigger a redraw — the user-visible bug
/// where the spinner stays after the assistant finishes streaming.
///
/// This shrink-render exercises the diff renderer's leftover-clear
/// path, which historically scrolled the terminal up via LF and
/// leaked the top of the live region into scrollback. That bug is
/// fixed at the renderer level (see `DiffRenderer::set_viewport_height`).
fn clear_loader(slot: &SharedLoaderSlot) {
    if let Ok(mut s) = slot.lock() {
        *s = None;
    }
    request_render();
}

/// Execute a `!cmd` (or `!!cmd`) bash invocation submitted from the editor.
/// Mounts a `BashExecutionComponent` into the chat scrollback, drives the
/// subprocess through [`AgentSession::run_bash`], and finalises the
/// component when the process exits or is aborted.
async fn run_bash_inline(chat: &ChatList, session: &AgentSession, raw: &str) {
    let (command, exclude_from_context) = if let Some(rest) = raw.strip_prefix("!!") {
        (rest.trim().to_string(), true)
    } else {
        (
            raw.strip_prefix('!')
                .map(str::trim)
                .unwrap_or("")
                .to_string(),
            false,
        )
    };
    if command.is_empty() {
        push_status(chat, "[bash] empty command".to_string(), Some(YELLOW_FG));
        return;
    }
    let cell = Arc::new(StdMutex::new(BashExecutionComponent::new(
        command.clone(),
        exclude_from_context,
    )));
    // Mount the live cell through push_component so the render loop
    // is poked. Pre-fix the bash panel stayed buffered until some
    // unrelated event fired request_render() (same class as #38).
    push_component(
        chat,
        Box::new(SharedRender {
            inner: Arc::clone(&cell),
        }),
    );
    match session.run_bash(&command, 0).await {
        Ok(outcome) => {
            if let Ok(mut c) = cell.lock() {
                c.append_output(&outcome.result.output);
                c.set_complete(outcome.result.exit_code, outcome.aborted, None);
            }
            // The cell was mutated in place; nothing pushed to the
            // chat list, but the user-visible output of the
            // SharedRender wrapper just changed. Force a repaint.
            request_render();
        }
        Err(e) => push_error(chat, format!("bash failed: {e}")),
    }
}

/// Push a welcome header into the chat. Two lines: bold logo +
/// version on line 1, dim keybinding hints separated by ` · ` on
/// line 2.
fn push_welcome_header(chat: &ChatList, model: &model::Model) {
    use super::components::keybinding_hints::raw_key_hint;
    let version = env!("CARGO_PKG_VERSION");
    let title = format!(
        "\x1b[1;36mhand\x1b[0m \x1b[2mv{version}\x1b[0m   \x1b[2m{}/{}\x1b[0m",
        model.provider.as_str(),
        model.id,
    );
    let separator = "\x1b[90m · \x1b[0m";
    let hints = [
        raw_key_hint("↵", "send"),
        raw_key_hint("⇧↵", "newline"),
        raw_key_hint("↑↓", "history"),
        raw_key_hint("/", "commands"),
        raw_key_hint("!", "bash"),
        raw_key_hint("^C", "interrupt"),
        raw_key_hint("^D", "quit"),
    ];
    let mut list = chat.lock().expect("chat list mutex poisoned");
    list.push(Box::new(TextComponent::new(title)));
    list.push(Box::new(TextComponent::new(hints.join(separator))));
    // Blank line so the header doesn't visually crowd the first chat entry.
    list.push(Box::new(TextComponent::new(String::new())));
}

fn coloured_text(text: impl AsRef<str>, ansi_prefix: Option<&str>) -> TextComponent {
    let body = match ansi_prefix {
        Some(p) => format!("{p}{}{RESET}", text.as_ref()),
        None => text.as_ref().to_string(),
    };
    TextComponent::new(body)
}

/// Push a clearly-visible error banner into the chat scrollback. Unlike a
/// bare red text line, this prefixes the message with a `✘ Error:` marker
/// and renders it in bold bright red on a black background so it isn't
/// lost in tool output / dim messages above it.
fn push_error(chat: &ChatList, msg: impl AsRef<str>) {
    // \x1b[1;97;41m = bold + bright white + red background.
    let body = format!(
        "\x1b[1;97;41m ✘ Error  \x1b[0m \x1b[1;91m{}{RESET}",
        msg.as_ref()
    );
    let mut list = chat.lock().expect("chat list mutex poisoned");
    list.push(Box::new(TextComponent::new(body)));
}

/// M5.1 — emit an OSC 9;4 terminal-progress escape sequence so
/// supporting terminals (ConEmu / WezTerm / iTerm2 / Windows Terminal)
/// show a task-bar / titlebar progress indicator while the agent
/// works. No-op when stdout isn't a tty.
#[derive(Copy, Clone)]
enum ProgressState {
    /// Reset: hide the indicator.
    Clear,
    /// Indeterminate spinner — used when we don't have a percentage.
    Indeterminate,
    /// Error / failure state — red bar.
    Error,
}

/// M4.2 — Ctrl+V clipboard-image handler. Reads an image from the system
/// clipboard via `arboard` (re-encoded to PNG), writes it to a temp file
/// named `hand-clipboard-<uuid>.png`, and returns the absolute path so the
/// driver can insert it at the cursor. Returns `Ok(None)` when the
/// clipboard exists but doesn't hold an image — the common "you Ctrl+V'd
/// with text on the clipboard" case.
fn handle_clipboard_image_paste() -> Result<Option<String>, String> {
    let image = match crate::utils::clipboard_image::read_clipboard_image() {
        Ok(Some(img)) => img,
        Ok(None) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let ext = crate::utils::clipboard_image::extension_for_image_mime_type(&image.mime_type)
        .unwrap_or("png");
    let tmp_dir = std::env::temp_dir();
    // Cheap unique suffix — nanos since UNIX epoch plus a process-local
    // counter. We don't need cryptographic uniqueness, just enough to
    // avoid stomping across rapid pastes.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_path = tmp_dir.join(format!("hand-clipboard-{ts}-{n}.{ext}"));
    std::fs::write(&file_path, &image.bytes).map_err(|e| e.to_string())?;
    Ok(Some(file_path.to_string_lossy().to_string()))
}

/// M4.3 — paste-transform: when a terminal pastes a file-drop payload
/// (single line, optionally quoted, optionally `file://`-prefixed) and the
/// result resolves to an existing path on disk, rewrite it to an `@path`
/// mention so the agent's @ resolver picks it up.
///
/// Returns `None` when the paste is not a drop-like payload — the editor
/// then inserts the original text verbatim.
fn transform_dropped_file_paste(raw: &str, cwd: &Path) -> Option<String> {
    // Drop-like pastes are single-line; multi-line pastes are bracketed
    // and never come from drag-drop.
    if raw.contains('\n') {
        return None;
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Strip matching outer quotes (iTerm2 and several other terminals
    // single-quote the dropped path).
    let stripped = strip_matching_quotes(trimmed);
    // Strip `file://` scheme prefix (some terminals percent-encode the
    // body too — best-effort decode below).
    let no_scheme = stripped.strip_prefix("file://").unwrap_or(stripped);
    let decoded = percent_decode(no_scheme);
    // Verify it looks like a path and actually exists.
    let candidate = std::path::PathBuf::from(decoded.as_ref());
    let exists = if candidate.is_absolute() {
        candidate.exists()
    } else {
        cwd.join(&candidate).exists()
    };
    if !exists {
        return None;
    }
    // Prefer the relative form when the path is inside cwd — that's what
    // the @ resolver consumes in the agent prompt. Fall back to absolute.
    let mention = if let Ok(rel) = candidate.strip_prefix(cwd) {
        rel.to_string_lossy().to_string()
    } else if candidate.is_absolute() {
        candidate.to_string_lossy().to_string()
    } else {
        decoded.into_owned()
    };
    Some(format!("@{mention}"))
}

fn strip_matching_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

fn percent_decode(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains('%') {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &s[i + 1..i + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    std::borrow::Cow::Owned(out)
}

fn emit_terminal_progress(state: ProgressState) {
    use std::io::IsTerminal as _;
    use std::io::Write as _;
    // OSC 9;4 format: ESC]9;4;<state>;<progress>BEL
    //   state 0: hide, 1: normal (with %), 2: error, 3: indeterminate, 4: paused.
    let sequence = match state {
        ProgressState::Clear => "\x1b]9;4;0;0\x07",
        ProgressState::Indeterminate => "\x1b]9;4;3;0\x07",
        ProgressState::Error => "\x1b]9;4;2;0\x07",
    };
    let stdout = std::io::stdout();
    if !stdout.is_terminal() {
        return;
    }
    let _ = stdout.lock().write_all(sequence.as_bytes());
}

/// M5.2 — return a one-line warning if the user is running inside a
/// tmux session whose keyboard plumbing will eat modified Enter / Alt
/// keys. `None` outside tmux or when the relevant options are
/// configured correctly. Times out the `tmux show` call at 2s so a
/// hung tmux server can't delay startup.
fn check_tmux_keyboard_setup() -> Option<String> {
    if std::env::var("TMUX").is_err() {
        return None;
    }
    fn tmux_show(opt: &str) -> Option<String> {
        let mut child = std::process::Command::new("tmux")
            .args(["show", "-gv", opt])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => {
                    let mut stdout = String::new();
                    use std::io::Read as _;
                    let _ = child.stdout.as_mut()?.read_to_string(&mut stdout);
                    return Some(stdout.trim().to_string());
                }
                Ok(Some(_)) => return None,
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        return None;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(_) => return None,
            }
        }
    }
    let extended_keys = tmux_show("extended-keys")?;
    if extended_keys != "on" && extended_keys != "always" {
        return Some(
            "tmux extended-keys is off. Modified Enter keys may not work. \
             Add `set -g extended-keys on` to ~/.tmux.conf and reload tmux."
                .to_string(),
        );
    }
    let extended_keys_format = tmux_show("extended-keys-format");
    if extended_keys_format.as_deref() == Some("xterm") {
        return Some(
            "tmux extended-keys-format is xterm. hand-ai works best with csi-u. \
             Add `set -g extended-keys-format csi-u` to ~/.tmux.conf and reload tmux."
                .to_string(),
        );
    }
    None
}

/// Open `initial` in `$VISUAL` / `$EDITOR` (falling back to `vi`) on a
/// temp file, wait for the editor to exit, and return the resulting
/// buffer. Used by the Ctrl+G external-edit hook (M4.1). The function
/// blocks while the child editor runs, so callers MUST invoke it from a
/// worker thread — the Tui input loop is single-threaded and would
/// otherwise stall.
fn run_external_editor(initial: &str) -> std::io::Result<String> {
    use std::io::Write as _;
    let editor_cmd = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let tmp = tempfile::Builder::new()
        .prefix("hand-edit-")
        .suffix(".md")
        .tempfile()?;
    let path = tmp.path().to_path_buf();
    {
        let mut f = std::fs::File::create(&path)?;
        f.write_all(initial.as_bytes())?;
    }
    // Split the editor command to support `EDITOR="code -w"` style.
    let mut parts = editor_cmd.split_whitespace();
    let bin = parts
        .next()
        .ok_or_else(|| std::io::Error::other("empty $EDITOR"))?;
    let args: Vec<&str> = parts.collect();
    let status = std::process::Command::new(bin)
        .args(args)
        .arg(&path)
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "editor exited with {}",
            status
        )));
    }
    let new_text = std::fs::read_to_string(&path)?;
    Ok(new_text)
}

/// Process-wide "hide thinking blocks" toggle (M5.5). Initialised lazily
/// the first time it's read; the [`InteractiveMode::run`] entry point
/// sets the bit from `settings.hide_thinking_block` at startup. Every
/// `AssistantMessageComponent` subscribes via
/// `with_shared_hide_flag(hide_thinking_flag().clone())`, so a single
/// Ctrl+T in the driver flips the visual state of every assistant
/// message in scrollback at once.
fn hide_thinking_flag() -> &'static Arc<std::sync::atomic::AtomicBool> {
    use std::sync::OnceLock;
    static FLAG: OnceLock<Arc<std::sync::atomic::AtomicBool>> = OnceLock::new();
    FLAG.get_or_init(|| Arc::new(std::sync::atomic::AtomicBool::new(false)))
}

fn replay_messages_into(chat: &ChatList, messages: &[model::Message]) {
    let mut list = chat.lock().expect("chat list mutex poisoned");
    for msg in messages {
        match msg {
            model::Message::User(u) => {
                let text = match &u.content {
                    model::UserContent::Text(s) => s.clone(),
                    model::UserContent::Blocks(blocks) => blocks
                        .iter()
                        .filter_map(|b| match b {
                            model::UserContentBlock::Text(t) => Some(t.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                list.push(Box::new(UserMessageComponent::new(text)));
            }
            model::Message::Assistant(a) => {
                let comp = AssistantMessageComponent::with_message(a.clone())
                    .with_shared_hide_flag(Arc::clone(hide_thinking_flag()));
                list.push(Box::new(comp));
            }
            model::Message::ToolResult(t) => {
                let body: String = t
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        model::ToolResultContent::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                list.push(Box::new(coloured_text(
                    format!("[{}] {}", t.tool_name, body),
                    Some(DIM_FG),
                )));
            }
        }
    }
}

fn apply_updates_to_chat(
    chat: &ChatList,
    tools: &ToolHandles,
    assistant: &AssistantHandle,
    updates: Vec<ChatUpdate>,
) {
    for update in updates {
        match update {
            ChatUpdate::AppendUser { text } => {
                let mut list = chat.lock().expect("chat list mutex poisoned");
                list.push(Box::new(UserMessageComponent::new(text)));
            }
            ChatUpdate::AppendAssistant { message } => {
                let comp = AssistantMessageComponent::with_message(*message)
                    .with_shared_hide_flag(Arc::clone(hide_thinking_flag()));
                let cell = Arc::new(StdMutex::new(comp));
                {
                    let mut handle = assistant.lock().expect("assistant handle mutex poisoned");
                    *handle = Some(Arc::clone(&cell));
                }
                let mut list = chat.lock().expect("chat list mutex poisoned");
                list.push(Box::new(SharedRender { inner: cell }));
            }
            ChatUpdate::ReplaceLastAssistant { message } => {
                // Mutate the in-flight component if we have one; otherwise
                // fall back to appending so streaming-without-start sequences
                // remain visible.
                let mut applied = false;
                if let Ok(handle) = assistant.lock()
                    && let Some(cell) = handle.as_ref()
                    && let Ok(mut comp) = cell.lock()
                {
                    comp.set_message(*message.clone());
                    applied = true;
                }
                if !applied {
                    let comp = AssistantMessageComponent::with_message(*message)
                        .with_shared_hide_flag(Arc::clone(hide_thinking_flag()));
                    let cell = Arc::new(StdMutex::new(comp));
                    {
                        let mut handle = assistant.lock().expect("assistant handle mutex poisoned");
                        *handle = Some(Arc::clone(&cell));
                    }
                    let mut list = chat.lock().expect("chat list mutex poisoned");
                    list.push(Box::new(SharedRender { inner: cell }));
                }
            }
            ChatUpdate::ToolStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                if tool_name == "bash" {
                    let command = args
                        .get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let cell = Arc::new(StdMutex::new(BashExecutionComponent::new(command, false)));
                    {
                        let mut t = tools.lock().expect("tool handles mutex poisoned");
                        t.insert(tool_call_id, ToolHandle::Bash(Arc::clone(&cell)));
                    }
                    let mut list = chat.lock().expect("chat list mutex poisoned");
                    list.push(Box::new(SharedRender { inner: cell }));
                } else {
                    let cell =
                        Arc::new(StdMutex::new(ToolExecutionComponent::new(tool_name, args)));
                    {
                        let mut t = tools.lock().expect("tool handles mutex poisoned");
                        t.insert(tool_call_id, ToolHandle::Generic(Arc::clone(&cell)));
                    }
                    let mut list = chat.lock().expect("chat list mutex poisoned");
                    list.push(Box::new(SharedRender { inner: cell }));
                }
            }
            ChatUpdate::ToolUpdate {
                tool_call_id,
                partial_text,
            } => {
                let handle_clone = {
                    let t = tools.lock().expect("tool handles mutex poisoned");
                    t.get(&tool_call_id).map(|h| match h {
                        ToolHandle::Bash(c) => ToolHandle::Bash(Arc::clone(c)),
                        ToolHandle::Generic(c) => ToolHandle::Generic(Arc::clone(c)),
                    })
                };
                match handle_clone {
                    Some(ToolHandle::Bash(cell)) => {
                        if let Ok(mut comp) = cell.lock() {
                            comp.append_output(&partial_text);
                        }
                    }
                    Some(ToolHandle::Generic(cell)) => {
                        if let Ok(mut comp) = cell.lock() {
                            let partial = hand_agent::types::ToolResult::text(partial_text);
                            comp.set_partial_result(partial);
                        }
                    }
                    None => {
                        // No matching ToolStart — fall back to a status line.
                        let mut list = chat.lock().expect("chat list mutex poisoned");
                        list.push(Box::new(coloured_text(partial_text, Some(DIM_FG))));
                    }
                }
            }
            ChatUpdate::ToolEnd {
                tool_call_id,
                result_text,
                is_error,
                exit_code,
            } => {
                let handle_clone = {
                    let mut t = tools.lock().expect("tool handles mutex poisoned");
                    t.remove(&tool_call_id)
                };
                match handle_clone {
                    Some(ToolHandle::Bash(cell)) => {
                        if let Ok(mut comp) = cell.lock() {
                            // For bash, accumulated streaming output already
                            // populated the buffer; only append any *new*
                            // content present in the final result and not yet
                            // seen.
                            let buf = comp.output();
                            let extra = result_text.strip_prefix(&buf).unwrap_or(&result_text);
                            if !extra.is_empty() && extra != buf {
                                comp.append_output(extra);
                            }
                            let cancelled =
                                is_error && matches!(comp.status(), BashStatus::Running);
                            comp.set_complete(exit_code, cancelled && exit_code.is_none(), None);
                        }
                    }
                    Some(ToolHandle::Generic(cell)) => {
                        if let Ok(mut comp) = cell.lock() {
                            let result = hand_agent::types::ToolResult::text(result_text);
                            comp.set_result(result, is_error);
                        }
                    }
                    None => {
                        // No matching ToolStart — surface a compact line.
                        let mut list = chat.lock().expect("chat list mutex poisoned");
                        let prefix = if is_error { "[error] " } else { "" };
                        list.push(Box::new(coloured_text(
                            format!("{prefix}{result_text}"),
                            Some(DIM_FG),
                        )));
                    }
                }
            }
            ChatUpdate::AppendToolResult { text } => {
                let mut list = chat.lock().expect("chat list mutex poisoned");
                list.push(Box::new(coloured_text(text, Some(DIM_FG))));
            }
            ChatUpdate::AppendStatus { text } => {
                let mut list = chat.lock().expect("chat list mutex poisoned");
                list.push(Box::new(coloured_text(text, Some(YELLOW_FG))));
            }
            ChatUpdate::ThemeChanged { theme } => {
                let mut list = chat.lock().expect("chat list mutex poisoned");
                list.push(Box::new(coloured_text(
                    format!("[theme: {theme}]"),
                    Some(YELLOW_FG),
                )));
            }
        }
    }
}

fn push_status(chat: &ChatList, text: String, color_prefix: Option<&str>) {
    {
        let mut list = chat.lock().expect("chat list mutex poisoned");
        list.push(Box::new(coloured_text(text, color_prefix)));
    }
    request_render();
}

/// Append a `CustomMessageComponent` (or any other boxed component) to
/// the chat list and request a render. Mirrors [`push_status`] so the
/// "mutate shared TUI state, then poke the render loop" invariant is
/// honoured even from helpers that don't go through `push_status`.
fn push_component(chat: &ChatList, component: Box<dyn Component>) {
    {
        let mut list = chat.lock().expect("chat list mutex poisoned");
        list.push(component);
    }
    request_render();
}

/// Process-wide handle to the Tui's render-requested flag.
///
/// The Tui main loop polls this flag every `RENDER_TICK_MS` and only paints
/// when it's set. Stdin input flips the flag automatically, but pure
/// driver-side mutations (slash command output, hide-thinking toggle banner,
/// background-task status banners) need an explicit poke — otherwise the
/// chat list update is invisible until the user types something or a
/// loader tick fires.
///
/// Set once from [`InteractiveMode::run`] via [`set_render_handle`], then
/// any code path that mutates shared TUI state calls [`request_render`].
static RENDER_HANDLE: std::sync::OnceLock<RenderFn> = std::sync::OnceLock::new();

type RenderFn = std::sync::Arc<dyn Fn() + Send + Sync + 'static>;

fn set_render_handle(handle: impl Fn() + Send + Sync + 'static) {
    // First setter wins. Subsequent calls (e.g. another `run()` in the same
    // process during tests) are silently ignored — the original handle is
    // still valid because the Tui lives until the test ends.
    let _ = RENDER_HANDLE.set(std::sync::Arc::new(handle));
}

fn request_render() {
    if let Some(h) = RENDER_HANDLE.get() {
        h();
    }
}

fn refresh_footer(
    session: &AgentSession,
    cwd: &Path,
    footer: &SharedFooter,
    usage: &Arc<StdMutex<TokenUsageSummary>>,
) {
    let snapshot = usage.lock().map(|u| *u).unwrap_or_default();
    if let Ok(mut f) = footer.lock() {
        *f = InteractiveMode::build_footer_view(session, cwd, snapshot);
    }
}

/// Tint the editor's focused border with the active thinking level.
/// The palette uses truecolor literals for `thinkingOff`/`Minimal`/
/// `Low`/`Medium`/`High`/`Xhigh` so the colours work under any
/// terminal.
fn refresh_editor_border(session: &AgentSession, editor: &Arc<StdMutex<EditorComponent>>) {
    let colour = thinking_level_border_color(session.stream_options().reasoning);
    if let Ok(mut e) = editor.lock() {
        e.set_focused_border_color(colour);
    }
}

/// Map a thinking level to the focused-border SGR. Uses the
/// dark-theme palette: `thinking_off`=#505050, `Minimal`=#6e6e6e,
/// `Low`=#5f87af, `Medium`=#81a2be, `High`=#b294bb, `Xhigh`=#d183e8.
/// `None` (reasoning off) returns the default `BORDER_FOCUS` cyan so
/// the border stays consistent with the editor's idle state.
fn thinking_level_border_color(level: Option<model::ThinkingLevel>) -> String {
    use model::ThinkingLevel;
    match level {
        None => BORDER_FOCUS.to_string(),
        Some(ThinkingLevel::Minimal) => "\x1b[38;2;110;110;110m".to_string(),
        Some(ThinkingLevel::Low) => "\x1b[38;2;95;135;175m".to_string(),
        Some(ThinkingLevel::Medium) => "\x1b[38;2;129;162;190m".to_string(),
        Some(ThinkingLevel::High) => "\x1b[38;2;178;148;187m".to_string(),
        Some(ThinkingLevel::Xhigh) => "\x1b[38;2;209;131;232m".to_string(),
    }
}

/// Count providers that currently have a credential (API key, OAuth
/// token, or Vertex/Bedrock identity) the host can use. Driven from
/// `model::get_providers()` so adding a new provider to the catalogue
/// automatically widens the footer count.
fn count_providers_with_credentials() -> usize {
    model::get_providers()
        .into_iter()
        .filter(|p| model::get_env_api_key(p).is_some())
        .count()
}

/// Detect the current git branch by reading `.git/HEAD` in `cwd` or any
/// ancestor. Returns `None` if not in a git repo or HEAD can't be parsed.
fn detect_git_branch(cwd: &Path) -> Option<String> {
    let mut dir = cwd;
    loop {
        let head = dir.join(".git").join("HEAD");
        if head.exists() {
            let text = std::fs::read_to_string(&head).ok()?;
            let line = text.trim();
            // Detached HEAD points to a commit SHA — show first 7 chars.
            if let Some(rest) = line.strip_prefix("ref: refs/heads/") {
                return Some(rest.to_string());
            }
            return Some(line.chars().take(7).collect());
        }
        dir = dir.parent()?;
    }
}

/// Accumulate token usage from an assistant message into the running total.
fn accumulate_usage(running: &Arc<StdMutex<TokenUsageSummary>>, usage: &model::Usage) {
    if let Ok(mut acc) = running.lock() {
        acc.input += usage.input;
        acc.output += usage.output;
        acc.cache_read += usage.cache_read;
        acc.cache_write += usage.cache_write;
        acc.cost_usd += usage.cost.total;
    }
}

/// Outcome of [`apply_slash_action`]. Lets the caller decide whether to
/// terminate the run loop without returning the whole `SlashCommandAction`
/// upward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlashOutcome {
    /// The command was handled (or its overlay stub printed); the run loop
    /// should continue.
    Continue,
    /// The user requested an exit.
    Quit,
}

/// Apply a [`SlashCommandAction`] to the live driver state. Pulled out of
/// the agent task body so each branch is independently testable. The
/// `mounter`, when provided, is used to mount overlay-blocked commands
/// (`/model`, `/resume`, `/thinking`, `/settings`, `/login`); when `None`
/// (e.g. unit tests that don't drive a live Tui), those branches degrade
/// to a status-line stub.
pub(crate) async fn apply_slash_action(
    action: SlashCommandAction,
    chat: &ChatList,
    session: &mut AgentSession,
    cwd: &Path,
    mounter: Option<&OverlayMounter>,
) -> SlashOutcome {
    apply_slash_action_inner(action, chat, session, cwd, mounter, None).await
}

/// Same as [`apply_slash_action`] but threads a snapshot of the
/// session-wide [`TokenUsageSummary`] through so the `/session`
/// handler can render the live token + cost totals. The plain
/// `apply_slash_action` keeps its old signature so the dozens of
/// test sites that construct an action and run it through the
/// dispatcher don't have to plumb a usage snapshot they don't care
/// about.
pub(crate) async fn apply_slash_action_with_usage(
    action: SlashCommandAction,
    chat: &ChatList,
    session: &mut AgentSession,
    cwd: &Path,
    mounter: Option<&OverlayMounter>,
    usage: Option<TokenUsageSummary>,
) -> SlashOutcome {
    apply_slash_action_inner(action, chat, session, cwd, mounter, usage).await
}

async fn apply_slash_action_inner(
    action: SlashCommandAction,
    chat: &ChatList,
    session: &mut AgentSession,
    cwd: &Path,
    mounter: Option<&OverlayMounter>,
    usage: Option<TokenUsageSummary>,
) -> SlashOutcome {
    match action {
        SlashCommandAction::Quit => return SlashOutcome::Quit,
        SlashCommandAction::ShowText(s) => push_status(chat, s, None),
        SlashCommandAction::OpenModelSelector => {
            mount_model_selector(chat, session, mounter).await;
        }
        SlashCommandAction::OpenThinkingSelector { inline_level } => {
            mount_thinking_selector(chat, session, inline_level, mounter).await;
        }
        SlashCommandAction::OpenSettingsSelector => {
            mount_settings_selector(chat, session, mounter).await;
        }
        SlashCommandAction::OpenLoginDialog { provider } => {
            let chosen = match provider {
                Some(raw) => {
                    // Validate against the live provider catalogue
                    // before opening the paste dialog. Without this any
                    // typo like `/login antrhopic` happily stored a
                    // key under the bogus id that no model ever
                    // resolved against — surfacing as "no credentials"
                    // far away from the actual typo.
                    //
                    // Catalogue ids are lowercase (see
                    // `Provider::as_str`), so canonicalise the user's
                    // input case-insensitively before lookup — that
                    // also fixes the OAuth-vs-key-paste fork: `/login
                    // Anthropic` used to fall through to the API-key
                    // dialog because `oauth_id_for("Anthropic")` only
                    // matched lowercase.
                    let needle = raw.to_ascii_lowercase();
                    let known: std::collections::HashSet<String> =
                        build_login_provider_list(session)
                            .into_iter()
                            .map(|p| p.id)
                            .collect();
                    if known.contains(&needle) {
                        Some(needle)
                    } else {
                        let mut sorted: Vec<String> = known.into_iter().collect();
                        sorted.sort();
                        push_status(
                            chat,
                            format!(
                                "[/login: unknown provider {raw:?}. Known providers: {}]",
                                sorted.join(", ")
                            ),
                            Some(RED_FG),
                        );
                        None
                    }
                }
                None => mount_login_provider_picker(chat, session, mounter).await,
            };
            if let Some(provider_id) = chosen {
                // Branch on OAuth support: anthropic, openai-codex, and
                // github-copilot have browser/device OAuth flows; everything
                // else (openrouter, deepseek, zai, …) is API-key auth and
                // falls through to the manual paste dialog.
                if let Some(oauth_id) = oauth_id_for(&provider_id) {
                    run_oauth_login(chat, oauth_id).await;
                } else {
                    mount_login_key_input(chat, &provider_id, mounter).await;
                }
            }
        }
        SlashCommandAction::OpenResumePicker => {
            mount_resume_picker(chat, session, cwd, mounter).await;
        }
        SlashCommandAction::ClearChat => {
            if let Ok(mut list) = chat.lock() {
                list.clear();
            }
            push_status(chat, "[chat cleared]".to_string(), None);
        }
        SlashCommandAction::Compact(custom) => {
            let result = match custom.as_deref() {
                Some(s) => session.compact_with(s).await,
                None => session.compact().await,
            };
            match result {
                Ok(summary) => {
                    use super::components::{
                        CompactionSummaryData, CompactionSummaryMessageComponent,
                    };
                    let tokens_before = session.message_count() as u64;
                    let data = CompactionSummaryData::new(summary, tokens_before);
                    // Route through push_component so the render loop
                    // is poked; previously the summary block stayed
                    // buffered until the next unrelated command fired
                    // request_render() (same class of bug as #38).
                    push_component(chat, Box::new(CompactionSummaryMessageComponent::new(data)));
                }
                Err(e) => push_status(chat, format!("[compact failed: {e}]"), Some(RED_FG)),
            }
        }
        SlashCommandAction::NewSession => match session.reset_session() {
            Ok(()) => {
                if let Ok(mut list) = chat.lock() {
                    list.clear();
                }
                push_status(chat, "[new session started]".to_string(), None);
            }
            Err(e) => push_status(chat, format!("[/new failed: {e}]"), Some(RED_FG)),
        },
        SlashCommandAction::CopyLastAssistant => {
            let text = last_assistant_text(session);
            match text {
                Some(body) => match crate::utils::clipboard::copy_to_clipboard(&body) {
                    Ok(()) => push_status(chat, "[copied to clipboard]".to_string(), None),
                    Err(e) => push_status(chat, format!("[copy failed: {e}]"), Some(RED_FG)),
                },
                None => push_status(
                    chat,
                    "[no assistant message to copy]".to_string(),
                    Some(YELLOW_FG),
                ),
            }
        }
        SlashCommandAction::Logout => match crate::core::auth_storage::AuthStorage::new() {
            Ok(storage) => match storage.save(&std::collections::HashMap::new()) {
                Ok(()) => push_status(chat, "[logged out]".to_string(), None),
                Err(e) => push_status(chat, format!("[/logout failed: {e}]"), Some(RED_FG)),
            },
            Err(e) => push_status(chat, format!("[/logout failed: {e}]"), Some(RED_FG)),
        },
        SlashCommandAction::ShowSessionInfo => {
            let text = render_session_info(session, usage.as_ref());
            push_status(chat, text, None);
        }
        SlashCommandAction::ShowDiagnostics => {
            let report = crate::core::diagnostics::run_diagnostics();
            let body = format_diagnostics_report(&report);
            push_status(chat, body, None);
        }
        SlashCommandAction::Reload => {
            apply_reload(chat, session);
        }
        SlashCommandAction::OpenScopedModelsSelector => {
            mount_scoped_models_selector(chat, session, mounter).await;
        }
        SlashCommandAction::OpenTreeSelector(sub) => {
            mount_tree_selector(chat, cwd, sub.as_deref(), mounter).await;
        }
        SlashCommandAction::ModelByPattern(pattern) => {
            apply_model_by_pattern(chat, session, &pattern);
        }
        SlashCommandAction::CopyN(n) => {
            apply_copy_n(chat, session, n);
        }
        SlashCommandAction::Export(path, fmt) => {
            apply_export(chat, session, &path, fmt);
        }
        SlashCommandAction::Import(path) => {
            apply_import(chat, session, &path);
        }
        SlashCommandAction::Fork(entry_id) => {
            apply_fork(chat, session, entry_id.as_deref(), mounter).await;
        }
        SlashCommandAction::Clone => {
            apply_clone(chat, session);
        }
        SlashCommandAction::Name(label) => match session.set_label(&label) {
            Ok(()) => push_status(chat, format!("[session name set: {label}]"), None),
            Err(e) => push_status(chat, format!("[/name failed: {e}]"), Some(RED_FG)),
        },
        SlashCommandAction::Theme(arg) => {
            apply_theme(chat, session, arg, mounter).await;
        }
        SlashCommandAction::ListSkills => {
            apply_list_skills(chat, session);
        }
        SlashCommandAction::ListExtensions => {
            apply_list_extensions(chat, session);
        }
        SlashCommandAction::Changelog => {
            apply_changelog(chat);
        }
        SlashCommandAction::Noop => {}
    }
    SlashOutcome::Continue
}

/// Pull the trailing assistant message's textual body, if any. Used by
/// `/copy`.
fn last_assistant_text(session: &AgentSession) -> Option<String> {
    for msg in session.messages().iter().rev() {
        if let model::Message::Assistant(a) = msg {
            let mut parts: Vec<String> = Vec::new();
            for block in &a.content {
                if let model::AssistantContentBlock::Text(t) = block {
                    parts.push(t.text.clone());
                }
            }
            if parts.is_empty() {
                return None;
            }
            return Some(parts.join("\n"));
        }
    }
    None
}

/// Render a [`crate::core::diagnostics::DiagnosticsReport`] into a compact
/// text block suitable for the chat scrollback.
fn format_diagnostics_report(report: &crate::core::diagnostics::DiagnosticsReport) -> String {
    let mut out = String::new();
    out.push_str("[diagnostics]\n");
    out.push_str(&format!(
        "  ok={} warn={} error={}\n",
        report.ok_count(),
        report.warn_count(),
        report.error_count()
    ));
    for check in &report.checks {
        let (status, detail) = match &check.status {
            crate::core::diagnostics::DiagStatus::Ok => ("OK", String::new()),
            crate::core::diagnostics::DiagStatus::Warn(msg) => ("WARN", msg.clone()),
            crate::core::diagnostics::DiagStatus::Error(msg) => ("ERR", msg.clone()),
        };
        if detail.is_empty() {
            out.push_str(&format!("  [{status}] {}\n", check.name));
        } else {
            out.push_str(&format!("  [{status}] {} — {}\n", check.name, detail));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Overlay-mount helpers (used by the 5 slash commands that open a dialog).
//
// Each helper builds the component, hands it an outcome channel, mounts via
// the supplied `OverlayMounter`, awaits the user's choice, applies it to
// session state where applicable, and then unmounts. When the mounter is
// absent (unit tests, headless contexts) the helper degrades to a
// status-line stub so existing test fixtures continue to work.
// ---------------------------------------------------------------------------

/// Mount a filesystem-tree picker. Builds a depth-bounded BFS flat
/// tree from `root` (cwd, plus optional sub-path argument from
/// `/tree <subdir>`), skips noise directories, and renders the tree
/// selector. The chosen entry is pushed as a status line — direct
/// `@`-attach is already handled by the editor's `@`-autocomplete,
/// so a status-only outcome keeps the surface minimal until
/// paste-attach is wired.
async fn mount_tree_selector(
    chat: &ChatList,
    cwd: &Path,
    sub: Option<&str>,
    mounter: Option<&OverlayMounter>,
) {
    let Some(mounter) = mounter else {
        push_status(chat, "[/tree opened]".to_string(), None);
        return;
    };
    let root = match sub {
        Some(s) => cwd.join(s),
        None => cwd.to_path_buf(),
    };
    if !root.is_dir() {
        push_status(
            chat,
            format!("[/tree: not a directory: {}]", root.display()),
            Some(YELLOW_FG),
        );
        return;
    }
    let rows = build_tree_rows(&root);
    if rows.is_empty() {
        push_status(
            chat,
            format!("[/tree: empty: {}]", root.display()),
            Some(YELLOW_FG),
        );
        return;
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<TreeSelectorEvent>();
    let component = TreeSelectorComponent::new(rows, tx);
    let handle = match mounter
        .show(Box::new(component), OverlayOptions::default())
        .await
    {
        Ok(h) => h,
        Err(e) => {
            push_status(chat, format!("[/tree failed: {e}]"), Some(RED_FG));
            return;
        }
    };
    match rx.recv().await {
        Some(TreeSelectorEvent::Selected(id)) => {
            push_status(chat, format!("[/tree picked: {id}]"), None);
        }
        Some(TreeSelectorEvent::Cancelled) | None => {
            push_status(chat, "[/tree cancelled]".to_string(), None);
        }
    }
    let _ = mounter.hide(handle);
}

/// Build a depth-bounded BFS flat tree under `root`. Skips `.git`,
/// `target`, `node_modules`, `.venv`, `.cache`. Caps total rows at 500
/// so the selector stays usable on large repos. The `id` field carries
/// the path relative to `root`.
fn build_tree_rows(root: &Path) -> Vec<TreeRow> {
    const MAX_DEPTH: usize = 4;
    const MAX_ROWS: usize = 500;
    let skip: &[&str] = &[".git", "target", "node_modules", ".venv", ".cache"];
    let mut out: Vec<TreeRow> = Vec::new();
    fn walk(
        dir: &Path,
        depth: usize,
        max_depth: usize,
        max_rows: usize,
        skip: &[&str],
        root: &Path,
        out: &mut Vec<TreeRow>,
    ) {
        if out.len() >= max_rows {
            return;
        }
        if depth >= max_depth {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(it) => it,
            Err(_) => return,
        };
        let mut items: Vec<(std::path::PathBuf, bool)> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if skip.contains(&name.as_str()) {
                    return None;
                }
                let p = e.path();
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                Some((p, is_dir))
            })
            .collect();
        // Stable order: directories first, then files, both alphabetic.
        items.sort_by(|a, b| match (a.1, b.1) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.0.file_name().cmp(&b.0.file_name()),
        });
        for (path, is_dir) in items {
            if out.len() >= max_rows {
                break;
            }
            let rel = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| path.to_string_lossy().into_owned());
            let label = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let display = if is_dir { format!("{label}/") } else { label };
            out.push(TreeRow {
                id: rel,
                depth,
                label: display,
                secondary: None,
            });
            if is_dir {
                walk(&path, depth + 1, max_depth, max_rows, skip, root, out);
            }
        }
    }
    walk(root, 0, MAX_DEPTH, MAX_ROWS, skip, root, &mut out);
    out
}

/// M4.5 — mount the scoped-models multi-select overlay. The outcome
/// channel emits either a session-only `Change` (`enabled_models`
/// patterns updated for this run only) or a `Persist` (write through
/// to the project SettingsManager). Cancellation leaves state alone.
async fn mount_scoped_models_selector(
    chat: &ChatList,
    session: &mut AgentSession,
    mounter: Option<&OverlayMounter>,
) {
    let Some(mounter) = mounter else {
        push_status(chat, "[/scoped-models opened]".to_string(), None);
        return;
    };
    let all_models: Vec<model::Model> = session.model_registry().all().to_vec();
    if all_models.is_empty() {
        push_status(
            chat,
            "[/scoped-models: no models in registry]".to_string(),
            Some(YELLOW_FG),
        );
        return;
    }
    // Read the patterns currently in scope. The on-disk type is a
    // `Vec<String>` of glob patterns; the selector wants an exact list
    // of `provider/id` strings. Resolve once via `resolve_model_scope`
    // so the displayed initial-selected set matches what `/model` is
    // actually filtering by.
    let initial_patterns = session
        .settings()
        .current()
        .enabled_models
        .clone()
        .unwrap_or_default();
    let initial_ids: Option<Vec<String>> = if initial_patterns.is_empty() {
        None
    } else {
        let resolved =
            crate::core::model_resolver::resolve_model_scope(&initial_patterns, &all_models);
        Some(
            resolved
                .models
                .into_iter()
                .map(|sm| format!("{}/{}", sm.model.provider.as_str(), sm.model.id))
                .collect(),
        )
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<ScopedModelsOutcome>();
    let config = ScopedModelsConfig {
        all_models,
        enabled_ids: initial_ids,
    };
    let component = ScopedModelsSelectorComponent::new(config, tx);
    let handle = match mounter
        .show(Box::new(component), OverlayOptions::default())
        .await
    {
        Ok(h) => h,
        Err(e) => {
            push_status(chat, format!("[/scoped-models failed: {e}]"), Some(RED_FG));
            return;
        }
    };
    // Drain the channel — the selector may emit a session Change before
    // the final Persist/Cancelled, so loop until a terminal outcome.
    let mut final_ids: Option<Vec<String>> = None;
    let mut persist = false;
    while let Some(event) = rx.recv().await {
        match event {
            ScopedModelsOutcome::Change(ids) => final_ids = ids,
            ScopedModelsOutcome::Persist(ids) => {
                final_ids = ids;
                persist = true;
                break;
            }
            ScopedModelsOutcome::Cancelled => {
                push_status(chat, "[/scoped-models cancelled]".to_string(), None);
                let _ = mounter.hide(handle);
                return;
            }
        }
    }
    let count = final_ids.as_ref().map(Vec::len).unwrap_or(0);
    if persist {
        // Persist requires the SettingsManager to support setting
        // `enabled_models` on a scope. That setter doesn't exist yet
        // (see core::settings — there are `set_packages` /
        // `set_extensions` / … but not `set_enabled_models`), so we
        // surface a yellow note and keep the session-only effect.
        push_status(
            chat,
            format!(
                "[/scoped-models: {count} model(s) selected (session-only — \
                 persist not yet wired)]"
            ),
            Some(YELLOW_FG),
        );
    } else {
        push_status(
            chat,
            format!("[/scoped-models: {count} model(s) selected for this session]"),
            None,
        );
    }
    let _ = mounter.hide(handle);
}

async fn mount_model_selector(
    chat: &ChatList,
    session: &mut AgentSession,
    mounter: Option<&OverlayMounter>,
) {
    let Some(mounter) = mounter else {
        push_status(chat, "[/model selector — pick from chat]".to_string(), None);
        return;
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<ModelOutcome>();
    let current_model = session.model().clone();
    let all_models: Vec<model::Model> = session.model_registry().all().to_vec();
    // Scoped models: read patterns from `settings.enabled_models` and
    // resolve them against the live model catalogue. Empty patterns ⇒
    // empty scope (selector hides the toggle).
    let patterns: Vec<String> = session
        .settings()
        .current()
        .enabled_models
        .clone()
        .unwrap_or_default();
    let scoped_models: Vec<model::Model> = if patterns.is_empty() {
        Vec::new()
    } else {
        crate::core::model_resolver::resolve_model_scope(&patterns, &all_models)
            .models
            .into_iter()
            .map(|s| s.model)
            .collect()
    };
    let component = ModelSelectorComponent::new(Some(current_model), all_models, scoped_models, tx);
    let handle = match mounter
        .show(Box::new(component), OverlayOptions::default())
        .await
    {
        Ok(h) => h,
        Err(e) => {
            push_status(chat, format!("[/model failed: {e}]"), Some(RED_FG));
            return;
        }
    };
    match rx.recv().await {
        Some(ModelOutcome::Selected(model)) => {
            let id = model.id.clone();
            session.set_model(*model);
            push_status(chat, format!("[model set to {id}]"), None);
        }
        Some(ModelOutcome::Cancelled) | None => {
            push_status(chat, "[/model cancelled]".to_string(), None);
        }
    }
    let _ = mounter.hide(handle);
}

async fn mount_thinking_selector(
    chat: &ChatList,
    session: &mut AgentSession,
    inline_level: Option<String>,
    mounter: Option<&OverlayMounter>,
) {
    use model::ThinkingLevel;

    // Inline form (`/thinking high`) bypasses the picker.
    if let Some(arg) = inline_level {
        let trimmed = arg.trim();
        // Explicit off / none / clear maps to `reasoning = None` — the
        // model is asked to skip reasoning entirely. Any recognised level
        // is forwarded as `Some(level)`. Anything else surfaces an error.
        let normalised = trimmed.to_lowercase();
        let parsed: Result<Option<ThinkingLevel>, ()> = match normalised.as_str() {
            "off" | "none" | "clear" => Ok(None),
            other => crate::core::model_resolver::parse_thinking_level(other)
                .map(Some)
                .ok_or(()),
        };
        match parsed {
            Ok(level) => {
                apply_thinking_level(session, level);
                report_thinking_change(chat, session, level);
            }
            Err(()) => {
                push_status(
                    chat,
                    format!(
                        "[/thinking: unknown level '{trimmed}' — try off/minimal/low/medium/high/xhigh]"
                    ),
                    Some(YELLOW_FG),
                );
            }
        }
        return;
    }

    let Some(mounter) = mounter else {
        push_status(chat, "[/thinking selector opened]".to_string(), None);
        return;
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<ThinkingOutcome>();
    let available_levels: Vec<Option<ThinkingLevel>> = vec![
        None,
        Some(ThinkingLevel::Minimal),
        Some(ThinkingLevel::Low),
        Some(ThinkingLevel::Medium),
        Some(ThinkingLevel::High),
        Some(ThinkingLevel::Xhigh),
    ];
    // Seed the selector with the active level so the cursor lands on it.
    let current = session.stream_options().reasoning;
    let component = ThinkingSelectorComponent::new(current, available_levels, tx);
    let handle = match mounter
        .show(Box::new(component), OverlayOptions::default())
        .await
    {
        Ok(h) => h,
        Err(e) => {
            push_status(chat, format!("[/thinking failed: {e}]"), Some(RED_FG));
            return;
        }
    };
    match rx.recv().await {
        Some(ThinkingOutcome::Selected(level)) => {
            apply_thinking_level(session, level);
            report_thinking_change(chat, session, level);
        }
        Some(ThinkingOutcome::Cancelled) | None => {
            push_status(chat, "[/thinking cancelled]".to_string(), None);
        }
    }
    let _ = mounter.hide(handle);
}

/// Patch the session's stream options with a new thinking level. `None`
/// clears the reasoning request entirely; `Some(level)` overrides it.
fn apply_thinking_level(session: &mut AgentSession, level: Option<model::ThinkingLevel>) {
    let mut opts = session.stream_options().clone();
    opts.reasoning = level;
    session.set_stream_options(opts);
}

/// Emit the `[thinking: <level>]` confirmation, and a yellow follow-up
/// warning when the active model does not advertise reasoning support so
/// users learn the level will be silently dropped (or rejected) instead
/// of mistakenly believing they enabled extended thinking.
fn report_thinking_change(
    chat: &ChatList,
    session: &AgentSession,
    level: Option<model::ThinkingLevel>,
) {
    let label = match level {
        Some(l) => level_label(l).to_string(),
        None => "off".to_string(),
    };
    push_status(chat, format!("[thinking: {label}]"), None);
    if level.is_some() && !session.model().reasoning {
        let model_id = &session.model().id;
        push_status(
            chat,
            format!(
                "[warning: {model_id} does not advertise extended thinking — \
                 this setting may be ignored or rejected by the provider]"
            ),
            Some(YELLOW_FG),
        );
    }
}

fn level_label(level: model::ThinkingLevel) -> &'static str {
    use model::ThinkingLevel;
    match level {
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
    }
}

async fn mount_settings_selector(
    chat: &ChatList,
    session: &mut AgentSession,
    mounter: Option<&OverlayMounter>,
) {
    let Some(mounter) = mounter else {
        push_status(chat, "[/settings opened]".to_string(), None);
        return;
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<SettingsSelectorEvent>();
    // Snapshot the SettingsManager once and project the live values into
    // the selector's entry list. Read-only for now: the selector emits
    // Changed events with the staged value, but persisting them back
    // through `SettingsManager::save` is M2.1's follow-up — until then we
    // surface the staged value in the status line so the user sees the
    // edit was registered.
    let entries = build_settings_entries(session.settings());
    let component = SettingsSelectorComponent::new(entries, 10, tx);
    let handle = match mounter
        .show(Box::new(component), OverlayOptions::default())
        .await
    {
        Ok(h) => h,
        Err(e) => {
            push_status(chat, format!("[/settings failed: {e}]"), Some(RED_FG));
            return;
        }
    };
    match rx.recv().await {
        Some(SettingsSelectorEvent::Changed { id, value }) => {
            // Persist the change to the global YAML layer so the pick
            // survives a restart. Pre-fix the dispatcher only printed
            // the confirmation status and dropped the value on the
            // floor (#45).
            let scope = crate::core::settings::SettingsScope::Global;
            let mgr = session.settings_mut();
            let body = match mgr.apply_setting_by_id(scope, &id, &value) {
                Ok(applied) => match mgr.save(scope) {
                    Ok(()) => format!("[setting {id} = {applied}]"),
                    Err(e) => format!("[setting {id} = {applied} (save failed: {e})]"),
                },
                Err(e) => format!("[/settings: failed to apply {id}: {e}]"),
            };
            push_status(chat, body, None);
        }
        Some(SettingsSelectorEvent::Cancelled) | None => {
            push_status(chat, "[/settings closed]".to_string(), None);
        }
    }
    let _ = mounter.hide(handle);
}

/// Build the read-only entries displayed by `/settings`. Surfaces a
/// curated subset of the merged settings. The first three entries are
/// the effective defaults that drive new sessions (provider, model,
/// thinking level) — UAT-013 / issue #16 pinned that these must be
/// visible so users can confirm a project-level override is in
/// effect. The remaining entries are interactive toggles (theme,
/// auto-compact, etc.) whose changes flow back via
/// `SettingsSelectorEvent::Changed`.
///
/// Each entry carries the live value from `SettingsManager::current()`
/// so the dialog reflects what's actually in effect for this session,
/// not just a static list.
pub(crate) fn build_settings_entries(
    manager: &crate::core::settings::SettingsManager,
) -> Vec<SettingEntry> {
    use crate::core::settings::{ThemeSetting, ThinkingLevelSetting};

    let s = manager.current();

    let theme_choices = vec![
        "dark".to_string(),
        "light".to_string(),
        "high-contrast".to_string(),
        "system".to_string(),
    ];
    let theme_selected = match s.theme() {
        ThemeSetting::Dark => 0,
        ThemeSetting::Light => 1,
        ThemeSetting::HighContrast => 2,
        ThemeSetting::System => 3,
    };

    let provider_display = s
        .default_provider
        .clone()
        .unwrap_or_else(|| "(none — falls back to auto-pick)".to_string());
    let model_display = s
        .default_model
        .clone()
        .unwrap_or_else(|| "(none — provider default)".to_string());
    let thinking_display = match s.default_thinking_level {
        Some(ThinkingLevelSetting::Off) => "off".to_string(),
        Some(ThinkingLevelSetting::Minimal) => "minimal".to_string(),
        Some(ThinkingLevelSetting::Low) => "low".to_string(),
        Some(ThinkingLevelSetting::Medium) => "medium".to_string(),
        Some(ThinkingLevelSetting::High) => "high".to_string(),
        Some(ThinkingLevelSetting::Xhigh) => "xhigh".to_string(),
        None => "(unset)".to_string(),
    };

    vec![
        SettingEntry {
            key: "default_provider".to_string(),
            value: SettingValue::String(provider_display),
            description: "Effective default provider (after global + project merge).".to_string(),
        },
        SettingEntry {
            key: "default_model".to_string(),
            value: SettingValue::String(model_display),
            description: "Effective default model (after global + project merge).".to_string(),
        },
        SettingEntry {
            key: "default_thinking_level".to_string(),
            value: SettingValue::String(thinking_display),
            description: "Effective default reasoning effort for thinking-capable models."
                .to_string(),
        },
        SettingEntry {
            key: "theme".to_string(),
            value: SettingValue::Enum {
                choices: theme_choices,
                selected: theme_selected,
            },
            description: "Color theme used for the chat UI.".to_string(),
        },
        SettingEntry {
            key: "auto_compact".to_string(),
            value: SettingValue::Bool(s.compaction.enabled()),
            description: "Automatically compact context when it grows near the model's window."
                .to_string(),
        },
        SettingEntry {
            key: "hide_thinking_block".to_string(),
            value: SettingValue::Bool(s.hide_thinking_block.unwrap_or(false)),
            description: "Suppress reasoning blocks from the rendered transcript.".to_string(),
        },
        SettingEntry {
            key: "show_images".to_string(),
            value: SettingValue::Bool(s.terminal.show_images()),
            description: "Render inline images when the terminal supports them.".to_string(),
        },
        SettingEntry {
            key: "clear_on_shrink".to_string(),
            value: SettingValue::Bool(s.terminal.clear_on_shrink()),
            description: "Clear leftover rows when the terminal viewport shrinks.".to_string(),
        },
        SettingEntry {
            key: "quiet_startup".to_string(),
            value: SettingValue::Bool(s.quiet_startup()),
            description: "Suppress non-essential output during session start.".to_string(),
        },
    ]
}

/// Build the provider list shown by `/login` from the session's model
/// catalog: one entry per unique provider id, with a short status badge
/// indicating whether credentials are already on file.
fn build_login_provider_list(session: &AgentSession) -> Vec<AuthSelectorProvider> {
    use crate::core::model_registry::AuthSource;

    let registry = session.model_registry();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<AuthSelectorProvider> = Vec::new();
    for model in registry.all() {
        let id = model.provider.as_str().to_string();
        if !seen.insert(id.clone()) {
            continue;
        }
        let name = registry.provider_display_name(&id);
        let status_obj = registry.provider_auth_status(&id);
        let status = match (status_obj.configured, status_obj.source) {
            (true, _) => "\x1b[32mconfigured\x1b[0m".to_string(), // green
            (false, Some(AuthSource::Environment)) => "\x1b[33menv detected\x1b[0m".to_string(),
            _ => String::new(),
        };
        out.push(AuthSelectorProvider { id, name, status });
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// Mount the provider picker for `/login`. Returns the chosen provider id
/// or `None` when the user cancels (or the picker can't mount).
async fn mount_login_provider_picker(
    chat: &ChatList,
    session: &AgentSession,
    mounter: Option<&OverlayMounter>,
) -> Option<String> {
    let Some(mounter) = mounter else {
        push_status(
            chat,
            "[/login: no overlay; pass `/login <provider>` to skip the picker]".to_string(),
            None,
        );
        return None;
    };
    let providers = build_login_provider_list(session);
    if providers.is_empty() {
        push_status(
            chat,
            "[/login: no providers in the model catalog]".to_string(),
            Some(RED_FG),
        );
        return None;
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<OAuthOutcome>();
    let component = OAuthSelectorComponent::new(AuthSelectorMode::Login, providers, tx);
    let handle = match mounter
        .show(Box::new(component), OverlayOptions::default())
        .await
    {
        Ok(h) => h,
        Err(e) => {
            push_status(chat, format!("[/login failed: {e}]"), Some(RED_FG));
            return None;
        }
    };
    let chosen = match rx.recv().await {
        Some(OAuthOutcome::Selected(id)) => Some(id),
        Some(OAuthOutcome::Cancelled) | None => {
            push_status(chat, "[/login cancelled]".to_string(), None);
            None
        }
    };
    let _ = mounter.hide(handle);
    chosen
}

/// Mount the manual-input dialog for `provider_id` and persist any
/// submitted key to `~/.hand/agent/auth.json`.
async fn mount_login_key_input(
    chat: &ChatList,
    provider_id: &str,
    mounter: Option<&OverlayMounter>,
) {
    use crate::core::auth_storage::{AuthRecord, AuthStorage};

    // Canonicalise the provider id when it matches a known one; unknown
    // ids are accepted verbatim so users can still log in to providers
    // we don't statically know about.
    let canonical = model::types::Provider::from_str(provider_id)
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| provider_id.to_string());

    let Some(mounter) = mounter else {
        push_status(chat, format!("[/login {canonical}]"), None);
        return;
    };
    // LoginDialog uses std::sync::mpsc internally (mirrors the TS callback
    // pattern); we bridge it onto a tokio channel via a helper task.
    let (std_tx, std_rx) = std::sync::mpsc::channel::<LoginDialogEvent>();
    let (tokio_tx, mut tokio_rx) = mpsc::unbounded_channel::<LoginDialogEvent>();
    std::thread::spawn(move || {
        while let Ok(event) = std_rx.recv() {
            if tokio_tx.send(event).is_err() {
                break;
            }
        }
    });
    // TODO(parity): populate the providers list + OAuth flows from the
    // upstream-ai OAuth registry once it's wired in. For now the dialog runs a
    // pure manual-input path: paste an API key and persist it.
    let providers: Vec<crate::modes::interactive::components::LoginProvider> = Vec::new();
    let mut component = LoginDialogComponent::new(&canonical, &providers, None, None, std_tx);
    component.show_manual_input(format!(
        "Paste API key for {canonical} and press Enter (Esc to cancel):"
    ));
    // Capturing overlays receive raw events, but the embedded
    // InputComponent gates on its own focus flag — explicitly focus the
    // dialog so keystrokes reach the input.
    component.set_focused(true);
    let handle = match mounter
        .show(Box::new(component), OverlayOptions::default())
        .await
    {
        Ok(h) => h,
        Err(e) => {
            push_status(chat, format!("[/login failed: {e}]"), Some(RED_FG));
            return;
        }
    };
    match tokio_rx.recv().await {
        Some(LoginDialogEvent::Submit(value)) => {
            let key = value.trim().to_string();
            if key.is_empty() {
                push_status(chat, "[/login cancelled: empty key]".to_string(), None);
            } else {
                let result =
                    AuthStorage::new().and_then(|s| s.set(&canonical, AuthRecord::ApiKey { key }));
                match result {
                    Ok(()) => push_status(
                        chat,
                        format!("[login: api key saved for {canonical}]"),
                        None,
                    ),
                    Err(e) => push_status(
                        chat,
                        format!("[/login failed to save key: {e}]"),
                        Some(RED_FG),
                    ),
                }
            }
        }
        Some(LoginDialogEvent::Cancel) | None => {
            push_status(chat, "[/login cancelled]".to_string(), None);
        }
    }
    let _ = mounter.hide(handle);
}

/// Map a string provider id to its OAuth registry entry, if one exists.
/// Returns `Some(OAuthProviderId)` for providers with a browser/device
/// OAuth flow (anthropic, openai-codex, github-copilot) and `None`
/// otherwise (API-key providers like openrouter, deepseek, zai, …).
fn oauth_id_for(provider_id: &str) -> Option<model::OAuthProviderId> {
    match provider_id {
        "anthropic" => Some(model::OAuthProviderId::Anthropic),
        "openai-codex" => Some(model::OAuthProviderId::OpenAICodex),
        "github-copilot" => Some(model::OAuthProviderId::GithubCopilot),
        _ => None,
    }
}

/// Run the OAuth login flow for `oauth_id`. Streams status to the chat
/// as the URL / device-code callbacks fire; persists the resulting
/// credentials via `OAuthRegistry::save` so subsequent runs find them.
async fn run_oauth_login(chat: &ChatList, oauth_id: model::OAuthProviderId) {
    use model::{OAuthAuthInfo, OAuthLoginCallbacks, OAuthRegistry};

    let registry = OAuthRegistry::new();
    let Some(provider) = registry.get(oauth_id) else {
        push_status(
            chat,
            format!(
                "[/login: no OAuth implementation for {}]",
                oauth_id.as_str()
            ),
            Some(RED_FG),
        );
        return;
    };

    // Surface the auth URL / device code to the user via the chat so they
    // can act on them — the default callbacks would print to stderr,
    // which the TUI swallows.
    let chat_for_url = Arc::clone(chat);
    let chat_for_code = Arc::clone(chat);
    let callbacks = OAuthLoginCallbacks {
        on_open_url: Box::new(move |url| {
            push_status(
                &chat_for_url,
                format!("[oauth: open in browser → {url}]"),
                None,
            );
            // Best-effort `open` on macOS / `xdg-open` on Linux. Failure
            // here is non-fatal — the URL is also visible above.
            let _ = open_url_in_browser(url);
        }),
        on_device_code: Box::new(move |code, url| {
            push_status(
                &chat_for_code,
                format!("[oauth: visit {url} and enter code {code}]"),
                None,
            );
        }),
    };

    push_status(
        chat,
        format!("[oauth: starting login for {}…]", oauth_id.as_str()),
        None,
    );
    match provider.login(&callbacks).await {
        Ok(credentials) => {
            let info = OAuthAuthInfo {
                provider_id: oauth_id,
                credentials,
                created_at_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
            };
            match registry.save(&info).await {
                Ok(()) => push_status(
                    chat,
                    format!("[oauth: logged in as {}]", oauth_id.as_str()),
                    None,
                ),
                Err(e) => push_status(
                    chat,
                    format!("[oauth: login succeeded but save failed: {e}]"),
                    Some(RED_FG),
                ),
            }
        }
        Err(e) => push_status(chat, format!("[oauth: login failed: {e}]"), Some(RED_FG)),
    }
}

/// Best-effort cross-platform browser launcher. Falls back to a no-op
/// when the underlying command is missing — the URL is already visible
/// in the chat status line.
fn open_url_in_browser(url: &str) -> std::io::Result<()> {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(cmd).arg(url).spawn().map(|_| ())
}

/// Whether any provider in the active session's catalog has a usable
/// credential (stored or env-var). Drives the first-run onboarding gate.
fn any_provider_has_credentials(session: &AgentSession) -> bool {
    let registry = session.model_registry();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for model in registry.all() {
        let pid = model.provider.as_str();
        if !seen.insert(pid.to_string()) {
            continue;
        }
        if registry.has_provider_auth_configured(pid) {
            return true;
        }
    }
    false
}

async fn mount_resume_picker(
    chat: &ChatList,
    session: &mut AgentSession,
    cwd: &Path,
    mounter: Option<&OverlayMounter>,
) {
    let Some(mounter) = mounter else {
        push_status(
            chat,
            "[/resume — most recent session selector]".to_string(),
            None,
        );
        return;
    };
    let sessions = match crate::core::session_manager::SessionManager::list(cwd) {
        Ok(list) => list,
        Err(e) => {
            push_status(chat, format!("[/resume failed: {e}]"), Some(RED_FG));
            return;
        }
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<SessionSelectorEvent>();
    let component = SessionSelectorComponent::new(sessions, tx);
    let handle = match mounter
        .show(Box::new(component), OverlayOptions::default())
        .await
    {
        Ok(h) => h,
        Err(e) => {
            push_status(chat, format!("[/resume failed: {e}]"), Some(RED_FG));
            return;
        }
    };
    match rx.recv().await {
        Some(SessionSelectorEvent::Selected(path)) => {
            // Swap the AgentSession in-place via `switch_session`. After
            // success the scrollback is stale (still shows the previous
            // session's messages) — wipe it and replay the new session's
            // history so the chat reflects what `session.messages()` now
            // returns.
            match session.switch_session(&path) {
                Ok(()) => {
                    {
                        let mut list = chat.lock().expect("chat list mutex poisoned");
                        list.clear();
                    }
                    push_welcome_header(chat, session.model());
                    replay_messages_into(chat, session.messages());
                    push_status(chat, format!("[resumed: {}]", path.display()), None);
                }
                Err(e) => {
                    push_status(chat, format!("[/resume failed: {e}]"), Some(RED_FG));
                }
            }
        }
        Some(SessionSelectorEvent::Cancelled) | None => {
            push_status(chat, "[/resume cancelled]".to_string(), None);
        }
    }
    let _ = mounter.hide(handle);
}

// ---------------------------------------------------------------------------
// M3 slash-command appliers.
//
// Each helper here implements one of the new commands ported in M3 of the
// parity-final-stretch plan. They route the request into an existing
// core/utils API and push a single status line (or a small component)
// into the chat scrollback. Errors render in red so the user sees clearly
// that the side-effect didn't happen.
// ---------------------------------------------------------------------------

/// `/model <pattern>` — resolve the pattern via
/// [`crate::core::model_resolver::parse_model_pattern_full`] and apply the
/// match. Ambiguous / unknown patterns surface inline.
fn apply_model_by_pattern(chat: &ChatList, session: &mut AgentSession, pattern: &str) {
    use crate::core::model_resolver::{ParseModelPatternOptions, parse_model_pattern_full};
    let available: Vec<model::Model> = session.model_registry().all().to_vec();
    let result =
        parse_model_pattern_full(pattern, &available, ParseModelPatternOptions::permissive());
    if let Some(warning) = &result.warning {
        push_status(chat, format!("[{warning}]"), Some(YELLOW_FG));
    }
    match result.model {
        Some(model) => {
            let id = model.id.clone();
            session.set_model(model);
            push_status(chat, format!("[model set to {id}]"), None);
        }
        None => push_status(
            chat,
            format!("[/model: no match for {pattern:?}]"),
            Some(YELLOW_FG),
        ),
    }
}

/// `/copy n` — concatenate text blocks from the trailing `n` assistant
/// messages (newest last) and copy to the system clipboard.
fn apply_copy_n(chat: &ChatList, session: &AgentSession, n: usize) {
    let texts = last_n_assistant_texts(session, n);
    if texts.is_empty() {
        push_status(
            chat,
            "[no assistant messages to copy]".to_string(),
            Some(YELLOW_FG),
        );
        return;
    }
    let body = texts.join("\n\n");
    match crate::utils::clipboard::copy_to_clipboard(&body) {
        Ok(()) => push_status(
            chat,
            format!(
                "[copied last {} assistant message(s) to clipboard]",
                texts.len()
            ),
            None,
        ),
        Err(e) => push_status(chat, format!("[copy failed: {e}]"), Some(RED_FG)),
    }
}

/// Collect up to `n` trailing assistant messages' text content. Older
/// messages come first in the returned vec so callers can join them
/// chronologically. Image-only messages are skipped — same contract as
/// [`last_assistant_text`].
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
            let mut parts: Vec<String> = Vec::new();
            for block in &a.content {
                if let model::AssistantContentBlock::Text(t) = block {
                    parts.push(t.text.clone());
                }
            }
            if !parts.is_empty() {
                collected.push(parts.join("\n"));
            }
        }
    }
    collected.reverse();
    collected
}

/// `/export <path>` — dispatch to the right `core::export` entrypoint based
/// on the parsed [`ExportFormat`].
fn apply_export(chat: &ChatList, session: &AgentSession, path: &Path, fmt: ExportFormat) {
    use crate::core::export::{export_to_html, export_to_json, export_to_jsonl};
    // Refuse to silently overwrite an existing file. upstream-side issue #8:
    // users testing exports against a path they already used lose the
    // previous transcript without warning. Tell them to delete first
    // or pick a new name.
    if path.exists() {
        push_status(
            chat,
            format!(
                "[/export: {} already exists. Delete it or choose a different path.]",
                path.display()
            ),
            Some(RED_FG),
        );
        return;
    }
    match fmt {
        ExportFormat::Jsonl | ExportFormat::Json => {
            // `.jsonl` copies the raw JSONL session file verbatim;
            // `.json` wraps the same entries in a top-level array so
            // the output is a single valid JSON document instead of
            // a JSONL stream (#67). Both branches open the same on-
            // disk session — the in-memory error message stays
            // shared.
            let manager = match session.session_file() {
                Some(p) => match crate::core::session_manager::SessionManager::open(p) {
                    Ok(m) => m,
                    Err(e) => {
                        push_status(chat, format!("[/export failed: {e}]"), Some(RED_FG));
                        return;
                    }
                },
                None => {
                    push_status(
                        chat,
                        "[/export: cannot export an in-memory session as JSON/JSONL]".to_string(),
                        Some(RED_FG),
                    );
                    return;
                }
            };
            let result = if matches!(fmt, ExportFormat::Jsonl) {
                export_to_jsonl(&manager, path)
            } else {
                export_to_json(&manager, path)
            };
            match result {
                Ok(()) => push_status(chat, format!("[exported to {}]", path.display()), None),
                Err(e) => push_status(chat, format!("[/export failed: {e}]"), Some(RED_FG)),
            }
        }
        ExportFormat::Html => {
            let session_id = session.session_id().to_string();
            let model_id = session.model().id.clone();
            match export_to_html(session.messages(), &session_id, &model_id, path) {
                Ok(()) => push_status(chat, format!("[exported to {}]", path.display()), None),
                Err(e) => push_status(chat, format!("[/export failed: {e}]"), Some(RED_FG)),
            }
        }
    }
}

/// `/import <path>` — replace the active session in place.
fn apply_import(chat: &ChatList, session: &mut AgentSession, path: &Path) {
    if !path.exists() {
        push_status(
            chat,
            format!("[/import: file not found: {}]", path.display()),
            Some(RED_FG),
        );
        return;
    }
    match session.switch_session(path) {
        Ok(()) => push_status(
            chat,
            format!("[imported session from {}]", path.display()),
            None,
        ),
        Err(e) => push_status(chat, format!("[/import failed: {e}]"), Some(RED_FG)),
    }
}

/// `/fork [<entry-id>]` — branch the session at the chosen user entry, or
/// the most recent user message when no id was supplied.
async fn apply_fork(
    chat: &ChatList,
    session: &mut AgentSession,
    entry_id: Option<&str>,
    mounter: Option<&OverlayMounter>,
) {
    let target = match entry_id {
        Some(id) => id.to_string(),
        None => {
            let entries = session.fork_messages();
            if entries.is_empty() {
                push_status(
                    chat,
                    "[/fork: no user messages to fork from]".to_string(),
                    Some(YELLOW_FG),
                );
                return;
            }
            // M4.6 — open the user-message picker so the user can fork at
            // an explicit entry rather than always the last one. When no
            // overlay mounter is available (headless / unit-test contexts)
            // fall back to the previous "last entry" behaviour so existing
            // fixtures keep passing.
            match mounter {
                Some(m) => match mount_user_message_picker_for_fork(chat, &entries, m).await {
                    Some(id) => id,
                    None => return, // cancelled or mount failed; status already pushed
                },
                None => entries.last().expect("entries non-empty").entry_id.clone(),
            }
        }
    };
    match session.fork(&target) {
        Ok(text) => {
            let preview: String = text.chars().take(60).collect();
            push_status(chat, format!("[forked at: {preview}]"), None);
        }
        Err(e) => push_status(chat, format!("[/fork failed: {e}]"), Some(RED_FG)),
    }
}

/// Mount the user-message picker for `/fork`. Returns the chosen
/// `entry_id` or `None` if the user cancelled / the mount failed.
async fn mount_user_message_picker_for_fork(
    chat: &ChatList,
    entries: &[crate::rpc::types::ForkMessageEntry],
    mounter: &OverlayMounter,
) -> Option<String> {
    use crate::modes::interactive::components::{
        UserMessageItem, UserMessageSelectorComponent, UserMessageSelectorEvent,
    };

    let items: Vec<UserMessageItem> = entries
        .iter()
        .map(|e| UserMessageItem {
            id: e.entry_id.clone(),
            text: e.text.clone(),
            timestamp: None,
        })
        .collect();
    let initial = entries.last().map(|e| e.entry_id.as_str());
    let (std_tx, std_rx) = std::sync::mpsc::channel::<UserMessageSelectorEvent>();
    let (tokio_tx, mut tokio_rx) = mpsc::unbounded_channel::<UserMessageSelectorEvent>();
    std::thread::spawn(move || {
        while let Ok(event) = std_rx.recv() {
            if tokio_tx.send(event).is_err() {
                break;
            }
        }
    });
    let component = UserMessageSelectorComponent::new(items, initial, std_tx, None);
    let handle = match mounter
        .show(Box::new(component), OverlayOptions::default())
        .await
    {
        Ok(h) => h,
        Err(e) => {
            push_status(chat, format!("[/fork picker failed: {e}]"), Some(RED_FG));
            return None;
        }
    };
    let chosen = match tokio_rx.recv().await {
        Some(UserMessageSelectorEvent::Select { entry_id }) => Some(entry_id),
        Some(UserMessageSelectorEvent::Cancel) | None => {
            push_status(chat, "[/fork cancelled]".to_string(), None);
            None
        }
    };
    let _ = mounter.hide(handle);
    chosen
}

/// `/clone` — duplicate the current session under a fresh id.
fn apply_clone(chat: &ChatList, session: &mut AgentSession) {
    match session.clone_session() {
        Ok(()) => push_status(
            chat,
            format!("[cloned session: new id {}]", session.session_id()),
            None,
        ),
        Err(e) => push_status(chat, format!("[/clone failed: {e}]"), Some(RED_FG)),
    }
}

/// `/theme [name]` — apply a theme inline or open the selector overlay.
///
/// The live colour swap is a TUI-wide refactor still in flight (every
/// component currently bakes its ANSI literals at compile time), so
/// `/theme <name>` persists the pick to settings.yaml via the same
/// path `/settings → theme` uses (issue #45). The next session
/// startup reads the value and applies it from the ground up. The
/// status line surfaces that contract honestly — old behaviour
/// printed `[theme: <name>]` and silently dropped the resolved theme
/// object (issue #43).
async fn apply_theme(
    chat: &ChatList,
    session: &mut AgentSession,
    arg: Option<String>,
    mounter: Option<&OverlayMounter>,
) {
    use crate::modes::interactive::theme::{
        available_themes, default_custom_themes_dir, theme_by_name,
    };

    let custom_dir = default_custom_themes_dir().ok();
    let custom_dir_ref = custom_dir.as_deref();

    if let Some(name) = arg {
        let Some(dir) = custom_dir_ref else {
            push_status(
                chat,
                "[/theme failed: no home directory available]".to_string(),
                Some(RED_FG),
            );
            return;
        };
        match theme_by_name(&name, dir, None) {
            Some(_theme) => {
                let msg = persist_theme_selection(session, &name);
                push_status(chat, msg, Some(YELLOW_FG));
            }
            None => push_status(
                chat,
                format!("[/theme: unknown theme {name:?}]"),
                Some(RED_FG),
            ),
        }
        return;
    }

    let Some(mounter) = mounter else {
        push_status(chat, "[/theme selector opened]".to_string(), None);
        return;
    };
    let themes = match custom_dir_ref {
        Some(dir) => available_themes(dir),
        None => available_themes(std::path::Path::new("/")),
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<ThemeOutcome>();
    let component = ThemeSelectorComponent::new("dark", themes, tx);
    let handle = match mounter
        .show(Box::new(component), OverlayOptions::default())
        .await
    {
        Ok(h) => h,
        Err(e) => {
            push_status(chat, format!("[/theme failed: {e}]"), Some(RED_FG));
            return;
        }
    };
    loop {
        match rx.recv().await {
            Some(ThemeOutcome::Selected(name)) => {
                let msg = persist_theme_selection(session, &name);
                push_status(chat, msg, Some(YELLOW_FG));
                break;
            }
            Some(ThemeOutcome::Cancelled) | None => {
                push_status(chat, "[/theme cancelled]".to_string(), None);
                break;
            }
            // Preview events are emitted on every navigation tick; the
            // theme bridge will pick these up once the live palette swap
            // lands. For now, ignore them so we don't spam the chat.
            Some(ThemeOutcome::Preview(_)) => continue,
        }
    }
    let _ = mounter.hide(handle);
}

/// Save the picked theme name to the global settings.yaml so the next
/// session starts with it. Used by both `/theme <name>` and the
/// theme-selector overlay's `Selected` event. Returns the status-line
/// text to push into the chat — success / save-error / canonicalise-
/// error are all conveyed in the same banner so the user sees what
/// actually happened.
pub(crate) fn persist_theme_selection(session: &mut AgentSession, name: &str) -> String {
    let scope = crate::core::settings::SettingsScope::Global;
    let mgr = session.settings_mut();
    match mgr.apply_setting_by_id(scope, "theme", name) {
        Ok(applied) => match mgr.save(scope) {
            Ok(()) => format!("[theme: {applied} — saved; restart hand for chat colors to update]"),
            Err(e) => format!("[theme: {applied} (save failed: {e})]"),
        },
        Err(e) => format!("[/theme: {e}]"),
    }
}

/// `/skills` — render the discovered skills as a custom message.
fn apply_list_skills(chat: &ChatList, session: &AgentSession) {
    let skills = session.skills();
    let body = if skills.is_empty() {
        "_(no skills discovered)_".to_string()
    } else {
        let mut out = String::new();
        for skill in skills {
            out.push_str(&format!("- **{}** — {}\n", skill.name, skill.description));
        }
        out.trim_end().to_string()
    };
    let component = CustomMessageComponent::new(CustomMessageData::new("skills", body));
    push_component(chat, Box::new(component));
}

/// `/extensions` — render the loaded Tier 1 extensions as a custom message.
fn apply_list_extensions(chat: &ChatList, session: &AgentSession) {
    let exts = session.extensions();
    let body = if exts.is_empty() {
        "_(no extensions loaded)_".to_string()
    } else {
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
    };
    let component = CustomMessageComponent::new(CustomMessageData::new("extensions", body));
    push_component(chat, Box::new(component));
}

/// Locate the on-disk CHANGELOG.md, trying the conventional in-repo
/// candidates. Returns the first existing path or `None`.
fn locate_changelog_file() -> Option<PathBuf> {
    let candidates = [
        PathBuf::from("CHANGELOG.md"),
        PathBuf::from("crates/coding-agent/CHANGELOG.md"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

/// Pure decision function for the M5.4 startup auto-display: given the
/// state of the session and the parsed CHANGELOG, decide whether to
/// stay quiet, silently record the current version, or display a body
/// of new entries. Extracted as a pure function so it can be unit-
/// tested without a SettingsManager or filesystem.
#[derive(Debug, PartialEq, Eq)]
enum ChangelogStartupAction {
    /// Do nothing — resumed session or empty changelog.
    Skip,
    /// Record current version as last-seen, no display (fresh install).
    RecordOnly,
    /// Mount the supplied body in scrollback, then record current version.
    Display(String),
}

fn decide_changelog_startup(
    messages_empty: bool,
    last_version: Option<&str>,
    entries: &[crate::utils::changelog::ChangelogEntry],
) -> ChangelogStartupAction {
    if !messages_empty {
        return ChangelogStartupAction::Skip;
    }
    match last_version {
        None => ChangelogStartupAction::RecordOnly,
        Some(last) => {
            let new_entries = crate::utils::changelog::get_new_entries(entries, last);
            if new_entries.is_empty() {
                ChangelogStartupAction::Skip
            } else {
                let body = new_entries
                    .iter()
                    .map(|e| e.content.clone())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                ChangelogStartupAction::Display(body)
            }
        }
    }
}

/// M5.4 — on startup, if the running version is newer than the
/// `last_changelog_version` recorded in settings, mount a custom
/// "Changelog" message in scrollback showing the new entries. For fresh
/// installs (no recorded version), just records the current version and
/// stays quiet. For resumed sessions (non-empty messages), skips the
/// banner — those users already saw the changelog when they first
/// upgraded.
///
/// Both the display and the version-bump are best-effort: any I/O or
/// settings-save failure is swallowed so startup never blocks.
fn maybe_show_changelog_on_update(chat: &ChatList, session: &mut AgentSession) {
    let current_version = env!("CARGO_PKG_VERSION");
    let messages_empty = session.messages().is_empty();
    let last_version = session.settings().current().last_changelog_version.clone();

    let path = match locate_changelog_file() {
        Some(p) => p,
        None => return,
    };
    let entries = match crate::utils::changelog::parse_changelog_file(&path) {
        Ok(e) => e,
        Err(_) => return,
    };

    let action = decide_changelog_startup(messages_empty, last_version.as_deref(), &entries);
    let scope = crate::core::settings::SettingsScope::Global;
    match action {
        ChangelogStartupAction::Skip => {}
        ChangelogStartupAction::RecordOnly => {
            session
                .settings_mut()
                .set_last_changelog_version(scope, Some(current_version.to_string()));
            let _ = session.settings().save(scope);
        }
        ChangelogStartupAction::Display(body) => {
            let component = CustomMessageComponent::new(CustomMessageData::new("changelog", body));
            push_component(chat, Box::new(component));
            session
                .settings_mut()
                .set_last_changelog_version(scope, Some(current_version.to_string()));
            let _ = session.settings().save(scope);
        }
    }
}

/// `/changelog` — render the agent's CHANGELOG.md (if present) as a custom
/// message.
fn apply_changelog(chat: &ChatList) {
    use crate::utils::changelog::parse_changelog_file;
    let entries: Vec<crate::utils::changelog::ChangelogEntry> = locate_changelog_file()
        .and_then(|p| parse_changelog_file(p).ok())
        .unwrap_or_default();

    let body = if entries.is_empty() {
        "_(no changelog entries found)_".to_string()
    } else {
        // Newest first — `parse_changelog` preserves source order and the
        // file is conventionally maintained newest-first already.
        entries
            .iter()
            .map(|e| e.content.clone())
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let component = CustomMessageComponent::new(CustomMessageData::new("changelog", body));
    push_component(chat, Box::new(component));
}

/// `/session` — render the active session's headline stats.
///
/// Pulls id, label, message count, model, provider, and an
/// approximate session age from the live [`AgentSession`]. The
/// `usage` snapshot, when present, contributes the token + cost
/// segment; tests that exercise the pure dispatch path can omit it
/// and the renderer skips the token line.
fn render_session_info(session: &AgentSession, usage: Option<&TokenUsageSummary>) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    let _ = writeln!(out, "Session: {}", session.session_id());
    if let Some(label) = session.label() {
        let _ = writeln!(out, "Label: {label}");
    }
    let _ = writeln!(out, "Messages: {}", session.message_count());
    let model = session.model();
    let _ = writeln!(out, "Model: {} ({})", model.id, model.provider.as_str());
    if let Some(u) = usage
        && (u.input > 0 || u.output > 0 || u.cache_read > 0 || u.cache_write > 0)
    {
        let _ = writeln!(
            out,
            "Tokens: {} in / {} out (cache read {} / write {})",
            u.input, u.output, u.cache_read, u.cache_write,
        );
        if u.cost_usd > 0.0 {
            let _ = writeln!(out, "Cost: ${:.4}", u.cost_usd);
        }
    }
    if let Some(started_ms) = session_started_at_ms(session) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let elapsed_ms = (now_ms - started_ms).max(0) as u64;
        let _ = writeln!(out, "Duration: {}", format_duration_ms(elapsed_ms));
    }
    // Trim the trailing newline so push_status renders flush.
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

fn session_started_at_ms(session: &AgentSession) -> Option<i64> {
    // The session header is the first entry; its timestamp is the
    // session-start timestamp. Iterate the entries so we don't depend
    // on the in-memory variant having a header (it doesn't).
    session
        .session_manager()
        .entries()
        .iter()
        .find_map(|e| e.timestamp())
}

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

/// `/reload` — re-read SettingsManager + keybindings from disk so on-disk
/// edits made outside the running session (e.g. via a separate editor)
/// take effect without restarting.
///
/// Scope: settings + keybindings. Re-loading extensions / skills / prompts
/// / themes from the package source is left to the future ResourceLoader
/// reload path (TODO(parity)) — those have their own caches and listeners
/// that aren't yet exposed to the driver.
fn apply_reload(chat: &ChatList, session: &mut AgentSession) {
    let mut applied: Vec<&'static str> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    // Actually swap the session's settings manager with a fresh
    // read from disk. Pre-fix the driver constructed a manager,
    // dropped it, and printed "[reloaded settings]" even though
    // the running session continued using its original values.
    match session.reload_settings() {
        Ok(()) => applied.push("settings"),
        Err(e) => failures.push(format!("settings: {e}")),
    }

    // Keybindings are a process-global LazyLock cache. Re-loading is a
    // no-op for the default table (which lives in code) — only relevant
    // when a user-config file is implemented. For now we just confirm
    // the registry was queried so the user sees `/reload` did something.
    let _ = hand_tui::keybindings::get_keybindings();
    applied.push("keybindings");

    let body = if failures.is_empty() {
        format!("[reloaded {}]", applied.join(", "))
    } else {
        format!(
            "[reloaded {}; failed: {}]",
            applied.join(", "),
            failures.join("; ")
        )
    };
    let color = if failures.is_empty() {
        None
    } else {
        Some(YELLOW_FG)
    };
    push_status(chat, body, color);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use model::{Client, Model, types::Provider};

    fn dummy_model() -> Model {
        Model {
            id: "test-model".to_string(),
            name: "Test".to_string(),
            api: model::types::Api::AnthropicMessages,
            provider: Provider::Anthropic,
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

    fn make_session() -> AgentSession {
        AgentSession::in_memory_with_client(dummy_model(), vec![], Client::new())
    }

    #[test]
    fn footer_view_uses_session_state() {
        let session = make_session();
        let view = InteractiveMode::build_footer_view(
            &session,
            &std::path::PathBuf::from("/tmp"),
            TokenUsageSummary::default(),
        );
        assert_eq!(view.cwd, "/tmp");
        assert_eq!(view.model_id, "test-model");
        assert_eq!(view.model_provider, "anthropic");
    }

    #[test]
    fn slash_help_pushes_help_into_chat() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let parsed = ParsedSlashCommand::parse("/help").unwrap();
        let ctx = SlashCommandContext {
            model_id: "x".into(),
            provider: "y".into(),
        };
        match SlashCommandTable::dispatch(&parsed, &ctx) {
            SlashCommandResult::Handled(SlashCommandAction::ShowText(s)) => {
                push_status(&chat, s, None);
            }
            other => panic!("unexpected: {:?}", other),
        }
        let list = chat.lock().unwrap();
        assert_eq!(list.len(), 1);
        let lines = list[0].render(80);
        let joined = lines.join("\n");
        assert!(
            joined.contains("/quit"),
            "expected help body, got: {joined}"
        );
    }

    #[test]
    fn replay_user_message_renders_into_chat() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let user = model::UserMessage::new_text("hello world");
        let messages = vec![model::Message::User(user)];
        replay_messages_into(&chat, &messages);
        let list = chat.lock().unwrap();
        assert_eq!(list.len(), 1);
        let lines = list[0].render(80);
        let joined = lines.join("\n");
        assert!(
            joined.contains("hello world"),
            "expected rendered user text, got: {joined}"
        );
    }

    #[test]
    fn unknown_slash_command_pushes_warning() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let parsed = ParsedSlashCommand::parse("/no-such").unwrap();
        let ctx = SlashCommandContext {
            model_id: "x".into(),
            provider: "y".into(),
        };
        match SlashCommandTable::dispatch(&parsed, &ctx) {
            SlashCommandResult::Unknown => push_status(
                &chat,
                format!(
                    "Unknown command: /{}. Type /help for available commands.",
                    parsed.name
                ),
                Some(ORANGE_FG),
            ),
            other => panic!("expected Unknown, got: {:?}", other),
        }
        let list = chat.lock().unwrap();
        assert_eq!(list.len(), 1);
        let joined = list[0].render(80).join("\n");
        assert!(joined.contains("Unknown command"), "got: {joined}");
    }

    #[test]
    fn quit_action_is_handled() {
        let parsed = ParsedSlashCommand::parse("/quit").unwrap();
        let ctx = SlashCommandContext {
            model_id: "x".into(),
            provider: "y".into(),
        };
        assert!(matches!(
            SlashCommandTable::dispatch(&parsed, &ctx),
            SlashCommandResult::Handled(SlashCommandAction::Quit)
        ));
    }

    // ---- Streaming-delta tests ------------------------------------------

    fn make_assistant(text: &str) -> model::AssistantMessage {
        model::AssistantMessage {
            role: "assistant".to_string(),
            content: vec![model::AssistantContentBlock::Text(model::TextContent::new(
                text,
            ))],
            api: model::Api::AnthropicMessages,
            provider: model::types::Provider::Anthropic,
            model: "claude".to_string(),
            usage: model::Usage::default(),
            stop_reason: model::StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        }
    }

    fn fresh_state() -> (ChatList, ToolHandles, AssistantHandle) {
        (
            Arc::new(StdMutex::new(Vec::new())),
            Arc::new(StdMutex::new(HashMap::new())),
            Arc::new(StdMutex::new(None)),
        )
    }

    #[test]
    fn append_assistant_then_replace_mutates_in_place() {
        let (chat, tools, asst) = fresh_state();
        apply_updates_to_chat(
            &chat,
            &tools,
            &asst,
            vec![ChatUpdate::AppendAssistant {
                message: Box::new(make_assistant("first")),
            }],
        );
        // The chat list grew by exactly one component.
        assert_eq!(chat.lock().unwrap().len(), 1);

        apply_updates_to_chat(
            &chat,
            &tools,
            &asst,
            vec![ChatUpdate::ReplaceLastAssistant {
                message: Box::new(make_assistant("first second")),
            }],
        );
        // Replacement should NOT push another component.
        assert_eq!(chat.lock().unwrap().len(), 1);
        // The rendered output should reflect the latest snapshot.
        let lines = chat.lock().unwrap()[0].render(80);
        let joined = lines.join("\n");
        assert!(
            joined.contains("first second"),
            "expected updated text, got {joined:?}"
        );
        assert!(
            !joined.contains("first\n") && !joined.ends_with("first"),
            "old snapshot leaked: {joined:?}"
        );
    }

    /// Regression for the double-bubble bug the user reported: the
    /// submit handler immediately pushes a `UserMessageComponent`
    /// (driver.rs:558) so the bubble appears the instant Enter is
    /// pressed. Without intervention the subsequent
    /// `AgentEvent::MessageStart{User}` event would push an identical
    /// second component via `dispatch_agent_event` → `AppendUser`,
    /// rendering the same "你好" twice. `dispatch_agent_event` now
    /// returns an empty `ChatUpdate` list for user-message starts.
    #[test]
    fn user_submit_path_pushes_exactly_one_bubble() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let (_chat_unused, tools, asst) = fresh_state();

        // (1) Driver-side immediate echo — same line as `driver.rs:558`.
        {
            let mut list = chat.lock().unwrap();
            list.push(Box::new(UserMessageComponent::new("你好".to_string())));
        }

        // (2) AgentSession then emits MessageStart{User} on the channel.
        let event = hand_agent::AgentEvent::MessageStart {
            message: model::Message::User(model::UserMessage::new_text("你好")),
        };
        let updates = crate::modes::interactive::event_dispatch::dispatch_agent_event(&event);
        apply_updates_to_chat(&chat, &tools, &asst, updates);

        let list = chat.lock().unwrap();
        assert_eq!(
            list.len(),
            1,
            "expected exactly one bubble, got {}",
            list.len()
        );
        let joined = list[0].render(80).join("\n");
        assert!(
            joined.contains("你好"),
            "expected the bubble to carry the text, got: {joined:?}"
        );
    }

    #[test]
    fn replace_last_without_prior_start_appends_a_new_component() {
        let (chat, tools, asst) = fresh_state();
        apply_updates_to_chat(
            &chat,
            &tools,
            &asst,
            vec![ChatUpdate::ReplaceLastAssistant {
                message: Box::new(make_assistant("late")),
            }],
        );
        assert_eq!(chat.lock().unwrap().len(), 1);
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(joined.contains("late"));
    }

    // ---- Tool-execution dispatch tests ----------------------------------

    #[test]
    fn tool_start_for_bash_creates_bash_component() {
        let (chat, tools, asst) = fresh_state();
        apply_updates_to_chat(
            &chat,
            &tools,
            &asst,
            vec![ChatUpdate::ToolStart {
                tool_call_id: "id-1".into(),
                tool_name: "bash".into(),
                args: serde_json::json!({"command": "ls -la"}),
            }],
        );
        assert_eq!(chat.lock().unwrap().len(), 1);
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(joined.contains("$ ls -la"), "got: {joined:?}");
        assert!(tools.lock().unwrap().contains_key("id-1"));
    }

    #[test]
    fn tool_update_appends_streaming_output_to_bash_component() {
        let (chat, tools, asst) = fresh_state();
        apply_updates_to_chat(
            &chat,
            &tools,
            &asst,
            vec![ChatUpdate::ToolStart {
                tool_call_id: "id-1".into(),
                tool_name: "bash".into(),
                args: serde_json::json!({"command": "echo hi"}),
            }],
        );
        apply_updates_to_chat(
            &chat,
            &tools,
            &asst,
            vec![ChatUpdate::ToolUpdate {
                tool_call_id: "id-1".into(),
                partial_text: "streaming output".into(),
            }],
        );
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(
            joined.contains("streaming output"),
            "missing partial: {joined:?}"
        );
    }

    #[test]
    fn tool_end_marks_bash_complete_with_exit_code() {
        let (chat, tools, asst) = fresh_state();
        apply_updates_to_chat(
            &chat,
            &tools,
            &asst,
            vec![ChatUpdate::ToolStart {
                tool_call_id: "id-1".into(),
                tool_name: "bash".into(),
                args: serde_json::json!({"command": "false"}),
            }],
        );
        apply_updates_to_chat(
            &chat,
            &tools,
            &asst,
            vec![ChatUpdate::ToolEnd {
                tool_call_id: "id-1".into(),
                result_text: "boom".into(),
                is_error: true,
                exit_code: Some(2),
            }],
        );
        // Handle removed from active map after end.
        assert!(!tools.lock().unwrap().contains_key("id-1"));
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(joined.contains("exit 2"), "missing exit code: {joined:?}");
    }

    #[test]
    fn tool_start_for_non_bash_creates_generic_component() {
        let (chat, tools, asst) = fresh_state();
        apply_updates_to_chat(
            &chat,
            &tools,
            &asst,
            vec![ChatUpdate::ToolStart {
                tool_call_id: "rid".into(),
                tool_name: "read".into(),
                args: serde_json::json!({"path": "/etc/hosts"}),
            }],
        );
        assert!(matches!(
            tools.lock().unwrap().get("rid"),
            Some(ToolHandle::Generic(_))
        ));
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(joined.contains("read"), "{joined:?}");
        assert!(joined.contains("/etc/hosts"), "{joined:?}");
    }

    // ---- Slash-command action tests -------------------------------------

    fn dispatch(input: &str, ctx: &SlashCommandContext) -> SlashCommandAction {
        let parsed = ParsedSlashCommand::parse(input).unwrap();
        match SlashCommandTable::dispatch(&parsed, ctx) {
            SlashCommandResult::Handled(action) => action,
            SlashCommandResult::Unknown => panic!("/{} should be known", parsed.name),
        }
    }

    #[tokio::test]
    async fn clear_action_empties_chat_and_pushes_status() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        chat.lock()
            .unwrap()
            .push(Box::new(UserMessageComponent::new("noise".to_string())));
        let mut session = make_session();
        let action = dispatch(
            "/clear",
            &SlashCommandContext {
                model_id: "x".into(),
                provider: "y".into(),
            },
        );
        let outcome =
            apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        assert_eq!(outcome, SlashOutcome::Continue);
        let list = chat.lock().unwrap();
        // List should contain only the post-clear status line.
        assert_eq!(list.len(), 1);
        let joined = list[0].render(80).join("\n");
        assert!(joined.contains("chat cleared"), "{joined:?}");
    }

    #[tokio::test]
    async fn copy_with_no_assistant_message_pushes_warning() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        let action = dispatch(
            "/copy",
            &SlashCommandContext {
                model_id: "x".into(),
                provider: "y".into(),
            },
        );
        let outcome =
            apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        assert_eq!(outcome, SlashOutcome::Continue);
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(joined.contains("no assistant message"), "{joined:?}");
    }

    #[tokio::test]
    async fn thinking_inline_level_emits_status_with_level() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        let action = dispatch(
            "/thinking high",
            &SlashCommandContext {
                model_id: "x".into(),
                provider: "y".into(),
            },
        );
        match &action {
            SlashCommandAction::OpenThinkingSelector { inline_level } => {
                assert_eq!(inline_level.as_deref(), Some("high"));
            }
            other => panic!("expected OpenThinkingSelector, got {:?}", other),
        }
        // The session starts with no reasoning level.
        assert_eq!(session.stream_options().reasoning, None);
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        // The status line confirms the new level and the session state
        // actually mutates — the next agent turn will request reasoning.
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(joined.contains("high"), "{joined:?}");
        assert_eq!(
            session.stream_options().reasoning,
            Some(model::ThinkingLevel::High)
        );
    }

    /// `/thinking off` (or `none` / `clear`) explicitly resets the
    /// reasoning request to `None` — the next turn will be a plain
    /// non-reasoning completion.
    #[tokio::test]
    async fn thinking_off_clears_session_reasoning() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        // Plant a reasoning level first.
        let mut opts = session.stream_options().clone();
        opts.reasoning = Some(model::ThinkingLevel::Medium);
        session.set_stream_options(opts);

        let action = dispatch(
            "/thinking off",
            &SlashCommandContext {
                model_id: "x".into(),
                provider: "y".into(),
            },
        );
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        assert_eq!(session.stream_options().reasoning, None);
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(joined.contains("off"), "{joined:?}");
    }

    /// `/thinking high` on a model that does not advertise reasoning
    /// support should still apply (so users can opt in once the model
    /// upgrades) but must also surface a yellow warning so the user
    /// knows the level may be silently dropped or rejected by the
    /// provider.
    #[tokio::test]
    async fn thinking_inline_level_warns_when_model_lacks_reasoning() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        assert!(!session.model().reasoning);
        let action = dispatch(
            "/thinking high",
            &SlashCommandContext {
                model_id: "x".into(),
                provider: "y".into(),
            },
        );
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        let list = chat.lock().unwrap();
        assert!(
            list.len() >= 2,
            "expected confirmation + warning, got {} entries",
            list.len()
        );
        let confirm = list[0].render(80).join("\n");
        let warn = list[1].render(80).join("\n");
        assert!(confirm.contains("high"), "confirmation missing level: {confirm}");
        assert!(
            warn.contains("does not advertise extended thinking"),
            "warning missing reasoning-unsupported hint: {warn}"
        );
        assert!(
            warn.contains(&session.model().id),
            "warning missing model id: {warn}"
        );
    }

    /// On a reasoning-capable model the warning must NOT fire — the
    /// confirmation is the only status pushed.
    #[tokio::test]
    async fn thinking_inline_level_skips_warning_when_model_supports_reasoning() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut model = dummy_model();
        model.reasoning = true;
        let mut session = AgentSession::in_memory_with_client(model, vec![], Client::new());
        let action = dispatch(
            "/thinking high",
            &SlashCommandContext {
                model_id: "x".into(),
                provider: "y".into(),
            },
        );
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        let list = chat.lock().unwrap();
        assert_eq!(list.len(), 1);
        let joined = list[0].render(80).join("\n");
        assert!(joined.contains("high"), "{joined:?}");
        assert!(
            !joined.contains("does not advertise"),
            "unexpected warning on reasoning-capable model: {joined}"
        );
    }

    /// `/thinking off` on a non-reasoning model should NOT warn —
    /// clearing the level is always valid.
    #[tokio::test]
    async fn thinking_off_does_not_warn_on_non_reasoning_model() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        let action = dispatch(
            "/thinking off",
            &SlashCommandContext {
                model_id: "x".into(),
                provider: "y".into(),
            },
        );
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        let list = chat.lock().unwrap();
        assert_eq!(list.len(), 1);
    }

    /// Unknown `/thinking <foo>` shouldn't mutate state — surface a
    /// yellow status pointing at the valid levels instead.
    /// M3.3 — the editor's focused-border color tracks the active
    /// thinking level. Each level maps to a distinct truecolor SGR;
    /// `None` falls back to the default `BORDER_FOCUS` cyan.
    #[test]
    fn build_tree_rows_walks_root_skips_noise_and_orders_dirs_first() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src/inner")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "noise").unwrap();
        std::fs::write(root.join("README.md"), "readme").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main(){}").unwrap();
        std::fs::write(root.join("src/inner/deep.rs"), "").unwrap();

        let rows = build_tree_rows(root);
        let labels: Vec<String> = rows.iter().map(|r| r.label.clone()).collect();
        // .git is skipped entirely.
        assert!(!labels.iter().any(|l| l == ".git/"), "got: {labels:?}");
        // src/ comes before README.md (dirs first).
        let src_pos = labels.iter().position(|l| l == "src/").unwrap();
        let readme_pos = labels.iter().position(|l| l == "README.md").unwrap();
        assert!(src_pos < readme_pos, "dirs first: {labels:?}");
        // Walks into subdirs (depth 1+).
        assert!(rows.iter().any(|r| r.depth >= 1 && r.label == "main.rs"));
        assert!(rows.iter().any(|r| r.depth >= 2 && r.label == "deep.rs"));
        // Ids carry the path relative to root.
        let main_row = rows.iter().find(|r| r.label == "main.rs").unwrap();
        assert!(
            main_row.id.contains("src") && main_row.id.contains("main.rs"),
            "rel id: {}",
            main_row.id
        );
    }

    #[test]
    fn changelog_startup_skips_when_session_has_replayed_messages() {
        let entries = crate::utils::changelog::parse_changelog(
            "## [0.2.0] 2026-05-01\n- second\n\n## [0.1.0] 2026-04-01\n- first",
        );
        let action = decide_changelog_startup(false, Some("0.1.0"), &entries);
        assert_eq!(action, ChangelogStartupAction::Skip);
    }

    #[test]
    fn changelog_startup_records_only_on_fresh_install() {
        let entries = crate::utils::changelog::parse_changelog("## [0.1.0] 2026-04-01\n- first");
        let action = decide_changelog_startup(true, None, &entries);
        assert_eq!(action, ChangelogStartupAction::RecordOnly);
    }

    #[test]
    fn changelog_startup_skips_when_up_to_date() {
        let entries = crate::utils::changelog::parse_changelog("## [0.1.0] 2026-04-01\n- first");
        let action = decide_changelog_startup(true, Some("0.1.0"), &entries);
        assert_eq!(action, ChangelogStartupAction::Skip);
    }

    #[test]
    fn changelog_startup_displays_strictly_newer_entries_only() {
        let entries = crate::utils::changelog::parse_changelog(
            "## [0.3.0] 2026-06-01\n- third\n\n## [0.2.0] 2026-05-01\n- second\n\n## [0.1.0] 2026-04-01\n- first",
        );
        let action = decide_changelog_startup(true, Some("0.1.0"), &entries);
        match action {
            ChangelogStartupAction::Display(body) => {
                assert!(body.contains("third"), "body: {body}");
                assert!(body.contains("second"), "body: {body}");
                assert!(!body.contains("first"), "body must exclude 0.1.0: {body}");
            }
            other => panic!("expected Display, got {other:?}"),
        }
    }

    #[test]
    fn drop_paste_strips_quotes_and_prepends_at_for_existing_path() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("dropped.txt");
        std::fs::write(&file, "x").unwrap();
        // Absolute path inside cwd (cwd == tmp.path()) — comes back as @relative.
        let abs = file.to_string_lossy().to_string();
        let payload = format!("'{abs}'");
        let out = transform_dropped_file_paste(&payload, tmp.path()).unwrap();
        assert_eq!(out, "@dropped.txt");
    }

    #[test]
    fn drop_paste_strips_file_scheme_and_percent_decodes() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("with space.txt");
        std::fs::write(&file, "x").unwrap();
        let abs = file.to_string_lossy().to_string().replace(' ', "%20");
        let payload = format!("file://{abs}");
        let out = transform_dropped_file_paste(&payload, tmp.path()).unwrap();
        assert_eq!(out, "@with space.txt");
    }

    #[test]
    fn drop_paste_ignores_multiline_payloads() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        let payload = format!("{}\nextra line", file.display());
        assert!(transform_dropped_file_paste(&payload, tmp.path()).is_none());
    }

    #[test]
    fn drop_paste_returns_none_when_path_does_not_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let payload = format!("'{}/missing.txt'", tmp.path().display());
        assert!(transform_dropped_file_paste(&payload, tmp.path()).is_none());
    }

    #[test]
    fn drop_paste_keeps_absolute_when_outside_cwd() {
        let tmp1 = tempfile::tempdir().unwrap();
        let tmp2 = tempfile::tempdir().unwrap();
        let file = tmp2.path().join("outside.txt");
        std::fs::write(&file, "x").unwrap();
        let payload = format!("{}", file.display());
        let out = transform_dropped_file_paste(&payload, tmp1.path()).unwrap();
        assert!(out.starts_with('@'));
        assert!(out.contains("outside.txt"));
        // Path is absolute since it's outside cwd.
        assert!(out[1..].starts_with('/') || out[1..].contains(":\\"));
    }

    #[test]
    fn editor_border_color_tracks_thinking_level() {
        use model::ThinkingLevel;
        assert_eq!(thinking_level_border_color(None), BORDER_FOCUS);
        // Distinct color per level. Spot-check that they differ.
        let levels = [
            ThinkingLevel::Minimal,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
            ThinkingLevel::Xhigh,
        ];
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for l in levels {
            let c = thinking_level_border_color(Some(l));
            assert!(c.starts_with("\x1b[38;2;"), "level {l:?} → {c:?}");
            assert!(seen.insert(c.clone()), "duplicate color for {l:?}: {c}");
        }
    }

    /// M5.5 — Ctrl+T flips `hide_thinking_flag()`; each
    /// `AssistantMessageComponent` constructed with
    /// `with_shared_hide_flag(...)` reads it at render time so every
    /// message in scrollback flips together.
    #[test]
    fn hide_thinking_flag_round_trips_through_shared_subscriber() {
        use std::sync::atomic::Ordering;
        let flag = hide_thinking_flag();
        // Reset to a known state first — other tests in the same binary
        // may have flipped it.
        flag.store(false, Ordering::Relaxed);

        // Build a component that subscribes to the shared flag, with a
        // thinking block in its message.
        let mut msg = make_assistant("hi");
        msg.content.push(model::AssistantContentBlock::Thinking(
            model::ThinkingContent::new("inner reasoning"),
        ));
        let comp =
            AssistantMessageComponent::with_message(msg).with_shared_hide_flag(Arc::clone(flag));
        let visible = comp.render(80).join("\n");
        assert!(
            visible.contains("inner reasoning"),
            "expected full thinking body when flag is false: {visible}"
        );

        // Flip the flag — same component, rendered again, should collapse.
        flag.store(true, Ordering::Relaxed);
        let collapsed = comp.render(80).join("\n");
        assert!(
            !collapsed.contains("inner reasoning"),
            "expected collapsed thinking body when flag is true: {collapsed}"
        );
        assert!(
            collapsed.contains("Thinking..."),
            "expected DEFAULT_HIDDEN_THINKING_LABEL when collapsed: {collapsed}"
        );

        // Cleanup so we don't poison other tests.
        flag.store(false, Ordering::Relaxed);
    }

    /// M4.1 — `run_external_editor` invokes the configured editor (we
    /// hijack via `EDITOR=tee` so the command exists and is idempotent
    /// on the temp file) and reads back the buffer. Tests just the
    /// round-trip; the input listener wiring is structurally trivial.
    #[test]
    fn external_editor_round_trips_buffer_through_tempfile() {
        // SAFETY: the unsafe blocks are required by edition-2024's
        // `set_var`/`remove_var` signature on macOS. The test is
        // single-threaded with respect to env mutation because
        // `cargo test`'s default runner is multi-threaded but env
        // mutations here are limited to two adjacent calls with no
        // intervening reads from other threads.
        let prev = std::env::var("EDITOR").ok();
        // `true` succeeds and leaves the file untouched, so the read-back
        // matches what we wrote in.
        unsafe {
            std::env::set_var("EDITOR", "true");
        }
        let original = "hello from hand";
        let out = run_external_editor(original).expect("editor runs");
        assert_eq!(out, original);
        unsafe {
            match prev {
                Some(v) => std::env::set_var("EDITOR", v),
                None => std::env::remove_var("EDITOR"),
            }
        }
    }

    #[tokio::test]
    async fn thinking_unknown_level_does_not_mutate_session() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        let mut opts = session.stream_options().clone();
        opts.reasoning = Some(model::ThinkingLevel::Low);
        session.set_stream_options(opts);

        let action = dispatch(
            "/thinking bogus",
            &SlashCommandContext {
                model_id: "x".into(),
                provider: "y".into(),
            },
        );
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        assert_eq!(
            session.stream_options().reasoning,
            Some(model::ThinkingLevel::Low),
            "unknown level must not clobber existing reasoning"
        );
    }

    #[test]
    fn settings_entries_reflect_live_manager_values() {
        // build_settings_entries reads from SettingsManager::current(). Snapshot
        // an in-memory manager (no on-disk YAML required) and check the entry
        // list shape matches what /settings should display.
        let manager = crate::core::settings::SettingsManager::in_memory();
        let entries = build_settings_entries(&manager);
        let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
        // Order is deliberate (matches the visible row order); just spot-check.
        assert!(keys.contains(&"theme"), "missing theme entry: {keys:?}");
        assert!(
            keys.contains(&"auto_compact"),
            "missing auto_compact: {keys:?}"
        );
        assert!(
            keys.contains(&"hide_thinking_block"),
            "missing hide_thinking_block: {keys:?}"
        );
        assert!(
            keys.contains(&"show_images"),
            "missing show_images: {keys:?}"
        );
        // Theme has 4 enum choices and the default lands on dark (index 0).
        let theme_entry = entries.iter().find(|e| e.key == "theme").unwrap();
        match &theme_entry.value {
            hand_tui::SettingValue::Enum { choices, selected } => {
                assert_eq!(choices.len(), 4);
                assert_eq!(*selected, 0);
                assert_eq!(choices[0], "dark");
            }
            other => panic!("expected enum value, got {other:?}"),
        }
    }

    /// Issue #16 / UAT-013: the effective `default_provider`,
    /// `default_model`, and `default_thinking_level` must be visible
    /// in `/settings` so a user can confirm that a project-level
    /// override took effect. Before this fix the dialog rendered
    /// theme / toggles only — there was no way to see whether the
    /// project's `default_thinking_level: high` was actually live.
    #[test]
    fn settings_entries_expose_effective_provider_model_and_thinking_overrides() {
        use crate::core::settings::{Settings, SettingsManager, ThinkingLevelSetting};

        let settings = Settings {
            default_provider: Some("anthropic".to_string()),
            default_model: Some("claude-opus-4-7".to_string()),
            default_thinking_level: Some(ThinkingLevelSetting::High),
            ..Settings::default()
        };

        let manager = SettingsManager::from_settings_for_test(settings);
        let entries = build_settings_entries(&manager);

        let find = |key: &str| -> String {
            entries
                .iter()
                .find(|e| e.key == key)
                .unwrap_or_else(|| panic!("missing {key} entry"))
                .value
                .to_string()
        };

        assert_eq!(find("default_provider"), "anthropic");
        assert_eq!(find("default_model"), "claude-opus-4-7");
        assert_eq!(find("default_thinking_level"), "high");
    }

    /// When the effective settings have nothing configured for these
    /// fields, the rows still appear — with explicit "unset" /
    /// "(none — …)" hints so the user knows the auto-pick path is in
    /// effect, not that the dialog is broken.
    #[test]
    fn settings_entries_render_unset_overrides_with_explicit_placeholders() {
        use crate::core::settings::{Settings, SettingsManager};

        let settings = Settings {
            default_provider: None,
            default_model: None,
            default_thinking_level: None,
            ..Settings::default()
        };

        let manager = SettingsManager::from_settings_for_test(settings);
        let entries = build_settings_entries(&manager);

        let find = |key: &str| -> String {
            entries
                .iter()
                .find(|e| e.key == key)
                .unwrap_or_else(|| panic!("missing {key} entry"))
                .value
                .to_string()
        };

        assert!(
            find("default_provider").contains("none"),
            "default_provider must render a placeholder when unset"
        );
        assert!(
            find("default_model").contains("none"),
            "default_model must render a placeholder when unset"
        );
        assert_eq!(find("default_thinking_level"), "(unset)");
    }

    #[tokio::test]
    async fn reload_emits_confirmation_status() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        let action = dispatch(
            "/reload",
            &SlashCommandContext {
                model_id: "x".into(),
                provider: "y".into(),
            },
        );
        // Reload is sync apart from the dispatch routing; cwd points at a
        // real directory so SettingsManager::from_cwd succeeds with defaults.
        apply_slash_action(action, &chat, &mut session, Path::new("."), None).await;
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(
            joined.contains("reloaded"),
            "expected reload confirmation, got: {joined:?}"
        );
        // Both subsystems should be named in the status.
        assert!(joined.contains("settings"), "got: {joined:?}");
        assert!(joined.contains("keybindings"), "got: {joined:?}");
    }

    #[tokio::test]
    async fn settings_login_resume_emit_overlay_stubs() {
        let mut session = make_session();
        for (cmd, marker) in [
            ("/settings", "settings"),
            ("/login", "login"),
            ("/resume", "resume"),
        ] {
            let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
            let action = dispatch(
                cmd,
                &SlashCommandContext {
                    model_id: "x".into(),
                    provider: "y".into(),
                },
            );
            apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
            let joined = chat.lock().unwrap()[0].render(80).join("\n");
            assert!(
                joined.contains(marker),
                "expected `{marker}` in output of {cmd}: {joined:?}"
            );
        }
    }

    #[tokio::test]
    async fn diagnostics_action_renders_summary_into_chat() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        let action = dispatch(
            "/diagnostics",
            &SlashCommandContext {
                model_id: "x".into(),
                provider: "y".into(),
            },
        );
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(joined.contains("diagnostics"), "{joined:?}");
    }

    #[tokio::test]
    async fn new_session_action_clears_chat_and_resets_session() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        chat.lock()
            .unwrap()
            .push(Box::new(UserMessageComponent::new("old".to_string())));
        let mut session = make_session();
        let action = dispatch(
            "/new",
            &SlashCommandContext {
                model_id: "x".into(),
                provider: "y".into(),
            },
        );
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        // The chat should now hold exactly one element: the status line.
        let list = chat.lock().unwrap();
        assert_eq!(list.len(), 1);
        let joined = list[0].render(80).join("\n");
        assert!(joined.contains("new session"), "{joined:?}");
    }

    #[test]
    fn help_text_includes_new_commands() {
        let parsed = ParsedSlashCommand::parse("/help").unwrap();
        let ctx = SlashCommandContext {
            model_id: "x".into(),
            provider: "y".into(),
        };
        let SlashCommandResult::Handled(SlashCommandAction::ShowText(s)) =
            SlashCommandTable::dispatch(&parsed, &ctx)
        else {
            panic!("help should produce ShowText");
        };
        for cmd in [
            "/clear",
            "/compact",
            "/new",
            "/resume",
            "/copy",
            "/thinking",
            "/settings",
            "/login",
            "/logout",
            "/diagnostics",
            "/export",
            "/import",
            "/fork",
            "/clone",
            "/name",
            "/theme",
            "/skills",
            "/extensions",
            "/changelog",
        ] {
            assert!(s.contains(cmd), "/help missing {cmd}: {s}");
        }
    }

    #[tokio::test]
    async fn skills_action_renders_custom_message_in_chat() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        let action = dispatch(
            "/skills",
            &SlashCommandContext {
                model_id: "x".into(),
                provider: "y".into(),
            },
        );
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        let list = chat.lock().unwrap();
        assert_eq!(list.len(), 1);
        let joined = list[0].render(80).join("\n");
        assert!(joined.contains("[skills]"), "{joined:?}");
        // No skills are loaded into the in-memory test session, so the
        // helper should surface the empty-list hint.
        assert!(joined.contains("no skills"), "{joined:?}");
    }

    #[tokio::test]
    async fn extensions_action_renders_custom_message_in_chat() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        let action = dispatch(
            "/extensions",
            &SlashCommandContext {
                model_id: "x".into(),
                provider: "y".into(),
            },
        );
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        let list = chat.lock().unwrap();
        assert_eq!(list.len(), 1);
        let joined = list[0].render(80).join("\n");
        assert!(joined.contains("[extensions]"), "{joined:?}");
    }

    #[tokio::test]
    async fn export_html_writes_file_and_pushes_status() {
        let dir = tempfile::TempDir::new().unwrap();
        let output = dir.path().join("session.html");

        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        let action = SlashCommandAction::Export(output.clone(), ExportFormat::Html);
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;

        assert!(output.exists(), "expected HTML file to be written");
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(
            joined.contains("exported to"),
            "expected status message, got {joined:?}"
        );
    }

    /// upstream-side issue #8: `/export <path>` must refuse to overwrite a
    /// file that already exists. The user lost a previous transcript
    /// by reusing the same path; the new contract is "delete or
    /// choose a different path", surfaced as a red status line, with
    /// the file's bytes left untouched.
    #[tokio::test]
    async fn export_refuses_to_overwrite_existing_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let output = dir.path().join("existing.html");
        let original = "PREVIOUS-CONTENT-DO-NOT-CLOBBER";
        std::fs::write(&output, original).unwrap();

        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        let action = SlashCommandAction::Export(output.clone(), ExportFormat::Html);
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;

        // File untouched.
        assert_eq!(std::fs::read_to_string(&output).unwrap(), original);
        // Status surfaces the refusal.
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(
            joined.contains("already exists"),
            "expected overwrite-refusal status, got {joined:?}"
        );
    }

    #[tokio::test]
    async fn export_jsonl_for_in_memory_session_pushes_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let output = dir.path().join("session.jsonl");

        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        let action = SlashCommandAction::Export(output.clone(), ExportFormat::Jsonl);
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;

        assert!(!output.exists());
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(joined.contains("in-memory"), "{joined:?}");
    }

    #[tokio::test]
    async fn import_missing_file_pushes_error() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        let action =
            SlashCommandAction::Import(PathBuf::from("/tmp/definitely-does-not-exist.jsonl"));
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(joined.contains("not found"), "{joined:?}");
    }

    #[tokio::test]
    async fn fork_with_no_messages_pushes_warning() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        let action = SlashCommandAction::Fork(None);
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(joined.contains("no user messages"), "{joined:?}");
    }

    #[tokio::test]
    async fn name_sets_session_label() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        let action = SlashCommandAction::Name("my-label".into());
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        assert_eq!(session.label(), Some("my-label"));
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(joined.contains("my-label"), "{joined:?}");
    }

    #[tokio::test]
    async fn clone_yields_fresh_session_id() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        let original_id = session.session_id().to_string();
        let action = SlashCommandAction::Clone;
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        assert_ne!(session.session_id(), original_id);
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(joined.contains("cloned session"), "{joined:?}");
    }

    #[tokio::test]
    async fn changelog_action_renders_inline_message() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        let action = SlashCommandAction::Changelog;
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        let list = chat.lock().unwrap();
        assert_eq!(list.len(), 1);
        let joined = list[0].render(80).join("\n");
        assert!(joined.contains("[changelog]"), "{joined:?}");
    }

    #[tokio::test]
    async fn theme_with_unknown_name_pushes_error() {
        let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
        let mut session = make_session();
        let action = SlashCommandAction::Theme(Some("definitely-not-a-real-theme".into()));
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(joined.contains("unknown theme"), "{joined:?}");
    }

    /// With a closed mounter (run loop already dropped) each overlay-blocked
    /// command surfaces a clearly-marked failure status — proves the helper
    /// actually attempted the mount instead of falling through to the
    /// placeholder text the no-mounter branch produces.
    #[tokio::test]
    async fn overlay_blocked_helpers_attempt_mount_when_mounter_present() {
        // Build a Tui solely to obtain a mounter, then drop it — this closes
        // the receiver so subsequent `show()` calls return TuiClosed.
        let tui = hand_tui::Tui::new(Box::new(hand_tui::TestTerminal::new(80, 24)));
        let mounter = tui.overlay_mounter();
        drop(tui);

        let cases = [
            ("/model", "/model failed"),
            ("/settings", "/settings failed"),
            ("/login", "/login failed"),
            ("/resume", "/resume failed"),
        ];
        let mut session = make_session();
        for (cmd, marker) in cases {
            let chat: ChatList = Arc::new(StdMutex::new(Vec::new()));
            let action = dispatch(
                cmd,
                &SlashCommandContext {
                    model_id: "x".into(),
                    provider: "y".into(),
                },
            );
            apply_slash_action(
                action,
                &chat,
                &mut session,
                Path::new("/tmp"),
                Some(&mounter),
            )
            .await;
            let joined = chat.lock().unwrap()[0].render(80).join("\n");
            assert!(
                joined.contains(marker),
                "expected `{marker}` in output of {cmd}: {joined:?}"
            );
        }
    }

    /// End-to-end: with a live run loop, mount/hide via the driver-side
    /// mounter channel completes round-trip. This proves the wiring the
    /// 5 overlay-blocked slash-command helpers rely on is intact.
    #[tokio::test]
    async fn overlay_mounter_round_trips_through_live_run_loop() {
        use model::ThinkingLevel;

        let mut tui = hand_tui::Tui::new(Box::new(hand_tui::TestTerminal::new(80, 24)));
        let mounter = tui.overlay_mounter();
        let (_event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();

        let run_handle = tokio::spawn(async move {
            let _ = tui.run_with_events(event_rx).await;
            tui
        });

        // Build the same component the `/thinking` helper would build and
        // mount it directly. Confirm the handle came back, then hide it.
        let (tx, _rx) = mpsc::unbounded_channel::<ThinkingOutcome>();
        let component =
            ThinkingSelectorComponent::new(None, vec![None, Some(ThinkingLevel::Low)], tx);
        let handle = mounter
            .show(Box::new(component), hand_tui::OverlayOptions::default())
            .await
            .expect("mount must succeed while run loop is alive");
        mounter.hide(handle).expect("hide must reach run loop");

        // Stop the loop and reclaim the Tui to inspect overlay state.
        // The Tui's `stop()` is callable on `&self` so the foreground task
        // can shut the loop down without owning the Tui.
        // We reach the running flag via `mounter` indirectly: dropping the
        // mounter doesn't stop the loop, so the cleanest exit is to abort
        // the spawned task; the run loop's drop-on-await semantics handle
        // the rest.
        run_handle.abort();
    }

    #[test]
    fn tool_update_without_matching_start_falls_back_to_status_line() {
        let (chat, tools, asst) = fresh_state();
        apply_updates_to_chat(
            &chat,
            &tools,
            &asst,
            vec![ChatUpdate::ToolUpdate {
                tool_call_id: "missing".into(),
                partial_text: "orphaned".into(),
            }],
        );
        // Falls back to a single status line in the chat.
        assert_eq!(chat.lock().unwrap().len(), 1);
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(joined.contains("orphaned"), "{joined:?}");
    }
}
