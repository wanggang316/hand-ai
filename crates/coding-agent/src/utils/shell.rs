//! Cross-platform shell helpers.
//!
//! Shell discovery lives in [`crate::core::bash_executor::resolve_shell`];
//! this module covers the platform-specific escape rules used to assemble
//! safe command lines, plus a `which`-style executable lookup.
//!
//! ## Escaping
//!
//! - On Unix the POSIX rule is: wrap the value in single quotes and replace
//!   any embedded single quote with `'\''`.
//! - On Windows we follow `cmd.exe` parsing rules: wrap in double quotes,
//!   escape internal `"` as `\"`, and double any preceding backslashes per
//!   `CommandLineToArgvW` semantics.
//!
//! Both `shell_escape_unix` and `shell_escape_windows` are pure functions
//! and exported for use even when running on the other OS (e.g. building a
//! wsl/`bash -c` payload from a Windows host).

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Errors returned by helpers in this module.
#[derive(Debug, Error)]
pub enum ShellError {
    /// The requested executable could not be located on `PATH`.
    #[error("executable not found on PATH: {0}")]
    NotFound(String),
}

/// Quote `arg` for a POSIX shell.
///
/// Empty input yields `''` (a literal empty argument, not the empty string,
/// which would disappear during parsing). Otherwise the value is wrapped in
/// single quotes; embedded `'` become `'\''`.
pub fn shell_escape_unix(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    // Fast path: if every character is in the "safe" set we can return it
    // verbatim. This matches what most POSIX shells accept unquoted.
    if arg.bytes().all(|b| {
        b.is_ascii_alphanumeric()
            || matches!(b, b'_' | b'-' | b'/' | b'.' | b':' | b'@' | b'+' | b',')
    }) {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('\'');
    for ch in arg.chars() {
        if ch == '\'' {
            // Close the quote, emit an escaped literal `'`, reopen.
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Quote `arg` for a Windows command line (CRT / `CommandLineToArgvW` rules).
///
/// Backslashes only need doubling when they precede a `"`; trailing
/// backslashes before the closing quote also need doubling. See
/// <https://learn.microsoft.com/en-us/cpp/c-runtime-library/parsing-cpp-command-line-arguments>.
pub fn shell_escape_windows(arg: &str) -> String {
    if !arg.is_empty()
        && !arg
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\\')
    {
        return arg.to_string();
    }

    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let chars: Vec<char> = arg.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let mut backslashes = 0;
        while i < chars.len() && chars[i] == '\\' {
            backslashes += 1;
            i += 1;
        }
        if i == chars.len() {
            // Trailing backslashes: double them.
            for _ in 0..(backslashes * 2) {
                out.push('\\');
            }
        } else if chars[i] == '"' {
            // `\\` * 2n + `\\"` to escape the quote.
            for _ in 0..(backslashes * 2 + 1) {
                out.push('\\');
            }
            out.push('"');
            i += 1;
        } else {
            for _ in 0..backslashes {
                out.push('\\');
            }
            out.push(chars[i]);
            i += 1;
        }
    }
    out.push('"');
    out
}

/// Quote `arg` for the host operating system's shell.
pub fn shell_escape(arg: &str) -> String {
    #[cfg(windows)]
    {
        shell_escape_windows(arg)
    }
    #[cfg(not(windows))]
    {
        shell_escape_unix(arg)
    }
}

/// Locate `executable` on `PATH`, returning the first match.
///
/// On Windows this also probes the variants in `PATHEXT` (defaulting to a
/// reasonable set when the env var is missing) so callers can pass `git`
/// and have it match `git.exe` / `git.cmd`.
pub fn which(executable: &str) -> Result<PathBuf, ShellError> {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let extensions: Vec<String> = if cfg!(windows) {
        let pathext =
            std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        pathext.split(';').map(|s| s.to_string()).collect()
    } else {
        vec![String::new()]
    };

    for dir in std::env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for ext in &extensions {
            let candidate: PathBuf = if ext.is_empty() {
                dir.join(executable)
            } else {
                dir.join(format!("{executable}{ext}"))
            };
            if is_executable(&candidate) {
                return Ok(candidate);
            }
        }
    }

    Err(ShellError::NotFound(executable.to_string()))
}

/// `true` when `path` exists and (on Unix) has any execute bit set.
fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_unix_passes_safe_chars_through() {
        assert_eq!(shell_escape_unix("foo"), "foo");
        assert_eq!(shell_escape_unix("foo-bar.txt"), "foo-bar.txt");
        assert_eq!(shell_escape_unix("/usr/local/bin"), "/usr/local/bin");
    }

    #[test]
    fn escape_unix_quotes_special_chars() {
        assert_eq!(shell_escape_unix("hello world"), "'hello world'");
        assert_eq!(shell_escape_unix("a$b"), "'a$b'");
        assert_eq!(shell_escape_unix("a;b"), "'a;b'");
    }

    #[test]
    fn escape_unix_handles_single_quotes() {
        assert_eq!(shell_escape_unix("it's"), "'it'\\''s'");
        assert_eq!(shell_escape_unix("'"), "''\\'''");
    }

    #[test]
    fn escape_unix_empty_string() {
        assert_eq!(shell_escape_unix(""), "''");
    }

    #[test]
    fn escape_windows_passes_safe_chars_through() {
        assert_eq!(shell_escape_windows("foo"), "foo");
        assert_eq!(shell_escape_windows("foo-bar.txt"), "foo-bar.txt");
    }

    #[test]
    fn escape_windows_quotes_whitespace() {
        assert_eq!(shell_escape_windows("hello world"), "\"hello world\"");
    }

    #[test]
    fn escape_windows_escapes_quotes() {
        assert_eq!(shell_escape_windows("a\"b"), "\"a\\\"b\"");
    }

    #[test]
    fn escape_windows_doubles_trailing_backslashes() {
        // `foo\` should become `"foo\\"` because the trailing backslash
        // would otherwise escape the closing quote.
        assert_eq!(shell_escape_windows("foo\\"), "\"foo\\\\\"");
    }

    #[test]
    fn escape_windows_doubles_backslashes_before_quote() {
        // `a\"b` -> `"a\\\"b"`. We supply `a\"b` which is `a`, `\`, `"`, `b`.
        assert_eq!(shell_escape_windows("a\\\"b"), "\"a\\\\\\\"b\"");
    }

    #[test]
    fn escape_windows_empty_string() {
        // Empty string follows the same rule as unix-style: must be quoted to
        // avoid disappearing during argv parsing.
        assert_eq!(shell_escape_windows(""), "\"\"");
    }

    #[test]
    fn which_finds_real_executable() {
        // `sh` is virtually guaranteed on Unix CI; on Windows we look for
        // `cmd`. This is the happy path.
        #[cfg(unix)]
        let bin = "sh";
        #[cfg(windows)]
        let bin = "cmd";
        let path = which(bin).expect("expected to find a shell on PATH");
        assert!(path.exists(), "which returned a path that does not exist");
    }

    #[test]
    fn which_returns_not_found_for_missing_binary() {
        let err = which("definitely-not-a-real-binary-xyzzy").unwrap_err();
        match err {
            ShellError::NotFound(name) => {
                assert_eq!(name, "definitely-not-a-real-binary-xyzzy");
            }
        }
    }
}
