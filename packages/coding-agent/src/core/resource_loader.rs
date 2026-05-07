//! Generic resource discovery with scope-priority deduplication.
//!
//! Walks a sequence of root directories looking for "resource directories":
//! every immediate subdirectory of a root is treated as one resource, and
//! the loader looks for a fixed filename (e.g., `SKILL.md`) inside it. The
//! file's frontmatter is parsed via [`crate::utils::frontmatter`] and the
//! result is returned together with [`SourceInfo`] describing where it came
//! from.
//!
//! Roots are processed in scope-precedence order. When two resources share
//! the same canonical name, the higher-priority scope (later in the input
//! list) wins. The output is sorted by name.

use crate::core::source_info::{SourceInfo, SourceScope};
use crate::utils::frontmatter::{FrontmatterError, parse_frontmatter};
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// One resource as returned by the loader.
#[derive(Debug, Clone)]
pub struct DiscoveredResource<T> {
    /// The canonical name used for deduplication.
    pub name: String,
    /// Parsed frontmatter metadata (None if the source had no frontmatter
    /// block or the block was empty/null).
    pub metadata: Option<T>,
    /// Resource body (everything after the frontmatter close).
    pub body: String,
    /// Where it came from.
    pub source: SourceInfo,
}

/// Errors raised while discovering resources.
#[derive(Debug, Error)]
pub enum ResourceLoaderError {
    #[error("I/O error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("frontmatter error in {path}: {source}")]
    Frontmatter {
        path: PathBuf,
        #[source]
        source: FrontmatterError,
    },
}

/// How to derive the canonical name for a discovered file.
#[derive(Debug, Clone, Copy)]
pub enum NameResolver {
    /// Use the parent directory's file name (e.g., for `<dir>/SKILL.md`).
    ParentDirName,
    /// Use the file stem of the discovered file itself (e.g., for `<dir>/<name>.md`).
    FileStem,
}

/// On-disk layout of resources under a root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    /// Each immediate subdirectory of the root is one resource and contains
    /// a fixed-name file (e.g., `<root>/<dir>/SKILL.md`). This is the
    /// historical layout used by Skills.
    PerDirectory,
    /// Each `.md` file directly under the root is one resource. The
    /// `resource_filename` field is ignored under this layout. Used by
    /// prompt templates.
    Flat,
}

/// Configuration for a single resource-discovery walk.
pub struct ResourceLoaderConfig {
    /// Roots to search, in scope-precedence order from lowest to highest
    /// (e.g., `[(builtin_dir, Builtin), (user_dir, User), (project_dir, Project)]`).
    /// A scope appearing later in the list shadows earlier ones with the same name.
    pub roots: Vec<(PathBuf, SourceScope)>,
    /// File name to match within each per-resource subdirectory (e.g., "SKILL.md").
    /// Ignored when `layout` is [`LayoutKind::Flat`].
    pub resource_filename: &'static str,
    /// How the canonical name is derived for each discovered file.
    pub name_resolver: NameResolver,
    /// On-disk layout. Defaults to `PerDirectory` for backward compatibility
    /// with callers that omit it via `..Default::default()`.
    pub layout: LayoutKind,
}

impl Default for ResourceLoaderConfig {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            resource_filename: "",
            name_resolver: NameResolver::ParentDirName,
            layout: LayoutKind::PerDirectory,
        }
    }
}

/// Discover and parse all resources under the configured roots.
///
/// Returns entries deduped by canonical name, keeping the highest-priority
/// scope. The result is sorted by name.
///
/// Per-file errors abort the entire walk. Callers that want to log-and-skip
/// should use [`discover_resources_lenient`].
pub fn discover_resources<T: DeserializeOwned>(
    config: &ResourceLoaderConfig,
) -> Result<Vec<DiscoveredResource<T>>, ResourceLoaderError> {
    let (successes, failures) = discover_resources_lenient::<T>(config);
    if let Some((_, err)) = failures.into_iter().next() {
        return Err(err);
    }
    Ok(successes)
}

/// Like [`discover_resources`], but per-file errors are collected separately
/// instead of aborting. Returns `(successes, failures)`.
pub fn discover_resources_lenient<T: DeserializeOwned>(
    config: &ResourceLoaderConfig,
) -> (
    Vec<DiscoveredResource<T>>,
    Vec<(PathBuf, ResourceLoaderError)>,
) {
    // Map from canonical name to the currently-best entry. Because roots are
    // processed in lowest-to-highest precedence order, a later insertion with
    // the same name simply overwrites the earlier one.
    let mut by_name: BTreeMap<String, DiscoveredResource<T>> = BTreeMap::new();
    let mut failures: Vec<(PathBuf, ResourceLoaderError)> = Vec::new();

    for (root, scope) in &config.roots {
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                failures.push((
                    root.clone(),
                    ResourceLoaderError::Io {
                        path: root.clone(),
                        source: err,
                    },
                ));
                continue;
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    failures.push((
                        root.clone(),
                        ResourceLoaderError::Io {
                            path: root.clone(),
                            source: err,
                        },
                    ));
                    continue;
                }
            };

            let entry_path = entry.path();

            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(err) => {
                    failures.push((
                        entry_path.clone(),
                        ResourceLoaderError::Io {
                            path: entry_path,
                            source: err,
                        },
                    ));
                    continue;
                }
            };

            let resource_file = match config.layout {
                LayoutKind::PerDirectory => {
                    // Only consider immediate subdirectories of the root as
                    // candidate resources. Files (e.g., a stray README.md)
                    // are ignored.
                    if !file_type.is_dir() {
                        continue;
                    }
                    let candidate = entry_path.join(config.resource_filename);
                    if !candidate.is_file() {
                        continue;
                    }
                    candidate
                }
                LayoutKind::Flat => {
                    // Only consider `.md` files directly under the root.
                    if !file_type.is_file() {
                        continue;
                    }
                    if entry_path.extension().and_then(|s| s.to_str()) != Some("md") {
                        continue;
                    }
                    entry_path
                }
            };

            match load_resource_file::<T>(&resource_file, *scope, config.name_resolver) {
                Ok(Some(resource)) => {
                    by_name.insert(resource.name.clone(), resource);
                }
                Ok(None) => {}
                Err(err) => {
                    failures.push((resource_file, err));
                }
            }
        }
    }

    let successes: Vec<DiscoveredResource<T>> = by_name.into_values().collect();
    (successes, failures)
}

fn load_resource_file<T: DeserializeOwned>(
    path: &Path,
    scope: SourceScope,
    resolver: NameResolver,
) -> Result<Option<DiscoveredResource<T>>, ResourceLoaderError> {
    let content = std::fs::read_to_string(path).map_err(|err| ResourceLoaderError::Io {
        path: path.to_path_buf(),
        source: err,
    })?;

    let parsed =
        parse_frontmatter::<T>(&content).map_err(|err| ResourceLoaderError::Frontmatter {
            path: path.to_path_buf(),
            source: err,
        })?;

    let name = match resolver {
        NameResolver::ParentDirName => match parent_dir_name(path) {
            Some(name) => name,
            // No parent directory or non-UTF-8 dir name — skip silently.
            None => return Ok(None),
        },
        NameResolver::FileStem => match file_stem_name(path) {
            Some(name) => name,
            // No file stem or non-UTF-8 stem — skip silently.
            None => return Ok(None),
        },
    };

    let source = match scope {
        SourceScope::Builtin => SourceInfo::builtin(path.to_path_buf()),
        SourceScope::User => SourceInfo::user(path.to_path_buf()),
        SourceScope::Project => SourceInfo::project(path.to_path_buf()),
        // Extension scope is not derivable from a (path, scope) pair alone —
        // an extension's name must come from elsewhere. Callers using the
        // generic loader for extensions should populate that themselves; here
        // we surface the path with no name so it can be patched later.
        SourceScope::Extension => {
            debug_assert!(
                false,
                "discover_resources cannot resolve Extension scope; callers must wrap and set extension_name explicitly",
            );
            SourceInfo {
                scope: SourceScope::Extension,
                path: path.to_path_buf(),
                extension_name: None,
            }
        }
    };

    Ok(Some(DiscoveredResource {
        name,
        metadata: parsed.metadata,
        body: parsed.body,
        source,
    }))
}

fn parent_dir_name(path: &Path) -> Option<String> {
    path.parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
}

fn file_stem_name(path: &Path) -> Option<String> {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::fs;
    use tempfile::TempDir;

    #[derive(Debug, Deserialize, PartialEq)]
    struct TestMeta {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        description: Option<String>,
    }

    /// Write `<root>/<dir>/<filename>` with `body`, creating parents as needed.
    fn write_resource(root: &Path, dir: &str, filename: &str, body: &str) -> PathBuf {
        let resource_dir = root.join(dir);
        fs::create_dir_all(&resource_dir).unwrap();
        let file = resource_dir.join(filename);
        fs::write(&file, body).unwrap();
        file
    }

    fn skill_md(name: &str, description: &str, body: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}")
    }

    fn config(roots: Vec<(PathBuf, SourceScope)>) -> ResourceLoaderConfig {
        ResourceLoaderConfig {
            roots,
            resource_filename: "SKILL.md",
            name_resolver: NameResolver::ParentDirName,
            layout: LayoutKind::PerDirectory,
        }
    }

    // 1. Single-root happy path.
    #[test]
    fn discovers_single_root() {
        let tmp = TempDir::new().unwrap();
        let builtin = tmp.path().join("builtin");
        write_resource(
            &builtin,
            "skill_a",
            "SKILL.md",
            &skill_md("skill_a", "desc", "body content"),
        );

        let cfg = config(vec![(builtin.clone(), SourceScope::Builtin)]);
        let result = discover_resources::<TestMeta>(&cfg).unwrap();

        assert_eq!(result.len(), 1);
        let r = &result[0];
        assert_eq!(r.name, "skill_a");
        assert_eq!(r.source.scope, SourceScope::Builtin);
        assert_eq!(r.body, "body content");
        assert_eq!(
            r.metadata.as_ref().unwrap().description.as_deref(),
            Some("desc")
        );
    }

    // 2. Multi-scope precedence: project shadows builtin.
    #[test]
    fn project_shadows_builtin() {
        let tmp = TempDir::new().unwrap();
        let builtin = tmp.path().join("builtin");
        let project = tmp.path().join("project");
        write_resource(
            &builtin,
            "skill_a",
            "SKILL.md",
            &skill_md("skill_a", "from builtin", "builtin body"),
        );
        write_resource(
            &project,
            "skill_a",
            "SKILL.md",
            &skill_md("skill_a", "from project", "project body"),
        );

        let cfg = config(vec![
            (builtin, SourceScope::Builtin),
            (project, SourceScope::Project),
        ]);
        let result = discover_resources::<TestMeta>(&cfg).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source.scope, SourceScope::Project);
        assert_eq!(result[0].body, "project body");
    }

    // 3. Three-scope precedence: project wins over both user and builtin.
    #[test]
    fn three_scope_precedence_picks_project() {
        let tmp = TempDir::new().unwrap();
        let builtin = tmp.path().join("builtin");
        let user = tmp.path().join("user");
        let project = tmp.path().join("project");
        for (root, label) in [(&builtin, "b"), (&user, "u"), (&project, "p")] {
            write_resource(
                root,
                "skill_a",
                "SKILL.md",
                &skill_md("skill_a", label, label),
            );
        }

        let cfg = config(vec![
            (builtin, SourceScope::Builtin),
            (user, SourceScope::User),
            (project, SourceScope::Project),
        ]);
        let result = discover_resources::<TestMeta>(&cfg).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source.scope, SourceScope::Project);
        assert_eq!(result[0].body, "p");
    }

    // 4. Resource only in user scope.
    #[test]
    fn user_only_keeps_user_scope() {
        let tmp = TempDir::new().unwrap();
        let builtin = tmp.path().join("builtin");
        let user = tmp.path().join("user");
        let project = tmp.path().join("project");
        // Make sure the other roots exist but are empty.
        fs::create_dir_all(&builtin).unwrap();
        fs::create_dir_all(&project).unwrap();
        write_resource(
            &user,
            "skill_b",
            "SKILL.md",
            &skill_md("skill_b", "u", "u body"),
        );

        let cfg = config(vec![
            (builtin, SourceScope::Builtin),
            (user, SourceScope::User),
            (project, SourceScope::Project),
        ]);
        let result = discover_resources::<TestMeta>(&cfg).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "skill_b");
        assert_eq!(result[0].source.scope, SourceScope::User);
    }

    // 5. Result is sorted by name.
    #[test]
    fn results_sorted_by_name() {
        let tmp = TempDir::new().unwrap();
        let builtin = tmp.path().join("builtin");
        for n in ["skill_z", "skill_a", "skill_m"] {
            write_resource(&builtin, n, "SKILL.md", &skill_md(n, "d", "b"));
        }

        let cfg = config(vec![(builtin, SourceScope::Builtin)]);
        let result = discover_resources::<TestMeta>(&cfg).unwrap();

        let names: Vec<_> = result.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["skill_a", "skill_m", "skill_z"]);
    }

    // 6. All roots exist but empty.
    #[test]
    fn empty_roots_return_empty() {
        let tmp = TempDir::new().unwrap();
        let builtin = tmp.path().join("builtin");
        let project = tmp.path().join("project");
        fs::create_dir_all(&builtin).unwrap();
        fs::create_dir_all(&project).unwrap();

        let cfg = config(vec![
            (builtin, SourceScope::Builtin),
            (project, SourceScope::Project),
        ]);
        let result = discover_resources::<TestMeta>(&cfg).unwrap();
        assert!(result.is_empty());
    }

    // 7. Missing root directories are silently skipped.
    #[test]
    fn missing_root_is_silently_skipped() {
        let tmp = TempDir::new().unwrap();
        let builtin = tmp.path().join("builtin");
        let user = tmp.path().join("does_not_exist"); // never created
        let project = tmp.path().join("project");
        write_resource(
            &builtin,
            "skill_a",
            "SKILL.md",
            &skill_md("skill_a", "b", "b"),
        );
        write_resource(
            &project,
            "skill_b",
            "SKILL.md",
            &skill_md("skill_b", "p", "p"),
        );

        let cfg = config(vec![
            (builtin, SourceScope::Builtin),
            (user, SourceScope::User),
            (project, SourceScope::Project),
        ]);
        let result = discover_resources::<TestMeta>(&cfg).unwrap();

        let names: Vec<_> = result.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["skill_a", "skill_b"]);
    }

    // 8. Per-file frontmatter error: lenient mode collects, strict aborts.
    #[test]
    fn lenient_collects_frontmatter_errors() {
        let tmp = TempDir::new().unwrap();
        let builtin = tmp.path().join("builtin");
        write_resource(
            &builtin,
            "good",
            "SKILL.md",
            &skill_md("good", "ok", "good body"),
        );
        // Invalid YAML (double colon).
        let bad_path = write_resource(&builtin, "bad", "SKILL.md", "---\nname: : :\n---\nbody");

        let cfg = config(vec![(builtin, SourceScope::Builtin)]);
        let (successes, failures) = discover_resources_lenient::<TestMeta>(&cfg);

        assert_eq!(successes.len(), 1);
        assert_eq!(successes[0].name, "good");
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, bad_path);
        assert!(matches!(
            failures[0].1,
            ResourceLoaderError::Frontmatter { .. }
        ));
        // Strict mode aborts.
        let strict = discover_resources::<TestMeta>(&cfg);
        assert!(strict.is_err());
    }

    // 9. Subdirectory missing the configured filename is ignored.
    #[test]
    fn subdir_without_resource_filename_ignored() {
        let tmp = TempDir::new().unwrap();
        let builtin = tmp.path().join("builtin");
        // Wrong filename (something.md, not SKILL.md).
        write_resource(
            &builtin,
            "skill_x",
            "something.md",
            &skill_md("skill_x", "x", "x"),
        );

        let cfg = config(vec![(builtin, SourceScope::Builtin)]);
        let result = discover_resources::<TestMeta>(&cfg).unwrap();
        assert!(result.is_empty());
    }

    // Flat layout + FileStem resolver: each `<root>/<name>.md` is one resource.
    #[test]
    fn flat_layout_with_file_stem_resolver() {
        let tmp = TempDir::new().unwrap();
        let builtin = tmp.path().join("templates");
        fs::create_dir_all(&builtin).unwrap();
        fs::write(
            builtin.join("alpha.md"),
            "---\ndescription: a\n---\nalpha body",
        )
        .unwrap();
        fs::write(
            builtin.join("beta.md"),
            "---\ndescription: b\n---\nbeta body",
        )
        .unwrap();
        // Subdir and non-md file should be ignored under Flat layout.
        fs::create_dir_all(builtin.join("subdir")).unwrap();
        fs::write(builtin.join("README.txt"), "ignored").unwrap();

        let cfg = ResourceLoaderConfig {
            roots: vec![(builtin, SourceScope::Builtin)],
            resource_filename: "",
            name_resolver: NameResolver::FileStem,
            layout: LayoutKind::Flat,
        };
        let result = discover_resources::<TestMeta>(&cfg).unwrap();

        let names: Vec<_> = result.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
        assert_eq!(result[0].body, "alpha body");
        assert_eq!(result[1].body, "beta body");
    }

    // 10. A non-directory entry directly under the root is ignored.
    #[test]
    fn non_directory_entry_under_root_ignored() {
        let tmp = TempDir::new().unwrap();
        let builtin = tmp.path().join("builtin");
        fs::create_dir_all(&builtin).unwrap();
        // Stray README.md directly under the root, not in a subdir.
        fs::write(builtin.join("README.md"), "hello").unwrap();
        // Also add a real resource so we can verify it does get picked up.
        write_resource(
            &builtin,
            "skill_a",
            "SKILL.md",
            &skill_md("skill_a", "a", "a"),
        );

        let cfg = config(vec![(builtin, SourceScope::Builtin)]);
        let result = discover_resources::<TestMeta>(&cfg).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "skill_a");
    }
}
