//! Agent session — lifecycle management for the coding agent.
//!
//! Ties together the agent loop, session persistence, settings, compaction,
//! and system prompt generation into a high-level session object.

use crate::core::compaction;
use crate::core::error::CodingAgentError;
use crate::core::session_manager::SessionManager;
use crate::core::settings::SettingsManager;
use crate::core::system_prompt::{self, BuildSystemPromptOptions};
use hand_agent::types::{AgentContext, AgentEvent, AgentLoopConfig, AgentTool};
use hand_agent::{AgentEventSink, CancellationToken, agent_loop};
use model::{Message, SimpleStreamOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

type EventListener = Arc<dyn Fn(AgentSessionEvent) + Send + Sync>;
type EventListeners = Arc<Mutex<Vec<EventListener>>>;

/// Events emitted by the agent session.
#[derive(Debug, Clone)]
pub enum AgentSessionEvent {
    /// Forwarded agent event.
    Agent(Box<AgentEvent>),
    /// Compaction started.
    CompactionStart,
    /// Compaction completed.
    CompactionEnd { summary: String },
    /// Session error.
    Error(String),
}

/// Configuration for creating an agent session.
pub struct AgentSessionConfig {
    /// Working directory.
    pub cwd: PathBuf,
    /// Model to use.
    pub model: model::Model,
    /// Stream options.
    pub stream_options: SimpleStreamOptions,
    /// Custom system prompt (overrides generated one).
    pub custom_system_prompt: Option<String>,
    /// Custom guidelines to append.
    pub custom_guidelines: Option<String>,
    /// Whether to resume an existing session.
    pub resume_session: Option<String>,
}

/// The main agent session coordinating all subsystems.
pub struct AgentSession {
    config: AgentSessionConfig,
    session_manager: SessionManager,
    settings_manager: SettingsManager,
    context: AgentContext,
    tools: Vec<AgentTool>,
    client: model::Client,
    event_listeners: EventListeners,
}

impl AgentSession {
    /// Create a new agent session.
    pub fn new(
        config: AgentSessionConfig,
        tools: Vec<AgentTool>,
    ) -> Result<Self, CodingAgentError> {
        let settings_manager = SettingsManager::new(&config.cwd);
        let client = model::Client::new();

        // Create or resume session
        let session_manager = if let Some(session_id) = &config.resume_session {
            let session_dir = config.cwd.join(".hand").join("sessions");
            let path = session_dir.join(format!("{}.jsonl", session_id));
            SessionManager::open(&path)?
        } else {
            SessionManager::create(&config.cwd)?
        };

        // Build tool names for system prompt
        let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();

        // Load context files
        let context_files = system_prompt::load_context_files(&config.cwd);

        // Build system prompt
        let system_prompt = system_prompt::build_system_prompt(BuildSystemPromptOptions {
            cwd: &config.cwd,
            tools: &tool_names,
            custom_guidelines: config.custom_guidelines.as_deref(),
            context_files,
            custom_prompt: config.custom_system_prompt.as_deref(),
        });

        // Restore messages from session
        let messages = session_manager.build_context();

        let context = AgentContext {
            system_prompt,
            messages,
        };

        Ok(Self {
            config,
            session_manager,
            settings_manager,
            context,
            tools,
            client,
            event_listeners: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Create an in-memory session (for testing).
    pub fn in_memory(model: model::Model, tools: Vec<AgentTool>) -> Self {
        let context = AgentContext {
            system_prompt: "You are a helpful coding assistant.".into(),
            messages: vec![],
        };

        Self {
            config: AgentSessionConfig {
                cwd: PathBuf::from("."),
                model,
                stream_options: SimpleStreamOptions::default(),
                custom_system_prompt: None,
                custom_guidelines: None,
                resume_session: None,
            },
            session_manager: SessionManager::in_memory(),
            settings_manager: SettingsManager::in_memory(),
            context,
            tools,
            client: model::Client::new(),
            event_listeners: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Subscribe to session events.
    pub fn subscribe(&mut self, listener: impl Fn(AgentSessionEvent) + Send + Sync + 'static) {
        self.event_listeners
            .lock()
            .unwrap()
            .push(Arc::new(listener));
    }

    /// Send a user message and run the agent loop.
    pub async fn send_message(&mut self, text: &str) -> Result<Vec<Message>, CodingAgentError> {
        let user_msg = Message::User(model::UserMessage::new_text(text));

        // Persist the user message
        self.session_manager.append_message(user_msg.clone())?;

        let prompts = vec![user_msg];

        // Build agent loop config
        let loop_config = AgentLoopConfig::new(
            self.config.model.clone(),
            self.config.stream_options.clone(),
        );

        // Create event sink for the agent loop
        let emit = self.build_event_sink();
        let cancel = CancellationToken::new();

        let result = agent_loop::run_agent_loop(
            prompts,
            &mut self.context,
            &self.tools,
            &loop_config,
            &self.client,
            &emit,
            &cancel,
        )
        .await
        .map_err(CodingAgentError::Agent)?;

        // Persist new messages to session
        for msg in &result.messages {
            let _ = self.session_manager.append_message(msg.clone());
        }

        // Check for compaction
        self.maybe_compact().await?;

        Ok(result.messages)
    }

    /// Check if compaction is needed and run it.
    async fn maybe_compact(&mut self) -> Result<(), CodingAgentError> {
        let settings = self.settings_manager.compaction_settings();
        let context_tokens = compaction::estimate_context_tokens(&self.context.messages);

        // Use a reasonable default for max context tokens
        let max_context_tokens = 200_000;

        if !compaction::should_compact(context_tokens, max_context_tokens, &settings) {
            return Ok(());
        }

        self.emit(AgentSessionEvent::CompactionStart);

        let (to_compact, _to_keep, _split_idx) = compaction::split_for_compaction(
            &self.context.messages,
            settings.keep_recent_tokens as usize,
        );

        let file_ops = compaction::extract_file_operations(&to_compact);
        let summary_prompt = compaction::build_compaction_prompt(&to_compact, &file_ops);

        // Call LLM to generate a compaction summary
        let summary = match self.generate_compaction_summary(&summary_prompt).await {
            Ok(s) => s,
            Err(_) => {
                // Fallback to a structured summary if LLM call fails
                format!(
                    "[Compacted {} messages. Files read: {}. Files edited: {}.]",
                    to_compact.len(),
                    file_ops.read.join(", "),
                    file_ops.edited.join(", "),
                )
            }
        };

        // Record compaction in session
        let first_kept_id = format!("compaction_{}", chrono::Utc::now().timestamp_millis());
        self.session_manager
            .append_compaction(&summary, &first_kept_id)?;

        self.emit(AgentSessionEvent::CompactionEnd {
            summary: summary.clone(),
        });

        Ok(())
    }

    /// Get the session ID.
    pub fn session_id(&self) -> &str {
        self.session_manager.id()
    }

    /// Get the current model.
    pub fn model(&self) -> &model::Model {
        &self.config.model
    }

    /// Set the model.
    pub fn set_model(&mut self, model: model::Model) {
        self.config.model = model;
    }

    /// Get the working directory.
    pub fn cwd(&self) -> &Path {
        &self.config.cwd
    }

    /// Get the current context messages.
    pub fn messages(&self) -> &[Message] {
        &self.context.messages
    }

    /// Get the settings manager.
    pub fn settings(&self) -> &SettingsManager {
        &self.settings_manager
    }

    /// Manually trigger compaction. Returns true if compaction was performed.
    pub async fn compact(&mut self) -> Result<bool, CodingAgentError> {
        let settings = self.settings_manager.compaction_settings();
        let context_tokens = compaction::estimate_context_tokens(&self.context.messages);
        let max_context_tokens = 200_000;

        if !compaction::should_compact(context_tokens, max_context_tokens, &settings) {
            return Ok(false);
        }

        self.maybe_compact().await?;
        Ok(true)
    }

    /// Get the stream options.
    pub fn stream_options(&self) -> &SimpleStreamOptions {
        &self.config.stream_options
    }

    /// Update the stream options (e.g. after changing thinking level).
    pub fn set_stream_options(&mut self, options: SimpleStreamOptions) {
        self.config.stream_options = options;
    }

    /// Get the session label (name).
    pub fn label(&self) -> Option<&str> {
        self.session_manager.label()
    }

    /// Set the session label (name).
    pub fn set_label(&mut self, label: &str) -> Result<(), CodingAgentError> {
        self.session_manager.append_label(label)
    }

    /// Get message count.
    pub fn message_count(&self) -> usize {
        self.session_manager.message_count()
    }

    /// Generate a compaction summary using the LLM.
    async fn generate_compaction_summary(&self, prompt: &str) -> Result<String, CodingAgentError> {
        use futures::StreamExt;

        let context = model::Context {
            system_prompt: Some(
                "You are a conversation summarizer. Produce a concise summary of the conversation \
                 that preserves all important context, decisions, and file operations."
                    .to_string(),
            ),
            messages: vec![Message::User(model::UserMessage::new_text(prompt))],
            tools: None,
        };

        let mut stream = self
            .client
            .stream_simple(&self.config.model, context, None)
            .map_err(|e| CodingAgentError::Other(format!("Compaction LLM error: {e}")))?;

        let mut summary = String::new();
        while let Some(event) = stream.next().await {
            if let model::AssistantMessageEvent::TextDelta { delta, .. } = event {
                summary.push_str(&delta);
            }
        }

        if summary.is_empty() {
            return Err(CodingAgentError::Other(
                "Empty compaction summary from LLM".into(),
            ));
        }

        Ok(summary)
    }

    fn emit(&self, event: AgentSessionEvent) {
        Self::emit_to_listeners(&self.event_listeners, event);
    }

    fn build_event_sink(&self) -> AgentEventSink {
        let listeners = Arc::clone(&self.event_listeners);
        Arc::new(move |event: AgentEvent| {
            Self::emit_to_listeners(&listeners, AgentSessionEvent::Agent(Box::new(event)));
        })
    }

    fn emit_to_listeners(listeners: &EventListeners, event: AgentSessionEvent) {
        let listeners = listeners.lock().unwrap().clone();
        for listener in listeners {
            listener(event.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn test_model() -> model::Model {
        model::Model {
            id: "test-model".into(),
            name: "Test".into(),
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

    #[test]
    fn test_in_memory_session() {
        let session = AgentSession::in_memory(test_model(), vec![]);
        assert_eq!(session.message_count(), 0);
        assert_eq!(session.model().id, "test-model");
    }

    #[test]
    fn test_session_id() {
        let session = AgentSession::in_memory(test_model(), vec![]);
        assert!(session.session_id().starts_with("s_"));
    }

    #[test]
    fn test_set_model() {
        let mut session = AgentSession::in_memory(test_model(), vec![]);
        let mut new_model = test_model();
        new_model.id = "new-model".into();
        session.set_model(new_model);
        assert_eq!(session.model().id, "new-model");
    }

    #[test]
    fn test_event_sink_forwards_agent_events_to_subscribers() {
        let mut session = AgentSession::in_memory(test_model(), vec![]);
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured_events = Arc::clone(&events);

        session.subscribe(move |event| {
            captured_events.lock().unwrap().push(event);
        });

        let emit = session.build_event_sink();
        emit(AgentEvent::AgentStart);

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgentSessionEvent::Agent(e) if matches!(**e, AgentEvent::AgentStart) => {}
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
