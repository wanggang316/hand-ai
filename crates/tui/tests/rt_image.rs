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
use std::sync::Mutex;

use hand_tui::rt::components::{
    ImageFormat, RawEmissionQueue, ResolvedProtocol, RtImage, resolve, sniff_label,
};
use hand_tui::rt::view::RtComponent;
use hand_tui::terminal_image::detect_capabilities;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

/// Serializes tests that mutate process-global environment variables (the
/// capability-detection matrix reads `TERM_PROGRAM`, `TMUX`, `TERM`, …).
static ENV_LOCK: Mutex<()> = Mutex::new(());

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
    let png = read_fixture("large.png"); // 512x512 → many rows at 16px/cell
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
    // 512px / 16px-per-cell = 32 rows, clamped to the 30-row area.
    assert_eq!(reserved, 30, "row allocation clamps to the area height");
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
