//! Bash command execution with streaming output.

use crate::core::error::CodingAgentError;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

/// Default maximum output bytes before truncation.
const DEFAULT_MAX_BYTES: usize = 64 * 1024;

/// Result of a bash command execution.
#[derive(Debug, Clone)]
pub struct BashResult {
    /// Combined stdout+stderr output (possibly truncated).
    pub output: String,
    /// Process exit code (`None` if killed by signal).
    pub exit_code: Option<i32>,
    /// Whether the output was truncated.
    pub truncated: bool,
}

/// Callback type for receiving output chunks.
pub type OnChunkFn = Box<dyn Fn(&str) + Send + Sync>;

/// Options for bash execution.
pub struct BashExecutorOptions {
    /// Called with each chunk of output as it arrives.
    pub on_chunk: Option<OnChunkFn>,
    /// Timeout in seconds (0 = no timeout).
    pub timeout_secs: u64,
    /// Maximum output bytes.
    pub max_bytes: usize,
}

impl Default for BashExecutorOptions {
    fn default() -> Self {
        Self {
            on_chunk: None,
            timeout_secs: 120,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// Execute a bash command in the given working directory.
pub async fn execute_bash(
    command: &str,
    cwd: &Path,
    shell_path: &str,
    options: BashExecutorOptions,
) -> Result<BashResult, CodingAgentError> {
    // `kill_on_drop(true)` ensures the child is reaped if this future
    // is dropped — e.g. when an outer `tokio::select!` races us against
    // a cancellation token. Without it, an `abort_bash` would return
    // success on the wire while the destructive command kept running
    // to natural completion (the timeout also lives in the dropped
    // future, so it's bypassed too).
    let child = Command::new(shell_path)
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| CodingAgentError::Tool(format!("Failed to spawn bash: {}", e)))?;

    let timeout_duration = if options.timeout_secs > 0 {
        Some(tokio::time::Duration::from_secs(options.timeout_secs))
    } else {
        None
    };

    let wait_future = child.wait_with_output();

    let child_output = if let Some(timeout) = timeout_duration {
        match tokio::time::timeout(timeout, wait_future).await {
            Ok(result) => result
                .map_err(|e| CodingAgentError::Tool(format!("Failed to wait for bash: {}", e)))?,
            Err(_) => {
                return Ok(BashResult {
                    output: format!("[Timed out after {}s]", options.timeout_secs),
                    exit_code: None,
                    truncated: true,
                });
            }
        }
    } else {
        wait_future
            .await
            .map_err(|e| CodingAgentError::Tool(format!("Failed to wait for bash: {}", e)))?
    };

    let mut output = String::from_utf8_lossy(&child_output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&child_output.stderr).to_string();
    if !stderr.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&stderr);
    }

    // Call chunk callback with combined output
    if let Some(ref cb) = options.on_chunk {
        cb(&output);
    }

    let mut truncated = false;
    if output.len() > options.max_bytes {
        // String::truncate panics if the cut point lands inside a multi-byte
        // UTF-8 sequence (CJK, emoji, accented Latin). Step back to the
        // nearest char boundary so a 64 KiB cap is safe for any payload.
        let mut cut = options.max_bytes;
        while cut > 0 && !output.is_char_boundary(cut) {
            cut -= 1;
        }
        output.truncate(cut);
        truncated = true;
    }

    Ok(BashResult {
        output,
        exit_code: child_output.status.code(),
        truncated,
    })
}

/// Sanitize output by stripping problematic characters.
pub fn sanitize_output(output: &str) -> String {
    // Strip ANSI escape sequences
    let ansi_re = regex_lite::Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap();
    let cleaned = ansi_re.replace_all(output, "");
    // Replace null bytes
    cleaned.replace('\0', "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_execute_simple_command() {
        let dir = TempDir::new().unwrap();
        let result = execute_bash(
            "echo hello",
            dir.path(),
            "/bin/bash",
            BashExecutorOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(result.output.trim(), "hello");
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.truncated);
    }

    #[tokio::test]
    async fn test_execute_failing_command() {
        let dir = TempDir::new().unwrap();
        let result = execute_bash(
            "exit 42",
            dir.path(),
            "/bin/bash",
            BashExecutorOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(result.exit_code, Some(42));
    }

    #[tokio::test]
    async fn test_execute_with_cwd() {
        let dir = TempDir::new().unwrap();
        // Create a marker file to verify we're in the right directory
        std::fs::write(dir.path().join("marker.txt"), "here").unwrap();
        let result = execute_bash(
            "cat marker.txt",
            dir.path(),
            "/bin/bash",
            BashExecutorOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(result.output.trim(), "here");
        assert_eq!(result.exit_code, Some(0));
    }

    #[tokio::test]
    async fn test_execute_with_timeout() {
        let dir = TempDir::new().unwrap();
        let result = execute_bash(
            "sleep 10",
            dir.path(),
            "/bin/bash",
            BashExecutorOptions {
                timeout_secs: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(result.truncated);
        assert!(result.output.contains("Timed out"));
    }

    #[tokio::test]
    async fn test_execute_multiline_output() {
        let dir = TempDir::new().unwrap();
        let result = execute_bash(
            "echo line1; echo line2; echo line3",
            dir.path(),
            "/bin/bash",
            BashExecutorOptions::default(),
        )
        .await
        .unwrap();
        assert!(result.output.contains("line1"));
        assert!(result.output.contains("line2"));
        assert!(result.output.contains("line3"));
    }

    #[test]
    fn test_sanitize_output() {
        assert_eq!(sanitize_output("hello\x1b[31m world\x1b[0m"), "hello world");
        assert_eq!(sanitize_output("foo\0bar"), "foobar");
    }

    /// Regression: a UTF-8 multi-byte sequence straddling the truncate boundary
    /// must not panic — `String::truncate` requires a char boundary.
    #[tokio::test]
    async fn test_truncate_respects_utf8_boundary() {
        let dir = TempDir::new().unwrap();
        // 4-byte emoji repeated; 16 KiB / 4 = 4096 emojis. Pick max_bytes that
        // does NOT divide evenly into 4 to land mid-codepoint.
        let result = execute_bash(
            "printf '\\xf0\\x9f\\x98\\x80%.0s' $(seq 1 1000)",
            dir.path(),
            "/bin/bash",
            BashExecutorOptions {
                max_bytes: 1023, // mid-codepoint cut: 1023 = 4*255 + 3
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(result.truncated);
        // Output must be valid UTF-8 (it already is — String guarantees that —
        // but confirm we cut on a boundary, not panicked en route).
        assert!(result.output.is_char_boundary(result.output.len()));
        assert!(result.output.len() <= 1023);
    }
}
