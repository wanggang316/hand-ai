//! Cwd-bound runtime services for an agent session.
//!
//! TypeScript reference (`pi-mono`): `core/agent-session-services.ts`. The
//! reference exposes [`AgentSessionServices`] as the service container
//! (auth, settings, model registry, resource loader, diagnostics) that an
//! [`crate::core::agent_session::AgentSession`] is later constructed against.
//! That separation lets a runtime swap services for a different effective
//! `cwd` without rebuilding the session immediately.
//!
//! The Rust port is intentionally narrow: it holds the subset of services
//! the existing Rust [`crate::core::agent_session::AgentSession`] already
//! understands. Pieces that depend on Rust-side parity not yet ported
//! (extension runner-driven flag values, dynamic provider registration via
//! the resource loader) are tracked with `TODO(parity)` markers and surface
//! as no-ops here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::Mutex;

use crate::core::auth_storage::{AuthStorage, AuthStorageError};
use crate::core::model_registry::ModelRegistry;
use crate::core::settings::{SettingsError, SettingsManager};

/// Severity of a non-fatal startup issue.
///
/// Matches the TS `AgentSessionRuntimeDiagnostic.type` values
/// (`"info" | "warning" | "error"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticLevel {
    Info,
    Warning,
    Error,
}

/// Non-fatal issue collected while creating services or sessions.
///
/// Runtime creation returns these to the caller instead of printing or
/// exiting; the host (CLI / TUI) decides whether to surface them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionRuntimeDiagnostic {
    pub level: DiagnosticLevel,
    pub message: String,
}

impl AgentSessionRuntimeDiagnostic {
    pub fn info(message: impl Into<String>) -> Self {
        Self {
            level: DiagnosticLevel::Info,
            message: message.into(),
        }
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            level: DiagnosticLevel::Warning,
            message: message.into(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            level: DiagnosticLevel::Error,
            message: message.into(),
        }
    }
}

/// Inputs for building a service container.
///
/// Mirrors the TS `CreateAgentSessionServicesOptions`. Optional fields fall
/// back to project defaults — [`AuthStorage::new`], [`SettingsManager::from_cwd`],
/// and [`ModelRegistry::build`]-equivalent stand-ins. CLI-provided overrides
/// must be resolved to absolute paths before reaching this builder.
#[derive(Default)]
pub struct CreateAgentSessionServicesOptions {
    pub cwd: PathBuf,
    pub agent_dir: Option<PathBuf>,
    pub auth_storage: Option<AuthStorage>,
    pub settings_manager: Option<SettingsManager>,
    pub model_registry: Option<ModelRegistry>,
    // TODO(parity): port `extensionFlagValues` once `ExtensionRunner` exists.
    // TODO(parity): port `resourceLoaderOptions` once `DefaultResourceLoader`
    // gains a parity-equivalent class API.
}

/// Cwd-bound runtime services.
///
/// Cheap to clone via the inner [`Arc`]/[`Mutex`] handles. The [`SettingsManager`]
/// and [`ModelRegistry`] are wrapped in [`Arc<Mutex<_>>`] because their parity
/// counterparts in TS are mutated in place during a session (e.g. registering
/// a provider added by an extension). Hosts that need read-only access can
/// `lock()` for the duration of the read.
#[derive(Clone)]
pub struct AgentSessionServices {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub auth_storage: Arc<AuthStorage>,
    pub settings_manager: Arc<Mutex<SettingsManager>>,
    pub model_registry: Arc<Mutex<ModelRegistry>>,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    // TODO(parity): add `resource_loader: Arc<dyn ResourceLoader>` once the
    // Rust resource-loader is reshaped to expose a per-session class-style
    // facade with `get_extensions()`.
}

impl AgentSessionServices {
    /// Borrow the working directory.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Borrow the agent config directory (`~/.hand/agent` by default).
    pub fn agent_dir(&self) -> &Path {
        &self.agent_dir
    }

    /// Borrow the diagnostics collected during service creation.
    pub fn diagnostics(&self) -> &[AgentSessionRuntimeDiagnostic] {
        &self.diagnostics
    }
}

/// Errors that can surface from [`create_agent_session_services`].
#[derive(Debug, Error)]
pub enum AgentSessionServicesError {
    #[error("failed to load settings: {0}")]
    Settings(#[from] SettingsError),
    #[error("failed to initialise auth storage: {0}")]
    Auth(#[from] AuthStorageError),
    #[error("home directory could not be determined")]
    NoHomeDir,
}

fn default_agent_dir() -> Result<PathBuf, AgentSessionServicesError> {
    let home = dirs::home_dir().ok_or(AgentSessionServicesError::NoHomeDir)?;
    Ok(home.join(".hand").join("agent"))
}

/// Build cwd-bound services.
///
/// Returns the service container plus diagnostics. It does not create an
/// [`crate::core::agent_session::AgentSession`]; that step belongs in the
/// runtime layer so session-level options (model, tools, thinking level)
/// can be resolved against the freshly-built services first.
pub fn create_agent_session_services(
    options: CreateAgentSessionServicesOptions,
) -> Result<AgentSessionServices, AgentSessionServicesError> {
    let cwd = options.cwd;
    let agent_dir = match options.agent_dir {
        Some(p) => p,
        None => default_agent_dir()?,
    };

    let auth_storage = Arc::new(match options.auth_storage {
        Some(a) => a,
        None => AuthStorage::at(agent_dir.join("auth.json")),
    });

    let settings_manager = match options.settings_manager {
        Some(s) => s,
        None => SettingsManager::from_cwd(&cwd)?,
    };

    // TODO(parity): TS uses `ModelRegistry.create(authStorage, agentDir/models.json)`,
    // a registry that knows about provider auth and extension-supplied
    // providers. The Rust `ModelRegistry::build(client)` is a thinner shim;
    // we use a default-construction stand-in for now and let callers replace
    // it via `options.model_registry`.
    let model_registry = options
        .model_registry
        .unwrap_or_else(|| ModelRegistry::build(&model::Client::default()));

    // TODO(parity): port `applyExtensionFlagValues` once an `ExtensionRunner`
    // with a flag-values map exists in the Rust extensions runtime.

    let diagnostics: Vec<AgentSessionRuntimeDiagnostic> = Vec::new();

    Ok(AgentSessionServices {
        cwd,
        agent_dir,
        auth_storage,
        settings_manager: Arc::new(Mutex::new(settings_manager)),
        model_registry: Arc::new(Mutex::new(model_registry)),
        diagnostics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn options_for(cwd: PathBuf, agent_dir: PathBuf) -> CreateAgentSessionServicesOptions {
        CreateAgentSessionServicesOptions {
            cwd,
            agent_dir: Some(agent_dir),
            settings_manager: Some(SettingsManager::in_memory()),
            ..Default::default()
        }
    }

    #[test]
    fn create_services_populates_paths_and_defaults() {
        let tmp = TempDir::new().expect("tempdir");
        let cwd = tmp.path().join("project");
        let agent_dir = tmp.path().join("agent");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&agent_dir).unwrap();

        let services = create_agent_session_services(options_for(cwd.clone(), agent_dir.clone()))
            .expect("services build");

        assert_eq!(services.cwd(), cwd.as_path());
        assert_eq!(services.agent_dir(), agent_dir.as_path());
        assert!(services.diagnostics().is_empty());
        assert_eq!(
            services.auth_storage.path(),
            agent_dir.join("auth.json").as_path()
        );
    }

    #[test]
    fn diagnostic_helpers_assign_levels() {
        assert_eq!(
            AgentSessionRuntimeDiagnostic::info("a").level,
            DiagnosticLevel::Info
        );
        assert_eq!(
            AgentSessionRuntimeDiagnostic::warning("b").level,
            DiagnosticLevel::Warning
        );
        assert_eq!(
            AgentSessionRuntimeDiagnostic::error("c").level,
            DiagnosticLevel::Error
        );
    }

    #[test]
    fn services_are_cloneable_for_shared_ownership() {
        let tmp = TempDir::new().expect("tempdir");
        let cwd = tmp.path().to_path_buf();
        let agent_dir = tmp.path().join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();

        let services =
            create_agent_session_services(options_for(cwd, agent_dir)).expect("services build");
        let cloned = services.clone();
        // Sanity: same Arc pointer for the shared registry.
        assert!(Arc::ptr_eq(
            &services.model_registry,
            &cloned.model_registry
        ));
    }
}
