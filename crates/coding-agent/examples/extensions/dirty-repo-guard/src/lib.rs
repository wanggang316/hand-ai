//! Tier 1 extension: blocks `write` / `edit` tool calls when the working
//! directory has uncommitted git changes.
//!
//! Hand's extension API does not yet expose `session_before_switch` /
//! `session_before_fork`, so this fixture approximates the same intent
//! at the most relevant available hook — the moment before a
//! write-shaped tool call mutates the tree.
//!
//! Algorithm: shell out to `git status --porcelain` in `cx.cwd`. A non-empty
//! stdout means the repo is dirty. A non-zero exit (e.g. cwd not a git repo)
//! is treated as "allow" — no information, no veto.

use async_trait::async_trait;
use hand_coding_agent::core::extensions::api::ToolCallEvent;
use hand_coding_agent::{
    Extension, ExtensionContext, ExtensionError, ExtensionManifest, HookDecision,
};
use std::path::Path;
use tokio::process::Command;

/// Tools that mutate files on disk and therefore require a clean tree.
/// Other tools (`read`, `bash`, `grep`, ...) are allowed regardless of
/// repo state.
const GUARDED_TOOLS: &[&str] = &["write", "edit"];

pub struct DirtyRepoGuard {
    manifest: ExtensionManifest,
}

impl DirtyRepoGuard {
    pub fn new() -> Self {
        Self {
            manifest: ExtensionManifest {
                name: "dirty-repo-guard".to_string(),
                version: "0.1.0".to_string(),
                description: Some(
                    "Blocks write/edit tool calls when the working dir has \
                     uncommitted git changes."
                        .to_string(),
                ),
                capabilities: hand_coding_agent::core::extensions::api::ExtensionCapabilities {
                    before_tool_call: true,
                    ..Default::default()
                },
                exec: None,
                env: Default::default(),
                slash_commands: Vec::new(),
                custom_tools: Vec::new(),
            },
        }
    }
}

impl Default for DirtyRepoGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns `Ok(true)` if the directory is a git repo with uncommitted
/// changes, `Ok(false)` otherwise (clean repo, or not a repo at all). An
/// `Err` is returned only when the subprocess itself could not be spawned.
async fn is_dirty(dir: &Path) -> std::io::Result<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()
        .await?;
    if !output.status.success() {
        // Not a git repo (or git not installed). No information => no veto.
        return Ok(false);
    }
    Ok(!output.stdout.is_empty())
}

#[async_trait]
impl Extension for DirtyRepoGuard {
    fn manifest(&self) -> &ExtensionManifest {
        &self.manifest
    }

    async fn on_before_tool_call(
        &self,
        cx: &ExtensionContext,
        event: &ToolCallEvent,
    ) -> Result<HookDecision, ExtensionError> {
        if !GUARDED_TOOLS.contains(&event.tool_name.as_str()) {
            return Ok(HookDecision::Continue);
        }
        match is_dirty(&cx.cwd).await {
            Ok(true) => Ok(HookDecision::Cancel(
                "dirty repo: commit or stash before editing".to_string(),
            )),
            Ok(false) => Ok(HookDecision::Continue),
            Err(err) => {
                tracing::warn!(error = %err, "dirty-repo-guard: git status failed; allowing");
                Ok(HookDecision::Continue)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tokio::process::Command;

    fn make_ctx(cwd: PathBuf) -> ExtensionContext {
        ExtensionContext {
            cwd: cwd.clone(),
            session_id: "test-session".to_string(),
            data_dir: cwd.join(".data"),
        }
    }

    fn write_event(path: &str) -> ToolCallEvent {
        ToolCallEvent {
            tool_name: "write".to_string(),
            arguments: serde_json::json!({ "path": path, "content": "x" }),
            call_id: "call-1".to_string(),
        }
    }

    /// Initialize a git repo at `dir`. Skips the test if git is not
    /// available on the runner.
    async fn init_git_repo(dir: &Path) -> bool {
        let init = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(dir)
            .output()
            .await;
        let init = match init {
            Ok(o) => o,
            Err(_) => return false,
        };
        if !init.status.success() {
            return false;
        }
        // Identity required for `git commit` to succeed.
        let _ = Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir)
            .output()
            .await;
        let _ = Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir)
            .output()
            .await;
        let _ = Command::new("git")
            .args(["config", "commit.gpgsign", "false"])
            .current_dir(dir)
            .output()
            .await;
        true
    }

    #[tokio::test]
    async fn non_write_tool_is_allowed_even_in_dirty_repo() {
        // No git interaction at all — we just verify the early-return path
        // for tools that aren't guarded.
        let dir = TempDir::new().unwrap();
        let cx = make_ctx(dir.path().to_path_buf());
        let guard = DirtyRepoGuard::new();
        let event = ToolCallEvent {
            tool_name: "read".to_string(),
            arguments: serde_json::json!({ "path": "/etc/hosts" }),
            call_id: "c".into(),
        };
        let decision = guard.on_before_tool_call(&cx, &event).await.unwrap();
        assert!(matches!(decision, HookDecision::Continue));
    }

    #[tokio::test]
    async fn dirty_repo_cancels_write() {
        let dir = TempDir::new().unwrap();
        if !init_git_repo(dir.path()).await {
            eprintln!("skipping: git not available");
            return;
        }
        // Create an untracked file → repo is dirty.
        std::fs::write(dir.path().join("untracked.txt"), "hi").unwrap();

        let cx = make_ctx(dir.path().to_path_buf());
        let guard = DirtyRepoGuard::new();
        let decision = guard
            .on_before_tool_call(&cx, &write_event("a.txt"))
            .await
            .unwrap();
        match decision {
            HookDecision::Cancel(reason) => {
                assert!(reason.contains("dirty repo"), "unexpected reason: {reason}");
            }
            other => panic!("expected Cancel for dirty repo, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn clean_repo_allows_write() {
        let dir = TempDir::new().unwrap();
        if !init_git_repo(dir.path()).await {
            eprintln!("skipping: git not available");
            return;
        }
        // No untracked or modified files → clean.
        let cx = make_ctx(dir.path().to_path_buf());
        let guard = DirtyRepoGuard::new();
        let decision = guard
            .on_before_tool_call(&cx, &write_event("a.txt"))
            .await
            .unwrap();
        assert!(matches!(decision, HookDecision::Continue));
    }
}
