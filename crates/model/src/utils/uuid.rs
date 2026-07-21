//! Time-ordered UUIDv7 generation (RFC 9562).

use rand::RngCore;
use std::time::{SystemTime, UNIX_EPOCH};

/// Generate a time-ordered UUIDv7 in standard hyphenated lowercase form.
///
/// Layout per RFC 9562 §5.7: a 48-bit big-endian Unix timestamp in
/// milliseconds, the version nibble (`0b0111`), the RFC 4122 variant bits
/// (`0b10`), and 74 random bits. The millisecond prefix makes ids sortable
/// by creation time, which backends that key caching or correlation on a
/// request id rely on.
pub fn uuid_v7() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    // Bytes 0-5: 48-bit big-endian unix-ms timestamp.
    bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..8]);
    // Byte 6 high nibble: version 7.
    bytes[6] = 0x70 | (bytes[6] & 0x0f);
    // Byte 8 top two bits: RFC 4122 variant.
    bytes[8] = 0x80 | (bytes[8] & 0x3f);

    let mut out = String::with_capacity(36);
    for (i, b) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}
