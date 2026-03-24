//! Text utilities for terminal rendering.
//!
//! ANSI-aware width calculation, text wrapping, and truncation.

use unicode_width::UnicodeWidthChar;

/// Calculate the visible width of a string, ignoring ANSI escape codes.
pub fn visible_width(text: &str) -> usize {
    let mut width = 0;
    let mut in_escape = false;
    let chars = text.chars().peekable();

    for ch in chars {
        if in_escape {
            if ch.is_ascii_alphabetic() || ch == 'm' || ch == 'H' || ch == 'J' || ch == 'K' {
                in_escape = false;
            }
            continue;
        }
        if ch == '\x1b' {
            in_escape = true;
            continue;
        }
        // Skip other control characters
        if ch.is_control() {
            continue;
        }
        width += char_width(ch);
    }

    width
}

/// Get the display width of a single character.
pub fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Truncate a string to a maximum visible width, preserving ANSI codes.
/// Appends "…" if truncated.
pub fn truncate_to_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }

    let mut result = String::new();
    let mut width = 0;
    let mut in_escape = false;
    let text_width = visible_width(text);

    if text_width <= max_width {
        return text.to_string();
    }

    let target_width = max_width.saturating_sub(1); // Reserve space for "…"

    for ch in text.chars() {
        if in_escape {
            result.push(ch);
            if ch.is_ascii_alphabetic() || ch == 'm' || ch == 'H' || ch == 'J' || ch == 'K' {
                in_escape = false;
            }
            continue;
        }
        if ch == '\x1b' {
            in_escape = true;
            result.push(ch);
            continue;
        }
        if ch.is_control() {
            result.push(ch);
            continue;
        }

        let cw = char_width(ch);
        if width + cw > target_width {
            break;
        }
        width += cw;
        result.push(ch);
    }

    result.push('…');
    // Close any open ANSI sequences
    result.push_str("\x1b[0m");
    result
}

/// Wrap text to a given width, preserving ANSI escape codes.
/// Returns a vector of wrapped lines.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }

    let mut lines = Vec::new();
    let mut current_line = String::new();
    let mut current_width = 0;
    let mut in_escape = false;

    for ch in text.chars() {
        if in_escape {
            current_line.push(ch);
            if ch.is_ascii_alphabetic() || ch == 'm' || ch == 'H' || ch == 'J' || ch == 'K' {
                in_escape = false;
            }
            continue;
        }
        if ch == '\x1b' {
            in_escape = true;
            current_line.push(ch);
            continue;
        }
        if ch == '\n' {
            lines.push(std::mem::take(&mut current_line));
            current_width = 0;
            continue;
        }
        if ch.is_control() {
            continue;
        }

        let cw = char_width(ch);
        if current_width + cw > width && current_width > 0 {
            lines.push(std::mem::take(&mut current_line));
            current_width = 0;
        }
        current_line.push(ch);
        current_width += cw;
    }

    lines.push(current_line);
    lines
}

/// Pad a string to a given width with spaces.
pub fn pad_to_width(text: &str, target_width: usize) -> String {
    let current = visible_width(text);
    if current >= target_width {
        return text.to_string();
    }
    let padding = target_width - current;
    format!("{}{}", text, " ".repeat(padding))
}

/// Apply a background color to a line, filling the full width.
pub fn apply_background(line: &str, width: usize, bg_code: &str, reset: &str) -> String {
    let padded = pad_to_width(line, width);
    format!("{}{}{}", bg_code, padded, reset)
}

/// Strip all ANSI escape codes from a string.
pub fn strip_ansi(text: &str) -> String {
    let mut result = String::new();
    let mut in_escape = false;

    for ch in text.chars() {
        if in_escape {
            if ch.is_ascii_alphabetic() || ch == 'm' || ch == 'H' || ch == 'J' || ch == 'K' {
                in_escape = false;
            }
            continue;
        }
        if ch == '\x1b' {
            in_escape = true;
            continue;
        }
        result.push(ch);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_visible_width_plain() {
        assert_eq!(visible_width("hello"), 5);
        assert_eq!(visible_width(""), 0);
        assert_eq!(visible_width("abc"), 3);
    }

    #[test]
    fn test_visible_width_ansi() {
        assert_eq!(visible_width("\x1b[31mhello\x1b[0m"), 5);
        assert_eq!(visible_width("\x1b[1;32mtest\x1b[0m"), 4);
    }

    #[test]
    fn test_visible_width_wide_chars() {
        // Chinese characters are double-width
        assert_eq!(visible_width("你好"), 4);
        assert_eq!(visible_width("a你好b"), 6);
    }

    #[test]
    fn test_truncate_to_width_no_truncation() {
        assert_eq!(truncate_to_width("hello", 10), "hello");
        assert_eq!(truncate_to_width("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_to_width_truncates() {
        let result = truncate_to_width("hello world", 6);
        assert!(result.contains("…"));
        assert!(visible_width(&strip_ansi(&result)) <= 6);
    }

    #[test]
    fn test_truncate_to_width_zero() {
        assert_eq!(truncate_to_width("hello", 0), "");
    }

    #[test]
    fn test_wrap_text_no_wrap() {
        assert_eq!(wrap_text("hello", 80), vec!["hello"]);
    }

    #[test]
    fn test_wrap_text_wraps() {
        let lines = wrap_text("hello world", 5);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "hello");
        assert_eq!(lines[1], " worl");
        assert_eq!(lines[2], "d");
    }

    #[test]
    fn test_wrap_text_newlines() {
        let lines = wrap_text("line1\nline2", 80);
        assert_eq!(lines, vec!["line1", "line2"]);
    }

    #[test]
    fn test_wrap_text_ansi_preserved() {
        let lines = wrap_text("\x1b[31mhello\x1b[0m", 80);
        assert_eq!(lines, vec!["\x1b[31mhello\x1b[0m"]);
    }

    #[test]
    fn test_pad_to_width() {
        assert_eq!(pad_to_width("hi", 5), "hi   ");
        assert_eq!(pad_to_width("hello", 3), "hello"); // No truncation
    }

    #[test]
    fn test_strip_ansi() {
        assert_eq!(strip_ansi("\x1b[31mhello\x1b[0m"), "hello");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn test_apply_background() {
        let result = apply_background("hi", 5, "\x1b[44m", "\x1b[0m");
        assert!(result.starts_with("\x1b[44m"));
        assert!(result.ends_with("\x1b[0m"));
    }

    #[test]
    fn test_char_width() {
        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width('你'), 2);
    }
}
