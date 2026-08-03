//! Read images from the system clipboard.
//!
//! [`arboard`] handles macOS, Windows, X11, and Wayland uniformly through
//! one API and returns RGBA pixel data — replacing the tower of
//! platform-specific tools (`wl-paste`, `xclip`, PowerShell on WSL, a
//! native clipboard addon, BMP→PNG via Photon) that a JS implementation
//! would need. We re-encode to PNG so callers can treat the bytes as a
//! normal image file.
//!
//! Supported MIME types: PNG, JPEG, WebP, GIF. We only ever produce PNG
//! (arboard hands us raw RGBA — there's no original file format to
//! preserve), but the type is exposed in [`ClipboardImage::mime_type`] so
//! call sites can branch in the future.

use std::io::Cursor;

use thiserror::Error;

/// Image bytes plus their MIME type.
#[derive(Debug, Clone)]
pub struct ClipboardImage {
    /// Encoded image bytes (always PNG today).
    pub bytes: Vec<u8>,
    /// MIME type matching `bytes` — `"image/png"` for now.
    pub mime_type: String,
}

/// Errors raised while reading or decoding the clipboard image.
#[derive(Debug, Error)]
pub enum ClipboardImageError {
    /// Could not open the system clipboard at all.
    #[error("clipboard unavailable: {0}")]
    Unavailable(String),
    /// PNG re-encoding of the RGBA buffer failed.
    #[error("failed to encode clipboard image as PNG: {0}")]
    Encode(#[from] image::ImageError),
}

/// Try to read an image from the clipboard.
///
/// Returns `Ok(None)` when the clipboard exists but holds no image (the
/// common case — text on the clipboard, or it's empty). `Err(...)` is
/// reserved for hard failures (no clipboard service, encode error).
pub fn read_clipboard_image() -> Result<Option<ClipboardImage>, ClipboardImageError> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| ClipboardImageError::Unavailable(e.to_string()))?;

    let image_data = match clipboard.get_image() {
        Ok(data) => data,
        Err(arboard::Error::ContentNotAvailable) => return Ok(None),
        Err(e) => return Err(ClipboardImageError::Unavailable(e.to_string())),
    };

    let bytes = encode_rgba_as_png(&image_data)?;
    Ok(Some(ClipboardImage {
        bytes,
        mime_type: "image/png".to_string(),
    }))
}

/// Encode an arboard `ImageData` (RGBA8) as PNG bytes.
fn encode_rgba_as_png(data: &arboard::ImageData<'_>) -> Result<Vec<u8>, image::ImageError> {
    let width = u32::try_from(data.width).unwrap_or(u32::MAX);
    let height = u32::try_from(data.height).unwrap_or(u32::MAX);
    let buffer: image::RgbaImage = image::ImageBuffer::from_raw(width, height, data.bytes.to_vec())
        .ok_or_else(|| {
            image::ImageError::Parameter(image::error::ParameterError::from_kind(
                image::error::ParameterErrorKind::DimensionMismatch,
            ))
        })?;
    let mut out = Cursor::new(Vec::new());
    buffer.write_to(&mut out, image::ImageFormat::Png)?;
    Ok(out.into_inner())
}

/// Map a clipboard MIME type to the file extension we'd save it under.
///
/// Mirrors the TS `extensionForImageMimeType` helper. Returns `None` for
/// types we don't recognize.
pub fn extension_for_image_mime_type(mime_type: &str) -> Option<&'static str> {
    let base = mime_type.split(';').next()?.trim().to_ascii_lowercase();
    match base.as_str() {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_handles_known_types() {
        assert_eq!(extension_for_image_mime_type("image/png"), Some("png"));
        assert_eq!(extension_for_image_mime_type("image/jpeg"), Some("jpg"));
        assert_eq!(extension_for_image_mime_type("image/webp"), Some("webp"));
        assert_eq!(extension_for_image_mime_type("image/gif"), Some("gif"));
    }

    #[test]
    fn extension_strips_charset_and_lowercases() {
        assert_eq!(
            extension_for_image_mime_type("Image/PNG; charset=binary"),
            Some("png")
        );
    }

    #[test]
    fn extension_returns_none_for_unknown_type() {
        assert_eq!(extension_for_image_mime_type("image/bmp"), None);
        assert_eq!(extension_for_image_mime_type("text/plain"), None);
        assert_eq!(extension_for_image_mime_type(""), None);
    }

    /// Round-trip a small RGBA buffer through `encode_rgba_as_png`. This
    /// runs everywhere — no clipboard needed, only the encoder path.
    #[test]
    fn encode_rgba_produces_valid_png() {
        // 2x2 fully-opaque red.
        let pixels: Vec<u8> = vec![
            255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
        ];
        let data = arboard::ImageData {
            width: 2,
            height: 2,
            bytes: std::borrow::Cow::Borrowed(&pixels),
        };
        let png = encode_rgba_as_png(&data).expect("encode");
        // PNG magic number: 89 50 4E 47 0D 0A 1A 0A
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }

    // The clipboard *read* itself (`read_clipboard_image` → `NSPasteboard` via
    // `arboard`) has no hermetic test on purpose: touching the real pasteboard
    // from arbitrary parallel test threads crashes the macOS process, and CI
    // has no image on the clipboard to assert against. The decode/encode logic
    // above and the paste-decision flow in
    // `modes::interactive::rt_driver::clipboard` (which injects the read
    // result) cover the behaviour that is ours to own.
}
