//! Package manager and language detection from project files.

use std::path::Path;

/// Detected package manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Cargo,
    Npm,
    Yarn,
    Pnpm,
    Bun,
    Unknown,
}

impl std::fmt::Display for PackageManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageManager::Cargo => write!(f, "cargo"),
            PackageManager::Npm => write!(f, "npm"),
            PackageManager::Yarn => write!(f, "yarn"),
            PackageManager::Pnpm => write!(f, "pnpm"),
            PackageManager::Bun => write!(f, "bun"),
            PackageManager::Unknown => write!(f, "unknown"),
        }
    }
}

/// Detected primary programming language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Go,
    Java,
    CSharp,
    Ruby,
    Unknown,
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Language::Rust => write!(f, "rust"),
            Language::TypeScript => write!(f, "typescript"),
            Language::JavaScript => write!(f, "javascript"),
            Language::Python => write!(f, "python"),
            Language::Go => write!(f, "go"),
            Language::Java => write!(f, "java"),
            Language::CSharp => write!(f, "csharp"),
            Language::Ruby => write!(f, "ruby"),
            Language::Unknown => write!(f, "unknown"),
        }
    }
}

/// Detect the package manager from lock files in the given directory.
pub fn detect_package_manager(dir: &Path) -> PackageManager {
    // Order matters — more specific lock files first
    if dir.join("Cargo.lock").exists() || dir.join("Cargo.toml").exists() {
        PackageManager::Cargo
    } else if dir.join("bun.lockb").exists() || dir.join("bun.lock").exists() {
        PackageManager::Bun
    } else if dir.join("pnpm-lock.yaml").exists() {
        PackageManager::Pnpm
    } else if dir.join("yarn.lock").exists() {
        PackageManager::Yarn
    } else if dir.join("package-lock.json").exists() || dir.join("package.json").exists() {
        PackageManager::Npm
    } else {
        PackageManager::Unknown
    }
}

/// Detect the primary programming language from project files.
pub fn detect_language(dir: &Path) -> Language {
    if dir.join("Cargo.toml").exists() {
        Language::Rust
    } else if dir.join("tsconfig.json").exists() {
        Language::TypeScript
    } else if dir.join("go.mod").exists() {
        Language::Go
    } else if dir.join("pyproject.toml").exists()
        || dir.join("setup.py").exists()
        || dir.join("requirements.txt").exists()
    {
        Language::Python
    } else if dir.join("pom.xml").exists() || dir.join("build.gradle").exists() {
        Language::Java
    } else if dir.join("Gemfile").exists() {
        Language::Ruby
    } else if dir.join("*.csproj").exists() || dir.join("*.sln").exists() {
        Language::CSharp
    } else if dir.join("package.json").exists() {
        Language::JavaScript
    } else {
        Language::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn detect_cargo() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(detect_package_manager(dir.path()), PackageManager::Cargo);
    }

    #[test]
    fn detect_npm() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("package-lock.json"), "{}").unwrap();
        assert_eq!(detect_package_manager(dir.path()), PackageManager::Npm);
    }

    #[test]
    fn detect_yarn() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("yarn.lock"), "").unwrap();
        assert_eq!(detect_package_manager(dir.path()), PackageManager::Yarn);
    }

    #[test]
    fn detect_pnpm() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(detect_package_manager(dir.path()), PackageManager::Pnpm);
    }

    #[test]
    fn detect_bun() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("bun.lockb"), "").unwrap();
        assert_eq!(detect_package_manager(dir.path()), PackageManager::Bun);
    }

    #[test]
    fn detect_unknown_package_manager() {
        let dir = TempDir::new().unwrap();
        assert_eq!(detect_package_manager(dir.path()), PackageManager::Unknown);
    }

    #[test]
    fn detect_rust_language() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        assert_eq!(detect_language(dir.path()), Language::Rust);
    }

    #[test]
    fn detect_typescript_language() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        assert_eq!(detect_language(dir.path()), Language::TypeScript);
    }

    #[test]
    fn detect_python_language() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "").unwrap();
        assert_eq!(detect_language(dir.path()), Language::Python);
    }

    #[test]
    fn detect_go_language() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("go.mod"), "").unwrap();
        assert_eq!(detect_language(dir.path()), Language::Go);
    }

    #[test]
    fn package_manager_display() {
        assert_eq!(PackageManager::Cargo.to_string(), "cargo");
        assert_eq!(PackageManager::Npm.to_string(), "npm");
    }

    #[test]
    fn language_display() {
        assert_eq!(Language::Rust.to_string(), "rust");
        assert_eq!(Language::TypeScript.to_string(), "typescript");
    }
}
