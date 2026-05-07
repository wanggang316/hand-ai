//! Integration tests for `ImageComponent`.

mod common;

use hand_tui::{Component, ImageComponent, ImageOptions, ImageProtocol, ImageTheme};

#[test]
fn empty_image_renders_placeholder_with_default_alt() {
    let comp = ImageComponent::new(ImageProtocol::Fallback);
    let lines = comp.render(80);
    assert!(!lines.is_empty());
    assert!(lines.iter().any(|l| l.contains("[image]")));
}

#[test]
fn custom_alt_text_appears_in_placeholder() {
    let mut comp = ImageComponent::new(ImageProtocol::Fallback);
    comp.set_alt_text("avatar.png");
    let lines = comp.render(80);
    assert!(lines.iter().any(|l| l.contains("avatar.png")));
}

#[test]
fn has_data_tracks_set_image_data() {
    let mut comp = ImageComponent::new(ImageProtocol::Fallback);
    assert!(!comp.has_data());
    comp.set_image_data(vec![0u8; 16], 20, 10);
    assert!(comp.has_data());
}

#[test]
fn options_round_trip_image_id() {
    let comp = ImageComponent::new(ImageProtocol::Fallback).with_options(ImageOptions {
        image_id: Some(42),
        ..ImageOptions::default()
    });
    assert_eq!(comp.image_id(), Some(42));
}

#[test]
fn theme_color_wraps_placeholder_lines() {
    let comp = ImageComponent::new(ImageProtocol::Fallback)
        .with_options(ImageOptions {
            filename: Some("photo.png".into()),
            ..ImageOptions::default()
        })
        .with_theme(ImageTheme {
            fallback_color: Some("\x1b[35m".into()),
        });
    let lines = comp.render(80);
    assert!(lines.iter().any(|l| l.contains("photo.png")));
    assert!(
        lines
            .iter()
            .all(|l| l.contains("\x1b[35m") && l.ends_with("\x1b[0m"))
    );
}
