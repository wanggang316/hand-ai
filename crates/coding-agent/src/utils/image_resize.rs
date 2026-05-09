//! Resize images to fit dimension and base64 byte budgets.
//!
//! Used before sending image attachments to model providers that cap upload
//! size (Anthropic's 5 MB limit is the motivating case). The output is
//! ready to ship: base64-encoded, EXIF-corrected, and always under
//! `max_bytes` when a fit exists.
//!
//! Behaviour parity with `pi-mono/.../image-resize.ts`:
//! - First pass: scale to fit `max_width` x `max_height`.
//! - Try PNG and JPEG (with a quality ladder), pick the first encoding that
//!   lands under `max_bytes`.
//! - If still too large, scale dimensions down by 25% and retry until 1x1.
//! - If the input already fits both the dimension and byte budgets, return
//!   it verbatim with `was_resized = false`.
//! - Decode/encode failures yield `None` rather than an error — same
//!   fall-through the TS version uses.
//!
//! Uses the `image` crate's Lanczos3 filter; the TS version used Photon's
//! identically-named filter so visual output is comparable.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use image::DynamicImage;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::{ColorType, ExtendedColorType, ImageEncoder};

use crate::utils::image_convert::decode_with_exif;

/// Resize options. All fields are optional and fall back to
/// [`ImageResizeOptions::DEFAULT`].
#[derive(Debug, Clone, Copy)]
pub struct ImageResizeOptions {
    /// Maximum output width in pixels.
    pub max_width: u32,
    /// Maximum output height in pixels.
    pub max_height: u32,
    /// Maximum encoded base64 payload size in bytes.
    ///
    /// Default is 4.5 MiB — under Anthropic's 5 MB cap with headroom.
    pub max_bytes: usize,
    /// JPEG quality (0..=100) for the first encoding attempt.
    pub jpeg_quality: u8,
}

impl ImageResizeOptions {
    /// 4.5 MiB of base64 payload. Headroom under Anthropic's 5 MB limit.
    pub const DEFAULT_MAX_BYTES: usize = (4.5 * 1024.0 * 1024.0) as usize;

    /// Default options matching the TypeScript original.
    pub const DEFAULT: Self = Self {
        max_width: 2000,
        max_height: 2000,
        max_bytes: Self::DEFAULT_MAX_BYTES,
        jpeg_quality: 80,
    };
}

impl Default for ImageResizeOptions {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Output of [`resize_image`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResizedImage {
    /// Base64-encoded image bytes.
    pub data: String,
    /// MIME type for the encoded bytes (`image/png` or `image/jpeg`).
    pub mime_type: String,
    /// Width of the source image, after EXIF correction.
    pub original_width: u32,
    /// Height of the source image, after EXIF correction.
    pub original_height: u32,
    /// Width of the encoded output.
    pub width: u32,
    /// Height of the encoded output.
    pub height: u32,
    /// True when the output is a re-encoded copy; false when the input
    /// already fit the budgets and was returned verbatim.
    pub was_resized: bool,
}

impl ResizedImage {
    /// Format a hint for prompts so the model can map screen coordinates
    /// reported against the displayed (resized) image back to the original.
    /// Returns `None` for verbatim outputs since no scaling occurred.
    pub fn dimension_note(&self) -> Option<String> {
        if !self.was_resized {
            return None;
        }
        let scale = self.original_width as f64 / self.width as f64;
        Some(format!(
            "[Image: original {}x{}, displayed at {}x{}. Multiply coordinates by {:.2} to map to original image.]",
            self.original_width, self.original_height, self.width, self.height, scale
        ))
    }
}

/// Source image fed to [`resize_image`].
pub struct ImageInput<'a> {
    /// Base64-encoded source bytes.
    pub data: &'a str,
    /// MIME type the source advertises. Used as a fallback for the output
    /// MIME when the input fits and is returned verbatim.
    pub mime_type: &'a str,
}

/// Resize an image to fit `options`, returning a new encoded payload or the
/// verbatim input if it already fits.
///
/// Returns `None` when the source is undecodable, malformed base64, or
/// cannot be reduced below `max_bytes` even at 1x1. Mirrors the TypeScript
/// `resizeImage` contract.
pub fn resize_image(input: &ImageInput<'_>, options: ImageResizeOptions) -> Option<ResizedImage> {
    let input_base64_size = input.data.len();
    let input_bytes = BASE64_STANDARD.decode(input.data).ok()?;
    let image = decode_with_exif(&input_bytes)?;
    let original_width = image.width();
    let original_height = image.height();

    // Verbatim path: already within both budgets.
    if original_width <= options.max_width
        && original_height <= options.max_height
        && input_base64_size < options.max_bytes
    {
        return Some(ResizedImage {
            data: input.data.to_string(),
            mime_type: input.mime_type.to_string(),
            original_width,
            original_height,
            width: original_width,
            height: original_height,
            was_resized: false,
        });
    }

    // Step 1: clamp to max dimensions, preserving aspect ratio.
    let (mut current_width, mut current_height) = clamp_dimensions(
        original_width,
        original_height,
        options.max_width,
        options.max_height,
    );

    // Step 2: encode at decreasing dimensions until under budget.
    let quality_ladder = quality_ladder(options.jpeg_quality);
    loop {
        let resized = image.resize_exact(current_width, current_height, FilterType::Lanczos3);
        if let Some((data, mime)) =
            first_encoding_under_budget(&resized, &quality_ladder, options.max_bytes)
        {
            return Some(ResizedImage {
                data,
                mime_type: mime.to_string(),
                original_width,
                original_height,
                width: current_width,
                height: current_height,
                was_resized: true,
            });
        }

        if current_width == 1 && current_height == 1 {
            return None;
        }

        let next_width = if current_width == 1 {
            1
        } else {
            ((current_width as f64 * 0.75) as u32).max(1)
        };
        let next_height = if current_height == 1 {
            1
        } else {
            ((current_height as f64 * 0.75) as u32).max(1)
        };
        if next_width == current_width && next_height == current_height {
            return None;
        }
        current_width = next_width;
        current_height = next_height;
    }
}

/// Compute target width/height that fits within `max_w`/`max_h` while
/// preserving aspect ratio. Mirrors the two-pass clamp the TS uses.
fn clamp_dimensions(width: u32, height: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let mut w = width;
    let mut h = height;
    if w > max_w {
        // Round half-up to match the TS `Math.round`.
        h = ((h as u64 * max_w as u64) as f64 / w as f64).round() as u32;
        w = max_w;
    }
    if h > max_h {
        w = ((w as u64 * max_h as u64) as f64 / h as f64).round() as u32;
        h = max_h;
    }
    (w.max(1), h.max(1))
}

/// JPEG quality steps tried in order. Deduplicated so a `jpeg_quality`
/// value already in the default ladder doesn't waste an encode pass.
fn quality_ladder(jpeg_quality: u8) -> Vec<u8> {
    let mut ladder = vec![jpeg_quality];
    for q in [85u8, 70, 55, 40] {
        if !ladder.contains(&q) {
            ladder.push(q);
        }
    }
    ladder
}

/// Encode `image` as PNG and at each JPEG quality, return the first base64
/// payload that fits under `max_bytes`. Encoding failures are silently
/// skipped (other formats may still succeed).
fn first_encoding_under_budget(
    image: &DynamicImage,
    quality_ladder: &[u8],
    max_bytes: usize,
) -> Option<(String, &'static str)> {
    if let Some(candidate) = encode_png_base64(image)
        && candidate.len() < max_bytes
    {
        return Some((candidate, "image/png"));
    }
    for &quality in quality_ladder {
        if let Some(candidate) = encode_jpeg_base64(image, quality)
            && candidate.len() < max_bytes
        {
            return Some((candidate, "image/jpeg"));
        }
    }
    None
}

fn encode_png_base64(image: &DynamicImage) -> Option<String> {
    let rgba = image.to_rgba8();
    let mut buf = Vec::new();
    PngEncoder::new(&mut buf)
        .write_image(
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
            ExtendedColorType::Rgba8,
        )
        .ok()?;
    Some(BASE64_STANDARD.encode(&buf))
}

fn encode_jpeg_base64(image: &DynamicImage, quality: u8) -> Option<String> {
    // JPEG has no alpha channel; flatten to RGB.
    let rgb = image.to_rgb8();
    let mut buf = Vec::new();
    let encoder = JpegEncoder::new_with_quality(&mut buf, quality);
    encoder
        .write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            ColorType::Rgb8.into(),
        )
        .ok()?;
    Some(BASE64_STANDARD.encode(&buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgb, RgbImage};
    use std::io::Cursor;

    /// Encode a freshly built RGB fixture in `format` and base64 it.
    fn encode_fixture_base64(width: u32, height: u32, format: ImageFormat) -> String {
        let mut img = RgbImage::new(width, height);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            *pixel = Rgb([
                ((x * 7) % 256) as u8,
                ((y * 11) % 256) as u8,
                (((x + y) * 13) % 256) as u8,
            ]);
        }
        let mut bytes = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut bytes), format)
            .expect("test fixture encodes cleanly");
        BASE64_STANDARD.encode(&bytes)
    }

    #[test]
    fn small_image_within_budget_is_returned_verbatim() {
        let data = encode_fixture_base64(50, 40, ImageFormat::Png);
        let result = resize_image(
            &ImageInput {
                data: &data,
                mime_type: "image/png",
            },
            ImageResizeOptions::DEFAULT,
        )
        .expect("small image succeeds");
        assert!(!result.was_resized);
        assert_eq!(result.width, 50);
        assert_eq!(result.height, 40);
        assert_eq!(result.data, data);
        assert_eq!(result.mime_type, "image/png");
    }

    #[test]
    fn oversized_dimensions_trigger_resize() {
        let data = encode_fixture_base64(300, 200, ImageFormat::Png);
        let opts = ImageResizeOptions {
            max_width: 100,
            max_height: 100,
            ..ImageResizeOptions::DEFAULT
        };
        let result = resize_image(
            &ImageInput {
                data: &data,
                mime_type: "image/png",
            },
            opts,
        )
        .expect("resize succeeds");
        assert!(result.was_resized);
        // 300x200 clamped to width=100 -> height=round(200*100/300)=67.
        assert_eq!(result.width, 100);
        assert_eq!(result.height, 67);
        assert_eq!(result.original_width, 300);
        assert_eq!(result.original_height, 200);
    }

    #[test]
    fn byte_budget_can_force_resize_below_max_dimensions() {
        // Small dimensions but a tiny byte budget → must downscale.
        let data = encode_fixture_base64(200, 200, ImageFormat::Png);
        let opts = ImageResizeOptions {
            max_width: 1000,
            max_height: 1000,
            max_bytes: 256,
            jpeg_quality: 40,
        };
        let result = resize_image(
            &ImageInput {
                data: &data,
                mime_type: "image/png",
            },
            opts,
        );
        // Either the loop finds a fit and was_resized=true, or returns
        // None when 1x1 also overflows; both are valid for a 256-byte cap.
        if let Some(r) = result {
            assert!(r.was_resized);
            assert!(r.data.len() < 256, "payload {} >= max 256", r.data.len());
        }
    }

    #[test]
    fn malformed_base64_is_none() {
        let result = resize_image(
            &ImageInput {
                data: "@@@",
                mime_type: "image/png",
            },
            ImageResizeOptions::DEFAULT,
        );
        assert!(result.is_none());
    }

    #[test]
    fn undecodable_payload_is_none() {
        let payload = BASE64_STANDARD.encode(b"nonsense");
        let result = resize_image(
            &ImageInput {
                data: &payload,
                mime_type: "image/png",
            },
            ImageResizeOptions::DEFAULT,
        );
        assert!(result.is_none());
    }

    #[test]
    fn dimension_note_only_when_resized() {
        let unchanged = ResizedImage {
            data: String::new(),
            mime_type: "image/png".to_string(),
            original_width: 100,
            original_height: 80,
            width: 100,
            height: 80,
            was_resized: false,
        };
        assert!(unchanged.dimension_note().is_none());

        let changed = ResizedImage {
            data: String::new(),
            mime_type: "image/png".to_string(),
            original_width: 200,
            original_height: 160,
            width: 100,
            height: 80,
            was_resized: true,
        };
        let note = changed.dimension_note().expect("resized image has a note");
        assert!(note.contains("200x160"));
        assert!(note.contains("100x80"));
        assert!(note.contains("2.00"));
    }

    #[test]
    fn quality_ladder_dedups_default_value() {
        // 85 already in the static ladder; passing it should not duplicate.
        let ladder = quality_ladder(85);
        let count = ladder.iter().filter(|&&q| q == 85).count();
        assert_eq!(count, 1, "ladder = {ladder:?}");
    }

    #[test]
    fn clamp_preserves_aspect_ratio() {
        assert_eq!(clamp_dimensions(400, 200, 100, 100), (100, 50));
        assert_eq!(clamp_dimensions(200, 400, 100, 100), (50, 100));
        assert_eq!(clamp_dimensions(50, 40, 100, 100), (50, 40));
    }
}
