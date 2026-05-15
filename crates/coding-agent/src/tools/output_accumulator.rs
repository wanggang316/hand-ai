//! Streaming output accumulator with bounded memory and a temp-file
//! spill path.
//!
//! Tools that stream long output (most notably `bash`) feed bytes into
//! an [`OutputAccumulator`] as they arrive. The accumulator:
//!
//! - decodes incrementally with a streaming UTF-8 decoder so multi-byte
//!   characters that straddle chunk boundaries don't get mangled;
//! - keeps only a *rolling tail* of decoded text for snapshots, capped
//!   at `2 × max_bytes` to keep memory bounded even for very long
//!   outputs;
//! - writes the *raw* bytes to a temp file once the output exceeds
//!   either the line or the byte limit, so callers can persist the full
//!   stream without holding it all in memory;
//! - exposes a [`snapshot`] that pairs a tail-truncated string with the
//!   full counts and the path to the spilled file (when present).
//!
//! ## Implementation notes
//!
//! - A small `pending` buffer holds incomplete UTF-8 continuation bytes
//!   between calls, so multi-byte characters that straddle chunk
//!   boundaries decode correctly. Invalid sequences fall back to
//!   U+FFFD.
//! - Spill writes use the blocking `std::fs::File` writer. The `bash`
//!   tool runs the accumulator on a `tokio::task::spawn_blocking`
//!   thread, so this is safe; an async caller should swap to
//!   `tokio::fs::File`.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use rand::RngCore;

use super::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, TruncationOptions, TruncationResult,
    truncate_tail,
};

/// Caller-tunable limits and identifiers for the spill file.
#[derive(Debug, Clone, Default)]
pub struct OutputAccumulatorOptions {
    pub max_lines: Option<usize>,
    pub max_bytes: Option<usize>,
    pub temp_file_prefix: Option<String>,
}

/// Result of a snapshot describing the buffered output.
#[derive(Debug, Clone)]
pub struct OutputSnapshot {
    pub content: String,
    pub truncation: TruncationResult,
    pub full_output_path: Option<PathBuf>,
}

/// Accumulator for streamed tool output.
pub struct OutputAccumulator {
    max_lines: usize,
    max_bytes: usize,
    max_rolling_bytes: usize,
    temp_file_prefix: String,

    /// Pending UTF-8 continuation bytes from the previous append.
    pending: Vec<u8>,

    /// Raw byte chunks held in memory while we haven't yet decided to
    /// spill to disk. Cleared once the temp file is opened.
    raw_chunks: Vec<Vec<u8>>,

    /// Decoded tail kept for snapshots. Bounded by [`Self::trim_tail`].
    tail_text: String,
    tail_starts_at_line_boundary: bool,

    total_raw_bytes: usize,
    total_decoded_bytes: usize,
    total_lines: usize,
    current_line_bytes: usize,
    finished: bool,

    temp_file_path: Option<PathBuf>,
    temp_file: Option<File>,
}

impl OutputAccumulator {
    pub fn new(options: OutputAccumulatorOptions) -> Self {
        let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
        let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
        let max_rolling_bytes = (max_bytes.saturating_mul(2)).max(1);
        let temp_file_prefix = options
            .temp_file_prefix
            .unwrap_or_else(|| "pi-output".to_string());
        Self {
            max_lines,
            max_bytes,
            max_rolling_bytes,
            temp_file_prefix,
            pending: Vec::new(),
            raw_chunks: Vec::new(),
            tail_text: String::new(),
            tail_starts_at_line_boundary: true,
            total_raw_bytes: 0,
            total_decoded_bytes: 0,
            total_lines: 1,
            current_line_bytes: 0,
            finished: false,
            temp_file_path: None,
            temp_file: None,
        }
    }

    /// Append a byte chunk. Panics if called after [`Self::finish`].
    pub fn append(&mut self, data: &[u8]) {
        if self.finished {
            panic!("cannot append to a finished output accumulator");
        }
        self.total_raw_bytes += data.len();

        // Streaming UTF-8 decode: combine pending with new bytes, find
        // the longest prefix that is valid UTF-8, decode that, and stash
        // the suffix as new pending. Invalid sequences are replaced by
        // U+FFFD so we never panic on garbage input.
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(data);
        let (decoded, leftover) = decode_streaming(&buf);
        self.pending = leftover;
        if !decoded.is_empty() {
            self.append_decoded_text(&decoded);
        }

        if self.temp_file.is_some() || self.should_use_temp_file() {
            self.ensure_temp_file();
            if let Some(f) = self.temp_file.as_mut() {
                let _ = f.write_all(data);
            }
        } else if !data.is_empty() {
            self.raw_chunks.push(data.to_vec());
        }
    }

    /// Finalise the accumulator. Flushes any pending bytes (replaced
    /// with U+FFFD if they're invalid UTF-8 at end-of-stream).
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;

        if !self.pending.is_empty() {
            // Whatever's left can't be decoded validly; render with
            // U+FFFD just like JS's TextDecoder finalisation does.
            let leftover = std::mem::take(&mut self.pending);
            let s = String::from_utf8_lossy(&leftover).into_owned();
            if !s.is_empty() {
                self.append_decoded_text(&s);
            }
        }

        if self.should_use_temp_file() {
            self.ensure_temp_file();
        }
    }

    /// Build a snapshot of the current state. If the output has already
    /// been truncated and `persist_if_truncated` is set, the spill file
    /// is opened (the snapshot's `full_output_path` is then populated).
    pub fn snapshot(&mut self, persist_if_truncated: bool) -> OutputSnapshot {
        let snapshot_text = self.snapshot_text().to_string();
        let tail = truncate_tail(
            &snapshot_text,
            TruncationOptions {
                max_lines: Some(self.max_lines),
                max_bytes: Some(self.max_bytes),
            },
        );
        let truncated =
            self.total_lines > self.max_lines || self.total_decoded_bytes > self.max_bytes;
        let truncated_by = if truncated {
            tail.truncated_by.or({
                if self.total_decoded_bytes > self.max_bytes {
                    Some(TruncatedBy::Bytes)
                } else {
                    Some(TruncatedBy::Lines)
                }
            })
        } else {
            None
        };
        let truncation = TruncationResult {
            content: tail.content.clone(),
            truncated,
            truncated_by,
            total_lines: self.total_lines,
            total_bytes: self.total_decoded_bytes,
            output_lines: tail.output_lines,
            output_bytes: tail.output_bytes,
            last_line_partial: tail.last_line_partial,
            first_line_exceeds_limit: tail.first_line_exceeds_limit,
            max_lines: self.max_lines,
            max_bytes: self.max_bytes,
        };

        if persist_if_truncated && truncation.truncated {
            self.ensure_temp_file();
        }

        OutputSnapshot {
            content: truncation.content.clone(),
            truncation,
            full_output_path: self.temp_file_path.clone(),
        }
    }

    /// Close the temp file, flushing any buffered writes. Idempotent.
    pub fn close_temp_file(&mut self) {
        if let Some(mut f) = self.temp_file.take() {
            let _ = f.flush();
            // Drop closes the descriptor.
            drop(f);
        }
    }

    /// Number of bytes since the last newline in the decoded stream.
    pub fn last_line_bytes(&self) -> usize {
        self.current_line_bytes
    }

    /// Path of the spill file, when present.
    pub fn temp_file_path(&self) -> Option<&Path> {
        self.temp_file_path.as_deref()
    }

    fn append_decoded_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let bytes = text.len();
        self.total_decoded_bytes += bytes;
        self.tail_text.push_str(text);
        if self.tail_text.len() > self.max_rolling_bytes * 2 {
            self.trim_tail();
        }

        // Count newlines and update `current_line_bytes` to the bytes
        // since the last newline in this chunk.
        let mut newlines = 0usize;
        let mut last_newline: Option<usize> = None;
        for (idx, _) in text.match_indices('\n') {
            newlines += 1;
            last_newline = Some(idx);
        }
        if newlines == 0 {
            self.current_line_bytes += bytes;
        } else {
            self.total_lines += newlines;
            // Bytes after the last newline.
            let after = last_newline.map(|i| i + 1).unwrap_or(0);
            self.current_line_bytes = text.len() - after;
        }
    }

    fn trim_tail(&mut self) {
        let bytes = self.tail_text.as_bytes();
        if bytes.len() <= self.max_rolling_bytes {
            return;
        }
        let mut start = bytes.len() - self.max_rolling_bytes;
        while start < bytes.len() && (bytes[start] & 0xC0) == 0x80 {
            start += 1;
        }
        self.tail_starts_at_line_boundary = if start == 0 {
            self.tail_starts_at_line_boundary
        } else {
            bytes[start - 1] == b'\n'
        };
        // Safety: `start` is on a UTF-8 boundary.
        self.tail_text = std::str::from_utf8(&bytes[start..])
            .unwrap_or("")
            .to_string();
    }

    fn snapshot_text(&self) -> &str {
        if self.tail_starts_at_line_boundary {
            return &self.tail_text;
        }
        match self.tail_text.find('\n') {
            Some(i) => &self.tail_text[i + 1..],
            None => &self.tail_text,
        }
    }

    fn should_use_temp_file(&self) -> bool {
        self.total_raw_bytes > self.max_bytes
            || self.total_decoded_bytes > self.max_bytes
            || self.total_lines > self.max_lines
    }

    fn ensure_temp_file(&mut self) {
        if self.temp_file_path.is_some() {
            return;
        }
        let path = default_temp_file_path(&self.temp_file_prefix);
        match File::create(&path) {
            Ok(mut f) => {
                for chunk in self.raw_chunks.drain(..) {
                    let _ = f.write_all(&chunk);
                }
                self.temp_file_path = Some(path);
                self.temp_file = Some(f);
            }
            Err(_) => {
                // Spill failed; keep the data in memory.
            }
        }
    }
}

/// Compose a unique temp-file path under the system temp dir.
fn default_temp_file_path(prefix: &str) -> PathBuf {
    let mut bytes = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut bytes);
    let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut p = std::env::temp_dir();
    p.push(format!("{prefix}-{id}.log"));
    p
}

/// Streaming UTF-8 decode: returns (decoded_string, trailing_bytes).
///
/// Trailing bytes are an incomplete UTF-8 sequence at the end of the
/// input that should be saved for the next chunk. Anything that's
/// invalid mid-stream is replaced with U+FFFD via `String::from_utf8_lossy`.
fn decode_streaming(buf: &[u8]) -> (String, Vec<u8>) {
    if buf.is_empty() {
        return (String::new(), Vec::new());
    }
    // Find the longest valid UTF-8 prefix, then look at the trailing
    // bytes to decide whether they're a partial multi-byte sequence we
    // should save for next time.
    let mut split = buf.len();
    while split > 0 {
        if std::str::from_utf8(&buf[..split]).is_ok() {
            break;
        }
        split -= 1;
    }
    let valid = &buf[..split];
    let trailing = &buf[split..];

    // If `trailing` could be the start of a valid multi-byte sequence
    // (UTF-8 lead byte with not-yet-arrived continuations), keep it as
    // pending. Otherwise absorb it as U+FFFD via from_utf8_lossy.
    let pending_len = utf8_pending_len(trailing);
    let pending = trailing[trailing.len() - pending_len..].to_vec();
    let absorb = &trailing[..trailing.len() - pending_len];

    let mut out = String::from_utf8_lossy(valid).into_owned();
    if !absorb.is_empty() {
        out.push_str(&String::from_utf8_lossy(absorb));
    }
    (out, pending)
}

/// How many trailing bytes of `buf` look like the start of a valid
/// UTF-8 multi-byte sequence (and therefore should be held as pending
/// for the next chunk). Bounded by 3 (since the longest sequence is 4
/// bytes and we can keep at most `length - 1` bytes pending).
fn utf8_pending_len(buf: &[u8]) -> usize {
    // Walk back looking for a UTF-8 lead byte (not a continuation).
    // Keep up to 3 trailing bytes pending; anything longer is invalid.
    let n = buf.len().min(3);
    for i in 1..=n {
        let b = buf[buf.len() - i];
        if (b & 0xC0) == 0x80 {
            // Continuation byte — keep walking.
            continue;
        }
        // Lead byte. Decide whether the partial sequence could become
        // valid with more bytes.
        let expected_len = if b < 0x80 {
            1
        } else if (b & 0xE0) == 0xC0 {
            2
        } else if (b & 0xF0) == 0xE0 {
            3
        } else if (b & 0xF8) == 0xF0 {
            4
        } else {
            // Invalid lead byte.
            return 0;
        };
        if expected_len == 1 {
            // Single-byte: not pending.
            return 0;
        }
        if i < expected_len {
            return i;
        }
        return 0;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(max_lines: usize, max_bytes: usize) -> OutputAccumulatorOptions {
        OutputAccumulatorOptions {
            max_lines: Some(max_lines),
            max_bytes: Some(max_bytes),
            temp_file_prefix: Some("test-output".into()),
        }
    }

    #[test]
    fn snapshot_returns_full_content_when_under_limits() {
        let mut acc = OutputAccumulator::new(opts(1000, 1024 * 1024));
        acc.append(b"hello\nworld\n");
        acc.finish();
        let snap = acc.snapshot(false);
        assert_eq!(snap.content, "hello\nworld\n");
        assert!(!snap.truncation.truncated);
        assert!(snap.full_output_path.is_none());
    }

    #[test]
    fn snapshot_marks_truncated_when_lines_exceed_max() {
        let mut acc = OutputAccumulator::new(opts(2, 1024 * 1024));
        acc.append(b"a\nb\nc\nd\n");
        acc.finish();
        let snap = acc.snapshot(true);
        assert!(snap.truncation.truncated);
        assert_eq!(snap.truncation.total_lines, 5); // trailing \n -> 5 elements
        assert!(snap.full_output_path.is_some());
    }

    #[test]
    fn streaming_utf8_decoder_preserves_split_multibyte() {
        let mut acc = OutputAccumulator::new(opts(1000, 1024 * 1024));
        // "你" is 0xE4 0xBD 0xA0; split across two appends.
        acc.append(&[0xE4, 0xBD]);
        acc.append(&[0xA0]);
        acc.finish();
        let snap = acc.snapshot(false);
        assert_eq!(snap.content, "你");
    }

    #[test]
    fn streaming_decoder_handles_invalid_at_finish() {
        let mut acc = OutputAccumulator::new(opts(1000, 1024 * 1024));
        // 0xC2 expects a continuation byte; never arrives.
        acc.append(&[0xC2]);
        acc.finish();
        let snap = acc.snapshot(false);
        // U+FFFD replacement char.
        assert!(snap.content.contains('\u{FFFD}'));
    }

    #[test]
    fn temp_file_path_populated_when_byte_budget_exceeded() {
        let mut acc = OutputAccumulator::new(opts(1000, 16));
        let payload = vec![b'a'; 64];
        acc.append(&payload);
        acc.finish();
        let snap = acc.snapshot(false);
        assert!(snap.truncation.truncated);
        assert!(
            snap.full_output_path.is_some(),
            "spill file must be created when byte budget is exceeded"
        );
        // The file should contain the full raw stream.
        let path = snap.full_output_path.unwrap();
        let on_disk = std::fs::read(&path).expect("temp file readable");
        assert_eq!(on_disk, payload);
        // Cleanup.
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn last_line_bytes_resets_on_newline() {
        let mut acc = OutputAccumulator::new(opts(1000, 1024 * 1024));
        acc.append(b"abc\nde");
        assert_eq!(acc.last_line_bytes(), 2);
        acc.append(b"f\n");
        assert_eq!(acc.last_line_bytes(), 0);
    }
}
