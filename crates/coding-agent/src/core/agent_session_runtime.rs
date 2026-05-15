//! High-level session orchestrator.
//!
//! [`AgentSessionRuntime`] owns the current
//! [`crate::core::agent_session::AgentSession`] plus its cwd-bound
//! [`AgentSessionServices`] and exposes `switch_session` /
//! `new_session` / `fork` / `import_from_jsonl` / `dispose` that all
//! follow the same teardown-then-rebuild pattern: emit lifecycle
//! events, dispose the previous session, create a new one via the
//! supplied factory, and rebind host UI.
//!
//! The current implementation carries the **structural** scaffolding:
//! the runtime struct, the factory type, the diagnostic carrier, and
//! the import-error type. Lifecycle methods that depend on pieces
//! still in flight (`ExtensionRunner` events, persistence-aware
//! `SessionManager` helpers such as `create_branched_session` /
//! `new_session` / `get_session_file`, `assert_session_cwd_exists`,
//! `AgentSession::dispose`, `AgentSession::create_replaced_session_context`)
//! are tracked with `TODO` markers and are not exposed here yet.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use thiserror::Error;

use crate::core::agent_session::AgentSession;
use crate::core::agent_session_services::{AgentSessionRuntimeDiagnostic, AgentSessionServices};
use crate::core::session_cwd::{SessionCwdSource, assert_session_cwd_exists};
use crate::core::session_manager::SessionManager;

/// Boxed future returned by a [`CreateAgentSessionRuntimeFactory`].
///
/// The `Send` bound matches how the TS reference is invoked from a Tokio task
/// in the Rust host, where `'static` enforces no borrowed inputs leak across
/// the await point.
pub type CreateAgentSessionRuntimeFuture = Pin<
    Box<dyn Future<Output = Result<CreateAgentSessionRuntimeResult, RuntimeFactoryError>> + Send>,
>;

/// Inputs handed to a [`CreateAgentSessionRuntimeFactory`] when the runtime
/// rebuilds itself for a new effective cwd or session file.
pub struct CreateAgentSessionRuntimeFactoryInput {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub session_manager: SessionManager,
    // TODO(parity): port `sessionStartEvent: SessionStartEvent` once
    // extension lifecycle events exist in the Rust extensions runtime.
}

/// Factory closure used to (re)build a runtime.
///
/// The Rust port mirrors the TS `CreateAgentSessionRuntimeFactory`: a closure
/// closing over process-global fixed inputs that constructs cwd-bound
/// services, resolves session options against them, and finally creates the
/// [`AgentSession`].
pub type CreateAgentSessionRuntimeFactory = Arc<
    dyn Fn(CreateAgentSessionRuntimeFactoryInput) -> CreateAgentSessionRuntimeFuture + Send + Sync,
>;

/// Result returned by a runtime factory invocation.
///
/// Mirrors the TS `CreateAgentSessionRuntimeResult` shape: the freshly built
/// session, the cwd-bound services it was wired against, any non-fatal
/// diagnostics raised during setup, and an optional model-fallback message
/// surfaced to the user when the persisted model could not be restored.
pub struct CreateAgentSessionRuntimeResult {
    pub session: AgentSession,
    pub services: AgentSessionServices,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    pub model_fallback_message: Option<String>,
}

/// Boxed error returned by the factory.
///
/// Hosts decide how to surface this to the user; the runtime simply
/// propagates failures from the factory unchanged.
pub type RuntimeFactoryError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Thin adapter so `SessionManager` can satisfy the `SessionCwdSource`
/// trait without `session_cwd.rs` reaching into the session_manager
/// module (and creating a cycle). Constructed only at the call site
/// inside [`create_agent_session_runtime`].
struct SessionManagerCwdSource<'a> {
    sm: &'a SessionManager,
}

impl<'a> SessionCwdSource for SessionManagerCwdSource<'a> {
    fn cwd(&self) -> Option<PathBuf> {
        self.sm.stored_cwd()
    }

    fn session_file(&self) -> Option<PathBuf> {
        self.sm.on_disk_session_file()
    }
}

// Re-export so callers downcasting `RuntimeFactoryError` can pattern-
// match the missing-cwd variant without depending on the cwd module
// path. Mirrors how the TS export surfaces `MissingSessionCwdError`.
pub use crate::core::session_cwd::MissingSessionCwdError as RuntimeMissingSessionCwdError;

/// Raised when `/import` references a JSONL file path that does not exist.
///
/// Direct port of the TS `SessionImportFileNotFoundError`.
#[derive(Debug, Error)]
#[error("file not found: {path}")]
pub struct SessionImportFileNotFoundError {
    pub path: PathBuf,
}

impl SessionImportFileNotFoundError {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

/// Owns the current [`AgentSession`] plus its cwd-bound services.
///
/// The TS reference exposes mutation methods (`switchSession`, `newSession`,
/// `fork`, `importFromJsonl`) that tear down the current runtime and replace
/// it with a freshly built one via the stored factory. Those methods are
/// not yet exposed here — see the module docs for the missing parity pieces.
pub struct AgentSessionRuntime {
    session: AgentSession,
    services: AgentSessionServices,
    create_runtime: CreateAgentSessionRuntimeFactory,
    diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    model_fallback_message: Option<String>,
}

impl AgentSessionRuntime {
    /// Construct a runtime around an already-built session and services.
    ///
    /// Hosts normally call [`create_agent_session_runtime`] which delegates
    /// to the supplied factory; this constructor is provided so callers that
    /// have already obtained a session via another path can still wrap it.
    pub fn new(
        session: AgentSession,
        services: AgentSessionServices,
        create_runtime: CreateAgentSessionRuntimeFactory,
        diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
        model_fallback_message: Option<String>,
    ) -> Self {
        Self {
            session,
            services,
            create_runtime,
            diagnostics,
            model_fallback_message,
        }
    }

    /// Borrow the current services container.
    pub fn services(&self) -> &AgentSessionServices {
        &self.services
    }

    /// Borrow the current session.
    pub fn session(&self) -> &AgentSession {
        &self.session
    }

    /// Mutable borrow of the current session — needed by hosts that drive
    /// the agent loop after construction.
    pub fn session_mut(&mut self) -> &mut AgentSession {
        &mut self.session
    }

    /// Borrow the working directory of the current services.
    pub fn cwd(&self) -> &Path {
        self.services.cwd()
    }

    /// Diagnostics collected during the most recent runtime build.
    pub fn diagnostics(&self) -> &[AgentSessionRuntimeDiagnostic] {
        &self.diagnostics
    }

    /// Optional model-fallback message from the most recent runtime build.
    pub fn model_fallback_message(&self) -> Option<&str> {
        self.model_fallback_message.as_deref()
    }

    /// Replace the current runtime state with a freshly built one.
    ///
    /// Used by the lifecycle methods (`switch_session`, `new_session`,
    /// `fork`, `import_from_jsonl`) that are still pending parity. Exposed
    /// here so the controller can re-dispatch a follow-up task that wires
    /// them up without changing the runtime's structural shape again.
    #[allow(dead_code)] // used by pending parity work in lifecycle methods.
    pub(crate) fn apply(&mut self, result: CreateAgentSessionRuntimeResult) {
        self.session = result.session;
        self.services = result.services;
        self.diagnostics = result.diagnostics;
        self.model_fallback_message = result.model_fallback_message;
    }

    /// Borrow the stored factory so reload-style flows can rebuild the
    /// runtime against a different `cwd` / session manager.
    pub fn factory(&self) -> &CreateAgentSessionRuntimeFactory {
        &self.create_runtime
    }

    /// Tear down the current runtime.
    ///
    /// The TS reference emits a `session_shutdown` extension event and runs
    /// a host-supplied `beforeSessionInvalidate` callback before dropping
    /// the session. Those hooks need extension/host parity that is not yet
    /// in tree, so for now this is a thin wrapper around dropping the
    /// session by-move.
    // TODO(parity): emit `session_shutdown` and run `before_session_invalidate`.
    pub fn dispose(self) {
        let Self {
            session,
            services,
            create_runtime,
            diagnostics,
            model_fallback_message,
        } = self;
        drop(session);
        drop(services);
        drop(create_runtime);
        drop(diagnostics);
        drop(model_fallback_message);
    }
}

/// Build the initial runtime from a factory.
///
/// Mirrors the TS `createAgentSessionRuntime`: invoke the factory once with
/// the initial inputs, then wrap the result in an [`AgentSessionRuntime`].
/// The same factory is stored on the returned runtime and reused for later
/// `/new`, `/resume`, `/fork`, and import flows once those land.
pub async fn create_agent_session_runtime(
    create_runtime: CreateAgentSessionRuntimeFactory,
    options: CreateAgentSessionRuntimeFactoryInput,
) -> Result<AgentSessionRuntime, RuntimeFactoryError> {
    // Guard the stored-cwd contract BEFORE any heavy runtime allocation.
    // When the session was persisted under a cwd that no longer exists
    // on disk, surface a controlled `MissingSessionCwdError` here —
    // pre-factory, pre-services — so neither the agent runtime nor any
    // extension lifecycle hook fires for a broken session.
    let cwd_source = SessionManagerCwdSource {
        sm: &options.session_manager,
    };
    if let Err(e) = assert_session_cwd_exists(&cwd_source, &options.cwd) {
        return Err(Box::new(e) as RuntimeFactoryError);
    }
    let result = (create_runtime)(options).await?;
    Ok(AgentSessionRuntime::new(
        result.session,
        result.services,
        create_runtime.clone(),
        result.diagnostics,
        result.model_fallback_message,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_file_not_found_error_carries_path() {
        let err = SessionImportFileNotFoundError::new("/tmp/missing.jsonl");
        assert_eq!(err.path, PathBuf::from("/tmp/missing.jsonl"));
        assert_eq!(err.to_string(), "file not found: /tmp/missing.jsonl");
    }

    /// UC-sm-007 — when the session's stored cwd is missing on disk,
    /// `create_agent_session_runtime` must fail with a
    /// `MissingSessionCwdError` BEFORE the factory runs. No services
    /// are allocated; no extension lifecycle event fires.
    #[tokio::test]
    async fn create_runtime_rejects_missing_stored_cwd_before_factory() {
        use crate::core::session_cwd::MissingSessionCwdError;

        // Set up a real on-disk session file whose header.cwd points at
        // a directory that does NOT exist. We write the JSONL by hand
        // so we don't need a SessionManager mutator.
        let tmp = tempfile::tempdir().expect("tempdir");
        let real_dir = tmp.path().join("real");
        std::fs::create_dir(&real_dir).unwrap();
        let session_dir = real_dir.join(".hand").join("sessions");
        std::fs::create_dir_all(&session_dir).unwrap();
        let bad_cwd = "/definitely-not-a-real-path-uc-sm-007";
        let session_path = session_dir.join("uc-sm-007.jsonl");
        let header_line = format!(
            "{{\"type\":\"session\",\"data\":{{\"version\":3,\"id\":\"uc-sm-007\",\"timestamp\":0,\"cwd\":\"{}\"}}}}\n",
            bad_cwd
        );
        std::fs::write(&session_path, header_line).unwrap();

        let sm = SessionManager::open(&session_path).expect("open session");

        // The factory MUST NOT be invoked. We use a sentinel that panics
        // if called so the assertion is direct.
        let factory: CreateAgentSessionRuntimeFactory = Arc::new(|_input| {
            Box::pin(async {
                panic!("factory was invoked despite a missing stored cwd");
            })
        });

        let input = CreateAgentSessionRuntimeFactoryInput {
            cwd: real_dir.clone(),
            agent_dir: real_dir.clone(),
            session_manager: sm,
        };
        let outcome = create_agent_session_runtime(factory, input).await;
        let err = match outcome {
            Ok(_) => panic!("expected MissingSessionCwdError, got Ok"),
            Err(e) => e,
        };
        let downcast = err
            .downcast_ref::<MissingSessionCwdError>()
            .expect("error type is MissingSessionCwdError");
        assert_eq!(downcast.issue().session_cwd, PathBuf::from(bad_cwd));
    }

    #[test]
    fn factory_input_holds_supplied_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().to_path_buf();
        let agent_dir = tmp.path().join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();

        let session_manager = SessionManager::in_memory();
        let input = CreateAgentSessionRuntimeFactoryInput {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            session_manager,
        };
        assert_eq!(input.cwd, cwd);
        assert_eq!(input.agent_dir, agent_dir);
        assert!(input.session_manager.is_in_memory());
    }
}
