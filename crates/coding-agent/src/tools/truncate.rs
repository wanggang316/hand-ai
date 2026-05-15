//! Shared truncation utilities for tool outputs.
//!
//! Truncation is governed by two independent limits — whichever is hit
//! first wins:
//!
//! - line limit (default `DEFAULT_MAX_LINES` = 2000)
//! - byte limit (default `DEFAULT_MAX_BYTES` = 50 KB)
//!
//! Byte counts are UTF-8 byte length, **not** char count. Tail truncation
//! is allowed to slice the *first* line of the output mid-character; we
//! advance to the next valid UTF-8 boundary rather than ever returning
//! invalid bytes.
//!
//! Head truncation never returns a partial line. If the very first line
//! exceeds the byte limit, the result is an empty string with
//! `first_line_exceeds_limit = true` — the caller is expected to render
//! the limit hint instead of any payload.

/// Default line cap before truncation kicks in.
pub const DEFAULT_MAX_LINES: usize = 2000;

/// Default byte cap (mirrors TS `DEFAULT_MAX_BYTES`).
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;

/// Per-line cap used by grep match rendering.
pub const GREP_MAX_LINE_LENGTH: usize = 500;

/// Which dimension a truncation hit, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncatedBy {
    Lines,
    Bytes,
}

/// Caller-tunable truncation limits.
#[derive(Debug, Clone, Copy, Default)]
pub struct TruncationOptions {
    pub max_lines: Option<usize>,
    pub max_bytes: Option<usize>,
}

/// Result of [`truncate_head`] / [`truncate_tail`].
#[derive(Debug, Clone)]
pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    pub truncated_by: Option<TruncatedBy>,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: usize,
    pub max_bytes: usize,
}

/// Format `bytes` as a human-readable size. Mirrors TS `formatSize`.
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", (bytes as f64) / 1024.0)
    } else {
        format!("{:.1}MB", (bytes as f64) / (1024.0 * 1024.0))
    }
}

/// Split a string on `\n` boundaries, mirroring JS `String.prototype.split("\n")`.
///
/// Notably, a trailing newline yields a final empty element so a 3-line
/// content with a trailing `\n` reports `total_lines = 4`. This matches
/// the TS reference's accounting; downstream UI subtracts 1 if needed.
fn split_lines(content: &str) -> Vec<&str> {
    content.split('\n').collect()
}

/// Head truncation: keep the first N complete lines, never returning a
/// partial line. If the very first line already exceeds `max_bytes`, the
/// result has empty content and `first_line_exceeds_limit = true`.
pub fn truncate_head(content: &str, opts: TruncationOptions) -> TruncationResult {
    let max_lines = opts.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = opts.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = content.len();
    let lines = split_lines(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    // First line alone exceeds the byte budget → bail with empty output.
    let first_line_bytes = lines.first().map(|l| l.len()).unwrap_or(0);
    if first_line_bytes > max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    let mut output_lines: Vec<&str> = Vec::new();
    let mut output_bytes: usize = 0;
    let mut truncated_by = TruncatedBy::Lines;

    for (i, line) in lines.iter().enumerate() {
        if i >= max_lines {
            break;
        }
        // +1 for the joining newline, except for the first line.
        let line_bytes = line.len() + if i > 0 { 1 } else { 0 };
        if output_bytes + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        output_lines.push(line);
        output_bytes += line_bytes;
    }

    if output_lines.len() >= max_lines && output_bytes <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = output_lines.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output_lines.len(),
        output_bytes: final_output_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Tail truncation: keep the last N complete lines. Edge case — if the
/// final line alone exceeds `max_bytes`, return its byte-suffix (sliced
/// on a UTF-8 boundary) and set `last_line_partial = true`.
pub fn truncate_tail(content: &str, opts: TruncationOptions) -> TruncationResult {
    let max_lines = opts.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = opts.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = content.len();
    let lines = split_lines(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    let mut output_lines: Vec<String> = Vec::new();
    let mut output_bytes: usize = 0;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;

    let mut i = lines.len();
    while i > 0 && output_lines.len() < max_lines {
        i -= 1;
        let line = lines[i];
        // +1 for the joining newline once we already have at least one line.
        let line_bytes = line.len() + if !output_lines.is_empty() { 1 } else { 0 };
        if output_bytes + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            // Edge case: nothing collected yet AND this line exceeds the
            // budget on its own — slice from the end, snapped to a UTF-8
            // char boundary.
            if output_lines.is_empty() {
                let truncated_line = truncate_string_to_bytes_from_end(line, max_bytes);
                output_bytes = truncated_line.len();
                output_lines.insert(0, truncated_line);
                last_line_partial = true;
            }
            break;
        }
        output_lines.insert(0, line.to_string());
        output_bytes += line_bytes;
    }

    if output_lines.len() >= max_lines && output_bytes <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = output_lines.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output_lines.len(),
        output_bytes: final_output_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Slice `s` to fit within `max_bytes` from the end, snapping to the
/// nearest UTF-8 character boundary. Returns the original string when it
/// is already within budget.
fn truncate_string_to_bytes_from_end(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut start = bytes.len() - max_bytes;
    // Move forward to the next character boundary. UTF-8 continuation
    // bytes have the form 10xxxxxx (mask 0xC0 == 0x80).
    while start < bytes.len() && (bytes[start] & 0xC0) == 0x80 {
        start += 1;
    }
    // Safety: `start` now lands on a UTF-8 char boundary or at the end.
    std::str::from_utf8(&bytes[start..])
        .unwrap_or("")
        .to_string()
}

/// Single-line truncation used by grep / similar tools. Returns the
/// truncated text plus a flag.
pub fn truncate_line(line: &str, max_chars: usize) -> (String, bool) {
    // Match TS semantics: max_chars is a *character* count (UTF-16 code
    // units in JS, which for our purposes is close enough to char count
    // for the strings we render). Use char-aware slicing so we never
    // produce invalid UTF-8.
    let char_count = line.chars().count();
    if char_count <= max_chars {
        return (line.to_string(), false);
    }
    let head: String = line.chars().take(max_chars).collect();
    (format!("{head}... [truncated]"), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(max_lines: usize, max_bytes: usize) -> TruncationOptions {
        TruncationOptions {
            max_lines: Some(max_lines),
            max_bytes: Some(max_bytes),
        }
    }

    #[test]
    fn format_size_renders_expected_units() {
        assert_eq!(format_size(0), "0B");
        assert_eq!(format_size(1023), "1023B");
        assert_eq!(format_size(1024), "1.0KB");
        assert_eq!(format_size(1024 * 1024), "1.0MB");
    }

    #[test]
    fn truncate_head_no_truncation_when_under_limits() {
        let content = "a\nb\nc";
        let r = truncate_head(content, opts(10, 1024));
        assert!(!r.truncated);
        assert_eq!(r.truncated_by, None);
        assert_eq!(r.content, "a\nb\nc");
        assert_eq!(r.total_lines, 3);
        assert_eq!(r.output_lines, 3);
    }

    #[test]
    fn truncate_head_line_limit_only() {
        let content = "1\n2\n3\n4\n5";
        let r = truncate_head(content, opts(3, 1024));
        assert!(r.truncated);
        assert_eq!(r.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(r.content, "1\n2\n3");
        assert_eq!(r.output_lines, 3);
    }

    #[test]
    fn truncate_head_byte_limit_only() {
        // Each line is 5 bytes ("aaaaa"); separator adds 1.
        let content = "aaaaa\nbbbbb\nccccc";
        // Budget for 5 + 1 + 5 = 11 bytes (cuts before third line).
        let r = truncate_head(content, opts(100, 11));
        assert!(r.truncated);
        assert_eq!(r.truncated_by, Some(TruncatedBy::Bytes));
        assert_eq!(r.content, "aaaaa\nbbbbb");
    }

    #[test]
    fn truncate_head_first_line_exceeds_limit() {
        let content = "this is a very long first line\nshort";
        let r = truncate_head(content, opts(100, 5));
        assert!(r.truncated);
        assert!(r.first_line_exceeds_limit);
        assert_eq!(r.truncated_by, Some(TruncatedBy::Bytes));
        assert_eq!(r.content, "");
        assert_eq!(r.output_lines, 0);
    }

    #[test]
    fn truncate_tail_keeps_last_lines() {
        let content = "1\n2\n3\n4\n5";
        let r = truncate_tail(content, opts(3, 1024));
        assert!(r.truncated);
        assert_eq!(r.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(r.content, "3\n4\n5");
    }

    #[test]
    fn truncate_tail_partial_first_line_on_utf8_boundary() {
        // Five 3-byte UTF-8 chars, total 15 bytes on one line.
        let content = "你好世界吧";
        // Limit to 8 bytes — between 2 and 3 chars worth, so the slicer
        // must skip continuation bytes to land on a char boundary.
        let r = truncate_tail(content, opts(100, 8));
        assert!(r.truncated);
        assert!(r.last_line_partial);
        // The result must be valid UTF-8 (already checked by being a String).
        // It should be a suffix consisting of complete characters that
        // fits within max_bytes.
        assert!(r.content.len() <= 8, "content: {:?}", r.content);
        assert!(content.ends_with(&r.content), "must be suffix");
    }

    #[test]
    fn truncate_tail_no_truncation_when_under_limits() {
        let content = "a\nb";
        let r = truncate_tail(content, opts(10, 1024));
        assert!(!r.truncated);
        assert_eq!(r.content, "a\nb");
    }

    #[test]
    fn truncate_line_under_limit_passes_through() {
        let (text, truncated) = truncate_line("short", 50);
        assert_eq!(text, "short");
        assert!(!truncated);
    }

    #[test]
    fn truncate_line_over_limit_appends_marker() {
        let line = "a".repeat(600);
        let (text, truncated) = truncate_line(&line, GREP_MAX_LINE_LENGTH);
        assert!(truncated);
        assert!(text.ends_with("... [truncated]"));
        // Head is exactly max_chars.
        assert_eq!(text.len(), GREP_MAX_LINE_LENGTH + "... [truncated]".len());
    }
}
