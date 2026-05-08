//! Pi-extension package-source registry.
//!
//! Ports the public surface of the TS `core/package-manager.ts` module
//! from pi-mono. This is **not** the same thing as
//! [`crate::core::package_manager`] — that module detects programming
//! languages from file extensions and is unrelated. The TS file is named
//! "package manager" in the sense of "manages packages of pi-extension
//! resources" (extensions, skills, prompts, themes); this module renames
//! it to `SourceRegistry` to avoid the collision.
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

use crate::core::settings::{PackageSource, SettingsManager};
use std::path::{Path, PathBuf};
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
    #[error("I/O error reading {path}: {source}", path = .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
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
// DefaultSourceRegistry
// ---------------------------------------------------------------------------

/// Default in-process implementation of [`SourceRegistry`].
///
/// Holds a borrowed `SettingsManager` snapshot (cloned `Settings`) and the
/// agent + cwd dirs needed to compute install paths. The mutating
/// operations are stubbed pending the Tier 3 install/persist port.
pub struct DefaultSourceRegistry {
    cwd: PathBuf,
    agent_dir: PathBuf,
    settings_global: crate::core::settings::Settings,
    settings_project: crate::core::settings::Settings,
    progress_callback: std::sync::Mutex<Option<ProgressCallback>>,
}

impl DefaultSourceRegistry {
    /// Construct from a [`SettingsManager`] snapshot. The settings
    /// layers are cloned at construction time; subsequent mutations to
    /// the manager are not reflected without rebuilding the registry.
    pub fn new(cwd: PathBuf, agent_dir: PathBuf, settings_manager: &SettingsManager) -> Self {
        // The current Rust SettingsManager does not separately expose
        // global vs. project layers. For the happy-path resolve we use
        // the merged view as both layers — that means a project-only
        // setting will also appear at user scope in the resolve output,
        // but the resolved paths are the same.
        // TODO(parity): once SettingsManager exposes layer-separated
        // accessors (gated on the "settings setters" follow-up),
        // re-thread them through here.
        let merged = settings_manager.current().clone();
        Self {
            cwd,
            agent_dir,
            settings_global: merged.clone(),
            settings_project: merged,
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
            settings_global,
            settings_project,
            progress_callback: std::sync::Mutex::new(None),
        }
    }

    /// Forward an event to the registered progress callback. The
    /// install/remove/update implementations will use this once they
    /// land; in the current port it's exercised by tests only.
    #[allow(dead_code)] // TODO(parity): wired by install/remove/update once ported
    fn emit_progress(&self, event: ProgressEvent) {
        if let Ok(guard) = self.progress_callback.lock()
            && let Some(cb) = guard.as_ref()
        {
            cb(&event);
        }
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

        // Project layer first so its resources win on later dedup. Note
        // that the current Rust SettingsManager doesn't separate layers,
        // so we duplicate the merged view; see DefaultSourceRegistry::new.
        for pkg in self.settings_project.packages() {
            let source = pkg.source().to_string();
            if let Some(installed) = self.get_installed_path(&source, InstallScope::Project) {
                self.add_package_resources(&installed, &source, InstallScope::Project, &mut paths);
            }
        }
        for pkg in self.settings_global.packages() {
            let source = pkg.source().to_string();
            if let Some(installed) = self.get_installed_path(&source, InstallScope::User) {
                self.add_package_resources(&installed, &source, InstallScope::User, &mut paths);
            }
        }

        // Top-level / convention-path discovery for both scopes.
        self.add_scope_top_level(
            InstallScope::Project,
            &self.settings_project,
            &self.cwd.join(".hand"),
            &mut paths,
        );
        self.add_scope_top_level(
            InstallScope::User,
            &self.settings_global,
            &self.agent_dir,
            &mut paths,
        );

        Ok(paths)
    }

    async fn install(
        &self,
        _source: &str,
        _options: PackageInstallOptions,
    ) -> Result<(), SourceRegistryError> {
        // TODO(parity): port npm/git install logic — see docs/exec-plans/parity-completion.md
        Err(SourceRegistryError::NotYetImplemented(
            "install: npm/git network install not yet ported",
        ))
    }

    async fn install_and_persist(
        &self,
        _source: &str,
        _options: PackageInstallOptions,
    ) -> Result<(), SourceRegistryError> {
        // TODO(parity): port npm/git install logic — see docs/exec-plans/parity-completion.md
        Err(SourceRegistryError::NotYetImplemented(
            "install_and_persist: depends on install + settings write",
        ))
    }

    async fn remove(
        &self,
        _source: &str,
        _options: PackageInstallOptions,
    ) -> Result<(), SourceRegistryError> {
        // TODO(parity): port npm/git uninstall logic — see docs/exec-plans/parity-completion.md
        Err(SourceRegistryError::NotYetImplemented(
            "remove: npm/git uninstall not yet ported",
        ))
    }

    async fn remove_and_persist(
        &self,
        _source: &str,
        _options: PackageInstallOptions,
    ) -> Result<bool, SourceRegistryError> {
        // TODO(parity): port npm/git uninstall logic — see docs/exec-plans/parity-completion.md
        Err(SourceRegistryError::NotYetImplemented(
            "remove_and_persist: depends on remove + settings write",
        ))
    }

    async fn update(&self, _source: Option<&str>) -> Result<(), SourceRegistryError> {
        // TODO(parity): port npm/git update logic — see docs/exec-plans/parity-completion.md
        Err(SourceRegistryError::NotYetImplemented(
            "update: npm/git update not yet ported",
        ))
    }

    fn list_configured_packages(&self) -> Vec<ConfiguredPackage> {
        let mut out = Vec::new();
        for pkg in self.settings_global.packages() {
            let source = pkg.source().to_string();
            let installed_path = self.get_installed_path(&source, InstallScope::User);
            out.push(ConfiguredPackage {
                source,
                scope: InstallScope::User,
                filtered: matches!(pkg, PackageSource::Filtered { .. }),
                installed_path,
            });
        }
        for pkg in self.settings_project.packages() {
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
        _sources: &[String],
        _options: ResolveExtensionSourcesOptions,
    ) -> Result<ResolvedPaths, SourceRegistryError> {
        // TODO(parity): the CLI override path needs the install logic to
        // handle missing sources for the temporary scope. Stubbed
        // pending the Tier 3 port.
        Err(SourceRegistryError::NotYetImplemented(
            "resolve_extension_sources: depends on install for ephemeral scope",
        ))
    }

    fn add_source_to_settings(
        &self,
        _source: &str,
        _options: PackageInstallOptions,
    ) -> Result<bool, SourceRegistryError> {
        // TODO(parity): needs SettingsManager YAML writers — see docs/exec-plans/parity-completion.md
        Err(SourceRegistryError::NotYetImplemented(
            "add_source_to_settings: requires SettingsManager YAML writers",
        ))
    }

    fn remove_source_from_settings(
        &self,
        _source: &str,
        _options: PackageInstallOptions,
    ) -> Result<bool, SourceRegistryError> {
        // TODO(parity): needs SettingsManager YAML writers — see docs/exec-plans/parity-completion.md
        Err(SourceRegistryError::NotYetImplemented(
            "remove_source_from_settings: requires SettingsManager YAML writers",
        ))
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

    #[test]
    fn install_returns_not_yet_implemented() {
        let reg = registry_with(
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp/agent"),
            Settings::default(),
            Settings::default(),
        );
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt
            .block_on(reg.install("npm:foo", PackageInstallOptions::default()))
            .unwrap_err();
        assert!(matches!(err, SourceRegistryError::NotYetImplemented(_)));
    }

    #[test]
    fn add_source_to_settings_returns_not_yet_implemented() {
        let reg = registry_with(
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp/agent"),
            Settings::default(),
            Settings::default(),
        );
        let err = reg
            .add_source_to_settings("npm:foo", PackageInstallOptions::default())
            .unwrap_err();
        assert!(matches!(err, SourceRegistryError::NotYetImplemented(_)));
    }

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
}
