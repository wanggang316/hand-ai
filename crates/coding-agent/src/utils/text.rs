//! Text-input normalization shared by the config and credential readers.

/// Drop a leading byte-order mark.
///
/// Editors on Windows routinely prefix UTF-8 files with U+FEFF, and a
/// file that round-trips through a shell redirect picks one up too.
/// Rust's `read_to_string` hands it back as an ordinary character, and
/// `serde_json` then rejects the whole document over a character that
/// appears before it starts.
///
/// This is specifically for JSON. YAML's spec allows a mark at the start
/// of a stream and `serde_yaml` honors that, so the YAML config layer
/// needs no stripping — a test next to that loader pins the difference.
///
/// Applied to files whose *content is parsed*, not to arbitrary text. A
/// mark inside a file the user is editing is theirs to keep — the edit
/// tool deliberately strips it for matching and writes it back
/// afterwards.
pub fn strip_bom(content: &str) -> &str {
    content.strip_prefix('\u{FEFF}').unwrap_or(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_a_leading_mark() {
        assert_eq!(strip_bom("\u{FEFF}model: gpt-5"), "model: gpt-5");
    }

    /// Only the leading one. A mark further in is content — it is a legal
    /// zero-width no-break space and removing it would corrupt the value
    /// that holds it.
    #[test]
    fn leaves_marks_that_are_not_leading() {
        assert_eq!(strip_bom("model: a\u{FEFF}b"), "model: a\u{FEFF}b");
    }

    /// Exactly one, so a file that somehow carries two keeps the second
    /// and still fails loudly rather than being silently half-repaired.
    #[test]
    fn strips_exactly_one_mark() {
        assert_eq!(strip_bom("\u{FEFF}\u{FEFF}x"), "\u{FEFF}x");
    }

    #[test]
    fn passes_ordinary_text_through_untouched() {
        assert_eq!(strip_bom(""), "");
        assert_eq!(strip_bom("model: gpt-5"), "model: gpt-5");
    }
}
