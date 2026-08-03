//! Ctrl+G external-editor roundtrip for the rt interactive driver (VAL-EDITOR-020).
//!
//! Pressing Ctrl+G opens the current chat-input buffer in the user's external
//! editor. The editor is resolved from the environment — **`$VISUAL` takes
//! precedence over `$EDITOR`** — the buffer is written to a temp file, the editor
//! is spawned on the inherited TTY (the driver yields the terminal via the M1
//! [`SessionGuard`](hand_tui::rt::session::SessionGuard) suspend/resume seam), and
//! on a clean exit the buffer is replaced with the file's contents.
//!
//! The outcome is a two-branch decision (VAL-EDITOR-020):
//!
//! - **save-and-exit (exit 0):** the buffer is replaced with the edited file
//!   contents and the TUI repaints cleanly;
//! - **non-zero exit / spawn failure:** the buffer is left unchanged and a red
//!   error status line lands in chat.
//!
//! The terminal handoff itself (leave raw / restore, spawn, re-enter raw / redraw)
//! lives in the input loop; this module owns the *pure* pieces — editor
//! resolution and the file roundtrip decision — so the control flow is
//! unit-tested without a real terminal by injecting a scripted `$VISUAL`.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

/// The environment variable checked first for the external editor command.
const VISUAL_ENV: &str = "VISUAL";
/// The environment variable checked second, when [`VISUAL_ENV`] is unset/empty.
const EDITOR_ENV: &str = "EDITOR";

/// The outcome of an external-editor roundtrip, ready for the input loop to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorOutcome {
    /// The editor saved and exited cleanly: replace the buffer with this text.
    Replaced(String),
    /// The editor exited non-zero, failed to spawn, or the roundtrip could not
    /// complete: leave the buffer unchanged and land this error line in chat.
    Failed(String),
}

/// Resolve the external editor command, honouring `$VISUAL` over `$EDITOR`.
///
/// Returns the raw command string (which may include arguments, e.g.
/// `code --wait`) or `None` when neither variable is set to a non-blank value —
/// the caller then lands the "no editor configured" guidance rather than spawning
/// nothing.
#[must_use]
pub fn resolve_editor_command() -> Option<String> {
    resolve_editor_command_from(std::env::var_os(VISUAL_ENV), std::env::var_os(EDITOR_ENV))
}

/// The pure resolver: pick `$VISUAL` when it is a non-blank value, else `$EDITOR`
/// when it is, else `None`. Kept separate so the precedence (VISUAL wins) is
/// unit-tested without mutating the process environment.
#[must_use]
pub fn resolve_editor_command_from(
    visual: Option<OsString>,
    editor: Option<OsString>,
) -> Option<String> {
    non_blank(visual).or_else(|| non_blank(editor))
}

/// A trimmed, non-empty string from an optional OS value, or `None`.
fn non_blank(value: Option<OsString>) -> Option<String> {
    let s = value?.to_string_lossy().into_owned();
    let trimmed = s.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Split a resolved editor command into its program and trailing arguments.
///
/// A command may carry flags (`code --wait`, `emacsclient -nw`); the first
/// whitespace-delimited token is the program and the rest are passed through.
/// Returns `None` when the command is blank.
#[must_use]
pub fn split_editor_command(command: &str) -> Option<(String, Vec<String>)> {
    let mut parts = command.split_whitespace();
    let program = parts.next()?.to_string();
    let args: Vec<String> = parts.map(str::to_string).collect();
    Some((program, args))
}

/// Run the external-editor roundtrip synchronously against a scratch file.
///
/// Writes `buffer` to a temp file, spawns `program args… <path>` with the parent
/// TTY inherited (so the editor draws to the real terminal), waits for it to
/// exit, and maps the result to an [`EditorOutcome`]:
///
/// - exit 0 → [`EditorOutcome::Replaced`] with the file's contents (trailing
///   newline trimmed once, so a `printf 'x\n'` editor stub doesn't append a blank
///   line);
/// - non-zero exit / spawn failure / read failure → [`EditorOutcome::Failed`]
///   with a descriptive error — the buffer is left unchanged.
///
/// This blocks the calling thread while the editor runs, so the input loop drives
/// it through `spawn_blocking`. It touches the terminal only through the spawned
/// child (inherited stdio); the raw-mode handoff is the caller's job.
#[must_use]
pub fn run_editor_roundtrip(program: &str, args: &[String], buffer: &str) -> EditorOutcome {
    let file = match tempfile::Builder::new()
        .prefix("hand-edit-")
        .suffix(".md")
        .tempfile()
    {
        Ok(f) => f,
        Err(e) => return EditorOutcome::Failed(format!("[editor: temp file failed: {e}]")),
    };
    if let Err(e) = std::fs::write(file.path(), buffer) {
        return EditorOutcome::Failed(format!("[editor: temp write failed: {e}]"));
    }

    match spawn_editor_status(program, args, file.path()) {
        Ok(true) => match std::fs::read_to_string(file.path()) {
            Ok(contents) => EditorOutcome::Replaced(strip_one_trailing_newline(&contents)),
            Err(e) => EditorOutcome::Failed(format!("[editor: read-back failed: {e}]")),
        },
        Ok(false) => EditorOutcome::Failed("[editor exited without saving]".to_string()),
        Err(e) => EditorOutcome::Failed(format!("[editor failed to launch: {e}]")),
    }
}

/// Spawn `program args… path` on the inherited TTY and report whether it exited
/// with a success status (exit 0). A spawn failure surfaces as an `Err`; a
/// non-zero exit or a signal termination is `Ok(false)`.
fn spawn_editor_status(program: &str, args: &[String], path: &Path) -> std::io::Result<bool> {
    let status = Command::new(program)
        .args(args)
        .arg(path)
        // Inherit the parent's stdio so the editor draws to the real terminal.
        // The driver has already yielded raw mode via the SessionGuard suspend
        // seam, so the child gets a clean cooked TTY.
        .status()?;
    Ok(status.success())
}

/// Strip exactly one trailing `\n` (and its preceding `\r`, if any) so an editor
/// that saves a POSIX trailing newline does not grow the buffer by a blank line
/// on every roundtrip.
fn strip_one_trailing_newline(text: &str) -> String {
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- $VISUAL precedence over $EDITOR (VAL-EDITOR-020) -----------------

    #[test]
    fn visual_takes_precedence_over_editor() {
        let picked =
            resolve_editor_command_from(Some(OsString::from("vim")), Some(OsString::from("nano")));
        assert_eq!(
            picked.as_deref(),
            Some("vim"),
            "$VISUAL must win over $EDITOR"
        );
    }

    #[test]
    fn falls_back_to_editor_when_visual_unset_or_blank() {
        // Unset $VISUAL falls back to $EDITOR.
        assert_eq!(
            resolve_editor_command_from(None, Some(OsString::from("nano"))).as_deref(),
            Some("nano"),
        );
        // A blank $VISUAL (whitespace only) is treated as unset.
        assert_eq!(
            resolve_editor_command_from(Some(OsString::from("   ")), Some(OsString::from("nano")))
                .as_deref(),
            Some("nano"),
        );
    }

    #[test]
    fn none_when_neither_is_set() {
        assert_eq!(resolve_editor_command_from(None, None), None);
        assert_eq!(
            resolve_editor_command_from(Some(OsString::from("")), Some(OsString::from(" "))),
            None,
            "two blank values resolve to no editor",
        );
    }

    #[test]
    fn split_separates_program_and_args() {
        assert_eq!(
            split_editor_command("code --wait"),
            Some(("code".to_string(), vec!["--wait".to_string()])),
        );
        assert_eq!(
            split_editor_command("vim"),
            Some(("vim".to_string(), Vec::new())),
        );
        assert_eq!(split_editor_command("   "), None);
    }

    // --- roundtrip success branch: buffer replaced (VAL-EDITOR-020) -------

    /// A scripted `$VISUAL` that overwrites the file (`sh -c 'echo replaced > "$1"' _`)
    /// and exits 0 replaces the buffer with the file's contents — the success
    /// branch of the Ctrl+G roundtrip, verified without a real terminal.
    #[cfg(unix)]
    #[test]
    fn save_and_exit_replaces_the_buffer() {
        // `sh -c 'echo replaced > "$1"' _ <tempfile>`: `$1` is the temp path the
        // roundtrip appends. The stub writes "replaced\n" and exits 0.
        let outcome = run_editor_roundtrip(
            "sh",
            &[
                "-c".to_string(),
                "echo replaced > \"$1\"".to_string(),
                "_".to_string(),
            ],
            "original draft",
        );
        assert_eq!(
            outcome,
            EditorOutcome::Replaced("replaced".to_string()),
            "a save-and-exit editor replaces the buffer with the file contents",
        );
    }

    /// The starting buffer is what the editor sees: a stub that appends " EDITED"
    /// to the file it was handed proves the original buffer was written out first.
    #[cfg(unix)]
    #[test]
    fn roundtrip_seeds_the_editor_with_the_current_buffer() {
        let outcome = run_editor_roundtrip(
            "sh",
            &[
                "-c".to_string(),
                "printf ' EDITED' >> \"$1\"".to_string(),
                "_".to_string(),
            ],
            "seed text",
        );
        assert_eq!(
            outcome,
            EditorOutcome::Replaced("seed text EDITED".to_string()),
            "the editor is seeded with the current buffer, then its edits are read back",
        );
    }

    // --- roundtrip failure branch: buffer unchanged + error (VAL-EDITOR-020) --

    /// A non-zero exit leaves the buffer unchanged and surfaces an error line —
    /// the failure branch of the roundtrip.
    #[cfg(unix)]
    #[test]
    fn non_zero_exit_reports_failure_and_preserves_the_buffer() {
        let outcome = run_editor_roundtrip(
            "sh",
            &[
                "-c".to_string(),
                // Overwrite the file but exit non-zero: the write must be ignored
                // because the editor did not exit cleanly (exit != 0).
                "echo clobbered > \"$1\"; exit 3".to_string(),
                "_".to_string(),
            ],
            "keep me",
        );
        assert!(
            matches!(outcome, EditorOutcome::Failed(_)),
            "a non-zero exit is a failure, got {outcome:?}",
        );
    }

    /// A missing editor binary is a launch failure, not a silent no-op.
    #[test]
    fn missing_editor_binary_reports_launch_failure() {
        let outcome = run_editor_roundtrip("definitely-not-a-real-editor-xyzzy", &[], "buffer");
        assert!(
            matches!(outcome, EditorOutcome::Failed(msg) if msg.contains("launch")),
            "a missing editor binary must report a launch failure",
        );
    }

    #[test]
    fn strips_one_trailing_newline_only() {
        assert_eq!(strip_one_trailing_newline("a\n"), "a");
        assert_eq!(strip_one_trailing_newline("a\r\n"), "a");
        assert_eq!(
            strip_one_trailing_newline("a\n\n"),
            "a\n",
            "only one newline stripped"
        );
        assert_eq!(strip_one_trailing_newline("a"), "a", "no newline to strip");
    }
}
