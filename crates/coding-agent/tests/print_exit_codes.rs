//! Regression coverage for issue #55: `hand --print` must exit non-zero
//! whenever it emits an `Error: …` line. Scripts that gate on `$?` need
//! a reliable signal that the run failed.
//!
//! Each test invokes the real `hand` binary against an error-producing
//! flag combination, captures stderr/stdout, and asserts both the
//! Error: prefix on stderr AND a non-zero status code. The binary path
//! comes from `env!("CARGO_BIN_EXE_hand")` which cargo populates
//! automatically when running integration tests.

use std::path::PathBuf;
use std::process::Command;

fn hand_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hand"))
}

/// Run the binary with `args`, no stdin, return (exit-code, stderr-text).
/// `cwd_override = None` keeps the current directory; `Some(dir)` runs
/// the child with that working directory.
fn run(args: &[&str]) -> (i32, String) {
    // Detach the child from any inherited terminal state so we exercise
    // the auto-print path the same way scripts would. `HAND_HOME` is
    // redirected to a tmpdir-style location through std::env::temp_dir()
    // — most error paths fire before any session file is written, but
    // anchor anyway so we don't pollute the user's real ~/.hand.
    let out = Command::new(hand_bin())
        .args(args)
        .env(
            "HAND_HOME",
            std::env::temp_dir().join("hand-print-exit-test"),
        )
        // Empty stdin so auto-print logic and prompt-reading paths
        // don't block waiting for input.
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run hand binary");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn print_with_bogus_cwd_exits_nonzero() {
    let (code, stderr) = run(&["--cwd", "/nonexistent/zzz-hand", "--print", "hi"]);
    assert_ne!(code, 0, "expected non-zero exit; stderr was: {stderr:?}");
    assert!(
        stderr.contains("Error:") && stderr.contains("--cwd"),
        "expected --cwd error on stderr, got: {stderr:?}"
    );
}

#[test]
fn print_with_cwd_pointing_at_a_file_exits_nonzero() {
    // /etc/hosts is a regular file on every Unix-y CI runner.
    let (code, stderr) = run(&["--cwd", "/etc/hosts", "--print", "hi"]);
    assert_ne!(code, 0, "expected non-zero exit; stderr was: {stderr:?}");
    assert!(
        stderr.contains("Error:") && stderr.contains("--cwd"),
        "expected --cwd error on stderr, got: {stderr:?}"
    );
}

#[test]
fn print_with_missing_at_file_exits_nonzero() {
    let (code, stderr) = run(&["--print", "--no-tools", "@/nonexistent/zzz-hand-file.md"]);
    assert_ne!(code, 0, "expected non-zero exit; stderr was: {stderr:?}");
    assert!(
        stderr.contains("Error:") && stderr.contains("File not found"),
        "expected @file error on stderr, got: {stderr:?}"
    );
}

/// Issue #62: `--print '/help'` (or any bare slash command) must not
/// be shipped to the LLM as plain text — the model hallucinates
/// plausible-looking command output. Print mode now refuses with a
/// non-zero exit and an explanatory stderr that mentions the
/// command the user typed.
#[test]
fn print_with_bare_slash_command_exits_nonzero() {
    let (code, stderr) = run(&["--print", "--no-tools", "/help"]);
    assert_ne!(code, 0, "expected non-zero exit; stderr was: {stderr:?}");
    assert!(
        stderr.contains("/help") && stderr.contains("slash command"),
        "expected slash-command refusal on stderr, got: {stderr:?}"
    );
}

/// Issue #64: `--theme`, `--extension`, `--prompt-template` are
/// parsed but the runtime doesn't yet wire them. Emit an honest
/// stderr warning per flag so the user isn't silently misled.
/// Detection happens at startup before any I/O, so the test can
/// run against the diagnostics path (which exits early) and still
/// see the warning.
#[test]
fn unplumbed_theme_flag_emits_warning() {
    let (_code, stderr) = run(&["--theme", "/tmp/zzz-theme.yaml", "--diagnostics"]);
    assert!(
        stderr.contains("warning") && stderr.contains("--theme"),
        "expected --theme warning on stderr, got: {stderr:?}"
    );
}

#[test]
fn unplumbed_extension_flag_emits_warning() {
    let (_code, stderr) = run(&["--extension", "/tmp/zzz-ext", "--diagnostics"]);
    assert!(
        stderr.contains("warning") && stderr.contains("--extension"),
        "expected --extension warning on stderr, got: {stderr:?}"
    );
}
