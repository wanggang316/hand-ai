//! Git utilities — repository info for system prompt and diagnostics.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the git metadata for a directory lives.
///
/// Both fields are canonicalized, so they compare cleanly against each
/// other and against canonicalized paths from elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPaths {
    /// The directory holding the `.git` entry — the worktree root.
    pub repo_dir: PathBuf,
    /// The repository's common git directory. For an ordinary repository
    /// this is `<repo_dir>/.git`; for a linked worktree it is the *main*
    /// repository's git directory, not the per-worktree one.
    pub common_git_dir: PathBuf,
}

/// Resolve the git layout around `cwd` by walking up until a `.git`
/// entry turns up.
///
/// Reads the layout off the filesystem rather than shelling out, because
/// callers run this during session startup where a subprocess per lookup
/// is not worth it, and because it keeps working when `git` is not on
/// PATH. Handles both shapes: `.git` as a directory (ordinary clone) and
/// `.git` as a file naming the per-worktree git directory (a linked
/// worktree from `git worktree add`).
///
/// Returns `None` when there is no repository above `cwd`, and also when
/// a `.git` entry turns up but doesn't resolve to a usable git directory
/// — a caller can't do anything useful with a half-readable layout.
pub fn find_git_paths(cwd: &Path) -> Option<GitPaths> {
    let mut dir = cwd;
    loop {
        let git_path = dir.join(".git");
        match std::fs::metadata(&git_path) {
            Ok(meta) if meta.is_dir() => {
                if !git_path.join("HEAD").exists() {
                    return None;
                }
                return Some(GitPaths {
                    repo_dir: dir.canonicalize().ok()?,
                    common_git_dir: git_path.canonicalize().ok()?,
                });
            }
            Ok(meta) if meta.is_file() => {
                let content = std::fs::read_to_string(&git_path).ok()?;
                let target = content.trim().strip_prefix("gitdir:")?.trim();
                let git_dir = resolve_against(dir, Path::new(target));
                if !git_dir.join("HEAD").exists() {
                    return None;
                }
                // `commondir` points at the main repository's git dir and
                // is what makes a linked worktree distinguishable from an
                // ordinary clone. A git dir without one (a submodule's,
                // under `.git/modules`) is its own common dir.
                let common_git_dir = match std::fs::read_to_string(git_dir.join("commondir")) {
                    Ok(rel) => resolve_against(&git_dir, Path::new(rel.trim())),
                    Err(_) => git_dir,
                };
                return Some(GitPaths {
                    repo_dir: dir.canonicalize().ok()?,
                    common_git_dir: common_git_dir.canonicalize().ok()?,
                });
            }
            _ => {}
        }
        dir = dir.parent()?;
    }
}

/// Join a git-metadata path against the file that named it, unless it is
/// already absolute. `.git` files and `commondir` may hold either form.
fn resolve_against(base: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        base.join(target)
    }
}

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

    /// In an ordinary clone the common git dir is the repository's own
    /// `.git`, and the walk finds it from a nested subdirectory.
    #[test]
    fn find_git_paths_resolves_an_ordinary_repository() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let nested = repo.join("crates").join("api");
        std::fs::create_dir_all(&nested).unwrap();

        let paths = find_git_paths(&nested).expect("repository found");
        assert_eq!(paths.repo_dir, repo.canonicalize().unwrap());
        assert_eq!(
            paths.common_git_dir,
            repo.join(".git").canonicalize().unwrap()
        );
    }

    /// A linked worktree's `.git` is a file naming a per-worktree git
    /// dir, whose `commondir` points back at the main repository. The
    /// common git dir must resolve to the latter, not the former —
    /// that difference is what identifies a worktree at all.
    #[test]
    fn find_git_paths_resolves_a_linked_worktree_to_the_main_git_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let main = dir.path().join("main");
        let worktree_git_dir = main.join(".git").join("worktrees").join("feat");
        std::fs::create_dir_all(&worktree_git_dir).unwrap();
        std::fs::write(main.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(worktree_git_dir.join("HEAD"), "ref: refs/heads/feat\n").unwrap();
        std::fs::write(worktree_git_dir.join("commondir"), "../..\n").unwrap();
        let worktree = dir.path().join("feat");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", worktree_git_dir.display()),
        )
        .unwrap();

        let paths = find_git_paths(&worktree).expect("worktree found");
        assert_eq!(paths.repo_dir, worktree.canonicalize().unwrap());
        assert_eq!(
            paths.common_git_dir,
            main.join(".git").canonicalize().unwrap()
        );
    }

    /// A git dir with no `commondir` — a submodule's, under
    /// `.git/modules` — is its own common dir.
    #[test]
    fn find_git_paths_without_commondir_uses_the_git_dir_itself() {
        let dir = tempfile::TempDir::new().unwrap();
        let git_dir = dir.path().join("modules").join("sub");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".git"), format!("gitdir: {}\n", git_dir.display())).unwrap();

        let paths = find_git_paths(&sub).expect("submodule found");
        assert_eq!(paths.common_git_dir, git_dir.canonicalize().unwrap());
    }

    /// No repository above the directory, and a `.git` that doesn't
    /// resolve to a usable git dir, both mean "nothing to report".
    #[test]
    fn find_git_paths_returns_none_without_a_usable_repository() {
        let dir = tempfile::TempDir::new().unwrap();
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(find_git_paths(&plain), None);

        let headless = dir.path().join("headless");
        std::fs::create_dir_all(headless.join(".git")).unwrap();
        assert_eq!(find_git_paths(&headless), None);
    }
}
