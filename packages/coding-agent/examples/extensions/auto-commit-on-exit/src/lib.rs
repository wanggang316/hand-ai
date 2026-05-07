//! Tier 1 extension: when the session ends, auto-commit any uncommitted
//! changes in the session's working directory.
//!
//! Ported from `pi-mono/.../examples/extensions/auto-commit-on-exit.ts`.
//! The pi-mono original derives a commit subject from the last assistant
//! message; we use a static subject because hand's `on_shutdown` hook does
//! not yet receive the message log. Lifting that limitation is a future
//! enhancement.
//!
//! All git operations are best-effort: failures are logged via
//! `tracing::warn!` and never surfaced as errors. A coding session must not
//! fail to shut down because git happens to be unhappy.

use async_trait::async_trait;
use hand_coding_agent::{
    Extension, ExtensionContext, ExtensionError, ExtensionManifest,
};
use std::path::Path;
use tokio::process::Command;

/// Commit message used when the session ends with uncommitted work.
pub const AUTO_COMMIT_MESSAGE: &str = "auto-commit: end of session";

pub struct AutoCommitOnExit {
    manifest: ExtensionManifest,
}

impl AutoCommitOnExit {
    pub fn new() -> Self {
        Self {
            manifest: ExtensionManifest {
                name: "auto-commit-on-exit".to_string(),
                version: "0.1.0".to_string(),
                description: Some(
                    "Auto-commits uncommitted changes when the agent session ends.".to_string(),
                ),
                capabilities: hand_coding_agent::core::extensions::api::ExtensionCapabilities::default(),
                exec: None,
                env: Default::default(),
                slash_commands: Vec::new(),
                custom_tools: Vec::new(),
            },
        }
    }
}

impl Default for AutoCommitOnExit {
    fn default() -> Self {
        Self::new()
    }
}

/// Run `git status --porcelain`. Returns Some(true) for dirty, Some(false)
/// for clean, None if git could not be invoked or the dir is not a repo.
async fn dirty_status(dir: &Path) -> Option<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(!output.stdout.is_empty())
}

async fn run_git(dir: &Path, args: &[&str]) -> Option<std::process::ExitStatus> {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .ok()
        .map(|o| o.status)
}

#[async_trait]
impl Extension for AutoCommitOnExit {
    fn manifest(&self) -> &ExtensionManifest {
        &self.manifest
    }

    async fn on_shutdown(&self, cx: &ExtensionContext) -> Result<(), ExtensionError> {
        match dirty_status(&cx.cwd).await {
            Some(true) => {
                if let Some(s) = run_git(&cx.cwd, &["add", "-A"]).await
                    && !s.success()
                {
                    tracing::warn!(cwd = %cx.cwd.display(), "auto-commit-on-exit: git add failed");
                    return Ok(());
                }
                match run_git(&cx.cwd, &["commit", "-m", AUTO_COMMIT_MESSAGE]).await {
                    Some(s) if s.success() => {
                        tracing::info!(
                            cwd = %cx.cwd.display(),
                            "auto-commit-on-exit: committed end-of-session changes"
                        );
                    }
                    Some(s) => {
                        tracing::warn!(
                            cwd = %cx.cwd.display(),
                            status = ?s,
                            "auto-commit-on-exit: git commit returned non-zero"
                        );
                    }
                    None => {
                        tracing::warn!(
                            cwd = %cx.cwd.display(),
                            "auto-commit-on-exit: git commit could not be invoked"
                        );
                    }
                }
            }
            Some(false) => {
                tracing::debug!(
                    cwd = %cx.cwd.display(),
                    "auto-commit-on-exit: tree clean, nothing to do"
                );
            }
            None => {
                tracing::debug!(
                    cwd = %cx.cwd.display(),
                    "auto-commit-on-exit: not a git repo or git unavailable"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_ctx(cwd: PathBuf) -> ExtensionContext {
        ExtensionContext {
            cwd: cwd.clone(),
            session_id: "test-session".to_string(),
            data_dir: cwd.join(".data"),
        }
    }

    /// Initialize a git repo with a known identity. Returns false (test should
    /// be skipped) if git is unavailable.
    async fn init_git_repo(dir: &Path) -> bool {
        if Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(dir)
            .output()
            .await
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            return false;
        }
        for args in [
            ["config", "user.email", "test@example.com"],
            ["config", "user.name", "Test"],
            ["config", "commit.gpgsign", "false"],
        ] {
            let _ = Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .await;
        }
        true
    }

    /// Count commits on `HEAD`. Returns 0 if there are no commits yet.
    async fn commit_count(dir: &Path) -> usize {
        let out = Command::new("git")
            .args(["rev-list", "--count", "HEAD"])
            .current_dir(dir)
            .output()
            .await;
        match out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse()
                .unwrap_or(0),
            _ => 0,
        }
    }

    /// Subject line of the most recent commit, or empty string if none.
    async fn last_commit_subject(dir: &Path) -> String {
        let out = Command::new("git")
            .args(["log", "-1", "--pretty=%s"])
            .current_dir(dir)
            .output()
            .await;
        match out {
            Ok(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout).trim().to_string()
            }
            _ => String::new(),
        }
    }

    #[tokio::test]
    async fn dirty_repo_gets_auto_commit() {
        let dir = TempDir::new().unwrap();
        if !init_git_repo(dir.path()).await {
            eprintln!("skipping: git not available");
            return;
        }
        // Make an initial commit so HEAD exists; then add an untracked
        // file so the repo is dirty.
        std::fs::write(dir.path().join("seed.txt"), "seed").unwrap();
        let _ = run_git(dir.path(), &["add", "-A"]).await;
        let _ = run_git(dir.path(), &["commit", "-m", "seed"]).await;
        let before = commit_count(dir.path()).await;
        assert_eq!(before, 1);

        std::fs::write(dir.path().join("new.txt"), "hello").unwrap();

        let ext = AutoCommitOnExit::new();
        ext.on_shutdown(&make_ctx(dir.path().to_path_buf()))
            .await
            .unwrap();

        let after = commit_count(dir.path()).await;
        assert_eq!(after, before + 1, "expected exactly one new commit");
        assert_eq!(last_commit_subject(dir.path()).await, AUTO_COMMIT_MESSAGE);
    }

    #[tokio::test]
    async fn clean_repo_is_a_no_op() {
        let dir = TempDir::new().unwrap();
        if !init_git_repo(dir.path()).await {
            eprintln!("skipping: git not available");
            return;
        }
        std::fs::write(dir.path().join("seed.txt"), "seed").unwrap();
        let _ = run_git(dir.path(), &["add", "-A"]).await;
        let _ = run_git(dir.path(), &["commit", "-m", "seed"]).await;
        let before = commit_count(dir.path()).await;

        let ext = AutoCommitOnExit::new();
        ext.on_shutdown(&make_ctx(dir.path().to_path_buf()))
            .await
            .unwrap();

        let after = commit_count(dir.path()).await;
        assert_eq!(after, before, "no commit should be created when tree is clean");
    }
}
