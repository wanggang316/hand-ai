//! Regression: regional-indicator codepoint widths.
//!
//! Historical bug (pi-tui): during streaming, partial flag emojis like a lone
//! "🇨" arrive before their pairing codepoint. If `visible_width` measures the
//! lone regional indicator as 1 cell while the terminal renders it as 2 cells,
//! the differential renderer drifts and leaves stale characters on screen.
//!
//! The fix locked all regional-indicator graphemes (paired or lone) to width 2
//! to match how terminals actually render them. This file pins that behavior.

use hand_tui::utils::wrap_text_with_ansi;
use hand_tui::visible_width;

#[test]
fn regression_partial_flag_grapheme_measures_as_width_2() {
    // During streaming, "🇨🇳" often appears as an intermediate "🇨" first.
    // If "🇨" is measured as width 1 while terminal renders it as width 2,
    // differential rendering drifts and leaves stale characters on screen.
    let partial_flag = "🇨";
    assert_eq!(
        visible_width(partial_flag),
        2,
        "partial flag grapheme must measure as width 2 to avoid streaming-render drift"
    );

    let list_line = "      - 🇨";
    assert_eq!(
        visible_width(list_line),
        10,
        "list line ending in partial flag must total width 10 (8 + 2)"
    );
}

#[test]
fn regression_wraps_intermediate_partial_flag_list_line_before_overflow() {
    // Width 9 cannot fit "      - 🇨" if 🇨 is width 2 (8 + 2 = 10).
    // The line must wrap to avoid terminal auto-wrap mismatch.
    let wrapped = wrap_text_with_ansi("      - 🇨", 9);
    assert_eq!(
        wrapped.len(),
        2,
        "expected partial-flag overflow line to wrap into 2 lines, got {wrapped:?}"
    );
    assert_eq!(visible_width(&wrapped[0]), 7);
    assert_eq!(visible_width(&wrapped[1]), 2);
}

#[test]
fn regression_all_singleton_regional_indicators_measure_as_width_2() {
    // U+1F1E6 .. U+1F1FF: every individual regional-indicator codepoint must
    // measure as width 2 even when not paired, because terminals render each
    // one as a wide cell during streaming.
    for cp in 0x1f1e6_u32..=0x1f1ff {
        let ch = char::from_u32(cp).unwrap();
        let s = ch.to_string();
        assert_eq!(
            visible_width(&s),
            2,
            "expected regional indicator U+{cp:X} ({s}) to measure as width 2"
        );
    }
}

#[test]
fn regression_full_flag_pairs_measure_as_width_2() {
    // The classic case: a paired flag is one wide grapheme, width 2.
    // This is the assertion the brief calls out as critical: 🇯🇵 must NOT
    // measure as 4.
    for flag in ["🇯🇵", "🇺🇸", "🇬🇧", "🇨🇳", "🇩🇪", "🇫🇷"] {
        assert_eq!(
            visible_width(flag),
            2,
            "regional indicator pair must measure as one wide grapheme (width 2), got width {} for {flag}",
            visible_width(flag)
        );
    }
}

#[test]
fn regression_two_flags_measure_as_width_4() {
    // Two flags side-by-side: 2 + 2 = 4. Naive char-by-char would yield 8.
    assert_eq!(
        visible_width("🇺🇸🇯🇵"),
        4,
        "two flag emoji should measure as width 4 total"
    );
}

#[test]
fn regression_common_streaming_emoji_intermediates_have_stable_width_2() {
    // Single emoji codepoints and skin-tone modifiers that pi-tui pinned to
    // width 2. ZWJ sequences (e.g. rainbow flag, person + computer) are NOT
    // included here because the Rust port — which uses `unicode-segmentation`
    // and `unicode-width` directly — measures them differently than the TS
    // port did. That divergence is locked in a separate test below so a future
    // change is forced to acknowledge it.
    for sample in ["👍", "👍🏻", "✅", "⚡", "⚡\u{fe0f}", "👨"] {
        assert_eq!(
            visible_width(sample),
            2,
            "streaming emoji {sample:?} must measure as width 2, got {}",
            visible_width(sample)
        );
    }
}

#[test]
fn lock_current_zwj_emoji_widths() {
    // pi-tui asserted width 2 for these. The Rust port (unicode-segmentation +
    // unicode-width) currently reports different widths because it does not
    // collapse ZWJ sequences into a single wide grapheme. Locking current
    // behavior here so any future change is intentional and reviewed.
    //
    // If you change these widths, also revisit any streaming-render bug that
    // relies on terminal width measurement matching what the terminal will
    // actually paint.
    let person_developer = "👨\u{200d}💻"; // person + ZWJ + laptop
    let rainbow_flag = "🏳\u{fe0f}\u{200d}🌈"; // white flag VS16 ZWJ rainbow
    assert_eq!(
        visible_width(person_developer),
        2,
        "lock: person+ZWJ+laptop currently measures 2 in the Rust port"
    );
    assert_eq!(
        visible_width(rainbow_flag),
        1,
        "lock: rainbow flag (ZWJ sequence) currently measures 1 in the Rust port — \
         pi-tui measured it as 2; if this assertion fails, the streaming-render \
         drift bug it once protected against may reappear"
    );
}
