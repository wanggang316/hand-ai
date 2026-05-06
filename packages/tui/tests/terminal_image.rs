//! Integration tests for the terminal-image utilities.

mod common;

use hand_tui::{
    ImageDimensions, allocate_image_id, calculate_image_rows, delete_all_kitty_images,
    delete_kitty_image, get_gif_dimensions, get_image_dimensions, get_jpeg_dimensions,
    get_png_dimensions, hyperlink,
};

#[test]
fn png_dimensions_parsed_from_signature() {
    let mut data = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    data.extend_from_slice(&[0u8; 8]); // chunk len + tag
    data.extend_from_slice(&100u32.to_be_bytes()); // width
    data.extend_from_slice(&50u32.to_be_bytes()); // height
    let dims = get_png_dimensions(&data).unwrap();
    assert_eq!(dims, ImageDimensions { width: 100, height: 50 });
}

#[test]
fn jpeg_dimensions_parsed_from_sof_marker() {
    // Minimal JPEG with an SOF0 marker. The parser reads bytes
    // [offset+5..offset+9] for height/width, and the loop guard requires at
    // least `offset + 10` bytes.
    let mut data = vec![0xff, 0xd8]; // SOI
    data.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]); // SOF0 marker, length, precision
    data.extend_from_slice(&60u16.to_be_bytes()); // height
    data.extend_from_slice(&80u16.to_be_bytes()); // width
    data.extend_from_slice(&[0u8; 16]); // pad so offset+9 < len
    let dims = get_jpeg_dimensions(&data).unwrap();
    assert_eq!(dims.width, 80);
    assert_eq!(dims.height, 60);
}

#[test]
fn gif_dimensions_parsed_from_header() {
    let mut data = b"GIF89a".to_vec();
    data.extend_from_slice(&200u16.to_le_bytes());
    data.extend_from_slice(&150u16.to_le_bytes());
    let dims = get_gif_dimensions(&data).unwrap();
    assert_eq!(dims, ImageDimensions { width: 200, height: 150 });
}

#[test]
fn get_image_dimensions_dispatches_by_format() {
    let mut png = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    png.extend_from_slice(&[0u8; 8]);
    png.extend_from_slice(&10u32.to_be_bytes());
    png.extend_from_slice(&20u32.to_be_bytes());
    assert_eq!(
        get_image_dimensions(&png),
        Some(ImageDimensions { width: 10, height: 20 })
    );
    assert_eq!(get_image_dimensions(b"not an image"), None);
}

#[test]
fn allocate_image_id_returns_unique_values() {
    let a = allocate_image_id();
    let b = allocate_image_id();
    assert_ne!(a, b);
}

#[test]
fn calculate_image_rows_handles_zero_dimensions() {
    let zero = ImageDimensions { width: 0, height: 0 };
    assert_eq!(calculate_image_rows(&zero, None), 1);
}

#[test]
fn calculate_image_rows_clamps_to_max() {
    let big = ImageDimensions { width: 1000, height: 100_000 };
    assert!(calculate_image_rows(&big, Some(20)) <= 20);
}

#[test]
fn hyperlink_wraps_text_in_osc8_envelope() {
    let out = hyperlink("click", "https://example.com");
    assert!(out.starts_with("\x1b]8;;https://example.com\x1b\\"));
    assert!(out.ends_with("\x1b]8;;\x1b\\"));
    assert!(out.contains("click"));
}

#[test]
fn delete_helpers_emit_kitty_apc_sequences() {
    let one = delete_kitty_image(7);
    assert!(one.starts_with("\x1b_G"));
    assert!(one.contains("i=7"));
    let all = delete_all_kitty_images();
    assert!(all.starts_with("\x1b_G"));
    assert!(all.contains("a=d"));
}
