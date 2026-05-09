//! JSON parsing helpers, including a best-effort partial parser used to
//! decode arguments while a streaming response is still arriving.
//!
//! `try_parse_strict` is a thin wrapper over `serde_json::from_str`.
//!
//! `safe_parse_partial` applies a short list of heuristics — trim trailing
//! whitespace, drop a trailing comma, close an unterminated string, and
//! balance any open `{` / `[` brackets — then re-attempts to parse. If the
//! repair still does not yield valid JSON, `None` is returned.

use serde_json::Value;

/// Attempt to parse `s` as strict JSON. Returns `None` on any error.
pub fn try_parse_strict(s: &str) -> Option<Value> {
    serde_json::from_str(s).ok()
}

/// Best-effort parse of potentially incomplete JSON.
///
/// First attempts a strict parse. If that fails, applies the following
/// repairs in order before retrying:
///
/// - trim trailing whitespace
/// - drop a trailing `,`
/// - if a string literal is unterminated, append a closing `"`
/// - close any still-open `{` / `[` with their matching `}` / `]`
///
/// Returns `None` if the repaired input is still not valid JSON.
pub fn safe_parse_partial(s: &str) -> Option<Value> {
    if let Some(value) = try_parse_strict(s) {
        return Some(value);
    }

    let trimmed = s.trim_end();
    if trimmed.is_empty() {
        return None;
    }

    let repaired = repair_partial(trimmed);
    if repaired == trimmed {
        return try_parse_strict(trimmed);
    }
    try_parse_strict(&repaired)
}

/// Apply the repair heuristics described on `safe_parse_partial`.
fn repair_partial(input: &str) -> String {
    // Strip trailing whitespace and any dangling trailing comma. Also strip
    // commas that appear immediately before a closing brace/bracket
    // (`{"a":1,}` and `[1,]`), which are the most common partial-JSON
    // shapes emitted by streaming providers.
    let stripped: String = strip_trailing_commas(input.trim_end());
    let mut s: String = stripped.trim_end_matches(',').to_string();

    // Walk the string tracking string-literal state and bracket stack so we
    // can close any unterminated string and any open containers.
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;

    for ch in s.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' if stack.last().copied() == Some(ch) => {
                stack.pop();
            }
            _ => {}
        }
    }

    // Close an unterminated string first, then any open containers.
    if in_string {
        // If we ended mid-escape sequence, drop the dangling backslash to
        // avoid producing an invalid `"\"` literal.
        if escaped {
            s.pop();
        }
        s.push('"');
    }

    // After closing the string we may have a dangling trailing comma inside
    // the now-closed string's container — strip whitespace + trailing comma
    // again before sealing the brackets.
    while let Some(last) = s.chars().last() {
        if last == ',' || last.is_whitespace() {
            s.pop();
        } else {
            break;
        }
    }

    while let Some(closer) = stack.pop() {
        s.push(closer);
    }

    s
}

/// Strip commas that appear immediately before a closing `}` or `]`, while
/// preserving any comma found inside a string literal.
fn strip_trailing_commas(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];

        if in_string {
            out.push(b as char);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        if b == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }

        if b == b',' {
            // Look ahead past whitespace; if next non-space char is `}` or
            // `]`, drop this comma.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                i += 1;
                continue;
            }
        }

        // Non-ASCII bytes are part of a multi-byte UTF-8 sequence — copy
        // them verbatim by walking the original char.
        if b < 0x80 {
            out.push(b as char);
            i += 1;
        } else {
            let ch_len = utf8_char_len(b);
            // Safety: input is a valid &str, so the slice is a valid char.
            let ch_str = &input[i..i + ch_len];
            out.push_str(ch_str);
            i += ch_len;
        }
    }

    out
}

fn utf8_char_len(first_byte: u8) -> usize {
    // first_byte < 0xC0 covers ASCII and stray continuation bytes; both
    // advance by one byte. The higher branches map UTF-8 lead bytes to the
    // length of their sequence.
    if first_byte < 0xC0 {
        1
    } else if first_byte < 0xE0 {
        2
    } else if first_byte < 0xF0 {
        3
    } else {
        4
    }
}
