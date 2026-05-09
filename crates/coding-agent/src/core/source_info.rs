//! Resource origin tracking.
//!
//! Identifies which on-disk location and which scope a discovered resource
//! came from. Used by the resource loader to attribute resources back to
//! their source for diagnostics, deduplication, and hot-reload.

use std::path::PathBuf;

/// The scope a resource was discovered in. Higher-priority scopes shadow
/// lower-priority scopes by canonical name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SourceScope {
    /// Built-in resource shipped with hand-coding-agent.
    Builtin = 0,
    /// User-level resource, typically `~/.hand/<kind>/`.
    User = 1,
    /// Project-level resource, typically `<cwd>/.hand/<kind>/`.
    Project = 2,
    /// An extension contributed this resource (Phase 3+).
    Extension = 3,
}

/// Where a resource was loaded from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceInfo {
    pub scope: SourceScope,
    /// On-disk path. For Builtin sources, may point at an embedded asset
    /// path or a synthesized path (e.g., `<builtin>/skill_name`).
    pub path: PathBuf,
    /// For Extension scope: which extension contributed this resource.
    pub extension_name: Option<String>,
}

impl SourceInfo {
    pub fn builtin(path: impl Into<PathBuf>) -> Self {
        Self {
            scope: SourceScope::Builtin,
            path: path.into(),
            extension_name: None,
        }
    }

    pub fn user(path: impl Into<PathBuf>) -> Self {
        Self {
            scope: SourceScope::User,
            path: path.into(),
            extension_name: None,
        }
    }

    pub fn project(path: impl Into<PathBuf>) -> Self {
        Self {
            scope: SourceScope::Project,
            path: path.into(),
            extension_name: None,
        }
    }

    pub fn extension(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            scope: SourceScope::Extension,
            path: path.into(),
            extension_name: Some(name.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_ordering_is_low_to_high() {
        assert!(SourceScope::Builtin < SourceScope::User);
        assert!(SourceScope::User < SourceScope::Project);
        assert!(SourceScope::Project < SourceScope::Extension);
    }

    #[test]
    fn builtin_constructor_sets_scope_and_no_extension() {
        let info = SourceInfo::builtin("/foo/bar");
        assert_eq!(info.scope, SourceScope::Builtin);
        assert_eq!(info.path, PathBuf::from("/foo/bar"));
        assert!(info.extension_name.is_none());
    }

    #[test]
    fn user_constructor_sets_user_scope() {
        let info = SourceInfo::user("/u");
        assert_eq!(info.scope, SourceScope::User);
        assert!(info.extension_name.is_none());
    }

    #[test]
    fn project_constructor_sets_project_scope() {
        let info = SourceInfo::project("/p");
        assert_eq!(info.scope, SourceScope::Project);
        assert!(info.extension_name.is_none());
    }

    #[test]
    fn extension_constructor_carries_name() {
        let info = SourceInfo::extension("ext-a", "/e/x");
        assert_eq!(info.scope, SourceScope::Extension);
        assert_eq!(info.extension_name.as_deref(), Some("ext-a"));
    }
}
