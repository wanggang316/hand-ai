//! EXIF orientation extraction for JPEG and WebP byte streams.
//!
//! Reads the raw container bytes (no full image decode) to find the TIFF
//! header, walks the IFD entries, and pulls the orientation tag (0x0112).
//! Returns a transform describing the rotation/flip the consumer should
//! apply when rendering the image upright.
//!
//! Implemented by hand rather than via a heavy EXIF crate to keep the
//! dependency footprint small.
//!
//! Supported containers:
//! - JPEG: `FF D8 FF` SOI + APP1 (`FF E1`) segment carrying `Exif\0\0` + TIFF.
//! - WebP: RIFF/WEBP with an `EXIF` chunk, optionally prefixed with the
//!   `Exif\0\0` marker before the TIFF header.
//!
//! Other containers (PNG, GIF, ...) return [`ExifTransform::Identity`].

/// EXIF orientation tag value as defined by TIFF 6.0.
///
/// The numeric value 1..=8 matches the on-disk tag verbatim. Values outside
/// that range are reported as [`ExifOrientation::TopLeft`] (identity), the
/// same fall-through the JPEG/WebP decoders use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExifOrientation {
    /// 1: row 0 top, column 0 left (no transform).
    TopLeft,
    /// 2: row 0 top, column 0 right (horizontal flip).
    TopRight,
    /// 3: row 0 bottom, column 0 right (180 degree rotation).
    BottomRight,
    /// 4: row 0 bottom, column 0 left (vertical flip).
    BottomLeft,
    /// 5: row 0 left, column 0 top (transpose: rotate 90 CW + horizontal flip).
    LeftTop,
    /// 6: row 0 right, column 0 top (rotate 90 CW).
    RightTop,
    /// 7: row 0 right, column 0 bottom (transverse: rotate 90 CCW + horizontal flip).
    RightBottom,
    /// 8: row 0 left, column 0 bottom (rotate 90 CCW).
    LeftBottom,
}

impl ExifOrientation {
    fn from_raw(value: u16) -> Self {
        match value {
            2 => Self::TopRight,
            3 => Self::BottomRight,
            4 => Self::BottomLeft,
            5 => Self::LeftTop,
            6 => Self::RightTop,
            7 => Self::RightBottom,
            8 => Self::LeftBottom,
            _ => Self::TopLeft,
        }
    }

    /// True when applying this orientation is a no-op.
    pub fn is_identity(self) -> bool {
        matches!(self, Self::TopLeft)
    }

    /// Reduce to the minimal set of pixel operations needed to render the
    /// image upright. Image-library-agnostic so callers can map onto whatever
    /// rotate/flip primitives they have.
    pub fn transform(self) -> ExifTransform {
        match self {
            Self::TopLeft => ExifTransform::Identity,
            Self::TopRight => ExifTransform::FlipHorizontal,
            Self::BottomRight => ExifTransform::Rotate180,
            Self::BottomLeft => ExifTransform::FlipVertical,
            Self::LeftTop => ExifTransform::Rotate90ThenFlipHorizontal,
            Self::RightTop => ExifTransform::Rotate90,
            Self::RightBottom => ExifTransform::Rotate270ThenFlipHorizontal,
            Self::LeftBottom => ExifTransform::Rotate270,
        }
    }
}

/// Concrete pixel-space transform a renderer should apply.
///
/// Tagged enum (one variant per primitive sequence) so callers can `match`
/// without juggling boolean flags. Rotations are clockwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExifTransform {
    /// No-op.
    Identity,
    /// Mirror across the vertical axis.
    FlipHorizontal,
    /// Mirror across the horizontal axis.
    FlipVertical,
    /// Rotate 180 degrees (equivalent to flip-h + flip-v).
    Rotate180,
    /// Rotate 90 degrees clockwise.
    Rotate90,
    /// Rotate 90 degrees clockwise, then mirror horizontally.
    Rotate90ThenFlipHorizontal,
    /// Rotate 270 degrees clockwise (90 counter-clockwise).
    Rotate270,
    /// Rotate 270 degrees clockwise, then mirror horizontally.
    Rotate270ThenFlipHorizontal,
}

const EXIF_HEADER: [u8; 6] = [b'E', b'x', b'i', b'f', 0x00, 0x00];
const ORIENTATION_TAG: u16 = 0x0112;

/// Read the EXIF orientation tag from raw image bytes.
///
/// Returns [`ExifOrientation::TopLeft`] when:
/// - The bytes are not a recognised JPEG/WebP container.
/// - The container has no EXIF/TIFF block.
/// - The TIFF block is malformed or truncated.
/// - The orientation value is outside the 1..=8 range defined by TIFF 6.0.
///
/// This mirrors the TypeScript original which silently falls back to "no
/// transform" rather than surfacing parse errors.
pub fn read_exif_orientation(bytes: &[u8]) -> ExifOrientation {
    let tiff_offset = if is_jpeg(bytes) {
        find_jpeg_tiff_offset(bytes)
    } else if is_webp(bytes) {
        find_webp_tiff_offset(bytes)
    } else {
        None
    };

    match tiff_offset {
        Some(offset) => read_orientation_from_tiff(bytes, offset),
        None => ExifOrientation::TopLeft,
    }
}

fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xd8
}

fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
}

fn has_exif_header(bytes: &[u8], offset: usize) -> bool {
    offset + EXIF_HEADER.len() <= bytes.len()
        && bytes[offset..offset + EXIF_HEADER.len()] == EXIF_HEADER
}

fn find_jpeg_tiff_offset(bytes: &[u8]) -> Option<usize> {
    // Skip SOI (FF D8). Walk APP segments looking for APP1 (FF E1) carrying EXIF.
    let mut offset: usize = 2;
    while offset + 1 < bytes.len() {
        if bytes[offset] != 0xff {
            return None;
        }
        let marker = bytes[offset + 1];
        if marker == 0xff {
            // Padding byte, skip.
            offset += 1;
            continue;
        }

        if marker == 0xe1 {
            // APP1 segment. Layout: FF E1 <len_hi> <len_lo> "Exif\0\0" <tiff>
            let segment_start = offset.checked_add(4)?;
            if !has_exif_header(bytes, segment_start) {
                return None;
            }
            return Some(segment_start + EXIF_HEADER.len());
        }

        // Other APPn / DQT / SOF segments. Length covers the two bytes of
        // length itself so the next marker sits at offset + 2 + length.
        let length_pos = offset.checked_add(2)?;
        if length_pos + 1 >= bytes.len() {
            return None;
        }
        let length = ((bytes[length_pos] as usize) << 8) | (bytes[length_pos + 1] as usize);
        offset = offset.checked_add(2)?.checked_add(length)?;
    }
    None
}

fn find_webp_tiff_offset(bytes: &[u8]) -> Option<usize> {
    // RIFF header is 12 bytes (`RIFF<size>WEBP`). Chunks follow as
    // <id:4><size:le32><payload> with even-byte padding.
    let mut offset: usize = 12;
    while offset + 8 <= bytes.len() {
        let chunk_id = &bytes[offset..offset + 4];
        let chunk_size = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]) as usize;
        let data_start = offset + 8;

        if chunk_id == b"EXIF" {
            let data_end = data_start.checked_add(chunk_size)?;
            if data_end > bytes.len() {
                return None;
            }
            // Some encoders prefix the TIFF block with the same `Exif\0\0`
            // marker JPEGs use; others omit it.
            if chunk_size >= EXIF_HEADER.len() && has_exif_header(bytes, data_start) {
                return Some(data_start + EXIF_HEADER.len());
            }
            return Some(data_start);
        }

        // Pad odd-sized chunks to the next even boundary.
        let pad = chunk_size & 1;
        offset = data_start.checked_add(chunk_size)?.checked_add(pad)?;
    }
    None
}

fn read_orientation_from_tiff(bytes: &[u8], tiff_start: usize) -> ExifOrientation {
    if tiff_start + 8 > bytes.len() {
        return ExifOrientation::TopLeft;
    }

    // Byte order marker: II (0x4949) = little endian, MM (0x4d4d) = big endian.
    let byte_order = ((bytes[tiff_start] as u16) << 8) | (bytes[tiff_start + 1] as u16);
    let little_endian = byte_order == 0x4949;

    let read_u16 = |pos: usize| -> Option<u16> {
        if pos + 1 >= bytes.len() {
            return None;
        }
        Some(if little_endian {
            u16::from_le_bytes([bytes[pos], bytes[pos + 1]])
        } else {
            u16::from_be_bytes([bytes[pos], bytes[pos + 1]])
        })
    };

    let read_u32 = |pos: usize| -> Option<u32> {
        if pos + 3 >= bytes.len() {
            return None;
        }
        Some(if little_endian {
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
        } else {
            u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
        })
    };

    let ifd_offset = match read_u32(tiff_start + 4) {
        Some(v) => v as usize,
        None => return ExifOrientation::TopLeft,
    };
    let ifd_start = match tiff_start.checked_add(ifd_offset) {
        Some(v) => v,
        None => return ExifOrientation::TopLeft,
    };
    if ifd_start + 2 > bytes.len() {
        return ExifOrientation::TopLeft;
    }

    let entry_count = match read_u16(ifd_start) {
        Some(v) => v as usize,
        None => return ExifOrientation::TopLeft,
    };

    for i in 0..entry_count {
        // Each IFD entry is 12 bytes: tag(2) + type(2) + count(4) + value(4).
        let entry_pos = match ifd_start.checked_add(2).and_then(|p| p.checked_add(i * 12)) {
            Some(p) => p,
            None => return ExifOrientation::TopLeft,
        };
        if entry_pos + 12 > bytes.len() {
            return ExifOrientation::TopLeft;
        }

        let tag = match read_u16(entry_pos) {
            Some(v) => v,
            None => return ExifOrientation::TopLeft,
        };
        if tag == ORIENTATION_TAG {
            // Orientation is a SHORT; the value sits in the low 2 bytes of
            // the value field at entry_pos + 8.
            let raw = match read_u16(entry_pos + 8) {
                Some(v) => v,
                None => return ExifOrientation::TopLeft,
            };
            return ExifOrientation::from_raw(raw);
        }
    }

    ExifOrientation::TopLeft
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal JPEG byte stream with a single APP1 segment whose
    /// TIFF block carries the orientation tag set to `value`.
    ///
    /// Layout:
    ///   FF D8                      SOI
    ///   FF E1 <seg_len:2 BE>       APP1 marker + segment length
    ///   "Exif\0\0"                 EXIF identifier
    ///   <tiff block>               TIFF header + 1 IFD entry
    ///
    /// TIFF block (little-endian):
    ///   "II" 0x002a                BOM + magic
    ///   00000008                   IFD offset (relative to TIFF start)
    ///   0001                       1 IFD entry
    ///   0112 0003 00000001 <val:2> 0000   orientation entry
    fn build_jpeg_with_orientation(value: u16) -> Vec<u8> {
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II"); // little endian
        tiff.extend_from_slice(&0x002au16.to_le_bytes()); // magic
        tiff.extend_from_slice(&0x00000008u32.to_le_bytes()); // ifd offset
        tiff.extend_from_slice(&0x0001u16.to_le_bytes()); // 1 entry
        tiff.extend_from_slice(&ORIENTATION_TAG.to_le_bytes()); // tag
        tiff.extend_from_slice(&0x0003u16.to_le_bytes()); // type SHORT
        tiff.extend_from_slice(&0x00000001u32.to_le_bytes()); // count
        tiff.extend_from_slice(&value.to_le_bytes()); // value (low 2 bytes)
        tiff.extend_from_slice(&[0x00, 0x00]); // value padding to 4 bytes

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0xff, 0xd8]); // SOI
        bytes.extend_from_slice(&[0xff, 0xe1]); // APP1
        let segment_payload_len = (EXIF_HEADER.len() + tiff.len() + 2) as u16;
        bytes.extend_from_slice(&segment_payload_len.to_be_bytes());
        bytes.extend_from_slice(&EXIF_HEADER);
        bytes.extend_from_slice(&tiff);
        bytes
    }

    fn build_webp_with_orientation(value: u16, with_exif_header: bool) -> Vec<u8> {
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&0x002au16.to_le_bytes());
        tiff.extend_from_slice(&0x00000008u32.to_le_bytes());
        tiff.extend_from_slice(&0x0001u16.to_le_bytes());
        tiff.extend_from_slice(&ORIENTATION_TAG.to_le_bytes());
        tiff.extend_from_slice(&0x0003u16.to_le_bytes());
        tiff.extend_from_slice(&0x00000001u32.to_le_bytes());
        tiff.extend_from_slice(&value.to_le_bytes());
        tiff.extend_from_slice(&[0x00, 0x00]);

        let mut chunk_payload = Vec::new();
        if with_exif_header {
            chunk_payload.extend_from_slice(&EXIF_HEADER);
        }
        chunk_payload.extend_from_slice(&tiff);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&0u32.to_le_bytes()); // file size — irrelevant for parsing
        bytes.extend_from_slice(b"WEBP");
        bytes.extend_from_slice(b"EXIF");
        bytes.extend_from_slice(&(chunk_payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&chunk_payload);
        // Pad to even boundary to mimic real RIFF encoders.
        if chunk_payload.len() % 2 == 1 {
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn empty_bytes_default_to_identity() {
        assert_eq!(read_exif_orientation(&[]), ExifOrientation::TopLeft);
    }

    #[test]
    fn unknown_container_is_identity() {
        // A PNG signature; no EXIF support in this module.
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        assert_eq!(read_exif_orientation(&png), ExifOrientation::TopLeft);
    }

    #[test]
    fn jpeg_orientation_each_value_round_trips() {
        let cases = [
            (1u16, ExifOrientation::TopLeft),
            (2, ExifOrientation::TopRight),
            (3, ExifOrientation::BottomRight),
            (4, ExifOrientation::BottomLeft),
            (5, ExifOrientation::LeftTop),
            (6, ExifOrientation::RightTop),
            (7, ExifOrientation::RightBottom),
            (8, ExifOrientation::LeftBottom),
        ];
        for (raw, expected) in cases {
            let bytes = build_jpeg_with_orientation(raw);
            assert_eq!(read_exif_orientation(&bytes), expected, "raw={raw}");
        }
    }

    #[test]
    fn jpeg_out_of_range_orientation_falls_back_to_identity() {
        let bytes = build_jpeg_with_orientation(42);
        assert_eq!(read_exif_orientation(&bytes), ExifOrientation::TopLeft);
    }

    #[test]
    fn jpeg_without_app1_returns_identity() {
        // SOI followed by SOS without any APP1 segment.
        let bytes = [0xff, 0xd8, 0xff, 0xda, 0x00, 0x02];
        assert_eq!(read_exif_orientation(&bytes), ExifOrientation::TopLeft);
    }

    #[test]
    fn webp_orientation_with_exif_marker() {
        let bytes = build_webp_with_orientation(6, true);
        assert_eq!(read_exif_orientation(&bytes), ExifOrientation::RightTop);
    }

    #[test]
    fn webp_orientation_without_exif_marker() {
        let bytes = build_webp_with_orientation(8, false);
        assert_eq!(read_exif_orientation(&bytes), ExifOrientation::LeftBottom);
    }

    #[test]
    fn webp_without_exif_chunk_is_identity() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"WEBP");
        // VP8 chunk with empty payload.
        bytes.extend_from_slice(b"VP8 ");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(read_exif_orientation(&bytes), ExifOrientation::TopLeft);
    }

    #[test]
    fn truncated_tiff_block_is_identity() {
        // Valid JPEG + APP1 header, but TIFF block cut off after the BOM.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0xff, 0xd8, 0xff, 0xe1, 0x00, 0x10]);
        bytes.extend_from_slice(&EXIF_HEADER);
        bytes.extend_from_slice(b"II");
        // Missing the rest of the TIFF header.
        assert_eq!(read_exif_orientation(&bytes), ExifOrientation::TopLeft);
    }

    #[test]
    fn transform_mapping_matches_orientation_semantics() {
        assert_eq!(
            ExifOrientation::TopLeft.transform(),
            ExifTransform::Identity
        );
        assert_eq!(
            ExifOrientation::TopRight.transform(),
            ExifTransform::FlipHorizontal
        );
        assert_eq!(
            ExifOrientation::BottomRight.transform(),
            ExifTransform::Rotate180
        );
        assert_eq!(
            ExifOrientation::BottomLeft.transform(),
            ExifTransform::FlipVertical
        );
        assert_eq!(
            ExifOrientation::RightTop.transform(),
            ExifTransform::Rotate90
        );
        assert_eq!(
            ExifOrientation::LeftBottom.transform(),
            ExifTransform::Rotate270
        );
        assert_eq!(
            ExifOrientation::LeftTop.transform(),
            ExifTransform::Rotate90ThenFlipHorizontal
        );
        assert_eq!(
            ExifOrientation::RightBottom.transform(),
            ExifTransform::Rotate270ThenFlipHorizontal
        );
    }

    #[test]
    fn is_identity_only_for_top_left() {
        assert!(ExifOrientation::TopLeft.is_identity());
        for o in [
            ExifOrientation::TopRight,
            ExifOrientation::BottomRight,
            ExifOrientation::BottomLeft,
            ExifOrientation::LeftTop,
            ExifOrientation::RightTop,
            ExifOrientation::RightBottom,
            ExifOrientation::LeftBottom,
        ] {
            assert!(!o.is_identity(), "{o:?}");
        }
    }
}
