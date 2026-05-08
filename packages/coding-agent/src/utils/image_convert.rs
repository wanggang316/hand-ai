//! Convert arbitrary image bytes into PNG, applying any EXIF orientation.
//!
//! The Kitty graphics protocol (`f=100`) requires PNG input. Terminal-
//! rendering callers feed in whatever the user pasted (JPEG, WebP, GIF, ...)
//! and need a normalised PNG payload back.
//!
//! Behaviour parity with `pi-mono/.../image-convert.ts`:
//! - PNG input is short-circuited (no decode/re-encode).
//! - Decode failures, encode failures, or unsupported MIME types yield
//!   `Ok(None)` rather than an error — matches the TS `try { ... } catch {
//!   return null }` pattern. The caller decides how to react (typically:
//!   render the original bytes verbatim).
//! - EXIF orientation is applied during conversion so the rendered PNG is
//!   already upright.
//!
//! Replaces the photon-WASM round-trip the TypeScript original uses with
//! native decoding via the `image` crate.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use image::{DynamicImage, ImageFormat, ImageReader};
use std::io::Cursor;

use crate::utils::exif_orientation::{ExifTransform, read_exif_orientation};

/// PNG-encoded image with the MIME type the caller should advertise.
///
/// `mime_type` is always `"image/png"` for converted output, but the field
/// is kept on the struct so the no-op short-circuit case can preserve the
/// original MIME without forcing the caller to special-case it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertedImage {
    /// Base64-encoded image bytes.
    pub data: String,
    /// MIME type for the encoded bytes.
    pub mime_type: String,
}

/// Convert `base64_data` (with declared `mime_type`) into a base64 PNG.
///
/// Returns `Ok(None)` when conversion is not possible — the source decoded
/// to no recognisable image, the output PNG could not be encoded, or the
/// base64 was malformed. Callers should treat `None` as "fall back to the
/// original bytes" the same way the TypeScript original does.
///
/// PNG input is returned verbatim without redecoding; even though that
/// skips the EXIF correction step, PNG containers do not carry EXIF
/// orientation in practice and the TS implementation makes the same trade.
pub fn convert_to_png(base64_data: &str, mime_type: &str) -> Option<ConvertedImage> {
    if mime_type == "image/png" {
        return Some(ConvertedImage {
            data: base64_data.to_string(),
            mime_type: mime_type.to_string(),
        });
    }

    let bytes = BASE64_STANDARD.decode(base64_data).ok()?;
    let png_bytes = decode_apply_exif_encode_png(&bytes)?;
    Some(ConvertedImage {
        data: BASE64_STANDARD.encode(&png_bytes),
        mime_type: "image/png".to_string(),
    })
}

/// Decode `bytes`, apply the EXIF transform from the same byte stream,
/// and re-encode as PNG.
///
/// Returns `None` when any step fails. Kept `pub(crate)` so `image_resize`
/// can share the EXIF-aware decode path.
pub(crate) fn decode_apply_exif_encode_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let image = decode_with_exif(bytes)?;
    let mut out = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .ok()?;
    Some(out)
}

/// Decode `bytes` into a `DynamicImage` and apply the EXIF orientation.
///
/// The `image` crate's `with_guessed_format` sniffs the container so callers
/// don't need to know the MIME up front. Format-detection failures and
/// decode failures both surface as `None` — same fall-through the TS uses.
pub(crate) fn decode_with_exif(bytes: &[u8]) -> Option<DynamicImage> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let image = reader.decode().ok()?;
    Some(apply_exif_transform(
        image,
        read_exif_orientation(bytes).transform(),
    ))
}

/// Apply an [`ExifTransform`] to a decoded image.
///
/// Public-in-crate so resize callers can run the same correction without
/// re-parsing the EXIF block.
pub(crate) fn apply_exif_transform(image: DynamicImage, transform: ExifTransform) -> DynamicImage {
    match transform {
        ExifTransform::Identity => image,
        ExifTransform::FlipHorizontal => image.fliph(),
        ExifTransform::FlipVertical => image.flipv(),
        ExifTransform::Rotate180 => image.rotate180(),
        ExifTransform::Rotate90 => image.rotate90(),
        ExifTransform::Rotate90ThenFlipHorizontal => image.rotate90().fliph(),
        ExifTransform::Rotate270 => image.rotate270(),
        ExifTransform::Rotate270ThenFlipHorizontal => image.rotate270().fliph(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgb, RgbImage};

    fn encode_to_format(image: &RgbImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(image.clone())
            .write_to(&mut Cursor::new(&mut out), format)
            .expect("test fixture encodes cleanly");
        out
    }

    fn rgb_fixture() -> RgbImage {
        let mut img = RgbImage::new(4, 3);
        // Distinctive pattern so we can detect orientation changes.
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            *pixel = Rgb([(x * 60) as u8, (y * 80) as u8, ((x + y) * 30) as u8]);
        }
        img
    }

    #[test]
    fn png_input_short_circuits_without_redecoding() {
        // Use a payload that is NOT a valid PNG — the short-circuit path
        // must not touch the bytes, only the MIME type matters.
        let opaque = "not-actually-png-bytes";
        let result = convert_to_png(opaque, "image/png").expect("PNG path returns Some");
        assert_eq!(result.data, opaque);
        assert_eq!(result.mime_type, "image/png");
    }

    #[test]
    fn jpeg_input_round_trips_to_png() {
        let jpeg_bytes = encode_to_format(&rgb_fixture(), ImageFormat::Jpeg);
        let base64 = BASE64_STANDARD.encode(&jpeg_bytes);

        let result = convert_to_png(&base64, "image/jpeg").expect("JPEG decodes cleanly");
        assert_eq!(result.mime_type, "image/png");

        let decoded = BASE64_STANDARD
            .decode(&result.data)
            .expect("output is valid base64");
        let reread = ImageReader::new(Cursor::new(&decoded))
            .with_guessed_format()
            .expect("output bytes are sniffable")
            .decode()
            .expect("output bytes are a valid PNG");
        assert_eq!(reread.width(), 4);
        assert_eq!(reread.height(), 3);
    }

    #[test]
    fn malformed_base64_yields_none() {
        // `!!!` is not valid base64 alphabet.
        assert_eq!(convert_to_png("!!!", "image/jpeg"), None);
    }

    #[test]
    fn undecodable_bytes_yield_none() {
        // Valid base64, but the decoded payload is not a recognisable image.
        let payload = BASE64_STANDARD.encode(b"definitely not an image");
        assert_eq!(convert_to_png(&payload, "image/jpeg"), None);
    }

    #[test]
    fn webp_input_round_trips_to_png() {
        let webp_bytes = encode_to_format(&rgb_fixture(), ImageFormat::WebP);
        let base64 = BASE64_STANDARD.encode(&webp_bytes);

        let result = convert_to_png(&base64, "image/webp").expect("WebP decodes cleanly");
        assert_eq!(result.mime_type, "image/png");

        let decoded = BASE64_STANDARD
            .decode(&result.data)
            .expect("output is valid base64");
        // Header bytes for PNG.
        assert_eq!(
            &decoded[..8],
            &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
        );
    }

    #[test]
    fn rotate_90_changes_dimensions() {
        let image = DynamicImage::ImageRgb8(rgb_fixture()); // 4x3
        let rotated = apply_exif_transform(image, ExifTransform::Rotate90);
        assert_eq!(rotated.width(), 3);
        assert_eq!(rotated.height(), 4);
    }

    #[test]
    fn identity_transform_preserves_pixels() {
        let image = DynamicImage::ImageRgb8(rgb_fixture());
        let unchanged = apply_exif_transform(image.clone(), ExifTransform::Identity);
        assert_eq!(image.to_rgb8(), unchanged.to_rgb8());
    }
}
