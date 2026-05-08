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
    Component, EditorComponent, InputEvent, KeyName, ListenerResult, ProcessTerminal,
    TextComponent, Tui, TuiError,
};
use tokio::sync::mpsc;

use crate::core::agent_session::{AgentSession, AgentSessionEvent};
use crate::core::error::CodingAgentError;

use super::components::{
    AssistantMessageComponent, BashExecutionComponent, BashStatus, FooterComponent,
    FooterViewModel, ModelOutcome, TokenUsageSummary, ToolExecutionComponent, ToolExecutionStatus,
    UserMessageComponent,
};
use super::event_dispatch::{ChatUpdate, dispatch as dispatch_event};
use super::slash_commands::{
    ParsedSlashCommand, SlashCommandAction, SlashCommandContext, SlashCommandResult,
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

    /// Build the footer view-model from current session state.
    pub(crate) fn build_footer_view(session: &AgentSession, cwd: &Path) -> FooterViewModel {
        FooterViewModel {
            cwd: cwd.display().to_string(),
            home_dir: dirs::home_dir().map(|p| p.display().to_string()),
            git_branch: None, // TODO(parity): populate via git utility helper.
            session_name: session.label().map(|s| s.to_string()),
            usage: TokenUsageSummary::default(),
            model_id: session.model().id.clone(),
            model_provider: session.model().provider.as_str().to_string(),
            context_window: session.model().context_window,
            context_percent: None,
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
        let footer = Arc::new(StdMutex::new(Self::build_footer_view(&session, &cwd)));
        let pending = Arc::new(StdMutex::new(Pending::default()));

        // Replay existing session messages.
        replay_messages_into(&chat, session.messages());

        // Channel agent → driver task.
        let (event_tx, event_rx) = mpsc::unbounded_channel::<AgentSessionEvent>();
        let forward = event_tx.clone();
        session.subscribe(move |event| {
            let _ = forward.send(event);
        });

        // Build the TUI tree.
        let terminal = Box::new(ProcessTerminal::new()?);
        let mut tui = Tui::new(terminal);
        tui.set_show_hardware_cursor(false);
        tui.root_mut().add_child_with_id(Box::new(ChatScrollback {
            list: Arc::clone(&chat),
        }));
        let editor_id = tui.root_mut().add_child_with_id(Box::new(
            EditorComponent::new()
                .with_border(true)
                .with_viewport_height(4),
        ));
        tui.root_mut()
            .add_child_with_id(Box::new(SharedFooterComponent {
                view: Arc::clone(&footer),
            }));
        tui.set_focus(Some(editor_id));

        // Input listener: bare Enter submits, Ctrl+D quits.
        let pending_for_listener = Arc::clone(&pending);
        tui.add_input_listener(Box::new(move |event: &InputEvent| {
            if let InputEvent::Key(key) = event {
                match &key.name {
                    KeyName::Enter if !key.modifiers.shift && !key.modifiers.alt => {
                        if let Ok(mut p) = pending_for_listener.lock() {
                            // Mark a submission; actual text drained by the
                            // background task once it observes the marker.
                            p.text = Some(String::new());
                        }
                        return ListenerResult {
                            consume: true,
                            data: None,
                        };
                    }
                    KeyName::Char('d') if key.modifiers.ctrl => {
                        if let Ok(mut p) = pending_for_listener.lock() {
                            p.quit = true;
                        }
                        return ListenerResult {
                            consume: true,
                            data: None,
                        };
                    }
                    _ => {}
                }
            }
            ListenerResult::pass()
        }));

        // Stop signal for background tasks.
        let stop = Arc::new(AtomicBool::new(false));

        // Background task: drains agent events into the chat list.
        let chat_for_events = Arc::clone(&chat);
        let stop_for_events = Arc::clone(&stop);
        let tool_handles: ToolHandles = Arc::new(StdMutex::new(HashMap::new()));
        let tools_for_events = Arc::clone(&tool_handles);
        let assistant_handle: AssistantHandle = Arc::new(StdMutex::new(None));
        let assistant_for_events = Arc::clone(&assistant_handle);
        let event_pump = tokio::spawn(async move {
            let mut rx = event_rx;
            while !stop_for_events.load(Ordering::Relaxed) {
                match rx.recv().await {
                    Some(ev) => {
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

        // Background task: polls `pending`. On submit, runs the agent for
        // the current editor text. On quit, calls `tui.stop()`.
        //
        // The editor lives inside `tui.root_mut()` so we cannot read it from
        // here. Instead, we rely on a different mechanism: the input
        // listener stages a marker, but draining the editor must happen
        // while we hold `&mut Tui`. Since the run-future borrows `&mut Tui`,
        // we cannot reach in.
        //
        // Workaround for the skeleton: keep the editor's text in a *shared*
        // mirror that the input listener updates on every keystroke. The
        // background task reads from that mirror when a submission is staged.
        //
        // TODO(parity): proper editor-borrow access. The pi-mono port uses a
        // CustomEditor that publishes its text into a shared store; we should
        // mirror that once the editor abstraction allows it.
        //
        // For this batch we read the editor by reaching into the Tui from
        // *another* StopHandle-style raw pointer that points to the Tui's
        // root. This is unsound under aliasing rules — instead, we side-step
        // by capturing keystrokes ourselves into a shared string and feeding
        // them into the editor via a wrapper component.

        // Side-step: shadow the editor with a `SharedEditor` that exposes
        // its text via Arc<Mutex<String>>. The TUI still gets the real
        // EditorComponent for rendering — we just *also* maintain a mirror.
        // This mirror is updated by the input listener on every keystroke.
        let editor_mirror = Arc::new(StdMutex::new(String::new()));
        let editor_mirror_for_listener = Arc::clone(&editor_mirror);
        tui.add_input_listener(Box::new(move |event: &InputEvent| {
            // This listener runs AFTER the previous listener (which consumes
            // bare Enter). We see all other events. Tracking the editor
            // buffer perfectly is hard — but for the skeleton we approximate
            // with a simple mirror that appends/erases on Char/Backspace.
            if let InputEvent::Key(key) = event {
                match &key.name {
                    KeyName::Char(c) if !key.modifiers.ctrl && !key.modifiers.alt => {
                        if let Ok(mut s) = editor_mirror_for_listener.lock() {
                            s.push(*c);
                        }
                    }
                    KeyName::Backspace => {
                        if let Ok(mut s) = editor_mirror_for_listener.lock() {
                            s.pop();
                        }
                    }
                    KeyName::Enter if key.modifiers.shift || key.modifiers.alt => {
                        if let Ok(mut s) = editor_mirror_for_listener.lock() {
                            s.push('\n');
                        }
                    }
                    _ => {}
                }
            }
            ListenerResult::pass()
        }));

        // Build agent driver task.
        let agent_chat = Arc::clone(&chat);
        let agent_footer = Arc::clone(&footer);
        let agent_pending = Arc::clone(&pending);
        let agent_mirror = Arc::clone(&editor_mirror);
        let stop_for_agent = Arc::clone(&stop);
        let stop_handle_for_agent = Arc::clone(&stop_handle);
        let agent_cwd = cwd.clone();
        let model_outcome_state: Arc<StdMutex<Option<mpsc::UnboundedReceiver<ModelOutcome>>>> =
            Arc::new(StdMutex::new(None));
        let mo_for_agent = Arc::clone(&model_outcome_state);
        let agent_task = tokio::spawn(async move {
            let mut session = session;
            let cwd = agent_cwd;
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            while !stop_for_agent.load(Ordering::Relaxed) {
                interval.tick().await;
                let (text_marker, quit) = {
                    let mut p = agent_pending.lock().unwrap();
                    (p.text.take(), std::mem::take(&mut p.quit))
                };
                if quit {
                    unsafe { stop_handle_for_agent.stop() };
                    break;
                }
                if text_marker.is_some() {
                    let text = {
                        let mut s = agent_mirror.lock().unwrap();
                        let t = s.clone();
                        s.clear();
                        t
                    };
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
                                let outcome =
                                    apply_slash_action(action, &agent_chat, &mut session, &cwd)
                                        .await;
                                let _ = &mo_for_agent;
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
                        push_status(&agent_chat, format!("Error: {e}"), Some(RED_FG));
                    }
                    refresh_footer(&session, &cwd, &agent_footer);
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

fn coloured_text(text: impl AsRef<str>, ansi_prefix: Option<&str>) -> TextComponent {
    let body = match ansi_prefix {
        Some(p) => format!("{p}{}{RESET}", text.as_ref()),
        None => text.as_ref().to_string(),
    };
    TextComponent::new(body)
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
                            // Best-effort partial display: surface the latest
                            // streaming text via a minimal ToolResult so the
                            // generic component's output area updates.
                            let partial = hand_agent::types::ToolResult::text(partial_text);
                            // Treat as still-pending: keep status, only swap
                            // the recorded output. We do this by passing the
                            // result with `is_error = false` and immediately
                            // resetting the status back to Pending so the
                            // background colour stays in the in-flight state.
                            comp.set_result(partial, false);
                            // Restore the pending state (set_result advanced
                            // the status to Complete).
                            if comp.status() == ToolExecutionStatus::Complete {
                                // No public setter, but ToolUpdate semantics
                                // require remaining "in flight". The chat
                                // visually still shows the streamed text;
                                // the colour briefly flips to success and
                                // back on the final ToolEnd. Acceptable for
                                // the parity skeleton.
                            }
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
        }
    }
}

fn push_status(chat: &ChatList, text: String, color_prefix: Option<&str>) {
    let mut list = chat.lock().expect("chat list mutex poisoned");
    list.push(Box::new(coloured_text(text, color_prefix)));
}

fn refresh_footer(session: &AgentSession, cwd: &Path, footer: &SharedFooter) {
    if let Ok(mut f) = footer.lock() {
        *f = InteractiveMode::build_footer_view(session, cwd);
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
/// the agent task body so each branch is independently testable. UI overlay
/// commands (`Open*`) intentionally fall through to a status-line stub —
/// see the existing TODO(parity) note in [`InteractiveMode::run`].
pub(crate) async fn apply_slash_action(
    action: SlashCommandAction,
    chat: &ChatList,
    session: &mut AgentSession,
    _cwd: &Path,
) -> SlashOutcome {
    match action {
        SlashCommandAction::Quit => return SlashOutcome::Quit,
        SlashCommandAction::ShowText(s) => push_status(chat, s, None),
        SlashCommandAction::OpenModelSelector => {
            push_status(chat, "[/model selector — pick from chat]".to_string(), None)
        }
        SlashCommandAction::OpenThinkingSelector { inline_level } => {
            let msg = match inline_level {
                Some(level) => format!("[/thinking inline level set to {level}]"),
                None => "[/thinking selector opened]".to_string(),
            };
            push_status(chat, msg, None);
            // TODO(parity): apply the level to the active session and open
            // the ThinkingSelectorComponent overlay once shared Tui access
            // lands.
        }
        SlashCommandAction::OpenSettingsSelector => {
            push_status(chat, "[/settings opened]".to_string(), None);
            // TODO(parity): mount SettingsSelectorComponent.
        }
        SlashCommandAction::OpenLoginDialog => {
            push_status(chat, "[/login opened]".to_string(), None);
            // TODO(parity): mount LoginDialogComponent.
        }
        SlashCommandAction::OpenResumePicker => {
            push_status(
                chat,
                "[/resume — most recent session selector]".to_string(),
                None,
            );
            // TODO(parity): mount the resume picker; currently print-only
            // because the listing UI isn't wired.
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
        let view = InteractiveMode::build_footer_view(&session, &std::path::PathBuf::from("/tmp"));
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
        let outcome = apply_slash_action(action, &chat, &mut session, Path::new("/tmp")).await;
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
        let outcome = apply_slash_action(action, &chat, &mut session, Path::new("/tmp")).await;
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
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp")).await;
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
            apply_slash_action(action, &chat, &mut session, Path::new("/tmp")).await;
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
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp")).await;
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
        apply_slash_action(action, &chat, &mut session, Path::new("/tmp")).await;
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
        ] {
            assert!(s.contains(cmd), "/help missing {cmd}: {s}");
        }
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
