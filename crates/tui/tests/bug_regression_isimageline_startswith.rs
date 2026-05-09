//! Bug regression — pi-tui's `isImageLine()` historically used `startsWith()`
//! to detect lines that contain image escape sequences. When the terminal did
//! not support images and an upstream component emitted a line that *began*
//! with arbitrary user text but *contained* an iTerm2 / Kitty image escape
//! later in the line, the old `startsWith` check returned `false`. The TUI
//! then ran the line through its width check and crashed with
//! "Rendered line exceeds terminal width (304401 > 115)".
//!
//! pi-tui's fix was to switch to `includes()` so any image escape anywhere in
//! the line is detected.
//!
//! In the Rust port there is **no `is_image_line` function**: the renderer
//! does not branch on whether a line "looks like" an image. The historical
//! crash path therefore cannot reproduce by construction.
//!
//! What we still want to lock here:
//!
//! - `image_fallback` produces output whose width is fully driven by its
//!   `max_cols` option, NOT by any user-provided label. A maliciously long
//!   label must NOT inflate any output line beyond `max_cols`. This pins the
//!   structural property that prevented the original crash from porting over.
//! - `image_fallback` is text-only and never embeds raw image escape
//!   sequences in its output, so the disambiguation problem the TS bug was
//!   guarding against does not exist in the Rust port.

use hand_tui::utils::visible_width;
use hand_tui::{ImageRenderOptions, image_fallback};

/// A 300KB+ user-text label simulates the historical crash scenario where a
/// line "starting with arbitrary text" carried an embedded image sequence.
/// `image_fallback` must clip the label to its declared `max_cols` regardless.
#[test]
fn regression_image_fallback_clips_huge_label_to_max_cols() {
    let huge_label = "A".repeat(300_000);
    let cols: u16 = 40;
    let opts = ImageRenderOptions {
        label: Some(huge_label),
        max_cols: Some(cols),
        max_rows: Some(3),
        ..ImageRenderOptions::default()
    };

    let lines = image_fallback(&opts);

    assert!(!lines.is_empty(), "fallback must produce at least one line");
    for (i, line) in lines.iter().enumerate() {
        let w = visible_width(line);
        assert!(
            w <= cols as usize,
            "fallback line {i} must not exceed max_cols {cols}, got width {w}: {line:?}"
        );
        assert!(
            w >= 8,
            "fallback line {i} must reach the inner-width floor (8), got {w}"
        );
    }
}

/// The fallback must never include raw image escape sequences in its output.
/// pi-tui's bug was detection-side; in the Rust port the rendering side is
/// also clean — the fallback emits only box-drawing + text, no `\x1b]1337` or
/// `\x1b_G` payloads. Locking this rules out the renderer accidentally
/// embedding an image escape into placeholder text in the future.
#[test]
fn regression_image_fallback_emits_no_image_escapes() {
    let opts = ImageRenderOptions {
        label: Some("hello".into()),
        max_cols: Some(40),
        max_rows: Some(3),
        ..ImageRenderOptions::default()
    };

    let lines = image_fallback(&opts);
    let joined = lines.join("\n");

    assert!(
        !joined.contains("\x1b]1337"),
        "fallback must not emit iTerm2 image escape: {joined:?}"
    );
    assert!(
        !joined.contains("\x1b_G"),
        "fallback must not emit Kitty image escape: {joined:?}"
    );
}

/// A label that itself contains image-escape-looking bytes must not fool the
/// fallback into either emitting them verbatim into a line wider than
/// `max_cols`, or otherwise mishandling its width budget. This is the
/// closest-fitting analogue of pi-tui's `isImageLine` regression scenario:
/// arbitrary user input adjacent to image escape bytes must remain bounded.
#[test]
fn regression_image_fallback_handles_user_text_with_image_escape_bytes() {
    let mut sneaky =
        String::from("Read image file [image/jpeg]\x1b]1337;File=size=800,600;inline=1:");
    sneaky.push_str(&"A".repeat(2_000));
    sneaky.push('\x07');

    let cols: u16 = 40;
    let opts = ImageRenderOptions {
        label: Some(sneaky),
        max_cols: Some(cols),
        max_rows: Some(3),
        ..ImageRenderOptions::default()
    };

    let lines = image_fallback(&opts);

    for (i, line) in lines.iter().enumerate() {
        let w = visible_width(line);
        assert!(
            w <= cols as usize,
            "fallback line {i} must clip to max_cols even with image-escape bytes \
             in the label, got width {w}: {line:?}"
        );
    }
}
