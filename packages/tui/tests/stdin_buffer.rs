//! Integration tests for `StdinBuffer`.

mod common;

use hand_tui::{StdinBuffer, StdinBufferEvent, StdinBufferOptions};

fn datas(events: Vec<StdinBufferEvent>) -> Vec<String> {
    events
        .into_iter()
        .filter_map(|e| match e {
            StdinBufferEvent::Data(s) => Some(s),
            StdinBufferEvent::Overflow => None,
        })
        .collect()
}

#[test]
fn plain_ascii_emits_per_codepoint_by_default() {
    let mut buf = StdinBuffer::new();
    let events = datas(buf.push(b"abc"));
    assert_eq!(events, vec!["a", "b", "c"]);
}

#[test]
fn coalesce_mode_groups_plain_runs() {
    let mut buf = StdinBuffer::with_options(StdinBufferOptions {
        split_per_sequence: false,
        ..StdinBufferOptions::default()
    });
    let events = datas(buf.push(b"abc"));
    assert_eq!(events, vec!["abc"]);
}

#[test]
fn split_escape_sequence_reassembled_across_pushes() {
    let mut buf = StdinBuffer::new();
    let part1 = datas(buf.push(b"\x1b[<35"));
    assert!(part1.is_empty(), "got {:?}", part1);
    assert!(buf.remainder_len() > 0);

    let part2 = datas(buf.push(b";20;5M"));
    assert_eq!(part2, vec!["\x1b[<35;20;5M".to_string()]);
    assert_eq!(buf.remainder_len(), 0);
}

#[test]
fn split_utf8_codepoint_reassembled() {
    // U+4E2D = E4 B8 AD
    let mut buf = StdinBuffer::new();
    let part1 = datas(buf.push(&[0xe4]));
    assert!(part1.is_empty());
    let part2 = datas(buf.push(&[0xb8, 0xad]));
    assert_eq!(part2, vec!["中".to_string()]);
}

#[test]
fn flush_emits_held_remainder_as_data() {
    let mut buf = StdinBuffer::new();
    let _ = buf.push(b"\x1b[123");
    assert!(buf.remainder_len() > 0);
    let flushed = datas(buf.flush());
    assert_eq!(flushed.len(), 1);
    assert!(flushed[0].starts_with('\x1b'));
    assert_eq!(buf.remainder_len(), 0);
}

#[test]
fn flush_when_empty_returns_no_events() {
    let mut buf = StdinBuffer::new();
    assert!(buf.flush().is_empty());
}

#[test]
fn overflow_emits_signal_when_remainder_cap_exceeded() {
    let mut buf = StdinBuffer::with_options(StdinBufferOptions {
        max_remainder_bytes: 8,
        split_per_sequence: true,
    });
    // Push more than 8 bytes of an incomplete escape sequence.
    let events = buf.push(b"\x1b[1234567890");
    assert!(events.iter().any(|e| matches!(e, StdinBufferEvent::Overflow)));
}
