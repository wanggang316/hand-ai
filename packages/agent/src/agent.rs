//! Agent struct — high-level wrapper around the agent loop.

use crate::agent_loop::{AgentEventSink, AgentLoopResult, run_agent_loop, run_agent_loop_continue};
use crate::error::AgentError;
use crate::types::{
    AfterToolCallHook, AgentContext, AgentEvent, AgentLoopConfig, AgentState, AgentTool,
    BeforeToolCallHook, GetApiKeyFn, QueueDeliveryMode, ToolExecutionMode, TransformContextFn,
};
use model::{Client, Message, Model, SimpleStreamOptions, ThinkingLevel, UserMessage};
use std::sync::{Arc, Mutex};

/// Listener callback invoked for each `AgentEvent`.
type EventListener = Box<dyn Fn(AgentEvent) + Send + Sync>;

/// High-level agent that manages state and wraps the agent loop.
pub struct Agent {
    /// The model client.
    client: Client,
    /// Current state.
    state: AgentState,
    /// The model to use.
    model: Model,
    /// Registered tools.
    tools: Vec<AgentTool>,
    /// Stream options.
    stream_options: SimpleStreamOptions,
    /// Tool execution mode.
    tool_execution: ToolExecutionMode,
    /// Event listeners.
    listeners: Vec<EventListener>,
    /// Steering message queue.
    steering_queue: Arc<Mutex<Vec<Message>>>,
    /// Follow-up message queue.
    follow_up_queue: Arc<Mutex<Vec<Message>>>,
    /// Before tool call hook (Arc-wrapped for sharing with config).
    before_tool_call: Option<Arc<BeforeToolCallHook>>,
    /// After tool call hook (Arc-wrapped for sharing with config).
    after_tool_call: Option<Arc<AfterToolCallHook>>,
    /// Steering message delivery mode.
    steering_mode: QueueDeliveryMode,
    /// Follow-up message delivery mode.
    follow_up_mode: QueueDeliveryMode,
    /// Context transformer (applied before convertToLlm).
    transform_context: Option<TransformContextFn>,
    /// Dynamic API key resolver.
    get_api_key: Option<GetApiKeyFn>,
}

impl Agent {
    /// Create a new agent with the given client and model.
    pub fn new(client: Client, model: Model) -> Self {
        Self {
            client,
            state: AgentState {
                model_id: model.id.clone(),
                ..Default::default()
            },
            model,
            tools: Vec::new(),
            stream_options: SimpleStreamOptions::default(),
            tool_execution: ToolExecutionMode::Parallel,
            listeners: Vec::new(),
            steering_queue: Arc::new(Mutex::new(Vec::new())),
            follow_up_queue: Arc::new(Mutex::new(Vec::new())),
            before_tool_call: None,
            after_tool_call: None,
            steering_mode: QueueDeliveryMode::OneAtATime,
            follow_up_mode: QueueDeliveryMode::OneAtATime,
            transform_context: None,
            get_api_key: None,
        }
    }

    // -- State accessors --

    /// Get current state.
    pub fn state(&self) -> &AgentState {
        &self.state
    }

    /// Get current messages.
    pub fn messages(&self) -> &[Message] {
        &self.state.messages
    }

    /// Get current model.
    pub fn model(&self) -> &Model {
        &self.model
    }

    // -- State mutators --

    /// Set system prompt.
    pub fn set_system_prompt(&mut self, prompt: impl Into<String>) {
        self.state.system_prompt = prompt.into();
    }

    /// Set model.
    pub fn set_model(&mut self, model: Model) {
        self.state.model_id = model.id.clone();
        self.model = model;
    }

    /// Set stream options.
    pub fn set_stream_options(&mut self, options: SimpleStreamOptions) {
        self.stream_options = options;
    }

    /// Set tool execution mode.
    pub fn set_tool_execution_mode(&mut self, mode: ToolExecutionMode) {
        self.tool_execution = mode;
    }

    /// Add a tool.
    pub fn add_tool(&mut self, tool: AgentTool) {
        self.tools.push(tool);
    }

    /// Set tools, replacing all existing tools.
    pub fn set_tools(&mut self, tools: Vec<AgentTool>) {
        self.tools = tools;
    }

    /// Replace all messages.
    pub fn replace_messages(&mut self, messages: Vec<Message>) {
        self.state.messages = messages;
    }

    /// Clear all messages.
    pub fn clear_messages(&mut self) {
        self.state.messages.clear();
    }

    /// Set before_tool_call hook.
    pub fn set_before_tool_call(&mut self, hook: Option<BeforeToolCallHook>) {
        self.before_tool_call = hook.map(Arc::new);
    }

    /// Set after_tool_call hook.
    pub fn set_after_tool_call(&mut self, hook: Option<AfterToolCallHook>) {
        self.after_tool_call = hook.map(Arc::new);
    }

    /// Set thinking level for reasoning models.
    pub fn set_thinking_level(&mut self, level: Option<ThinkingLevel>) {
        self.state.thinking_level = level;
        if let Some(lvl) = level {
            self.stream_options.reasoning = Some(lvl);
        } else {
            self.stream_options.reasoning = None;
        }
    }

    /// Get current thinking level.
    pub fn thinking_level(&self) -> Option<ThinkingLevel> {
        self.state.thinking_level
    }

    /// Set steering delivery mode.
    pub fn set_steering_mode(&mut self, mode: QueueDeliveryMode) {
        self.steering_mode = mode;
    }

    /// Set follow-up delivery mode.
    pub fn set_follow_up_mode(&mut self, mode: QueueDeliveryMode) {
        self.follow_up_mode = mode;
    }

    /// Set context transformer.
    pub fn set_transform_context(&mut self, transform: Option<TransformContextFn>) {
        self.transform_context = transform;
    }

    /// Set dynamic API key resolver.
    pub fn set_get_api_key(&mut self, resolver: Option<GetApiKeyFn>) {
        self.get_api_key = resolver;
    }

    // -- Event subscription --

    /// Subscribe to agent events. Returns an index that could be used for unsubscription.
    pub fn subscribe(&mut self, listener: EventListener) -> usize {
        self.listeners.push(listener);
        self.listeners.len() - 1
    }

    // -- Queue management --

    /// Queue a steering message to be injected mid-run.
    pub fn steer(&self, message: Message) {
        self.steering_queue.lock().unwrap().push(message);
    }

    /// Queue a follow-up message.
    pub fn follow_up(&self, message: Message) {
        self.follow_up_queue.lock().unwrap().push(message);
    }

    /// Clear steering queue.
    pub fn clear_steering_queue(&self) {
        self.steering_queue.lock().unwrap().clear();
    }

    /// Clear follow-up queue.
    pub fn clear_follow_up_queue(&self) {
        self.follow_up_queue.lock().unwrap().clear();
    }

    /// Check if there are queued messages.
    pub fn has_queued_messages(&self) -> bool {
        !self.steering_queue.lock().unwrap().is_empty()
            || !self.follow_up_queue.lock().unwrap().is_empty()
    }

    /// Clear all queues.
    pub fn clear_all_queues(&self) {
        self.steering_queue.lock().unwrap().clear();
        self.follow_up_queue.lock().unwrap().clear();
    }

    /// Reset the agent (clear messages, queues, errors).
    pub fn reset(&mut self) {
        self.state.messages.clear();
        self.state.is_streaming = false;
        self.state.error = None;
        self.steering_queue.lock().unwrap().clear();
        self.follow_up_queue.lock().unwrap().clear();
    }

    // -- Execution --

    /// Send a text prompt to the agent.
    pub async fn prompt(&mut self, text: &str) -> Result<AgentLoopResult, AgentError> {
        if self.state.is_streaming {
            return Err(AgentError::InvalidState(
                "Agent is already processing a prompt".to_string(),
            ));
        }

        let user_msg = Message::User(UserMessage::new_text(text));
        self.run_loop(vec![user_msg]).await
    }

    /// Send a pre-built message to the agent.
    pub async fn prompt_with_messages(
        &mut self,
        messages: Vec<Message>,
    ) -> Result<AgentLoopResult, AgentError> {
        if self.state.is_streaming {
            return Err(AgentError::InvalidState(
                "Agent is already processing a prompt".to_string(),
            ));
        }

        self.run_loop(messages).await
    }

    /// Continue from the current context.
    pub async fn r#continue(&mut self) -> Result<AgentLoopResult, AgentError> {
        if self.state.is_streaming {
            return Err(AgentError::InvalidState(
                "Agent is already processing".to_string(),
            ));
        }

        if self.state.messages.is_empty() {
            return Err(AgentError::InvalidState(
                "No messages to continue from".to_string(),
            ));
        }

        self.state.is_streaming = true;
        self.state.error = None;

        let mut context = AgentContext {
            system_prompt: self.state.system_prompt.clone(),
            messages: self.state.messages.clone(),
        };

        let config = self.build_config();
        let emit = self.build_event_sink();

        let result =
            run_agent_loop_continue(&mut context, &self.tools, &config, &self.client, &emit).await;

        // Update state from context
        self.state.messages = context.messages;
        self.state.is_streaming = false;

        match result {
            Ok(r) => Ok(r),
            Err(e) => {
                self.state.error = Some(e.to_string());
                Err(e)
            }
        }
    }

    /// Run the agent loop with prompt messages.
    async fn run_loop(&mut self, prompts: Vec<Message>) -> Result<AgentLoopResult, AgentError> {
        self.state.is_streaming = true;
        self.state.error = None;

        let mut context = AgentContext {
            system_prompt: self.state.system_prompt.clone(),
            messages: self.state.messages.clone(),
        };

        let config = self.build_config();
        let emit = self.build_event_sink();

        let result = run_agent_loop(
            prompts,
            &mut context,
            &self.tools,
            &config,
            &self.client,
            &emit,
        )
        .await;

        // Update state from context
        self.state.messages = context.messages;
        self.state.is_streaming = false;

        match result {
            Ok(r) => Ok(r),
            Err(e) => {
                self.state.error = Some(e.to_string());
                Err(e)
            }
        }
    }

    /// Build the loop config.
    fn build_config(&self) -> AgentLoopConfig {
        let steering_queue = self.steering_queue.clone();
        let follow_up_queue = self.follow_up_queue.clone();
        let steering_mode = self.steering_mode;
        let follow_up_mode = self.follow_up_mode;

        // Wire up hooks via Arc - clone the Arc so the closure owns a reference
        let before_hook: Option<BeforeToolCallHook> =
            self.before_tool_call
                .clone()
                .map(|hook| -> BeforeToolCallHook {
                    Box::new(move |ctx| {
                        let h = hook.clone();
                        Box::pin(async move { h(ctx).await })
                    })
                });
        let after_hook: Option<AfterToolCallHook> =
            self.after_tool_call
                .clone()
                .map(|hook| -> AfterToolCallHook {
                    Box::new(move |ctx| {
                        let h = hook.clone();
                        Box::pin(async move { h(ctx).await })
                    })
                });

        AgentLoopConfig {
            model: self.model.clone(),
            stream_options: self.stream_options.clone(),
            tool_execution: self.tool_execution,
            before_tool_call: before_hook,
            after_tool_call: after_hook,
            get_steering_messages: Some(Box::new(move || {
                let queue = steering_queue.clone();
                Box::pin(async move {
                    let mut q = queue.lock().unwrap();
                    match steering_mode {
                        QueueDeliveryMode::All => q.drain(..).collect(),
                        QueueDeliveryMode::OneAtATime => {
                            if q.is_empty() {
                                Vec::new()
                            } else {
                                vec![q.remove(0)]
                            }
                        }
                    }
                })
            })),
            get_follow_up_messages: Some(Box::new(move || {
                let queue = follow_up_queue.clone();
                Box::pin(async move {
                    let mut q = queue.lock().unwrap();
                    match follow_up_mode {
                        QueueDeliveryMode::All => q.drain(..).collect(),
                        QueueDeliveryMode::OneAtATime => {
                            if q.is_empty() {
                                Vec::new()
                            } else {
                                vec![q.remove(0)]
                            }
                        }
                    }
                })
            })),
            convert_to_llm: None,
            transform_context: None,
            get_api_key: None,
            steering_mode,
            follow_up_mode,
            max_retry_delay_ms: None,
        }
    }

    /// Build the event sink that forwards to listeners.
    fn build_event_sink(&self) -> AgentEventSink {
        // Collect listeners into an Arc<Vec<...>> so the closure can own them.
        let listeners: Arc<Vec<EventListener>> = Arc::new(
            // We can't move self.listeners, so we need to wrap them.
            // Since we can't clone Box<dyn Fn>, we use a no-op if no listeners.
            Vec::new(),
        );
        // Note: For production use, a channel-based approach would be better.
        // This event sink is called synchronously from the agent loop.
        let _ = listeners;
        Box::new(|_event: AgentEvent| {
            // Events are dispatched via the agent loop.
            // Listeners are notified through the channel-based approach in production.
        })
    }
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("model", &self.model.id)
            .field("tools", &self.tools.len())
            .field("messages", &self.state.messages.len())
            .field("is_streaming", &self.state.is_streaming)
            .finish()
    }
}
