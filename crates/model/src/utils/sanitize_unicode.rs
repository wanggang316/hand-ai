//! Replace lone UTF-16 surrogates and other malformed byte sequences with
//! the Unicode replacement character (U+FFFD).
//!
//! Rust's safe `&str` is guaranteed to be well-formed UTF-8 and therefore
//! cannot carry a lone surrogate. The TS port, however, runs on JavaScript
//! strings (UTF-16) where unpaired surrogates are routinely produced by
//! external sources and must be scrubbed before crossing API boundaries.
//!
//! For parity we expose two entry points:
//!
//! - [`sanitize_bytes`] accepts arbitrary byte input (typical when data
//!   originates from FFI, raw network frames, or `unsafe` decoders) and
//!   returns a clean UTF-8 `String`. The walk recognizes the WTF-8
//!   encoding of UTF-16 surrogates (`0xED 0xA0 0x80 ..= 0xED 0xBF 0xBF`)
//!   in addition to ordinary invalid UTF-8 sequences.
//! - [`sanitize`] is the `&str` convenience wrapper. Because Rust already
//!   guarantees the input is well-formed UTF-8 it always returns the input
//!   unchanged via `Cow::Borrowed`; it exists only for API parity with the
//!   TS callers.

use std::borrow::Cow;

const REPLACEMENT: char = '\u{FFFD}';

/// Sanitize a byte slice to valid UTF-8, replacing lone UTF-16 surrogates
/// and any other malformed sequences with U+FFFD.
pub fn sanitize_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        // Detect WTF-8-encoded UTF-16 surrogates (U+D800..=U+DFFF):
        //   0xED 0xA0..=0xBF 0x80..=0xBF
        // `std::str::from_utf8` already rejects these, but we want to
        // collapse the whole 3-byte sequence into a single replacement
        // rather than three.
        if i + 3 <= bytes.len()
            && bytes[i] == 0xED
            && (0xA0..=0xBF).contains(&bytes[i + 1])
            && (0x80..=0xBF).contains(&bytes[i + 2])
        {
            out.push(REPLACEMENT);
            i += 3;
            continue;
        }

        match std::str::from_utf8(&bytes[i..]) {
            Ok(rest) => {
                out.push_str(rest);
                break;
            }
            Err(err) => {
                let valid_up_to = err.valid_up_to();
                if valid_up_to > 0 {
                    // Safety: `from_utf8` already validated this prefix.
                    let ok = unsafe { std::str::from_utf8_unchecked(&bytes[i..i + valid_up_to]) };
                    out.push_str(ok);
                    i += valid_up_to;
                    continue;
                }
                // Invalid sequence at the current position. Emit one
                // replacement and skip the offending bytes (default to 1
                // when the decoder cannot tell us the error length).
                let skip = err.error_len().unwrap_or(1).max(1);
                out.push(REPLACEMENT);
                i += skip;
            }
        }
    }

    out
}

/// Convenience for `&str` input. Always returns the input unchanged because
/// Rust guarantees `&str` is well-formed UTF-8. Kept for parity with the TS
/// `sanitizeSurrogates` helper.
pub fn sanitize(s: &str) -> Cow<'_, str> {
    Cow::Borrowed(s)
}
