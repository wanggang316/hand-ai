//! Bash command execution with streaming output.

use crate::core::error::CodingAgentError;
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
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
    let mut child = Command::new(shell_path)
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

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CodingAgentError::Tool("bash child missing stdout pipe".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CodingAgentError::Tool("bash child missing stderr pipe".into()))?;

    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let stdout_tx = chunk_tx.clone();
    let stderr_tx = chunk_tx;
    let stdout_task = tokio::spawn(forward_pipe(stdout, stdout_tx));
    let stderr_task = tokio::spawn(forward_pipe(stderr, stderr_tx));

    // Drain chunks into a combined raw buffer, invoking on_chunk on each
    // arrival. The accumulator runs in this task so back-pressure is
    // bounded by tokio's mpsc buffer (unbounded — fine for terminal
    // output rates).
    let drain_future = async {
        let mut raw: Vec<u8> = Vec::new();
        while let Some(chunk) = chunk_rx.recv().await {
            raw.extend_from_slice(&chunk);
            if let Some(ref cb) = options.on_chunk {
                let snapshot = sanitize_output(&String::from_utf8_lossy(&raw)).replace('\r', "");
                cb(&snapshot);
            }
        }
        raw
    };

    let wait_future = async {
        let status = child
            .wait()
            .await
            .map_err(|e| CodingAgentError::Tool(format!("Failed to wait for bash: {}", e)))?;
        let _ = stdout_task.await;
        let _ = stderr_task.await;
        let raw = drain_future.await;
        Ok::<_, CodingAgentError>((status, raw))
    };

    let (status, raw_output) = if let Some(timeout) = timeout_duration {
        match tokio::time::timeout(timeout, wait_future).await {
            Ok(result) => result?,
            Err(_) => {
                return Ok(BashResult {
                    output: format!("[Timed out after {}s]", options.timeout_secs),
                    exit_code: None,
                    truncated: true,
                });
            }
        }
    } else {
        wait_future.await?
    };

    // Pi-mono parity: strip ANSI escapes, C0 controls, Unicode format chars,
    // then drop bare `\r` (progress-bar overwrites garble captured streams).
    let mut output = sanitize_output(&String::from_utf8_lossy(&raw_output));
    output = output.replace('\r', "");

    let mut truncated = false;
    if output.len() > options.max_bytes {
        output = truncate_tail_bytes(&output, options.max_bytes);
        truncated = true;
    }

    Ok(BashResult {
        output,
        exit_code: status.code(),
        truncated,
    })
}

/// Read from a child pipe in small chunks and forward each chunk to
/// the drain task. Ends when the pipe reports EOF.
async fn forward_pipe<R>(mut reader: R, tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>)
where
    R: AsyncReadExt + Unpin,
{
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    return;
                }
            }
        }
    }
}

/// Resolve the shell to invoke for `bash`-tagged commands. Honors `$SHELL`
/// when set (so users who configure a non-default shell get consistent
/// behavior across the model-tool path and the RPC `runBash` path), and
/// falls back to `/bin/bash`.
pub fn resolve_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
}

/// Truncate output keeping the TAIL (end) within `max_bytes`.
///
/// Pi-mono parity: bash output is tail-truncated because errors, exit
/// summaries, and final results live at the end. Head-truncating hides the
/// useful information under compiler banners and progress logs.
///
/// Prefers to start at a line boundary so the LLM does not see a chopped
/// first line. Always lands on a UTF-8 char boundary so we never panic and
/// never emit invalid bytes.
fn truncate_tail_bytes(output: &str, max_bytes: usize) -> String {
    if output.len() <= max_bytes {
        return output.to_string();
    }
    // Start ~max_bytes from the end.
    let mut start = output.len() - max_bytes;
    // Step forward to a char boundary so we don't slice mid-codepoint.
    while start < output.len() && !output.is_char_boundary(start) {
        start += 1;
    }
    // Prefer to align to a newline so the first surviving line is whole.
    // Only do this when it would leave at least half the budget intact —
    // otherwise the whole final line is bigger than the budget itself and
    // we have to return a partial line (pi's "lastLinePartial" edge case).
    if let Some(nl_offset) = output[start..].find('\n') {
        let aligned = start + nl_offset + 1;
        if output.len() - aligned >= max_bytes / 2 {
            start = aligned;
        }
    }
    output[start..].to_string()
}

/// Sanitize output by stripping problematic characters.
///
/// Strips ANSI escapes, all C0 control characters except `\t \n \r`,
/// and the Unicode format characters U+FFF9-U+FFFB (which crash
/// terminal-width calculators). Embedded control sequences can be used
/// by an attacker to fake terminal output or smuggle prompt-injection
/// instructions to the LLM through bash tool results.
pub fn sanitize_output(output: &str) -> String {
    let ansi_re = regex_lite::Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap();
    let cleaned = ansi_re.replace_all(output, "");
    cleaned
        .chars()
        .filter(|&c| {
            let code = c as u32;
            // Allow tab, LF, CR
            if code == 0x09 || code == 0x0A || code == 0x0D {
                return true;
            }
            // Drop other C0 control chars
            if code <= 0x1F {
                return false;
            }
            // Drop Unicode format characters (string-width crashers)
            if (0xFFF9..=0xFFFB).contains(&code) {
                return false;
            }
            true
        })
        .collect()
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

    /// Pi-mono parity: `execute_bash` must actually apply `sanitize_output`
    /// to captured stdout/stderr. Embedded ANSI escapes, BEL bytes, and
    /// Unicode format chars from bash output must not reach the LLM.
    #[tokio::test]
    async fn test_execute_sanitizes_bash_output() {
        let dir = TempDir::new().unwrap();
        let result = execute_bash(
            // Emit ANSI red + BEL + visible text + ANSI reset
            "printf 'pre\\x1b[31m\\x07mid\\x1b[0mpost'",
            dir.path(),
            "/bin/bash",
            BashExecutorOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            result.output, "premidpost",
            "bash output must be sanitized of ANSI + BEL"
        );
    }

    /// Pi-mono parity: bare `\r` (without trailing `\n`) is stripped from
    /// bash output. Programs use `\r` for progress-bar overwrites; in a
    /// captured non-interactive stream this just produces garbled lines.
    #[tokio::test]
    async fn test_execute_strips_bare_carriage_returns() {
        let dir = TempDir::new().unwrap();
        let result = execute_bash(
            r"printf 'loading 10%%\rloading 50%%\rloading 100%%\n'",
            dir.path(),
            "/bin/bash",
            BashExecutorOptions::default(),
        )
        .await
        .unwrap();
        assert!(
            !result.output.contains('\r'),
            "expected \\r stripped, got: {:?}",
            result.output
        );
        assert!(
            result.output.contains("loading 100%"),
            "final line must survive, got: {:?}",
            result.output
        );
    }

    /// Pi-mono parity: when bash output exceeds the byte budget, keep the
    /// TAIL (end of output). The tail is where errors and final results live;
    /// head-truncation hides them under compiler boilerplate or progress logs.
    #[tokio::test]
    async fn test_execute_truncates_from_tail_not_head() {
        let dir = TempDir::new().unwrap();
        // Emit ~3000 numbered lines so we comfortably exceed a small byte cap.
        // The early lines contain HEAD_MARKER, the final lines contain
        // TAIL_MARKER. Tail-truncation must keep TAIL_MARKER and drop
        // HEAD_MARKER.
        let result = execute_bash(
            r#"
            for i in $(seq 1 3000); do
                echo "line $i HEAD_MARKER"
            done
            echo "TAIL_MARKER_FINAL"
            "#,
            dir.path(),
            "/bin/bash",
            BashExecutorOptions {
                max_bytes: 2048,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(result.truncated, "expected truncated=true");
        assert!(
            result.output.contains("TAIL_MARKER_FINAL"),
            "tail must survive truncation, got tail: {:?}",
            &result.output[result.output.len().saturating_sub(200)..]
        );
        assert!(
            !result.output.contains("line 1 HEAD_MARKER\n"),
            "head must be dropped, but found early line in output"
        );
        // Output should not exceed the budget plus a tiny truncation marker.
        assert!(
            result.output.len() <= 2048 + 256,
            "output ({} bytes) exceeds budget+slack",
            result.output.len()
        );
    }

    #[test]
    fn test_sanitize_output() {
        assert_eq!(sanitize_output("hello\x1b[31m world\x1b[0m"), "hello world");
        assert_eq!(sanitize_output("foo\0bar"), "foobar");
    }

    /// `on_chunk` must fire while the child is still running, not only
    /// once after it exits. A 1.5s command that emits a line every
    /// ~200ms should yield more than one snapshot, with the early
    /// snapshots strictly shorter than the final one.
    #[tokio::test]
    async fn on_chunk_fires_incrementally() {
        use std::sync::{Arc, Mutex};

        let dir = TempDir::new().unwrap();
        let snapshots: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let snapshots_for_cb = Arc::clone(&snapshots);
        let on_chunk: OnChunkFn = Box::new(move |s: &str| {
            snapshots_for_cb.lock().unwrap().push(s.to_string());
        });

        let result = execute_bash(
            "for i in 1 2 3 4 5 6; do echo line $i; sleep 0.2; done",
            dir.path(),
            "/bin/bash",
            BashExecutorOptions {
                on_chunk: Some(on_chunk),
                timeout_secs: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(result.exit_code, Some(0));
        let captured = snapshots.lock().unwrap().clone();
        assert!(
            captured.len() >= 2,
            "expected ≥ 2 chunk callbacks, got {}: {:?}",
            captured.len(),
            captured
        );
        let final_len = captured.last().unwrap().len();
        let first_len = captured.first().unwrap().len();
        assert!(
            first_len < final_len,
            "first snapshot must be shorter than final: first={} final={}",
            first_len,
            final_len
        );
    }

    /// Pi-mono parity: drop C0 control characters except `\t \n \r`. Bash
    /// output can contain BEL (0x07), VT (0x0B), or other control bytes that
    /// confuse terminal width calculators and can be used to smuggle
    /// instructions into LLM-rendered tool results.
    #[test]
    fn test_sanitize_strips_c0_controls_except_whitespace() {
        // Tab, LF, CR pass through
        assert_eq!(sanitize_output("a\tb\nc\rd"), "a\tb\nc\rd");
        // BEL (0x07), VT (0x0B), FF (0x0C), SOH (0x01) get stripped
        assert_eq!(sanitize_output("a\x07b"), "ab");
        assert_eq!(sanitize_output("a\x0Bb\x0Cc"), "abc");
        assert_eq!(sanitize_output("\x01\x02\x03hello\x1F"), "hello");
        // DEL (0x7F) is not C0 — pi keeps it
        assert_eq!(sanitize_output("a\x7Fb"), "a\x7Fb");
    }

    /// Pi-mono parity: drop Unicode format characters U+FFF9..U+FFFB. These
    /// crash `string-width`-style libraries; pi filters them, so we do too.
    #[test]
    fn test_sanitize_strips_unicode_format_chars() {
        assert_eq!(sanitize_output("a\u{FFF9}b\u{FFFA}c\u{FFFB}d"), "abcd");
        // Adjacent codepoints (U+FFF8, U+FFFC) survive — only the documented
        // range is stripped.
        assert_eq!(sanitize_output("a\u{FFF8}b\u{FFFC}c"), "a\u{FFF8}b\u{FFFC}c");
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
