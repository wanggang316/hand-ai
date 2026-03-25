//! Extension loader — discovers and loads extensions from the filesystem.
//!
//! Extensions are discovered from:
//! - `~/.hand/agent/extensions/*.rs` (global)
//! - `~/.hand/agent/extensions/*/mod.rs` (global subdirectory)
//! - `.hand/extensions/*.rs` (project-local)
//! - `.hand/extensions/*/mod.rs` (project-local subdirectory)
//! - Paths specified in settings

use std::path::{Path, PathBuf};

use super::types::{Extension, ExtensionConfig, ExtensionError, ExtensionManifest};

/// Result of loading extensions.
#[derive(Debug, Clone)]
pub struct LoadExtensionsResult {
    /// Successfully loaded extensions.
    pub extensions: Vec<Extension>,
    /// Errors encountered during loading.
    pub errors: Vec<ExtensionError>,
}

/// Standard discovery paths for extensions.
fn discovery_paths(global_dir: Option<&Path>, project_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(global) = global_dir {
        let ext_dir = global.join("agent").join("extensions");
        if ext_dir.is_dir() {
            paths.push(ext_dir);
        }
    }

    if let Some(project) = project_dir {
        let ext_dir = project.join("extensions");
        if ext_dir.is_dir() {
            paths.push(ext_dir);
        }
    }

    paths
}

/// Discover extension files in standard locations.
pub fn discover_extensions(config: &ExtensionConfig) -> Vec<PathBuf> {
    let mut found = Vec::new();

    if config.auto_discover {
        let dirs = discovery_paths(config.global_dir.as_deref(), config.project_dir.as_deref());

        for dir in dirs {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if is_extension_file(&path) {
                        found.push(path);
                    } else if path.is_dir() {
                        // Check for index file in subdirectory
                        let index = path.join("mod.rs");
                        if index.is_file() {
                            found.push(index);
                        }
                    }
                }
            }
        }
    }

    // Add explicitly configured paths
    for path in &config.paths {
        let expanded = expand_path(path);
        if expanded.is_file() && is_extension_file(&expanded) {
            found.push(expanded);
        } else if expanded.is_dir() {
            let index = expanded.join("mod.rs");
            if index.is_file() {
                found.push(index);
            }
        }
    }

    found.sort();
    found.dedup();
    found
}

/// Check if a path is an extension file.
fn is_extension_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .is_some_and(|ext| ext == "rs" || ext == "wasm" || ext == "so" || ext == "dylib")
}

/// Expand ~ in paths.
fn expand_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s.starts_with("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(&s[2..]);
    }
    path.to_path_buf()
}

/// Load a single extension from a file path.
///
/// Currently creates a manifest-only extension since dynamic loading
/// (via WASM or dylib) is not yet implemented. The skeleton tracks
/// what would be loaded and where.
pub fn load_extension(path: &Path) -> Result<Extension, ExtensionError> {
    if !path.is_file() {
        return Err(ExtensionError {
            extension_path: path.to_path_buf(),
            event: "load".to_string(),
            message: format!("Extension file not found: {}", path.display()),
        });
    }

    // Extract name from filename
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let manifest = ExtensionManifest {
        name,
        description: String::new(),
        version: "0.0.0".to_string(),
        path: path.to_path_buf(),
    };

    Ok(Extension::new(manifest))
}

/// Discover and load all extensions according to configuration.
pub fn load_extensions(config: &ExtensionConfig) -> LoadExtensionsResult {
    let paths = discover_extensions(config);
    let mut extensions = Vec::new();
    let mut errors = Vec::new();

    for path in paths {
        match load_extension(&path) {
            Ok(ext) => extensions.push(ext),
            Err(e) => errors.push(e),
        }
    }

    LoadExtensionsResult { extensions, errors }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn discover_empty_config() {
        let config = ExtensionConfig::default();
        let found = discover_extensions(&config);
        assert!(found.is_empty());
    }

    #[test]
    fn discover_with_auto_discover() {
        let dir = TempDir::new().unwrap();
        let ext_dir = dir.path().join("extensions");
        fs::create_dir_all(&ext_dir).unwrap();
        fs::write(ext_dir.join("my_ext.rs"), "// extension").unwrap();
        fs::write(ext_dir.join("not_ext.txt"), "text").unwrap();

        let config = ExtensionConfig {
            auto_discover: true,
            project_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };

        let found = discover_extensions(&config);
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("my_ext.rs"));
    }

    #[test]
    fn discover_subdirectory_with_mod() {
        let dir = TempDir::new().unwrap();
        let ext_dir = dir.path().join("extensions").join("my_plugin");
        fs::create_dir_all(&ext_dir).unwrap();
        fs::write(ext_dir.join("mod.rs"), "// plugin").unwrap();

        let config = ExtensionConfig {
            auto_discover: true,
            project_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };

        let found = discover_extensions(&config);
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("mod.rs"));
    }

    #[test]
    fn discover_explicit_paths() {
        let dir = TempDir::new().unwrap();
        let ext_file = dir.path().join("custom.rs");
        fs::write(&ext_file, "// custom ext").unwrap();

        let config = ExtensionConfig {
            paths: vec![ext_file.clone()],
            ..Default::default()
        };

        let found = discover_extensions(&config);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0], ext_file);
    }

    #[test]
    fn load_extension_valid_file() {
        let dir = TempDir::new().unwrap();
        let ext_file = dir.path().join("hello.rs");
        fs::write(&ext_file, "// hello extension").unwrap();

        let ext = load_extension(&ext_file).unwrap();
        assert_eq!(ext.manifest.name, "hello");
        assert_eq!(ext.manifest.path, ext_file);
    }

    #[test]
    fn load_extension_missing_file() {
        let result = load_extension(Path::new("/nonexistent/ext.rs"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("not found"));
    }

    #[test]
    fn load_extensions_mixed() {
        let dir = TempDir::new().unwrap();
        let ext_dir = dir.path().join("extensions");
        fs::create_dir_all(&ext_dir).unwrap();
        fs::write(ext_dir.join("good.rs"), "// good").unwrap();
        fs::write(ext_dir.join("also_good.rs"), "// also good").unwrap();

        let config = ExtensionConfig {
            auto_discover: true,
            project_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };

        let result = load_extensions(&config);
        assert_eq!(result.extensions.len(), 2);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn is_extension_file_checks() {
        let dir = TempDir::new().unwrap();
        // Create actual files to test against
        let rs_file = dir.path().join("test.rs");
        let wasm_file = dir.path().join("plugin.wasm");
        let md_file = dir.path().join("readme.md");
        let json_file = dir.path().join("data.json");
        fs::write(&rs_file, "").unwrap();
        fs::write(&wasm_file, "").unwrap();
        fs::write(&md_file, "").unwrap();
        fs::write(&json_file, "").unwrap();

        assert!(is_extension_file(&rs_file));
        assert!(is_extension_file(&wasm_file));
        assert!(!is_extension_file(&md_file));
        assert!(!is_extension_file(&json_file));
    }

    #[test]
    fn load_extensions_result_fields() {
        let result = LoadExtensionsResult {
            extensions: vec![],
            errors: vec![],
        };
        assert!(result.extensions.is_empty());
        assert!(result.errors.is_empty());
    }
}
