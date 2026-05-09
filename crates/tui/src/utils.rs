//! Text utilities for terminal rendering.
//!
//! Grapheme-aware visible width, wrapping that preserves SGR + OSC 8 hyperlinks
//! across line breaks, truncation with ellipsis, and ANSI-aware column slicing.
//!
//! Ported from `pi-mono/packages/tui/src/utils.ts`. Internal helper layout is
//! Rust-idiomatic; the public API mirrors the TypeScript surface.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

// ---------------------------------------------------------------------------
// Grapheme width
// ---------------------------------------------------------------------------

/// Display width of a single grapheme cluster in terminal columns.
///
/// Treats isolated regional indicators (U+1F1E6..=U+1F1FF) as width 2 to match
/// the typical terminal rendering and avoid auto-wrap drift artifacts. Other
/// clusters use the first base codepoint's east-asian width; ZWJ-joined emoji
/// take the width of their leading codepoint, mirroring the TS port.
fn grapheme_width(cluster: &str) -> usize {
    let mut chars = cluster.chars();
    let Some(first) = chars.next() else {
        return 0;
    };
    let cp = first as u32;

    // Regional indicator → always width 2 (single or paired flag).
    if (0x1F1E6..=0x1F1FF).contains(&cp) {
        return 2;
    }

    // Pure zero-width cluster (controls, default-ignorable, marks).
    if cluster.chars().all(is_zero_width_codepoint) {
        return 0;
    }

    // Find the first non-zero-width codepoint and use its width as the
    // grapheme width. This matches the behavior we want for ZWJ-joined emoji
    // (the base emoji's width carries the cluster) and keeps Thai/Lao AM
    // clusters at their normal cell width (the AM combining mark contributes 0,
    // and the base consonant contributes 1).
    let mut chars = cluster.chars();
    let mut width = 0;
    for c in chars.by_ref() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if w > 0 {
            width = w;
            break;
        }
    }

    // Trailing halfwidth/fullwidth forms occasionally segment with a base,
    // and Thai/Lao SARA AM (U+0E33 / U+0EB3) are additive when they trail a
    // base consonant within the same cluster.
    for c in chars {
        let cp = c as u32;
        if (0xFF00..=0xFFEF).contains(&cp) {
            width += UnicodeWidthChar::width(c).unwrap_or(0);
        } else if cp == 0x0E33 || cp == 0x0EB3 {
            width += 1;
        }
    }

    width
}

fn is_zero_width_codepoint(c: char) -> bool {
    if c.is_control() {
        return true;
    }
    let cp = c as u32;
    // Default ignorable + marks. unicode-width returns 0 for marks already,
    // but is_control() doesn't catch zero-width joiner / VS16 etc. These
    // ranges cover the common cases without pulling in a full Unicode db.
    matches!(
        cp,
        0x00AD                                  // soft hyphen
        | 0x034F                                // CGJ
        | 0x061C                                // ALM
        | 0x115F | 0x1160                       // hangul fillers
        | 0x17B4 | 0x17B5
        | 0x180B..=0x180E
        | 0x200B..=0x200F                       // ZWSP, ZWNJ, ZWJ, LRM, RLM
        | 0x202A..=0x202E
        | 0x2060..=0x206F
        | 0x3164
        | 0xFE00..=0xFE0F                       // VS1..VS16
        | 0xFEFF
        | 0xFFA0
        | 0xFFF0..=0xFFF8
        | 0x1D173..=0x1D17A
        | 0xE0000..=0xE007F                     // tag chars
        | 0xE0100..=0xE01EF                     // VS17..VS256
    ) || UnicodeWidthChar::width(c).unwrap_or(1) == 0
}

// ---------------------------------------------------------------------------
// Visible width
// ---------------------------------------------------------------------------

fn is_printable_ascii(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7E).contains(&b))
}

/// Visible width of `s` in terminal columns.
///
/// ANSI escape sequences (CSI, OSC, APC) and OSC 8 hyperlinks contribute zero.
/// Tabs are counted as 3 columns to match the TS port.
pub fn visible_width(s: &str) -> usize {
    if s.is_empty() {
        return 0;
    }

    if is_printable_ascii(s) {
        return s.len();
    }

    let bytes = s.as_bytes();
    let has_ansi = bytes.contains(&0x1b);
    let has_tabs = bytes.contains(&b'\t');

    if !has_ansi && !has_tabs {
        return s.graphemes(true).map(grapheme_width).sum();
    }

    // Strip ANSI / OSC / APC escape sequences in one pass and convert tabs to
    // 3 spaces, then sum grapheme widths.
    let mut clean = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if let Some((_code, len)) = extract_ansi_code_at(s, i) {
            i += len;
            continue;
        }
        let rest = &s[i..];
        let ch = rest.chars().next().expect("non-empty remainder");
        if ch == '\t' {
            clean.push_str("   ");
        } else {
            clean.push(ch);
        }
        i += ch.len_utf8();
    }

    clean.graphemes(true).map(grapheme_width).sum()
}

// ---------------------------------------------------------------------------
// ANSI extraction
// ---------------------------------------------------------------------------

/// Extract a single ANSI/OSC/APC escape sequence starting at byte position
/// `pos`. Returns `(code, byte_length)` if a valid sequence is parsed.
///
/// Handles:
/// - CSI sequences: `ESC [ ... <final>` where final is one of `m G K H J`
/// - OSC sequences: `ESC ] ... BEL` or `ESC ] ... ESC \` (used by OSC 8
///   hyperlinks and OSC 133 prompt markers)
/// - APC sequences: `ESC _ ... BEL` or `ESC _ ... ESC \` (cursor markers)
pub fn extract_ansi_code(s: &str, pos: usize) -> Option<(String, usize)> {
    extract_ansi_code_at(s, pos).map(|(slice, len)| (slice.to_string(), len))
}

/// Internal zero-allocation version returning a borrowed slice.
fn extract_ansi_code_at(s: &str, pos: usize) -> Option<(&str, usize)> {
    let bytes = s.as_bytes();
    if pos >= bytes.len() || bytes[pos] != 0x1b {
        return None;
    }
    if pos + 1 >= bytes.len() {
        return None;
    }
    let next = bytes[pos + 1];

    // CSI: ESC [ ... <m|G|K|H|J>
    if next == b'[' {
        let mut j = pos + 2;
        while j < bytes.len() {
            let b = bytes[j];
            if matches!(b, b'm' | b'G' | b'K' | b'H' | b'J') {
                let end = j + 1;
                return Some((&s[pos..end], end - pos));
            }
            j += 1;
        }
        return None;
    }

    // OSC: ESC ] ... <BEL | ESC \>
    if next == b']' {
        let mut j = pos + 2;
        while j < bytes.len() {
            if bytes[j] == 0x07 {
                let end = j + 1;
                return Some((&s[pos..end], end - pos));
            }
            if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == b'\\' {
                let end = j + 2;
                return Some((&s[pos..end], end - pos));
            }
            j += 1;
        }
        return None;
    }

    // APC: ESC _ ... <BEL | ESC \>
    if next == b'_' {
        let mut j = pos + 2;
        while j < bytes.len() {
            if bytes[j] == 0x07 {
                let end = j + 1;
                return Some((&s[pos..end], end - pos));
            }
            if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == b'\\' {
                let end = j + 2;
                return Some((&s[pos..end], end - pos));
            }
            j += 1;
        }
        return None;
    }

    None
}

// ---------------------------------------------------------------------------
// Strip ANSI / strip helpers
// ---------------------------------------------------------------------------

/// Remove all ANSI/OSC/APC escape sequences from `s`.
pub fn strip_ansi(s: &str) -> String {
    if !s.contains('\x1b') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if let Some((_, len)) = extract_ansi_code_at(s, i) {
            i += len;
            continue;
        }
        let ch = s[i..].chars().next().expect("non-empty");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

// ---------------------------------------------------------------------------
// Whitespace / punctuation classification
// ---------------------------------------------------------------------------

/// Whitespace classification used by wrap logic. Mirrors the TS `\s` regex.
pub fn is_whitespace_char(c: char) -> bool {
    c.is_whitespace()
}

const PUNCTUATION_CHARS: &str = "(){}[]<>.,;:'\"!?+-=*/\\|&%^$#@~`";

/// Punctuation classification used by wrap logic. Mirrors the TS punctuation
/// regex: `[(){}[\]<>.,;:'"!?+\-=*/\\|&%^$#@~`]`.
pub fn is_punctuation_char(c: char) -> bool {
    PUNCTUATION_CHARS.contains(c)
}

// ---------------------------------------------------------------------------
// OSC 8 hyperlinks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Osc8Terminator {
    Bel,
    St,
}

impl Osc8Terminator {
    fn as_str(self) -> &'static str {
        match self {
            Self::Bel => "\x07",
            Self::St => "\x1b\\",
        }
    }
}

#[derive(Debug, Clone)]
struct ActiveHyperlink {
    params: String,
    url: String,
    terminator: Osc8Terminator,
}

/// Parse an OSC 8 hyperlink open or close.
///
/// Returns:
/// - `Some(Some(link))` for an open with a non-empty URL
/// - `Some(None)` for an explicit close (`ESC ] 8 ; ; ST`)
/// - `None` if the code is not an OSC 8 hyperlink
fn parse_osc8(code: &str) -> Option<Option<ActiveHyperlink>> {
    let prefix = "\x1b]8;";
    if !code.starts_with(prefix) {
        return None;
    }
    let (body, terminator) = if let Some(stripped) = code.strip_suffix('\x07') {
        (&stripped[prefix.len()..], Osc8Terminator::Bel)
    } else if let Some(stripped) = code.strip_suffix("\x1b\\") {
        (&stripped[prefix.len()..], Osc8Terminator::St)
    } else {
        return None;
    };
    let sep = body.find(';')?;
    let params = &body[..sep];
    let url = &body[sep + 1..];
    if url.is_empty() {
        return Some(None);
    }
    Some(Some(ActiveHyperlink {
        params: params.to_string(),
        url: url.to_string(),
        terminator,
    }))
}

fn format_osc8_open(link: &ActiveHyperlink) -> String {
    format!(
        "\x1b]8;{};{}{}",
        link.params,
        link.url,
        link.terminator.as_str()
    )
}

fn format_osc8_close(terminator: Osc8Terminator) -> String {
    format!("\x1b]8;;{}", terminator.as_str())
}

// ---------------------------------------------------------------------------
// SGR + hyperlink tracker
// ---------------------------------------------------------------------------

/// Tracks active SGR attributes and OSC 8 hyperlinks so styling can be
/// re-emitted at the start of each wrapped line.
#[derive(Debug, Default)]
struct AnsiCodeTracker {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    blink: bool,
    inverse: bool,
    hidden: bool,
    strikethrough: bool,
    fg_color: Option<String>,
    bg_color: Option<String>,
    active_hyperlink: Option<ActiveHyperlink>,
}

impl AnsiCodeTracker {
    fn new() -> Self {
        Self::default()
    }

    #[allow(dead_code)]
    fn clear(&mut self) {
        self.reset_sgr();
        self.active_hyperlink = None;
    }

    #[allow(dead_code)]
    fn has_active(&self) -> bool {
        self.bold
            || self.dim
            || self.italic
            || self.underline
            || self.blink
            || self.inverse
            || self.hidden
            || self.strikethrough
            || self.fg_color.is_some()
            || self.bg_color.is_some()
            || self.active_hyperlink.is_some()
    }

    fn reset_sgr(&mut self) {
        self.bold = false;
        self.dim = false;
        self.italic = false;
        self.underline = false;
        self.blink = false;
        self.inverse = false;
        self.hidden = false;
        self.strikethrough = false;
        self.fg_color = None;
        self.bg_color = None;
        // SGR reset does not affect OSC 8 hyperlink state.
    }

    fn process(&mut self, code: &str) {
        // OSC 8 hyperlink first.
        match parse_osc8(code) {
            Some(Some(link)) => {
                self.active_hyperlink = Some(link);
                return;
            }
            Some(None) => {
                self.active_hyperlink = None;
                return;
            }
            None => {}
        }

        // Only SGR codes (ending with 'm') are tracked.
        if !code.ends_with('m') {
            return;
        }
        // Strip leading "\x1b[" and trailing "m".
        if !code.starts_with("\x1b[") {
            return;
        }
        let body = &code[2..code.len() - 1];
        if body.is_empty() || body == "0" {
            self.reset_sgr();
            return;
        }
        let parts: Vec<&str> = body.split(';').collect();
        let mut i = 0;
        while i < parts.len() {
            let Ok(num) = parts[i].parse::<i32>() else {
                i += 1;
                continue;
            };
            if num == 38 || num == 48 {
                // Extended color: ;5;N (256-color) or ;2;R;G;B (truecolor).
                if i + 2 < parts.len() && parts[i + 1] == "5" {
                    let color = format!("{};{};{}", parts[i], parts[i + 1], parts[i + 2]);
                    if num == 38 {
                        self.fg_color = Some(color);
                    } else {
                        self.bg_color = Some(color);
                    }
                    i += 3;
                    continue;
                } else if i + 4 < parts.len() && parts[i + 1] == "2" {
                    let color = format!(
                        "{};{};{};{};{}",
                        parts[i],
                        parts[i + 1],
                        parts[i + 2],
                        parts[i + 3],
                        parts[i + 4]
                    );
                    if num == 38 {
                        self.fg_color = Some(color);
                    } else {
                        self.bg_color = Some(color);
                    }
                    i += 5;
                    continue;
                }
            }

            match num {
                0 => self.reset_sgr(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => self.underline = true,
                5 => self.blink = true,
                7 => self.inverse = true,
                8 => self.hidden = true,
                9 => self.strikethrough = true,
                21 => self.bold = false,
                22 => {
                    self.bold = false;
                    self.dim = false;
                }
                23 => self.italic = false,
                24 => self.underline = false,
                25 => self.blink = false,
                27 => self.inverse = false,
                28 => self.hidden = false,
                29 => self.strikethrough = false,
                39 => self.fg_color = None,
                49 => self.bg_color = None,
                n if (30..=37).contains(&n) || (90..=97).contains(&n) => {
                    self.fg_color = Some(n.to_string());
                }
                n if (40..=47).contains(&n) || (100..=107).contains(&n) => {
                    self.bg_color = Some(n.to_string());
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn active_codes(&self) -> String {
        let mut codes: Vec<String> = Vec::new();
        if self.bold {
            codes.push("1".to_string());
        }
        if self.dim {
            codes.push("2".to_string());
        }
        if self.italic {
            codes.push("3".to_string());
        }
        if self.underline {
            codes.push("4".to_string());
        }
        if self.blink {
            codes.push("5".to_string());
        }
        if self.inverse {
            codes.push("7".to_string());
        }
        if self.hidden {
            codes.push("8".to_string());
        }
        if self.strikethrough {
            codes.push("9".to_string());
        }
        if let Some(c) = &self.fg_color {
            codes.push(c.clone());
        }
        if let Some(c) = &self.bg_color {
            codes.push(c.clone());
        }
        let mut result = if codes.is_empty() {
            String::new()
        } else {
            format!("\x1b[{}m", codes.join(";"))
        };
        if let Some(link) = &self.active_hyperlink {
            result.push_str(&format_osc8_open(link));
        }
        result
    }

    /// Reset codes that must be closed at line end. Underline is closed to
    /// prevent it bleeding into padding; OSC 8 hyperlinks must be closed and
    /// re-opened on the next line.
    fn line_end_reset(&self) -> String {
        let mut out = String::new();
        if self.underline {
            out.push_str("\x1b[24m");
        }
        if let Some(link) = &self.active_hyperlink {
            out.push_str(&format_osc8_close(link.terminator));
        }
        out
    }
}

fn update_tracker_from_text(text: &str, tracker: &mut AnsiCodeTracker) {
    let mut i = 0;
    while i < text.len() {
        if let Some((code, len)) = extract_ansi_code_at(text, i) {
            tracker.process(code);
            i += len;
        } else {
            // Advance by one full char to preserve UTF-8 boundaries.
            let ch = text[i..].chars().next().expect("non-empty remainder");
            i += ch.len_utf8();
        }
    }
}

// ---------------------------------------------------------------------------
// Tokenizer for wrap
// ---------------------------------------------------------------------------

/// Split `text` into tokens where each token is either a run of whitespace
/// or a run of non-whitespace, with ANSI codes attached to the *next* visible
/// character.
fn split_into_tokens_with_ansi(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut pending_ansi = String::new();
    let mut in_whitespace = false;
    let mut i = 0;

    while i < text.len() {
        if let Some((code, len)) = extract_ansi_code_at(text, i) {
            pending_ansi.push_str(code);
            i += len;
            continue;
        }
        let ch = text[i..].chars().next().expect("non-empty remainder");
        let ch_is_space = ch == ' ';

        if ch_is_space != in_whitespace && !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }

        if !pending_ansi.is_empty() {
            current.push_str(&pending_ansi);
            pending_ansi.clear();
        }

        in_whitespace = ch_is_space;
        current.push(ch);
        i += ch.len_utf8();
    }

    if !pending_ansi.is_empty() {
        current.push_str(&pending_ansi);
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

// ---------------------------------------------------------------------------
// Wrapping
// ---------------------------------------------------------------------------

/// Wrap `text` to `width` visible columns, preserving SGR attributes and
/// OSC 8 hyperlinks across wrapped lines.
///
/// - Word wrapping only; does not apply padding or background colors.
/// - Lines are not padded to `width`; each line has visible width <= `width`.
/// - Active styling carries over: each continuation line begins with the
///   current SGR codes and re-opens the active OSC 8 hyperlink (if any).
/// - Underline is closed at line end to avoid bleeding into padding when the
///   caller right-pads the line.
pub fn wrap_text_with_ansi(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut result: Vec<String> = Vec::new();
    let mut tracker = AnsiCodeTracker::new();

    for input_line in text.split('\n') {
        let prefix = if !result.is_empty() {
            tracker.active_codes()
        } else {
            String::new()
        };
        let combined = if prefix.is_empty() {
            input_line.to_string()
        } else {
            format!("{prefix}{input_line}")
        };
        result.extend(wrap_single_line(&combined, width));
        update_tracker_from_text(input_line, &mut tracker);
    }

    if result.is_empty() {
        vec![String::new()]
    } else {
        result
    }
}

fn wrap_single_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }

    if visible_width(line) <= width {
        return vec![line.to_string()];
    }

    let mut wrapped: Vec<String> = Vec::new();
    let mut tracker = AnsiCodeTracker::new();
    let tokens = split_into_tokens_with_ansi(line);

    let mut current_line = String::new();
    let mut current_visible = 0usize;

    for token in &tokens {
        let token_visible = visible_width(token);
        let is_whitespace = strip_ansi(token).trim().is_empty();

        // Token alone exceeds width and is not whitespace → break by graphemes.
        if token_visible > width && !is_whitespace {
            if !current_line.is_empty() {
                let line_end_reset = tracker.line_end_reset();
                if !line_end_reset.is_empty() {
                    current_line.push_str(&line_end_reset);
                }
                wrapped.push(std::mem::take(&mut current_line));
                current_visible = 0;
            }

            let broken = break_long_word(token, width, &mut tracker);
            if broken.len() > 1 {
                let last = broken.last().cloned().unwrap_or_default();
                for piece in broken.iter().take(broken.len() - 1) {
                    wrapped.push(piece.clone());
                }
                current_visible = visible_width(&last);
                current_line = last;
            } else if let Some(only) = broken.into_iter().next() {
                current_visible = visible_width(&only);
                current_line = only;
            }
            continue;
        }

        let total_needed = current_visible + token_visible;
        if total_needed > width && current_visible > 0 {
            // Trim trailing whitespace, then close underline / hyperlink at
            // line end so padding does not inherit them.
            let mut line_to_wrap = trim_end_inplace(&current_line);
            let line_end_reset = tracker.line_end_reset();
            if !line_end_reset.is_empty() {
                line_to_wrap.push_str(&line_end_reset);
            }
            wrapped.push(line_to_wrap);
            if is_whitespace {
                current_line = tracker.active_codes();
                current_visible = 0;
            } else {
                current_line = tracker.active_codes();
                current_line.push_str(token);
                current_visible = token_visible;
            }
        } else {
            current_line.push_str(token);
            current_visible += token_visible;
        }

        update_tracker_from_text(token, &mut tracker);
    }

    if !current_line.is_empty() {
        wrapped.push(current_line);
    }

    if wrapped.is_empty() {
        vec![String::new()]
    } else {
        wrapped
            .into_iter()
            .map(|line| trim_end_inplace(&line))
            .collect()
    }
}

/// Trim trailing ASCII whitespace from a line, treating the value by-copy.
/// Preserves any non-whitespace UTF-8 content (we only trim ` `, `\t`).
fn trim_end_inplace(s: &str) -> String {
    let mut bytes_end = s.len();
    let bytes = s.as_bytes();
    while bytes_end > 0 {
        let last = bytes[bytes_end - 1];
        if last == b' ' || last == b'\t' {
            bytes_end -= 1;
        } else {
            break;
        }
    }
    s[..bytes_end].to_string()
}

fn break_long_word(word: &str, width: usize, tracker: &mut AnsiCodeTracker) -> Vec<String> {
    enum Seg<'a> {
        Ansi(&'a str),
        Grapheme(&'a str),
    }

    let mut segments: Vec<Seg> = Vec::new();
    let mut i = 0;
    while i < word.len() {
        if let Some((code, len)) = extract_ansi_code_at(word, i) {
            segments.push(Seg::Ansi(code));
            i += len;
        } else {
            let mut end = i;
            while end < word.len() {
                if extract_ansi_code_at(word, end).is_some() {
                    break;
                }
                let ch = word[end..].chars().next().expect("non-empty remainder");
                end += ch.len_utf8();
            }
            for g in word[i..end].graphemes(true) {
                segments.push(Seg::Grapheme(g));
            }
            i = end;
        }
    }

    let mut lines: Vec<String> = Vec::new();
    let mut current_line = tracker.active_codes();
    let mut current_width = 0usize;

    for seg in segments {
        match seg {
            Seg::Ansi(code) => {
                current_line.push_str(code);
                tracker.process(code);
            }
            Seg::Grapheme(g) => {
                if g.is_empty() {
                    continue;
                }
                let gw = grapheme_width(g);
                if current_width + gw > width {
                    let line_end_reset = tracker.line_end_reset();
                    if !line_end_reset.is_empty() {
                        current_line.push_str(&line_end_reset);
                    }
                    lines.push(std::mem::take(&mut current_line));
                    current_line = tracker.active_codes();
                    current_width = 0;
                }
                current_line.push_str(g);
                current_width += gw;
            }
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

// ---------------------------------------------------------------------------
// Truncation
// ---------------------------------------------------------------------------

/// Truncate `s` to a maximum visible width, appending an ellipsis (`…`) when
/// truncation occurs. Uses default ellipsis "…" and no padding to preserve
/// the existing call-site contract in this crate. For full TS-port behavior
/// see [`truncate_to_width_with`].
pub fn truncate_to_width(s: &str, max_width: usize) -> String {
    truncate_to_width_with(s, max_width, "…", false)
}

/// Full TS-port of `truncateToWidth(text, maxWidth, ellipsis, pad)`.
///
/// - `ellipsis` is appended only when truncation occurs.
/// - `pad = true` right-pads the result with spaces to exactly `max_width`.
/// - Truncation always brackets the ellipsis with `\x1b[0m` resets so the
///   ellipsis itself is unstyled and styling does not bleed into following
///   content.
pub fn truncate_to_width_with(text: &str, max_width: usize, ellipsis: &str, pad: bool) -> String {
    if max_width == 0 {
        return String::new();
    }
    if text.is_empty() {
        return if pad {
            " ".repeat(max_width)
        } else {
            String::new()
        };
    }

    let ellipsis_width = visible_width(ellipsis);
    if ellipsis_width >= max_width {
        let text_width = visible_width(text);
        if text_width <= max_width {
            return if pad {
                let mut out = text.to_string();
                out.push_str(&" ".repeat(max_width - text_width));
                out
            } else {
                text.to_string()
            };
        }
        let clipped = truncate_fragment_to_width(ellipsis, max_width);
        if clipped.width == 0 {
            return if pad {
                " ".repeat(max_width)
            } else {
                String::new()
            };
        }
        return finalize_truncated("", 0, &clipped.text, clipped.width, max_width, pad);
    }

    if is_printable_ascii(text) {
        if text.len() <= max_width {
            return if pad {
                let mut out = text.to_string();
                out.push_str(&" ".repeat(max_width - text.len()));
                out
            } else {
                text.to_string()
            };
        }
        let target_width = max_width - ellipsis_width;
        return finalize_truncated(
            &text[..target_width],
            target_width,
            ellipsis,
            ellipsis_width,
            max_width,
            pad,
        );
    }

    let target_width = max_width.saturating_sub(ellipsis_width);
    let mut result = String::new();
    let mut pending_ansi = String::new();
    let mut visible_so_far = 0usize;
    let mut kept_width = 0usize;
    let mut keep_contiguous_prefix = true;
    let mut overflowed = false;
    let has_ansi = text.contains('\x1b');
    let has_tabs = text.contains('\t');

    if !has_ansi && !has_tabs {
        for g in text.graphemes(true) {
            let w = grapheme_width(g);
            if keep_contiguous_prefix && kept_width + w <= target_width {
                result.push_str(g);
                kept_width += w;
            } else {
                keep_contiguous_prefix = false;
            }
            visible_so_far += w;
            if visible_so_far > max_width {
                overflowed = true;
                break;
            }
        }
        let exhausted_input = !overflowed;
        if !overflowed && exhausted_input {
            return if pad {
                let mut out = text.to_string();
                out.push_str(&" ".repeat(max_width.saturating_sub(visible_so_far)));
                out
            } else {
                text.to_string()
            };
        }
    } else {
        let mut i = 0;
        let bytes = text.as_bytes();
        let mut break_outer = false;
        while i < bytes.len() && !break_outer {
            if let Some((code, len)) = extract_ansi_code_at(text, i) {
                pending_ansi.push_str(code);
                i += len;
                continue;
            }
            if bytes[i] == b'\t' {
                if keep_contiguous_prefix && kept_width + 3 <= target_width {
                    if !pending_ansi.is_empty() {
                        result.push_str(&pending_ansi);
                        pending_ansi.clear();
                    }
                    result.push('\t');
                    kept_width += 3;
                } else {
                    keep_contiguous_prefix = false;
                    pending_ansi.clear();
                }
                visible_so_far += 3;
                if visible_so_far > max_width {
                    overflowed = true;
                    break;
                }
                i += 1;
                continue;
            }

            let mut end = i;
            while end < bytes.len() && bytes[end] != b'\t' {
                if extract_ansi_code_at(text, end).is_some() {
                    break;
                }
                let ch = text[end..].chars().next().expect("non-empty remainder");
                end += ch.len_utf8();
            }

            for g in text[i..end].graphemes(true) {
                let w = grapheme_width(g);
                if keep_contiguous_prefix && kept_width + w <= target_width {
                    if !pending_ansi.is_empty() {
                        result.push_str(&pending_ansi);
                        pending_ansi.clear();
                    }
                    result.push_str(g);
                    kept_width += w;
                } else {
                    keep_contiguous_prefix = false;
                    pending_ansi.clear();
                }
                visible_so_far += w;
                if visible_so_far > max_width {
                    overflowed = true;
                    break_outer = true;
                    break;
                }
            }
            i = end;
        }
        let exhausted_input = i >= bytes.len();
        if !overflowed && exhausted_input {
            return if pad {
                let mut out = text.to_string();
                out.push_str(&" ".repeat(max_width.saturating_sub(visible_so_far)));
                out
            } else {
                text.to_string()
            };
        }
    }

    finalize_truncated(
        &result,
        kept_width,
        ellipsis,
        ellipsis_width,
        max_width,
        pad,
    )
}

struct ClippedFragment {
    text: String,
    width: usize,
}

fn truncate_fragment_to_width(text: &str, max_width: usize) -> ClippedFragment {
    if max_width == 0 || text.is_empty() {
        return ClippedFragment {
            text: String::new(),
            width: 0,
        };
    }

    if is_printable_ascii(text) {
        let take = text.len().min(max_width);
        let clipped = &text[..take];
        return ClippedFragment {
            text: clipped.to_string(),
            width: clipped.len(),
        };
    }

    let has_ansi = text.contains('\x1b');
    let has_tabs = text.contains('\t');
    if !has_ansi && !has_tabs {
        let mut result = String::new();
        let mut width = 0usize;
        for g in text.graphemes(true) {
            let w = grapheme_width(g);
            if width + w > max_width {
                break;
            }
            result.push_str(g);
            width += w;
        }
        return ClippedFragment {
            text: result,
            width,
        };
    }

    let mut result = String::new();
    let mut width = 0usize;
    let mut i = 0;
    let bytes = text.as_bytes();
    let mut pending_ansi = String::new();
    let mut break_outer = false;
    while i < bytes.len() && !break_outer {
        if let Some((code, len)) = extract_ansi_code_at(text, i) {
            pending_ansi.push_str(code);
            i += len;
            continue;
        }
        if bytes[i] == b'\t' {
            if width + 3 > max_width {
                break;
            }
            if !pending_ansi.is_empty() {
                result.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            result.push('\t');
            width += 3;
            i += 1;
            continue;
        }

        let mut end = i;
        while end < bytes.len() && bytes[end] != b'\t' {
            if extract_ansi_code_at(text, end).is_some() {
                break;
            }
            let ch = text[end..].chars().next().expect("non-empty remainder");
            end += ch.len_utf8();
        }

        for g in text[i..end].graphemes(true) {
            let w = grapheme_width(g);
            if width + w > max_width {
                break_outer = true;
                break;
            }
            if !pending_ansi.is_empty() {
                result.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            result.push_str(g);
            width += w;
        }
        i = end;
    }
    ClippedFragment {
        text: result,
        width,
    }
}

fn finalize_truncated(
    prefix: &str,
    prefix_width: usize,
    ellipsis: &str,
    ellipsis_width: usize,
    max_width: usize,
    pad: bool,
) -> String {
    let reset = "\x1b[0m";
    let visible = prefix_width + ellipsis_width;
    let mut result = String::with_capacity(prefix.len() + ellipsis.len() + 8);
    result.push_str(prefix);
    result.push_str(reset);
    if !ellipsis.is_empty() {
        result.push_str(ellipsis);
        result.push_str(reset);
    }
    if pad {
        result.push_str(&" ".repeat(max_width.saturating_sub(visible)));
    }
    result
}

// ---------------------------------------------------------------------------
// Column slicing
// ---------------------------------------------------------------------------

/// Result of [`slice_with_width`]: the sliced text and its visible width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlicedSegment {
    pub text: String,
    pub width: usize,
}

/// Extract a range of visible columns from `line`, preserving ANSI codes and
/// honoring wide-character boundaries.
///
/// `strict = true` excludes a wide grapheme that would extend past the end of
/// the requested range.
pub fn slice_by_column(line: &str, start_col: usize, length: usize, strict: bool) -> String {
    slice_with_width(line, start_col, length, strict).text
}

/// Like [`slice_by_column`] but also returns the visible width of the slice.
pub fn slice_with_width(
    line: &str,
    start_col: usize,
    length: usize,
    strict: bool,
) -> SlicedSegment {
    if length == 0 {
        return SlicedSegment {
            text: String::new(),
            width: 0,
        };
    }
    let end_col = start_col + length;
    let mut result = String::new();
    let mut result_width = 0usize;
    let mut current_col = 0usize;
    let mut i = 0;
    let mut pending_ansi = String::new();
    let bytes = line.as_bytes();
    let mut done = false;

    while i < bytes.len() && !done {
        if let Some((code, len)) = extract_ansi_code_at(line, i) {
            if current_col >= start_col && current_col < end_col {
                result.push_str(code);
            } else if current_col < start_col {
                pending_ansi.push_str(code);
            }
            i += len;
            continue;
        }

        let mut text_end = i;
        while text_end < bytes.len() {
            if extract_ansi_code_at(line, text_end).is_some() {
                break;
            }
            let ch = line[text_end..]
                .chars()
                .next()
                .expect("non-empty remainder");
            text_end += ch.len_utf8();
        }

        for g in line[i..text_end].graphemes(true) {
            let w = grapheme_width(g);
            let in_range = current_col >= start_col && current_col < end_col;
            let fits = !strict || current_col + w <= end_col;
            if in_range && fits {
                if !pending_ansi.is_empty() {
                    result.push_str(&pending_ansi);
                    pending_ansi.clear();
                }
                result.push_str(g);
                result_width += w;
            }
            current_col += w;
            if current_col >= end_col {
                done = true;
                break;
            }
        }
        i = text_end;
    }

    SlicedSegment {
        text: result,
        width: result_width,
    }
}

// ---------------------------------------------------------------------------
// Background + padding
// ---------------------------------------------------------------------------

/// Apply a background color to `line`, padding on the right with spaces so the
/// total visible width equals `width`. The `bg` callback receives the padded
/// content and returns the styled string (typically wrapping it with SGR
/// background open + reset).
pub fn apply_background_to_line(line: &str, width: usize, bg: impl Fn(&str) -> String) -> String {
    let visible = visible_width(line);
    let padding_needed = width.saturating_sub(visible);
    let mut padded = line.to_string();
    if padding_needed > 0 {
        padded.push_str(&" ".repeat(padding_needed));
    }
    bg(&padded)
}

// ---------------------------------------------------------------------------
// extract_segments
// ---------------------------------------------------------------------------

/// Result of [`extract_segments`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExtractedSegments {
    pub before: String,
    pub before_width: usize,
    pub after: String,
    pub after_width: usize,
}

/// Extract "before" and "after" segments from a line in a single pass.
///
/// Used for overlay compositing: we need the text before the overlay and the
/// text after the overlay region. Styling state from before the overlay is
/// inherited into `after` so that styling continues correctly past the overlay.
pub fn extract_segments(
    line: &str,
    before_end: usize,
    after_start: usize,
    after_len: usize,
    strict_after: bool,
) -> ExtractedSegments {
    let mut out = ExtractedSegments::default();
    let mut current_col = 0usize;
    let mut i = 0usize;
    let mut pending_ansi_before = String::new();
    let mut after_started = false;
    let after_end = after_start + after_len;
    let bytes = line.as_bytes();

    let mut tracker = AnsiCodeTracker::new();

    let done = |col: usize| -> bool {
        if after_len == 0 {
            col >= before_end
        } else {
            col >= after_end
        }
    };

    while i < bytes.len() {
        if let Some((code, len)) = extract_ansi_code_at(line, i) {
            tracker.process(code);
            if current_col < before_end {
                pending_ansi_before.push_str(code);
            } else if current_col >= after_start && current_col < after_end && after_started {
                out.after.push_str(code);
            }
            i += len;
            continue;
        }

        let mut text_end = i;
        while text_end < bytes.len() {
            if extract_ansi_code_at(line, text_end).is_some() {
                break;
            }
            let ch = line[text_end..]
                .chars()
                .next()
                .expect("non-empty remainder");
            text_end += ch.len_utf8();
        }

        let mut early_break = false;
        for g in line[i..text_end].graphemes(true) {
            let w = grapheme_width(g);
            if current_col < before_end {
                if !pending_ansi_before.is_empty() {
                    out.before.push_str(&pending_ansi_before);
                    pending_ansi_before.clear();
                }
                out.before.push_str(g);
                out.before_width += w;
            } else if current_col >= after_start && current_col < after_end {
                let fits = !strict_after || current_col + w <= after_end;
                if fits {
                    if !after_started {
                        out.after.push_str(&tracker.active_codes());
                        after_started = true;
                    }
                    out.after.push_str(g);
                    out.after_width += w;
                }
            }
            current_col += w;
            if done(current_col) {
                early_break = true;
                break;
            }
        }
        i = text_end;
        if early_break {
            break;
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Normalize Thai/Lao AM vowels for terminal output
// ---------------------------------------------------------------------------

/// Normalize text for terminal output without changing logical content.
/// Decomposes Thai/Lao AM precomposed vowels (U+0E33, U+0EB3) into their
/// compatibility decompositions, which render more reliably in differential
/// repaint.
pub fn normalize_terminal_output(s: &str) -> String {
    if !s.chars().any(|c| c == '\u{0e33}' || c == '\u{0eb3}') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\u{0e33}' => {
                out.push('\u{0e4d}');
                out.push('\u{0e32}');
            }
            '\u{0eb3}' => {
                out.push('\u{0ecd}');
                out.push('\u{0eb2}');
            }
            other => out.push(other),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Legacy helpers preserved for in-crate callers
// ---------------------------------------------------------------------------

/// Display width of a single character. Convenience wrapper around
/// [`UnicodeWidthChar`].
pub fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Pad `text` on the right with spaces so its visible width equals
/// `target_width`. Returns the input unchanged if it already meets or exceeds
/// the target.
pub fn pad_to_width(text: &str, target_width: usize) -> String {
    let current = visible_width(text);
    if current >= target_width {
        return text.to_string();
    }
    format!("{}{}", text, " ".repeat(target_width - current))
}

/// Wrap text by visible columns (character-by-character; not word-aware).
///
/// This is a legacy helper used by simple components that do not need full
/// word-wrap behavior. For SGR/OSC-aware word wrap, use
/// [`wrap_text_with_ansi`].
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current_line = String::new();
    let mut current_width = 0usize;
    let mut i = 0;

    while i < text.len() {
        if let Some((code, len)) = extract_ansi_code_at(text, i) {
            current_line.push_str(code);
            i += len;
            continue;
        }
        let ch = text[i..].chars().next().expect("non-empty remainder");
        if ch == '\n' {
            lines.push(std::mem::take(&mut current_line));
            current_width = 0;
            i += ch.len_utf8();
            continue;
        }
        if ch.is_control() {
            i += ch.len_utf8();
            continue;
        }
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width + cw > width && current_width > 0 {
            lines.push(std::mem::take(&mut current_line));
            current_width = 0;
        }
        current_line.push(ch);
        current_width += cw;
        i += ch.len_utf8();
    }
    lines.push(current_line);
    lines
}

/// Apply a background color to a line, filling to `width` columns.
///
/// Legacy helper for callers that pre-format their `bg_code` and `reset` as
/// strings rather than supplying a closure. For closure-based usage prefer
/// [`apply_background_to_line`].
pub fn apply_background(line: &str, width: usize, bg_code: &str, reset: &str) -> String {
    let padded = pad_to_width(line, width);
    format!("{bg_code}{padded}{reset}")
}

/// Stub for paste-marker-aware grapheme segmentation. Currently yields plain
/// graphemes ungrouped.
//
// TODO(M3.T2): implement paste-marker handling for the editor; this should
// preserve OSC paste markers as zero-width markers around the pasted region
// so the editor can detect contiguous paste content.
pub fn segment_with_markers(s: &str) -> Vec<String> {
    s.graphemes(true).map(|g| g.to_string()).collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- visible_width -----------------------------------------------------

    #[test]
    fn visible_width_plain_ascii() {
        assert_eq!(visible_width(""), 0);
        assert_eq!(visible_width("hello"), 5);
        assert_eq!(visible_width("abc"), 3);
    }

    #[test]
    fn visible_width_skips_ansi() {
        assert_eq!(visible_width("\x1b[31mhello\x1b[0m"), 5);
        assert_eq!(visible_width("\x1b[1;32mtest\x1b[0m"), 4);
    }

    #[test]
    fn visible_width_wide_chars() {
        assert_eq!(visible_width("你好"), 4);
        assert_eq!(visible_width("a你好b"), 6);
    }

    #[test]
    fn visible_width_counts_tabs_and_skips_inline_ansi() {
        // Mirrors TS truncate-to-width.test.ts visibleWidth case.
        assert_eq!(visible_width("\t\x1b[31m界\x1b[0m"), 5);
    }

    #[test]
    fn visible_width_ignores_osc_133_bel() {
        assert_eq!(visible_width("\x1b]133;A\x07hello\x1b]133;B\x07"), 5);
    }

    #[test]
    fn visible_width_ignores_osc_133_st() {
        assert_eq!(visible_width("\x1b]133;A\x1b\\hello\x1b]133;B\x1b\\"), 5);
    }

    #[test]
    fn visible_width_regional_indicators() {
        // Single regional indicator and a flag both render as width 2.
        assert_eq!(visible_width("\u{1F1E8}"), 2);
        assert_eq!(visible_width("\u{1F1E8}\u{1F1F3}"), 2); // 🇨🇳
        assert_eq!(visible_width("\u{1F1EF}\u{1F1F5}"), 2); // 🇯🇵
    }

    #[test]
    fn visible_width_zwj_emoji_sequence() {
        // 👨‍👩‍👧 (man, ZWJ, woman, ZWJ, girl) → one cluster, width 2.
        assert_eq!(
            visible_width("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"),
            2
        );
    }

    #[test]
    fn visible_width_thai_lao_am() {
        assert_eq!(visible_width("ำ"), 1);
        assert_eq!(visible_width("ຳ"), 1);
        assert_eq!(visible_width("กำ"), 2);
        assert_eq!(visible_width("ກຳ"), 2);
    }

    #[test]
    fn visible_width_mixed_cjk_ascii_ansi() {
        assert_eq!(visible_width("\x1b[1m你好world\x1b[0m"), 9);
    }

    // --- extract_ansi_code -------------------------------------------------

    #[test]
    fn extract_ansi_csi() {
        let r = extract_ansi_code("\x1b[31mhi", 0).unwrap();
        assert_eq!(r.0, "\x1b[31m");
        assert_eq!(r.1, 5);
    }

    #[test]
    fn extract_ansi_osc_bel() {
        let r = extract_ansi_code("\x1b]8;;https://x\x07after", 0).unwrap();
        assert_eq!(r.0, "\x1b]8;;https://x\x07");
    }

    #[test]
    fn extract_ansi_osc_st() {
        let r = extract_ansi_code("\x1b]8;;https://x\x1b\\after", 0).unwrap();
        assert_eq!(r.0, "\x1b]8;;https://x\x1b\\");
    }

    #[test]
    fn extract_ansi_apc() {
        let r = extract_ansi_code("\x1b_marker\x07tail", 0).unwrap();
        assert_eq!(r.0, "\x1b_marker\x07");
    }

    #[test]
    fn extract_ansi_returns_none_for_plain() {
        assert!(extract_ansi_code("plain", 0).is_none());
    }

    // --- strip_ansi --------------------------------------------------------

    #[test]
    fn strip_ansi_basic() {
        assert_eq!(strip_ansi("\x1b[31mhello\x1b[0m"), "hello");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn strip_ansi_removes_osc8() {
        let s = "\x1b]8;;u\x1b\\link\x1b]8;;\x1b\\";
        assert_eq!(strip_ansi(s), "link");
    }

    // --- truncate_to_width -------------------------------------------------

    #[test]
    fn truncate_no_truncation_returns_input() {
        assert_eq!(truncate_to_width("hello", 10), "hello");
        assert_eq!(truncate_to_width("hello", 5), "hello");
    }

    #[test]
    fn truncate_zero_width() {
        assert_eq!(truncate_to_width("hello", 0), "");
    }

    #[test]
    fn truncate_appends_ellipsis_when_overflow() {
        let r = truncate_to_width("hello world", 6);
        assert!(r.contains('…'));
        assert!(visible_width(&strip_ansi(&r)) <= 6);
    }

    #[test]
    fn truncate_keeps_within_width_for_huge_unicode() {
        let text = "🙂界".repeat(100_000);
        let r = truncate_to_width_with(&text, 40, "…", false);
        assert!(visible_width(&r) <= 40);
        assert!(r.ends_with("…\x1b[0m"));
    }

    #[test]
    fn truncate_preserves_styling_with_resets() {
        let mut text = String::from("\x1b[31m");
        text.push_str(&"hello ".repeat(1000));
        text.push_str("\x1b[0m");
        let r = truncate_to_width_with(&text, 20, "…", false);
        assert!(visible_width(&r) <= 20);
        assert!(r.contains("\x1b[31m"));
        assert!(r.ends_with("\x1b[0m…\x1b[0m"));
    }

    #[test]
    fn truncate_handles_malformed_ansi_prefix() {
        let mut text = String::from("abc\x1bnot-ansi ");
        text.push_str(&"🙂".repeat(1000));
        let r = truncate_to_width_with(&text, 20, "…", false);
        assert!(visible_width(&r) <= 20);
    }

    #[test]
    fn truncate_clips_wide_ellipsis_safely() {
        assert_eq!(truncate_to_width_with("abcdef", 1, "🙂", false), "");
        assert_eq!(
            truncate_to_width_with("abcdef", 2, "🙂", false),
            "\x1b[0m🙂\x1b[0m"
        );
        assert!(visible_width(&truncate_to_width_with("abcdef", 2, "🙂", false)) <= 2);
    }

    #[test]
    fn truncate_returns_text_when_fits_even_if_ellipsis_too_wide() {
        assert_eq!(truncate_to_width_with("a", 2, "🙂", false), "a");
        assert_eq!(truncate_to_width_with("界", 2, "🙂", false), "界");
    }

    #[test]
    fn truncate_pads_to_width() {
        let r = truncate_to_width_with("🙂界🙂界🙂界", 8, "…", true);
        assert_eq!(visible_width(&r), 8);
    }

    #[test]
    fn truncate_no_ellipsis_still_appends_reset() {
        let mut text = String::from("\x1b[31m");
        text.push_str(&"hello".repeat(100));
        let r = truncate_to_width_with(&text, 10, "", false);
        assert!(visible_width(&r) <= 10);
        assert!(r.ends_with("\x1b[0m"));
    }

    #[test]
    fn truncate_keeps_contiguous_prefix_skips_wide_grapheme() {
        // Reproduces TS test case verbatim.
        let r = truncate_to_width_with("🙂\t界 \x1b_abc\x07", 7, "…", true);
        assert_eq!(r, "🙂\t\x1b[0m…\x1b[0m ");
    }

    // --- wrap_text_with_ansi ----------------------------------------------

    #[test]
    fn wrap_basic_plain_text() {
        let lines = wrap_text_with_ansi("hello world this is a test", 10);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(visible_width(line) <= 10);
        }
    }

    #[test]
    fn wrap_truncates_trailing_whitespace() {
        let lines = wrap_text_with_ansi("  ", 1);
        assert!(visible_width(&lines[0]) <= 1);
    }

    #[test]
    fn wrap_preserves_color_across_wraps() {
        let red = "\x1b[31m";
        let reset = "\x1b[0m";
        let text = format!("{red}hello world this is red{reset}");
        let lines = wrap_text_with_ansi(&text, 10);

        for line in lines.iter().skip(1) {
            assert!(
                line.starts_with(red),
                "continuation line missing red: {line:?}"
            );
        }
        for line in lines.iter().take(lines.len() - 1) {
            assert!(
                !line.ends_with("\x1b[0m"),
                "non-final line should not end with full reset: {line:?}"
            );
        }
    }

    #[test]
    fn wrap_underline_not_applied_before_styled_text() {
        let underline_on = "\x1b[4m";
        let underline_off = "\x1b[24m";
        let url = "https://example.com/very/long/path/that/will/wrap";
        let text = format!("read this thread {underline_on}{url}{underline_off}");
        let wrapped = wrap_text_with_ansi(&text, 40);

        assert_eq!(wrapped[0], "read this thread");
        assert!(wrapped[1].starts_with(underline_on));
        assert!(wrapped[1].contains("https://"));
    }

    #[test]
    fn wrap_no_whitespace_before_underline_off() {
        let underline_on = "\x1b[4m";
        let underline_off = "\x1b[24m";
        let text = format!("{underline_on}underlined text here {underline_off}more");
        let wrapped = wrap_text_with_ansi(&text, 18);
        assert!(!wrapped[0].contains(&format!(" {underline_off}")));
    }

    #[test]
    fn wrap_underline_uses_underline_off_at_line_end_not_full_reset() {
        let underline_on = "\x1b[4m";
        let underline_off = "\x1b[24m";
        let url = "https://example.com/very/long/path/that/will/definitely/wrap";
        let text = format!("prefix {underline_on}{url}{underline_off} suffix");
        let wrapped = wrap_text_with_ansi(&text, 30);

        for line in wrapped.iter().skip(1).take(wrapped.len().saturating_sub(2)) {
            if line.contains(underline_on) {
                assert!(line.ends_with(underline_off));
                assert!(!line.ends_with("\x1b[0m"));
            }
        }
    }

    #[test]
    fn wrap_preserves_background_across_lines() {
        let bg_blue = "\x1b[44m";
        let reset = "\x1b[0m";
        let text = format!("{bg_blue}hello world this is blue background text{reset}");
        let wrapped = wrap_text_with_ansi(&text, 15);
        for line in &wrapped {
            assert!(line.contains(bg_blue));
        }
        for line in wrapped.iter().take(wrapped.len() - 1) {
            assert!(!line.ends_with("\x1b[0m"));
        }
    }

    #[test]
    fn wrap_resets_underline_but_preserves_background() {
        let underline_on = "\x1b[4m";
        let underline_off = "\x1b[24m";
        let text = format!(
            "\x1b[41mprefix {underline_on}UNDERLINED_CONTENT_THAT_WRAPS{underline_off} suffix\x1b[0m"
        );
        let wrapped = wrap_text_with_ansi(&text, 20);

        for line in &wrapped {
            let has_bg = line.contains("[41m") || line.contains(";41m") || line.contains("[41;");
            assert!(has_bg, "line missing bg 41: {line:?}");
        }
    }

    #[test]
    fn wrap_osc8_reopens_on_continuation_lines() {
        let url = "https://example.com";
        let input = format!("\x1b]8;;{url}\x1b\\0123456789\x1b]8;;\x1b\\");
        let lines = wrap_text_with_ansi(&input, 6);

        for line in &lines {
            // Strip OSC 8 and SGR. If anything visible remains, the line must
            // contain an OSC 8 open.
            let mut stripped = line.clone();
            // Remove OSC 8 sequences (any variant).
            while let Some(start) = stripped.find("\x1b]8;") {
                let rest = &stripped[start..];
                let end_offset = if let Some(p) = rest.find('\x07') {
                    p + 1
                } else if let Some(p) = rest.find("\x1b\\") {
                    p + 2
                } else {
                    break;
                };
                stripped.replace_range(start..start + end_offset, "");
            }
            // Remove CSI sequences.
            stripped = strip_ansi(&stripped);
            if !stripped.trim().is_empty() {
                let opener = format!("\x1b]8;;{url}\x1b\\");
                assert!(
                    line.starts_with(&opener) || line.contains(&opener),
                    "line {line:?} has visible text but no OSC 8 re-open"
                );
            }
        }
    }

    #[test]
    fn wrap_osc8_closes_before_each_line_break() {
        let url = "https://example.com";
        let input = format!("\x1b]8;;{url}\x1b\\0123456789\x1b]8;;\x1b\\");
        let lines = wrap_text_with_ansi(&input, 6);
        let opener = format!("\x1b]8;;{url}\x1b\\");

        for line in lines.iter().take(lines.len() - 1) {
            if line.contains(&opener) {
                assert!(
                    line.ends_with("\x1b]8;;\x1b\\"),
                    "non-final line {line:?} inside hyperlink does not close it"
                );
            }
        }
    }

    #[test]
    fn wrap_osc8_preserves_bel_terminator() {
        let url = format!("https://example.com/oauth/{}", "a".repeat(32));
        let input = format!("\x1b]8;;{url}\x07{url}\x1b]8;;\x07");
        let lines = wrap_text_with_ansi(&input, 20);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(
                line.contains(&format!("\x1b]8;;{url}\x07")),
                "line {line:?} does not reopen with BEL"
            );
            assert!(
                !line.contains(&format!("\x1b]8;;{url}\x1b\\")),
                "line {line:?} reopens with ST"
            );
        }
        for line in lines.iter().take(lines.len() - 1) {
            assert!(
                line.ends_with("\x1b]8;;\x07"),
                "line {line:?} does not close with BEL"
            );
        }
    }

    #[test]
    fn wrap_osc8_no_emit_outside_hyperlink() {
        let url = "https://example.com";
        let input = format!("before \x1b]8;;{url}\x1b\\link\x1b]8;;\x1b\\ after");
        let lines = wrap_text_with_ansi(&input, 80);
        assert_eq!(lines.len(), 1);
        let opener = format!("\x1b]8;;{url}\x1b\\");
        let count_open = lines[0].matches(&opener).count();
        let count_close = lines[0].matches("\x1b]8;;\x1b\\").count();
        assert_eq!(count_open, 1);
        assert_eq!(count_close, 1);
    }

    // --- slice_by_column ---------------------------------------------------

    #[test]
    fn slice_by_column_basic() {
        assert_eq!(slice_by_column("hello world", 6, 5, false), "world");
    }

    #[test]
    fn slice_by_column_with_ansi() {
        let s = "\x1b[31mhello\x1b[0m world";
        let r = slice_by_column(s, 0, 5, false);
        assert_eq!(visible_width(&r), 5);
        assert!(r.contains("\x1b[31m"));
    }

    #[test]
    fn slice_by_column_strict_excludes_overflow_wide_char() {
        // 界 is width 2; with strict, slicing [1, 2) must exclude it.
        let r = slice_by_column("a界b", 1, 1, true);
        assert_eq!(visible_width(&r), 0);
    }

    #[test]
    fn slice_with_width_returns_width() {
        let r = slice_with_width("hello", 1, 3, false);
        assert_eq!(r.text, "ell");
        assert_eq!(r.width, 3);
    }

    // --- extract_segments --------------------------------------------------

    #[test]
    fn extract_segments_plain() {
        let r = extract_segments("hello world", 5, 6, 5, false);
        assert_eq!(r.before, "hello");
        assert_eq!(r.after, "world");
    }

    #[test]
    fn extract_segments_inherits_styling_into_after() {
        // Bold open before overlay; after segment should inherit bold.
        let line = "\x1b[1mAAAAA\x1b[0m  BBBBB";
        let r = extract_segments(line, 5, 7, 5, false);
        assert!(r.after.contains("BBBBB"));
        // The "after" segment starts with a styling reset (no codes left after \x1b[0m).
        // So `after` should not start with a leftover bold opener.
    }

    // --- background / pad --------------------------------------------------

    #[test]
    fn apply_background_to_line_pads_and_styles() {
        let r = apply_background_to_line("hi", 5, |s| format!("\x1b[44m{s}\x1b[0m"));
        assert_eq!(r, "\x1b[44mhi   \x1b[0m");
    }

    #[test]
    fn pad_to_width_short() {
        assert_eq!(pad_to_width("hi", 5), "hi   ");
        assert_eq!(pad_to_width("hello", 3), "hello");
    }

    // --- normalize_terminal_output -----------------------------------------

    #[test]
    fn normalize_terminal_output_thai_lao_am() {
        assert_eq!(normalize_terminal_output("ำ"), "\u{0e4d}\u{0e32}");
        assert_eq!(normalize_terminal_output("ຳ"), "\u{0ecd}\u{0eb2}");
        assert_eq!(
            visible_width(&normalize_terminal_output("ำabc")),
            visible_width("ำabc")
        );
        assert_eq!(
            visible_width(&normalize_terminal_output("ຳabc")),
            visible_width("ຳabc")
        );
    }

    #[test]
    fn normalize_terminal_output_passthrough() {
        assert_eq!(normalize_terminal_output("hello"), "hello");
    }

    // --- whitespace / punctuation -----------------------------------------

    #[test]
    fn whitespace_classification() {
        assert!(is_whitespace_char(' '));
        assert!(is_whitespace_char('\t'));
        assert!(!is_whitespace_char('a'));
    }

    #[test]
    fn punctuation_classification() {
        for c in "(){}[]<>.,;:'\"!?+-=*/\\|&%^$#@~`".chars() {
            assert!(is_punctuation_char(c), "{c:?} should be punctuation");
        }
        assert!(!is_punctuation_char('a'));
        assert!(!is_punctuation_char('0'));
    }

    // --- legacy wrap_text --------------------------------------------------

    #[test]
    fn wrap_text_no_wrap() {
        assert_eq!(wrap_text("hello", 80), vec!["hello"]);
    }

    #[test]
    fn wrap_text_newlines() {
        assert_eq!(wrap_text("a\nb", 80), vec!["a", "b"]);
    }

    #[test]
    fn wrap_text_ansi_preserved() {
        assert_eq!(
            wrap_text("\x1b[31mhello\x1b[0m", 80),
            vec!["\x1b[31mhello\x1b[0m"]
        );
    }

    // --- char_width --------------------------------------------------------

    #[test]
    fn char_width_basic() {
        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width('你'), 2);
    }
}
