//! Agent session — lifecycle management for the coding agent.
//!
//! Ties together the agent loop, session persistence, settings, compaction,
//! and system prompt generation into a high-level session object.

use crate::core::compaction;
use crate::core::error::CodingAgentError;
use crate::core::extensions::api::{
    Extension, ExtensionContext, ExtensionContextFactory, HookDecision, SlashCommandSpec,
    ToolCallEvent, ToolResultEvent, TurnEndEvent, UserMessageEvent,
};
#[cfg(test)]
use crate::core::extensions::api::{ResultDecision, UserMessageOutcome};
use crate::core::extensions::dispatch::{
    MAX_TURN_END_CONTINUATIONS, dispatch_after_tool_call, dispatch_before_tool_call,
    dispatch_turn_end, dispatch_user_message,
};
use crate::core::extensions::registry::builtin_tier1_extensions;
use crate::core::model_registry::ModelRegistry;
use crate::core::session_manager::{SessionBackend, SessionEntry, SessionManager};
use crate::core::settings::SettingsManager;
use crate::core::skills::{self, Skill, SkillError};
use crate::core::system_prompt::{self, BuildSystemPromptOptions};
use crate::rpc::types::{ForkMessageEntry, QueueMode};
use hand_agent::types::{
    AfterToolCallContext, AfterToolCallResult, AgentContext, AgentEvent, AgentLoopConfig,
    AgentTool, BeforeToolCallContext, BeforeToolCallResult, BoxFuture,
};
use hand_agent::{AgentEventSink, CancellationToken, agent_loop};
use model::types::{ImageContent, UserContent};
use model::{Message, SimpleStreamOptions, UserContentBlock};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
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
    /// Session metadata changed — currently just the display name (label).
    /// Subscribers (extensions, UI) get notified when
    /// [`AgentSession::set_label`] runs, without having to poll
    /// [`AgentSession::label`].
    SessionInfoChanged { name: Option<String> },
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
    /// When `true`, run with an in-memory ephemeral session — no JSONL
    /// file is written under `.hand/sessions/`. Backs the `--no-session`
    /// CLI flag.
    pub no_session: bool,
    /// When `true`, skip auto-loading project context files (HAND.md,
    /// .hand/context.md). Backs the `--no-context-files` CLI flag.
    pub no_context_files: bool,
    /// Optional override for the session storage directory. When `None`,
    /// sessions land under `<cwd>/.hand/sessions`. Backs the
    /// `--session-dir <dir>` CLI flag.
    pub session_dir: Option<PathBuf>,
    /// When `true`, skip skill discovery entirely. Backs the
    /// `--no-skills` CLI flag.
    pub no_skills: bool,
    /// Extra skill directories explicitly supplied by the user. Each
    /// entry is a path that either holds a `SKILL.md` directly
    /// (treated as a single Project-scope skill) or is a directory
    /// containing per-skill subdirectories. Backs the `--skill <path>`
    /// CLI flag (repeatable). Ignored when `no_skills` is `true`.
    pub extra_skill_dirs: Vec<PathBuf>,
    /// Override for the agent data root (replacement for `~/.hand/agent`).
    ///
    /// When `Some(base)`, sessions land under `base/sessions/<flattened-cwd>/`.
    /// When `None`, the existing default applies (`HAND_HOME` env var, then
    /// `dirs::home_dir().join(".hand/agent")`, then `<cwd>/.hand/sessions`).
    ///
    /// Embedders (Tauri, sandboxed apps) should pass their per-app data
    /// directory (e.g. `app.path().app_data_dir()`) so persistent state
    /// stays inside the host application instead of the user's home.
    ///
    /// `session_dir` (above) still takes precedence when set, since that
    /// flag is an explicit per-session override.
    pub base_dir: Option<PathBuf>,
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
    /// Whether each entry of `extensions` has had `on_load` run, index-
    /// aligned with it. Drives the "exactly once per session" contract in
    /// [`Self::load_extensions`] / [`Self::shutdown_extensions`].
    extensions_loaded: Vec<bool>,
    /// Extensions whose `on_load` failed, as `(name, error)`. They are
    /// dropped from the dispatch chain; hosts surface this the way they
    /// surface `skill_errors`.
    extension_errors: Vec<(String, String)>,
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
    /// In-flight indicator for [`Self::run_bash`]. `true` for the
    /// duration of an active bash invocation, `false` otherwise. RPC
    /// clients and UIs read it to know whether [`Self::abort_bash`]
    /// would have an effect, and tests pin the state transitions
    /// across an abort against it.
    bash_running: Arc<std::sync::atomic::AtomicBool>,
    /// Queue of user messages submitted via the RPC `steer` command.
    /// Drained by the `get_steering_messages` callback at mid-turn
    /// boundaries inside an active agent loop. Held behind an
    /// `Arc<Mutex<>>` so the RPC dispatcher can enqueue from another task
    /// while `send_message` exclusively borrows `&mut self`. The
    /// `steering_mode` field decides how many messages are dequeued per
    /// turn (`OneAtATime` returns at most one; `All` returns the full
    /// queue).
    steering_queue: Arc<Mutex<Vec<Message>>>,
    /// Queue of user messages submitted via the RPC `follow_up` command.
    /// Drained by `get_follow_up_messages` at the end of each turn.
    /// Mirrors `steering_queue` in shape and concurrency.
    follow_up_queue: Arc<Mutex<Vec<Message>>>,
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
        let session_backend: SessionBackend = settings_manager.current().session_backend().into();
        let client = model::Client::new();

        // Create or resume session
        let session_manager = if let Some(session_id) = &config.resume_session {
            // Match the writer side: when no explicit session_dir is
            // configured, sessions live under
            // `~/.hand/agent/sessions/<flattened-cwd>/`. Using the old
            // cwd-relative `.hand/sessions` path here would silently
            // miss every session written by the new layout.
            //
            // Issue #20: tolerate legacy `<cwd>/.hand/sessions/<id>.jsonl`
            // too — older binaries (and runs that fell through the
            // no-HOME path of `default_session_dir`) wrote sessions
            // there, and `--export --resume <id>` was failing for users
            // whose session files predate the home-based layout. Try
            // the primary location first, then the legacy fallback, and
            // surface both paths in the error message when neither
            // resolves.
            let session_dir = config.session_dir.clone().unwrap_or_else(|| {
                SessionManager::default_session_dir_with_base(
                    config.base_dir.as_deref(),
                    &config.cwd,
                )
            });
            // Tolerate a literal `.jsonl` path: `--resume /…/s_xxx.jsonl`
            // (and any other concrete path the user has on disk) must
            // not re-append `.jsonl` and miss the file. If the raw
            // session_id is an existing file path, use it verbatim.
            let as_path = Path::new(session_id.as_str());
            let direct = if as_path.is_file() {
                Some(as_path.to_path_buf())
            } else {
                None
            };
            if session_backend == SessionBackend::Sqlite && direct.is_none() {
                // Sqlite backend: ids resolve inside the session
                // directory's database. An explicit literal path
                // (`direct`) always wins and opens via the jsonl flow
                // below, regardless of the setting.
                SessionManager::open_by_id_in(SessionBackend::Sqlite, &session_dir, session_id)?
            } else {
                let primary = session_dir.join(format!("{}.jsonl", session_id));
                let legacy = config
                    .cwd
                    .join(".hand")
                    .join("sessions")
                    .join(format!("{}.jsonl", session_id));
                // Prefix-match: when the exact `<dir>/<id>.jsonl` does
                // not exist, scan the dir for `*.jsonl` files whose
                // basename starts with the user's value. Restores the
                // `--resume <prefix>` behaviour that regressed (#78)
                // after the new long id format (#76) made the full id
                // tedious to type. Ambiguous matches return None so the
                // caller surfaces the "not found" error rather than
                // silently picking one.
                let prefix_match = |dir: &Path| -> Option<PathBuf> {
                    let entries = std::fs::read_dir(dir).ok()?;
                    let mut candidates: Vec<PathBuf> = entries
                        .flatten()
                        .filter_map(|e| {
                            let name = e.file_name().to_string_lossy().into_owned();
                            if name.starts_with(session_id.as_str()) && name.ends_with(".jsonl") {
                                Some(e.path())
                            } else {
                                None
                            }
                        })
                        .collect();
                    if candidates.len() == 1 {
                        candidates.pop()
                    } else {
                        None
                    }
                };
                let legacy_dir = config.cwd.join(".hand").join("sessions");
                let resolved = if let Some(p) = direct {
                    p
                } else if primary.exists() {
                    primary
                } else if legacy.exists() && legacy != primary {
                    legacy
                } else if let Some(p) = prefix_match(&session_dir) {
                    p
                } else if let Some(p) = prefix_match(&legacy_dir) {
                    p
                } else {
                    // Nothing matched. Surface both attempted locations
                    // plus a hint that id-prefix lookup was also tried.
                    return Err(CodingAgentError::Session(format!(
                        "Session \"{session_id}\" not found. Looked in:\n  - {primary}\n  - {legacy}\n  (also tried matching as an id prefix)",
                        primary = primary.display(),
                        legacy = legacy.display(),
                    )));
                };
                SessionManager::open(&resolved)?
            }
        } else if config.no_session {
            // --no-session: pure in-memory, no JSONL file under
            // .hand/sessions.
            SessionManager::in_memory()
        } else if let Some(dir) = &config.session_dir {
            SessionManager::create_in_with_backend(session_backend, &config.cwd, dir)?
        } else if let Some(base) = &config.base_dir {
            let dir =
                SessionManager::default_session_dir_with_base(Some(base.as_path()), &config.cwd);
            SessionManager::create_in_with_backend(session_backend, &config.cwd, &dir)?
        } else {
            SessionManager::create_with_backend(session_backend, &config.cwd)?
        };

        // Build tool names for system prompt
        let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();

        // Load context files
        let context_files = if config.no_context_files {
            Vec::new()
        } else {
            system_prompt::load_context_files(&config.cwd)
        };

        // Discover skills (project + user + optional builtin). Skipped
        // entirely when --no-skills is set so the system prompt stays
        // reproducible across machines with different dotfile contents.
        let (skills_discovered, skill_errors): (Vec<Skill>, Vec<SkillError>) = if config.no_skills {
            (Vec::new(), Vec::new())
        } else {
            let (mut skills, mut errors) =
                skills::discover_skills(&config.cwd, user_dir, builtin_dir);
            // CLI-supplied --skill <path> entries. Each path can be
            // either a single-skill dir (contains SKILL.md) or a
            // parent dir of per-skill subdirectories. Load both
            // shapes by adding the parent-of-dir as a root when the
            // path's SKILL.md exists, otherwise treat the path as
            // the parent.
            for explicit in &config.extra_skill_dirs {
                let (extra, errs) = skills::discover_explicit_skill_path(explicit);
                skills.extend(extra);
                errors.extend(errs);
            }
            (skills, errors)
        };

        // Build system prompt
        let system_prompt = system_prompt::build_system_prompt(BuildSystemPromptOptions {
            cwd: &config.cwd,
            tools: &tool_names,
            tool_snippets: None,
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

        // Bind on-disk auth storage so credentials saved via `/login` are
        // visible to provider request resolution. Falls back to an
        // unbound registry when the home dir can't be resolved (the
        // env-var fallback path still works).
        let model_registry = match crate::core::auth_storage::AuthStorage::new() {
            Ok(auth) => ModelRegistry::create(auth),
            Err(_) => ModelRegistry::build(&client),
        };
        let extensions = builtin_tier1_extensions();
        let extensions_loaded = vec![false; extensions.len()];
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
            extensions,
            extensions_loaded,
            extension_errors: Vec::new(),
            model_registry,
            steering_mode: QueueMode::OneAtATime,
            follow_up_mode: QueueMode::OneAtATime,
            auto_compaction_enabled: true,
            auto_retry_enabled: true,
            is_streaming: false,
            is_compacting: false,
            cancel: Arc::new(Mutex::new(CancellationToken::new())),
            bash_cancel: Arc::new(Mutex::new(CancellationToken::new())),
            bash_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            steering_queue: Arc::new(Mutex::new(Vec::new())),
            follow_up_queue: Arc::new(Mutex::new(Vec::new())),
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
                no_session: true,
                no_context_files: true,
                session_dir: None,
                no_skills: true,
                extra_skill_dirs: Vec::new(),
                base_dir: None,
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
            extensions_loaded: Vec::new(),
            extension_errors: Vec::new(),
            model_registry,
            steering_mode: QueueMode::OneAtATime,
            follow_up_mode: QueueMode::OneAtATime,
            auto_compaction_enabled: true,
            auto_retry_enabled: true,
            is_streaming: false,
            is_compacting: false,
            cancel: Arc::new(Mutex::new(CancellationToken::new())),
            bash_cancel: Arc::new(Mutex::new(CancellationToken::new())),
            bash_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            steering_queue: Arc::new(Mutex::new(Vec::new())),
            follow_up_queue: Arc::new(Mutex::new(Vec::new())),
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
        self.send_message_with_images(text, None).await
    }

    /// Send a user message that may carry images, then run the agent loop.
    /// [`Self::send_message`] is the text-only shorthand (`images = None`);
    /// `build_user_message(text, None)` is identical to a plain text message.
    pub async fn send_message_with_images(
        &mut self,
        text: &str,
        images: Option<Vec<ImageContent>>,
    ) -> Result<Vec<Message>, CodingAgentError> {
        // Give every extension its one-time setup before any hook can fire.
        // Idempotent, so this is a no-op on every turn after the first.
        self.load_extensions().await;

        // Let extensions see the prompt before the transcript does: a
        // `Replace` rewrites what both the transcript and the model
        // receive, a `Cancel` aborts the turn. Cancelling here leaves no
        // state to unwind — the turn has not started, `is_streaming` is
        // still false, and nothing has been persisted.
        let (text, extension_contexts) = self.dispatch_user_message_hook(text).await?;
        let user_msg = Message::User(build_user_message(&text, images));

        // Context an extension contributed goes in front of the prompt as
        // its own message, and is persisted like any other: the transcript
        // must show what the model actually read, or a resume would replay
        // a different conversation than the one that happened.
        let context_msg = build_extension_context_message(&extension_contexts);

        let mut prompts = Vec::with_capacity(2);
        if let Some(msg) = context_msg {
            self.session_manager.append_message(msg.clone())?;
            prompts.push(msg);
        }

        // Persist the user message
        self.session_manager.append_message(user_msg.clone())?;
        prompts.push(user_msg);
        // Capture the prompt count before `prompts` is moved into the
        // agent loop below — used to skip-already-persisted entries
        // when writing back `result.messages`.
        let prompts_len = prompts.len();

        // Mark the session as streaming for the duration of this turn.
        // Restored on the happy path below; on cancel/panic the field
        // stays `true` until the next turn or `reset_session()`. We
        // accept this minor staleness — RPC callers that observe a
        // cancelled session will reconcile via `get_state` after their
        // own retry/abort logic, and the field has no safety impact.
        self.is_streaming = true;

        // Filled by the event sink below, read by the follow-up callback.
        let last_turn: Arc<Mutex<Option<TurnEndEvent>>> = Arc::new(Mutex::new(None));

        // Snapshot the extension chain and per-session context so the hook
        // closures can own them as `'static` data captured by the `Box<dyn Fn>`.
        // Cloning the `Vec<Arc<...>>` is cheap (Arc bumps).
        let (before_hook, after_hook) = if self.extensions.is_empty() {
            (None, None)
        } else {
            let extensions: Arc<Vec<Arc<dyn Extension>>> = Arc::new(self.extensions.clone());
            let cx = Arc::new(self.extension_context_factory());
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
        // Mode is honored caller-side inside the `get_steering_messages` /
        // `get_follow_up_messages` closures via `drain_queue`. The agent
        // loop does not consult `loop_config.{steering,follow_up}_mode`, so
        // we deliberately leave them at their defaults to avoid a misleading
        // signal that the runtime enforces the mode.

        // Wire steering / follow-up queues into the agent loop. The queues
        // live on `self` behind `Arc<Mutex<Vec<Message>>>`; clone the Arcs
        // into the callbacks so the dispatcher can enqueue concurrently
        // while `send_message` exclusively borrows `&mut self`.
        //
        // The queue mode is snapshotted by value here. Within a single
        // `send_message`, `set_steering_mode` is deferred by the dispatcher
        // (it requires `&mut self`), so the snapshot is effectively the
        // live mode for every drain in this turn.
        let steering_queue = self.steering_queue.clone();
        let steering_mode = self.steering_mode;
        loop_config.get_steering_messages =
            Some(Arc::new(move || -> BoxFuture<'static, Vec<Message>> {
                let queue = steering_queue.clone();
                let mode = steering_mode;
                Box::pin(async move { drain_queue(&queue, mode) })
            }));

        // The agent loop asks for follow-up messages at the one point
        // where it would otherwise stop, which is exactly the turn
        // boundary `on_turn_end` describes. The queue keeps priority: a
        // user who typed while the agent worked is answered before any
        // extension gets to argue that it should keep going.
        let follow_up_queue = self.follow_up_queue.clone();
        let follow_up_mode = self.follow_up_mode;
        let turn_end_extensions = turn_end_hook_extensions(&self.extensions);
        let turn_end_cx = Arc::new(self.extension_context_factory());
        let turn_end_snapshot = Arc::clone(&last_turn);
        let continuations = Arc::new(AtomicUsize::new(0));
        loop_config.get_follow_up_messages =
            Some(Arc::new(move || -> BoxFuture<'static, Vec<Message>> {
                let queue = follow_up_queue.clone();
                let mode = follow_up_mode;
                let extensions = turn_end_extensions.clone();
                let cx = turn_end_cx.clone();
                let last_turn = turn_end_snapshot.clone();
                let continuations = continuations.clone();
                Box::pin(async move {
                    let queued = drain_queue(&queue, mode);
                    if !queued.is_empty() || extensions.is_empty() {
                        return queued;
                    }

                    if continuations.load(Ordering::SeqCst) >= MAX_TURN_END_CONTINUATIONS {
                        tracing::warn!(
                            bound = MAX_TURN_END_CONTINUATIONS,
                            "on_turn_end kept the agent working up to the re-entry bound; \
                             letting the turn end"
                        );
                        return Vec::new();
                    }

                    let event = last_turn.lock().unwrap().clone().unwrap_or_default();
                    match dispatch_turn_end(&extensions, &cx, &event).await {
                        HookDecision::Cancel(reason) => {
                            continuations.fetch_add(1, Ordering::SeqCst);
                            // The reason is the instruction: the loop
                            // resumes with it as the next user turn, which
                            // is what makes it visible to the model.
                            vec![Message::User(build_user_message(&reason, None))]
                        }
                        _ => Vec::new(),
                    }
                })
            }));

        // Create event sink for the agent loop. Replace the session's
        // cancellation token with a fresh one so a previous turn's
        // `abort()` can't poison this turn — and so the new token is
        // observable by `Self::abort()` while this turn is running.
        //
        // The sink also snapshots each `TurnEnd` so the follow-up callback
        // can describe the finished turn: `GetFollowUpMessagesFn` takes no
        // arguments, so the event is the only way that data reaches it.
        let emit = {
            let base = self.build_event_sink();
            let last_turn = Arc::clone(&last_turn);
            Arc::new(move |event: AgentEvent| {
                if let AgentEvent::TurnEnd { message, .. } = &event
                    && let Message::Assistant(assistant) = message
                {
                    *last_turn.lock().unwrap() = Some(TurnEndEvent {
                        last_assistant_message: assistant_text(assistant),
                        stop_reason: format!("{:?}", assistant.stop_reason),
                    });
                }
                base(event);
            }) as AgentEventSink
        };
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
        let tools_ref: &[AgentTool] = guard.tools.as_deref().expect("guard tools set above");

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

        // Persist new messages produced by the loop, skipping the
        // prompts we already appended upfront at the top of this
        // method. `result.messages` is `[prompts.., assistant_msgs..]`
        // (see `run_agent_loop` in `agent_loop.rs`), so re-appending
        // the first `prompts.len()` entries would double-persist every
        // user turn — issue #19 surfaced as the user's text appearing
        // twice in `--export` output for each prompt sent.
        for msg in result.messages.iter().skip(prompts_len) {
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

    /// Mutably borrow the underlying [`SessionManager`]. `pub(crate)`
    /// because the dispatcher unit tests seed JSONL entries directly
    /// via `append_message` (capturing the assigned IDs to drive
    /// fork/clone cases) — full `send_message` round-trips are too
    /// heavy for that setup. Outside of `coding-agent` the abstraction
    /// stays sealed.
    #[cfg(test)]
    pub(crate) fn session_manager_mut(&mut self) -> &mut SessionManager {
        &mut self.session_manager
    }

    /// Get the current model.
    pub fn model(&self) -> &model::Model {
        &self.config.model
    }

    /// Set the model.
    ///
    /// Persists a `ModelChange` entry to the session journal so a
    /// resume picks up the user's switch instead of replaying with the
    /// session's original model. Without this, switching from
    /// `claude-haiku` to `gpt-4o` mid-session would silently revert to
    /// claude on every `--continue`.
    ///
    /// Errors from the journal append are intentionally swallowed
    /// (with a tracing warn) so a write failure doesn't prevent the
    /// in-memory model from being updated — the user's next prompt
    /// still goes to the new model, just without an audit trail.
    pub fn set_model(&mut self, model: model::Model) {
        let provider = model.provider.as_str().to_string();
        let model_id = model.id.clone();
        self.config.model = model;
        if let Err(err) = self
            .session_manager
            .append_model_change(&provider, &model_id)
        {
            tracing::warn!(error = %err, provider = %provider, model = %model_id,
                "failed to append model_change entry; in-memory switch still applied");
        }
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
            SessionManager::create_with_backend(self.session_manager.backend(), &self.config.cwd)?
        };
        self.session_manager = new_sm;
        self.context.messages.clear();
        // Runtime flags are conversation-scoped; reset alongside the
        // messages so `is_streaming`/`is_compacting` don't bleed over
        // from a cancelled prior turn.
        self.is_streaming = false;
        self.is_compacting = false;
        // Drop any queued steer/follow-up messages — they belonged to
        // the prior conversation and would leak into the fresh one on
        // the next turn boundary.
        self.steering_queue.lock().unwrap().clear();
        self.follow_up_queue.lock().unwrap().clear();
        Ok(())
    }

    /// Fork the session at the given JSONL entry id. The new session's
    /// history contains every entry strictly BEFORE the entry identified
    /// by `entry_id`; the entry itself and everything after it are
    /// dropped. Mirrors the TS `agent-session-runtime.ts` `fork()` with
    /// `position == "before"` (the default; the only mode the RPC
    /// surface exposes).
    ///
    /// The session manager is replaced with a fresh on-disk JSONL file
    /// (or in-memory if the parent was in-memory). Listeners,
    /// extensions, tools, client, model, settings, and skills are
    /// preserved — same preservation contract as
    /// [`Self::reset_session`].
    ///
    /// Returns the text content of the forked-from user message so the
    /// RPC handler can echo it on the wire (matches TS
    /// `selectedText` semantics).
    ///
    /// Errors:
    /// - `CodingAgentError::Session("entry_id not found: …")` if no
    ///   entry has the given id.
    /// - `CodingAgentError::Session("entry_id is not a user message …")`
    ///   if the entry exists but is not a user message — TS rejects
    ///   the same way under default `position`.
    pub fn fork(&mut self, entry_id: &str) -> Result<String, CodingAgentError> {
        let entries = self.session_manager.entries();

        // Find the entry by id and capture its index + the user-message
        // text. Reject non-user-message entries to match TS's default
        // `position == "before"` validation.
        let mut found: Option<(usize, String)> = None;
        for (idx, entry) in entries.iter().enumerate() {
            if let SessionEntry::Message { id, message, .. } = entry
                && id == entry_id
            {
                let Message::User(user_msg) = message.as_ref() else {
                    return Err(CodingAgentError::Session(format!(
                        "entry_id is not a user message: {entry_id}"
                    )));
                };
                let text = extract_user_message_text(&user_msg.content);
                found = Some((idx, text));
                break;
            }
        }
        let (cut_idx, text) = found
            .ok_or_else(|| CodingAgentError::Session(format!("entry_id not found: {entry_id}")))?;

        // Truncated body: every entry strictly BEFORE the cut point,
        // skipping any session header (the new manager generates its
        // own).
        let body: Vec<SessionEntry> = entries[..cut_idx]
            .iter()
            .filter(|e| !matches!(e, SessionEntry::Session(_)))
            .cloned()
            .collect();

        self.replace_session_with_body(body)?;
        Ok(text)
    }

    /// Clone the session: produce a fresh session_manager carrying a
    /// complete copy of the current session's body entries. Same
    /// preservation contract as [`Self::fork`] / [`Self::reset_session`].
    /// Mirrors the TS pattern in `interactive-mode.ts::handleCloneCommand`,
    /// which calls `runtimeHost.fork(leafId, { position: "at" })` —
    /// effectively "fork at the leaf with everything included".
    pub fn clone_session(&mut self) -> Result<(), CodingAgentError> {
        let body: Vec<SessionEntry> = self
            .session_manager
            .entries()
            .iter()
            .filter(|e| !matches!(e, SessionEntry::Session(_)))
            .cloned()
            .collect();
        self.replace_session_with_body(body)
    }

    /// Shared replacement path used by [`Self::fork`] and
    /// [`Self::clone_session`]: build a fresh session manager that
    /// adopts `body` verbatim, swap it in, rebuild the in-memory
    /// message context, and reset queues + runtime flags. The new
    /// session's `parent_session` header points at the prior session's
    /// id (provenance, matching `SessionManager::fork_from`).
    fn replace_session_with_body(
        &mut self,
        body: Vec<SessionEntry>,
    ) -> Result<(), CodingAgentError> {
        let parent_id = self.session_manager.id().to_string();
        let new_sm = SessionManager::from_branched_entries_with_backend(
            self.session_manager.backend(),
            &self.config.cwd,
            self.session_manager.is_in_memory(),
            Some(&parent_id),
            body,
        )?;
        self.adopt_session_manager(new_sm);
        Ok(())
    }

    /// Replace the active session in place with one loaded from the
    /// given JSONL path. Same preservation/reset contract as
    /// [`Self::fork`] / [`Self::clone_session`]: listeners, extensions,
    /// tools, client, model, settings, and skills are preserved;
    /// `is_streaming` / `is_compacting` and the steer / follow-up
    /// queues are reset.
    ///
    /// Unlike fork/clone, switch_session adopts the loaded file's
    /// session id verbatim — it is a *switch*, not a branch. The prior
    /// session file is left intact on disk. Mirrors the TS reference
    /// `agent-session-runtime.ts::switchSession` (with `position` /
    /// extension hooks deferred — those land with the extension API).
    ///
    /// Returns `Err` if the file is missing or malformed (no session
    /// header).
    pub fn switch_session(&mut self, path: &Path) -> Result<(), CodingAgentError> {
        let new_sm = SessionManager::open(path)?;
        self.adopt_session_manager(new_sm);
        Ok(())
    }

    /// [`Self::switch_session`] addressed by session id instead of
    /// file path, resolved through the active backend in `cwd`'s
    /// default session directory. The `/resume` picker uses this under
    /// the sqlite backend, where every session shares one database
    /// path and only the id identifies the session.
    pub fn switch_session_by_id(&mut self, id: &str) -> Result<(), CodingAgentError> {
        let dir = SessionManager::default_session_dir(&self.config.cwd);
        let new_sm = SessionManager::open_by_id_in(self.session_manager.backend(), &dir, id)?;
        self.adopt_session_manager(new_sm);
        Ok(())
    }

    /// Storage backend of the active session manager.
    pub fn session_backend(&self) -> SessionBackend {
        self.session_manager.backend()
    }

    /// Adopt `new_sm` as the active session manager: rebuild the
    /// in-memory message context from it, swap it in, and reset the
    /// runtime flags + queues. Shared between `replace_session_with_body`
    /// (fork / clone) and [`Self::switch_session`].
    fn adopt_session_manager(&mut self, new_sm: SessionManager) {
        self.context.messages = new_sm.build_context();
        self.session_manager = new_sm;
        self.is_streaming = false;
        self.is_compacting = false;
        self.steering_queue.lock().unwrap().clear();
        self.follow_up_queue.lock().unwrap().clear();
    }

    /// User-message-only summary of the current session's entries,
    /// ordered chronologically. Each item carries the JSONL `entry_id`
    /// (usable as `fork.entryId`) and the concatenated text content.
    /// Empty-text entries are filtered out (parity with the TS
    /// `getUserMessagesForForking`'s `if (text)` guard, which skips
    /// image-only user messages).
    pub fn fork_messages(&self) -> Vec<ForkMessageEntry> {
        self.session_manager
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                SessionEntry::Message { id, message, .. } => match message.as_ref() {
                    Message::User(user) => {
                        let text = extract_user_message_text(&user.content);
                        if text.is_empty() {
                            None
                        } else {
                            Some(ForkMessageEntry {
                                entry_id: id.clone(),
                                text,
                            })
                        }
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    /// Get the working directory.
    pub fn cwd(&self) -> &Path {
        &self.config.cwd
    }

    /// Get the current context messages.
    pub fn messages(&self) -> &[Message] {
        &self.context.messages
    }

    /// Replace the in-memory context messages. Used to restore a previously
    /// persisted transcript into this (per-connection) session so a follow-up
    /// turn carries that history. Does not touch the session manager's stored
    /// file; callers seeding an ephemeral session own persistence separately.
    pub fn set_messages(&mut self, messages: Vec<Message>) {
        self.context.messages = messages;
    }

    /// Get the settings manager.
    pub fn settings(&self) -> &SettingsManager {
        &self.settings_manager
    }

    /// Borrow the session-storage manager for read-only inspection
    /// (entries, timestamps, etc.). Used by `/session` to surface the
    /// session-start timestamp without re-reading the JSONL.
    pub fn session_manager(&self) -> &SessionManager {
        &self.session_manager
    }

    /// Mutable settings access. Required by the M5.4 startup flow to
    /// record `last_changelog_version` after auto-displaying entries.
    /// Callers that mutate must invoke `save(scope)` themselves to
    /// persist the change.
    pub fn settings_mut(&mut self) -> &mut SettingsManager {
        &mut self.settings_manager
    }

    /// Re-read settings from disk and swap them into the running
    /// session. Used by `/reload` to pick up out-of-band edits to
    /// `~/.hand/agent/settings.yaml` and `<cwd>/.hand/settings.yaml`
    /// without restarting. Pre-fix the driver constructed a new
    /// `SettingsManager` and immediately dropped it, leaving the
    /// session's own manager untouched and the user staring at stale
    /// values.
    ///
    /// Note: fields that are read once at session construction
    /// (resolved model, initial system prompt) do NOT change live —
    /// `/reload` only refreshes settings that downstream code consults
    /// on demand (compaction thresholds, theme, quiet-startup, etc.).
    pub fn reload_settings(&mut self) -> Result<(), CodingAgentError> {
        let fresh = SettingsManager::from_cwd(&self.config.cwd)
            .map_err(|e| CodingAgentError::Settings(e.to_string()))?;
        self.settings_manager = fresh;
        Ok(())
    }

    /// Manually trigger compaction unconditionally — bypasses both the
    /// `auto_compaction_enabled` toggle and the
    /// `compaction::should_compact` token-threshold gate. Returns the
    /// summary string the agent generated (or a structured fallback if
    /// the LLM call failed). Used by the RPC `compact` handler and the
    /// `/compact` slash command, where the user is explicitly asking
    /// for compaction regardless of session state.
    pub async fn compact(&mut self) -> Result<String, CodingAgentError> {
        self.do_compact(None).await
    }

    /// Compaction variant that lets the caller steer the summarizer
    /// with custom instructions — e.g. `/compact focus on the
    /// database schema changes`. The instructions are folded into the
    /// summary prompt prefix so the model retains them under the
    /// caller's frame.
    pub async fn compact_with(&mut self, instructions: &str) -> Result<String, CodingAgentError> {
        let trimmed = instructions.trim();
        let custom = (!trimmed.is_empty()).then(|| trimmed.to_string());
        self.do_compact(custom).await
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
        let _ = self.do_compact(None).await?;
        Ok(())
    }

    /// Run the compaction summarizer + record the result on the session
    /// manager. Returns the summary text. Caller is responsible for any
    /// gating (auto-toggle, token threshold).
    async fn do_compact(
        &mut self,
        custom_instructions: Option<String>,
    ) -> Result<String, CodingAgentError> {
        let settings = self.settings_manager.compaction_settings();

        self.is_compacting = true;
        self.emit(AgentSessionEvent::CompactionStart);

        let (to_compact, _to_keep, _split_idx) = compaction::split_for_compaction(
            &self.context.messages,
            settings.keep_recent_tokens() as usize,
        );

        let file_ops = compaction::extract_file_operations(&to_compact);
        let summary_prompt = compaction::build_compaction_prompt_with(
            &to_compact,
            &file_ops,
            custom_instructions.as_deref(),
        );

        let summary = match self.generate_compaction_summary(&summary_prompt).await {
            Ok(s) => s,
            Err(_) => format!(
                "[Compacted {} messages. Files read: {}. Files edited: {}.]",
                to_compact.len(),
                file_ops
                    .read
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", "),
                file_ops
                    .edited
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        };

        let first_kept_id = format!("compaction_{}", chrono::Utc::now().timestamp_millis());
        let appended = self
            .session_manager
            .append_compaction(&summary, &first_kept_id);

        // Clear before the `?` and before the end event, for two
        // reasons. A failed append used to leave the flag set for the
        // rest of the session, so `get_state` reported a compaction that
        // was long over. And a listener reacting to the end event by
        // submitting a queued prompt has to observe an idle session, not
        // the one it is being notified about the end of.
        self.is_compacting = false;
        appended?;

        self.emit(AgentSessionEvent::CompactionEnd {
            summary: summary.clone(),
        });

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
    ///
    /// Persists the new label as a `Label` entry in the session JSONL
    /// and emits [`AgentSessionEvent::SessionInfoChanged`] so
    /// subscribers (extensions, UI, RPC clients) see the change
    /// immediately without polling.
    pub fn set_label(&mut self, label: &str) -> Result<(), CodingAgentError> {
        self.session_manager.append_label(label)?;
        let new_name = self.session_manager.label().map(|s| s.to_string());
        self.emit(AgentSessionEvent::SessionInfoChanged { name: new_name });
        Ok(())
    }

    /// Get message count.
    pub fn message_count(&self) -> usize {
        self.session_manager.message_count()
    }

    /// Push a user message onto the steering queue. Returns the new
    /// queue length (post-push). Wakes nothing — the agent loop
    /// observes the queue at the next turn boundary inside the active
    /// prompt run via the `get_steering_messages` callback. If no prompt
    /// is in flight the message just waits there until the next
    /// `send_message` starts (it will be drained as soon as the loop
    /// polls `get_steering_messages`).
    pub fn enqueue_steer(&self, text: &str, images: Option<Vec<ImageContent>>) -> usize {
        let msg = Message::User(build_user_message(text, images));
        let mut queue = self.steering_queue.lock().unwrap();
        queue.push(msg);
        queue.len()
    }

    /// Push a user message onto the follow-up queue. Returns the new
    /// queue length. Picked up at the end of the current turn (or the
    /// start of the next idle turn) via `get_follow_up_messages`.
    pub fn enqueue_follow_up(&self, text: &str, images: Option<Vec<ImageContent>>) -> usize {
        let msg = Message::User(build_user_message(text, images));
        let mut queue = self.follow_up_queue.lock().unwrap();
        queue.push(msg);
        queue.len()
    }

    /// Total queued user messages across the steer + follow-up queues.
    /// Surfaced via `get_state.pending_message_count`.
    pub fn pending_message_count(&self) -> u64 {
        let steer = self.steering_queue.lock().unwrap().len() as u64;
        let follow_up = self.follow_up_queue.lock().unwrap().len() as u64;
        steer + follow_up
    }

    /// Shared handle to the steering queue. Used by the RPC dispatcher
    /// to enqueue messages while `send_message` exclusively borrows
    /// `&mut self`. The dispatcher must clone this handle BEFORE driving
    /// `send_message` — once `&mut self` is taken, the session itself is
    /// unreachable for the duration of the call.
    pub fn steering_queue_handle(&self) -> Arc<Mutex<Vec<Message>>> {
        self.steering_queue.clone()
    }

    /// Shared handle to the follow-up queue. See
    /// [`Self::steering_queue_handle`] for the same dispatcher-side
    /// concurrency caveat.
    pub fn follow_up_queue_handle(&self) -> Arc<Mutex<Vec<Message>>> {
        self.follow_up_queue.clone()
    }

    /// Shared handle to the cancellation token used by [`Self::abort`].
    /// The dispatcher clones this BEFORE driving `send_message` so it can
    /// flip the token mid-flight (the prompt's `&mut self` borrow makes
    /// `session.abort()` itself unreachable during the race). Cancelling
    /// via this handle has identical semantics to [`Self::abort`].
    pub fn cancel_handle(&self) -> Arc<Mutex<CancellationToken>> {
        self.cancel.clone()
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

    /// Whether a [`Self::run_bash`] call is currently in flight.
    /// Useful for RPC clients and UIs that want to disable the abort
    /// button when no command is running, and for tests that pin the
    /// state transitions across completion / abort.
    ///
    /// The flag is set with Release ordering at the entry of `run_bash`
    /// and cleared by an RAII guard, so the value observed here is
    /// monotone within a single executor: false → true → false.
    pub fn is_bash_running(&self) -> bool {
        self.bash_running.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Run a one-off bash command, racing it against [`Self::abort_bash`].
    ///
    /// Replaces the stored `bash_cancel` with a fresh token so a stale
    /// abort can't poison this call, then races the executor future
    /// against the new token's `cancelled()` future via `tokio::select!`.
    /// On cancel, returns a synthesized
    /// [`crate::core::bash_executor::BashResult`] with `truncated: true`,
    /// the abort marker `"[bash aborted]"` on `output`, and
    /// `aborted: true` so the caller can route the marker to `stderr`
    /// on the wire (see [`RunBashOutcome`]). The underlying child
    /// process is killed via [`tokio::process::Command::kill_on_drop`]
    /// — dropping the executor future on the cancel arm reaps the child.
    ///
    /// `timeout_secs` is forwarded to
    /// [`crate::core::bash_executor::BashExecutorOptions::timeout_secs`]
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

        // Flip the in-flight flag inside an RAII guard so it always
        // clears on completion, abort, panic, or future-drop. The
        // `is_bash_running` test expects strict bracketing.
        struct InFlightGuard(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for InFlightGuard {
            fn drop(&mut self) {
                self.0.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        self.bash_running
            .store(true, std::sync::atomic::Ordering::Release);
        let _guard = InFlightGuard(self.bash_running.clone());

        // Prefer the session's `shell_path` setting over the ambient
        // `$SHELL`. Without this, multiple agent sessions running from the
        // same parent process all inherit whatever shell launched the
        // launcher — per-project shellPath in settings.json never takes
        // effect during bash execution.
        let shell_path = self
            .settings_manager
            .shell_path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(crate::core::bash_executor::resolve_shell);
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
                        full_output_path: None,
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
        // `on_load` cannot run here (this is a sync fn); the next
        // `load_extensions` — which `send_message` runs itself — picks the
        // new extension up.
        self.extensions_loaded.push(false);
        self.model_registry = ModelRegistry::build(&self.client);
    }

    /// Run `on_load` for every registered extension that has not been
    /// loaded yet, then mark it loaded.
    ///
    /// Idempotent: an extension is loaded at most once per session, so
    /// calling this repeatedly (as [`Self::send_message`] does) is cheap and
    /// safe. Hosts that want setup to happen before the first turn — to
    /// surface load errors at startup rather than mid-conversation — can
    /// call it directly after construction.
    ///
    /// **A failing `on_load` drops the extension from the chain.** An
    /// extension that could not set itself up would otherwise run degraded,
    /// silently answering `Continue` for hooks it was registered to police.
    /// The failure is logged and recorded in [`Self::extension_errors`]; it
    /// is never fatal to the session.
    ///
    /// Load state is per `AgentSession` instance, not per session file:
    /// `reset_session` and `fork` keep the chain loaded rather than
    /// re-running setup.
    pub async fn load_extensions(&mut self) {
        let pending: Vec<(usize, Arc<dyn Extension>)> = self
            .extensions
            .iter()
            .enumerate()
            .filter(|(idx, _)| !self.extensions_loaded[*idx])
            .map(|(idx, ext)| (idx, ext.clone()))
            .collect();
        if pending.is_empty() {
            return;
        }

        let contexts = self.extension_context_factory();
        let mut failed: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for (idx, ext) in pending {
            let name = ext.manifest().name.clone();
            match ext.on_load(&contexts.for_extension(&name)).await {
                Ok(()) => self.extensions_loaded[idx] = true,
                Err(err) => {
                    tracing::warn!(
                        extension = %name,
                        error = %err,
                        "extension on_load failed; dropping it from the dispatch chain"
                    );
                    self.extension_errors.push((name, err.to_string()));
                    failed.insert(idx);
                }
            }
        }

        if !failed.is_empty() {
            let loaded = std::mem::take(&mut self.extensions_loaded);
            let mut kept_exts = Vec::with_capacity(self.extensions.len() - failed.len());
            let mut kept_loaded = Vec::with_capacity(kept_exts.capacity());
            for (idx, ext) in std::mem::take(&mut self.extensions).into_iter().enumerate() {
                if failed.contains(&idx) {
                    continue;
                }
                kept_exts.push(ext);
                kept_loaded.push(loaded[idx]);
            }
            self.extensions = kept_exts;
            self.extensions_loaded = kept_loaded;
            self.model_registry = ModelRegistry::build(&self.client);
        }
    }

    /// Run `on_shutdown` for every loaded extension, in reverse
    /// registration order (teardown mirrors setup), then mark them
    /// unloaded.
    ///
    /// Idempotent, and errors are logged rather than propagated — teardown
    /// must not fail. Hosts call this before dropping or replacing a
    /// session; [`crate::core::agent_session_runtime::AgentSessionRuntime::dispose`]
    /// does it for them. Tier 2 children are killed here rather than
    /// lingering until the host process exits.
    pub async fn shutdown_extensions(&mut self) {
        let loaded: Vec<Arc<dyn Extension>> = self
            .extensions
            .iter()
            .enumerate()
            .filter(|(idx, _)| self.extensions_loaded[*idx])
            .map(|(_, ext)| ext.clone())
            .rev()
            .collect();
        if loaded.is_empty() {
            return;
        }

        let contexts = self.extension_context_factory();
        for ext in loaded {
            let name = ext.manifest().name.clone();
            if let Err(err) = ext.on_shutdown(&contexts.for_extension(&name)).await {
                tracing::warn!(
                    extension = %name,
                    error = %err,
                    "extension on_shutdown failed; continuing teardown"
                );
            }
        }
        self.extensions_loaded.iter_mut().for_each(|f| *f = false);
    }

    /// Run the `on_user_message` chain and resolve it to the prompt text
    /// the turn should actually use, plus any context extensions want in
    /// front of the model.
    ///
    /// Returns `Err` when an extension cancelled the turn — the caller must
    /// not persist the message or start the agent loop.
    async fn dispatch_user_message_hook(
        &self,
        text: &str,
    ) -> Result<(String, Vec<(String, String)>), CodingAgentError> {
        if self.extensions.is_empty() {
            return Ok((text.to_string(), Vec::new()));
        }
        let event = UserMessageEvent {
            text: text.to_string(),
        };
        let resolution =
            dispatch_user_message(&self.extensions, &self.extension_context_factory(), &event)
                .await;
        let resolved = match resolution.decision {
            HookDecision::Continue => text.to_string(),
            HookDecision::Replace(value) => value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| text.to_string()),
            HookDecision::Cancel(reason) => {
                return Err(CodingAgentError::Other(format!(
                    "message cancelled by extension: {reason}"
                )));
            }
        };
        Ok((resolved, resolution.contexts))
    }

    /// Extensions that failed `on_load`, as `(name, error)`. Populated by
    /// [`Self::load_extensions`]; the listed extensions are no longer in
    /// the dispatch chain.
    pub fn extension_errors(&self) -> &[(String, String)] {
        &self.extension_errors
    }

    /// Aggregate model catalog for this session.
    ///
    /// Built eagerly at construction; rebuilt on
    /// [`Self::register_extension`]. The returned reference is stable until
    /// the next mutation that triggers a rebuild.
    pub fn model_registry(&self) -> &ModelRegistry {
        &self.model_registry
    }

    /// Re-snapshot the model registry's catalog: reload `models.json` plus
    /// the in-process catalog (which a background
    /// `model::refresh_from_remote` may have hot-swapped mid-session), and
    /// replay registered providers on top. Synchronous local-disk reload
    /// only — never the network. See [`ModelRegistry::refresh`].
    pub fn refresh_model_registry(&mut self) {
        self.model_registry.refresh();
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
        let contexts = self.extension_context_factory();
        let mut out = Vec::new();
        for ext in &self.extensions {
            out.extend(ext.custom_tools(&contexts.for_extension(&ext.manifest().name)));
        }
        out
    }

    /// Factory for the per-extension [`ExtensionContext`] values handed to
    /// this session's hooks, custom tools, and slash commands.
    ///
    /// The data root is `<base_dir>/extensions` when the host pinned a
    /// [`AgentSessionConfig::base_dir`] — a GUI embedder keeps extension
    /// state in its own app-data directory rather than inside whatever
    /// repository the user pointed the agent at — and `<cwd>/.hand/extensions`
    /// otherwise, which is where the CLI has always put it.
    pub fn extension_context_factory(&self) -> ExtensionContextFactory {
        let root = match &self.config.base_dir {
            Some(base) => base.clone(),
            None => self.config.cwd.join(".hand"),
        };
        ExtensionContextFactory::new(
            self.config.cwd.clone(),
            self.session_manager.id().to_string(),
            root.join("extensions"),
        )
    }

    /// Build the [`ExtensionContext`] for one named extension. Shorthand for
    /// `extension_context_factory().for_extension(name)`.
    pub fn extension_context_for(&self, name: &str) -> ExtensionContext {
        self.extension_context_factory().for_extension(name)
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

/// Build a [`model::UserMessage`] for the steer / follow-up queues.
///
/// `pub(crate)` so the RPC dispatcher can construct messages directly
/// when enqueuing into a session's queue handle while `send_message`
/// holds `&mut self` (see `rpc/server.rs` Prompt arm).
///
/// Mirrors [`AgentSession::send_message`]'s prompt path: when no images
/// are attached, use [`model::types::UserMessage::new_text`] for the
/// cheap text-only shape; otherwise emit a `Blocks` message with the
/// text first and the images appended. An empty text + non-empty images list still produces
/// a valid `Blocks` payload (the leading text block is always present
/// even when empty, matching the wire shape the model sees for
/// multi-modal user turns).
pub(crate) fn build_user_message(
    text: &str,
    images: Option<Vec<ImageContent>>,
) -> model::UserMessage {
    match images {
        Some(images) if !images.is_empty() => {
            let mut blocks: Vec<UserContentBlock> = Vec::with_capacity(images.len() + 1);
            blocks.push(UserContentBlock::Text(model::TextContent::new(text)));
            for img in images {
                blocks.push(UserContentBlock::Image(img));
            }
            model::UserMessage::new_blocks(blocks)
        }
        _ => model::UserMessage::new_text(text),
    }
}

/// Concatenate the text parts of a user message's content.
///
/// Mirrors the TS reference's `extractUserMessageText` in
/// `agent-session-runtime.ts`: text-only content returns its string
/// directly; block content returns the concatenation of every text
/// block (image blocks are ignored). Used by [`AgentSession::fork`]
/// to echo the forked-from message text on the wire.
fn extract_user_message_text(content: &UserContent) -> String {
    match content {
        UserContent::Text(s) => s.clone(),
        UserContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                UserContentBlock::Text(t) => Some(t.text.as_str()),
                UserContentBlock::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join(""),
    }
}

/// Drain the queue according to the requested delivery mode.
///
/// `OneAtATime` removes and returns the first message; `All` returns the
/// full queue and leaves it empty. Anything not returned stays in the
/// queue for the next drain. Holding the lock across the read+write is
/// safe because the closure body never awaits between `lock()` and
/// release — the drained messages are owned, not lock-borrowed.
fn drain_queue(queue: &Mutex<Vec<Message>>, mode: QueueMode) -> Vec<Message> {
    let mut q = queue.lock().unwrap();
    if q.is_empty() {
        return Vec::new();
    }
    match mode {
        QueueMode::All => std::mem::take(&mut *q),
        QueueMode::OneAtATime => vec![q.remove(0)],
    }
}

/// Build a `BeforeToolCallHook` that fans the tool-call event out to
/// every registered extension and aggregates their decisions.
///
/// The three decisions map onto [`BeforeToolCallResult`] directly:
/// `Continue` returns `None` (nothing to override), `Cancel` blocks the
/// call with the extension's reason, and `Replace` hands the agent loop the
/// rewritten arguments — which the loop re-validates against the tool's
/// schema before executing. The transcript keeps the model's original tool
/// call; the rewrite describes what the host let it do.
fn build_before_tool_call_hook(
    extensions: Arc<Vec<Arc<dyn Extension>>>,
    cx: Arc<ExtensionContextFactory>,
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
                    HookDecision::Replace(args) => Some(BeforeToolCallResult {
                        block: false,
                        reason: None,
                        replace_args: Some(args),
                    }),
                    HookDecision::Cancel(reason) => Some(BeforeToolCallResult {
                        block: true,
                        reason: Some(reason),
                        replace_args: None,
                    }),
                }
            })
        },
    )
}

/// Build an `AfterToolCallHook` that fans the result event out to every
/// registered extension. The result is read-only — extensions cannot
/// rewrite it — so the hook always returns `None`.
/// Concatenated text blocks of an assistant message, which is what an
/// `on_turn_end` hook means by "what the agent just said". Tool-call
/// blocks are skipped — a commit subject derived from them would be
/// noise.
fn assistant_text(message: &model::AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            model::AssistantContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// The subset of the chain that asked for `on_turn_end`, snapshotted as
/// `'static` data the follow-up callback can own. Empty when nothing
/// subscribes, which lets the callback skip the hook entirely.
fn turn_end_hook_extensions(extensions: &[Arc<dyn Extension>]) -> Arc<Vec<Arc<dyn Extension>>> {
    Arc::new(
        extensions
            .iter()
            .filter(|ext| ext.manifest().capabilities.on_turn_end)
            .cloned()
            .collect(),
    )
}

/// Render extension-contributed context as one message placed ahead of the
/// user's prompt, or `None` when nothing was contributed.
///
/// Each contribution is wrapped in a tag naming its extension. Without the
/// attribution the model reads injected text as if the user typed it, which
/// is how an extension's advice ends up being treated as an instruction the
/// user never gave.
fn build_extension_context_message(contexts: &[(String, String)]) -> Option<Message> {
    if contexts.is_empty() {
        return None;
    }
    let mut body = String::new();
    for (name, text) in contexts {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&format!(
            "<extension-context extension=\"{name}\">\n{text}\n</extension-context>"
        ));
    }
    Some(Message::User(build_user_message(&body, None)))
}

fn build_after_tool_call_hook(
    extensions: Arc<Vec<Arc<dyn Extension>>>,
    cx: Arc<ExtensionContextFactory>,
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
                let replacement = dispatch_after_tool_call(&extensions, &cx, &event).await?;
                // The chain hands back a value shaped like the event's
                // `result`, i.e. a serialized ToolResult. Parse it back so
                // a malformed replacement fails here — dropping the
                // override and keeping the tool's own output — rather than
                // reaching the model as a mangled result.
                match serde_json::from_value::<hand_agent::types::ToolResult>(replacement) {
                    Ok(parsed) => Some(AfterToolCallResult {
                        content: Some(parsed.content),
                        details: parsed.details,
                        terminate: parsed.terminate,
                        // `is_error` is not part of the result shape the
                        // event exposes; a hook rewrites what the model
                        // reads, not whether the call counted as failed.
                        is_error: None,
                    }),
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            "extension chain returned an unparseable tool-result replacement; \
                             keeping the original result"
                        );
                        None
                    }
                }
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

    /// `is_bash_running` starts false on a fresh session. The flag
    /// only flips during an active `run_bash` call.
    #[test]
    fn is_bash_running_starts_false() {
        let session = AgentSession::in_memory(test_model(), vec![]);
        assert!(!session.is_bash_running());
    }

    /// `run_bash` flips `is_bash_running` true for the duration of the
    /// call and back to false on completion. A fast `true` command
    /// exercises the bracket without timing races — the RAII guard
    /// ensures the post-state is observed even on cancel or panic,
    /// but the success path is the common case to pin.
    #[tokio::test]
    async fn run_bash_brackets_is_bash_running_flag() {
        let session = AgentSession::in_memory(test_model(), vec![]);
        assert!(!session.is_bash_running());
        let outcome = session.run_bash("exit 0", 5).await.expect("ok");
        assert!(!outcome.aborted);
        assert!(
            !session.is_bash_running(),
            "flag must clear after completion"
        );
    }

    /// `drain_queue` in `OneAtATime` mode pops exactly one message
    /// from the front of the queue, preserving insertion order for
    /// subsequent drains. Used by the steer/follow-up dispatcher
    /// mid-turn to deliver one queued message per agent loop iteration
    /// without stalling on a big batch.
    #[test]
    fn drain_queue_one_at_a_time_pops_front_only() {
        let queue = Mutex::new(vec![
            Message::User(model::UserMessage::new_text("first")),
            Message::User(model::UserMessage::new_text("second")),
            Message::User(model::UserMessage::new_text("third")),
        ]);
        let drained = drain_queue(&queue, crate::rpc::types::QueueMode::OneAtATime);
        assert_eq!(drained.len(), 1, "OneAtATime drains exactly one");
        // Order: drained = ["first"], queue retains ["second", "third"].
        match &drained[0] {
            Message::User(u) => match &u.content {
                model::UserContent::Text(t) => assert_eq!(t, "first"),
                _ => panic!("expected text content"),
            },
            _ => panic!("expected user message"),
        }
        assert_eq!(queue.lock().unwrap().len(), 2);
    }

    /// `drain_queue` in `All` mode drains everything in one shot —
    /// used when the consumer wants to bulk-deliver every queued
    /// message at the next turn boundary.
    #[test]
    fn drain_queue_all_takes_full_queue_in_order() {
        let queue = Mutex::new(vec![
            Message::User(model::UserMessage::new_text("first")),
            Message::User(model::UserMessage::new_text("second")),
            Message::User(model::UserMessage::new_text("third")),
        ]);
        let drained = drain_queue(&queue, crate::rpc::types::QueueMode::All);
        assert_eq!(drained.len(), 3, "All drains everything");
        let texts: Vec<&str> = drained
            .iter()
            .filter_map(|m| match m {
                Message::User(u) => match &u.content {
                    model::UserContent::Text(t) => Some(t.as_str()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["first", "second", "third"]);
        assert!(queue.lock().unwrap().is_empty(), "queue must be empty");
    }

    /// Empty queue is a no-op for both modes — no panic, no spurious
    /// allocations beyond the empty Vec sentinel.
    #[test]
    fn drain_queue_empty_returns_empty() {
        let queue: Mutex<Vec<Message>> = Mutex::new(Vec::new());
        assert!(drain_queue(&queue, crate::rpc::types::QueueMode::All).is_empty());
        assert!(drain_queue(&queue, crate::rpc::types::QueueMode::OneAtATime).is_empty());
    }

    /// `set_label` must emit a `SessionInfoChanged` event so
    /// subscribers see the new name without polling. An earlier
    /// implementation only persisted the label entry to disk;
    /// extensions and UI components had no signal that the display
    /// name had changed.
    #[test]
    fn set_label_emits_session_info_changed() {
        let dir = TempDir::new().unwrap();
        let mut session =
            AgentSession::new(test_config(dir.path().to_path_buf()), vec![]).expect("new session");
        // Capture all events that pass through. Use a Mutex<Vec> so the
        // subscribe closure can append from any thread.
        let captured: Arc<Mutex<Vec<AgentSessionEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_cb = captured.clone();
        session.subscribe(move |ev| {
            captured_cb.lock().unwrap().push(ev);
        });

        session.set_label("hello world").expect("set_label ok");

        let events = captured.lock().unwrap();
        let names: Vec<Option<String>> = events
            .iter()
            .filter_map(|ev| match ev {
                AgentSessionEvent::SessionInfoChanged { name } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            vec![Some("hello world".to_string())],
            "expected exactly one session_info_changed with the new name, got: {names:?}"
        );
        // Persisted state matches.
        assert_eq!(session.label(), Some("hello world"));
    }

    /// `AgentSessionConfig::base_dir` routes the JSONL file under the
    /// provided root instead of the user's home dir. Verifies the
    /// path-rewriting contract embedders (Tauri, sandboxed apps)
    /// depend on: sessions belong inside `<base_dir>/sessions/<flattened-cwd>/`.
    #[test]
    fn base_dir_override_routes_session_storage_under_provided_root() {
        let base = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let mut cfg = test_config(cwd.path().to_path_buf());
        cfg.base_dir = Some(base.path().to_path_buf());

        let mut session = AgentSession::new(cfg, vec![]).expect("session creates under base_dir");

        let path = session.session_manager_mut().path().to_path_buf();
        assert!(
            path.starts_with(base.path()),
            "session path {path:?} must live under base_dir {:?}",
            base.path()
        );
        // Defence in depth: ensure the path actually nests under
        // `<base>/sessions/<flattened-cwd>/` (not just any descendant).
        assert!(
            path.components()
                .any(|c| c.as_os_str() == std::ffi::OsStr::new("sessions")),
            "session path {path:?} must traverse a 'sessions' segment"
        );
    }

    /// abort_bash on a session with no in-flight bash must return
    /// false (nothing to cancel) and leave is_bash_running at false.
    /// The contract was always there but no test pinned it after the
    /// flag accessor landed.
    #[test]
    fn abort_bash_no_inflight_returns_false_and_flag_stays_false() {
        let session = AgentSession::in_memory(test_model(), vec![]);
        assert!(!session.is_bash_running());
        // First call: cancel-able token is uncancelled, so cancel
        // *something* and return true. (Semantically equivalent to
        // "the next run_bash will be aborted before it starts".)
        let first = session.abort_bash();
        assert!(first, "first abort_bash flips the token");
        // Second back-to-back call: token already cancelled → false.
        let second = session.abort_bash();
        assert!(!second, "second abort_bash sees already-cancelled");
        // Flag never went true — no bash actually ran.
        assert!(!session.is_bash_running());
    }

    #[test]
    fn test_set_model() {
        let mut session = AgentSession::in_memory(test_model(), vec![]);
        let mut new_model = test_model();
        new_model.id = "new-model".into();
        session.set_model(new_model);
        assert_eq!(session.model().id, "new-model");
    }

    /// set_model must append a ModelChange entry to the session
    /// journal so a resume picks up the user's switch. Without it,
    /// `hand --model claude --continue` after the user had switched
    /// to `gpt-4o` mid-session would silently revert.
    #[test]
    fn set_model_appends_model_change_to_journal() {
        let mut session = AgentSession::in_memory(test_model(), vec![]);
        // Before switch: no ModelChange entries (only the Session header).
        let pre = session
            .session_manager
            .entries()
            .iter()
            .filter(|e| matches!(e, SessionEntry::ModelChange { .. }))
            .count();
        assert_eq!(pre, 0);

        let mut new_model = test_model();
        new_model.id = "gpt-4o".into();
        new_model.provider = model::types::Provider::OpenAI;
        session.set_model(new_model);

        let entries = session.session_manager.entries();
        let model_changes: Vec<_> = entries
            .iter()
            .filter_map(|e| match e {
                SessionEntry::ModelChange {
                    provider, model_id, ..
                } => Some((provider.as_str(), model_id.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            model_changes,
            vec![("openai", "gpt-4o")],
            "expected one ModelChange entry with the new model, got: {model_changes:?}"
        );
    }

    fn test_config(cwd: PathBuf) -> AgentSessionConfig {
        AgentSessionConfig {
            cwd,
            model: test_model(),
            stream_options: SimpleStreamOptions::default(),
            custom_system_prompt: None,
            custom_guidelines: None,
            resume_session: None,
            no_session: false,
            no_context_files: false,
            session_dir: None,
            no_skills: false,
            extra_skill_dirs: Vec::new(),
            base_dir: None,
        }
    }

    // Issue #43 regression: the YAML round-trip is pinned in
    // `core::settings::tests::apply_setting_by_id_persists_and_round_trips`,
    // which doesn't mutate process-global `$HOME` and so doesn't
    // race with sibling tests that build sessions. The wrapper
    // `driver::persist_theme_selection` is a 5-line shim around
    // `apply_setting_by_id` + `save`; an integration-style test
    // here that flips `$HOME` to a temp dir surfaced as a parallel-
    // run flake (`set_label_emits_session_info_changed` would
    // intermittently fail with `NotFound` when its session-dir
    // resolution caught the temp `$HOME` mid-test). The settings-
    // level test is sufficient; the wrapper is too thin to need a
    // separate $HOME-mutating fixture.

    /// Issue #48: `/reload` must actually swap the session's
    /// `SettingsManager`, not just construct a fresh one and drop
    /// it. Pin the contract by editing a settings file on disk after
    /// the session is constructed and asserting the running session
    /// sees the new value after `reload_settings()`.
    #[test]
    fn reload_settings_picks_up_on_disk_edits() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        // Seed an initial project-scope settings.yaml.
        let settings_dir = cwd.join(".hand");
        fs::create_dir_all(&settings_dir).unwrap();
        let settings_path = settings_dir.join("settings.yaml");
        fs::write(&settings_path, "quiet-startup: false\n").unwrap();

        let mut session =
            AgentSession::new(test_config(cwd.to_path_buf()), vec![]).expect("new session");
        assert!(
            !session.settings().current().quiet_startup(),
            "baseline: quiet-startup must read false"
        );

        // Mutate the file out-of-band — exactly what /reload exists to
        // pick up.
        fs::write(&settings_path, "quiet-startup: true\n").unwrap();

        // Without reload the session still sees the old value.
        assert!(
            !session.settings().current().quiet_startup(),
            "pre-reload: session keeps the original value until reload_settings is called"
        );

        session.reload_settings().expect("reload ok");
        assert!(
            session.settings().current().quiet_startup(),
            "post-reload: session must observe the on-disk change"
        );
    }

    /// Issue #23 / #28: `--no-context-files` (and its `-nc` alias)
    /// must keep HAND.md content out of the system prompt. Two
    /// independent reports landed within hours saying the model
    /// still echoed HAND.md text under the flag, so pin both the
    /// "no flag → loads HAND.md" baseline and the "flag set → does
    /// NOT load" promise.
    #[test]
    fn no_context_files_keeps_hand_md_out_of_system_prompt() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let token = "SECRET-NO-CONTEXT-FILES-TOKEN-XYZ";
        fs::write(cwd.join("HAND.md"), token).unwrap();

        // Baseline: without the flag, HAND.md content lands in the
        // system prompt (so the test below proves something).
        let mut cfg_with = test_config(cwd.to_path_buf());
        cfg_with.no_context_files = false;
        let session_with = AgentSession::new_with_skill_dirs(cfg_with, vec![], None, None)
            .expect("baseline session");
        let prompt_with = &session_with.context.system_prompt;
        assert!(
            prompt_with.contains(token),
            "baseline (no_context_files=false) must include HAND.md, did not. \
             Test environment regressed before the fix can be verified."
        );

        // Real test: flag set, token must NOT appear.
        let mut cfg_no = test_config(cwd.to_path_buf());
        cfg_no.no_context_files = true;
        let session_no =
            AgentSession::new_with_skill_dirs(cfg_no, vec![], None, None).expect("flagged session");
        let prompt_no = &session_no.context.system_prompt;
        assert!(
            !prompt_no.contains(token),
            "no_context_files=true must suppress HAND.md, but the token leaked:\n{prompt_no}"
        );
    }

    /// Issue #20: `--export --resume <id>` was failing for users whose
    /// session lived at the legacy `<cwd>/.hand/sessions/<id>.jsonl`
    /// path — the resume lookup only consulted the home-based primary
    /// location. The fallback now tolerates the legacy layout so older
    /// sessions still resolve.
    #[test]
    fn resume_falls_back_to_legacy_cwd_session_dir() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let session_id = "s_test_legacy_123";
        // Seed a fake session file at the legacy `<cwd>/.hand/sessions/`
        // location. Minimum-viable JSONL: a header line so SessionManager::open
        // accepts the file.
        let legacy_dir = cwd.join(".hand").join("sessions");
        fs::create_dir_all(&legacy_dir).unwrap();
        // JSONL entries are wrapped in {"type": "session", "data": {...}}
        // per the SessionEntry envelope.
        let header = format!(
            "{{\"type\":\"session\",\"data\":{{\"version\":3,\"id\":\"{session_id}\",\"timestamp\":0,\"cwd\":\"{}\"}}}}\n",
            cwd.display()
        );
        fs::write(legacy_dir.join(format!("{session_id}.jsonl")), header).unwrap();

        // Point HAND_HOME at a fresh dir so the "primary" lookup misses
        // — that forces the fallback path. Don't touch the user's real
        // ~/.hand.
        let fake_home = TempDir::new().unwrap();
        // SAFETY: tests run single-threaded under cargo's default
        // unless --test-threads is set higher; this test reads/writes
        // HAND_HOME without lock so callers in parallel would race —
        // accept the risk for now, mark with serial_test if it bites.
        unsafe {
            std::env::set_var("HAND_HOME", fake_home.path());
        }

        let mut config = test_config(cwd.to_path_buf());
        config.resume_session = Some(session_id.to_string());
        let result = AgentSession::new_with_skill_dirs(config, vec![], None, None);

        unsafe {
            std::env::remove_var("HAND_HOME");
        }

        let session = result.expect("legacy session must resolve via fallback");
        assert_eq!(session.session_id(), session_id);
    }

    /// Issue #25: `--resume` must accept a literal `.jsonl` path
    /// without re-appending the suffix. Pre-fix, the resume site
    /// composed `format!("{session_id}.jsonl")` unconditionally and
    /// produced `…/s_xxx.jsonl.jsonl`, missing the real file. Use a
    /// session that lives at a path completely unrelated to the
    /// home-based session_dir so only the verbatim-path branch can
    /// satisfy the lookup.
    #[test]
    fn resume_accepts_literal_jsonl_path_without_double_suffix() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let session_id = "s_literal_path_777";

        // Park the session file under a directory the resolver wouldn't
        // discover on its own. The primary (home) and legacy (cwd) paths
        // must both miss; only `direct = as_path.is_file()` can win.
        let custom_dir = tmp.path().join("anywhere/else");
        fs::create_dir_all(&custom_dir).unwrap();
        let session_path = custom_dir.join(format!("{session_id}.jsonl"));
        let header = format!(
            "{{\"type\":\"session\",\"data\":{{\"version\":3,\"id\":\"{session_id}\",\"timestamp\":0,\"cwd\":\"{}\"}}}}\n",
            cwd.display()
        );
        fs::write(&session_path, header).unwrap();

        let fake_home = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("HAND_HOME", fake_home.path());
        }

        let mut config = test_config(cwd.to_path_buf());
        config.resume_session = Some(session_path.to_string_lossy().into_owned());
        let result = AgentSession::new_with_skill_dirs(config, vec![], None, None);

        unsafe {
            std::env::remove_var("HAND_HOME");
        }

        let session = result.expect("literal .jsonl path must resolve verbatim");
        assert_eq!(session.session_id(), session_id);
    }

    /// When neither the primary nor the legacy session path exists, the
    /// error message must surface BOTH attempted locations so the user
    /// can see exactly where to drop their session file. Pre-fix the
    /// error said only `No session header found in <primary>` and
    /// hid the legacy location entirely.
    #[test]
    fn resume_missing_session_reports_both_attempted_paths() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();

        let fake_home = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("HAND_HOME", fake_home.path());
        }

        let mut config = test_config(cwd.to_path_buf());
        config.resume_session = Some("s_does_not_exist".to_string());
        let result = AgentSession::new_with_skill_dirs(config, vec![], None, None);

        unsafe {
            std::env::remove_var("HAND_HOME");
        }

        let msg = match result {
            Ok(_) => panic!("missing session must error"),
            Err(e) => e.to_string(),
        };
        assert!(msg.contains("s_does_not_exist"), "got: {msg}");
        assert!(
            msg.contains(".hand/agent/sessions"),
            "primary path missing: {msg}"
        );
        assert!(msg.contains(".hand/sessions"), "legacy path missing: {msg}");
    }

    /// Regression for #78: `--resume <prefix>` must resolve a session
    /// by id-prefix when the exact `<dir>/<value>.jsonl` does not
    /// exist. After #76 lengthened the id format, the resume site
    /// only built a literal path and `.open()`d it, so anything
    /// short of the full 32-char id missed the file. Seed a fresh
    /// session under a custom `--session-dir`, then resume by a
    /// short prefix and assert it resolves to the same id.
    #[test]
    fn resume_resolves_id_by_prefix() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let session_dir_tmp = TempDir::new().unwrap();
        let session_dir = session_dir_tmp.path().to_path_buf();
        std::fs::create_dir_all(&session_dir).unwrap();
        // Mint a real session under the override dir so its id has
        // the full collision-free shape from #76.
        let sm = SessionManager::create_in(cwd, &session_dir).expect("create_in");
        let full_id = sm.id().to_string();
        drop(sm);
        // A safe non-trivial prefix: drop the random suffix (the last
        // hex group separated by `_`). The prefix is unique within
        // the empty session_dir so the scan returns exactly one
        // candidate.
        let prefix = full_id.rsplit_once('_').unwrap().0.to_string();
        assert!(
            prefix.len() < full_id.len(),
            "prefix must actually be shorter"
        );

        let mut config = test_config(cwd.to_path_buf());
        config.session_dir = Some(session_dir.clone());
        config.resume_session = Some(prefix.clone());

        let session = AgentSession::new_with_skill_dirs(config, vec![], None, None)
            .unwrap_or_else(|e| panic!("resume by prefix `{prefix}` failed: {e}"));
        assert_eq!(
            session.session_id(),
            full_id,
            "prefix resume must land on the same session id"
        );
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
        assert!(
            prompt.contains("<name>foo</name>"),
            "prompt missing skill name: {prompt}"
        );
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
        fs::write(
            bad.join("SKILL.md"),
            "---\ndescription: oops\nbody without close\n",
        )
        .unwrap();

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
            timeouts: Default::default(),
        }
    }

    /// A test extension that records every invocation it sees, in order.
    /// `before_decision` is what `on_before_tool_call` returns;
    /// `load_fails` makes `on_load` return an error.
    struct RecordingExt {
        manifest: ExtensionManifest,
        before_decision: HookDecision,
        /// What the after-hook returns. Defaults to `Continue`; a test that
        /// exercises result rewriting sets it via `with_after_decision`.
        after_decision: ResultDecision,
        before_calls: Mutex<Vec<ToolCallEvent>>,
        after_calls: Mutex<Vec<ToolResultEvent>>,
        /// Every hook this extension saw, in call order — lets a test
        /// assert `on_load` really precedes the first tool call.
        trace: Mutex<Vec<String>>,
        load_fails: bool,
    }

    impl RecordingExt {
        fn new(name: &str, before_decision: HookDecision) -> Arc<Self> {
            Arc::new(Self {
                manifest: ext_manifest(name),
                before_decision,
                after_decision: ResultDecision::Continue,
                before_calls: Mutex::new(Vec::new()),
                after_calls: Mutex::new(Vec::new()),
                trace: Mutex::new(Vec::new()),
                load_fails: false,
            })
        }

        /// Same as `new`, but the after-hook rewrites the tool result.
        fn replacing_result(name: &str, replacement: serde_json::Value) -> Arc<Self> {
            Arc::new(Self {
                manifest: ext_manifest(name),
                before_decision: HookDecision::Continue,
                after_decision: ResultDecision::Replace(replacement),
                before_calls: Mutex::new(Vec::new()),
                after_calls: Mutex::new(Vec::new()),
                trace: Mutex::new(Vec::new()),
                load_fails: false,
            })
        }

        fn failing_load(name: &str) -> Arc<Self> {
            Arc::new(Self {
                manifest: ext_manifest(name),
                before_decision: HookDecision::Continue,
                after_decision: ResultDecision::Continue,
                before_calls: Mutex::new(Vec::new()),
                after_calls: Mutex::new(Vec::new()),
                trace: Mutex::new(Vec::new()),
                load_fails: true,
            })
        }

        fn trace(&self) -> Vec<String> {
            self.trace.lock().unwrap().clone()
        }

        fn count(&self, hook: &str) -> usize {
            self.trace().iter().filter(|h| *h == hook).count()
        }
    }

    #[async_trait]
    impl Extension for RecordingExt {
        fn manifest(&self) -> &ExtensionManifest {
            &self.manifest
        }

        async fn on_load(&self, _cx: &ExtensionContext) -> Result<(), ExtensionError> {
            self.trace.lock().unwrap().push("load".into());
            if self.load_fails {
                return Err(ExtensionError::Custom {
                    name: self.manifest.name.clone(),
                    message: "setup failed".into(),
                });
            }
            Ok(())
        }

        async fn on_shutdown(&self, _cx: &ExtensionContext) -> Result<(), ExtensionError> {
            self.trace.lock().unwrap().push("shutdown".into());
            Ok(())
        }

        async fn on_before_tool_call(
            &self,
            _cx: &ExtensionContext,
            event: &ToolCallEvent,
        ) -> Result<HookDecision, ExtensionError> {
            self.trace.lock().unwrap().push("before".into());
            self.before_calls.lock().unwrap().push(event.clone());
            Ok(self.before_decision.clone())
        }

        async fn on_after_tool_call(
            &self,
            _cx: &ExtensionContext,
            event: &ToolResultEvent,
        ) -> Result<ResultDecision, ExtensionError> {
            self.trace.lock().unwrap().push("after".into());
            self.after_calls.lock().unwrap().push(event.clone());
            Ok(self.after_decision.clone())
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

    /// `on_load` runs once per extension no matter how often the session
    /// drives the lifecycle, and `on_shutdown` runs once per load.
    #[tokio::test]
    async fn extension_lifecycle_runs_once_per_session() {
        let mut session = AgentSession::in_memory(test_model(), vec![]);
        let ext = RecordingExt::new("recorder", HookDecision::Continue);
        session.register_extension(ext.clone());

        session.load_extensions().await;
        session.load_extensions().await;
        assert_eq!(ext.count("load"), 1, "on_load must not run twice");

        session.shutdown_extensions().await;
        session.shutdown_extensions().await;
        assert_eq!(ext.count("shutdown"), 1, "on_shutdown must not run twice");
        assert_eq!(ext.trace(), vec!["load", "shutdown"]);
    }

    /// Shutting down a session that never loaded its extensions is a no-op:
    /// an extension that never got `on_load` must not get `on_shutdown`.
    #[tokio::test]
    async fn shutdown_without_load_is_a_no_op() {
        let mut session = AgentSession::in_memory(test_model(), vec![]);
        let ext = RecordingExt::new("recorder", HookDecision::Continue);
        session.register_extension(ext.clone());

        session.shutdown_extensions().await;
        assert!(ext.trace().is_empty());
    }

    /// An extension whose setup failed is dropped from the chain rather
    /// than left running degraded, and the failure is reported.
    #[tokio::test]
    async fn failing_on_load_drops_the_extension() {
        let mut session = AgentSession::in_memory(test_model(), vec![]);
        let broken = RecordingExt::failing_load("broken");
        let healthy = RecordingExt::new("healthy", HookDecision::Continue);
        session.register_extension(broken.clone());
        session.register_extension(healthy.clone());

        session.load_extensions().await;

        assert_eq!(session.extensions().len(), 1);
        assert_eq!(session.extensions()[0].manifest().name, "healthy");
        assert_eq!(session.extension_errors().len(), 1);
        assert_eq!(session.extension_errors()[0].0, "broken");
        assert!(session.extension_errors()[0].1.contains("setup failed"));

        // The dropped extension never sees teardown for a load that failed.
        session.shutdown_extensions().await;
        assert_eq!(broken.trace(), vec!["load"]);
        assert_eq!(healthy.trace(), vec!["load", "shutdown"]);
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
        let cx = session.extension_context_for("foo");

        assert!(!cx.session_id.is_empty(), "session id must not be empty");
        assert!(!cx.cwd.as_os_str().is_empty(), "cwd must not be empty");
        assert!(
            cx.data_dir.ends_with("extensions/foo/data"),
            "data_dir should be the extension's own slot, got {:?}",
            cx.data_dir
        );
    }

    /// Every extension gets its own directory: two extensions writing
    /// `state.json` must not collide.
    #[test]
    fn extension_context_is_per_extension() {
        let session = AgentSession::in_memory(test_model(), vec![]);
        let foo = session.extension_context_for("foo");
        let bar = session.extension_context_for("bar");

        assert_ne!(foo.data_dir, bar.data_dir);
        assert!(foo.data_dir.ends_with("foo/data"));
        assert!(bar.data_dir.ends_with("bar/data"));
    }

    /// With no `base_dir`, extension state stays where the CLI has always
    /// put it: `<cwd>/.hand/extensions/<name>/data`.
    #[test]
    fn extension_data_dir_falls_back_to_cwd_when_no_base_dir() {
        let cwd = TempDir::new().unwrap();
        let cfg = test_config(cwd.path().to_path_buf());
        let session = AgentSession::new(cfg, vec![]).expect("session creates");

        let cx = session.extension_context_for("foo");
        assert_eq!(
            cx.data_dir,
            cwd.path()
                .join(".hand")
                .join("extensions")
                .join("foo")
                .join("data")
        );
    }

    /// An embedder that pinned `base_dir` (a Tauri app-data dir, say) keeps
    /// extension state out of the user's repository entirely — and nothing
    /// is created under `cwd` just by resolving the path.
    #[test]
    fn extension_data_dir_honors_base_dir() {
        let base = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let mut cfg = test_config(cwd.path().to_path_buf());
        cfg.base_dir = Some(base.path().to_path_buf());
        let session = AgentSession::new(cfg, vec![]).expect("session creates under base_dir");

        let cx = session.extension_context_for("foo");
        assert_eq!(
            cx.data_dir,
            base.path().join("extensions").join("foo").join("data")
        );
        assert!(
            !cwd.path().join(".hand").join("extensions").exists(),
            "resolving an extension data dir must not write into the workspace"
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

    /// Mock provider: always emits a single text message and stops.
    /// Used by tests that need a clean user→assistant turn without
    /// tool calls.
    struct TextOnlyProvider {
        reply: String,
    }

    impl ApiProvider for TextOnlyProvider {
        fn stream(
            &self,
            _model: model::Model,
            _context: Context,
            _options: Option<StreamOptions>,
        ) -> AssistantMessageEventStream<'static> {
            let reply = self.reply.clone();
            Box::pin(async_stream::stream! {
                let msg = assistant_text_message(&reply);
                yield AssistantMessageEvent::Start { partial: msg.clone() };
                yield AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: msg,
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

    /// Mock provider that records the user text it was handed, so a test
    /// can assert what the model actually received.
    struct PromptRecordingProvider {
        prompts: Arc<Mutex<Vec<String>>>,
    }

    impl ApiProvider for PromptRecordingProvider {
        fn stream(
            &self,
            _model: model::Model,
            context: Context,
            _options: Option<StreamOptions>,
        ) -> AssistantMessageEventStream<'static> {
            for msg in &context.messages {
                if let Message::User(user) = msg {
                    self.prompts
                        .lock()
                        .unwrap()
                        .push(extract_user_message_text(&user.content));
                }
            }
            Box::pin(async_stream::stream! {
                let msg = assistant_text_message("ack");
                yield AssistantMessageEvent::Start { partial: msg.clone() };
                yield AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: msg,
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

    /// A test extension for the user-message hook, gated on the capability
    /// exactly as a real one would be.
    struct PromptExt {
        manifest: ExtensionManifest,
        outcome: UserMessageOutcome,
        seen: Mutex<Vec<String>>,
    }

    impl PromptExt {
        fn new(name: &str, subscribed: bool, outcome: UserMessageOutcome) -> Arc<Self> {
            let mut manifest = ext_manifest(name);
            manifest.capabilities.on_user_message = subscribed;
            Arc::new(Self {
                manifest,
                outcome,
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl Extension for PromptExt {
        fn manifest(&self) -> &ExtensionManifest {
            &self.manifest
        }

        async fn on_user_message(
            &self,
            _cx: &ExtensionContext,
            event: &crate::core::extensions::api::UserMessageEvent,
        ) -> Result<UserMessageOutcome, ExtensionError> {
            self.seen.lock().unwrap().push(event.text.clone());
            Ok(self.outcome.clone())
        }
    }

    /// A `Replace` from the user-message hook changes what both the
    /// transcript and the model receive.
    #[tokio::test]
    async fn user_message_hook_replace_changes_what_the_model_receives() {
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(PromptRecordingProvider {
                prompts: prompts.clone(),
            }),
            Some("test".into()),
        );

        let mut session = AgentSession::in_memory_with_client(openai_test_model(), vec![], client);
        let ext = PromptExt::new(
            "scrubber",
            true,
            HookDecision::Replace(serde_json::json!("token=[redacted]")).into(),
        );
        session.register_extension(ext.clone());

        session
            .send_message("token=hunter2")
            .await
            .expect("send_message succeeds");

        assert_eq!(*ext.seen.lock().unwrap().first().unwrap(), "token=hunter2");
        assert!(
            prompts
                .lock()
                .unwrap()
                .iter()
                .any(|p| p == "token=[redacted]"),
            "model should receive the rewritten prompt, got {:?}",
            prompts.lock().unwrap()
        );
        assert!(
            !prompts
                .lock()
                .unwrap()
                .iter()
                .any(|p| p.contains("hunter2")),
            "the raw prompt must not reach the model"
        );
    }

    /// `additional_context` reaches the model without the user's own
    /// message being edited, and is attributed to its extension so the
    /// model does not read it as something the user typed.
    #[tokio::test]
    async fn user_message_additional_context_reaches_the_model_separately() {
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(PromptRecordingProvider {
                prompts: prompts.clone(),
            }),
            Some("test".into()),
        );

        let mut session = AgentSession::in_memory_with_client(openai_test_model(), vec![], client);
        session.register_extension(PromptExt::new(
            "git-status",
            true,
            UserMessageOutcome::context("on branch main, 3 files modified"),
        ));

        session
            .send_message("what changed?")
            .await
            .expect("send_message succeeds");

        let seen = prompts.lock().unwrap().clone();
        assert!(
            seen.iter().any(|p| p.contains("on branch main")),
            "the model must receive the contributed context, got {seen:?}"
        );
        assert!(
            seen.iter().any(|p| p.contains("extension=\"git-status\"")),
            "context must be attributed to its extension, got {seen:?}"
        );
        // The user's own message is delivered untouched, as its own entry.
        assert!(
            seen.iter().any(|p| p == "what changed?"),
            "the user's prompt must arrive unedited, got {seen:?}"
        );
    }

    /// With no extension contributing context, the request is exactly what
    /// it was before the feature existed — one message, the user's.
    #[tokio::test]
    async fn no_additional_context_leaves_the_request_unchanged() {
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(PromptRecordingProvider {
                prompts: prompts.clone(),
            }),
            Some("test".into()),
        );

        let mut session = AgentSession::in_memory_with_client(openai_test_model(), vec![], client);
        session.register_extension(PromptExt::new("quiet", true, HookDecision::Continue.into()));

        session
            .send_message("hello")
            .await
            .expect("send_message succeeds");

        assert_eq!(*prompts.lock().unwrap(), vec!["hello".to_string()]);
    }

    /// A turn-end extension, gated on the capability like a real one.
    /// `refusals` counts down: it keeps the agent working that many times,
    /// then relents.
    struct TurnEndExt {
        manifest: ExtensionManifest,
        refusals: Mutex<usize>,
        seen: Mutex<Vec<TurnEndEvent>>,
    }

    impl TurnEndExt {
        fn new(name: &str, refusals: usize) -> Arc<Self> {
            let mut manifest = ext_manifest(name);
            manifest.capabilities.on_turn_end = true;
            Arc::new(Self {
                manifest,
                refusals: Mutex::new(refusals),
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl Extension for TurnEndExt {
        fn manifest(&self) -> &ExtensionManifest {
            &self.manifest
        }

        async fn on_turn_end(
            &self,
            _cx: &ExtensionContext,
            event: &TurnEndEvent,
        ) -> Result<HookDecision, ExtensionError> {
            self.seen.lock().unwrap().push(event.clone());
            let mut left = self.refusals.lock().unwrap();
            if *left == 0 {
                return Ok(HookDecision::Continue);
            }
            *left -= 1;
            Ok(HookDecision::Cancel("run the tests first".into()))
        }
    }

    /// The hook fires once per turn and carries the assistant's final
    /// text — the piece `auto-commit-on-exit` needs to derive a real
    /// commit subject instead of a static one.
    #[tokio::test]
    async fn turn_end_hook_sees_the_finished_turn() {
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(PromptRecordingProvider {
                prompts: Default::default(),
            }),
            Some("test".into()),
        );

        let mut session = AgentSession::in_memory_with_client(openai_test_model(), vec![], client);
        let ext = TurnEndExt::new("committer", 0);
        session.register_extension(ext.clone());

        session
            .send_message("do the thing")
            .await
            .expect("send_message succeeds");

        let seen = ext.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "exactly one call per turn, got {seen:?}");
        // `PromptRecordingProvider` replies "ack".
        assert_eq!(seen[0].last_assistant_message, "ack");
        assert!(
            !seen[0].stop_reason.is_empty(),
            "stop reason must be populated"
        );
    }

    /// `Cancel` keeps the agent working: the loop runs another turn with
    /// the reason as the instruction, instead of returning to the user.
    #[tokio::test]
    async fn turn_end_cancel_keeps_the_agent_working() {
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(PromptRecordingProvider {
                prompts: prompts.clone(),
            }),
            Some("test".into()),
        );

        let mut session = AgentSession::in_memory_with_client(openai_test_model(), vec![], client);
        let ext = TurnEndExt::new("nag", 1);
        session.register_extension(ext.clone());

        session
            .send_message("do the thing")
            .await
            .expect("send_message succeeds");

        assert_eq!(
            ext.seen.lock().unwrap().len(),
            2,
            "one refusal then one acceptance"
        );
        assert!(
            prompts
                .lock()
                .unwrap()
                .iter()
                .any(|p| p == "run the tests first"),
            "the reason must reach the model as the next instruction, got {:?}",
            prompts.lock().unwrap()
        );
    }

    /// An extension that never relents loses at the bound. Without it the
    /// session would bill the user for model round-trips forever.
    #[tokio::test]
    async fn turn_end_repeated_cancel_terminates_at_the_bound() {
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(PromptRecordingProvider {
                prompts: Default::default(),
            }),
            Some("test".into()),
        );

        let mut session = AgentSession::in_memory_with_client(openai_test_model(), vec![], client);
        // Far more refusals than the bound allows.
        let ext = TurnEndExt::new("stubborn", 100);
        session.register_extension(ext.clone());

        session
            .send_message("do the thing")
            .await
            .expect("the turn must end rather than loop forever");

        assert_eq!(
            ext.seen.lock().unwrap().len(),
            MAX_TURN_END_CONTINUATIONS,
            "the hook is consulted up to the bound, then the turn ends"
        );
    }

    /// A `Cancel` aborts the turn before anything is persisted and surfaces
    /// the reason to the caller.
    #[tokio::test]
    async fn user_message_hook_cancel_aborts_the_turn() {
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(PromptRecordingProvider {
                prompts: prompts.clone(),
            }),
            Some("test".into()),
        );

        let mut session = AgentSession::in_memory_with_client(openai_test_model(), vec![], client);
        session.register_extension(PromptExt::new(
            "guard",
            true,
            HookDecision::Cancel("prompt contains a secret".into()).into(),
        ));

        let err = session
            .send_message("token=hunter2")
            .await
            .expect_err("a cancelled prompt must not start a turn");
        assert!(
            err.to_string().contains("prompt contains a secret"),
            "the reason should reach the user, got {err}"
        );
        assert!(
            prompts.lock().unwrap().is_empty(),
            "the model must not be called at all"
        );
        assert!(
            session.messages().is_empty(),
            "nothing may be persisted for a cancelled turn"
        );
    }

    /// An extension that did not declare the capability is never consulted.
    #[tokio::test]
    async fn user_message_hook_respects_the_capability_flag() {
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(TextOnlyProvider {
                reply: "ack".into(),
            }),
            Some("test".into()),
        );

        let mut session = AgentSession::in_memory_with_client(openai_test_model(), vec![], client);
        let ext = PromptExt::new(
            "unsubscribed",
            false,
            HookDecision::Cancel("nope".into()).into(),
        );
        session.register_extension(ext.clone());

        session
            .send_message("hello")
            .await
            .expect("an unsubscribed extension cannot cancel the turn");
        assert!(ext.seen.lock().unwrap().is_empty());
    }

    /// Mock provider: turn 1 emits a tool call, turn 2+ emit text and stop.
    struct ToolThenTextProvider {
        tool_name: String,
        args: serde_json::Value,
        invocation: AtomicUsize,
        /// Text of every tool result present in the context each time the
        /// provider is called — i.e. exactly what the model got to read.
        tool_results: Arc<Mutex<Vec<String>>>,
    }

    impl ApiProvider for ToolThenTextProvider {
        fn stream(
            &self,
            _model: model::Model,
            context: Context,
            _options: Option<StreamOptions>,
        ) -> AssistantMessageEventStream<'static> {
            for msg in &context.messages {
                if let Message::ToolResult(tr) = msg {
                    for block in &tr.content {
                        if let model::ToolResultContent::Text(text) = block {
                            self.tool_results.lock().unwrap().push(text.text.clone());
                        }
                    }
                }
            }
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

    /// Issue #19: `send_message` was double-persisting the user
    /// message — once upfront for crash-resilience, then a second
    /// time when iterating `result.messages` (which the agent loop
    /// returns as `[prompts.., assistant_msgs..]`). The duplicate
    /// surfaced in `--export` HTML as each user turn appearing
    /// twice. Pin the persistence shape so a future refactor of
    /// the agent loop's return contract can't quietly regress.
    #[tokio::test]
    async fn send_message_persists_user_message_exactly_once() {
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(TextOnlyProvider {
                reply: "ack".into(),
            }),
            Some("test".into()),
        );

        let mut session = AgentSession::in_memory_with_client(openai_test_model(), vec![], client);

        session
            .send_message("hello once")
            .await
            .expect("send_message ok");

        let ctx = session.session_manager.build_context();
        let user_count = ctx.iter().filter(|m| matches!(m, Message::User(_))).count();
        let assistant_count = ctx
            .iter()
            .filter(|m| matches!(m, Message::Assistant(_)))
            .count();
        assert_eq!(
            user_count, 1,
            "user message must be persisted exactly once, got {user_count} (full ctx: {ctx:?})"
        );
        assert_eq!(
            assistant_count, 1,
            "expected one assistant reply, got {assistant_count}"
        );
    }

    /// The redaction case from the after-hook contract: a `Replace` must
    /// change what the model reads, and the un-replaced value must never
    /// appear in the request.
    #[tokio::test]
    async fn after_hook_replace_changes_what_the_model_reads() {
        let tool_results: Arc<Mutex<Vec<String>>> = Default::default();
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(ToolThenTextProvider {
                tool_name: "noop".into(),
                args: serde_json::json!({}),
                invocation: AtomicUsize::new(0),
                tool_results: tool_results.clone(),
            }),
            Some("test".into()),
        );

        let mut session =
            AgentSession::in_memory_with_client(openai_test_model(), vec![noop_tool()], client);

        // `noop_tool` returns "noop ok"; the extension swaps it for a
        // scrubbed body of the same shape.
        session.register_extension(RecordingExt::replacing_result(
            "scrubber",
            serde_json::json!({
                "content": [{"type": "text", "text": "[redacted]"}]
            }),
        ));

        session
            .send_message("please call noop")
            .await
            .expect("send_message should succeed");

        let seen = tool_results.lock().unwrap().clone();
        assert!(
            seen.iter().any(|t| t == "[redacted]"),
            "model must read the replacement, got {seen:?}"
        );
        assert!(
            !seen.iter().any(|t| t.contains("noop ok")),
            "the original result must never reach the model, got {seen:?}"
        );
    }

    /// An unparseable replacement is a contract violation by the
    /// extension. It must cost the rewrite, not the result — dropping the
    /// tool's output entirely would be a far worse failure.
    #[tokio::test]
    async fn after_hook_malformed_replacement_keeps_the_original_result() {
        let tool_results: Arc<Mutex<Vec<String>>> = Default::default();
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(ToolThenTextProvider {
                tool_name: "noop".into(),
                args: serde_json::json!({}),
                invocation: AtomicUsize::new(0),
                tool_results: tool_results.clone(),
            }),
            Some("test".into()),
        );

        let mut session =
            AgentSession::in_memory_with_client(openai_test_model(), vec![noop_tool()], client);
        session.register_extension(RecordingExt::replacing_result(
            "confused",
            serde_json::json!("not a tool result"),
        ));

        session
            .send_message("please call noop")
            .await
            .expect("send_message should succeed");

        let seen = tool_results.lock().unwrap().clone();
        assert!(
            seen.iter().any(|t| t.contains("noop ok")),
            "a malformed rewrite must leave the tool's own output intact, got {seen:?}"
        );
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
                tool_results: Default::default(),
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

    /// `send_message` drives the lifecycle itself: an extension registered
    /// on a session sees exactly one `on_load`, before the first tool call,
    /// without the host having to call `load_extensions` by hand.
    #[tokio::test]
    async fn send_message_loads_extensions_before_the_first_tool_call() {
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(ToolThenTextProvider {
                tool_name: "noop".into(),
                args: serde_json::json!({}),
                invocation: AtomicUsize::new(0),
                tool_results: Default::default(),
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

        assert_eq!(
            ext.trace(),
            vec!["load", "before", "after"],
            "on_load must precede the first tool-call hook"
        );

        session.shutdown_extensions().await;
        assert_eq!(ext.trace(), vec!["load", "before", "after", "shutdown"]);
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
        let outcome = tokio::time::timeout(std::time::Duration::from_millis(50), send_fut).await;
        assert!(
            outcome.is_err(),
            "send_message should have been cancelled by timeout"
        );

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
                tool_results: Default::default(),
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

    /// A `HookDecision::Replace(args)` from a Tier-1 extension reaches the
    /// tool: the tool executes the rewritten arguments, not the model's.
    #[tokio::test]
    async fn replace_args_reaches_the_tool() {
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(ToolThenTextProvider {
                tool_name: "noop".into(),
                args: serde_json::json!({"original": true}),
                invocation: AtomicUsize::new(0),
                tool_results: Default::default(),
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

        let mut session =
            AgentSession::in_memory_with_client(openai_test_model(), vec![recorder_tool], client);

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
            Some(serde_json::json!({"replaced": true})),
            "the tool must run the arguments the extension chain settled on"
        );

        // The transcript still records what the model asked for — the
        // rewrite describes what the host allowed, not what the model said.
        let asked: Vec<serde_json::Value> = session
            .messages()
            .iter()
            .filter_map(|m| match m {
                Message::Assistant(a) => Some(a),
                _ => None,
            })
            .flat_map(|a| a.content.iter())
            .filter_map(|block| match block {
                model::AssistantContentBlock::ToolCall(tc) => Some(tc.arguments.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(asked, vec![serde_json::json!({"original": true})]);
    }

    /// A rewrite that violates the tool's schema is rejected instead of
    /// being handed to the tool: the call fails with an error result and
    /// the tool is never entered.
    #[tokio::test]
    async fn replace_args_violating_the_schema_is_rejected() {
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(ToolThenTextProvider {
                tool_name: "echo".into(),
                args: serde_json::json!({"message": "hello"}),
                invocation: AtomicUsize::new(0),
                tool_results: Default::default(),
            }),
            Some("test".into()),
        );

        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let entered_for_tool = entered.clone();
        let echo_tool = AgentTool::simple(
            "echo",
            "Echoes a message",
            serde_json::json!({
                "type": "object",
                "properties": { "message": { "type": "string" } },
                "required": ["message"]
            }),
            "Echo",
            move |_call_id, _args| {
                let entered = entered_for_tool.clone();
                async move {
                    entered.store(true, Ordering::SeqCst);
                    hand_agent::types::ToolResult::text("echoed")
                }
            },
        );

        let mut session =
            AgentSession::in_memory_with_client(openai_test_model(), vec![echo_tool], client);
        session.register_extension(RecordingExt::new(
            "rewriter",
            // `message` is required and must be a string.
            HookDecision::Replace(serde_json::json!({"message": 42})),
        ));

        session
            .send_message("call echo")
            .await
            .expect("the turn survives a rejected rewrite");

        assert!(
            !entered.load(Ordering::SeqCst),
            "the tool must not run with arguments its schema rejects"
        );
        let results: Vec<String> = session
            .messages()
            .iter()
            .filter_map(|m| match m {
                Message::ToolResult(tr) => Some(tr),
                _ => None,
            })
            .flat_map(|tr| tr.content.iter())
            .filter_map(|c| match c {
                model::ToolResultContent::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect();
        assert!(
            results
                .iter()
                .any(|t| t.contains("Invalid replacement arguments")),
            "the model should see why the call failed, got {results:?}"
        );
    }

    /// An explicit literal `.jsonl` path handed to `--resume` opens
    /// via the jsonl flow even when the `session-backend: sqlite`
    /// setting is active — explicit file wins.
    #[test]
    fn resume_literal_jsonl_path_bypasses_sqlite_backend() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();

        // Project-layer setting selects the sqlite backend.
        let hand_dir = cwd.join(".hand");
        fs::create_dir_all(&hand_dir).unwrap();
        fs::write(hand_dir.join("settings.yaml"), "session-backend: sqlite\n").unwrap();

        // Literal session file outside any session dir.
        let session_id = "s_literal_wins_1";
        let session_path = cwd.join(format!("{session_id}.jsonl"));
        let header = format!(
            "{{\"type\":\"session\",\"data\":{{\"version\":3,\"id\":\"{session_id}\",\"timestamp\":0,\"cwd\":\"{}\"}}}}\n",
            cwd.display()
        );
        fs::write(&session_path, header).unwrap();

        let fake_home = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("HAND_HOME", fake_home.path());
        }
        let mut config = test_config(cwd.to_path_buf());
        config.resume_session = Some(session_path.to_string_lossy().into_owned());
        let result = AgentSession::new_with_skill_dirs(config, vec![], None, None);
        unsafe {
            std::env::remove_var("HAND_HOME");
        }

        let session = result.expect("literal .jsonl path must open via the jsonl flow");
        assert_eq!(session.session_id(), session_id);
        assert_eq!(session.session_backend(), SessionBackend::Jsonl);
        assert_eq!(
            session
                .session_file()
                .and_then(|p| p.extension())
                .and_then(|e| e.to_str()),
            Some("jsonl")
        );
    }

    /// With `session-backend: sqlite` in the project settings, a fresh
    /// session lands in the session directory's database.
    #[cfg(feature = "sqlite")]
    #[test]
    fn new_session_honours_sqlite_backend_setting() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let hand_dir = cwd.join(".hand");
        fs::create_dir_all(&hand_dir).unwrap();
        fs::write(hand_dir.join("settings.yaml"), "session-backend: sqlite\n").unwrap();

        // Explicit session_dir override keeps the db inside the test's
        // tempdir without touching HAND_HOME.
        let session_dir = tmp.path().join("sessions");
        let mut config = test_config(cwd.to_path_buf());
        config.session_dir = Some(session_dir.clone());
        let session = AgentSession::new_with_skill_dirs(config, vec![], None, None)
            .expect("sqlite-backed session");

        assert_eq!(session.session_backend(), SessionBackend::Sqlite);
        assert_eq!(
            session
                .session_file()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str()),
            Some("sessions.db")
        );
        assert!(session_dir.join("sessions.db").exists());
    }
}
