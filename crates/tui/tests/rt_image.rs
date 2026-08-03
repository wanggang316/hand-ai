//! Behavioural tests for the rt image widget + graphics-protocol emission
//! (`hand_tui::rt::components::image`).
//!
//! These pin the assertions the external validator probes, exercised against the
//! encoder framing, the capability matrix, the dimension sniffer, the row
//! allocation, and the raw-emission channel — no live terminal:
//!
//! - **VAL-IMG-001** — Kitty APC framing (`\x1b_G…\x1b\\`, transfer params +
//!   `i=<id>`) and row allocation: the reserved rows equal the computed image
//!   rows, and the emission is queued at the widget's top row.
//! - **VAL-IMG-002** — iTerm2 OSC 1337 framing pins only the protocol-required
//!   parts: `inline=1`, the base64 payload, a BEL/ST terminator. Encoder-internal
//!   `name=`/parameter order is deliberately *not* asserted (Decision Log:
//!   IMG-does-not-predict-ratatui-image).
//! - **VAL-IMG-003** — a plain-terminal image paints a bordered placeholder box
//!   with a label and emits **zero** graphics bytes (no `\x1b_G`, no
//!   `\x1b]1337`).
//! - **VAL-IMG-004** — a multiplexer (TMUX / screen `TERM`) resolves to fallback
//!   even with a graphics capability marker present, so the tmux path is also
//!   zero graphics bytes.
//! - **VAL-IMG-012** — the detection matrix: ghostty/wezterm → Kitty APC;
//!   TMUX + Kitty marker → none (suppressed); Kitty + iTerm2 markers together →
//!   Kitty only.
//! - **VAL-IMG-016** — non-PNG sniff labels: jpeg/gif/webp on a non-graphics
//!   terminal tag `[<mime> WxH]`; a corrupt file tags `[<mime>]` with no size.
//! - **VAL-IMG-021** — non-PNG on a graphics terminal displays: Kitty transcodes
//!   to a PNG APC (still a valid `\x1b_G` frame, never an invalid one); iTerm2
//!   passes the native bytes through in an OSC 1337.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use hand_tui::rt::components::{
    ImageFormat, RawEmissionQueue, ResolvedProtocol, RtImage, clamp_to_area, clip_label, decodes,
    parse_cell_size_reply, resolve, sanitize_label, sniff_label,
};
use hand_tui::rt::view::RtComponent;
use hand_tui::terminal_image::{
    CellDimensions, ImageDimensions, detect_capabilities, set_cell_dimensions,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

/// Serializes tests that mutate process-global environment variables (the
/// capability-detection matrix reads `TERM_PROGRAM`, `TMUX`, `TERM`, …).
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Serializes tests that mutate the process-global cached cell dimensions
/// (`set_cell_dimensions` / `get_cell_dimensions`), so one case's cell size does
/// not leak into another running in parallel.
static CELL_LOCK: Mutex<()> = Mutex::new(());

/// Hold the cell-dimension lock and pin the cache to the conservative 8×16
/// default for the duration of a test, so row-allocation math is deterministic
/// regardless of what a prior cell-size test left cached. The returned guard both
/// serializes the test and keeps the RAII lifetime tied to the caller.
fn with_default_cell_dimensions() -> MutexGuard<'static, ()> {
    let guard = CELL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_cell_dimensions(CellDimensions {
        width: 8,
        height: 16,
    });
    guard
}

/// The graphics capability / multiplexer env vars the detector inspects. Cleared
/// before each matrix case so one case's markers do not leak into the next.
const CAP_ENV_VARS: &[&str] = &[
    "TERM_PROGRAM",
    "TERM",
    "TMUX",
    "CMUX_WORKSPACE_ID",
    "KITTY_WINDOW_ID",
    "GHOSTTY_RESOURCES_DIR",
    "WEZTERM_PANE",
    "ITERM_SESSION_ID",
];

/// Snapshot every capability env var, clear them all, run `f` against a clean
/// slate, then restore the originals. Keeps the process env pristine across the
/// matrix cases even when one panics.
fn with_clean_env(f: impl FnOnce()) {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved: Vec<(&str, Option<String>)> = CAP_ENV_VARS
        .iter()
        .map(|&k| (k, std::env::var(k).ok()))
        .collect();
    for &k in CAP_ENV_VARS {
        unsafe { std::env::remove_var(k) };
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    for (k, v) in saved {
        match v {
            Some(v) => unsafe { std::env::set_var(k, v) },
            None => unsafe { std::env::remove_var(k) },
        }
    }
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

/// Absolute path to a fixture under `tests/fixtures/tui/images/`.
fn fixture(name: &str) -> PathBuf {
    // CARGO_MANIFEST_DIR is `crates/tui`; the fixtures live at the repo root.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../tests/fixtures/tui/images");
    p.push(name);
    p
}

/// Read a fixture's bytes, failing the test with a clear message if it is missing.
fn read_fixture(name: &str) -> Vec<u8> {
    let path = fixture(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {} missing: {e}", path.display()))
}

/// Render a component into a fresh buffer sized to `area` and return it.
fn render_to_buffer_at(component: &dyn RtComponent, area: Rect) -> Buffer {
    let mut buf = Buffer::empty(area);
    component.render(area, &mut buf);
    buf
}

/// Every cell symbol of the buffer concatenated into one string, so a probe can
/// scan for graphics-escape bytes that would (wrongly) have been painted as text.
fn buffer_text(buf: &Buffer) -> String {
    let area = buf.area;
    let mut s = String::new();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell((x, y)) {
                s.push_str(cell.symbol());
            }
        }
        s.push('\n');
    }
    s
}

// --- VAL-IMG-012: capability detection matrix -------------------------------

#[test]
fn ghostty_env_resolves_to_kitty() {
    with_clean_env(|| {
        unsafe { std::env::set_var("TERM_PROGRAM", "ghostty") };
        let caps = detect_capabilities();
        assert!(caps.kitty, "ghostty must report Kitty capability");
        assert!(!caps.iterm2);
        assert_eq!(resolve(caps.kitty, caps.iterm2), ResolvedProtocol::Kitty);
    });
}

#[test]
fn wezterm_env_resolves_to_kitty() {
    with_clean_env(|| {
        unsafe { std::env::set_var("WEZTERM_PANE", "0") };
        let caps = detect_capabilities();
        assert!(caps.kitty, "wezterm must report Kitty capability");
        assert_eq!(resolve(caps.kitty, caps.iterm2), ResolvedProtocol::Kitty);
    });
}

#[test]
fn tmux_suppresses_graphics_even_with_kitty_marker() {
    with_clean_env(|| {
        // Both a multiplexer marker *and* a Kitty capability marker are present;
        // the multiplexer must win and suppress graphics entirely.
        unsafe { std::env::set_var("TMUX", "/tmp/tmux-1000/default,1234,0") };
        unsafe { std::env::set_var("KITTY_WINDOW_ID", "1") };
        let caps = detect_capabilities();
        assert!(!caps.kitty, "TMUX must suppress the Kitty marker");
        assert!(!caps.iterm2);
        assert_eq!(resolve(caps.kitty, caps.iterm2), ResolvedProtocol::Fallback);
    });
}

#[test]
fn screen_term_suppresses_graphics() {
    with_clean_env(|| {
        unsafe { std::env::set_var("TERM", "screen-256color") };
        unsafe { std::env::set_var("KITTY_WINDOW_ID", "1") };
        let caps = detect_capabilities();
        assert!(!caps.kitty, "a screen TERM must suppress graphics");
        assert_eq!(resolve(caps.kitty, caps.iterm2), ResolvedProtocol::Fallback);
    });
}

#[test]
fn kitty_and_iterm2_markers_together_pick_kitty_only() {
    with_clean_env(|| {
        // Both a Kitty and an iTerm2 marker present: exactly one protocol is
        // chosen, and it is Kitty (the tie-break rule).
        unsafe { std::env::set_var("KITTY_WINDOW_ID", "1") };
        unsafe { std::env::set_var("ITERM_SESSION_ID", "w0t0p0:UUID") };
        let caps = detect_capabilities();
        assert!(caps.kitty, "Kitty marker must win the tie");
        assert!(
            !caps.iterm2,
            "detection must not report both protocols simultaneously"
        );
        assert_eq!(resolve(caps.kitty, caps.iterm2), ResolvedProtocol::Kitty);
    });
}

#[test]
fn plain_term_resolves_to_fallback() {
    with_clean_env(|| {
        unsafe { std::env::set_var("TERM", "xterm-256color") };
        let caps = detect_capabilities();
        assert!(!caps.kitty);
        assert!(!caps.iterm2);
        assert_eq!(resolve(caps.kitty, caps.iterm2), ResolvedProtocol::Fallback);
    });
}

// --- pure resolution tie-break ----------------------------------------------

#[test]
fn resolve_tie_break_and_arms() {
    assert_eq!(resolve(true, true), ResolvedProtocol::Kitty);
    assert_eq!(resolve(true, false), ResolvedProtocol::Kitty);
    assert_eq!(resolve(false, true), ResolvedProtocol::ITerm2);
    assert_eq!(resolve(false, false), ResolvedProtocol::Fallback);
}

// --- VAL-IMG-001: Kitty APC framing + row allocation ------------------------

#[test]
fn kitty_emission_is_valid_apc_with_transfer_and_id() {
    let png = read_fixture("sample.png");
    let queue = RawEmissionQueue::new();
    let image = RtImage::new(png)
        .label("sample.png")
        .protocol(ResolvedProtocol::Kitty)
        .emission_queue(queue.clone());
    let area = Rect::new(0, 3, 40, 6);
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 12));
    image.render(area, &mut buf);

    let pending = queue.take();
    assert_eq!(pending.len(), 1, "one Kitty emission queued");
    let e = &pending[0];
    // Well-formed APC envelope.
    assert!(e.escape.starts_with("\x1b_G"), "APC introducer");
    assert!(e.escape.ends_with("\x1b\\"), "ST terminator");
    // Transfer parameters + an image id.
    assert!(e.escape.contains("a=T"), "transfer-and-display action");
    assert!(e.escape.contains("f=100"), "PNG format");
    assert!(
        e.escape.contains("i="),
        "an image id is present: {:?}",
        e.escape
    );
    // The emission is anchored at the widget's top row.
    assert_eq!(e.row, area.y, "emission queued at the widget top row");
}

#[test]
fn kitty_reserved_rows_match_computed_image_rows() {
    let _cells = with_default_cell_dimensions();
    let png = read_fixture("large.png"); // 512x512 → 64x32 natural cells at 8x16
    let queue = RawEmissionQueue::new();
    let area = Rect::new(0, 0, 40, 30);
    let image = RtImage::new(png)
        .protocol(ResolvedProtocol::Kitty)
        .emission_queue(queue.clone());
    let reserved = image.reserved_rows(area);
    let mut buf = Buffer::empty(area);
    image.render(area, &mut buf);
    let pending = queue.take();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].rows, reserved,
        "the queued row count is the reserved footprint"
    );
    // 512x512 at 8x16 cells is 64x32 natural cells: wider than the 40-col area,
    // so width binds (64->40) and the height scales in proportion, 32*40/64 = 20,
    // preserving the aspect rather than clamping height alone to 30.
    assert_eq!(
        reserved, 20,
        "wide image is width-bound; height scales to match"
    );
    // The reserved rows are painted blank (the graphics image is drawn over
    // them out of band), never carrying any escape bytes as text.
    let text = buffer_text(&buf);
    assert!(!text.contains("\x1b_G"), "no APC bytes leaked into cells");
}

#[test]
fn flush_positions_cursor_and_emits_escape() {
    let png = read_fixture("sample.png");
    let queue = RawEmissionQueue::new();
    let image = RtImage::new(png)
        .protocol(ResolvedProtocol::Kitty)
        .emission_queue(queue.clone());
    let area = Rect::new(0, 2, 40, 5);
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 12));
    image.render(area, &mut buf);

    let mut out: Vec<u8> = Vec::new();
    // Viewport starts at absolute row 4 (as if insert_before slid it down).
    queue.flush_to(&mut out, 4).unwrap();
    let s = String::from_utf8_lossy(&out);
    // Cursor saved, moved to absolute row (4 + 2) + 1 = 7 (1-based CUP), then the
    // APC, then cursor restored.
    assert!(s.starts_with("\x1b7"), "cursor saved first");
    assert!(
        s.contains("\x1b[7;1H"),
        "cursor moved to the image row: {s:?}"
    );
    assert!(s.contains("\x1b_G"), "the APC escape is written");
    assert!(
        s.ends_with("\x1b8") || s.contains("\x1b8"),
        "cursor restored"
    );
    // The queue is drained after a flush.
    assert!(queue.is_empty(), "flush drains the queue");
}

// --- VAL-IMG-002: iTerm2 OSC 1337 framing (protocol-required parts only) -----

#[test]
fn iterm2_emission_pins_only_protocol_required_parts() {
    let jpg = read_fixture("sample.jpg");
    let expected_payload = hand_tui::rt::components::base64_encode(&jpg);
    let queue = RawEmissionQueue::new();
    let image = RtImage::new(jpg)
        .label("sample.jpg")
        .protocol(ResolvedProtocol::ITerm2)
        .emission_queue(queue.clone());
    let area = Rect::new(0, 0, 40, 6);
    let mut buf = Buffer::empty(area);
    image.render(area, &mut buf);

    let pending = queue.take();
    assert_eq!(pending.len(), 1);
    let escape = &pending[0].escape;
    // Protocol-required: the OSC 1337 File introducer, inline=1, the base64
    // payload, and a BEL or ST terminator. Parameter order and `name=` are NOT
    // asserted (Decision Log).
    assert!(escape.starts_with("\x1b]1337;File="), "OSC 1337 introducer");
    assert!(escape.contains("inline=1"), "inline flag");
    assert!(escape.contains(&expected_payload), "native base64 payload");
    assert!(
        escape.ends_with('\x07') || escape.ends_with("\x1b\\"),
        "terminated by BEL or ST"
    );
}

// --- VAL-IMG-003 / 004: fallback paints a box, emits zero graphics bytes -----

#[test]
fn plain_fallback_paints_labelled_box_with_zero_graphics_bytes() {
    let png = read_fixture("sample.png");
    let queue = RawEmissionQueue::new();
    let image = RtImage::new(png)
        .label("sample.png")
        .protocol(ResolvedProtocol::Fallback)
        .emission_queue(queue.clone());
    let buf = render_to_buffer_at(&image, Rect::new(0, 0, 40, 5));
    let text = buffer_text(&buf);
    // A bordered box with the label.
    assert!(text.contains('┌') && text.contains('┘'), "bordered box");
    assert!(text.contains("sample.png"), "label shown");
    // Zero graphics bytes in the painted cells.
    assert!(!text.contains("\x1b_G"), "no Kitty APC in cells");
    assert!(!text.contains("\x1b]1337"), "no iTerm2 OSC in cells");
    // And nothing was queued for out-of-band emission on the fallback path.
    assert!(queue.is_empty(), "fallback queues no emission");
}

#[test]
fn tmux_path_is_fallback_with_zero_graphics_bytes() {
    // The tmux capture path exercises exactly the fallback resolution: a
    // multiplexer resolves to `Fallback` (VAL-IMG-004), so the widget paints a
    // box and emits no graphics bytes.
    let png = read_fixture("sample.png");
    let image = RtImage::new(png)
        .label("in-tmux.png")
        .protocol(ResolvedProtocol::Fallback);
    let buf = render_to_buffer_at(&image, Rect::new(0, 0, 40, 4));
    let text = buffer_text(&buf);
    assert!(!text.contains("\x1b_G"));
    assert!(!text.contains("\x1b]1337"));
    assert!(text.contains("in-tmux.png"));
}

// --- VAL-IMG-016: non-PNG sniff labels --------------------------------------

#[test]
fn jpeg_sniff_label_carries_mime_and_dims() {
    let jpg = read_fixture("sample.jpg");
    assert_eq!(ImageFormat::sniff(&jpg), ImageFormat::Jpeg);
    let label = sniff_label(&jpg).expect("jpeg sniffs");
    assert!(label.starts_with("[jpeg "), "mime tag: {label}");
    assert!(label.contains("64x48"), "sniffed dims: {label}");
}

#[test]
fn gif_sniff_label_carries_mime_and_dims() {
    let gif = read_fixture("sample.gif");
    assert_eq!(ImageFormat::sniff(&gif), ImageFormat::Gif);
    let label = sniff_label(&gif).expect("gif sniffs");
    assert!(label.starts_with("[gif "));
    assert!(label.contains("32x24"), "sniffed dims: {label}");
}

#[test]
fn webp_sniff_label_carries_mime_and_dims() {
    let webp = read_fixture("sample.webp");
    assert_eq!(ImageFormat::sniff(&webp), ImageFormat::Webp);
    let label = sniff_label(&webp).expect("webp sniffs");
    assert!(label.starts_with("[webp "));
    assert!(label.contains("96x72"), "sniffed dims: {label}");
}

#[test]
fn corrupt_file_labels_mime_without_dims() {
    let corrupt = read_fixture("corrupt.jpg");
    // Magic still sniffs the container (jpeg), but no readable dimensions.
    assert_eq!(ImageFormat::sniff(&corrupt), ImageFormat::Jpeg);
    let label = sniff_label(&corrupt).expect("mime still known");
    assert_eq!(label, "[jpeg]", "no dims on a corrupt file: {label}");
}

#[test]
fn corrupt_fallback_box_shows_mime_only_label() {
    let corrupt = read_fixture("corrupt.jpg");
    let image = RtImage::new(corrupt).protocol(ResolvedProtocol::Fallback);
    let buf = render_to_buffer_at(&image, Rect::new(0, 0, 30, 4));
    let text = buffer_text(&buf);
    assert!(text.contains("[jpeg]"), "corrupt box tag: {text}");
}

// --- VAL-IMG-021: non-PNG on a graphics terminal displays --------------------

#[test]
fn kitty_transcodes_non_png_to_a_valid_apc() {
    // A jpeg on the Kitty path is transcoded to PNG; the emission is still a
    // valid APC (never an invalid one), and it declares the PNG format.
    for name in ["sample.jpg", "sample.gif", "sample.webp"] {
        let data = read_fixture(name);
        let queue = RawEmissionQueue::new();
        let image = RtImage::new(data)
            .protocol(ResolvedProtocol::Kitty)
            .emission_queue(queue.clone());
        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);
        image.render(area, &mut buf);
        let pending = queue.take();
        assert_eq!(pending.len(), 1, "{name}: one emission queued");
        let escape = &pending[0].escape;
        assert!(escape.starts_with("\x1b_G"), "{name}: valid APC introducer");
        assert!(escape.ends_with("\x1b\\"), "{name}: ST terminator");
        assert!(escape.contains("f=100"), "{name}: transcoded to PNG format");
    }
}

#[test]
fn kitty_undecodable_source_degrades_to_box_not_invalid_apc() {
    // A corrupt source cannot be transcoded; the Kitty path must degrade to the
    // placeholder box rather than emit an invalid APC (no graphics bytes).
    let corrupt = read_fixture("corrupt.jpg");
    let queue = RawEmissionQueue::new();
    let image = RtImage::new(corrupt)
        .label("broken.jpg")
        .protocol(ResolvedProtocol::Kitty)
        .emission_queue(queue.clone());
    let buf = render_to_buffer_at(&image, Rect::new(0, 0, 30, 4));
    let text = buffer_text(&buf);
    assert!(
        queue.is_empty(),
        "no emission on an undecodable Kitty source"
    );
    assert!(!text.contains("\x1b_G"), "no invalid APC emitted");
    assert!(text.contains('┌'), "degraded to the placeholder box");
}

#[test]
fn iterm2_passes_non_png_native() {
    // iTerm2 decodes jpeg/gif/webp itself: the emission carries the *source*
    // bytes, not a transcoded PNG.
    for name in ["sample.jpg", "sample.gif", "sample.webp"] {
        let data = read_fixture(name);
        let expected = hand_tui::rt::components::base64_encode(&data);
        let queue = RawEmissionQueue::new();
        let image = RtImage::new(data)
            .protocol(ResolvedProtocol::ITerm2)
            .emission_queue(queue.clone());
        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);
        image.render(area, &mut buf);
        let pending = queue.take();
        assert_eq!(pending.len(), 1, "{name}: one emission");
        assert!(
            pending[0].escape.contains(&expected),
            "{name}: native source bytes passed through"
        );
    }
}

// --- VAL-IMG-006: width clamp preserves aspect ------------------------------

#[test]
fn clamp_wider_than_terminal_binds_width_and_scales_height() {
    // A 400x40 image at 8x16 cells is 50x3 natural cells. In a 20-col area, width
    // binds (50 -> 20) and the height scales proportionally: 3 * 20/50 -> 1 row.
    let dims = ImageDimensions {
        width: 400,
        height: 40,
    };
    let cell = CellDimensions {
        width: 8,
        height: 16,
    };
    let clamped = clamp_to_area(dims, Rect::new(0, 0, 20, 10), cell);
    assert_eq!(clamped.cols, 20, "width clamps to the display width");
    assert!(
        clamped.cols <= 20 && clamped.rows <= 10,
        "both axes clamped"
    );
    // Aspect preserved: cols/rows tracks the source 10:1, not stretched to fill.
    assert!(
        clamped.cols >= clamped.rows * 5,
        "wide aspect preserved (not stretched to the area): {clamped:?}"
    );
}

#[test]
fn clamp_wide_fixture_reserved_rows_match_width_bound_allocation() {
    let _cells = with_default_cell_dimensions();
    let png = read_fixture("wide.png"); // 400x40 -> 50x3 natural cells at 8x16
    let queue = RawEmissionQueue::new();
    let area = Rect::new(0, 0, 20, 10);
    let image = RtImage::new(png)
        .protocol(ResolvedProtocol::Kitty)
        .emission_queue(queue.clone());
    let reserved = image.reserved_rows(area);
    let mut buf = Buffer::empty(area);
    image.render(area, &mut buf);
    let pending = queue.take();
    assert_eq!(pending.len(), 1);
    // The blanked/reserved rows in the buffer, the queued footprint, and the
    // computed clamp all agree — the layout allocation matches the row count.
    assert_eq!(
        pending[0].rows, reserved,
        "queued footprint == reserved rows"
    );
    assert!(reserved <= area.height, "rows never exceed the pane height");
    // 3 natural rows, width-bound scale 20/50 -> 1 row.
    assert_eq!(reserved, 1, "width-bound height allocation");
}

// --- VAL-IMG-014: super-tall clamp ------------------------------------------

#[test]
fn clamp_taller_than_pane_binds_height_and_scales_width() {
    // A 40x400 image at 8x16 cells is 5x25 natural cells. In a 10-row area, height
    // binds (25 -> 10) and the width scales proportionally: 5 * 10/25 -> 2 cols.
    let dims = ImageDimensions {
        width: 40,
        height: 400,
    };
    let cell = CellDimensions {
        width: 8,
        height: 16,
    };
    let clamped = clamp_to_area(dims, Rect::new(0, 0, 40, 10), cell);
    assert_eq!(clamped.rows, 10, "height clamps to the pane height");
    assert!(
        clamped.cols <= 40 && clamped.rows <= 10,
        "both axes clamped"
    );
    assert!(
        clamped.cols < clamped.rows,
        "tall aspect preserved: {clamped:?}"
    );
}

#[test]
fn super_tall_image_reserved_rows_never_exceed_pane() {
    let _cells = with_default_cell_dimensions();
    let png = read_fixture("tall.png"); // 40x400 -> 5x25 natural cells at 8x16
    let queue = RawEmissionQueue::new();
    // A short pane: the 25 natural rows must clamp to the 8-row pane, never spill.
    let area = Rect::new(0, 0, 40, 8);
    let image = RtImage::new(png)
        .protocol(ResolvedProtocol::Kitty)
        .emission_queue(queue.clone());
    let reserved = image.reserved_rows(area);
    let mut buf = Buffer::empty(area);
    image.render(area, &mut buf);
    let pending = queue.take();
    assert_eq!(pending.len(), 1);
    assert_eq!(reserved, 8, "tall image clamps to the pane height");
    assert_eq!(pending[0].rows, reserved, "footprint matches the clamp");
    // The blanked rows never overflow into a row the pane does not own: only the
    // pane's own rows carry blanks, no escape leaks into cells (no garbage into
    // scrollback below the pane).
    let text = buffer_text(&buf);
    assert!(!text.contains("\x1b_G"), "no APC bytes leaked into cells");
    assert_eq!(
        text.lines().count(),
        area.height as usize,
        "footprint stays within the pane's row band"
    );
}

// --- VAL-IMG-013: cell-size query does not block + reply scales rows ---------

#[test]
fn reserved_rows_honour_a_cell_size_reply() {
    let guard = CELL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // A tall image, sized against a *reported* cell height rather than the 8x16
    // default: the row allocation must use ceil(pixel_height / reported_cell_h).
    let png = read_fixture("tall.png"); // 40x400
    let area = Rect::new(0, 0, 40, 100); // tall enough not to clamp

    // Default 16px cells: 400px / 16 = 25 rows.
    set_cell_dimensions(CellDimensions {
        width: 8,
        height: 16,
    });
    let image = RtImage::new(png.clone()).protocol(ResolvedProtocol::Kitty);
    assert_eq!(image.reserved_rows(area), 25, "rows at 16px/cell");

    // A CSI 16 t reply reports 20px-tall cells: 400px / 20 = 20 rows. The row
    // allocation follows the reported metric, not the default.
    set_cell_dimensions(CellDimensions {
        width: 10,
        height: 20,
    });
    let image = RtImage::new(png).protocol(ResolvedProtocol::Kitty);
    assert_eq!(
        image.reserved_rows(area),
        20,
        "rows scale by the reported cell height, not the 8x16 default"
    );

    // Restore the default so later tests are unaffected.
    set_cell_dimensions(CellDimensions {
        width: 8,
        height: 16,
    });
    drop(guard);
}

#[test]
fn parse_cell_size_reply_extracts_pixel_metrics() {
    // The CSI 6 ; height ; width t report a terminal sends in answer to CSI 16 t.
    let dims = parse_cell_size_reply(b"\x1b[6;34;15t").expect("valid reply");
    assert_eq!(dims.height, 34);
    assert_eq!(dims.width, 15);
    // Embedded in a larger input buffer (as it would arrive interleaved).
    let dims = parse_cell_size_reply(b"junk\x1b[6;20;10ttrailing").expect("embedded reply");
    assert_eq!(dims.height, 20);
    assert_eq!(dims.width, 10);
}

#[test]
fn parse_cell_size_reply_rejects_garbage() {
    assert!(parse_cell_size_reply(b"").is_none());
    assert!(
        parse_cell_size_reply(b"\x1b[6n").is_none(),
        "DSR is not a cell report"
    );
    assert!(
        parse_cell_size_reply(b"\x1b[6;0;10t").is_none(),
        "zero dimension is rejected"
    );
    assert!(
        parse_cell_size_reply(b"\x1b[6;20t").is_none(),
        "a truncated report (one field) is rejected"
    );
    assert!(
        parse_cell_size_reply(b"\x1b[6;20;10;5t").is_none(),
        "an over-long report is rejected"
    );
}

#[test]
fn cell_size_query_is_off_by_default_and_never_reads() {
    // Off by default: without the force env, the query writes nothing — the render
    // loop never issues a blocking read against a silent terminal.
    unsafe { std::env::remove_var("HAND_TUI_QUERY_CELL_SIZE") };
    let mut out: Vec<u8> = Vec::new();
    hand_tui::rt::components::write_cell_size_query(&mut out).unwrap();
    assert!(out.is_empty(), "query is off by default");

    // Force-enabled: it writes CSI 16 t and returns immediately (fire-and-forget,
    // no read). The function returning at all *is* the non-blocking proof.
    unsafe { std::env::set_var("HAND_TUI_QUERY_CELL_SIZE", "1") };
    let mut out: Vec<u8> = Vec::new();
    hand_tui::rt::components::write_cell_size_query(&mut out).unwrap();
    assert_eq!(out, b"\x1b[16t", "forced query writes CSI 16 t");
    unsafe { std::env::remove_var("HAND_TUI_QUERY_CELL_SIZE") };
}

// --- VAL-IMG-007: 4096-char chunking does not tear --------------------------

/// Split a Kitty emission into its APC frames (`\x1b_G … \x1b\\`), asserting every
/// frame is well-formed (opens with the introducer, closes with the ST), and
/// return the frames.
fn kitty_frames(escape: &str) -> Vec<&str> {
    let mut frames = Vec::new();
    let mut rest = escape;
    while let Some(start) = rest.find("\x1b_G") {
        let after = &rest[start..];
        let end = after
            .find("\x1b\\")
            .expect("every APC frame is terminated by ST");
        frames.push(&after[..end + 2]);
        rest = &after[end + 2..];
    }
    // Nothing outside the frames: no half-escape, no stray base64.
    assert!(
        !rest.contains('\x1b'),
        "no dangling escape after the last frame: {rest:?}"
    );
    frames
}

#[test]
fn large_image_chunks_into_balanced_apc_frames() {
    let _cells = with_default_cell_dimensions();
    let png = read_fixture("huge.png"); // >4096 base64 chars -> multi-chunk APC
    let queue = RawEmissionQueue::new();
    let image = RtImage::new(png)
        .protocol(ResolvedProtocol::Kitty)
        .emission_queue(queue.clone());
    let area = Rect::new(0, 0, 40, 12);
    let mut buf = Buffer::empty(area);
    image.render(area, &mut buf);
    let pending = queue.take();
    assert_eq!(pending.len(), 1);
    let escape = &pending[0].escape;

    let frames = kitty_frames(escape);
    assert!(
        frames.len() >= 2,
        "a >4096-char payload is split into multiple APC chunks: {} frames",
        frames.len()
    );
    // First chunk carries the transfer params + the continuation flag; the last
    // chunk closes the transfer (m=0). No chunk exceeds the 4096-char protocol
    // limit for its base64 body.
    assert!(frames[0].contains("a=T"), "first chunk carries the header");
    assert!(frames[0].contains("m=1"), "first chunk continues (m=1)");
    assert!(
        frames.last().unwrap().contains("m=0"),
        "last chunk closes the transfer (m=0)"
    );
    for frame in &frames {
        // The body between the header/`;` and the closing ST is the base64 chunk;
        // it must not exceed the 4096-char limit.
        let body = frame
            .rsplit_once(';')
            .map(|(_, b)| b.trim_end_matches("\x1b\\"))
            .unwrap_or("");
        assert!(
            body.len() <= 4096,
            "no chunk exceeds the 4096-char APC limit: {} chars",
            body.len()
        );
    }
    // Balanced terminators: exactly one ST per introducer, no base64 in cells.
    assert_eq!(
        escape.matches("\x1b_G").count(),
        escape.matches("\x1b\\").count(),
        "every APC introducer has a matching ST terminator"
    );
    let text = buffer_text(&buf);
    assert!(!text.contains("\x1b_G"), "no APC bytes leaked into cells");
}

// --- VAL-IMG-008: decode-validation before emit (all personas) --------------

#[test]
fn decodes_gate_accepts_real_images_and_rejects_corrupt() {
    assert!(decodes(&read_fixture("sample.png")), "a real PNG decodes");
    assert!(decodes(&read_fixture("sample.jpg")), "a real JPEG decodes");
    assert!(
        !decodes(&read_fixture("corrupt.jpg")),
        "a corrupt file whose magic sniffs must not decode"
    );
    assert!(!decodes(b"not an image at all"), "garbage does not decode");
}

#[test]
fn undecodable_source_degrades_to_box_on_every_persona() {
    // The migration fix: an undecodable source degrades to the placeholder box on
    // *every* graphics persona — Kitty AND iTerm2 — never emitting a graphics
    // escape wrapping bytes the terminal cannot decode.
    let corrupt = read_fixture("corrupt.jpg");
    for protocol in [ResolvedProtocol::Kitty, ResolvedProtocol::ITerm2] {
        let queue = RawEmissionQueue::new();
        let image = RtImage::new(corrupt.clone())
            .label("broken.jpg")
            .protocol(protocol)
            .emission_queue(queue.clone());
        let buf = render_to_buffer_at(&image, Rect::new(0, 0, 30, 4));
        let text = buffer_text(&buf);
        assert!(
            queue.is_empty(),
            "{protocol:?}: no emission on an undecodable source"
        );
        assert!(
            !text.contains("\x1b_G"),
            "{protocol:?}: no Kitty APC emitted"
        );
        assert!(
            !text.contains("\x1b]1337"),
            "{protocol:?}: no iTerm2 OSC emitted"
        );
        assert!(text.contains('┌'), "{protocol:?}: degraded to the box");
        assert!(
            text.contains("[jpeg]"),
            "{protocol:?}: sized-less mime tag on the box"
        );
    }
}

// --- VAL-IMG-017: CJK alt-text does not overflow the placeholder ------------

#[test]
fn clip_label_clips_by_display_width_not_char_count() {
    // Each CJK glyph is two display columns. A 6-glyph label (12 columns) clipped
    // to 8 columns keeps whole glyphs by width (not 8 chars) and appends an
    // ellipsis, landing at <= 8 columns.
    let clipped = clip_label("你好世界你好", 8);
    assert!(
        UnicodeWidthStr::width(clipped.as_str()) <= 8,
        "clipped label fits the budget by display width: {clipped:?} = {} cols",
        UnicodeWidthStr::width(clipped.as_str())
    );
    assert!(
        clipped.ends_with('…'),
        "ellipsis marks the clip: {clipped:?}"
    );
    // An already-fitting label is returned unchanged.
    assert_eq!(clip_label("hi", 8), "hi");
}

#[test]
fn cjk_label_box_border_stays_aligned() {
    // A long CJK label in a narrow fallback box. The migration bug is that a
    // `chars().take(inner_w)` clip lets wide glyphs (two columns each) overrun the
    // box: the label row grows past the frame and the right border is pushed out
    // of the pane. With display-width clipping the label row lands on exactly the
    // box width, so the right-border column `│` and the bottom corners stay put.
    let png = read_fixture("sample.png");
    let area = Rect::new(0, 0, 20, 4);
    // The border-alignment contract on a wide-glyph label.
    let border = RtImage::new(png.clone())
        .label("你好世界你好世界你好世界你好世界")
        .protocol(ResolvedProtocol::Fallback);
    let cjk_buf = render_to_buffer_at(&border, area);
    // An ASCII control box of the same geometry to compare border columns against.
    let ascii = RtImage::new(png)
        .label("photo.png")
        .protocol(ResolvedProtocol::Fallback);
    let ascii_buf = render_to_buffer_at(&ascii, area);

    // Ratatui reserves a continuation cell after each wide glyph, so a
    // cell-by-cell read is not a display-width measure. The invariant that catches
    // the tear is structural: the border glyphs occupy the *same* cells in the CJK
    // box as in the ASCII control box — the wide label did not shove them.
    let border_cell = |buf: &Buffer, x: u16, y: u16| buf.cell((x, y)).unwrap().symbol().to_string();
    let last_x = area.x + area.width - 1;
    for y in area.y..area.y + area.height {
        assert_eq!(
            border_cell(&cjk_buf, area.x, y),
            border_cell(&ascii_buf, area.x, y),
            "left border column matches the ASCII box at row {y}"
        );
        assert_eq!(
            border_cell(&cjk_buf, last_x, y),
            border_cell(&ascii_buf, last_x, y),
            "right border column not pushed out by the wide label at row {y}"
        );
    }
    // The frame corners are present and aligned in the CJK box.
    let top = border_cell(&cjk_buf, area.x, area.y);
    let bottom_right = border_cell(&cjk_buf, last_x, area.y + area.height - 1);
    assert_eq!(top, "┌", "top-left corner intact");
    assert_eq!(bottom_right, "┘", "bottom-right corner not displaced");
}

// --- VAL-IMG-018: escape bytes in a label are defused -----------------------

#[test]
fn sanitize_label_strips_escape_and_control_bytes() {
    let dirty = "photo\x1b[31m\x07\x1b]1337;evil\x07.png";
    let clean = sanitize_label(dirty);
    assert!(!clean.contains('\x1b'), "no ESC survives: {clean:?}");
    assert!(!clean.contains('\x07'), "no BEL survives: {clean:?}");
    assert!(
        clean.contains("photo"),
        "printable text preserved: {clean:?}"
    );
    assert!(
        clean.contains(".png"),
        "printable text preserved: {clean:?}"
    );
    // CJK/emoji printable content is preserved.
    assert_eq!(sanitize_label("图\x1b片"), "图片");
}

#[test]
fn escape_label_produces_no_graphics_protocol_bytes_in_box() {
    // A filename carrying a graphics-protocol escape must land in the box as inert
    // text with zero protocol bytes reaching the cells.
    let png = read_fixture("sample.png");
    let image = RtImage::new(png)
        .label("evil\x1b_Ga=d;\x1b\\\x1b]1337;File=inline=1\x07.png")
        .protocol(ResolvedProtocol::Fallback);
    let buf = render_to_buffer_at(&image, Rect::new(0, 0, 40, 4));
    let text = buffer_text(&buf);
    assert!(
        !text.contains("\x1b_G"),
        "no Kitty APC bytes in cells: {text:?}"
    );
    assert!(!text.contains("\x1b]1337"), "no iTerm2 OSC bytes in cells");
    assert!(!text.contains('\x1b'), "no ESC anywhere in the cells");
}

#[test]
fn escape_label_produces_no_graphics_bytes_in_iterm2_name() {
    // The same defence on the emission path: a smuggled escape in the label must
    // not survive into the iTerm2 image name (which carries the label).
    let jpg = read_fixture("sample.jpg");
    let queue = RawEmissionQueue::new();
    let image = RtImage::new(jpg)
        .label("name\x1b]1337;File=inline=1\x07.jpg")
        .protocol(ResolvedProtocol::ITerm2)
        .emission_queue(queue.clone());
    let area = Rect::new(0, 0, 40, 6);
    let mut buf = Buffer::empty(area);
    image.render(area, &mut buf);
    let pending = queue.take();
    assert_eq!(pending.len(), 1);
    let escape = &pending[0].escape;
    // Exactly one OSC 1337 introducer (the emission itself) and one BEL/ST
    // terminator — the smuggled second `\x1b]1337` was stripped from the label
    // before it was base64-encoded into the name.
    assert_eq!(
        escape.matches("\x1b]1337").count(),
        1,
        "only the emission's own OSC introducer, none smuggled via the name"
    );
}

// --- VAL-IMG-015: resize with an image does not tear ------------------------

#[test]
fn resize_keeps_apc_terminators_balanced_and_footprint_coherent() {
    let _cells = with_default_cell_dimensions();
    let png = read_fixture("huge.png");
    // Render the same image across a sequence of widths (a resize sweep): at each
    // size the APC introducers and ST terminators stay balanced, no base64 leaks
    // into cells, and the reserved footprint stays within the pane.
    for width in [80u16, 40, 20, 60] {
        let area = Rect::new(0, 0, width, 12);
        let queue = RawEmissionQueue::new();
        let image = RtImage::new(png.clone())
            .protocol(ResolvedProtocol::Kitty)
            .emission_queue(queue.clone());
        let reserved = image.reserved_rows(area);
        let mut buf = Buffer::empty(area);
        image.render(area, &mut buf);
        let pending = queue.take();
        assert_eq!(pending.len(), 1, "width {width}: one emission");
        let escape = &pending[0].escape;
        assert_eq!(
            escape.matches("\x1b_G").count(),
            escape.matches("\x1b\\").count(),
            "width {width}: balanced APC terminators after resize"
        );
        // Every frame well-formed (the helper asserts no dangling escape/base64).
        let _frames = kitty_frames(escape);
        assert!(
            reserved <= area.height,
            "width {width}: footprint within the pane"
        );
        assert_eq!(
            pending[0].rows, reserved,
            "width {width}: queued footprint matches the reserved rows"
        );
        let text = buffer_text(&buf);
        assert!(
            !text.contains("\x1b_G"),
            "width {width}: no base64/APC visible in cells"
        );
    }
}
