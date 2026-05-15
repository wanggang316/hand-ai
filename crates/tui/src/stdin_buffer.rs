//! Stdin reassembly buffer for fragmented escape sequences and partial UTF-8.
//!
//! `StdinBuffer` is the first layer of the TUI input pipeline. Raw stdin
//! `read()` calls return arbitrary byte chunks that may split escape
//! sequences (e.g. `\x1b[<35;20;5M` arriving as `\x1b`, `[<35`, `;20;5M`)
//! or split a multi-byte UTF-8 codepoint mid-sequence. This module
//! reassembles those into complete logical units before they reach the
//! key parser.
//!
//! Bugs here surface as "F1 doesn't work over slow SSH" or "Chinese
//! input shows mojibake under load". The implementation exposes a
//! synchronous `push(&[u8]) -> Vec<Event>` shape — no callbacks, no
//! implicit timeouts. Async consumers can wrap one with
//! [`channel_from_buffer`].
//!
//! Higher layers (paste-mode framing, Kitty raw-duplicate suppression)
//! are intentionally out of scope here: this module only guarantees
//! that each emitted [`StdinBufferEvent::Data`] is either a single
//! complete escape sequence or a single printable codepoint.

use tokio::sync::mpsc;

const ESC: char = '\x1b';

/// Default cap on retained-but-incomplete escape-sequence bytes. Large
/// enough for any realistic terminal response (DCS XTVersion, OSC 52
/// clipboard echoes), small enough to bound memory if the stream
/// desynchronises.
const DEFAULT_MAX_REMAINDER_BYTES: usize = 64 * 1024;

/// Outcome of inspecting a candidate escape-sequence prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Completeness {
    /// The prefix is a fully-formed escape sequence.
    Complete,
    /// The prefix is a valid start but needs more bytes.
    Incomplete,
    /// The prefix does not begin with `ESC` — caller should treat it
    /// as plain text.
    NotEscape,
}

/// Configuration for [`StdinBuffer`].
#[derive(Debug, Clone)]
pub struct StdinBufferOptions {
    /// Maximum bytes to retain as an incomplete escape-sequence
    /// remainder before emitting [`StdinBufferEvent::Overflow`] and
    /// dropping the held bytes. Defaults to 64 KiB.
    pub max_remainder_bytes: usize,
    /// When `true`, each complete sequence (or printable codepoint) is
    /// emitted as its own [`StdinBufferEvent::Data`]. When `false`,
    /// runs of plain printable text within a single `push` call are
    /// coalesced into one event, while escape sequences remain
    /// individually framed. Defaults to `true` to match the upstream
    /// TS behaviour.
    pub split_per_sequence: bool,
}

impl Default for StdinBufferOptions {
    fn default() -> Self {
        Self {
            max_remainder_bytes: DEFAULT_MAX_REMAINDER_BYTES,
            split_per_sequence: true,
        }
    }
}

/// Event emitted by [`StdinBuffer::push`] / [`StdinBuffer::flush`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdinBufferEvent {
    /// A complete logical input unit: either one escape sequence or
    /// one printable codepoint (possibly coalesced — see
    /// [`StdinBufferOptions::split_per_sequence`]).
    Data(String),
    /// The held remainder exceeded `max_remainder_bytes`; the oldest
    /// held bytes have been discarded to bound memory. Emitted at
    /// most once per overflow event.
    Overflow,
}

/// Reassembles raw stdin bytes into complete escape sequences and
/// printable codepoints.
///
/// See the [module-level documentation](self) for context.
pub struct StdinBuffer {
    /// Bytes received but not yet emitted. Holds either:
    /// - a partial UTF-8 codepoint (1–3 trailing bytes), or
    /// - a partial escape sequence (starts with `\x1b`).
    remainder: Vec<u8>,
    options: StdinBufferOptions,
}

impl Default for StdinBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl StdinBuffer {
    /// Create a buffer with default options.
    pub fn new() -> Self {
        Self::with_options(StdinBufferOptions::default())
    }

    /// Create a buffer with explicit options.
    pub fn with_options(options: StdinBufferOptions) -> Self {
        Self {
            remainder: Vec::new(),
            options,
        }
    }

    /// Feed raw bytes from stdin. Returns every complete logical unit
    /// extracted by this push. Held remainder (incomplete UTF-8 or
    /// incomplete escape) stays inside the buffer until completed by a
    /// future push or surfaced via [`Self::flush`].
    pub fn push(&mut self, bytes: &[u8]) -> Vec<StdinBufferEvent> {
        let mut events = Vec::new();
        if bytes.is_empty() && self.remainder.is_empty() {
            return events;
        }

        self.remainder.extend_from_slice(bytes);

        // Enforce the remainder cap before doing any work; keep the
        // most recent bytes (likely to form the next valid sequence)
        // and emit a single Overflow signal.
        if self.remainder.len() > self.options.max_remainder_bytes {
            let keep_from = self.remainder.len() - self.options.max_remainder_bytes;
            self.remainder.drain(..keep_from);
            events.push(StdinBufferEvent::Overflow);
        }

        // Split into a UTF-8 prefix we can decode plus a trailing
        // partial-codepoint tail to retain.
        let (decoded, tail_keep) = decode_with_tail(&self.remainder);
        let (sequences, remainder_str) = extract_complete_sequences(&decoded);

        // Rebuild the held remainder: any incomplete escape (string
        // remainder) plus the partial-codepoint tail bytes.
        let mut new_remainder = remainder_str.into_bytes();
        if tail_keep > 0 {
            let start = self.remainder.len() - tail_keep;
            new_remainder.extend_from_slice(&self.remainder[start..]);
        }
        self.remainder = new_remainder;

        if self.options.split_per_sequence {
            events.extend(sequences.into_iter().map(StdinBufferEvent::Data));
        } else {
            // Coalesce consecutive plain (non-ESC) sequences into one
            // event; keep escapes individual.
            let mut buf = String::new();
            for seq in sequences {
                if seq.starts_with(ESC) {
                    if !buf.is_empty() {
                        events.push(StdinBufferEvent::Data(std::mem::take(&mut buf)));
                    }
                    events.push(StdinBufferEvent::Data(seq));
                } else {
                    buf.push_str(&seq);
                }
            }
            if !buf.is_empty() {
                events.push(StdinBufferEvent::Data(buf));
            }
        }

        events
    }

    /// Force-emit any held bytes as a single [`StdinBufferEvent::Data`]
    /// (typically called on shutdown or when the consumer wants to
    /// release a stuck partial sequence). Invalid UTF-8 in the
    /// remainder is replaced with `U+FFFD`.
    pub fn flush(&mut self) -> Vec<StdinBufferEvent> {
        if self.remainder.is_empty() {
            return Vec::new();
        }
        let held = std::mem::take(&mut self.remainder);
        let s = String::from_utf8_lossy(&held).into_owned();
        vec![StdinBufferEvent::Data(s)]
    }

    /// Currently held but not yet emitted bytes (for diagnostics).
    pub fn remainder_len(&self) -> usize {
        self.remainder.len()
    }
}

/// Decode the leading valid-UTF-8 prefix of `bytes`, returning the
/// decoded `String` and the count of trailing bytes that should be
/// retained as a possibly-incomplete codepoint.
///
/// Truly malformed UTF-8 (not just incomplete at the end) is replaced
/// with `U+FFFD`. The "incomplete tail" branch only fires when the
/// trailing bytes form a valid UTF-8 *prefix* shorter than its
/// declared length — i.e. the next push could complete it.
fn decode_with_tail(bytes: &[u8]) -> (String, usize) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), 0),
        Err(e) => {
            let valid_up_to = e.valid_up_to();
            match e.error_len() {
                // Trailing bytes are an incomplete-but-valid UTF-8
                // prefix — hold them for the next push.
                None => {
                    // Safe: 0..valid_up_to is guaranteed valid UTF-8.
                    let head = std::str::from_utf8(&bytes[..valid_up_to])
                        .expect("valid_up_to is a UTF-8 boundary")
                        .to_string();
                    (head, bytes.len() - valid_up_to)
                }
                // Genuinely malformed bytes inside the stream — fall
                // back to lossy decode of the whole buffer so the
                // U+FFFD lands at the right offset, and retain
                // nothing.
                Some(_) => (String::from_utf8_lossy(bytes).into_owned(), 0),
            }
        }
    }
}

/// Walk `buffer` left-to-right, emitting either a single complete
/// escape sequence or a single printable codepoint at each step.
/// Returns `(sequences, remainder)` where `remainder` is any trailing
/// incomplete escape sequence still being assembled.
pub(crate) fn extract_complete_sequences(buffer: &str) -> (Vec<String>, String) {
    let mut sequences: Vec<String> = Vec::new();
    let mut iter = buffer.char_indices().peekable();

    while let Some(&(pos, ch)) = iter.peek() {
        if ch == ESC {
            // Try to extend the candidate one char at a time until it
            // becomes Complete, runs out of input (Incomplete), or we
            // hit NotEscape (cannot happen when starting at ESC).
            let tail = &buffer[pos..];
            let mut found: Option<usize> = None;
            let mut last_status = Completeness::Incomplete;
            for (boundary, _) in tail.char_indices().skip(1) {
                let candidate = &tail[..boundary];
                match is_complete_sequence(candidate) {
                    Completeness::Complete => {
                        found = Some(boundary);
                        last_status = Completeness::Complete;
                        break;
                    }
                    Completeness::Incomplete => {
                        last_status = Completeness::Incomplete;
                        continue;
                    }
                    Completeness::NotEscape => {
                        // Cannot occur because `candidate` starts with ESC.
                        last_status = Completeness::NotEscape;
                        break;
                    }
                }
            }
            // Also consider the full tail (its char_indices skip the
            // start, so the loop above only checks prefixes shorter
            // than the full tail).
            if found.is_none() && matches!(is_complete_sequence(tail), Completeness::Complete) {
                found = Some(tail.len());
                last_status = Completeness::Complete;
            }

            match (found, last_status) {
                (Some(end), _) => {
                    sequences.push(tail[..end].to_string());
                    // Advance the iterator past the consumed sequence.
                    let target = pos + end;
                    while let Some(&(p, _)) = iter.peek() {
                        if p >= target {
                            break;
                        }
                        iter.next();
                    }
                }
                (None, _) => {
                    // Incomplete escape: stash everything from `pos`
                    // as remainder and stop.
                    return (sequences, tail.to_string());
                }
            }
        } else {
            sequences.push(ch.to_string());
            iter.next();
        }
    }

    (sequences, String::new())
}

/// Decide whether `data` is a complete escape sequence, a valid
/// prefix in flight, or not an escape at all.
pub(crate) fn is_complete_sequence(data: &str) -> Completeness {
    let mut chars = data.chars();
    match chars.next() {
        None => return Completeness::NotEscape,
        Some(c) if c != ESC => return Completeness::NotEscape,
        _ => {}
    }

    let after_esc = &data[ESC.len_utf8()..];
    if after_esc.is_empty() {
        return Completeness::Incomplete;
    }

    let first = after_esc.chars().next().unwrap();
    match first {
        '[' => {
            // Old-style mouse: ESC [ M + 3 bytes = 6 chars total
            if after_esc.starts_with("[M") {
                if data.chars().count() >= 6 {
                    return Completeness::Complete;
                }
                return Completeness::Incomplete;
            }
            is_complete_csi_sequence(data)
        }
        ']' => is_complete_osc_sequence(data),
        'P' => is_complete_dcs_sequence(data),
        '_' => is_complete_apc_sequence(data),
        'O' => {
            // SS3: ESC O <single char>
            if after_esc.chars().count() >= 2 {
                Completeness::Complete
            } else {
                Completeness::Incomplete
            }
        }
        _ => {
            // Meta-key (ESC + single char) or unknown — treat as
            // complete once we have at least one byte after ESC.
            Completeness::Complete
        }
    }
}

/// CSI: `ESC [ ... <final 0x40..=0x7E>`. SGR-mouse (`ESC[<...M|m`)
/// has extra structural validation to avoid prematurely terminating
/// on a literal `m` inside the parameters.
pub(crate) fn is_complete_csi_sequence(data: &str) -> Completeness {
    if !data.starts_with("\x1b[") {
        return Completeness::Complete;
    }
    if data.chars().count() < 3 {
        return Completeness::Incomplete;
    }

    let payload = &data[ESC.len_utf8() + 1..];
    // Last char's codepoint is checked against the CSI final-byte range.
    let Some(last) = payload.chars().next_back() else {
        return Completeness::Incomplete;
    };
    let last_code = last as u32;

    if (0x40..=0x7e).contains(&last_code) {
        if payload.starts_with('<') {
            // SGR-mouse: <digits;digits;digits[Mm]
            if last == 'M' || last == 'm' {
                let inner = &payload[1..payload.len() - last.len_utf8()];
                let parts: Vec<&str> = inner.split(';').collect();
                if parts.len() == 3
                    && parts
                        .iter()
                        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
                {
                    return Completeness::Complete;
                }
                return Completeness::Incomplete;
            }
            return Completeness::Incomplete;
        }
        return Completeness::Complete;
    }

    Completeness::Incomplete
}

/// OSC: `ESC ] ... ST` where ST is `BEL` (`\x07`) or `ESC \`.
pub(crate) fn is_complete_osc_sequence(data: &str) -> Completeness {
    if !data.starts_with("\x1b]") {
        return Completeness::Complete;
    }
    if data.ends_with("\x1b\\") || data.ends_with('\x07') {
        return Completeness::Complete;
    }
    Completeness::Incomplete
}

/// DCS: `ESC P ... ESC \`. Used for XTVersion, sixel, etc.
pub(crate) fn is_complete_dcs_sequence(data: &str) -> Completeness {
    if !data.starts_with("\x1bP") {
        return Completeness::Complete;
    }
    if data.ends_with("\x1b\\") {
        return Completeness::Complete;
    }
    Completeness::Incomplete
}

/// APC: `ESC _ ... ESC \`. Used for Kitty graphics responses.
pub(crate) fn is_complete_apc_sequence(data: &str) -> Completeness {
    if !data.starts_with("\x1b_") {
        return Completeness::Complete;
    }
    if data.ends_with("\x1b\\") {
        return Completeness::Complete;
    }
    Completeness::Incomplete
}

/// If `seq` is a Kitty CSI-u sequence with no modifier set (i.e.
/// `ESC[<codepoint>u` or `ESC[<codepoint>:<text>u`, with no `;<mods>`
/// segment), returns the printable Unicode codepoint. Returns `None`
/// for control characters (codepoint < 32) or non-matching input.
///
/// This is a structural-completeness helper — semantic decoding lives
/// in `keys::decode_kitty_printable`.
pub fn parse_unmodified_kitty_printable_codepoint(seq: &str) -> Option<u32> {
    // Match: ESC [ <digits>{1,} ( : <digits>* )? ( : <digits>+ )? u
    let inner = seq.strip_prefix("\x1b[")?.strip_suffix('u')?;
    if inner.is_empty() {
        return None;
    }

    let mut parts = inner.split(':');
    let codepoint_str = parts.next()?;
    if codepoint_str.is_empty() || !codepoint_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    // Up to two further `:`-separated segments are tolerated; both
    // must be digit-only (possibly empty for the first).
    if let Some(p1) = parts.next()
        && !p1.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    if let Some(p2) = parts.next()
        && (p2.is_empty() || !p2.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    if parts.next().is_some() {
        return None;
    }

    let cp: u32 = codepoint_str.parse().ok()?;
    if cp >= 32 { Some(cp) } else { None }
}

/// Build an unbounded MPSC pair around `buffer`: bytes pushed into
/// the input sender are reassembled and emitted via the output
/// receiver. Spawned task ends when the sender is dropped; a final
/// [`StdinBuffer::flush`] is performed on shutdown.
pub fn channel_from_buffer(
    mut buffer: StdinBuffer,
) -> (
    mpsc::UnboundedSender<Vec<u8>>,
    mpsc::UnboundedReceiver<StdinBufferEvent>,
) {
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (out_tx, out_rx) = mpsc::unbounded_channel::<StdinBufferEvent>();

    tokio::spawn(async move {
        while let Some(chunk) = in_rx.recv().await {
            for ev in buffer.push(&chunk) {
                if out_tx.send(ev).is_err() {
                    return;
                }
            }
        }
        for ev in buffer.flush() {
            if out_tx.send(ev).is_err() {
                return;
            }
        }
    });

    (in_tx, out_rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_strings(events: &[StdinBufferEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                StdinBufferEvent::Data(s) => Some(s.clone()),
                StdinBufferEvent::Overflow => None,
            })
            .collect()
    }

    #[test]
    fn complete_csi_in_one_chunk() {
        let mut buf = StdinBuffer::new();
        let events = buf.push(b"\x1b[A");
        assert_eq!(data_strings(&events), vec!["\x1b[A"]);
        assert_eq!(buf.remainder_len(), 0);
    }

    #[test]
    fn csi_split_across_two_chunks() {
        let mut buf = StdinBuffer::new();
        assert!(data_strings(&buf.push(b"\x1b[")).is_empty());
        let events = buf.push(b"A");
        assert_eq!(data_strings(&events), vec!["\x1b[A"]);
    }

    #[test]
    fn sgr_mouse_split_many_chunks() {
        let mut buf = StdinBuffer::new();
        let chunks: &[&[u8]] = &[
            b"\x1b", b"[", b"<", b"3", b"5", b";", b"2", b"0", b";", b"5", b"m",
        ];
        let mut all = Vec::new();
        for c in chunks {
            all.extend(data_strings(&buf.push(c)));
        }
        assert_eq!(all, vec!["\x1b[<35;20;5m"]);
    }

    #[test]
    fn osc_with_bel_terminator() {
        let mut buf = StdinBuffer::new();
        let seq = "\x1b]0;hello\x07";
        let events = buf.push(seq.as_bytes());
        assert_eq!(data_strings(&events), vec![seq]);
    }

    #[test]
    fn osc_with_st_terminator() {
        let mut buf = StdinBuffer::new();
        let seq = "\x1b]0;hello\x1b\\";
        let events = buf.push(seq.as_bytes());
        assert_eq!(data_strings(&events), vec![seq]);
    }

    #[test]
    fn osc_split_pending_until_terminator() {
        let mut buf = StdinBuffer::new();
        assert!(data_strings(&buf.push(b"\x1b]52;c;abc")).is_empty());
        let events = buf.push(b"\x07");
        assert_eq!(data_strings(&events), vec!["\x1b]52;c;abc\x07"]);
    }

    #[test]
    fn dcs_complete_and_incomplete() {
        let mut buf = StdinBuffer::new();
        let seq = "\x1bP>|xterm(370)\x1b\\";
        assert_eq!(data_strings(&buf.push(seq.as_bytes())), vec![seq]);

        let mut buf2 = StdinBuffer::new();
        assert!(data_strings(&buf2.push(b"\x1bP>|partial")).is_empty());
        assert!(buf2.remainder_len() > 0);
    }

    #[test]
    fn apc_complete_and_incomplete() {
        let mut buf = StdinBuffer::new();
        let seq = "\x1b_Gi=1,a=q;\x1b\\";
        assert_eq!(data_strings(&buf.push(seq.as_bytes())), vec![seq]);

        let mut buf2 = StdinBuffer::new();
        assert!(data_strings(&buf2.push(b"\x1b_Gi=1,a=q;")).is_empty());
        assert!(buf2.remainder_len() > 0);
    }

    #[test]
    fn plain_printable_text_passthrough() {
        let mut buf = StdinBuffer::new();
        let events = buf.push(b"abc");
        assert_eq!(data_strings(&events), vec!["a", "b", "c"]);
    }

    #[test]
    fn unicode_passthrough_per_codepoint() {
        let mut buf = StdinBuffer::new();
        let events = buf.push("hi 世界".as_bytes());
        assert_eq!(data_strings(&events), vec!["h", "i", " ", "世", "界"]);
    }

    #[test]
    fn partial_utf8_three_byte_split() {
        // CJK char "中" is e4 b8 ad in UTF-8.
        let mut buf = StdinBuffer::new();
        assert!(data_strings(&buf.push(&[0xe4, 0xb8])).is_empty());
        assert_eq!(buf.remainder_len(), 2);
        let events = buf.push(&[0xad]);
        assert_eq!(data_strings(&events), vec!["中"]);
        assert_eq!(buf.remainder_len(), 0);
    }

    #[test]
    fn partial_utf8_four_byte_emoji_split() {
        // "🎉" is f0 9f 8e 89.
        let mut buf = StdinBuffer::new();
        assert!(data_strings(&buf.push(&[0xf0, 0x9f, 0x8e])).is_empty());
        let events = buf.push(&[0x89]);
        assert_eq!(data_strings(&events), vec!["🎉"]);
    }

    #[test]
    fn malformed_utf8_becomes_replacement() {
        let mut buf = StdinBuffer::new();
        // 0xff is never valid UTF-8 lead, followed by a valid ASCII.
        let events = buf.push(&[0xff, b'a']);
        let strings = data_strings(&events);
        assert_eq!(strings, vec!["\u{FFFD}", "a"]);
    }

    #[test]
    fn mixed_text_csi_text_split_per_sequence() {
        let mut buf = StdinBuffer::with_options(StdinBufferOptions {
            split_per_sequence: true,
            ..Default::default()
        });
        let events = buf.push(b"a\x1b[Ab");
        assert_eq!(data_strings(&events), vec!["a", "\x1b[A", "b"]);
    }

    #[test]
    fn mixed_text_csi_text_coalesced() {
        let mut buf = StdinBuffer::with_options(StdinBufferOptions {
            split_per_sequence: false,
            ..Default::default()
        });
        let events = buf.push(b"abc\x1b[Adef");
        assert_eq!(data_strings(&events), vec!["abc", "\x1b[A", "def"]);
    }

    #[test]
    fn overflow_when_remainder_exceeds_cap() {
        let mut buf = StdinBuffer::with_options(StdinBufferOptions {
            max_remainder_bytes: 8,
            split_per_sequence: true,
        });
        // Single push that exceeds the cap with an unterminated OSC
        // so the remainder must be retained but cannot be.
        let events = buf.push(b"\x1b]52;abcdefghijklmn");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, StdinBufferEvent::Overflow))
        );
        assert!(buf.remainder_len() <= 8);
    }

    #[test]
    fn bracketed_paste_is_two_csi_sequences() {
        // The brief: bracketed paste markers are individual complete
        // CSI sequences, NOT joined as one unit.
        let mut buf = StdinBuffer::new();
        let events = buf.push(b"\x1b[200~hello\x1b[201~");
        assert_eq!(
            data_strings(&events),
            vec!["\x1b[200~", "h", "e", "l", "l", "o", "\x1b[201~",]
        );
    }

    #[test]
    fn kitty_csi_u_press_and_release() {
        let mut buf = StdinBuffer::new();
        let events = buf.push(b"\x1b[97u\x1b[97;1:3u");
        assert_eq!(data_strings(&events), vec!["\x1b[97u", "\x1b[97;1:3u"]);
    }

    #[test]
    fn kitty_printable_codepoint_helper() {
        assert_eq!(
            parse_unmodified_kitty_printable_codepoint("\x1b[97u"),
            Some(97)
        );
        assert_eq!(
            parse_unmodified_kitty_printable_codepoint("\x1b[224u"),
            Some(224)
        );
        // Modifier present → not unmodified printable.
        assert_eq!(
            parse_unmodified_kitty_printable_codepoint("\x1b[97;3u"),
            None
        );
        // Control codepoint → rejected.
        assert_eq!(parse_unmodified_kitty_printable_codepoint("\x1b[27u"), None);
        // Not a CSI-u shape.
        assert_eq!(parse_unmodified_kitty_printable_codepoint("\x1b[A"), None);
        assert_eq!(parse_unmodified_kitty_printable_codepoint("hi"), None);
    }

    #[test]
    fn meta_key_sequence_complete_after_one_char() {
        let mut buf = StdinBuffer::new();
        let events = buf.push(b"\x1ba");
        assert_eq!(data_strings(&events), vec!["\x1ba"]);
    }

    #[test]
    fn ss3_sequence_complete() {
        let mut buf = StdinBuffer::new();
        let events = buf.push(b"\x1bOA");
        assert_eq!(data_strings(&events), vec!["\x1bOA"]);
    }

    #[test]
    fn old_style_mouse_six_bytes() {
        let mut buf = StdinBuffer::new();
        // ESC [ M then three "byte" chars; the seventh char is
        // standalone printable.
        let events = buf.push(b"\x1b[M abc");
        let strings = data_strings(&events);
        assert_eq!(strings, vec!["\x1b[M ab", "c"]);
    }

    #[test]
    fn lone_escape_held_until_flush() {
        let mut buf = StdinBuffer::new();
        assert!(data_strings(&buf.push(b"\x1b")).is_empty());
        assert_eq!(buf.remainder_len(), 1);
        let events = buf.flush();
        assert_eq!(data_strings(&events), vec!["\x1b"]);
        assert_eq!(buf.remainder_len(), 0);
    }

    #[test]
    fn empty_push_with_empty_buffer_is_noop() {
        let mut buf = StdinBuffer::new();
        let events = buf.push(b"");
        assert!(events.is_empty());
    }

    #[test]
    fn flush_with_empty_buffer_is_noop() {
        let mut buf = StdinBuffer::new();
        let events = buf.flush();
        assert!(events.is_empty());
    }

    #[test]
    fn long_csi_sequence_ok() {
        let mut buf = StdinBuffer::new();
        let mut seq = String::from("\x1b[");
        for _ in 0..50 {
            seq.push_str("1;");
        }
        seq.push('H');
        let events = buf.push(seq.as_bytes());
        assert_eq!(data_strings(&events), vec![seq]);
    }

    #[test]
    fn multiple_complete_sequences_in_one_chunk() {
        let mut buf = StdinBuffer::new();
        let events = buf.push(b"\x1b[A\x1b[B\x1b[C");
        assert_eq!(data_strings(&events), vec!["\x1b[A", "\x1b[B", "\x1b[C"]);
    }

    #[test]
    fn partial_sequence_with_preceding_chars() {
        let mut buf = StdinBuffer::new();
        let events = buf.push(b"abc\x1b[<35");
        assert_eq!(data_strings(&events), vec!["a", "b", "c"]);
        assert!(buf.remainder_len() > 0);
        let events = buf.push(b";20;5m");
        assert_eq!(data_strings(&events), vec!["\x1b[<35;20;5m"]);
    }

    #[tokio::test]
    async fn channel_helper_emits_events() {
        let buf = StdinBuffer::new();
        let (tx, mut rx) = channel_from_buffer(buf);
        tx.send(b"\x1b[".to_vec()).unwrap();
        tx.send(b"A".to_vec()).unwrap();
        drop(tx);

        let mut received = Vec::new();
        while let Some(ev) = rx.recv().await {
            received.push(ev);
        }
        let strings: Vec<String> = received
            .into_iter()
            .filter_map(|e| match e {
                StdinBufferEvent::Data(s) => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(strings, vec!["\x1b[A"]);
    }
}
