//! Top-level SDK assembly.
//!
//! Exports [`CreateAgentSessionOptions`] / [`CreateAgentSessionResult`],
//! a curated set of re-exports, and the
//! [`build_default_runtime`] factory that wires every cwd-bound piece
//! (auth, model registry, settings, resource loader, extensions, agent
//! runtime) into a runnable [`AgentSession`].
//!
//! The module today is intentionally light:
//!
//! 1. Re-exports for the curated public surface — `lib.rs` already
//!    re-exports most of these at the crate root, so `core::sdk` exists
//!    primarily as a one-stop import.
//! 2. The public option/result types so consumers can write code
//!    against the shape that will be honoured once
//!    [`build_default_runtime`] becomes meaningful.
//! 3. A [`build_default_runtime`] entry-point that returns an explicit
//!    "not yet wired" error today. The factory cannot be implemented
//!    until [`crate::core::agent_session_runtime::CreateAgentSessionRuntimeFactory`]
//!    has counterparts for `findInitialModel`, the extension runner,
//!    the full `ResourceLoader` facade, and the cwd-bound `Agent`
//!    builder. Each missing dependency is tracked in
//!    `agent_session_services.rs` and `agent_session_runtime.rs`.

use std::path::{Path, PathBuf};

use thiserror::Error;

pub use crate::core::agent_session::{AgentSession, AgentSessionConfig, AgentSessionEvent};
pub use crate::core::agent_session_runtime::{
    AgentSessionRuntime, CreateAgentSessionRuntimeFactory, CreateAgentSessionRuntimeFactoryInput,
    CreateAgentSessionRuntimeFuture, CreateAgentSessionRuntimeResult, RuntimeFactoryError,
    SessionImportFileNotFoundError, create_agent_session_runtime,
};
pub use crate::core::agent_session_services::{
    AgentSessionRuntimeDiagnostic, AgentSessionServices, AgentSessionServicesError,
    CreateAgentSessionServicesOptions, DiagnosticLevel, create_agent_session_services,
};
pub use crate::core::auth_storage::AuthStorage;
pub use crate::core::event_bus::EventBus;
pub use crate::core::model_registry::ModelRegistry;
pub use crate::core::resource_loader::{ResourceLoaderConfig, ResourceLoaderError};
pub use crate::core::session_manager::SessionManager;
pub use crate::core::settings::SettingsManager;

/// Inputs accepted by the SDK-level session factory.
///
/// Direct shape-port of the TS `CreateAgentSessionOptions`. Optional fields
/// fall back to project defaults; CLI-provided overrides should be resolved
/// to absolute paths before they reach the factory so later cwd switches do
/// not reinterpret them.
#[derive(Default)]
pub struct CreateAgentSessionOptions {
    /// Working directory for project-local discovery. Defaults to the
    /// process cwd.
    pub cwd: Option<PathBuf>,
    /// Global config directory. Defaults to `~/.hand/agent`.
    pub agent_dir: Option<PathBuf>,
    /// Optional pre-built auth storage. When omitted the SDK builds one at
    /// `agent_dir/auth.json`.
    pub auth_storage: Option<AuthStorage>,
    /// Optional pre-built model registry.
    pub model_registry: Option<ModelRegistry>,
    /// Optional pre-built settings manager.
    pub settings_manager: Option<SettingsManager>,
    /// Optional pre-built session manager. Defaults to a fresh persisted
    /// session under the project session dir.
    pub session_manager: Option<SessionManager>,
    // TODO(parity): port `model`, `thinking_level`, `scoped_models`,
    // `no_tools`, `tools`, `custom_tools`, `resource_loader`,
    // `session_start_event` once the corresponding Rust pieces (model
    // resolver, extension lifecycle events, class-style ResourceLoader)
    // are in place.
}

/// Result returned by the SDK-level session factory.
///
/// Direct shape-port of the TS `CreateAgentSessionResult`. The fields will
/// be populated once [`build_default_runtime`] is wired up; today the
/// factory returns [`SdkError::NotYetWired`] instead.
pub struct CreateAgentSessionResult {
    /// The created session.
    pub session: AgentSession,
    /// Warning if the session was restored with a different model than
    /// saved.
    pub model_fallback_message: Option<String>,
    // TODO(parity): port `extensions_result: LoadExtensionsResult` once an
    // `ExtensionRunner`-backed runtime exists.
}

/// Error returned by SDK entry-points.
#[derive(Debug, Error)]
pub enum SdkError {
    /// The factory cannot run yet because Rust-side parity is incomplete.
    /// See module docs for the missing pieces.
    #[error(
        "build_default_runtime is not yet wired: missing parity for ExtensionRunner, \
         findInitialModel, and the cwd-bound Agent builder"
    )]
    NotYetWired,
    /// Error surfaced from service construction.
    #[error(transparent)]
    Services(#[from] AgentSessionServicesError),
}

/// Build the default runtime for a target `cwd`.
///
/// Once wired, this will mirror the TS `createAgentSession` factory: build
/// cwd-bound services, resolve the initial model, construct the
/// `AgentSession`, and return an [`AgentSessionRuntime`] holding everything.
///
/// Today the factory only constructs services so callers can begin
/// integrating against the SDK shape; session construction returns
/// [`SdkError::NotYetWired`] until the parity gaps in
/// [`crate::core::agent_session_runtime`] close.
pub async fn build_default_runtime(cwd: impl AsRef<Path>) -> Result<AgentSessionRuntime, SdkError> {
    // Construct services so the call exercises the real path; this both
    // validates that the target `cwd` is usable and produces a meaningful
    // error path for callers that pass a broken layout.
    let _services = create_agent_session_services(CreateAgentSessionServicesOptions {
        cwd: cwd.as_ref().to_path_buf(),
        ..Default::default()
    })?;

    // TODO(parity): build a `CreateAgentSessionRuntimeFactory` that wires
    // `findInitialModel`, the cwd-bound `Agent`, the extension runner, and
    // the resource loader, then call `create_agent_session_runtime` with it.
    Err(SdkError::NotYetWired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn build_default_runtime_returns_not_yet_wired_for_valid_cwd() {
        let tmp = TempDir::new().expect("tempdir");
        match build_default_runtime(tmp.path()).await {
            Err(SdkError::NotYetWired) => {}
            Err(other) => panic!("expected NotYetWired, got {other}"),
            Ok(_) => panic!("expected NotYetWired, got a runtime"),
        }
    }

    #[test]
    fn options_default_uses_none_everywhere() {
        let options = CreateAgentSessionOptions::default();
        assert!(options.cwd.is_none());
        assert!(options.agent_dir.is_none());
        assert!(options.session_manager.is_none());
    }
}
