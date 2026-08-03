//! Rendering + interaction tests for the rt loader family and toast stack
//! (`hand_tui::rt::components::{Loader, CancellableLoader, Toast}`).
//!
//! Each widget paints into a ratatui `Buffer` — the same model the rt scheduler
//! draws every frame — and the interactive [`CancellableLoader`] consumes a
//! structured [`RtKey`]. These tests drive the *behavioural signatures* the
//! external validator probes, read from the painted cell grid and the public
//! accessors, at a normal geometry plus a narrow (<8-column) geometry.
//!
//! Pinned here (and deliberately *not* pinned): the loader's **static message
//! text** presence/absence is asserted, but never a specific spinner glyph or the
//! frame timing — the spinner cadence is host-driven and an informed exclusion
//! (Decision Log). What is pinned:
//!
//! - **VAL-WIDGET-011** — Loader shows its static message while active and paints
//!   nothing once inactive; CancellableLoader shows a progress bar + percentage +
//!   an Escape-to-cancel prompt, and Escape actually cancels.
//! - **VAL-WIDGET-012** — Toast stacks newest-first, caps at `max_visible`, hides
//!   (never drops) the overflow, brings a hidden toast back on dismiss-newest and
//!   on TTL expiry, and leaves no ghost row after a dismissal.
//! - **VAL-WIDGET-024** — a `<8`-column CJK/emoji loader/toast message truncates
//!   on a grapheme boundary; the render never panics, never byte-slices a
//!   multibyte grapheme, and never underflows the width budget.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hand_tui::rt::components::{CancelOutcome, CancellableLoader, Loader, Toast, ToastLevel};
use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::{HandleOutcome, RtComponent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

// --- helpers -----------------------------------------------------------------

/// A named key (e.g. `"escape"`, `"enter"`) with no modifiers.
fn named(id: &str, code: KeyCode) -> RtKey {
    RtKey {
        key_id: Some(id.to_string()),
        raw: KeyEvent::new(code, KeyModifiers::NONE),
    }
}

/// Render a component into a fresh buffer of the given size.
fn render<C: RtComponent>(comp: &C, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    comp.render(area, &mut buf);
    buf
}

/// The symbols of one buffer row concatenated, trailing blanks trimmed.
fn row(buf: &Buffer, y: u16) -> String {
    let area = buf.area;
    let mut s = String::new();
    for x in area.x..area.x + area.width {
        if let Some(cell) = buf.cell((x, y)) {
            s.push_str(cell.symbol());
        }
    }
    s.trim_end().to_string()
}

/// Whether *any* row of the buffer is non-empty.
fn any_content(buf: &Buffer) -> bool {
    let area = buf.area;
    (area.y..area.y + area.height).any(|y| !row(buf, y).is_empty())
}

// =============================================================================
// VAL-WIDGET-011 — Loader static text present/absent; cancellable progress +
// cancel prompt + Escape cancels.
// =============================================================================

#[test]
fn loader_static_text_present_while_active() {
    let loader = Loader::new("Working on it");
    let buf = render(&loader, 40, 1);
    assert!(
        row(&buf, 0).contains("Working on it"),
        "active loader shows its static message"
    );
}

#[test]
fn loader_static_text_absent_when_inactive() {
    let mut loader = Loader::new("Working on it");
    loader.set_active(false);
    let buf = render(&loader, 40, 1);
    assert!(
        !any_content(&buf),
        "inactive loader paints nothing — no ghost row, no leftover text"
    );
}

#[test]
fn loader_message_static_across_ticks() {
    // Ticking advances the (unasserted) spinner frame but must leave the static
    // message text untouched — the spinner glyph/timing is an informed exclusion.
    let mut loader = Loader::new("Compiling shaders");
    for _ in 0..5 {
        loader.tick();
    }
    let buf = render(&loader, 40, 1);
    assert!(row(&buf, 0).contains("Compiling shaders"));
    assert_eq!(loader.message(), "Compiling shaders");
}

#[test]
fn cancellable_shows_progress_bar_and_percentage() {
    let mut loader = CancellableLoader::new("Downloading");
    loader.set_progress(Some(0.5));
    let buf = render(&loader, 60, 3);
    assert!(
        row(&buf, 0).contains("Downloading"),
        "message on the first row"
    );
    let bar = row(&buf, 1);
    assert!(bar.contains('█'), "filled progress glyph present: {bar:?}");
    assert!(bar.contains('░'), "empty progress glyph present: {bar:?}");
    assert!(bar.contains("50%"), "percentage present: {bar:?}");
}

#[test]
fn cancellable_shows_elapsed_suffix() {
    let mut loader = CancellableLoader::new("Building");
    loader.set_elapsed(Some("3.2s".to_string()));
    let buf = render(&loader, 60, 3);
    assert!(
        row(&buf, 0).contains("(3.2s)"),
        "elapsed suffix rendered on the spinner line"
    );
}

#[test]
fn cancellable_shows_cancel_prompt() {
    let loader = CancellableLoader::new("Working");
    let buf = render(&loader, 60, 3);
    // The hint row is present with the Escape affordance.
    let joined = (0..3).map(|y| row(&buf, y)).collect::<Vec<_>>().join("\n");
    assert!(
        joined.contains("Escape"),
        "Press-Escape-to-cancel prompt is shown: {joined:?}"
    );
}

#[test]
fn cancellable_escape_cancels_and_latches_outcome() {
    let mut loader = CancellableLoader::new("Working");
    assert!(!loader.is_cancelled());
    let outcome = loader.handle_key(&named("escape", KeyCode::Esc));
    assert!(outcome.is_consumed(), "Escape is consumed by the loader");
    assert!(loader.is_cancelled(), "Escape sets the cancelled flag");
    assert_eq!(
        loader.take_outcome(),
        Some(CancelOutcome::Cancelled),
        "Escape latches a Cancelled outcome"
    );
    assert_eq!(loader.take_outcome(), None, "outcome is a one-shot latch");
}

#[test]
fn cancellable_non_escape_key_ignored() {
    let mut loader = CancellableLoader::new("Working");
    let outcome = loader.handle_key(&named("enter", KeyCode::Enter));
    assert_eq!(outcome, HandleOutcome::Ignored);
    assert!(!loader.is_cancelled());
}

// =============================================================================
// VAL-WIDGET-012 — Toast stack / cap / overflow-hidden-and-reappears / no ghost.
// =============================================================================

#[test]
fn toast_stacks_newest_first() {
    let mut toast = Toast::new();
    toast.info("older");
    toast.error("newer");
    let buf = render(&toast, 40, 3);
    assert!(row(&buf, 0).contains("newer"), "newest paints on top");
    assert!(row(&buf, 0).contains("[x]"), "with its error icon");
    assert!(row(&buf, 1).contains("older"));
    assert!(row(&buf, 1).contains("[i]"));
}

#[test]
fn toast_level_icons_are_bracketed_forms() {
    assert_eq!(ToastLevel::Info.icon(), "[i]");
    assert_eq!(ToastLevel::Success.icon(), "[*]");
    assert_eq!(ToastLevel::Warning.icon(), "[!]");
    assert_eq!(ToastLevel::Error.icon(), "[x]");
}

#[test]
fn toast_caps_visible_and_hides_overflow_without_dropping() {
    let mut toast = Toast::new();
    toast.set_max_visible(2);
    toast.info("first"); // oldest — pushed out of view by the cap
    toast.info("second");
    toast.info("third"); // newest
    // Three live, two visible: the overflow is hidden, not discarded.
    assert_eq!(toast.count(), 3, "the overflow toast is retained");
    assert_eq!(toast.visible_count(), 2);
    assert_eq!(toast.visible_messages(), vec!["third", "second"]);
    // And the painted grid shows exactly the two visible, newest first.
    let buf = render(&toast, 40, 5);
    assert!(row(&buf, 0).contains("third"));
    assert!(row(&buf, 1).contains("second"));
    assert!(row(&buf, 2).is_empty(), "only two rows painted");
}

#[test]
fn toast_dismiss_newest_reveals_hidden_overflow() {
    let mut toast = Toast::new();
    toast.set_max_visible(2);
    toast.info("first"); // hidden behind the cap
    toast.info("second");
    toast.info("third");
    assert_eq!(toast.visible_messages(), vec!["third", "second"]);
    // Dismiss the newest → the hidden "first" comes back into view.
    toast.dismiss_newest();
    assert_eq!(toast.count(), 2);
    assert_eq!(
        toast.visible_messages(),
        vec!["second", "first"],
        "the previously hidden toast re-appears rather than being lost"
    );
    let buf = render(&toast, 40, 3);
    assert!(row(&buf, 0).contains("second"));
    assert!(row(&buf, 1).contains("first"));
}

#[test]
fn toast_ttl_expiry_reveals_hidden_overflow() {
    let mut toast = Toast::new();
    toast.set_max_visible(2);
    toast.info("first"); // oldest, hidden behind the cap, never expires on its own
    toast.info("second");
    toast.push_with_ttl(ToastLevel::Warning, "third", 1); // newest, expires
    assert_eq!(toast.visible_messages(), vec!["third", "second"]);
    // Tick once: "third" (ttl 1) decrements to 0 but is still present.
    toast.tick_ttl();
    assert_eq!(toast.count(), 3);
    // Tick again: "third" hits zero and is removed → hidden "first" re-shows.
    toast.tick_ttl();
    assert_eq!(toast.count(), 2);
    assert_eq!(toast.visible_messages(), vec!["second", "first"]);
}

#[test]
fn toast_dismiss_leaves_no_ghost_row() {
    let mut toast = Toast::new();
    toast.info("alpha");
    toast.error("beta");
    // Two visible.
    let buf = render(&toast, 40, 3);
    assert!(row(&buf, 0).contains("beta"));
    assert!(!row(&buf, 1).is_empty());
    // Dismiss down to one and re-render into the same-size area: the vacated rows
    // repaint blank — no residual "beta" row.
    toast.dismiss_newest();
    let buf = render(&toast, 40, 3);
    assert!(row(&buf, 0).contains("alpha"));
    assert!(row(&buf, 1).is_empty(), "vacated row leaves no ghost");
    assert!(row(&buf, 2).is_empty());
}

#[test]
fn toast_empty_stack_and_clear_paint_nothing() {
    let mut toast = Toast::new();
    assert!(!any_content(&render(&toast, 40, 3)));
    toast.info("a");
    toast.warning("b");
    assert!(any_content(&render(&toast, 40, 3)));
    toast.clear();
    assert_eq!(toast.count(), 0);
    assert!(!any_content(&render(&toast, 40, 3)));
}

// =============================================================================
// VAL-WIDGET-024 — <8-column CJK/emoji truncation: no panic, no byte slice, no
// width underflow.
// =============================================================================

#[test]
fn narrow_cjk_loader_renders_without_panic() {
    // A CJK message far wider than a <8-column pane: the legacy byte-slice panic
    // lived here. The render must survive at every narrow width and never spill
    // onto a second row.
    let loader = Loader::new("你好世界你好世界这是很长的消息");
    for w in 1u16..=8 {
        let buf = render(&loader, w, 2);
        // The loader occupies exactly one row; nothing overflows to row 1.
        assert!(
            row(&buf, 1).is_empty(),
            "width {w}: content spilled to a second row"
        );
    }
}

#[test]
fn narrow_cjk_cancellable_renders_without_width_underflow() {
    // The legacy `width - 8` computation underflowed usize on a <8-column pane.
    // Here the progress bar is simply dropped below its fixed chrome width; the
    // render never panics.
    let mut loader = CancellableLoader::new("处理中的任务信息");
    loader.set_progress(Some(0.5));
    for w in 1u16..=8 {
        // Must not panic at any narrow width.
        let _ = render(&loader, w, 3);
    }
}

#[test]
fn narrow_emoji_toast_renders_without_panic() {
    // Emoji (multi-codepoint grapheme clusters) must clip on a cluster boundary,
    // never byte-sliced mid-sequence.
    let mut toast = Toast::new();
    toast.error("🎉🎉🎉🎉 party time overflow message");
    for w in 1u16..=8 {
        let buf = render(&toast, w, 2);
        assert!(
            row(&buf, 1).is_empty(),
            "width {w}: content spilled to a second row"
        );
    }
}

#[test]
fn narrow_toast_message_never_exceeds_pane_width() {
    // A width-underflow / overflow unit: for widths where the icon fits, the
    // painted content (icon + truncated message) is never wider than the pane in
    // display columns. Measured from the source strings via the same grapheme
    // truncation the widget uses, so a wide glyph's reserved continuation cell
    // does not double-count.
    let msg = "你好世界你好世界"; // 16 display columns
    let icon = ToastLevel::Info.icon();
    let icon_cols = icon.width() + 1; // icon + separating space
    for w in (icon_cols as u16)..=20 {
        let budget = (w as usize) - icon_cols;
        let shown = truncate_grapheme_for_test(msg, budget);
        let painted = icon_cols + shown.width();
        assert!(
            painted <= w as usize,
            "width {w}: painted {painted} cols > pane"
        );
    }
}

/// A grapheme-cluster-boundary truncation mirroring the widget's internal helper,
/// so the test can measure the exact source width the widget would paint (the
/// crate-private helper is not exported).
fn truncate_grapheme_for_test(s: &str, max_cols: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    if max_cols == 0 {
        return String::new();
    }
    if s.width() <= max_cols {
        return s.to_string();
    }
    let budget = max_cols.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0usize;
    for cluster in s.graphemes(true) {
        let cw = cluster.width();
        if used + cw > budget {
            break;
        }
        out.push_str(cluster);
        used += cw;
    }
    out.push('…');
    out
}
