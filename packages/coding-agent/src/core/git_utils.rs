//! Git utilities — repository info for system prompt and diagnostics.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Get the current git branch name.
pub fn git_branch(cwd: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;

    if output.status.success() {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if branch.is_empty() || branch == "HEAD" {
            // Detached HEAD — try to get short hash
            let hash_output = Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .current_dir(cwd)
                .output()
                .ok()?;
            Some(
                String::from_utf8_lossy(&hash_output.stdout)
                    .trim()
                    .to_string(),
            )
        } else {
            Some(branch)
        }
    } else {
        None
    }
}

/// Get the git repository root directory.
pub fn git_root(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()?;

    if output.status.success() {
        let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Some(PathBuf::from(root))
    } else {
        None
    }
}

/// Check if the working directory has uncommitted changes.
pub fn git_is_dirty(cwd: &Path) -> bool {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output();

    match output {
        Ok(o) if o.status.success() => !o.stdout.is_empty(),
        _ => false,
    }
}

/// Get a short status summary (e.g., "3 modified, 1 untracked").
pub fn git_status_summary(cwd: &Path) -> String {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output();

    let lines = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(String::from)
            .collect::<Vec<_>>(),
        _ => return "not a git repository".to_string(),
    };

    if lines.is_empty() {
        return "clean".to_string();
    }

    let mut modified = 0;
    let mut added = 0;
    let mut deleted = 0;
    let mut untracked = 0;

    for line in &lines {
        if line.len() < 2 {
            continue;
        }
        match &line[..2] {
            "??" => untracked += 1,
            " M" | "M " | "MM" => modified += 1,
            "A " | "AM" => added += 1,
            " D" | "D " => deleted += 1,
            _ => modified += 1, // fallback
        }
    }

    let mut parts = Vec::new();
    if modified > 0 {
        parts.push(format!("{modified} modified"));
    }
    if added > 0 {
        parts.push(format!("{added} added"));
    }
    if deleted > 0 {
        parts.push(format!("{deleted} deleted"));
    }
    if untracked > 0 {
        parts.push(format!("{untracked} untracked"));
    }

    if parts.is_empty() {
        "clean".to_string()
    } else {
        parts.join(", ")
    }
}

/// Get recent commit messages.
pub fn git_recent_commits(cwd: &Path, n: usize) -> Vec<String> {
    let output = Command::new("git")
        .args(["log", "--oneline", &format!("-{n}"), "--no-color"])
        .current_dir(cwd)
        .output();

    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(String::from)
            .collect(),
        _ => Vec::new(),
    }
}

/// Get the default/main branch name.
pub fn git_default_branch(cwd: &Path) -> Option<String> {
    // Try common names
    for branch in &["main", "master"] {
        let output = Command::new("git")
            .args(["rev-parse", "--verify", branch])
            .current_dir(cwd)
            .output();
        if let Ok(o) = output
            && o.status.success()
        {
            return Some(branch.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_dir() -> PathBuf {
        // Use the current project root which is a git repo
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn git_branch_in_repo() {
        let branch = git_branch(&repo_dir());
        assert!(branch.is_some());
        assert!(!branch.unwrap().is_empty());
    }

    #[test]
    fn git_root_in_repo() {
        let root = git_root(&repo_dir());
        assert!(root.is_some());
    }

    #[test]
    fn git_is_dirty_runs() {
        // Just test it doesn't panic
        let _ = git_is_dirty(&repo_dir());
    }

    #[test]
    fn git_status_summary_runs() {
        let summary = git_status_summary(&repo_dir());
        assert!(!summary.is_empty());
    }

    #[test]
    fn git_recent_commits_returns_list() {
        let commits = git_recent_commits(&repo_dir(), 5);
        assert!(!commits.is_empty());
    }

    #[test]
    fn git_branch_in_non_repo() {
        let dir = std::env::temp_dir();
        // May or may not be a git repo
        let _ = git_branch(&dir);
    }
}
