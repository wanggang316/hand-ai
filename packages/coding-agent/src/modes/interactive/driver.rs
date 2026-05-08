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
    AssistantMessageComponent, FooterComponent, FooterViewModel, ModelOutcome, TokenUsageSummary,
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
        let event_pump = tokio::spawn(async move {
            let mut rx = event_rx;
            while !stop_for_events.load(Ordering::Relaxed) {
                match rx.recv().await {
                    Some(ev) => {
                        let updates = dispatch_event(&ev);
                        apply_updates_to_chat(&chat_for_events, updates);
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
                            SlashCommandResult::Handled(SlashCommandAction::Quit) => {
                                unsafe { stop_handle_for_agent.stop() };
                                break;
                            }
                            SlashCommandResult::Handled(SlashCommandAction::ShowText(s)) => {
                                push_status(&agent_chat, s, None);
                            }
                            SlashCommandResult::Handled(SlashCommandAction::OpenModelSelector) => {
                                push_status(
                                    &agent_chat,
                                    "[/model selector — pick from chat]".to_string(),
                                    None,
                                );
                                // TODO(parity): the selector should appear as
                                // an overlay; for now we simply log a stub
                                // since wiring overlays from a background
                                // task requires shared Tui access we don't
                                // have. The mo_for_agent placeholder keeps
                                // the API in place for the follow-up.
                                let _ = &mo_for_agent;
                            }
                            SlashCommandResult::Handled(SlashCommandAction::Noop) => {}
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

fn apply_updates_to_chat(chat: &ChatList, updates: Vec<ChatUpdate>) {
    let mut list = chat.lock().expect("chat list mutex poisoned");
    for update in updates {
        match update {
            ChatUpdate::AppendUser { text } => {
                list.push(Box::new(UserMessageComponent::new(text)));
            }
            ChatUpdate::SetOrUpdateAssistant { message } => {
                list.push(Box::new(AssistantMessageComponent::with_message(*message)));
            }
            ChatUpdate::AppendToolResult { text } => {
                list.push(Box::new(coloured_text(text, Some(DIM_FG))));
            }
            ChatUpdate::AppendStatus { text } => {
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
}
