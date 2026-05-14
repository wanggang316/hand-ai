//! Extension package-source registry.
//!
//! Resolves npm/git/local "sources" into on-disk extension packages and
//! manages installation, updates, and lookups across them.
//!
//! Not to be confused with [`crate::core::package_manager`], which
//! detects programming languages from file extensions and is unrelated.
//! The naming `SourceRegistry` keeps the two concepts visually distinct
//! at every call site.
//!
//! ## Scope of the current port (Tier S2 happy-path)
//!
//! The TS module is ~2400 lines and bundles together:
//!
//! 1. **Source resolution** — given a configured set of npm/git/local
//!    sources, return the on-disk paths to every contained resource.
//! 2. **Network installation** — `npm install`, `git clone`, version
//!    pinning, update detection, atomic upgrades.
//! 3. **Settings persistence** — adding/removing sources from
//!    `Settings::packages` and writing the YAML back to disk.
//! 4. **Manifest interpretation** — package.json `pi:` blocks, glob
//!    pattern allow-lists, `.gitignore`-aware file walking, override
//!    pattern syntax (`+/-/!`).
//!
//! This Rust port currently implements **(1) only**, in its happy-path
//! form: read configured sources from `Settings`, look up the cached
//! install directory layout matching the TS reference, and walk it for
//! resources by file extension. Methods that depend on (2)/(3)/(4)
//! return [`SourceRegistryError::NotYetImplemented`] and are marked
//! `// TODO(parity): port npm/git install logic — see docs/exec-plans/parity-completion.md`.
//!
//! ## Scope vs. SourceScope
//!
//! The TS module uses `SourceScope = "user" | "project" | "temporary"`.
//! There is already a [`crate::core::source_info::SourceScope`] enum in
//! the Rust codebase but with different semantics
//! (`Builtin/User/Project/Extension`); reusing it would be incorrect.
//! This module defines its own [`InstallScope`] for clarity.

use crate::core::settings::{PackageSource, SettingsManager, SettingsScope};
use crate::utils::child_process;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use thiserror::Error;

/// On-disk install scope used by the source registry. Mirrors the TS
/// reference's `SourceScope` (`"user" | "project" | "temporary"`).
///
/// Distinct from [`crate::core::source_info::SourceScope`] which is used
/// by the resource loader for resource-discovery shadowing semantics —
/// the two enums describe different concepts. See module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstallScope {
    /// User-global install — persisted under the agent dir
    /// (`~/.hand/agent/...`).
    User,
    /// Project-local install — persisted under `<cwd>/.hand/...`.
    Project,
    /// Ephemeral install — under the OS tmp dir, used for one-shot
    /// `--extensions` CLI overrides.
    Temporary,
}

impl InstallScope {
    pub fn as_str(self) -> &'static str {
        match self {
            InstallScope::User => "user",
            InstallScope::Project => "project",
            InstallScope::Temporary => "temporary",
        }
    }
}

/// Origin of a [`ResolvedResource`]: contributed by a configured package
/// source, or auto-discovered/explicit from the user's local file paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceOrigin {
    /// Came from a `packages:` entry (npm/git/local package).
    Package,
    /// Came from `extensions:`/`skills:`/`prompts:`/`themes:` entries
    /// or auto-discovered under the agent/project dir convention paths.
    TopLevel,
}

/// Metadata about a resolved resource — where it came from, attached to
/// every entry in [`ResolvedPaths`] for downstream UI grouping.
#[derive(Debug, Clone)]
pub struct PathMetadata {
    /// The configured source string, or `"local"` for top-level
    /// settings entries, or `"auto"` for auto-discovered files.
    pub source: String,
    /// Install scope.
    pub scope: InstallScope,
    /// Whether this file came from a `packages:` entry or a top-level
    /// settings entry / convention path.
    pub origin: ResourceOrigin,
    /// For package resources, the install directory; for top-level, the
    /// settings layer's base dir. `None` is fine for local entries that
    /// resolve to absolute paths.
    pub base_dir: Option<PathBuf>,
}

/// One discovered resource on disk.
#[derive(Debug, Clone)]
pub struct ResolvedResource {
    /// Absolute path to the resource file.
    pub path: PathBuf,
    /// Whether the resource is enabled in the merged settings (i.e.
    /// not filtered out by package allow-lists or override patterns).
    pub enabled: bool,
    /// Where the resource came from.
    pub metadata: PathMetadata,
}

/// All resources resolved by [`SourceRegistry::resolve`], grouped by kind.
#[derive(Debug, Clone, Default)]
pub struct ResolvedPaths {
    pub extensions: Vec<ResolvedResource>,
    pub skills: Vec<ResolvedResource>,
    pub prompts: Vec<ResolvedResource>,
    pub themes: Vec<ResolvedResource>,
}

/// What to do when [`SourceRegistry::resolve`] hits a configured source
/// whose install directory does not yet exist on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingSourceAction {
    /// Install the source via the appropriate package manager.
    Install,
    /// Skip the source for this resolve call.
    Skip,
    /// Error out.
    Error,
}

/// Lifecycle event emitted via [`ProgressCallback`].
#[derive(Debug, Clone)]
pub struct ProgressEvent {
    pub event_type: ProgressEventType,
    pub action: ProgressAction,
    pub source: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressEventType {
    Start,
    Progress,
    Complete,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressAction {
    Install,
    Remove,
    Update,
    Clone,
    Pull,
}

/// Caller-supplied progress callback for long-running install/update
/// operations. Cloned and shared across the registry by the host.
pub type ProgressCallback = Box<dyn Fn(&ProgressEvent) + Send + Sync + 'static>;

/// Options for [`SourceRegistry::install`] / [`SourceRegistry::remove`].
#[derive(Debug, Clone, Default)]
pub struct PackageInstallOptions {
    /// `true` → install into the project (`.hand/`) scope; `false` → user
    /// (`~/.hand/agent/`) scope. Mirrors the TS `{ local?: boolean }`.
    pub local: bool,
}

/// Options for [`SourceRegistry::resolve_extension_sources`].
#[derive(Debug, Clone, Default)]
pub struct ResolveExtensionSourcesOptions {
    pub local: bool,
    pub temporary: bool,
}

/// One configured package source as surfaced to the UI.
#[derive(Debug, Clone)]
pub struct ConfiguredPackage {
    /// Source spec exactly as the user wrote it.
    pub source: String,
    /// Which scope the entry lives in.
    pub scope: InstallScope,
    /// Whether the entry was object-form (had per-kind allow-lists).
    pub filtered: bool,
    /// Resolved on-disk install path if present.
    pub installed_path: Option<PathBuf>,
}

/// Errors raised by the registry.
#[derive(Debug, Error)]
pub enum SourceRegistryError {
    #[error("not yet implemented: {0}")]
    NotYetImplemented(&'static str),
    #[error("unsupported source: {0}")]
    UnsupportedSource(String),
    #[error("local source path does not exist: {path}", path = .0.display())]
    LocalPathMissing(PathBuf),
    #[error("I/O error reading {path}: {source}", path = .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Spawning or waiting on `npm`/`git` failed.
    #[error("process error: {0}")]
    Process(String),
    /// `npm install` / `git clone` exited non-zero. Stderr is included
    /// verbatim so callers can surface it to the user.
    #[error("{program} exited {code:?}: {stderr}")]
    CommandFailed {
        program: String,
        code: Option<i32>,
        stderr: String,
    },
    /// A persistence helper was called on a registry that was constructed
    /// without a [`SettingsManager`] handle — see
    /// [`DefaultSourceRegistry::with_settings_manager`].
    #[error("registry has no SettingsManager handle for persistence")]
    NoSettingsManager,
    /// Settings I/O / serialisation failure surfaced from `SettingsManager::save`.
    #[error("settings persistence failed: {0}")]
    Settings(#[from] crate::core::settings::SettingsError),
}

/// The TS `PackageManager` interface, ported as a Rust trait.
///
/// Methods are async because the network-bound implementations
/// (install/update/remove) need to spawn child processes and do I/O.
/// The current port does not implement those — see module docs.
#[async_trait::async_trait]
pub trait SourceRegistry: Send + Sync {
    /// Resolve every configured package and top-level path, returning
    /// the absolute paths to every discovered resource grouped by kind.
    ///
    /// `on_missing` is called for every package whose install directory
    /// is not present on disk; the callback decides whether to install,
    /// skip, or error. Pass `None` to install unconditionally
    /// (matches the TS default).
    async fn resolve(
        &self,
        on_missing: Option<MissingSourceCallback>,
    ) -> Result<ResolvedPaths, SourceRegistryError>;

    /// Install a single source.
    async fn install(
        &self,
        source: &str,
        options: PackageInstallOptions,
    ) -> Result<(), SourceRegistryError>;

    /// Install + add to settings.
    async fn install_and_persist(
        &self,
        source: &str,
        options: PackageInstallOptions,
    ) -> Result<(), SourceRegistryError>;

    /// Remove a single source's install.
    async fn remove(
        &self,
        source: &str,
        options: PackageInstallOptions,
    ) -> Result<(), SourceRegistryError>;

    /// Remove + drop from settings. Returns `true` if settings changed.
    async fn remove_and_persist(
        &self,
        source: &str,
        options: PackageInstallOptions,
    ) -> Result<bool, SourceRegistryError>;

    /// Update a single configured source (if `Some`) or every configured
    /// source (if `None`).
    async fn update(&self, source: Option<&str>) -> Result<(), SourceRegistryError>;

    /// Snapshot of configured packages from both layers, with resolved
    /// install paths populated when the install dir exists.
    fn list_configured_packages(&self) -> Vec<ConfiguredPackage>;

    /// Resolve a one-shot list of sources, typically the `--extensions`
    /// CLI flag. Does not mutate settings; installs to whichever scope
    /// the options select.
    async fn resolve_extension_sources(
        &self,
        sources: &[String],
        options: ResolveExtensionSourcesOptions,
    ) -> Result<ResolvedPaths, SourceRegistryError>;

    /// Add `source` to the appropriate `Settings` scope. Returns `true`
    /// when the list changed (i.e. the source wasn't already present).
    fn add_source_to_settings(
        &self,
        source: &str,
        options: PackageInstallOptions,
    ) -> Result<bool, SourceRegistryError>;

    /// Remove `source` from the appropriate `Settings` scope. Returns
    /// `true` when the list changed.
    fn remove_source_from_settings(
        &self,
        source: &str,
        options: PackageInstallOptions,
    ) -> Result<bool, SourceRegistryError>;

    /// Replace the progress callback. Pass `None` to clear.
    fn set_progress_callback(&self, callback: Option<ProgressCallback>);

    /// Where this source would be installed under `scope`. Returns
    /// `None` if the install dir does not exist.
    fn get_installed_path(&self, source: &str, scope: InstallScope) -> Option<PathBuf>;
}

/// Caller-side decision callback for missing sources.
pub type MissingSourceCallback = Box<
    dyn Fn(
            &str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = MissingSourceAction> + Send + 'static>,
        > + Send
        + Sync
        + 'static,
>;

// ---------------------------------------------------------------------------
// ProcessRunner — abstracts npm/git shell-out so tests can mock
// ---------------------------------------------------------------------------

/// Result of one [`ProcessRunner::run`] invocation.
#[derive(Debug, Clone, Default)]
pub struct ProcessRunResult {
    pub exit_code: Option<i32>,
    pub stderr: String,
}

impl ProcessRunResult {
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }

    pub fn ok() -> Self {
        Self {
            exit_code: Some(0),
            stderr: String::new(),
        }
    }
}

/// Abstracts shell-out for npm/git so install/remove/update tests can
/// inject deterministic responses without spawning a real process.
///
/// The trait is intentionally narrow (one method, no streaming) — match
/// it to [`crate::utils::version_check::VersionFetcher`]'s pattern.
#[async_trait::async_trait]
pub trait ProcessRunner: Send + Sync {
    /// Run `program args…` in `cwd`. Implementations should not surface
    /// stdout — the registry only ever cares about exit code and stderr.
    async fn run(
        &self,
        program: &str,
        args: &[&str],
        cwd: Option<&Path>,
    ) -> Result<ProcessRunResult, SourceRegistryError>;
}

/// Real [`ProcessRunner`] that shells out via [`child_process::spawn_with_output`].
pub struct DefaultProcessRunner;

#[async_trait::async_trait]
impl ProcessRunner for DefaultProcessRunner {
    async fn run(
        &self,
        program: &str,
        args: &[&str],
        cwd: Option<&Path>,
    ) -> Result<ProcessRunResult, SourceRegistryError> {
        let output = child_process::spawn_with_output(program, args, cwd)
            .await
            .map_err(|e| SourceRegistryError::Process(e.to_string()))?;
        Ok(ProcessRunResult {
            exit_code: output.exit_code,
            stderr: output.stderr_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// DefaultSourceRegistry
// ---------------------------------------------------------------------------

/// Default in-process implementation of [`SourceRegistry`].
///
/// Holds the agent + cwd dirs needed to compute install paths, a
/// snapshot of the per-layer [`Settings`] for read-side resolve, an
/// optional shared [`SettingsManager`] handle so the persistence
/// helpers ([`SourceRegistry::add_source_to_settings`] et al.) can
/// commit changes back to YAML, and a [`ProcessRunner`] used by the
/// install/remove/update path so tests can mock npm/git.
pub struct DefaultSourceRegistry {
    cwd: PathBuf,
    agent_dir: PathBuf,
    /// Cached per-layer settings snapshot. Behind a mutex so persistence
    /// helpers running on `&self` can refresh after a write.
    settings_layers: Mutex<(
        crate::core::settings::Settings,
        crate::core::settings::Settings,
    )>,
    settings_manager: Option<Arc<Mutex<SettingsManager>>>,
    runner: Arc<dyn ProcessRunner>,
    progress_callback: std::sync::Mutex<Option<ProgressCallback>>,
}

impl DefaultSourceRegistry {
    /// Construct from a [`SettingsManager`] snapshot. The settings layers
    /// are cloned at construction time; subsequent mutations to the
    /// manager are not reflected without rebuilding the registry.
    /// Persistence helpers (`add_source_to_settings` /
    /// `remove_source_from_settings` / `install_and_persist` /
    /// `remove_and_persist`) will return
    /// [`SourceRegistryError::NoSettingsManager`] — call
    /// [`Self::with_settings_manager`] for a registry that can write
    /// settings back.
    pub fn new(cwd: PathBuf, agent_dir: PathBuf, settings_manager: &SettingsManager) -> Self {
        Self {
            cwd,
            agent_dir,
            settings_layers: Mutex::new((
                settings_manager.global_layer().clone(),
                settings_manager.project_layer().clone(),
            )),
            settings_manager: None,
            runner: Arc::new(DefaultProcessRunner),
            progress_callback: std::sync::Mutex::new(None),
        }
    }

    /// Construct from a shared [`SettingsManager`] handle. The same
    /// handle is used for read-side layer access *and* for the
    /// persistence helpers — `set_packages` + `save` round-trips reflect
    /// in subsequent `list_configured_packages` / `resolve` calls.
    pub fn with_settings_manager(
        cwd: PathBuf,
        agent_dir: PathBuf,
        settings_manager: Arc<Mutex<SettingsManager>>,
    ) -> Self {
        let (global, project) = {
            let guard = settings_manager.lock().expect("settings manager poisoned");
            (guard.global_layer().clone(), guard.project_layer().clone())
        };
        Self {
            cwd,
            agent_dir,
            settings_layers: Mutex::new((global, project)),
            settings_manager: Some(settings_manager),
            runner: Arc::new(DefaultProcessRunner),
            progress_callback: std::sync::Mutex::new(None),
        }
    }

    /// Construct directly from explicit layer values. Test-only and for
    /// callers that have already loaded the layers separately.
    #[doc(hidden)]
    pub fn from_layers(
        cwd: PathBuf,
        agent_dir: PathBuf,
        settings_global: crate::core::settings::Settings,
        settings_project: crate::core::settings::Settings,
    ) -> Self {
        Self {
            cwd,
            agent_dir,
            settings_layers: Mutex::new((settings_global, settings_project)),
            settings_manager: None,
            runner: Arc::new(DefaultProcessRunner),
            progress_callback: std::sync::Mutex::new(None),
        }
    }

    /// Override the [`ProcessRunner`]. Test-only — production callers
    /// stick with [`DefaultProcessRunner`].
    #[doc(hidden)]
    pub fn with_runner(mut self, runner: Arc<dyn ProcessRunner>) -> Self {
        self.runner = runner;
        self
    }

    /// Snapshot the cached `(global, project)` layer pair.
    fn layer_snapshot(
        &self,
    ) -> (
        crate::core::settings::Settings,
        crate::core::settings::Settings,
    ) {
        let guard = self.settings_layers.lock().expect("layer lock poisoned");
        guard.clone()
    }

    /// Forward an event to the registered progress callback. Used by
    /// the install/remove/update path; production callers see start /
    /// progress / complete / error events for each long-running op.
    fn emit_progress(&self, event: ProgressEvent) {
        if let Ok(guard) = self.progress_callback.lock()
            && let Some(cb) = guard.as_ref()
        {
            cb(&event);
        }
    }

    /// Wrap an install/remove/update operation with start/complete/error
    /// progress events. Mirrors the TS `withProgress` helper.
    async fn with_progress<F, Fut>(
        &self,
        action: ProgressAction,
        source: &str,
        message: &str,
        operation: F,
    ) -> Result<(), SourceRegistryError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(), SourceRegistryError>>,
    {
        self.emit_progress(ProgressEvent {
            event_type: ProgressEventType::Start,
            action,
            source: source.to_string(),
            message: Some(message.to_string()),
        });
        match operation().await {
            Ok(()) => {
                self.emit_progress(ProgressEvent {
                    event_type: ProgressEventType::Complete,
                    action,
                    source: source.to_string(),
                    message: None,
                });
                Ok(())
            }
            Err(e) => {
                self.emit_progress(ProgressEvent {
                    event_type: ProgressEventType::Error,
                    action,
                    source: source.to_string(),
                    message: Some(e.to_string()),
                });
                Err(e)
            }
        }
    }

    /// Root directory under which `npm install --prefix <root>` lays
    /// down `node_modules/<pkg>` for `scope`. Mirrors the TS
    /// `getNpmInstallRoot` for the non-global case (we always use a
    /// directory-scoped install rather than `npm install -g`).
    fn npm_install_root(&self, scope: InstallScope) -> PathBuf {
        match scope {
            InstallScope::User => self.agent_dir.join("npm"),
            InstallScope::Project => self.cwd.join(".hand/npm"),
            InstallScope::Temporary => std::env::temp_dir()
                .join("pi-extensions/npm")
                .join(short_hash("temporary")),
        }
    }

    /// Root directory under which git clones live for `scope`. Returns
    /// `None` for [`InstallScope::Temporary`] which uses an unrooted
    /// per-source tmp dir (no shared parent worth tracking).
    fn git_install_root(&self, scope: InstallScope) -> Option<PathBuf> {
        match scope {
            InstallScope::User => Some(self.agent_dir.join("git")),
            InstallScope::Project => Some(self.cwd.join(".hand/git")),
            InstallScope::Temporary => None,
        }
    }

    /// Ensure the npm install root has a minimal `package.json` so
    /// `npm install --prefix` doesn't refuse to operate. Mirrors the TS
    /// `ensureNpmProject`.
    fn ensure_npm_project(&self, install_root: &Path) -> Result<(), SourceRegistryError> {
        if !install_root.exists() {
            std::fs::create_dir_all(install_root).map_err(|source| SourceRegistryError::Io {
                path: install_root.to_path_buf(),
                source,
            })?;
        }
        Self::ensure_gitignore(install_root)?;
        let package_json = install_root.join("package.json");
        if !package_json.exists() {
            let body = "{\n  \"name\": \"hand-extensions\",\n  \"private\": true\n}\n";
            std::fs::write(&package_json, body).map_err(|source| SourceRegistryError::Io {
                path: package_json,
                source,
            })?;
        }
        Ok(())
    }

    /// Drop a `.gitignore` next to the install root so the cache dir
    /// doesn't leak into user repos. Mirrors the TS `ensureGitIgnore`.
    fn ensure_gitignore(dir: &Path) -> Result<(), SourceRegistryError> {
        if !dir.exists() {
            std::fs::create_dir_all(dir).map_err(|source| SourceRegistryError::Io {
                path: dir.to_path_buf(),
                source,
            })?;
        }
        let path = dir.join(".gitignore");
        if !path.exists() {
            std::fs::write(&path, "*\n!.gitignore\n")
                .map_err(|source| SourceRegistryError::Io { path, source })?;
        }
        Ok(())
    }

    /// Resolve the install scope a `local: bool` option maps to.
    /// Mirrors the TS `options?.local ? "project" : "user"`.
    fn scope_from_options(options: &PackageInstallOptions) -> InstallScope {
        if options.local {
            InstallScope::Project
        } else {
            InstallScope::User
        }
    }

    /// Run `npm install <spec> --prefix <root>` after preparing the
    /// install root. Idempotent — running twice with the same spec is
    /// a no-op for npm itself.
    async fn install_npm(
        &self,
        npm: &NpmSource,
        scope: InstallScope,
    ) -> Result<(), SourceRegistryError> {
        let root = self.npm_install_root(scope);
        self.ensure_npm_project(&root)?;
        let prefix = root.to_string_lossy().into_owned();
        let result = self
            .runner
            .run("npm", &["install", &npm.spec, "--prefix", &prefix], None)
            .await?;
        if !result.success() {
            return Err(SourceRegistryError::CommandFailed {
                program: "npm".into(),
                code: result.exit_code,
                stderr: result.stderr,
            });
        }
        Ok(())
    }

    /// Run `npm uninstall <name> --prefix <root>`. Skips when the root
    /// directory does not exist (nothing to remove).
    async fn uninstall_npm(
        &self,
        npm: &NpmSource,
        scope: InstallScope,
    ) -> Result<(), SourceRegistryError> {
        let root = self.npm_install_root(scope);
        if !root.exists() {
            return Ok(());
        }
        let prefix = root.to_string_lossy().into_owned();
        let result = self
            .runner
            .run("npm", &["uninstall", &npm.name, "--prefix", &prefix], None)
            .await?;
        if !result.success() {
            return Err(SourceRegistryError::CommandFailed {
                program: "npm".into(),
                code: result.exit_code,
                stderr: result.stderr,
            });
        }
        Ok(())
    }

    /// `git clone <repo> <dest>` after creating the parent dir and
    /// landing a `.gitignore`. Mirrors the TS `installGit`. No-op if the
    /// destination already exists.
    async fn install_git(
        &self,
        git: &GitSource,
        scope: InstallScope,
    ) -> Result<(), SourceRegistryError> {
        let dest = self.git_install_path(git, scope);
        if dest.exists() {
            return Ok(());
        }
        if let Some(root) = self.git_install_root(scope) {
            Self::ensure_gitignore(&root)?;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|source| SourceRegistryError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let dest_str = dest.to_string_lossy().into_owned();
        let result = self
            .runner
            .run("git", &["clone", &git.repo, &dest_str], None)
            .await?;
        if !result.success() {
            return Err(SourceRegistryError::CommandFailed {
                program: "git".into(),
                code: result.exit_code,
                stderr: result.stderr,
            });
        }
        Ok(())
    }

    /// `git pull` inside the install dir. Returns silently when the
    /// install dir does not yet exist (caller is expected to install
    /// first; the TS reference has the same shape via `installGit`).
    async fn update_git(
        &self,
        git: &GitSource,
        scope: InstallScope,
    ) -> Result<(), SourceRegistryError> {
        let dest = self.git_install_path(git, scope);
        if !dest.exists() {
            return self.install_git(git, scope).await;
        }
        let result = self.runner.run("git", &["pull"], Some(&dest)).await?;
        if !result.success() {
            return Err(SourceRegistryError::CommandFailed {
                program: "git".into(),
                code: result.exit_code,
                stderr: result.stderr,
            });
        }
        Ok(())
    }

    /// Recursively delete the package's install dir.
    fn remove_install_dir(path: &Path) -> Result<(), SourceRegistryError> {
        if !path.exists() {
            return Ok(());
        }
        std::fs::remove_dir_all(path).map_err(|source| SourceRegistryError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }

    /// Persistence helper: route an in-memory layer mutation + save
    /// through the shared [`SettingsManager`]. Returns
    /// [`SourceRegistryError::NoSettingsManager`] when no manager handle
    /// was supplied at construction.
    fn with_settings_lock<F, R>(&self, f: F) -> Result<R, SourceRegistryError>
    where
        F: FnOnce(&mut SettingsManager) -> Result<R, SourceRegistryError>,
    {
        let mgr = self
            .settings_manager
            .as_ref()
            .ok_or(SourceRegistryError::NoSettingsManager)?;
        let mut guard = mgr.lock().expect("settings manager poisoned");
        f(&mut guard)
    }

    /// Map [`InstallScope`] (which has a Temporary variant) to
    /// [`SettingsScope`] (which doesn't). Temporary scopes can never be
    /// persisted — caller has already filtered.
    fn settings_scope(scope: InstallScope) -> Option<SettingsScope> {
        match scope {
            InstallScope::User => Some(SettingsScope::Global),
            InstallScope::Project => Some(SettingsScope::Project),
            InstallScope::Temporary => None,
        }
    }

    /// Refresh the cached `(global, project)` snapshot from the live
    /// `SettingsManager` after a write. Keeps `resolve` /
    /// `list_configured_packages` reflective of post-write state. No-op
    /// when the registry has no `SettingsManager` handle.
    fn refresh_settings_snapshot(&self) {
        let Some(mgr) = self.settings_manager.as_ref() else {
            return;
        };
        let mgr_guard = mgr.lock().expect("settings manager poisoned");
        let mut layers = self.settings_layers.lock().expect("layer lock poisoned");
        layers.0 = mgr_guard.global_layer().clone();
        layers.1 = mgr_guard.project_layer().clone();
    }

    fn base_dir_for_scope(&self, scope: InstallScope) -> PathBuf {
        match scope {
            InstallScope::Project => self.cwd.join(".hand"),
            InstallScope::User => self.agent_dir.clone(),
            // For temporary scopes the base is the cwd, matching TS.
            InstallScope::Temporary => self.cwd.clone(),
        }
    }

    /// Resolve a (potentially `~`-prefixed, potentially relative) path
    /// against `base_dir`. Mirrors the TS `resolvePathFromBase`.
    fn resolve_path_from_base(&self, input: &str, base_dir: &Path) -> PathBuf {
        let trimmed = input.trim();
        if trimmed == "~" {
            return home_dir();
        }
        if let Some(rest) = trimmed.strip_prefix("~/") {
            return home_dir().join(rest);
        }
        if let Some(rest) = trimmed.strip_prefix('~') {
            return home_dir().join(rest);
        }
        if Path::new(trimmed).is_absolute() {
            return PathBuf::from(trimmed);
        }
        base_dir.join(trimmed)
    }

    /// Compute the install path for an npm-style source.
    ///
    /// Mirrors the TS `getNpmInstallPath`:
    /// - `user`: `<global_npm_root>/<pkg>` (we can't easily query npm
    ///   root without running it; we use a deterministic fallback under
    ///   the agent dir to keep the function pure).
    /// - `project`: `<cwd>/.hand/npm/node_modules/<pkg>`.
    /// - `temporary`: `<tmpdir>/pi-extensions/npm/<hash>/node_modules/<pkg>`.
    ///
    /// TODO(parity): query the real npm prefix for `user` scope by
    /// invoking `npm root -g` (gated on the install logic port).
    fn npm_install_path(&self, parsed: &NpmSource, scope: InstallScope) -> PathBuf {
        match scope {
            InstallScope::User => self.agent_dir.join("npm/node_modules").join(&parsed.name),
            InstallScope::Project => self.cwd.join(".hand/npm/node_modules").join(&parsed.name),
            InstallScope::Temporary => std::env::temp_dir()
                .join("pi-extensions/npm")
                .join(short_hash(&parsed.name))
                .join("node_modules")
                .join(&parsed.name),
        }
    }

    /// Compute the install path for a git source. Mirrors the TS
    /// `getGitInstallPath`.
    fn git_install_path(&self, parsed: &GitSource, scope: InstallScope) -> PathBuf {
        match scope {
            InstallScope::User => self
                .agent_dir
                .join("git")
                .join(&parsed.host)
                .join(&parsed.path),
            InstallScope::Project => self
                .cwd
                .join(".hand/git")
                .join(&parsed.host)
                .join(&parsed.path),
            InstallScope::Temporary => std::env::temp_dir()
                .join("pi-extensions")
                .join(format!("git-{}", parsed.host))
                .join(short_hash(&parsed.path))
                .join(&parsed.path),
        }
    }

    /// Walk a directory looking for resource files of `kind`. Returns
    /// every absolute path that ends with the matching extension.
    /// Skips hidden entries (`.foo`) and `node_modules`. This is the
    /// happy-path file walker — `.gitignore` semantics, manifest
    /// allow-lists, and override patterns are deferred.
    ///
    /// TODO(parity): port `.gitignore`/`.ignore`/`.fdignore` filtering,
    /// manifest pattern matching, override patterns (`+/-/!`).
    fn walk_resource_files(&self, root: &Path, kind: ResourceKind) -> Vec<PathBuf> {
        let mut out = Vec::new();
        walk_dir_inner(root, kind, &mut out);
        out.sort();
        out
    }

    /// Convention-path discovery under a single base dir for one scope.
    /// `<base>/{extensions,skills,prompts,themes}` + the corresponding
    /// settings overrides resolved against `base`.
    ///
    /// Adds entries to `paths` with `enabled = true` when no override
    /// list is present, otherwise marks files according to the override
    /// list. Pattern matching is degraded — see TODO(parity) above.
    fn add_scope_top_level(
        &self,
        scope: InstallScope,
        layer: &crate::core::settings::Settings,
        base_dir: &Path,
        paths: &mut ResolvedPaths,
    ) {
        let make_metadata = |source: &str| PathMetadata {
            source: source.to_string(),
            scope,
            origin: ResourceOrigin::TopLevel,
            base_dir: Some(base_dir.to_path_buf()),
        };

        // Settings-listed entries (resolved against base_dir).
        let resolve_for_kind =
            |entries: &[String], kind: ResourceKind, into: &mut Vec<ResolvedResource>| {
                for entry in entries {
                    let resolved = self.resolve_path_from_base(entry, base_dir);
                    if !resolved.exists() {
                        continue;
                    }
                    if resolved.is_file() {
                        into.push(ResolvedResource {
                            path: resolved,
                            enabled: true,
                            metadata: make_metadata("local"),
                        });
                        continue;
                    }
                    if resolved.is_dir() {
                        for f in self.walk_resource_files(&resolved, kind) {
                            into.push(ResolvedResource {
                                path: f,
                                enabled: true,
                                metadata: make_metadata("local"),
                            });
                        }
                    }
                }
            };

        resolve_for_kind(
            layer.extensions(),
            ResourceKind::Extensions,
            &mut paths.extensions,
        );
        resolve_for_kind(layer.skills(), ResourceKind::Skills, &mut paths.skills);
        resolve_for_kind(layer.prompts(), ResourceKind::Prompts, &mut paths.prompts);
        resolve_for_kind(layer.themes(), ResourceKind::Themes, &mut paths.themes);

        // Convention-path discovery under `<base>/{kind}`.
        let auto_for_kind = |kind: ResourceKind, into: &mut Vec<ResolvedResource>| {
            let dir = base_dir.join(kind.dir_name());
            if !dir.is_dir() {
                return;
            }
            for f in self.walk_resource_files(&dir, kind) {
                into.push(ResolvedResource {
                    path: f,
                    enabled: true,
                    metadata: make_metadata("auto"),
                });
            }
        };
        auto_for_kind(ResourceKind::Extensions, &mut paths.extensions);
        auto_for_kind(ResourceKind::Skills, &mut paths.skills);
        auto_for_kind(ResourceKind::Prompts, &mut paths.prompts);
        auto_for_kind(ResourceKind::Themes, &mut paths.themes);
    }

    /// Walk a single package install root for resources. Happy-path:
    /// look for `<root>/{kind}/...` directories. Manifest filters and
    /// allow-lists are deferred.
    ///
    /// TODO(parity): port `package.json#pi` manifest, glob entries,
    /// override patterns, and per-kind filter objects.
    fn add_package_resources(
        &self,
        package_root: &Path,
        source: &str,
        scope: InstallScope,
        paths: &mut ResolvedPaths,
    ) {
        for kind in ResourceKind::all().iter().copied() {
            let dir = package_root.join(kind.dir_name());
            if !dir.is_dir() {
                continue;
            }
            let into: &mut Vec<ResolvedResource> = match kind {
                ResourceKind::Extensions => &mut paths.extensions,
                ResourceKind::Skills => &mut paths.skills,
                ResourceKind::Prompts => &mut paths.prompts,
                ResourceKind::Themes => &mut paths.themes,
            };
            for f in self.walk_resource_files(&dir, kind) {
                into.push(ResolvedResource {
                    path: f,
                    enabled: true,
                    metadata: PathMetadata {
                        source: source.to_string(),
                        scope,
                        origin: ResourceOrigin::Package,
                        base_dir: Some(package_root.to_path_buf()),
                    },
                });
            }
        }
    }
}

#[async_trait::async_trait]
impl SourceRegistry for DefaultSourceRegistry {
    async fn resolve(
        &self,
        _on_missing: Option<MissingSourceCallback>,
    ) -> Result<ResolvedPaths, SourceRegistryError> {
        let mut paths = ResolvedPaths::default();
        let (settings_global, settings_project) = self.layer_snapshot();

        // Project layer first so its resources win on later dedup.
        for pkg in settings_project.packages() {
            let source = pkg.source().to_string();
            if let Some(installed) = self.get_installed_path(&source, InstallScope::Project) {
                self.add_package_resources(&installed, &source, InstallScope::Project, &mut paths);
            }
        }
        for pkg in settings_global.packages() {
            let source = pkg.source().to_string();
            if let Some(installed) = self.get_installed_path(&source, InstallScope::User) {
                self.add_package_resources(&installed, &source, InstallScope::User, &mut paths);
            }
        }

        // Top-level / convention-path discovery for both scopes.
        self.add_scope_top_level(
            InstallScope::Project,
            &settings_project,
            &self.cwd.join(".hand"),
            &mut paths,
        );
        self.add_scope_top_level(
            InstallScope::User,
            &settings_global,
            &self.agent_dir,
            &mut paths,
        );

        Ok(paths)
    }

    async fn install(
        &self,
        source: &str,
        options: PackageInstallOptions,
    ) -> Result<(), SourceRegistryError> {
        let scope = Self::scope_from_options(&options);
        let parsed = parse_source(source);
        let message = format!("Installing {source}...");
        self.with_progress(ProgressAction::Install, source, &message, || async {
            match parsed {
                ParsedSource::Npm(npm) => self.install_npm(&npm, scope).await,
                ParsedSource::Git(git) => self.install_git(&git, scope).await,
                ParsedSource::Local(local) => {
                    let base = self.base_dir_for_scope(scope);
                    let resolved = self.resolve_path_from_base(&local, &base);
                    if !resolved.exists() {
                        return Err(SourceRegistryError::LocalPathMissing(resolved));
                    }
                    Ok(())
                }
            }
        })
        .await
    }

    async fn install_and_persist(
        &self,
        source: &str,
        options: PackageInstallOptions,
    ) -> Result<(), SourceRegistryError> {
        // Bail early if there's no settings handle — calling install
        // without persistence would leave the UI in a confused state
        // (a fresh install dir but no settings entry pointing at it).
        if self.settings_manager.is_none() {
            return Err(SourceRegistryError::NoSettingsManager);
        }
        self.install(source, options.clone()).await?;
        self.add_source_to_settings(source, options)?;
        Ok(())
    }

    async fn remove(
        &self,
        source: &str,
        options: PackageInstallOptions,
    ) -> Result<(), SourceRegistryError> {
        let scope = Self::scope_from_options(&options);
        let parsed = parse_source(source);
        let message = format!("Removing {source}...");
        self.with_progress(ProgressAction::Remove, source, &message, || async {
            match parsed {
                ParsedSource::Npm(npm) => {
                    // Best-effort: ask npm to uninstall (so the
                    // package.json is updated), then drop the per-pkg
                    // dir as a belt-and-braces cleanup.
                    self.uninstall_npm(&npm, scope).await?;
                    let dir = self.npm_install_path(&npm, scope);
                    Self::remove_install_dir(&dir)
                }
                ParsedSource::Git(git) => {
                    let dir = self.git_install_path(&git, scope);
                    Self::remove_install_dir(&dir)
                }
                // Local paths are user-managed — nothing to delete.
                ParsedSource::Local(_) => Ok(()),
            }
        })
        .await
    }

    async fn remove_and_persist(
        &self,
        source: &str,
        options: PackageInstallOptions,
    ) -> Result<bool, SourceRegistryError> {
        if self.settings_manager.is_none() {
            return Err(SourceRegistryError::NoSettingsManager);
        }
        self.remove(source, options.clone()).await?;
        self.remove_source_from_settings(source, options)
    }

    async fn update(&self, source: Option<&str>) -> Result<(), SourceRegistryError> {
        let targets: Vec<(String, InstallScope)> = match source {
            Some(s) => {
                // Update the scope where the source is configured. If
                // the source is configured in both layers, update both
                // (matches the TS reference's `update(source)` shape).
                let mut out = Vec::new();
                let (g, p) = self.layer_snapshot();
                if g.packages().iter().any(|pkg| pkg.source() == s) {
                    out.push((s.to_string(), InstallScope::User));
                }
                if p.packages().iter().any(|pkg| pkg.source() == s) {
                    out.push((s.to_string(), InstallScope::Project));
                }
                if out.is_empty() {
                    // Treat as ad-hoc update at user scope — the TS
                    // reference lets you update an unconfigured source.
                    out.push((s.to_string(), InstallScope::User));
                }
                out
            }
            None => {
                let mut out = Vec::new();
                let (g, p) = self.layer_snapshot();
                for pkg in g.packages() {
                    out.push((pkg.source().to_string(), InstallScope::User));
                }
                for pkg in p.packages() {
                    out.push((pkg.source().to_string(), InstallScope::Project));
                }
                out
            }
        };

        for (src, scope) in targets {
            let parsed = parse_source(&src);
            let message = format!("Updating {src}...");
            self.with_progress(ProgressAction::Update, &src, &message, || async {
                match parsed {
                    ParsedSource::Npm(npm) => {
                        // `npm install <pkg>@latest --prefix <root>` —
                        // mirror the TS reference's update path.
                        let root = self.npm_install_root(scope);
                        self.ensure_npm_project(&root)?;
                        let prefix = root.to_string_lossy().into_owned();
                        let latest_spec = format!("{}@latest", npm.name);
                        let result = self
                            .runner
                            .run("npm", &["install", &latest_spec, "--prefix", &prefix], None)
                            .await?;
                        if !result.success() {
                            return Err(SourceRegistryError::CommandFailed {
                                program: "npm".into(),
                                code: result.exit_code,
                                stderr: result.stderr,
                            });
                        }
                        Ok(())
                    }
                    ParsedSource::Git(git) => self.update_git(&git, scope).await,
                    // Local sources are user-managed.
                    ParsedSource::Local(_) => Ok(()),
                }
            })
            .await?;
        }
        Ok(())
    }

    fn list_configured_packages(&self) -> Vec<ConfiguredPackage> {
        let (settings_global, settings_project) = self.layer_snapshot();
        let mut out = Vec::new();
        for pkg in settings_global.packages() {
            let source = pkg.source().to_string();
            let installed_path = self.get_installed_path(&source, InstallScope::User);
            out.push(ConfiguredPackage {
                source,
                scope: InstallScope::User,
                filtered: matches!(pkg, PackageSource::Filtered { .. }),
                installed_path,
            });
        }
        for pkg in settings_project.packages() {
            let source = pkg.source().to_string();
            let installed_path = self.get_installed_path(&source, InstallScope::Project);
            out.push(ConfiguredPackage {
                source,
                scope: InstallScope::Project,
                filtered: matches!(pkg, PackageSource::Filtered { .. }),
                installed_path,
            });
        }
        out
    }

    async fn resolve_extension_sources(
        &self,
        sources: &[String],
        options: ResolveExtensionSourcesOptions,
    ) -> Result<ResolvedPaths, SourceRegistryError> {
        // Pick the install scope: temporary > local > user, mirroring TS.
        let scope = if options.temporary {
            InstallScope::Temporary
        } else if options.local {
            InstallScope::Project
        } else {
            InstallScope::User
        };

        let mut paths = ResolvedPaths::default();
        for source in sources {
            // Make sure each ephemeral source is laid down on disk
            // before resolve. We don't go through `install()` — that
            // path requires a SettingsManager when used via
            // install_and_persist; resolve_extension_sources is by
            // design ephemeral and never persists.
            match parse_source(source) {
                ParsedSource::Npm(npm) => self.install_npm(&npm, scope).await?,
                ParsedSource::Git(git) => self.install_git(&git, scope).await?,
                ParsedSource::Local(local) => {
                    let base = self.base_dir_for_scope(scope);
                    let resolved = self.resolve_path_from_base(&local, &base);
                    if !resolved.exists() {
                        return Err(SourceRegistryError::LocalPathMissing(resolved));
                    }
                }
            }
            if let Some(installed) = self.get_installed_path(source, scope) {
                self.add_package_resources(&installed, source, scope, &mut paths);
            }
        }
        Ok(paths)
    }

    fn add_source_to_settings(
        &self,
        source: &str,
        options: PackageInstallOptions,
    ) -> Result<bool, SourceRegistryError> {
        let install_scope = Self::scope_from_options(&options);
        let settings_scope = Self::settings_scope(install_scope)
            .expect("PackageInstallOptions only maps to User/Project");
        let result = self.with_settings_lock(|mgr| {
            let mut packages = mgr.layer(settings_scope).packages().to_vec();
            // Dedupe on the source string. We don't normalise variants
            // here — the TS reference does some normalisation around
            // bare-vs-filtered round-tripping, but in this port a user
            // who wants the filtered form is expected to construct the
            // PackageSource themselves and call set_packages.
            if packages.iter().any(|p| p.source() == source) {
                return Ok(false);
            }
            packages.push(PackageSource::Bare(source.to_string()));
            mgr.set_packages(settings_scope, Some(packages));
            mgr.save(settings_scope)?;
            Ok(true)
        })?;
        if result {
            self.refresh_settings_snapshot();
        }
        Ok(result)
    }

    fn remove_source_from_settings(
        &self,
        source: &str,
        options: PackageInstallOptions,
    ) -> Result<bool, SourceRegistryError> {
        let install_scope = Self::scope_from_options(&options);
        let settings_scope = Self::settings_scope(install_scope)
            .expect("PackageInstallOptions only maps to User/Project");
        let result = self.with_settings_lock(|mgr| {
            let mut packages = mgr.layer(settings_scope).packages().to_vec();
            let original_len = packages.len();
            packages.retain(|p| p.source() != source);
            if packages.len() == original_len {
                return Ok(false);
            }
            mgr.set_packages(settings_scope, Some(packages));
            mgr.save(settings_scope)?;
            Ok(true)
        })?;
        if result {
            self.refresh_settings_snapshot();
        }
        Ok(result)
    }

    fn set_progress_callback(&self, callback: Option<ProgressCallback>) {
        if let Ok(mut guard) = self.progress_callback.lock() {
            *guard = callback;
        }
    }

    fn get_installed_path(&self, source: &str, scope: InstallScope) -> Option<PathBuf> {
        match parse_source(source) {
            ParsedSource::Npm(npm) => {
                let path = self.npm_install_path(&npm, scope);
                path.exists().then_some(path)
            }
            ParsedSource::Git(git) => {
                let path = self.git_install_path(&git, scope);
                path.exists().then_some(path)
            }
            ParsedSource::Local(local) => {
                let base = self.base_dir_for_scope(scope);
                let resolved = self.resolve_path_from_base(&local, &base);
                resolved.exists().then_some(resolved)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Source parsing
// ---------------------------------------------------------------------------

/// Parsed shape of a source string — npm spec, git URL, or local path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedSource {
    Npm(NpmSource),
    Git(GitSource),
    Local(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NpmSource {
    /// The full spec exactly as the user wrote it (e.g. `foo@1.2.3`).
    spec: String,
    /// Bare package name (e.g. `foo` for `foo@1.2.3`).
    name: String,
    /// `true` if a version was supplied.
    pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitSource {
    /// Clone URL (no ref suffix).
    repo: String,
    /// Hostname (e.g. `github.com`).
    host: String,
    /// Repo path on the host (e.g. `owner/repo`).
    path: String,
    /// Optional ref (branch/tag/commit).
    ref_: Option<String>,
    /// `true` if a ref was supplied.
    pinned: bool,
}

/// Parse a source string. The TS reference uses `hosted-git-info` for the
/// git half; we ship a minimal hand-rolled parser that handles the
/// shorthand forms the docs advertise (`github:owner/repo`, full HTTPS
/// URLs, `git@host:owner/repo`). Anything else falls through to
/// `Local`. Edge cases (bitbucket shorthand, `:ref` suffix, gist URLs)
/// are not yet ported.
///
/// TODO(parity): port the remaining `parseGitUrl` cases.
fn parse_source(source: &str) -> ParsedSource {
    if let Some(rest) = source.strip_prefix("npm:") {
        let spec = rest.trim().to_string();
        let (name, pinned) = parse_npm_spec(&spec);
        return ParsedSource::Npm(NpmSource { spec, name, pinned });
    }
    // Git first — `is_local_path` is permissive (only rejects known
    // remote prefixes like `npm:`/`http://`/...), so SCP-form URLs
    // (`git@host:owner/repo`) and shorthand (`github:owner/repo`)
    // would otherwise be classified as local paths.
    if let Some(git) = parse_git_url(source) {
        return ParsedSource::Git(git);
    }
    if crate::utils::paths::is_local_path(source) {
        return ParsedSource::Local(source.to_string());
    }
    ParsedSource::Local(source.to_string())
}

/// Parse an npm spec (`@scope/pkg@version`, `pkg@version`, or `pkg`).
/// Returns the bare name and whether a version was attached.
fn parse_npm_spec(spec: &str) -> (String, bool) {
    if let Some(stripped) = spec.strip_prefix('@') {
        if let Some(at_idx) = stripped.find('@') {
            let name = format!("@{}", &stripped[..at_idx]);
            return (name, true);
        }
        return (spec.to_string(), false);
    }
    if let Some(at_idx) = spec.find('@') {
        return (spec[..at_idx].to_string(), true);
    }
    (spec.to_string(), false)
}

/// Minimal git URL parser. Handles:
/// - `github:owner/repo` (and `bitbucket:`/`gitlab:` shorthand)
/// - `https://github.com/owner/repo[.git]`
/// - `git@github.com:owner/repo[.git]`
///
/// Anything else returns `None` and is treated as a local path.
fn parse_git_url(source: &str) -> Option<GitSource> {
    // Shorthand: `<host>:<path>`. Only hosts whose path doesn't itself
    // contain a `:` reach here — everything else is parsed as URL.
    for (prefix, host) in [
        ("github:", "github.com"),
        ("gitlab:", "gitlab.com"),
        ("bitbucket:", "bitbucket.org"),
    ] {
        if let Some(rest) = source.strip_prefix(prefix) {
            let path = rest.trim_end_matches(".git").to_string();
            if path.is_empty() {
                return None;
            }
            return Some(GitSource {
                repo: format!("https://{host}/{path}"),
                host: host.to_string(),
                path,
                ref_: None,
                pinned: false,
            });
        }
    }

    // SCP-like form: `git@host:owner/repo`.
    if let Some(rest) = source.strip_prefix("git@")
        && let Some(colon_idx) = rest.find(':')
    {
        let host = rest[..colon_idx].to_string();
        let path = rest[colon_idx + 1..].trim_end_matches(".git").to_string();
        if path.is_empty() {
            return None;
        }
        return Some(GitSource {
            repo: format!("git@{host}:{path}.git"),
            host,
            path,
            ref_: None,
            pinned: false,
        });
    }

    // Full URL: `<scheme>://host/owner/repo[.git]`. Only HTTP/HTTPS/SSH
    // schemes are recognised — anything else is treated as a local
    // path. We parse manually to avoid pulling in the `url` crate.
    if let Some((host, path)) = parse_http_or_ssh_url(source) {
        if path.is_empty() || !path.contains('/') {
            return None;
        }
        return Some(GitSource {
            repo: format!("https://{host}/{path}.git"),
            host,
            path,
            ref_: None,
            pinned: false,
        });
    }

    None
}

/// Parse the host + path from a `<scheme>://host/path` URL where the
/// scheme is one of `http`, `https`, `ssh`, or `git`. Returns `None` for
/// any other shape.
fn parse_http_or_ssh_url(source: &str) -> Option<(String, String)> {
    let scheme_end = source.find("://")?;
    let scheme = &source[..scheme_end];
    if !matches!(scheme, "http" | "https" | "ssh" | "git") {
        return None;
    }
    let after_scheme = &source[scheme_end + 3..];
    let (host_part, path_part) = match after_scheme.find('/') {
        Some(slash) => (&after_scheme[..slash], &after_scheme[slash + 1..]),
        None => return None,
    };
    // Strip any `user@` prefix from the host part.
    let host = host_part
        .rsplit('@')
        .next()
        .unwrap_or(host_part)
        .to_string();
    if host.is_empty() {
        return None;
    }
    let path = path_part
        .trim_start_matches('/')
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_string();
    Some((host, path))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceKind {
    Extensions,
    Skills,
    Prompts,
    Themes,
}

impl ResourceKind {
    fn all() -> &'static [ResourceKind] {
        &[Self::Extensions, Self::Skills, Self::Prompts, Self::Themes]
    }

    fn dir_name(self) -> &'static str {
        match self {
            Self::Extensions => "extensions",
            Self::Skills => "skills",
            Self::Prompts => "prompts",
            Self::Themes => "themes",
        }
    }

    fn matches_filename(self, name: &str) -> bool {
        match self {
            Self::Extensions => name.ends_with(".ts") || name.ends_with(".js"),
            Self::Skills | Self::Prompts => name.ends_with(".md"),
            Self::Themes => name.ends_with(".json"),
        }
    }
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Recursive helper for [`DefaultSourceRegistry::walk_resource_files`].
/// Free-function form to avoid the only-used-in-recursion lint while
/// still threading `kind` for the filename predicate.
fn walk_dir_inner(dir: &Path, kind: ResourceKind, out: &mut Vec<PathBuf>) {
    let read = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(_) => return,
    };
    for entry in read.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') || name_str == "node_modules" {
            continue;
        }
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            walk_dir_inner(&path, kind, out);
        } else if file_type.is_file() && kind.matches_filename(&name_str) {
            out.push(path);
        }
    }
}

/// Short hex hash used by [`DefaultSourceRegistry::npm_install_path`] /
/// [`DefaultSourceRegistry::git_install_path`] for the temporary-scope
/// directory namespace.
///
/// This is **not** security-sensitive — it only needs to disambiguate
/// otherwise-colliding tmp paths for distinct sources. A 64-bit FNV-1a
/// (rendered as 16-char hex; we then truncate to 8) is sufficient and
/// keeps us off the `sha2` crate.
fn short_hash(input: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")[..8].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::settings::Settings;
    use std::fs;
    use tempfile::TempDir;

    fn registry_with(
        cwd: PathBuf,
        agent: PathBuf,
        global: Settings,
        project: Settings,
    ) -> DefaultSourceRegistry {
        DefaultSourceRegistry::from_layers(cwd, agent, global, project)
    }

    #[test]
    fn parse_source_recognises_npm_with_version() {
        let parsed = parse_source("npm:lodash@4.17.21");
        match parsed {
            ParsedSource::Npm(NpmSource { name, pinned, spec }) => {
                assert_eq!(name, "lodash");
                assert!(pinned);
                assert_eq!(spec, "lodash@4.17.21");
            }
            other => panic!("expected Npm, got {other:?}"),
        }
    }

    #[test]
    fn parse_source_recognises_npm_without_version() {
        match parse_source("npm:lodash") {
            ParsedSource::Npm(NpmSource { name, pinned, .. }) => {
                assert_eq!(name, "lodash");
                assert!(!pinned);
            }
            other => panic!("expected Npm, got {other:?}"),
        }
    }

    #[test]
    fn parse_source_recognises_scoped_npm() {
        match parse_source("npm:@scope/pkg@1.2.3") {
            ParsedSource::Npm(NpmSource { name, pinned, .. }) => {
                assert_eq!(name, "@scope/pkg");
                assert!(pinned);
            }
            other => panic!("expected Npm, got {other:?}"),
        }
    }

    #[test]
    fn parse_source_recognises_github_shorthand() {
        match parse_source("github:owner/repo") {
            ParsedSource::Git(GitSource { host, path, .. }) => {
                assert_eq!(host, "github.com");
                assert_eq!(path, "owner/repo");
            }
            other => panic!("expected Git, got {other:?}"),
        }
    }

    #[test]
    fn parse_source_recognises_https_url() {
        match parse_source("https://github.com/owner/repo.git") {
            ParsedSource::Git(GitSource { host, path, .. }) => {
                assert_eq!(host, "github.com");
                assert_eq!(path, "owner/repo");
            }
            other => panic!("expected Git, got {other:?}"),
        }
    }

    #[test]
    fn parse_source_recognises_scp_url() {
        match parse_source("git@github.com:owner/repo.git") {
            ParsedSource::Git(GitSource {
                host, path, repo, ..
            }) => {
                assert_eq!(host, "github.com");
                assert_eq!(path, "owner/repo");
                assert_eq!(repo, "git@github.com:owner/repo.git");
            }
            other => panic!("expected Git, got {other:?}"),
        }
    }

    #[test]
    fn parse_source_falls_back_to_local() {
        match parse_source("./local/path") {
            ParsedSource::Local(p) => assert_eq!(p, "./local/path"),
            other => panic!("expected Local, got {other:?}"),
        }
    }

    #[test]
    fn list_configured_packages_separates_user_from_project() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&agent).unwrap();

        let global = Settings {
            packages: Some(vec![PackageSource::Bare("npm:foo".into())]),
            ..Settings::default()
        };
        let project = Settings {
            packages: Some(vec![PackageSource::Filtered {
                source: "github:owner/repo".into(),
                extensions: Some(vec!["ext-a".into()]),
                skills: None,
                prompts: None,
                themes: None,
            }]),
            ..Settings::default()
        };
        let reg = registry_with(cwd, agent, global, project);
        let listed = reg.list_configured_packages();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].source, "npm:foo");
        assert_eq!(listed[0].scope, InstallScope::User);
        assert!(!listed[0].filtered);
        assert_eq!(listed[1].source, "github:owner/repo");
        assert_eq!(listed[1].scope, InstallScope::Project);
        assert!(listed[1].filtered);
    }

    // Replaced "returns_not_yet_implemented" tests with mocked
    // install/remove/update coverage further below.

    #[test]
    fn get_installed_path_returns_none_when_dir_missing() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&agent).unwrap();

        let reg = registry_with(cwd, agent, Settings::default(), Settings::default());
        // Nothing has been installed, so every lookup should be None.
        assert!(
            reg.get_installed_path("npm:foo", InstallScope::User)
                .is_none()
        );
        assert!(
            reg.get_installed_path("github:owner/repo", InstallScope::Project)
                .is_none()
        );
    }

    #[test]
    fn get_installed_path_returns_some_when_dir_exists() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&agent).unwrap();
        // Pre-create an npm install dir matching the layout.
        fs::create_dir_all(agent.join("npm/node_modules/foo")).unwrap();

        let reg = registry_with(cwd, agent.clone(), Settings::default(), Settings::default());
        let resolved = reg
            .get_installed_path("npm:foo", InstallScope::User)
            .expect("install dir should resolve");
        assert!(resolved.ends_with("npm/node_modules/foo"));
    }

    #[test]
    fn resolve_returns_empty_when_no_packages_or_paths() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&agent).unwrap();

        let reg = registry_with(cwd, agent, Settings::default(), Settings::default());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resolved = rt.block_on(reg.resolve(None)).unwrap();
        assert!(resolved.extensions.is_empty());
        assert!(resolved.skills.is_empty());
        assert!(resolved.prompts.is_empty());
        assert!(resolved.themes.is_empty());
    }

    #[test]
    fn resolve_picks_up_convention_path_skills() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(cwd.join(".hand/skills")).unwrap();
        fs::create_dir_all(&agent).unwrap();
        // Drop a markdown file under the project skills dir.
        fs::write(
            cwd.join(".hand/skills/my-skill.md"),
            "---\nname: my-skill\n---\n# Body",
        )
        .unwrap();

        let reg = registry_with(cwd, agent, Settings::default(), Settings::default());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resolved = rt.block_on(reg.resolve(None)).unwrap();
        assert_eq!(resolved.skills.len(), 1, "found {:?}", resolved.skills);
        assert!(resolved.skills[0].path.ends_with("my-skill.md"));
        assert_eq!(resolved.skills[0].metadata.scope, InstallScope::Project);
        assert_eq!(resolved.skills[0].metadata.origin, ResourceOrigin::TopLevel);
    }

    #[test]
    fn resolve_walks_settings_listed_extension_paths() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&agent).unwrap();
        // The project layer points at an absolute file path.
        let ext_file = dir.path().join("listed-ext.ts");
        fs::write(&ext_file, "// extension").unwrap();

        let project = Settings {
            extensions: Some(vec![ext_file.to_string_lossy().into_owned()]),
            ..Settings::default()
        };
        let reg = registry_with(cwd, agent, Settings::default(), project);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resolved = rt.block_on(reg.resolve(None)).unwrap();
        assert_eq!(resolved.extensions.len(), 1);
        assert_eq!(resolved.extensions[0].path, ext_file);
    }

    #[test]
    fn resolve_picks_up_package_resources_when_install_dir_exists() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();

        // Pre-stage an npm install dir with an extension file.
        let install_dir = agent.join("npm/node_modules/foo");
        fs::create_dir_all(install_dir.join("extensions")).unwrap();
        fs::write(install_dir.join("extensions/index.ts"), "// ext").unwrap();

        let global = Settings {
            packages: Some(vec![PackageSource::Bare("npm:foo".into())]),
            ..Settings::default()
        };
        let reg = registry_with(cwd, agent, global, Settings::default());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resolved = rt.block_on(reg.resolve(None)).unwrap();
        assert_eq!(resolved.extensions.len(), 1);
        assert!(resolved.extensions[0].path.ends_with("index.ts"));
        assert_eq!(
            resolved.extensions[0].metadata.origin,
            ResourceOrigin::Package
        );
        assert_eq!(resolved.extensions[0].metadata.source, "npm:foo");
    }

    #[test]
    fn set_progress_callback_round_trips() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let reg = registry_with(
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp/agent"),
            Settings::default(),
            Settings::default(),
        );
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);
        reg.set_progress_callback(Some(Box::new(move |_event| {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        })));
        reg.emit_progress(ProgressEvent {
            event_type: ProgressEventType::Start,
            action: ProgressAction::Install,
            source: "npm:foo".into(),
            message: None,
        });
        reg.emit_progress(ProgressEvent {
            event_type: ProgressEventType::Complete,
            action: ProgressAction::Install,
            source: "npm:foo".into(),
            message: None,
        });
        assert_eq!(counter.load(Ordering::Relaxed), 2);
        // Clearing the callback stops further increments.
        reg.set_progress_callback(None);
        reg.emit_progress(ProgressEvent {
            event_type: ProgressEventType::Error,
            action: ProgressAction::Install,
            source: "npm:foo".into(),
            message: Some("boom".into()),
        });
        assert_eq!(counter.load(Ordering::Relaxed), 2);
    }

    // -----------------------------------------------------------------
    // install / remove / update / persist (Track 2)
    // -----------------------------------------------------------------

    use crate::core::settings::SettingsManager;
    use std::sync::Mutex as StdMutex;

    /// One recorded invocation: (program, args, cwd).
    type RunnerCall = (String, Vec<String>, Option<PathBuf>);

    /// Test ProcessRunner that records every invocation and returns
    /// pre-canned results.
    #[derive(Default)]
    struct RecordingRunner {
        calls: StdMutex<Vec<RunnerCall>>,
        // Map from program name to result. Defaults to success.
        outcomes: StdMutex<std::collections::HashMap<String, ProcessRunResult>>,
    }

    impl RecordingRunner {
        fn new() -> Self {
            Self::default()
        }

        fn calls(&self) -> Vec<RunnerCall> {
            self.calls.lock().unwrap().clone()
        }

        fn set_outcome(&self, program: &str, result: ProcessRunResult) {
            self.outcomes
                .lock()
                .unwrap()
                .insert(program.to_string(), result);
        }
    }

    #[async_trait::async_trait]
    impl ProcessRunner for RecordingRunner {
        async fn run(
            &self,
            program: &str,
            args: &[&str],
            cwd: Option<&Path>,
        ) -> Result<ProcessRunResult, SourceRegistryError> {
            self.calls.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|a| a.to_string()).collect(),
                cwd.map(Path::to_path_buf),
            ));
            Ok(self
                .outcomes
                .lock()
                .unwrap()
                .get(program)
                .cloned()
                .unwrap_or_else(ProcessRunResult::ok))
        }
    }

    fn registry_with_runner(
        cwd: PathBuf,
        agent: PathBuf,
        runner: Arc<RecordingRunner>,
    ) -> DefaultSourceRegistry {
        DefaultSourceRegistry::from_layers(cwd, agent, Settings::default(), Settings::default())
            .with_runner(runner as Arc<dyn ProcessRunner>)
    }

    #[tokio::test]
    async fn install_npm_runs_npm_install_with_prefix() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();

        let runner = Arc::new(RecordingRunner::new());
        let reg = registry_with_runner(cwd, agent.clone(), Arc::clone(&runner));
        reg.install("npm:foo", PackageInstallOptions::default())
            .await
            .expect("install ok");

        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        let (program, args, _cwd) = &calls[0];
        assert_eq!(program, "npm");
        assert_eq!(args[0], "install");
        assert_eq!(args[1], "foo");
        assert_eq!(args[2], "--prefix");
        // The prefix should resolve under the agent dir.
        assert!(args[3].contains("npm"));
        // The npm install root + .gitignore + package.json were all
        // staged before the runner saw the call.
        assert!(agent.join("npm/.gitignore").exists());
        assert!(agent.join("npm/package.json").exists());
    }

    #[tokio::test]
    async fn install_npm_surfaces_command_failure() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();

        let runner = Arc::new(RecordingRunner::new());
        runner.set_outcome(
            "npm",
            ProcessRunResult {
                exit_code: Some(1),
                stderr: "ERR: boom".into(),
            },
        );
        let reg = registry_with_runner(cwd, agent, Arc::clone(&runner));
        let err = reg
            .install("npm:foo", PackageInstallOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, SourceRegistryError::CommandFailed { .. }));
    }

    #[tokio::test]
    async fn install_git_clones_into_destination() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();

        let runner = Arc::new(RecordingRunner::new());
        let reg = registry_with_runner(cwd, agent.clone(), Arc::clone(&runner));
        reg.install("github:owner/repo", PackageInstallOptions::default())
            .await
            .expect("install ok");

        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        let (program, args, _cwd) = &calls[0];
        assert_eq!(program, "git");
        assert_eq!(args[0], "clone");
        assert_eq!(args[1], "https://github.com/owner/repo");
        // Destination ends with the repo path so the install layout is
        // observable through `get_installed_path` post-install.
        assert!(args[2].ends_with("owner/repo"));
        // git_install_root .gitignore landed.
        assert!(agent.join("git/.gitignore").exists());
    }

    #[tokio::test]
    async fn install_git_skip_when_destination_exists() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(agent.join("git/github.com/owner/repo")).unwrap();

        let runner = Arc::new(RecordingRunner::new());
        let reg = registry_with_runner(cwd, agent, Arc::clone(&runner));
        reg.install("github:owner/repo", PackageInstallOptions::default())
            .await
            .unwrap();
        // Runner not called — install short-circuited.
        assert!(runner.calls().is_empty());
    }

    #[tokio::test]
    async fn install_local_path_validates_existence() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&agent).unwrap();

        let runner = Arc::new(RecordingRunner::new());
        let reg = registry_with_runner(cwd, agent, Arc::clone(&runner));
        // Missing path -> error, no shell-out.
        let err = reg
            .install("./does-not-exist", PackageInstallOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, SourceRegistryError::LocalPathMissing(_)));
        assert!(runner.calls().is_empty());
    }

    #[tokio::test]
    async fn remove_npm_invokes_uninstall_then_drops_dir() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        // Pre-create the install dir + npm prefix layout.
        let install_root = agent.join("npm");
        fs::create_dir_all(install_root.join("node_modules/foo")).unwrap();
        fs::write(install_root.join("package.json"), "{}").unwrap();

        let runner = Arc::new(RecordingRunner::new());
        let reg = registry_with_runner(cwd, agent.clone(), Arc::clone(&runner));
        reg.remove("npm:foo", PackageInstallOptions::default())
            .await
            .unwrap();

        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "npm");
        assert_eq!(calls[0].1[0], "uninstall");
        assert_eq!(calls[0].1[1], "foo");
        // Per-package dir cleaned up belt-and-braces.
        assert!(!install_root.join("node_modules/foo").exists());
    }

    #[tokio::test]
    async fn remove_git_drops_install_dir_without_shelling_out() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        let dest = agent.join("git/github.com/owner/repo");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("README.md"), "x").unwrap();

        let runner = Arc::new(RecordingRunner::new());
        let reg = registry_with_runner(cwd, agent, Arc::clone(&runner));
        reg.remove("github:owner/repo", PackageInstallOptions::default())
            .await
            .unwrap();
        assert!(!dest.exists());
        assert!(
            runner.calls().is_empty(),
            "remove should not shell out for git"
        );
    }

    #[tokio::test]
    async fn update_git_runs_pull_in_install_dir() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        let dest = agent.join("git/github.com/owner/repo");
        fs::create_dir_all(&dest).unwrap();

        let runner = Arc::new(RecordingRunner::new());
        let global = Settings {
            packages: Some(vec![PackageSource::Bare("github:owner/repo".into())]),
            ..Settings::default()
        };
        let reg = DefaultSourceRegistry::from_layers(cwd, agent, global, Settings::default())
            .with_runner(Arc::clone(&runner) as Arc<dyn ProcessRunner>);
        reg.update(Some("github:owner/repo")).await.unwrap();

        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "git");
        assert_eq!(calls[0].1, vec!["pull"]);
        assert_eq!(calls[0].2.as_deref(), Some(dest.as_path()));
    }

    #[tokio::test]
    async fn update_npm_runs_install_at_latest() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();

        let runner = Arc::new(RecordingRunner::new());
        let global = Settings {
            packages: Some(vec![PackageSource::Bare("npm:foo".into())]),
            ..Settings::default()
        };
        let reg = DefaultSourceRegistry::from_layers(cwd, agent, global, Settings::default())
            .with_runner(Arc::clone(&runner) as Arc<dyn ProcessRunner>);
        reg.update(Some("npm:foo")).await.unwrap();

        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "npm");
        assert_eq!(calls[0].1[0], "install");
        assert_eq!(calls[0].1[1], "foo@latest");
    }

    #[tokio::test]
    async fn install_emits_start_complete_progress_events() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();

        let runner = Arc::new(RecordingRunner::new());
        let reg = registry_with_runner(cwd, agent, Arc::clone(&runner));
        let events = Arc::new(StdMutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        reg.set_progress_callback(Some(Box::new(move |ev| {
            captured.lock().unwrap().push(ev.event_type);
        })));
        reg.install("npm:foo", PackageInstallOptions::default())
            .await
            .unwrap();
        let kinds = events.lock().unwrap().clone();
        assert_eq!(
            kinds,
            vec![ProgressEventType::Start, ProgressEventType::Complete]
        );
    }

    #[test]
    fn add_source_to_settings_without_manager_errors() {
        let reg = DefaultSourceRegistry::from_layers(
            PathBuf::from("/tmp/proj"),
            PathBuf::from("/tmp/agent"),
            Settings::default(),
            Settings::default(),
        );
        let err = reg
            .add_source_to_settings("npm:foo", PackageInstallOptions::default())
            .unwrap_err();
        assert!(matches!(err, SourceRegistryError::NoSettingsManager));
    }

    #[test]
    fn add_source_to_settings_persists_and_dedupes() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&agent).unwrap();
        let global_yaml = agent.join("settings.yaml");
        let project_yaml = cwd.join("settings.yaml");
        fs::write(&global_yaml, "").unwrap();
        fs::write(&project_yaml, "").unwrap();

        let mgr = SettingsManager::from_layers_for_test(
            Settings::default(),
            Settings::default(),
            Some(global_yaml.clone()),
            Some(project_yaml.clone()),
        );
        let mgr = Arc::new(StdMutex::new(mgr));
        let reg = DefaultSourceRegistry::with_settings_manager(
            cwd.clone(),
            agent.clone(),
            Arc::clone(&mgr),
        );

        // First add → true, written to disk.
        let added = reg
            .add_source_to_settings("npm:foo", PackageInstallOptions::default())
            .unwrap();
        assert!(added);
        // Reload from disk to confirm round-trip.
        let reloaded = Settings::load(Some(&global_yaml), Some(&project_yaml)).unwrap();
        let pkgs = reloaded.packages();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].source(), "npm:foo");

        // Second add → false (no change).
        let added_again = reg
            .add_source_to_settings("npm:foo", PackageInstallOptions::default())
            .unwrap();
        assert!(!added_again);

        // Cached snapshot inside the registry should reflect the write.
        let listed = reg.list_configured_packages();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].source, "npm:foo");
        assert_eq!(listed[0].scope, InstallScope::User);
    }

    #[test]
    fn add_source_to_settings_local_uses_project_layer() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&agent).unwrap();
        let global_yaml = agent.join("settings.yaml");
        let project_yaml = cwd.join("settings.yaml");
        fs::write(&global_yaml, "").unwrap();
        fs::write(&project_yaml, "").unwrap();

        let mgr = SettingsManager::from_layers_for_test(
            Settings::default(),
            Settings::default(),
            Some(global_yaml.clone()),
            Some(project_yaml.clone()),
        );
        let mgr = Arc::new(StdMutex::new(mgr));
        let reg = DefaultSourceRegistry::with_settings_manager(
            cwd.clone(),
            agent.clone(),
            Arc::clone(&mgr),
        );
        reg.add_source_to_settings("npm:proj-only", PackageInstallOptions { local: true })
            .unwrap();

        // Project YAML carries the entry, global YAML does not.
        let project_yaml_body = fs::read_to_string(&project_yaml).unwrap();
        assert!(project_yaml_body.contains("npm:proj-only"));
        let global_yaml_body = fs::read_to_string(&global_yaml).unwrap();
        assert!(!global_yaml_body.contains("npm:proj-only"));
    }

    #[test]
    fn remove_source_from_settings_round_trips() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&agent).unwrap();
        let global_yaml = agent.join("settings.yaml");
        let project_yaml = cwd.join("settings.yaml");
        fs::write(&global_yaml, "packages:\n  - npm:foo\n  - npm:bar\n").unwrap();
        fs::write(&project_yaml, "").unwrap();

        let (g, p, _) = Settings::load_layers(Some(&global_yaml), Some(&project_yaml)).unwrap();
        let mgr = SettingsManager::from_layers_for_test(
            g,
            p,
            Some(global_yaml.clone()),
            Some(project_yaml),
        );
        let mgr = Arc::new(StdMutex::new(mgr));
        let reg = DefaultSourceRegistry::with_settings_manager(cwd, agent, Arc::clone(&mgr));

        let removed = reg
            .remove_source_from_settings("npm:foo", PackageInstallOptions::default())
            .unwrap();
        assert!(removed);

        // Settings file lost npm:foo, npm:bar still present.
        let body = fs::read_to_string(&global_yaml).unwrap();
        assert!(!body.contains("npm:foo"));
        assert!(body.contains("npm:bar"));

        // Removing again -> false.
        let removed_again = reg
            .remove_source_from_settings("npm:foo", PackageInstallOptions::default())
            .unwrap();
        assert!(!removed_again);
    }

    #[tokio::test]
    async fn install_and_persist_writes_settings() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&agent).unwrap();
        let global_yaml = agent.join("settings.yaml");
        let project_yaml = cwd.join("settings.yaml");
        fs::write(&global_yaml, "").unwrap();
        fs::write(&project_yaml, "").unwrap();

        let mgr = SettingsManager::from_layers_for_test(
            Settings::default(),
            Settings::default(),
            Some(global_yaml.clone()),
            Some(project_yaml),
        );
        let mgr = Arc::new(StdMutex::new(mgr));

        let runner = Arc::new(RecordingRunner::new());
        let reg = DefaultSourceRegistry::with_settings_manager(cwd, agent, Arc::clone(&mgr))
            .with_runner(Arc::clone(&runner) as Arc<dyn ProcessRunner>);
        reg.install_and_persist("npm:foo", PackageInstallOptions::default())
            .await
            .unwrap();

        // npm install was invoked.
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "npm");
        // Settings YAML on disk now mentions npm:foo.
        let body = fs::read_to_string(&global_yaml).unwrap();
        assert!(body.contains("npm:foo"));
    }

    #[tokio::test]
    async fn resolve_extension_sources_uses_temporary_when_flag_set() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("proj");
        let agent = dir.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&agent).unwrap();

        let runner = Arc::new(RecordingRunner::new());
        let reg = registry_with_runner(cwd, agent.clone(), Arc::clone(&runner));
        let _resolved = reg
            .resolve_extension_sources(
                &["npm:foo".to_string()],
                ResolveExtensionSourcesOptions {
                    local: false,
                    temporary: true,
                },
            )
            .await
            .unwrap();
        // The npm runner was invoked with a tmp-dir prefix.
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        let prefix = &calls[0].1[3];
        assert!(
            prefix.starts_with(std::env::temp_dir().to_string_lossy().as_ref()),
            "expected temp prefix, got: {prefix}",
        );
    }
}
