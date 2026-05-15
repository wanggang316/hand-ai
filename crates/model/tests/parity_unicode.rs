//! Unicode-surrogate sanitisation coverage.
//!
//! Verifies that `sanitize_bytes` collapses
//! a WTF-8 encoded lone UTF-16 surrogate (`0xED 0xA0 0x80`) into a single
//! U+FFFD replacement character.

use model::sanitize_bytes;

#[test]
fn lone_surrogate_becomes_replacement_char() {
    // 0xED 0xA0 0x80 is the WTF-8 encoding of U+D800, the first lone high
    // surrogate. Valid UTF-8 strings cannot contain this sequence; the
    // sanitizer must collapse it to a single U+FFFD.
    let input: &[u8] = &[0xED, 0xA0, 0x80];
    let out = sanitize_bytes(input);
    assert_eq!(out, "\u{FFFD}");
    assert_eq!(out.chars().count(), 1);
}

#[test]
fn lone_surrogate_in_context_preserves_surrounding_text() {
    // "hi" + lone surrogate + "!"
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"hi");
    bytes.extend_from_slice(&[0xED, 0xA0, 0x80]);
    bytes.extend_from_slice(b"!");

    let out = sanitize_bytes(&bytes);
    assert_eq!(out, "hi\u{FFFD}!");
}

#[test]
fn full_surrogate_range_collapses_to_single_replacement() {
    // Every byte sequence ED A0..BF 80..BF encodes a surrogate-half and must
    // collapse to one replacement, not three.
    for second in 0xA0u8..=0xBFu8 {
        for third in 0x80u8..=0xBFu8 {
            let bytes = [0xED, second, third];
            let out = sanitize_bytes(&bytes);
            assert_eq!(
                out.chars().count(),
                1,
                "surrogate ED {second:02X} {third:02X} should yield a single replacement, got {out:?}"
            );
            assert_eq!(out, "\u{FFFD}");
        }
    }
}
