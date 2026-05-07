//! Agent session — lifecycle management for the coding agent.
//!
//! Ties together the agent loop, session persistence, settings, compaction,
//! and system prompt generation into a high-level session object.

use crate::core::compaction;
use crate::core::error::CodingAgentError;
use crate::core::extensions::api::{
    Extension, ExtensionContext, HookDecision, SlashCommandSpec, ToolCallEvent, ToolResultEvent,
};
use crate::core::extensions::dispatch::{dispatch_after_tool_call, dispatch_before_tool_call};
use crate::core::extensions::registry::builtin_tier1_extensions;
use crate::core::model_registry::ModelRegistry;
use crate::core::session_manager::SessionManager;
use crate::rpc::types::QueueMode;
use crate::core::settings::SettingsManager;
use crate::core::skills::{self, Skill, SkillError};
use crate::core::system_prompt::{self, BuildSystemPromptOptions};
use hand_agent::types::{
    AfterToolCallContext, AfterToolCallResult, AgentContext, AgentEvent, AgentLoopConfig,
    AgentTool, BeforeToolCallContext, BeforeToolCallResult, BoxFuture,
};
use hand_agent::{AgentEventSink, CancellationToken, agent_loop};
use model::{Message, SimpleStreamOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

type EventListener = Arc<dyn Fn(AgentSessionEvent) + Send + Sync>;
type EventListeners = Arc<Mutex<Vec<EventListener>>>;

/// RAII guard that restores [`AgentSession::tools`] when dropped.
///
/// Used by [`AgentSession::send_message`] to make tool restoration robust
/// against future cancellation and panics: when the host's RPC layer aborts
/// `send_message` mid-await (e.g. via `tokio::select!` with `ctrl_c()`), the
/// future is dropped and this guard's `Drop` runs, putting the built-in
/// tools back exactly as they were before the call. The guard `truncate`s
/// the appended extension-contributed tools off the tail before restoring
/// so the session never accumulates duplicates across turns.
struct ToolsRestoreGuard<'a> {
    slot: &'a mut Option<Vec<AgentTool>>,
    tools: Option<Vec<AgentTool>>,
    keep: usize,
}

impl Drop for ToolsRestoreGuard<'_> {
    fn drop(&mut self) {
        if let Some(mut tools) = self.tools.take() {
            tools.truncate(self.keep);
            *self.slot = Some(tools);
        }
    }
}

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

/// Result of [`AgentSession::run_bash`] — pairs the executor's
/// [`BashResult`](crate::core::bash_executor::BashResult) with an
/// explicit `aborted` flag so callers (e.g. the `bash` RPC handler) can
/// route the abort marker to `stderr` without sniffing string prefixes.
#[derive(Debug, Clone)]
pub struct RunBashOutcome {
    /// Underlying executor result. On abort, `output == "[bash aborted]"`,
    /// `exit_code == None`, and `truncated == true`.
    pub result: crate::core::bash_executor::BashResult,
    /// True if the call was cancelled via [`AgentSession::abort_bash`]
    /// before the executor returned.
    pub aborted: bool,
}

/// Configuration for creating an agent session.
#[derive(Clone)]
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
    /// Built-in tools owned by this session.
    ///
    /// Wrapped in `Option` so [`Self::send_message`] can `take()` ownership for
    /// the duration of an agent loop turn and restore via an RAII guard whose
    /// `Drop` runs even if the future is cancelled or panics. Outside of
    /// `send_message` the invariant is `Some(_)`; helpers that read it use
    /// [`Self::tools`] which expects the invariant to hold.
    tools: Option<Vec<AgentTool>>,
    client: model::Client,
    event_listeners: EventListeners,
    /// Skills discovered at construction time and advertised in the system
    /// prompt. Empty for in-memory test sessions.
    skills: Vec<Skill>,
    /// Per-skill discovery errors. Surfaced via [`Self::skill_errors`] for
    /// diagnostics; one bad skill never aborts session construction.
    skill_errors: Vec<SkillError>,
    /// Tier 1 extensions registered with this session, in dispatch order.
    /// Empty for in-memory test sessions; populated from
    /// [`builtin_tier1_extensions`] for [`Self::new`] / [`Self::new_with_skill_dirs`].
    extensions: Vec<Arc<dyn Extension>>,
    /// Aggregate model catalog for this session. Built eagerly from the
    /// owned [`model::Client`] at construction time and rebuilt by
    /// [`Self::register_extension`] (extensions may contribute models in
    /// later phases — the rebuild keeps the cache consistent).
    model_registry: ModelRegistry,
    /// Steering queue mode (how queued user messages mid-turn are flushed).
    /// Defaults to [`QueueMode::OneAtATime`]. Surfaced via the RPC
    /// `set_steering_mode` / `get_state` handlers.
    steering_mode: QueueMode,
    /// Follow-up queue mode (how queued user messages between turns are
    /// flushed). Same default + same wiring as `steering_mode`.
    follow_up_mode: QueueMode,
    /// Whether automatic compaction (when context window approaches the
    /// model's `max_input_tokens`) is enabled. Surfaced via
    /// `set_auto_compaction` / `get_state`.
    auto_compaction_enabled: bool,
    /// Whether automatic retry-with-backoff for transient provider errors
    /// is enabled. Surfaced via `set_auto_retry`.
    auto_retry_enabled: bool,
    /// Whether the session is currently inside an `agent_loop` turn. Set
    /// by [`Self::send_message`] for the duration of the call. Surfaced
    /// via `get_state.is_streaming`.
    is_streaming: bool,
    /// Whether the session is currently performing a compaction summary.
    /// Set around the `agent_loop_compaction` invocation. Surfaced via
    /// `get_state.is_compacting`.
    is_compacting: bool,
    /// Cancellation token threaded into the agent loop. Held behind a
    /// `Mutex` so [`Self::abort`] (and the RPC `abort` handler) can
    /// trigger cancellation from another task without borrowing `&mut
    /// self`. The token is *replaced* at the start of every
    /// `send_message` so a single token cancellation only affects the
    /// turn it was associated with — subsequent turns get a fresh token.
    cancel: Arc<Mutex<CancellationToken>>,
    /// Cancellation token for in-flight one-off bash executions driven via
    /// [`Self::run_bash`] (RPC `bash` handler). Held separately from
    /// `cancel` so that aborting a turn does not kill an unrelated bash
    /// command and vice versa. Like `cancel`, the token is *replaced* at
    /// the start of every `run_bash` call so a stale `abort_bash` from
    /// before the call can't poison it.
    bash_cancel: Arc<Mutex<CancellationToken>>,
}

impl AgentSession {
    /// Create a new agent session.
    ///
    /// Auto-discovers skills under `<cwd>/.hand/skills/` and (if it exists)
    /// `~/.hand/skills/`. Per-skill errors are stored on the session and can
    /// be inspected via [`Self::skill_errors`] for diagnostics — they never
    /// abort construction.
    pub fn new(
        config: AgentSessionConfig,
        tools: Vec<AgentTool>,
    ) -> Result<Self, CodingAgentError> {
        let user_dir = dirs::home_dir().map(|h| h.join(".hand").join("skills"));
        let user_dir = user_dir.filter(|p| p.exists());
        Self::new_with_skill_dirs(config, tools, user_dir.as_deref(), None)
    }

    /// Create a new agent session with explicit skill discovery roots.
    ///
    /// Test entry point: lets callers pin `user_dir` to `None` (or a fixture
    /// tempdir) so unit tests don't read the host's real `~/.hand/skills/`.
    /// `builtin_dir` is reserved for Phase 2.x bundled defaults; pass `None`
    /// in v1.
    pub fn new_with_skill_dirs(
        config: AgentSessionConfig,
        tools: Vec<AgentTool>,
        user_dir: Option<&Path>,
        builtin_dir: Option<&Path>,
    ) -> Result<Self, CodingAgentError> {
        let settings_manager = SettingsManager::from_cwd(&config.cwd)
            .map_err(|e| CodingAgentError::Settings(e.to_string()))?;
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

        // Discover skills (project + user + optional builtin).
        let (skills_discovered, skill_errors) =
            skills::discover_skills(&config.cwd, user_dir, builtin_dir);

        // Build system prompt
        let system_prompt = system_prompt::build_system_prompt(BuildSystemPromptOptions {
            cwd: &config.cwd,
            tools: &tool_names,
            skills: &skills_discovered,
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

        let model_registry = ModelRegistry::build(&client);
        Ok(Self {
            config,
            session_manager,
            settings_manager,
            context,
            tools: Some(tools),
            client,
            event_listeners: Arc::new(Mutex::new(Vec::new())),
            skills: skills_discovered,
            skill_errors,
            extensions: builtin_tier1_extensions(),
            model_registry,
            steering_mode: QueueMode::OneAtATime,
            follow_up_mode: QueueMode::OneAtATime,
            auto_compaction_enabled: true,
            auto_retry_enabled: true,
            is_streaming: false,
            is_compacting: false,
            cancel: Arc::new(Mutex::new(CancellationToken::new())),
            bash_cancel: Arc::new(Mutex::new(CancellationToken::new())),
        })
    }

    /// Create an in-memory session (for testing).
    pub fn in_memory(model: model::Model, tools: Vec<AgentTool>) -> Self {
        Self::in_memory_with_client(model, tools, model::Client::new())
    }

    /// Create an in-memory session with a custom `Client` (for testing).
    ///
    /// This allows tests to register mock providers on the client without
    /// going through env vars or an [`AgentSessionConfig`]. The dispatcher
    /// unit tests in `rpc::server` use this to wire `mock_text_provider`
    /// directly into the session that drives a `prompt` turn.
    pub fn in_memory_with_client(
        model: model::Model,
        tools: Vec<AgentTool>,
        client: model::Client,
    ) -> Self {
        let context = AgentContext {
            system_prompt: "You are a helpful coding assistant.".into(),
            messages: vec![],
        };

        let model_registry = ModelRegistry::build(&client);
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
            tools: Some(tools),
            client,
            event_listeners: Arc::new(Mutex::new(Vec::new())),
            skills: Vec::new(),
            skill_errors: Vec::new(),
            extensions: Vec::new(),
            model_registry,
            steering_mode: QueueMode::OneAtATime,
            follow_up_mode: QueueMode::OneAtATime,
            auto_compaction_enabled: true,
            auto_retry_enabled: true,
            is_streaming: false,
            is_compacting: false,
            cancel: Arc::new(Mutex::new(CancellationToken::new())),
            bash_cancel: Arc::new(Mutex::new(CancellationToken::new())),
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

        // Mark the session as streaming for the duration of this turn.
        // Restored on the happy path below; on cancel/panic the field
        // stays `true` until the next turn or `reset_session()`. We
        // accept this minor staleness — RPC callers that observe a
        // cancelled session will reconcile via `get_state` after their
        // own retry/abort logic, and the field has no safety impact.
        self.is_streaming = true;

        // Snapshot the extension chain and per-session context so the hook
        // closures can own them as `'static` data captured by the `Box<dyn Fn>`.
        // Cloning the `Vec<Arc<...>>` is cheap (Arc bumps).
        let (before_hook, after_hook) = if self.extensions.is_empty() {
            (None, None)
        } else {
            let extensions: Arc<Vec<Arc<dyn Extension>>> = Arc::new(self.extensions.clone());
            let cx = Arc::new(self.extension_context());
            (
                Some(build_before_tool_call_hook(extensions.clone(), cx.clone())),
                Some(build_after_tool_call_hook(extensions, cx)),
            )
        };

        // Build agent loop config from defaults, then apply session-level
        // overrides. After the merge with origin/main, AgentLoopConfig no
        // longer carries `cwd` / `session_id` — those moved out of the
        // hook surface entirely. Extensions that need cwd/session_id read
        // them from the host-supplied `ExtensionContext` instead.
        let mut loop_config = AgentLoopConfig::new(
            self.config.model.clone(),
            self.config.stream_options.clone(),
        );
        loop_config.tool_execution = hand_agent::types::ToolExecutionMode::Parallel;
        loop_config.before_tool_call = before_hook;
        loop_config.after_tool_call = after_hook;
        loop_config.steering_mode = queue_mode_to_delivery(self.steering_mode);
        loop_config.follow_up_mode = queue_mode_to_delivery(self.follow_up_mode);

        // Create event sink for the agent loop. Replace the session's
        // cancellation token with a fresh one so a previous turn's
        // `abort()` can't poison this turn — and so the new token is
        // observable by `Self::abort()` while this turn is running.
        let emit = self.build_event_sink();
        let cancel = {
            let new_token = CancellationToken::new();
            *self.cancel.lock().unwrap() = new_token.clone();
            new_token
        };

        // Merge built-in tools with extension-contributed custom tools so
        // the model can call them through the same agent loop tool list.
        // `AgentTool` is not `Clone` (its `execute` is a boxed closure), so
        // we *move* the session's tools out and rely on an RAII guard
        // (`ToolsRestoreGuard`) to restore them on scope exit. The guard's
        // `Drop` fires on the happy path AND on cancellation/panic — without
        // it, a cancelled `send_message` future would leak the built-in
        // tools and the next turn would see an empty tool list.
        let extension_tools = self.collected_custom_tools();
        let mut owned_tools = self
            .tools
            .take()
            .expect("AgentSession::tools invariant: Some outside send_message");
        let original_len = owned_tools.len();
        owned_tools.extend(extension_tools);

        // The guard borrows `&mut self.tools` (the `Option`) but NOT `&mut self.context`.
        // Rust's split borrows on disjoint fields permit this even across the
        // await below.
        let guard = ToolsRestoreGuard {
            slot: &mut self.tools,
            tools: Some(owned_tools),
            keep: original_len,
        };
        let tools_ref: &[AgentTool] = guard
            .tools
            .as_deref()
            .expect("guard tools set above");

        let result_outcome = agent_loop::run_agent_loop(
            prompts,
            &mut self.context,
            tools_ref,
            &loop_config,
            &self.client,
            &emit,
            &cancel,
        )
        .await;

        // Explicit drop ends the borrow and restores `self.tools` here on the
        // happy path. On cancel/panic the same Drop runs implicitly.
        drop(guard);

        // Streaming complete (either Ok or Err — happens before compaction
        // so `is_streaming` doesn't bleed into the compaction window).
        self.is_streaming = false;

        let result = result_outcome.map_err(CodingAgentError::Agent)?;

        // Persist new messages to session
        for msg in &result.messages {
            let _ = self.session_manager.append_message(msg.clone());
        }

        // Check for compaction
        self.maybe_compact_if_needed().await?;

        Ok(result.messages)
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

    /// Borrow the underlying `model::Client`. Used by the RPC dispatcher
    /// to preserve a session's provider registry across `new_session`.
    pub fn client(&self) -> &model::Client {
        &self.client
    }

    /// Reset the conversation state to a fresh session, preserving
    /// subscriber-facing fields (event listeners, extensions, tools, client,
    /// model). This is the in-place equivalent of constructing a new
    /// session — used by the RPC `new_session` handler so listeners that
    /// subscribed via [`Self::subscribe`] before the reset keep receiving
    /// events from the post-reset turn.
    ///
    /// What gets reset:
    /// - `context.messages` is cleared (system prompt is preserved).
    /// - `session_manager` is replaced with a fresh manager. If the current
    ///   manager is in-memory the replacement is also in-memory; otherwise
    ///   we create a new on-disk JSONL file under the session's `cwd`.
    ///
    /// What is preserved:
    /// - `event_listeners` (the regression this method exists to fix).
    /// - `extensions`, `tools`, `client`, `settings_manager`, `skills`,
    ///   `skill_errors`, `config` (model, cwd, stream options, ...).
    pub fn reset_session(&mut self) -> Result<(), CodingAgentError> {
        let new_sm = if self.session_manager.is_in_memory() {
            SessionManager::in_memory()
        } else {
            SessionManager::create(&self.config.cwd)?
        };
        self.session_manager = new_sm;
        self.context.messages.clear();
        // Runtime flags are conversation-scoped; reset alongside the
        // messages so `is_streaming`/`is_compacting` don't bleed over
        // from a cancelled prior turn.
        self.is_streaming = false;
        self.is_compacting = false;
        Ok(())
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

    /// Manually trigger compaction unconditionally — bypasses both the
    /// `auto_compaction_enabled` toggle and the
    /// `compaction::should_compact` token-threshold gate. Returns the
    /// summary string the agent generated (or a structured fallback if
    /// the LLM call failed). Used by the RPC `compact` handler and the
    /// `/compact` slash command, where the user is explicitly asking
    /// for compaction regardless of session state.
    pub async fn compact(&mut self) -> Result<String, CodingAgentError> {
        self.do_compact().await
    }

    /// Compact-if-needed: no-op unless auto-compaction is enabled AND
    /// the should_compact threshold is met. Called automatically at
    /// the end of `send_message`.
    async fn maybe_compact_if_needed(&mut self) -> Result<(), CodingAgentError> {
        if !self.auto_compaction_enabled {
            return Ok(());
        }
        let settings = self.settings_manager.compaction_settings();
        let context_tokens = compaction::estimate_context_tokens(&self.context.messages);
        let max_context_tokens = settings.max_context_tokens() as usize;
        if !compaction::should_compact(context_tokens, max_context_tokens, &settings) {
            return Ok(());
        }
        let _ = self.do_compact().await?;
        Ok(())
    }

    /// Run the compaction summarizer + record the result on the session
    /// manager. Returns the summary text. Caller is responsible for any
    /// gating (auto-toggle, token threshold).
    async fn do_compact(&mut self) -> Result<String, CodingAgentError> {
        let settings = self.settings_manager.compaction_settings();

        self.is_compacting = true;
        self.emit(AgentSessionEvent::CompactionStart);

        let (to_compact, _to_keep, _split_idx) = compaction::split_for_compaction(
            &self.context.messages,
            settings.keep_recent_tokens() as usize,
        );

        let file_ops = compaction::extract_file_operations(&to_compact);
        let summary_prompt = compaction::build_compaction_prompt(&to_compact, &file_ops);

        let summary = match self.generate_compaction_summary(&summary_prompt).await {
            Ok(s) => s,
            Err(_) => format!(
                "[Compacted {} messages. Files read: {}. Files edited: {}.]",
                to_compact.len(),
                file_ops.read.join(", "),
                file_ops.edited.join(", "),
            ),
        };

        let first_kept_id = format!("compaction_{}", chrono::Utc::now().timestamp_millis());
        self.session_manager
            .append_compaction(&summary, &first_kept_id)?;

        self.emit(AgentSessionEvent::CompactionEnd {
            summary: summary.clone(),
        });
        self.is_compacting = false;

        Ok(summary)
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

    /// Steering queue mode (mid-turn user-message delivery policy).
    pub fn steering_mode(&self) -> QueueMode {
        self.steering_mode
    }

    /// Set the steering queue mode.
    pub fn set_steering_mode(&mut self, mode: QueueMode) {
        self.steering_mode = mode;
    }

    /// Follow-up queue mode (between-turn user-message delivery policy).
    pub fn follow_up_mode(&self) -> QueueMode {
        self.follow_up_mode
    }

    /// Set the follow-up queue mode.
    pub fn set_follow_up_mode(&mut self, mode: QueueMode) {
        self.follow_up_mode = mode;
    }

    /// Whether auto-compaction is enabled for this session.
    pub fn auto_compaction_enabled(&self) -> bool {
        self.auto_compaction_enabled
    }

    /// Toggle auto-compaction.
    pub fn set_auto_compaction(&mut self, enabled: bool) {
        self.auto_compaction_enabled = enabled;
    }

    /// Whether automatic retry-with-backoff is enabled.
    pub fn auto_retry_enabled(&self) -> bool {
        self.auto_retry_enabled
    }

    /// Toggle automatic retry-with-backoff.
    pub fn set_auto_retry(&mut self, enabled: bool) {
        self.auto_retry_enabled = enabled;
    }

    /// Whether the session is currently inside an `agent_loop` turn.
    pub fn is_streaming(&self) -> bool {
        self.is_streaming
    }

    /// Whether the session is currently performing a compaction summary.
    pub fn is_compacting(&self) -> bool {
        self.is_compacting
    }

    /// Cancel the in-flight `send_message` (if any). Idempotent — safe
    /// to call when no turn is running. Returns `true` if a token was
    /// cancelled, `false` if there was nothing to cancel (token was
    /// already cancelled or no turn ever started). The agent loop
    /// observes the cancellation at its next await point and unwinds
    /// the future, restoring tool state via `ToolsRestoreGuard`.
    pub fn abort(&self) -> bool {
        let token = self.cancel.lock().unwrap();
        if token.is_cancelled() {
            false
        } else {
            token.cancel();
            true
        }
    }

    /// Cancel an in-flight [`Self::run_bash`] (if any). Idempotent — safe
    /// to call when no bash command is running. Returns `true` if a token
    /// was cancelled, `false` if there was nothing to cancel. Mirrors the
    /// shape of [`Self::abort`] but for the bash-only cancellation
    /// channel.
    pub fn abort_bash(&self) -> bool {
        let token = self.bash_cancel.lock().unwrap();
        if token.is_cancelled() {
            false
        } else {
            token.cancel();
            true
        }
    }

    /// Run a one-off bash command, racing it against [`Self::abort_bash`].
    ///
    /// Replaces the stored `bash_cancel` with a fresh token so a stale
    /// abort can't poison this call, then races the executor future
    /// against the new token's `cancelled()` future via `tokio::select!`.
    /// On cancel, returns a synthesized [`BashResult`] with
    /// `truncated: true`, the abort marker `"[bash aborted]"` on
    /// `output`, and `aborted: true` so the caller can route the marker
    /// to `stderr` on the wire (see [`RunBashOutcome`]). The underlying
    /// child process is killed via [`tokio::process::Command::kill_on_drop`]
    /// — dropping the executor future on the cancel arm reaps the child.
    ///
    /// `timeout_secs` is forwarded to [`BashExecutorOptions::timeout_secs`]
    /// (0 disables the timeout).
    pub async fn run_bash(
        &self,
        command: &str,
        timeout_secs: u64,
    ) -> Result<RunBashOutcome, CodingAgentError> {
        // Replace the cancel token so callers from a previous run can't
        // poison this call.
        let cancel = {
            let new_token = CancellationToken::new();
            *self.bash_cancel.lock().unwrap() = new_token.clone();
            new_token
        };

        let shell_path = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let options = crate::core::bash_executor::BashExecutorOptions {
            on_chunk: None,
            timeout_secs,
            ..Default::default()
        };

        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                Ok(RunBashOutcome {
                    result: crate::core::bash_executor::BashResult {
                        output: "[bash aborted]".to_string(),
                        exit_code: None,
                        truncated: true,
                    },
                    aborted: true,
                })
            }
            res = crate::core::bash_executor::execute_bash(
                command,
                &self.config.cwd,
                &shell_path,
                options,
            ) => res.map(|result| RunBashOutcome { result, aborted: false }),
        }
    }

    /// Path to the on-disk JSONL session file, if any. Returns `None`
    /// for in-memory sessions.
    pub fn session_file(&self) -> Option<&Path> {
        if self.session_manager.is_in_memory() {
            None
        } else {
            Some(self.session_manager.path())
        }
    }

    /// Per-skill discovery errors collected at construction time.
    ///
    /// Returns an empty slice for in-memory sessions (which skip disk
    /// discovery entirely). Used by `--diagnostics` and similar surfaces
    /// to report malformed `SKILL.md` files without aborting the session.
    pub fn skill_errors(&self) -> &[SkillError] {
        &self.skill_errors
    }

    /// Skills successfully discovered at construction time and advertised
    /// in the system prompt. Empty for in-memory sessions.
    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }

    /// Tier 1 extensions registered on this session, in dispatch order.
    pub fn extensions(&self) -> &[Arc<dyn Extension>] {
        &self.extensions
    }

    /// Built-in tools registered on this session.
    ///
    /// Returns an empty slice if invoked while the session is mid-`send_message`
    /// (the tools are temporarily moved into a guard for the duration of the
    /// agent loop). Outside of `send_message` the slice always reflects the
    /// tools the caller passed into the constructor.
    pub fn tools(&self) -> &[AgentTool] {
        self.tools.as_deref().unwrap_or(&[])
    }

    /// Append an extension to the dispatch chain. Useful for tests and
    /// dynamic registration scenarios that don't go through
    /// [`builtin_tier1_extensions`].
    ///
    /// Rebuilds the [`ModelRegistry`] so any models contributed by the new
    /// extension surface in [`Self::model_registry`]. Extensions don't
    /// contribute models in v1, but the rebuild keeps the cache consistent
    /// once they do (cheap: the static catalog has ~dozens of entries).
    pub fn register_extension(&mut self, ext: Arc<dyn Extension>) {
        self.extensions.push(ext);
        self.model_registry = ModelRegistry::build(&self.client);
    }

    /// Aggregate model catalog for this session.
    ///
    /// Built eagerly at construction; rebuilt on
    /// [`Self::register_extension`]. The returned reference is stable until
    /// the next mutation that triggers a rebuild.
    pub fn model_registry(&self) -> &ModelRegistry {
        &self.model_registry
    }

    /// Slash commands contributed by every registered extension, paired with
    /// the contributing extension. Used by the slash-command dispatcher to
    /// resolve a `/foo` invocation to the right extension. The list is built
    /// fresh on each call; for v1 it's recomputed lazily because the cost is
    /// negligible (extensions are not added mid-session in practice).
    pub fn collected_slash_commands(&self) -> Vec<(SlashCommandSpec, Arc<dyn Extension>)> {
        let mut out = Vec::new();
        for ext in &self.extensions {
            for spec in ext.slash_commands() {
                out.push((spec, ext.clone()));
            }
        }
        out
    }

    /// Custom AgentTools contributed by every registered extension, flattened
    /// into one list ready to merge with the session's built-in tools. Tier 1
    /// extensions return tools backed by Rust closures; Tier 2 extensions
    /// return tools whose execute fn drives an RPC into the subprocess.
    pub fn collected_custom_tools(&self) -> Vec<AgentTool> {
        let cx = self.extension_context();
        let mut out = Vec::new();
        for ext in &self.extensions {
            out.extend(ext.custom_tools(&cx));
        }
        out
    }

    /// Build the [`ExtensionContext`] passed to hooks for this session.
    ///
    /// `data_dir` is computed as `<cwd>/.hand/extensions/<unspecified>/data/`
    /// at the session level — extensions get a per-extension subdirectory
    /// resolved when they're invoked. For now we surface the session-wide
    /// root so callers and tests can verify it's well-formed; lazy creation
    /// of the per-extension subdir lands in T3.4.
    pub fn extension_context(&self) -> ExtensionContext {
        ExtensionContext {
            cwd: self.config.cwd.clone(),
            session_id: self.session_manager.id().to_string(),
            data_dir: self.config.cwd.join(".hand").join("extensions"),
        }
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

/// Build a `BeforeToolCallHook` that drives `dispatch_before_tool_call`.
///
/// Map the RPC-level [`QueueMode`] enum to the hand-agent runtime
/// equivalent. Both have the same shape but live in different layers;
/// keep the conversion exhaustive so a new variant on either side
/// surfaces as a compile error.
fn queue_mode_to_delivery(mode: QueueMode) -> hand_agent::QueueDeliveryMode {
    match mode {
        QueueMode::All => hand_agent::QueueDeliveryMode::All,
        QueueMode::OneAtATime => hand_agent::QueueDeliveryMode::OneAtATime,
    }
}

/// Build a `BeforeToolCallHook` that fans the tool-call event out to
/// every registered extension and aggregates their decisions.
///
/// NOTE: after the merge with origin/main, hand-agent's
/// [`BeforeToolCallResult`] dropped its `replace_args` field. The Tier-1
/// hook chain still emits [`HookDecision::Replace(args)`] but we can no
/// longer forward it to the agent loop — the rewrite is logged and
/// downgraded to `Continue` until a follow-up re-introduces argument
/// rewriting in hand-agent.
fn build_before_tool_call_hook(
    extensions: Arc<Vec<Arc<dyn Extension>>>,
    cx: Arc<ExtensionContext>,
) -> hand_agent::types::BeforeToolCallHook {
    Arc::new(
        move |ctx: BeforeToolCallContext<'_>,
              _cancel: hand_agent::CancellationToken|
              -> BoxFuture<'_, Option<BeforeToolCallResult>> {
            let extensions = extensions.clone();
            let cx = cx.clone();
            let event = ToolCallEvent {
                tool_name: ctx.tool_call.name.clone(),
                arguments: ctx.args.clone(),
                call_id: ctx.tool_call.id.clone(),
            };
            Box::pin(async move {
                let decision = dispatch_before_tool_call(&extensions, &cx, &event).await;
                match decision {
                    HookDecision::Continue => None,
                    HookDecision::Replace(_args) => {
                        tracing::warn!(
                            tool = %event.tool_name,
                            "extension requested arg rewrite (HookDecision::Replace) but \
                             hand-agent::BeforeToolCallResult no longer supports it; \
                             treating as Continue. Re-enable by restoring replace_args."
                        );
                        None
                    }
                    HookDecision::Cancel(reason) => Some(BeforeToolCallResult {
                        block: true,
                        reason: Some(reason),
                    }),
                }
            })
        },
    )
}

/// Build an `AfterToolCallHook` that fans the result event out to every
/// registered extension. The result is read-only — extensions cannot
/// rewrite it — so the hook always returns `None`.
fn build_after_tool_call_hook(
    extensions: Arc<Vec<Arc<dyn Extension>>>,
    cx: Arc<ExtensionContext>,
) -> hand_agent::types::AfterToolCallHook {
    Arc::new(
        move |ctx: AfterToolCallContext<'_>,
              _cancel: hand_agent::CancellationToken|
              -> BoxFuture<'_, Option<AfterToolCallResult>> {
            let extensions = extensions.clone();
            let cx = cx.clone();
            // Render the tool result content as JSON for the extension. The
            // ToolResult shape is internal to hand-agent; the v1 event
            // surface exposes `success` (== !is_error) and the JSON body.
            let event = ToolResultEvent {
                tool_name: ctx.tool_call.name.clone(),
                call_id: ctx.tool_call.id.clone(),
                success: !ctx.is_error,
                result: serde_json::to_value(ctx.result).unwrap_or_else(|err| {
                    tracing::warn!(
                        error = %err,
                        "failed to serialize tool result for after-hook; using null"
                    );
                    serde_json::Value::Null
                }),
            };
            Box::pin(async move {
                dispatch_after_tool_call(&extensions, &cx, &event).await;
                None
            })
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

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

    fn test_config(cwd: PathBuf) -> AgentSessionConfig {
        AgentSessionConfig {
            cwd,
            model: test_model(),
            stream_options: SimpleStreamOptions::default(),
            custom_system_prompt: None,
            custom_guidelines: None,
            resume_session: None,
        }
    }

    /// `AgentSession::new_with_skill_dirs` discovers a project skill and
    /// inserts it into the system prompt.
    #[test]
    fn agent_session_discovers_project_skill() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join(".hand").join("skills").join("foo");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: A foo skill for tests.\n---\nfoo body",
        )
        .unwrap();

        let session = AgentSession::new_with_skill_dirs(
            test_config(tmp.path().to_path_buf()),
            vec![],
            None,
            None,
        )
        .expect("session constructs");

        assert!(
            session.skill_errors().is_empty(),
            "unexpected errors: {:?}",
            session.skill_errors()
        );
        assert_eq!(session.skills().len(), 1);
        assert_eq!(session.skills()[0].name, "foo");

        let prompt = &session.context.system_prompt;
        assert!(prompt.contains("<name>foo</name>"), "prompt missing skill name: {prompt}");
        assert!(prompt.contains("A foo skill for tests."));
    }

    /// A malformed SKILL.md is recorded in `skill_errors()` but the session
    /// itself still constructs cleanly. The bad skill is just absent from
    /// the system prompt.
    #[test]
    fn agent_session_records_skill_errors() {
        let tmp = TempDir::new().unwrap();
        let bad = tmp.path().join(".hand").join("skills").join("bad");
        fs::create_dir_all(&bad).unwrap();
        // Frontmatter open with no close → loader-level frontmatter error.
        fs::write(bad.join("SKILL.md"), "---\ndescription: oops\nbody without close\n").unwrap();

        let session = AgentSession::new_with_skill_dirs(
            test_config(tmp.path().to_path_buf()),
            vec![],
            None,
            None,
        )
        .expect("session constructs even with a bad skill");

        assert!(
            !session.skill_errors().is_empty(),
            "expected at least one error from malformed SKILL.md"
        );
        assert!(
            session.skills().is_empty(),
            "bad skill should not be advertised: {:?}",
            session.skills()
        );
        // The bad skill name does not leak into the system prompt.
        assert!(!session.context.system_prompt.contains("<name>bad</name>"));
    }

    /// In-memory sessions skip disk discovery entirely, so both the skills
    /// list and the error list are empty.
    #[test]
    fn in_memory_session_has_no_skills() {
        let session = AgentSession::in_memory(test_model(), vec![]);
        assert!(session.skill_errors().is_empty());
        assert!(session.skills().is_empty());
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

    // ------------------------------------------------------------------
    // Extension wiring tests
    // ------------------------------------------------------------------

    use crate::core::extensions::api::{
        ExtensionCapabilities, ExtensionError, ExtensionManifest, HookDecision, ToolCallEvent,
        ToolResultEvent,
    };
    use async_trait::async_trait;

    fn ext_manifest(name: &str) -> ExtensionManifest {
        ExtensionManifest {
            name: name.into(),
            version: "0.1.0".into(),
            description: None,
            capabilities: ExtensionCapabilities::default(),
            exec: None,
            env: Default::default(),
            slash_commands: Vec::new(),
            custom_tools: Vec::new(),
        }
    }

    /// A test extension that records every before/after invocation it sees.
    /// `before_decision` is what `on_before_tool_call` returns; `after_ok`
    /// controls whether `on_after_tool_call` returns Ok or Err.
    struct RecordingExt {
        manifest: ExtensionManifest,
        before_decision: HookDecision,
        before_calls: Mutex<Vec<ToolCallEvent>>,
        after_calls: Mutex<Vec<ToolResultEvent>>,
    }

    impl RecordingExt {
        fn new(name: &str, before_decision: HookDecision) -> Arc<Self> {
            Arc::new(Self {
                manifest: ext_manifest(name),
                before_decision,
                before_calls: Mutex::new(Vec::new()),
                after_calls: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl Extension for RecordingExt {
        fn manifest(&self) -> &ExtensionManifest {
            &self.manifest
        }

        async fn on_before_tool_call(
            &self,
            _cx: &ExtensionContext,
            event: &ToolCallEvent,
        ) -> Result<HookDecision, ExtensionError> {
            self.before_calls.lock().unwrap().push(event.clone());
            Ok(self.before_decision.clone())
        }

        async fn on_after_tool_call(
            &self,
            _cx: &ExtensionContext,
            event: &ToolResultEvent,
        ) -> Result<(), ExtensionError> {
            self.after_calls.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    #[test]
    fn register_extension_appends_to_chain() {
        let mut session = AgentSession::in_memory(test_model(), vec![]);
        assert!(session.extensions().is_empty());

        let ext = RecordingExt::new("recorder", HookDecision::Continue);
        session.register_extension(ext.clone());

        assert_eq!(session.extensions().len(), 1);
        assert_eq!(session.extensions()[0].manifest().name, "recorder");
    }

    /// With no extensions registered, `collected_slash_commands()` returns
    /// an empty list.
    #[test]
    fn collected_slash_commands_empty_when_no_extensions() {
        let session = AgentSession::in_memory(test_model(), vec![]);
        assert!(session.collected_slash_commands().is_empty());
    }

    /// With no extensions registered, `collected_custom_tools()` returns an
    /// empty list.
    #[test]
    fn collected_custom_tools_empty_when_no_extensions() {
        let session = AgentSession::in_memory(test_model(), vec![]);
        assert!(session.collected_custom_tools().is_empty());
    }

    /// A Tier-1 extension that contributes a single slash command surfaces
    /// it via `collected_slash_commands()`, paired with the contributing
    /// extension Arc.
    #[test]
    fn tier1_extension_contributes_slash_command() {
        struct CmdExt {
            manifest: ExtensionManifest,
        }
        #[async_trait]
        impl Extension for CmdExt {
            fn manifest(&self) -> &ExtensionManifest {
                &self.manifest
            }
            fn slash_commands(&self) -> Vec<crate::core::extensions::api::SlashCommandSpec> {
                vec![crate::core::extensions::api::SlashCommandSpec {
                    name: "commit-now".into(),
                    description: "Commit pending changes".into(),
                    usage: None,
                }]
            }
        }

        let mut session = AgentSession::in_memory(test_model(), vec![]);
        session.register_extension(Arc::new(CmdExt {
            manifest: ext_manifest("auto-commit"),
        }));

        let collected = session.collected_slash_commands();
        assert_eq!(collected.len(), 1);
        let (spec, ext) = &collected[0];
        assert_eq!(spec.name, "commit-now");
        assert_eq!(ext.manifest().name, "auto-commit");
    }

    /// A Tier-1 extension that contributes a single custom tool surfaces it
    /// via `collected_custom_tools()`. The tool's execute fn is invokable
    /// and returns the expected result.
    #[tokio::test]
    async fn tier1_extension_contributes_custom_tool() {
        struct ToolExt {
            manifest: ExtensionManifest,
        }
        #[async_trait]
        impl Extension for ToolExt {
            fn manifest(&self) -> &ExtensionManifest {
                &self.manifest
            }
            fn custom_tools(
                &self,
                _cx: &crate::core::extensions::api::ExtensionContext,
            ) -> Vec<AgentTool> {
                vec![AgentTool::simple(
                    "echo",
                    "Echo a string",
                    serde_json::json!({"type":"object","properties":{}}),
                    "Echo",
                    |_call_id, _args| async move {
                        hand_agent::types::ToolResult::text("custom tool ran")
                    },
                )]
            }
        }

        let mut session = AgentSession::in_memory(test_model(), vec![]);
        session.register_extension(Arc::new(ToolExt {
            manifest: ext_manifest("echoer"),
        }));

        let tools = session.collected_custom_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");

        let ctx = hand_agent::types::ToolExecuteCtx {
            tool_call_id: "c1".into(),
            args: serde_json::json!({}),
            cancel: hand_agent::CancellationToken::new(),
            on_update: std::sync::Arc::new(|_| {}),
        };
        let result = (tools[0].execute)(ctx).await.expect("tool execute Ok");
        let mut text = String::new();
        for block in &result.content {
            if let model::ToolResultContent::Text(t) = block {
                text = t.text.clone();
            }
        }
        assert_eq!(text, "custom tool ran");
    }

    #[test]
    fn extension_context_returns_well_formed_values() {
        let session = AgentSession::in_memory(test_model(), vec![]);
        let cx = session.extension_context();

        assert!(!cx.session_id.is_empty(), "session id must not be empty");
        assert!(!cx.cwd.as_os_str().is_empty(), "cwd must not be empty");
        assert!(
            cx.data_dir.ends_with("extensions"),
            "data_dir should be rooted at .hand/extensions, got {:?}",
            cx.data_dir
        );
    }

    // -- Integration: send_message fires hooks via a mock tool-call provider.
    //
    // The mock returns a tool_use turn first and a text turn afterwards so
    // the agent loop terminates. The session is configured with a single
    // `noop` AgentTool so the tool execution succeeds.

    use model::types::{Api, Provider};
    use model::{
        ApiProvider, AssistantContentBlock, AssistantMessage, AssistantMessageEvent,
        AssistantMessageEventStream, Context, StopReason, StreamOptions, TextContent, ToolCall,
        Usage,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn assistant_text_message(text: &str) -> AssistantMessage {
        AssistantMessage {
            role: "assistant".into(),
            content: vec![AssistantContentBlock::Text(TextContent::new(text))],
            api: Api::OpenAICompletions,
            provider: Provider::OpenAI,
            model: "test-model".into(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        }
    }

    fn assistant_tool_call_message(
        tool_name: &str,
        tool_id: &str,
        args: serde_json::Value,
    ) -> AssistantMessage {
        AssistantMessage {
            role: "assistant".into(),
            content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
                tool_id, tool_name, args,
            ))],
            api: Api::OpenAICompletions,
            provider: Provider::OpenAI,
            model: "test-model".into(),
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        }
    }

    /// Mock provider: turn 1 emits a tool call, turn 2+ emit text and stop.
    struct ToolThenTextProvider {
        tool_name: String,
        args: serde_json::Value,
        invocation: AtomicUsize,
    }

    impl ApiProvider for ToolThenTextProvider {
        fn stream(
            &self,
            _model: model::Model,
            _context: Context,
            _options: Option<StreamOptions>,
        ) -> AssistantMessageEventStream<'static> {
            let n = self.invocation.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                let tool_name = self.tool_name.clone();
                let args = self.args.clone();
                Box::pin(async_stream::stream! {
                    let msg = assistant_tool_call_message(&tool_name, "call_1", args);
                    let tool_call = match &msg.content[0] {
                        AssistantContentBlock::ToolCall(tc) => tc.clone(),
                        _ => unreachable!("constructed with ToolCall block"),
                    };
                    yield AssistantMessageEvent::Start { partial: msg.clone() };
                    yield AssistantMessageEvent::ToolCallStart {
                        content_index: 0,
                        partial: msg.clone(),
                    };
                    yield AssistantMessageEvent::ToolCallEnd {
                        content_index: 0,
                        tool_call,
                        partial: msg.clone(),
                    };
                    yield AssistantMessageEvent::Done {
                        reason: StopReason::ToolUse,
                        message: msg,
                    };
                })
            } else {
                Box::pin(async_stream::stream! {
                    let msg = assistant_text_message("done");
                    yield AssistantMessageEvent::Start { partial: msg.clone() };
                    yield AssistantMessageEvent::Done {
                        reason: StopReason::Stop,
                        message: msg,
                    };
                })
            }
        }

        fn stream_simple(
            &self,
            model: model::Model,
            context: Context,
            options: Option<SimpleStreamOptions>,
        ) -> AssistantMessageEventStream<'static> {
            self.stream(model, context, options.map(|o| o.base))
        }
    }

    fn noop_tool() -> AgentTool {
        AgentTool::simple(
            "noop",
            "A no-op test tool.",
            serde_json::json!({"type": "object", "properties": {}}),
            "Noop",
            |_call_id, _args| async move { hand_agent::types::ToolResult::text("noop ok") },
        )
    }

    /// Same shape as `test_model()` but on `OpenAICompletions` so the mock
    /// provider registered for that API actually matches.
    fn openai_test_model() -> model::Model {
        let mut m = test_model();
        m.api = Api::OpenAICompletions;
        m.provider = Provider::OpenAI;
        m
    }

    #[tokio::test]
    async fn send_message_fires_before_and_after_hooks_on_tool_call() {
        // Register the mock provider on a Client.
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(ToolThenTextProvider {
                tool_name: "noop".into(),
                args: serde_json::json!({}),
                invocation: AtomicUsize::new(0),
            }),
            Some("test".into()),
        );

        let mut session =
            AgentSession::in_memory_with_client(openai_test_model(), vec![noop_tool()], client);

        let ext = RecordingExt::new("recorder", HookDecision::Continue);
        session.register_extension(ext.clone());

        let _ = session
            .send_message("please call noop")
            .await
            .expect("send_message should succeed");

        let before_calls = ext.before_calls.lock().unwrap();
        assert_eq!(
            before_calls.len(),
            1,
            "before hook should fire exactly once for the tool call"
        );
        assert_eq!(before_calls[0].tool_name, "noop");

        let after_calls = ext.after_calls.lock().unwrap();
        assert_eq!(
            after_calls.len(),
            1,
            "after hook should fire exactly once for the tool result"
        );
        assert_eq!(after_calls[0].tool_name, "noop");
        assert!(after_calls[0].success, "noop tool should report success");
    }

    /// Cancel-safety regression: when the future returned by `send_message`
    /// is dropped mid-flight (the typical cancellation path the host RPC
    /// layer uses via `tokio::select!`), the session's built-in tools must
    /// be restored. Without the [`ToolsRestoreGuard`] this test fails:
    /// `tools()` returns `&[]` because the manual restore never ran.
    #[tokio::test]
    async fn send_message_cancel_restores_tools() {
        /// Provider whose stream pends forever — guarantees the agent loop
        /// is awaiting when we cancel `send_message`.
        struct PendingForeverProvider;
        impl ApiProvider for PendingForeverProvider {
            fn stream(
                &self,
                _model: model::Model,
                _context: Context,
                _options: Option<StreamOptions>,
            ) -> AssistantMessageEventStream<'static> {
                Box::pin(async_stream::stream! {
                    let () = std::future::pending().await;
                    yield AssistantMessageEvent::Done {
                        reason: StopReason::Stop,
                        message: assistant_text_message("unreachable"),
                    };
                })
            }
            fn stream_simple(
                &self,
                model: model::Model,
                context: Context,
                options: Option<SimpleStreamOptions>,
            ) -> AssistantMessageEventStream<'static> {
                self.stream(model, context, options.map(|o| o.base))
            }
        }

        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(PendingForeverProvider),
            Some("test".into()),
        );

        let mut session =
            AgentSession::in_memory_with_client(openai_test_model(), vec![noop_tool()], client);
        assert_eq!(session.tools().len(), 1, "precondition: one built-in tool");

        // Drive `send_message` to its first await on the provider stream,
        // then cancel by dropping the future via `timeout`. The
        // `ToolsRestoreGuard`'s `Drop` must restore `self.tools`.
        let send_fut = session.send_message("hi");
        let outcome =
            tokio::time::timeout(std::time::Duration::from_millis(50), send_fut).await;
        assert!(outcome.is_err(), "send_message should have been cancelled by timeout");

        assert_eq!(
            session.tools().len(),
            1,
            "tools must be restored to their original state after cancel"
        );
        assert_eq!(session.tools()[0].name, "noop");
    }

    #[tokio::test]
    async fn send_message_cancel_blocks_tool_execution() {
        // The recording extension cancels every tool call. The tool executor
        // increments a counter — assert the counter stays at 0 because the
        // host short-circuited before running the tool.
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(ToolThenTextProvider {
                tool_name: "noop".into(),
                args: serde_json::json!({}),
                invocation: AtomicUsize::new(0),
            }),
            Some("test".into()),
        );

        let executions = Arc::new(AtomicUsize::new(0));
        let executions_for_tool = executions.clone();
        let counted_tool = AgentTool::simple(
            "noop",
            "A no-op test tool.",
            serde_json::json!({"type": "object", "properties": {}}),
            "Noop",
            move |_call_id, _args| {
                let counter = executions_for_tool.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    hand_agent::types::ToolResult::text("ran")
                }
            },
        );

        let mut session =
            AgentSession::in_memory_with_client(openai_test_model(), vec![counted_tool], client);

        let ext = RecordingExt::new("blocker", HookDecision::Cancel("nope".into()));
        session.register_extension(ext.clone());

        let _ = session
            .send_message("please call noop")
            .await
            .expect("send_message should succeed even when hook cancels");

        assert_eq!(
            executions.load(Ordering::SeqCst),
            0,
            "tool must NOT run when before-hook returns Cancel"
        );
        // before fires once; after does NOT fire because the agent loop emits an
        // Immediate error result for blocked calls and skips finalize_executed_tool_call.
        assert_eq!(ext.before_calls.lock().unwrap().len(), 1);
        assert_eq!(
            ext.after_calls.lock().unwrap().len(),
            0,
            "after hook must not fire when the call was blocked",
        );
    }

    /// F23 regression (post-merge): a `HookDecision::Replace(args)` from a
    /// Tier-1 extension is currently downgraded to `Continue` (with a
    /// warning) because hand-agent's `BeforeToolCallResult` no longer
    /// carries `replace_args` after the merge with origin/main. This test
    /// pins the contract: send_message still succeeds, the tool observes
    /// the model's ORIGINAL args, and no panic / unwind escapes.
    ///
    /// When `replace_args` is restored upstream this test should flip to
    /// asserting the rewritten args are observed.
    #[tokio::test]
    async fn replace_args_currently_downgrades_to_continue() {
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(ToolThenTextProvider {
                tool_name: "noop".into(),
                args: serde_json::json!({"original": true}),
                invocation: AtomicUsize::new(0),
            }),
            Some("test".into()),
        );

        let observed: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let observed_for_tool = observed.clone();
        let recorder_tool = AgentTool::simple(
            "noop",
            "Records args",
            serde_json::json!({"type":"object","properties":{}}),
            "Noop",
            move |_call_id, args| {
                let observed = observed_for_tool.clone();
                async move {
                    *observed.lock().unwrap() = Some(args);
                    hand_agent::types::ToolResult::text("recorded")
                }
            },
        );

        let mut session = AgentSession::in_memory_with_client(
            openai_test_model(),
            vec![recorder_tool],
            client,
        );

        let ext = RecordingExt::new(
            "rewriter",
            HookDecision::Replace(serde_json::json!({"replaced": true})),
        );
        session.register_extension(ext.clone());

        session
            .send_message("call noop")
            .await
            .expect("send_message ok");

        let captured = observed.lock().unwrap().clone();
        assert_eq!(
            captured,
            Some(serde_json::json!({"original": true})),
            "with replace_args removed upstream, tool observes the model's original args"
        );
    }
}
