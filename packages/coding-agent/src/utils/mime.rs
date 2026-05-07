//! MIME-type detection helpers.
//!
//! Two complementary surfaces:
//!
//! - [`mime_from_extension`] — fast extension-based lookup via the
//!   `mime_guess` crate. Suitable for display labels and Accept hints where
//!   speed matters and bytes aren't readily available.
//! - [`detect_supported_image_mime_type_from_file`] — magic-byte sniffing
//!   over the first ~4KB of a file. Mirrors the TS
//!   `detectSupportedImageMimeTypeFromFile` contract: returns the MIME type
//!   only when the bytes match one of `{image/jpeg, image/png, image/gif,
//!   image/webp}`, otherwise `None`. Other (or unsupported) image formats
//!   yield `None`.
//!
//! Magic-byte detection is implemented inline rather than pulled in via a
//! separate sniffing crate because we only support four image formats and
//! their signatures are stable and well-documented.

use std::io::Read;
use std::path::Path;

use thiserror::Error;

/// Maximum number of leading bytes inspected for magic-byte detection.
const SNIFF_BYTES: usize = 4100;

/// Error type for [`detect_supported_image_mime_type_from_file`].
#[derive(Debug, Error)]
pub enum MimeError {
    /// Underlying I/O failure while reading the file.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Look up a MIME type from a file extension via `mime_guess`.
///
/// Returns `None` when the path has no extension or `mime_guess` has no
/// registered mapping for it. The returned string follows the IANA
/// canonical form (e.g. `text/markdown`, `image/png`).
pub fn mime_from_extension(path: impl AsRef<Path>) -> Option<String> {
    let guess = mime_guess::from_path(path.as_ref()).first()?;
    Some(guess.essence_str().to_string())
}

/// Detect a *supported* image MIME type by sniffing the file's leading bytes.
///
/// Returns `Some(mime)` only when the bytes match one of `image/jpeg`,
/// `image/png`, `image/gif`, or `image/webp`. Returns `None` for any other
/// content (text files, unsupported image formats, empty files).
///
/// I/O errors during read are surfaced; mismatched magic bytes are not.
pub fn detect_supported_image_mime_type_from_file(
    path: impl AsRef<Path>,
) -> Result<Option<String>, MimeError> {
    let mut file = std::fs::File::open(path.as_ref())?;
    let mut buf = vec![0u8; SNIFF_BYTES];
    let mut filled = 0usize;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(detect_supported_image_mime_type(&buf[..filled]))
}

/// Detect a supported image MIME type from an in-memory byte slice.
///
/// Pure function over the same magic-byte rules as
/// [`detect_supported_image_mime_type_from_file`].
pub fn detect_supported_image_mime_type(bytes: &[u8]) -> Option<String> {
    if is_jpeg(bytes) {
        Some("image/jpeg".to_string())
    } else if is_png(bytes) {
        Some("image/png".to_string())
    } else if is_gif(bytes) {
        Some("image/gif".to_string())
    } else if is_webp(bytes) {
        Some("image/webp".to_string())
    } else {
        None
    }
}

// ---- magic-byte predicates ------------------------------------------------

/// JPEG: starts with `FF D8 FF`.
fn is_jpeg(b: &[u8]) -> bool {
    b.len() >= 3 && b[0] == 0xFF && b[1] == 0xD8 && b[2] == 0xFF
}

/// PNG: 8-byte signature `89 50 4E 47 0D 0A 1A 0A`.
fn is_png(b: &[u8]) -> bool {
    b.len() >= 8 && &b[..8] == b"\x89PNG\r\n\x1a\n"
}

/// GIF: starts with `GIF87a` or `GIF89a`.
fn is_gif(b: &[u8]) -> bool {
    b.len() >= 6 && (&b[..6] == b"GIF87a" || &b[..6] == b"GIF89a")
}

/// WebP: RIFF container with `WEBP` form (`52 49 46 46 .. .. .. .. 57 45 42 50`).
fn is_webp(b: &[u8]) -> bool {
    b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn extension_lookup_known_types() {
        assert_eq!(mime_from_extension("foo.png").as_deref(), Some("image/png"));
        assert_eq!(
            mime_from_extension("foo.json").as_deref(),
            Some("application/json")
        );
    }

    #[test]
    fn extension_lookup_unknown_returns_none() {
        assert!(mime_from_extension("noext").is_none());
        assert!(mime_from_extension("foo.thisisnotarealextension").is_none());
    }

    #[test]
    fn detect_jpeg_bytes() {
        let bytes = [0xFFu8, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        assert_eq!(
            detect_supported_image_mime_type(&bytes).as_deref(),
            Some("image/jpeg")
        );
    }

    #[test]
    fn detect_png_bytes() {
        let bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00";
        assert_eq!(
            detect_supported_image_mime_type(bytes).as_deref(),
            Some("image/png")
        );
    }

    #[test]
    fn detect_gif87_and_gif89() {
        assert_eq!(
            detect_supported_image_mime_type(b"GIF87a...").as_deref(),
            Some("image/gif")
        );
        assert_eq!(
            detect_supported_image_mime_type(b"GIF89a...").as_deref(),
            Some("image/gif")
        );
    }

    #[test]
    fn detect_webp() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&[0u8; 4]); // file size placeholder
        bytes.extend_from_slice(b"WEBP");
        bytes.extend_from_slice(b"VP8 ");
        assert_eq!(
            detect_supported_image_mime_type(&bytes).as_deref(),
            Some("image/webp")
        );
    }

    #[test]
    fn detect_rejects_text() {
        assert!(detect_supported_image_mime_type(b"hello world").is_none());
    }

    #[test]
    fn detect_rejects_empty() {
        assert!(detect_supported_image_mime_type(&[]).is_none());
    }

    #[test]
    fn detect_rejects_unsupported_image_format_bmp() {
        // BMP starts with "BM" — not in the supported set.
        assert!(detect_supported_image_mime_type(b"BM\x00\x00\x00\x00").is_none());
    }

    #[test]
    fn detect_rejects_riff_without_webp_form() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&[0u8; 4]);
        bytes.extend_from_slice(b"WAVE"); // RIFF audio, not image
        assert!(detect_supported_image_mime_type(&bytes).is_none());
    }

    #[test]
    fn from_file_jpeg() {
        let f = write_temp(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]);
        let result = detect_supported_image_mime_type_from_file(f.path()).unwrap();
        assert_eq!(result.as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn from_file_unsupported_returns_none() {
        let f = write_temp(b"not an image");
        let result = detect_supported_image_mime_type_from_file(f.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn from_file_empty_returns_none() {
        let f = write_temp(&[]);
        let result = detect_supported_image_mime_type_from_file(f.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn from_file_missing_path_errors() {
        let result =
            detect_supported_image_mime_type_from_file("/this/path/does/not/exist/zzz-mime-test");
        assert!(matches!(result, Err(MimeError::Io(_))));
    }
}
