//! High-level `Agent` — stateful wrapper around the agent loop.
//!
//! Mirrors `pi-agent-core/src/agent.ts`. Owns the transcript, dispatches events
//! to subscribers, manages steering / follow-up queues, and exposes
//! cancellation via `tokio_util::sync::CancellationToken`.

use crate::agent_loop::{
    AgentEventSink, AgentLoopResult, now_ms, run_agent_loop_continue, run_agent_loop_with_messages,
};
use crate::error::AgentError;
use crate::types::{
    AfterToolCallHook, AgentContext, AgentEvent, AgentLoopConfig, AgentState, AgentTool,
    BeforeToolCallHook, ConvertToLlmFn, GetApiKeyFn, GetFollowUpMessagesFn, GetSteeringMessagesFn,
    QueueDeliveryMode, ShouldStopAfterTurnFn, ToolExecutionMode, TransformContextFn,
};
use model::{
    AssistantMessage, ImageContent, Message, SimpleStreamOptions, StopReason, TextContent,
    ThinkingLevel, Usage, UserContentBlock, UserMessage,
};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Shared runtime state (written by event sink, read via Agent::state())
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RuntimeState {
    is_streaming: bool,
    streaming_message: Option<Message>,
    pending_tool_calls: HashSet<String>,
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Listener registry
// ---------------------------------------------------------------------------

/// Listener function signature: receives a borrowed event and the run's cancellation token.
pub type Listener = Arc<dyn Fn(&AgentEvent, &CancellationToken) + Send + Sync>;

#[derive(Default)]
struct ListenerRegistry {
    next_id: u64,
    listeners: Vec<(u64, Listener)>,
}

impl ListenerRegistry {
    fn add(&mut self, listener: Listener) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.listeners.push((id, listener));
        id
    }

    fn remove(&mut self, id: u64) {
        self.listeners.retain(|(i, _)| *i != id);
    }

    fn snapshot(&self) -> Vec<Listener> {
        self.listeners.iter().map(|(_, l)| l.clone()).collect()
    }
}

/// RAII handle that unsubscribes the listener on drop.
pub struct SubscriptionHandle {
    id: u64,
    registry: Arc<Mutex<ListenerRegistry>>,
}

impl Drop for SubscriptionHandle {
    fn drop(&mut self) {
        if let Ok(mut reg) = self.registry.lock() {
            reg.remove(self.id);
        }
    }
}

// ---------------------------------------------------------------------------
// Pending message queue
// ---------------------------------------------------------------------------

#[derive(Default)]
struct PendingQueue {
    mode: QueueDeliveryMode,
    messages: Vec<Message>,
}

impl PendingQueue {
    fn new(mode: QueueDeliveryMode) -> Self {
        Self {
            mode,
            messages: Vec::new(),
        }
    }

    fn enqueue(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    fn drain(&mut self) -> Vec<Message> {
        match self.mode {
            QueueDeliveryMode::All => std::mem::take(&mut self.messages),
            QueueDeliveryMode::OneAtATime => {
                if self.messages.is_empty() {
                    Vec::new()
                } else {
                    vec![self.messages.remove(0)]
                }
            }
        }
    }

    fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }

    fn clear(&mut self) {
        self.messages.clear();
    }
}

// ---------------------------------------------------------------------------
// Prompt input abstraction
// ---------------------------------------------------------------------------

/// Anything that can be turned into a starting prompt for `Agent::prompt`.
pub trait IntoPromptInput {
    fn into_prompt(self) -> Vec<Message>;
}

impl IntoPromptInput for &str {
    fn into_prompt(self) -> Vec<Message> {
        vec![Message::User(UserMessage::new_text(self))]
    }
}

impl IntoPromptInput for String {
    fn into_prompt(self) -> Vec<Message> {
        vec![Message::User(UserMessage::new_text(self))]
    }
}

impl IntoPromptInput for Message {
    fn into_prompt(self) -> Vec<Message> {
        vec![self]
    }
}

impl IntoPromptInput for Vec<Message> {
    fn into_prompt(self) -> Vec<Message> {
        self
    }
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

/// High-level agent. Wraps the low-level loop and adds transcript ownership,
/// listener dispatch, queued steering / follow-up, and cancellation.
pub struct Agent {
    client: model::Client,
    model: model::Model,

    // Persistent (non-runtime) state
    system_prompt: String,
    messages: Vec<Message>,
    tools: Vec<AgentTool>,
    stream_options: SimpleStreamOptions,
    thinking_level: Option<ThinkingLevel>,
    tool_execution: ToolExecutionMode,

    // Hooks
    before_tool_call: Option<BeforeToolCallHook>,
    after_tool_call: Option<AfterToolCallHook>,
    should_stop_after_turn: Option<ShouldStopAfterTurnFn>,
    convert_to_llm: Option<ConvertToLlmFn>,
    transform_context: Option<TransformContextFn>,
    get_api_key: Option<GetApiKeyFn>,

    // Queues
    steering_queue: Arc<Mutex<PendingQueue>>,
    follow_up_queue: Arc<Mutex<PendingQueue>>,

    // Runtime state (shared with event sink)
    runtime: Arc<RwLock<RuntimeState>>,

    // Listener registry
    listeners: Arc<Mutex<ListenerRegistry>>,

    // Cancellation — kept behind an Arc<Mutex<...>> so an `AbortHandle` (or
    // `Agent::abort()`) can cancel the in-flight run from another task without
    // borrowing `self`.
    cancel: Arc<Mutex<CancellationToken>>,

    // Misc
    max_retry_delay_ms: Option<u64>,

    // Optional custom streaming transport.
    stream_fn: Option<crate::types::StreamFn>,
}

/// Options for constructing an `Agent`.
#[derive(Default)]
pub struct AgentOptions {
    pub system_prompt: Option<String>,
    pub initial_messages: Option<Vec<Message>>,
    pub tools: Option<Vec<AgentTool>>,
    pub stream_options: Option<SimpleStreamOptions>,
    pub thinking_level: Option<ThinkingLevel>,
    pub tool_execution: Option<ToolExecutionMode>,
    pub steering_mode: Option<QueueDeliveryMode>,
    pub follow_up_mode: Option<QueueDeliveryMode>,
    pub before_tool_call: Option<BeforeToolCallHook>,
    pub after_tool_call: Option<AfterToolCallHook>,
    pub should_stop_after_turn: Option<ShouldStopAfterTurnFn>,
    pub convert_to_llm: Option<ConvertToLlmFn>,
    pub transform_context: Option<TransformContextFn>,
    pub get_api_key: Option<GetApiKeyFn>,
    pub max_retry_delay_ms: Option<u64>,
    /// Optional custom streaming transport (e.g., `stream_fn_proxy(...)`).
    /// When set, the agent uses this in place of `client.stream_simple`.
    pub stream_fn: Option<crate::types::StreamFn>,
}

impl Agent {
    /// Create a new agent with the given client, model, and default options.
    pub fn new(client: model::Client, model: model::Model) -> Self {
        Self::with_options(client, model, AgentOptions::default())
    }

    /// Create a new agent with full options.
    pub fn with_options(client: model::Client, model: model::Model, opts: AgentOptions) -> Self {
        let steering_mode = opts.steering_mode.unwrap_or_default();
        let follow_up_mode = opts.follow_up_mode.unwrap_or_default();
        Self {
            client,
            model,
            system_prompt: opts.system_prompt.unwrap_or_default(),
            messages: opts.initial_messages.unwrap_or_default(),
            tools: opts.tools.unwrap_or_default(),
            stream_options: opts.stream_options.unwrap_or_default(),
            thinking_level: opts.thinking_level,
            tool_execution: opts.tool_execution.unwrap_or_default(),
            before_tool_call: opts.before_tool_call,
            after_tool_call: opts.after_tool_call,
            should_stop_after_turn: opts.should_stop_after_turn,
            convert_to_llm: opts.convert_to_llm,
            transform_context: opts.transform_context,
            get_api_key: opts.get_api_key,
            steering_queue: Arc::new(Mutex::new(PendingQueue::new(steering_mode))),
            follow_up_queue: Arc::new(Mutex::new(PendingQueue::new(follow_up_mode))),
            runtime: Arc::new(RwLock::new(RuntimeState::default())),
            listeners: Arc::new(Mutex::new(ListenerRegistry::default())),
            cancel: Arc::new(Mutex::new(CancellationToken::new())),
            max_retry_delay_ms: opts.max_retry_delay_ms,
            stream_fn: opts.stream_fn,
        }
    }

    // -- State accessors --

    /// Snapshot the current state.
    pub fn state(&self) -> AgentState {
        let rt = self.runtime.read().unwrap();
        AgentState {
            system_prompt: self.system_prompt.clone(),
            model_id: self.model.id.clone(),
            messages: self.messages.clone(),
            thinking_level: self.thinking_level,
            is_streaming: rt.is_streaming,
            error: rt.error.clone(),
            streaming_message: rt.streaming_message.clone(),
            pending_tool_calls: rt.pending_tool_calls.clone(),
        }
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn model(&self) -> &model::Model {
        &self.model
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    pub fn tools(&self) -> &[AgentTool] {
        &self.tools
    }

    /// Whether the agent currently holds an active run.
    pub fn is_streaming(&self) -> bool {
        self.runtime.read().unwrap().is_streaming
    }

    // -- Mutators --

    pub fn set_system_prompt(&mut self, prompt: impl Into<String>) {
        self.system_prompt = prompt.into();
    }

    pub fn set_model(&mut self, model: model::Model) {
        self.model = model;
    }

    pub fn set_stream_options(&mut self, options: SimpleStreamOptions) {
        self.stream_options = options;
    }

    pub fn set_tool_execution_mode(&mut self, mode: ToolExecutionMode) {
        self.tool_execution = mode;
    }

    pub fn add_tool(&mut self, tool: AgentTool) {
        self.tools.push(tool);
    }

    pub fn set_tools(&mut self, tools: Vec<AgentTool>) {
        self.tools = tools;
    }

    pub fn replace_messages(&mut self, messages: Vec<Message>) {
        self.messages = messages;
    }

    pub fn clear_messages(&mut self) {
        self.messages.clear();
    }

    pub fn set_thinking_level(&mut self, level: Option<ThinkingLevel>) {
        self.thinking_level = level;
        self.stream_options.reasoning = level;
    }

    pub fn thinking_level(&self) -> Option<ThinkingLevel> {
        self.thinking_level
    }

    pub fn set_before_tool_call(&mut self, hook: Option<BeforeToolCallHook>) {
        self.before_tool_call = hook;
    }

    pub fn set_after_tool_call(&mut self, hook: Option<AfterToolCallHook>) {
        self.after_tool_call = hook;
    }

    pub fn set_should_stop_after_turn(&mut self, hook: Option<ShouldStopAfterTurnFn>) {
        self.should_stop_after_turn = hook;
    }

    pub fn set_convert_to_llm(&mut self, hook: Option<ConvertToLlmFn>) {
        self.convert_to_llm = hook;
    }

    pub fn set_transform_context(&mut self, hook: Option<TransformContextFn>) {
        self.transform_context = hook;
    }

    pub fn set_get_api_key(&mut self, hook: Option<GetApiKeyFn>) {
        self.get_api_key = hook;
    }

    pub fn set_steering_mode(&mut self, mode: QueueDeliveryMode) {
        self.steering_queue.lock().unwrap().mode = mode;
    }

    pub fn set_follow_up_mode(&mut self, mode: QueueDeliveryMode) {
        self.follow_up_queue.lock().unwrap().mode = mode;
    }

    pub fn set_max_retry_delay_ms(&mut self, ms: Option<u64>) {
        self.max_retry_delay_ms = ms;
    }

    // -- Subscribe / cancel --

    /// Subscribe a listener. Drop the returned handle to unsubscribe.
    pub fn subscribe<F>(&self, listener: F) -> SubscriptionHandle
    where
        F: Fn(&AgentEvent, &CancellationToken) + Send + Sync + 'static,
    {
        let listener: Listener = Arc::new(listener);
        let id = self.listeners.lock().unwrap().add(listener);
        SubscriptionHandle {
            id,
            registry: self.listeners.clone(),
        }
    }

    /// Cancel the in-flight run, if any.
    ///
    /// # Semantics
    ///
    /// `abort` only cancels a run that is *currently executing*. Calling
    /// `abort` between runs (when no `prompt`/`continue` is in flight) is
    /// silently lost: the next run's [`start_run`] installs a fresh
    /// cancellation token before any cancellable work begins.
    ///
    /// Cancellation surfaces in two places:
    /// - `prompt()` and `continue()` return `Ok(_)` with the final assistant
    ///   message's [`crate::types::StopReason`] set to `Aborted`. They do
    ///   **not** return `Err(AgentError::Aborted)` — `Err` is reserved for
    ///   transport / lifecycle errors. Callers that need to distinguish a
    ///   normal completion from an aborted one should inspect
    ///   `result.stop_reason`.
    /// - In-flight tool futures racing on the same cancellation token are
    ///   dropped via `tokio::select!`.
    pub fn abort(&self) {
        self.cancel.lock().unwrap().cancel();
    }

    /// Clone the cancellation token currently associated with the agent.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.lock().unwrap().clone()
    }

    /// Return a cheap clonable handle that can cancel the agent's in-flight
    /// run from any task or thread without borrowing the agent.
    pub fn abort_handle(&self) -> AbortHandle {
        AbortHandle {
            cancel: self.cancel.clone(),
        }
    }

    // -- Queues --

    pub fn steer(&self, message: Message) {
        self.steering_queue.lock().unwrap().enqueue(message);
    }

    pub fn follow_up(&self, message: Message) {
        self.follow_up_queue.lock().unwrap().enqueue(message);
    }

    pub fn clear_steering_queue(&self) {
        self.steering_queue.lock().unwrap().clear();
    }

    pub fn clear_follow_up_queue(&self) {
        self.follow_up_queue.lock().unwrap().clear();
    }

    pub fn clear_all_queues(&self) {
        self.clear_steering_queue();
        self.clear_follow_up_queue();
    }

    pub fn has_queued_messages(&self) -> bool {
        self.steering_queue.lock().unwrap().has_items()
            || self.follow_up_queue.lock().unwrap().has_items()
    }

    /// Reset transcript and runtime state.
    pub fn reset(&mut self) {
        self.messages.clear();
        let mut rt = self.runtime.write().unwrap();
        *rt = RuntimeState::default();
        self.clear_all_queues();
    }

    // -- Execution --

    /// Run the agent on a new prompt input (text, message, or message batch).
    pub async fn prompt<P: IntoPromptInput>(
        &mut self,
        input: P,
    ) -> Result<AgentLoopResult, AgentError> {
        if self.is_streaming() {
            return Err(AgentError::InvalidState(
                "agent is already processing a prompt".into(),
            ));
        }
        let messages = input.into_prompt();
        self.run_prompt_messages(messages, false).await
    }

    /// Convenience: prompt with text and image attachments.
    pub async fn prompt_with_images(
        &mut self,
        text: impl Into<String>,
        images: Vec<ImageContent>,
    ) -> Result<AgentLoopResult, AgentError> {
        let mut blocks: Vec<UserContentBlock> = Vec::with_capacity(1 + images.len());
        blocks.push(UserContentBlock::Text(TextContent::new(text)));
        for img in images {
            blocks.push(UserContentBlock::Image(img));
        }
        let mut user = UserMessage::new_blocks(blocks);
        user.timestamp = now_ms();
        self.prompt(Message::User(user)).await
    }

    /// Continue from the current transcript.
    ///
    /// If the last message is `assistant`, this drains queued steering messages
    /// first, then follow-up; if both are empty, returns an error.
    pub async fn r#continue(&mut self) -> Result<AgentLoopResult, AgentError> {
        if self.is_streaming() {
            return Err(AgentError::InvalidState(
                "agent is already processing".into(),
            ));
        }

        match self.messages.last() {
            None => Err(AgentError::InvalidState(
                "no messages to continue from".into(),
            )),
            Some(Message::Assistant(_)) => {
                let queued_steering = self.steering_queue.lock().unwrap().drain();
                if !queued_steering.is_empty() {
                    return self.run_prompt_messages(queued_steering, true).await;
                }
                let queued_followup = self.follow_up_queue.lock().unwrap().drain();
                if !queued_followup.is_empty() {
                    return self.run_prompt_messages(queued_followup, false).await;
                }
                Err(AgentError::InvalidState(
                    "cannot continue from message role: assistant".into(),
                ))
            }
            Some(Message::User(_)) | Some(Message::ToolResult(_)) => self.run_continuation().await,
        }
    }

    async fn run_prompt_messages(
        &mut self,
        prompts: Vec<Message>,
        skip_initial_steering_poll: bool,
    ) -> Result<AgentLoopResult, AgentError> {
        let cancel = self.start_run();
        let emit = self.build_event_sink(cancel.clone());

        let mut context = AgentContext {
            system_prompt: self.system_prompt.clone(),
            messages: self.messages.clone(),
        };

        let config = self.build_config();
        let outcome = run_agent_loop_with_messages(
            prompts,
            &mut context,
            &self.tools,
            &config,
            &self.client,
            &emit,
            &cancel,
            skip_initial_steering_poll,
        )
        .await;

        self.messages = context.messages;
        self.finish_run_outcome(outcome, &emit, cancel)
    }

    async fn run_continuation(&mut self) -> Result<AgentLoopResult, AgentError> {
        let cancel = self.start_run();
        let emit = self.build_event_sink(cancel.clone());

        let mut context = AgentContext {
            system_prompt: self.system_prompt.clone(),
            messages: self.messages.clone(),
        };

        let config = self.build_config();
        let outcome = run_agent_loop_continue(
            &mut context,
            &self.tools,
            &config,
            &self.client,
            &emit,
            &cancel,
        )
        .await;

        self.messages = context.messages;
        self.finish_run_outcome(outcome, &emit, cancel)
    }

    fn start_run(&mut self) -> CancellationToken {
        // Fresh token per run.
        let new_token = CancellationToken::new();
        *self.cancel.lock().unwrap() = new_token.clone();
        let mut rt = self.runtime.write().unwrap();
        rt.is_streaming = true;
        rt.streaming_message = None;
        rt.error = None;
        rt.pending_tool_calls.clear();
        new_token
    }

    fn finish_run_outcome(
        &mut self,
        outcome: Result<AgentLoopResult, AgentError>,
        emit: &AgentEventSink,
        cancel: CancellationToken,
    ) -> Result<AgentLoopResult, AgentError> {
        match outcome {
            Ok(r) => {
                let mut rt = self.runtime.write().unwrap();
                rt.is_streaming = false;
                rt.streaming_message = None;
                rt.pending_tool_calls.clear();
                Ok(r)
            }
            Err(e) => {
                // Lifecycle error: synthesize a failed assistant message and emit agent_end
                // so listeners always see a terminal event.
                let aborted = cancel.is_cancelled();
                let stop_reason = if aborted {
                    StopReason::Aborted
                } else {
                    StopReason::Error
                };
                let failure = AssistantMessage {
                    role: "assistant".into(),
                    content: vec![],
                    api: self.model.api,
                    provider: self.model.provider,
                    model: self.model.id.clone(),
                    response_model: None,
                    response_id: None,
                    diagnostics: None,
                    usage: Usage::default(),
                    stop_reason,
                    error_message: Some(e.to_string()),
                    timestamp: now_ms(),
                };
                let failure_msg = Message::Assistant(failure.clone());
                self.messages.push(failure_msg.clone());

                {
                    let mut rt = self.runtime.write().unwrap();
                    rt.is_streaming = false;
                    rt.streaming_message = None;
                    rt.pending_tool_calls.clear();
                    rt.error = failure.error_message.clone();
                }

                emit(AgentEvent::AgentEnd {
                    messages: vec![failure_msg],
                });

                Err(e)
            }
        }
    }

    fn build_config(&self) -> AgentLoopConfig {
        let steering_queue = self.steering_queue.clone();
        let follow_up_queue = self.follow_up_queue.clone();

        let get_steering: GetSteeringMessagesFn = Arc::new(move || {
            let q = steering_queue.clone();
            Box::pin(async move { q.lock().unwrap().drain() })
        });
        let get_follow_up: GetFollowUpMessagesFn = Arc::new(move || {
            let q = follow_up_queue.clone();
            Box::pin(async move { q.lock().unwrap().drain() })
        });

        let mut stream_options = self.stream_options.clone();
        if stream_options.reasoning.is_none() {
            stream_options.reasoning = self.thinking_level;
        }
        if stream_options.base.max_retry_delay_ms.is_none() {
            stream_options.base.max_retry_delay_ms = self.max_retry_delay_ms;
        }

        AgentLoopConfig {
            model: self.model.clone(),
            stream_options,
            tool_execution: self.tool_execution,
            before_tool_call: self.before_tool_call.clone(),
            after_tool_call: self.after_tool_call.clone(),
            should_stop_after_turn: self.should_stop_after_turn.clone(),
            get_steering_messages: Some(get_steering),
            get_follow_up_messages: Some(get_follow_up),
            convert_to_llm: self.convert_to_llm.clone(),
            transform_context: self.transform_context.clone(),
            get_api_key: self.get_api_key.clone(),
            steering_mode: self.steering_queue.lock().unwrap().mode,
            follow_up_mode: self.follow_up_queue.lock().unwrap().mode,
            max_retry_delay_ms: self.max_retry_delay_ms,
            stream_fn: self.stream_fn.clone(),
        }
    }

    fn build_event_sink(&self, cancel: CancellationToken) -> AgentEventSink {
        let runtime = self.runtime.clone();
        let listeners = self.listeners.clone();
        Arc::new(move |event: AgentEvent| {
            // Reduce internal state.
            {
                let mut rt = runtime.write().unwrap();
                match &event {
                    AgentEvent::AgentStart => {
                        rt.is_streaming = true;
                    }
                    AgentEvent::MessageStart { message } => {
                        if matches!(message, Message::Assistant(_)) {
                            rt.streaming_message = Some(message.clone());
                        }
                    }
                    AgentEvent::MessageUpdate { message, .. } => {
                        rt.streaming_message = Some(message.clone());
                    }
                    AgentEvent::MessageEnd { message } => {
                        if matches!(message, Message::Assistant(_)) {
                            rt.streaming_message = None;
                        }
                    }
                    AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                        rt.pending_tool_calls.insert(tool_call_id.clone());
                    }
                    AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                        rt.pending_tool_calls.remove(tool_call_id);
                    }
                    AgentEvent::TurnEnd { message, .. } => {
                        if let Message::Assistant(a) = message
                            && let Some(err) = &a.error_message
                        {
                            rt.error = Some(err.clone());
                        }
                    }
                    AgentEvent::AgentEnd { .. } => {
                        rt.streaming_message = None;
                        rt.is_streaming = false;
                    }
                    _ => {}
                }
            }

            // Forward to listeners (snapshot to avoid holding the lock during dispatch).
            let snapshot = listeners.lock().unwrap().snapshot();
            for listener in snapshot {
                listener(&event, &cancel);
            }
        })
    }
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("model", &self.model.id)
            .field("tools", &self.tools.len())
            .field("messages", &self.messages.len())
            .field("is_streaming", &self.is_streaming())
            .finish()
    }
}

/// A cheap clone-able handle that can cancel the agent's in-flight run
/// from any task or thread.
///
/// The handle holds an `Arc` to the agent's shared cancellation cell. Each
/// [`Agent::start_run`] replaces the cell's contents with a fresh
/// [`CancellationToken`], so a handle created before run N still cancels
/// run N+1 if that run is what's in flight when [`Self::abort`] is called.
///
/// # Threading model
///
/// `Agent` itself is held by `&mut self` in [`Agent::prompt`] /
/// [`Agent::r#continue`], so concurrent prompts on the same agent require
/// external synchronization. `AbortHandle`, [`Agent::steer`],
/// [`Agent::follow_up`], and [`Agent::subscribe`] all take `&self` and are
/// safe to call from any task or thread while a prompt is running.
#[derive(Clone)]
pub struct AbortHandle {
    cancel: Arc<Mutex<CancellationToken>>,
}

impl AbortHandle {
    /// Cancel the in-flight run, if any. See [`Agent::abort`] for the full
    /// semantics — the same caveats apply: between runs, `abort` is silently
    /// lost because [`Agent::start_run`] installs a fresh token.
    pub fn abort(&self) {
        self.cancel.lock().unwrap().cancel();
    }
}
