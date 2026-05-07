//! Spawn helpers around `tokio::process::Command`.
//!
//! Mirrors `pi-coding-agent`'s `child-process.ts`, but rebuilt in idiomatic
//! Rust:
//!
//! - The TS file's `waitForChildProcess` exists to paper over Node's
//!   `exit`-vs-`close` divergence on Windows. `tokio::process::Command` with
//!   `kill_on_drop(true)` and `Child::wait_with_output()` already gives us
//!   that contract — when the future is dropped the child is reaped, and
//!   `wait_with_output()` settles once the pipes close. So we don't need
//!   the manual stdio-grace dance, only the spawn-and-collect helper.
//! - `shouldUseWindowsShell` (the small heuristic for `.cmd`/`.bat` files)
//!   is preserved because `tokio::process::Command` does not auto-launch
//!   shell wrappers on Windows the way Node's `child_process.spawn({ shell:
//!   true })` does.
//! - The TS `killProcessTree` helper has its own home in
//!   [`crate::utils::shell`]'s sibling area; here we offer
//!   [`spawn_with_output`] (one-shot collection) and a Unix-only
//!   [`kill_process_group`] used by callers that explicitly opted into a
//!   detached process group via [`spawn_in_process_group`].
//!
//! On Windows tree-kill is documented as a follow-up; today we rely on
//! `kill_on_drop(true)` and the inherited handle behavior.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use thiserror::Error;
use tokio::process::{Child, Command};

/// Errors raised by helpers in this module.
#[derive(Debug, Error)]
pub enum ChildProcessError {
    /// Spawning the child failed (binary not found, permission denied, …).
    #[error("failed to spawn {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    /// Waiting on the child failed before it exited cleanly.
    #[error("failed to wait on {program}: {source}")]
    Wait {
        program: String,
        #[source]
        source: std::io::Error,
    },
}

/// Output of a one-shot child-process invocation.
#[derive(Debug, Clone)]
pub struct ProcessOutput {
    /// Captured stdout as raw bytes.
    pub stdout: Vec<u8>,
    /// Captured stderr as raw bytes.
    pub stderr: Vec<u8>,
    /// Exit code; `None` when the child was terminated by a signal.
    pub exit_code: Option<i32>,
}

impl ProcessOutput {
    /// `true` when the process exited with status 0.
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// stdout as UTF-8 (lossy for non-UTF-8 bytes).
    pub fn stdout_string(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// stderr as UTF-8 (lossy for non-UTF-8 bytes).
    pub fn stderr_string(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// Names of executables that on Windows always need to be launched through
/// `cmd.exe` because they ship as `.cmd` shims.
const WINDOWS_SHELL_COMMANDS: &[&str] = &["npm", "npx", "pnpm", "yarn", "yarnpkg", "corepack"];

/// `true` when `command` should be launched via the Windows shell rather
/// than directly. Always `false` on non-Windows platforms.
pub fn should_use_windows_shell(command: &str) -> bool {
    if !cfg!(windows) {
        return false;
    }
    let name = Path::new(command)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(command)
        .to_ascii_lowercase();
    if name.ends_with(".cmd") || name.ends_with(".bat") {
        return true;
    }
    let stem = name.trim_end_matches(".exe");
    WINDOWS_SHELL_COMMANDS.contains(&stem)
}

/// Spawn `program args…` in `cwd`, capturing stdout and stderr, then wait.
///
/// `cwd` is optional — `None` inherits the parent's working directory.
/// `kill_on_drop(true)` is set so an outer cancellation (e.g. `tokio::select!`
/// races) reaps the child rather than leaving it running.
pub async fn spawn_with_output(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
) -> Result<ProcessOutput, ChildProcessError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }

    let child = command.spawn().map_err(|e| ChildProcessError::Spawn {
        program: program.to_string(),
        source: e,
    })?;

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| ChildProcessError::Wait {
            program: program.to_string(),
            source: e,
        })?;

    Ok(ProcessOutput {
        stdout: output.stdout,
        stderr: output.stderr,
        exit_code: output.status.code(),
    })
}

/// Spawn `program args…` in its own process group on Unix.
///
/// Unlike [`spawn_with_output`], this returns the live [`Child`] so the
/// caller can wait, kill the whole group via [`kill_process_group`], or
/// otherwise drive the lifecycle manually. On Windows this is identical to
/// a regular spawn (process-tree kill is a follow-up — see module docs).
///
/// On Unix we set the child as a session leader (`setsid`) so signals to
/// the group reach all descendants — matching the TS `killProcessTree`
/// `process.kill(-pid, "SIGKILL")` behavior.
pub fn spawn_in_process_group(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
) -> Result<Child, ChildProcessError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }

    #[cfg(unix)]
    {
        // SAFETY: `setsid` is async-signal-safe and detaches the child
        // from its parent's process group. Anything we run after fork()
        // and before exec() must stay async-signal-safe — `nix::unistd`
        // wraps the libc syscall directly with no extra allocation.
        unsafe {
            command.pre_exec(|| {
                nix::unistd::setsid()
                    .map(|_| ())
                    .map_err(std::io::Error::from)
            });
        }
    }

    command.spawn().map_err(|e| ChildProcessError::Spawn {
        program: program.to_string(),
        source: e,
    })
}

/// Send `SIGKILL` to the entire process group rooted at `pid`.
///
/// Only available on Unix; on Windows this is a no-op stub. Use after
/// [`spawn_in_process_group`] so the children spawned by the child also
/// receive the signal.
#[cfg(unix)]
pub fn kill_process_group(pid: u32) -> std::io::Result<()> {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    let pgid = Pid::from_raw(pid as i32);
    killpg(pgid, Signal::SIGKILL).map_err(std::io::Error::from)
}

/// Stub for non-Unix targets — process-tree kill is a follow-up.
#[cfg(not(unix))]
pub fn kill_process_group(_pid: u32) -> std::io::Result<()> {
    Ok(())
}

/// Convenience: locate `program` on PATH and return its absolute location.
/// Wraps [`crate::utils::shell::which`] so callers don't need a second
/// import for the common discover-then-spawn flow.
pub fn locate(program: &str) -> Result<PathBuf, crate::utils::shell::ShellError> {
    crate::utils::shell::which(program)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use tempfile::TempDir;

    #[tokio::test]
    async fn spawn_captures_stdout() {
        // `echo` is universally available on Unix CI; on Windows it would
        // need `cmd /c echo`, which the parent `should_use_windows_shell`
        // helper handles in real call sites. The test is gated to Unix.
        #[cfg(unix)]
        {
            let out = spawn_with_output("echo", &["hello", "world"], None)
                .await
                .expect("spawn");
            assert!(out.success());
            assert_eq!(out.stdout_string().trim(), "hello world");
            assert!(out.stderr.is_empty());
        }
    }

    #[tokio::test]
    async fn spawn_captures_stderr_and_nonzero_exit() {
        #[cfg(unix)]
        {
            let out = spawn_with_output("sh", &["-c", "echo oops 1>&2; exit 7"], None)
                .await
                .expect("spawn");
            assert!(!out.success());
            assert_eq!(out.exit_code, Some(7));
            assert_eq!(out.stderr_string().trim(), "oops");
            assert!(out.stdout.is_empty());
        }
    }

    #[tokio::test]
    async fn spawn_respects_cwd() {
        #[cfg(unix)]
        {
            let dir = TempDir::new().expect("tmp");
            std::fs::write(dir.path().join("marker.txt"), "x").expect("write");
            let out = spawn_with_output("ls", &["marker.txt"], Some(dir.path()))
                .await
                .expect("spawn");
            assert!(out.success());
            assert_eq!(out.stdout_string().trim(), "marker.txt");
        }
    }

    #[tokio::test]
    async fn spawn_missing_binary_returns_error() {
        let err = spawn_with_output("definitely-not-a-real-binary-xyzzy", &[], None).await;
        assert!(matches!(err, Err(ChildProcessError::Spawn { .. })));
    }

    #[test]
    fn should_use_windows_shell_only_on_windows() {
        if cfg!(windows) {
            assert!(should_use_windows_shell("npm"));
            assert!(should_use_windows_shell("npm.cmd"));
            assert!(should_use_windows_shell("foo.bat"));
            assert!(!should_use_windows_shell("foo"));
        } else {
            // On Unix we always answer false — the heuristic is a no-op.
            assert!(!should_use_windows_shell("npm"));
            assert!(!should_use_windows_shell("foo.cmd"));
        }
    }

    /// Process-group kill is a Unix-only path. This test verifies the
    /// happy-path API: spawn a long-running child via
    /// `spawn_in_process_group`, send a group SIGKILL, then confirm the
    /// child reports termination by signal (i.e. exit code is `None`).
    #[cfg(unix)]
    #[tokio::test]
    async fn process_group_kill_terminates_child() {
        let mut child =
            spawn_in_process_group("sh", &["-c", "sleep 30"], None).expect("spawn group");
        let pid = child.id().expect("child has pid");
        kill_process_group(pid).expect("kill group");
        let status = child.wait().await.expect("wait");
        // Killed by signal -> no exit code.
        assert!(status.code().is_none(), "expected signal termination");
    }
}
