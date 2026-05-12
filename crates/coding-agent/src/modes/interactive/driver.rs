//! `InteractiveMode` — TUI driver wiring [`AgentSession`], a [`Tui`], and the
//! chat / footer / editor components into a runnable interactive session.
//!
//! This is a deliberately minimal port of pi-mono's
//! `interactive-mode.ts` (5500 LOC). The skeleton covers the happy path:
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
    Component, EditorComponent, Focusable, InputEvent, KeyName, ListenerResult, OverlayMounter,
    OverlayOptions, ProcessTerminal, TextComponent, Tui, TuiError,
};
use tokio::sync::mpsc;

use crate::core::agent_session::{AgentSession, AgentSessionEvent};
use crate::core::error::CodingAgentError;

use super::components::{
    AssistantMessageComponent, AuthSelectorMode, AuthSelectorProvider, BashExecutionComponent,
    BashStatus, CustomMessageComponent, CustomMessageData, FooterComponent, FooterViewModel,
    LoginDialogComponent, LoginDialogEvent, ModelOutcome, ModelSelectorComponent, OAuthOutcome,
    OAuthSelectorComponent, SessionSelectorComponent, SessionSelectorEvent,
    SettingsSelectorComponent, SettingsSelectorEvent, ThemeOutcome, ThemeSelectorComponent,
    ThinkingOutcome, ThinkingSelectorComponent, TokenUsageSummary, ToolExecutionComponent,
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
            has_reasoning: false, // TODO(parity): derive from model capabilities.
            thinking_level: String::new(),
            available_provider_count: 1, // TODO(parity): derive from credentials registry.
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
        let pending_for_submit = Arc::clone(&pending);
        let editor = EditorComponent::new()
            .with_border(true)
            .with_viewport_height(4)
            .with_placeholder(EDITOR_PLACEHOLDER)
            .with_border_color(BORDER_DIM)
            .with_focused_border_color(BORDER_FOCUS)
            .with_on_submit(move |text: String| {
                if let Ok(mut p) = pending_for_submit.lock() {
                    p.text = Some(text);
                }
            });

        // Build the TUI tree.
        let terminal = Box::new(ProcessTerminal::new()?);
        let mut tui = Tui::new(terminal);
        tui.root_mut().add_child_with_id(Box::new(ChatScrollback {
            list: Arc::clone(&chat),
        }));
        let editor_id = tui.root_mut().add_child_with_id(Box::new(editor));
        tui.root_mut()
            .add_child_with_id(Box::new(TextComponent::new(build_hint_line())));
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

        // Stop signal for background tasks.
        let stop = Arc::new(AtomicBool::new(false));

        // Background task: drains agent events into the chat list AND
        // accumulates token usage from `MessageEnd` events into the running
        // usage counter so the footer reflects spend in real time.
        let chat_for_events = Arc::clone(&chat);
        let stop_for_events = Arc::clone(&stop);
        let tool_handles: ToolHandles = Arc::new(StdMutex::new(HashMap::new()));
        let tools_for_events = Arc::clone(&tool_handles);
        let assistant_handle: AssistantHandle = Arc::new(StdMutex::new(None));
        let assistant_for_events = Arc::clone(&assistant_handle);
        let usage_for_events = Arc::clone(&usage);
        let event_pump = tokio::spawn(async move {
            let mut rx = event_rx;
            while !stop_for_events.load(Ordering::Relaxed) {
                match rx.recv().await {
                    Some(ev) => {
                        if let AgentSessionEvent::Agent(agent_ev) = &ev
                            && let hand_agent::types::AgentEvent::MessageEnd { message } =
                                agent_ev.as_ref()
                            && let model::Message::Assistant(a) = message
                        {
                            accumulate_usage(&usage_for_events, &a.usage);
                        }
                        let updates = dispatch_event(&ev);
                        apply_updates_to_chat(
                            &chat_for_events,
                            &tools_for_events,
                            &assistant_for_events,
                            updates,
                        );
                    }
                    None => break,
                }
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
        let stop_for_agent = Arc::clone(&stop);
        let stop_handle_for_agent = Arc::clone(&stop_handle);
        let agent_cwd = cwd.clone();
        let agent_mounter = tui.overlay_mounter();
        let agent_task = tokio::spawn(async move {
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
                    unsafe { stop_handle_for_agent.stop() };
                    break;
                }
                if let Some(text) = submitted {
                    let trimmed = text.trim().to_string();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Some(parsed) = ParsedSlashCommand::parse(&trimmed) {
                        let ctx = SlashCommandContext {
                            model_id: session.model().id.clone(),
                            provider: session.model().provider.as_str().to_string(),
                        };
                        match SlashCommandTable::dispatch(&parsed, &ctx) {
                            SlashCommandResult::Handled(action) => {
                                let outcome = apply_slash_action(
                                    action,
                                    &agent_chat,
                                    &mut session,
                                    &cwd,
                                    Some(&mounter),
                                )
                                .await;
                                if matches!(outcome, SlashOutcome::Quit) {
                                    unsafe { stop_handle_for_agent.stop() };
                                    break;
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
                    if let Err(e) = session.send_message(&trimmed).await {
                        push_error(&agent_chat, format!("send failed: {e}"));
                    }
                    refresh_footer(&session, &cwd, &agent_footer, &agent_usage);
                }
            }
        });

        // Run the Tui — this blocks until `tui.stop()` fires from the agent
        // task (or stdin closes).
        tui.run().await?;

        // Shutdown.
        stop.store(true, Ordering::Relaxed);
        let _ = agent_task.await;
        let _ = event_pump.await;
        Ok(())
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
/// Placeholder text shown inside the editor while the buffer is empty.
const EDITOR_PLACEHOLDER: &str = "Type your message — Enter to send, Shift+Enter for newline, / for commands";

/// Build the single dim hint line rendered between the editor and the footer.
fn build_hint_line() -> String {
    use super::components::keybinding_hints::raw_key_hint;
    let hints = [
        raw_key_hint("↵", "send"),
        raw_key_hint("⇧↵", "newline"),
        raw_key_hint("/", "commands"),
        raw_key_hint("^D", "quit"),
    ];
    hints.join("  ")
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
    let body = format!("\x1b[1;97;41m ✘ Error  \x1b[0m \x1b[1;91m{}{RESET}", msg.as_ref());
    let mut list = chat.lock().expect("chat list mutex poisoned");
    list.push(Box::new(TextComponent::new(body)));
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
                list.push(Box::new(AssistantMessageComponent::with_message(a.clone())));
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
                let cell = Arc::new(StdMutex::new(AssistantMessageComponent::with_message(
                    *message,
                )));
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
                    let cell = Arc::new(StdMutex::new(AssistantMessageComponent::with_message(
                        *message,
                    )));
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
    let mut list = chat.lock().expect("chat list mutex poisoned");
    list.push(Box::new(coloured_text(text, color_prefix)));
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
            mount_settings_selector(chat, mounter).await;
        }
        SlashCommandAction::OpenLoginDialog { provider } => {
            let chosen = match provider {
                Some(p) => Some(p),
                None => mount_login_provider_picker(chat, session, mounter).await,
            };
            if let Some(provider_id) = chosen {
                mount_login_key_input(chat, &provider_id, mounter).await;
            }
        }
        SlashCommandAction::OpenResumePicker => {
            mount_resume_picker(chat, cwd, mounter).await;
        }
        SlashCommandAction::ClearChat => {
            if let Ok(mut list) = chat.lock() {
                list.clear();
            }
            push_status(chat, "[chat cleared]".to_string(), None);
        }
        SlashCommandAction::Compact => match session.compact().await {
            Ok(summary) => {
                use super::components::{CompactionSummaryData, CompactionSummaryMessageComponent};
                let tokens_before = session.message_count() as u64;
                let data = CompactionSummaryData::new(summary, tokens_before);
                if let Ok(mut list) = chat.lock() {
                    list.push(Box::new(CompactionSummaryMessageComponent::new(data)));
                }
            }
            Err(e) => push_status(chat, format!("[compact failed: {e}]"), Some(RED_FG)),
        },
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
        SlashCommandAction::ShowDiagnostics => {
            let report = crate::core::diagnostics::run_diagnostics();
            let body = format_diagnostics_report(&report);
            push_status(chat, body, None);
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
            apply_fork(chat, session, entry_id.as_deref());
        }
        SlashCommandAction::Clone => {
            apply_clone(chat, session);
        }
        SlashCommandAction::Name(label) => match session.set_label(&label) {
            Ok(()) => push_status(chat, format!("[session name set: {label}]"), None),
            Err(e) => push_status(chat, format!("[/name failed: {e}]"), Some(RED_FG)),
        },
        SlashCommandAction::Theme(arg) => {
            apply_theme(chat, arg, mounter).await;
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
    // TODO(parity): scoped models are not yet plumbed through the session
    // — pi-mono pulls these from the per-cwd settings file. The overlay
    // works fine with an empty scoped list (scope toggle hidden).
    let scoped_models: Vec<model::Model> = Vec::new();
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
    _session: &mut AgentSession,
    inline_level: Option<String>,
    mounter: Option<&OverlayMounter>,
) {
    use model::ThinkingLevel;

    // Inline form (`/thinking high`) bypasses the picker.
    if let Some(level) = inline_level {
        // TODO(parity): apply the parsed level to the active session once
        // AgentSession exposes a thinking-level setter. For now, surface
        // the choice in the status line so the user sees it took effect.
        push_status(chat, format!("[thinking level: {level}]"), None);
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
    // TODO(parity): read the current thinking level from the session once
    // exposed; for now we default to "off" so the cursor lands on the
    // first row.
    let component = ThinkingSelectorComponent::new(None, available_levels, tx);
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
        Some(ThinkingOutcome::Selected(Some(level))) => {
            push_status(
                chat,
                format!("[thinking level: {}]", level_label(level)),
                None,
            );
        }
        Some(ThinkingOutcome::Selected(None)) => {
            push_status(chat, "[thinking off]".to_string(), None);
        }
        Some(ThinkingOutcome::Cancelled) | None => {
            push_status(chat, "[/thinking cancelled]".to_string(), None);
        }
    }
    let _ = mounter.hide(handle);
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

async fn mount_settings_selector(chat: &ChatList, mounter: Option<&OverlayMounter>) {
    let Some(mounter) = mounter else {
        push_status(chat, "[/settings opened]".to_string(), None);
        return;
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<SettingsSelectorEvent>();
    // TODO(parity): build the entries list from the active SettingsManager.
    // The pi-mono port enumerates a fixed set of settings (theme, auto-
    // compaction, etc.) — porting the full list is out of scope for this
    // batch. The selector renders the dialog frame even with no entries.
    let entries = Vec::new();
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
            push_status(chat, format!("[setting {id} = {value}]"), None);
        }
        Some(SettingsSelectorEvent::Cancelled) | None => {
            push_status(chat, "[/settings closed]".to_string(), None);
        }
    }
    let _ = mounter.hide(handle);
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
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
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
    // pi-ai OAuth registry once it's wired in. For now the dialog runs a
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
                let result = AuthStorage::new()
                    .and_then(|s| s.set(&canonical, AuthRecord::ApiKey { key }));
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

async fn mount_resume_picker(chat: &ChatList, cwd: &Path, mounter: Option<&OverlayMounter>) {
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
            // TODO(parity): re-open the session in-place. The TS source
            // calls `SessionManager.open(path)` and swaps the live session;
            // the Rust port doesn't yet support hot-swap, so for now we
            // surface the chosen path and let the user restart.
            push_status(
                chat,
                format!("[would resume: {}]", path.display()),
                Some(YELLOW_FG),
            );
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
    use crate::core::export::{export_to_html, export_to_jsonl};
    match fmt {
        ExportFormat::Jsonl | ExportFormat::Json => {
            // For `.json` we still copy the JSONL stream verbatim — pi-mono
            // has no separate JSON exporter and the JSONL form parses as a
            // sequence of JSON values, which is what most consumers expect.
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
                        "[/export: cannot export an in-memory session as JSONL]".to_string(),
                        Some(RED_FG),
                    );
                    return;
                }
            };
            match export_to_jsonl(&manager, path) {
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
        ExportFormat::Markdown => {
            // TODO(parity-M6): markdown export lands with the M6 batch.
            push_status(
                chat,
                "[/export: markdown export not yet implemented (tracked in M6)]".to_string(),
                Some(YELLOW_FG),
            );
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
fn apply_fork(chat: &ChatList, session: &mut AgentSession, entry_id: Option<&str>) {
    let target = match entry_id {
        Some(id) => id.to_string(),
        None => {
            let entries = session.fork_messages();
            match entries.last() {
                Some(entry) => entry.entry_id.clone(),
                None => {
                    push_status(
                        chat,
                        "[/fork: no user messages to fork from]".to_string(),
                        Some(YELLOW_FG),
                    );
                    return;
                }
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
async fn apply_theme(chat: &ChatList, arg: Option<String>, mounter: Option<&OverlayMounter>) {
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
                let mut list = chat.lock().expect("chat list mutex poisoned");
                list.push(Box::new(coloured_text(
                    format!("[theme: {name}]"),
                    Some(YELLOW_FG),
                )));
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
                let mut list = chat.lock().expect("chat list mutex poisoned");
                list.push(Box::new(coloured_text(
                    format!("[theme: {name}]"),
                    Some(YELLOW_FG),
                )));
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
    let mut list = chat.lock().expect("chat list mutex poisoned");
    list.push(Box::new(component));
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
    let mut list = chat.lock().expect("chat list mutex poisoned");
    list.push(Box::new(component));
}

/// `/changelog` — render the agent's CHANGELOG.md (if present) as a custom
/// message.
fn apply_changelog(chat: &ChatList) {
    use crate::utils::changelog::parse_changelog_file;
    let candidates = [
        PathBuf::from("CHANGELOG.md"),
        PathBuf::from("crates/coding-agent/CHANGELOG.md"),
    ];
    let mut entries: Vec<crate::utils::changelog::ChangelogEntry> = Vec::new();
    for path in &candidates {
        if let Ok(parsed) = parse_changelog_file(path)
            && !parsed.is_empty()
        {
            entries = parsed;
            break;
        }
    }

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
    let mut list = chat.lock().expect("chat list mutex poisoned");
    list.push(Box::new(component));
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
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp"), None).await;
        let joined = chat.lock().unwrap()[0].render(80).join("\n");
        assert!(joined.contains("high"), "{joined:?}");
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
        let action = SlashCommandAction::Import(PathBuf::from("/tmp/definitely-does-not-exist.jsonl"));
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
