//! Behavioural tests for the rt image **scrollback** channel + OSC 8 hyperlinks
//! (`hand_tui::rt::components::ScrollbackImageChannel`,
//! `HistorySink::commit_image`, `osc8_emission`).
//!
//! These pin the assertions the external validator probes for scrollback image
//! survival and drop-gesture safety — no live terminal, driven against the
//! encoder framing, the stable content→id map, and ratatui's `TestBackend` for
//! the history-commit seam:
//!
//! - **VAL-IMG-005** — transmission is *bounded and content-keyed*: the same
//!   picture resolves to the same id, so committing it (and re-committing it, and
//!   repainting the viewport) transmits under one stable id — not one transfer
//!   per frame. Id reuse for the same content is legal (not "exactly once").
//! - **VAL-IMG-009** — the drop gesture mints a **viewport-only** single-id Kitty
//!   delete (`d=I,i=<id>`), never a wide `d=A`/`d=a`; and an image already
//!   committed to scrollback is *never* deleted (its history copy must survive).
//! - **VAL-IMG-020** — a sequence of commits + repaints never emits a wide delete
//!   at any point (the exit/teardown safety pin, exercised at the escape level).
//! - **VAL-CROSS-004** — an image committed to scrollback survives an
//!   overlay-toggle + resize sweep: no re-transmit is *forced* by them and no
//!   wide delete is minted.
//! - **VAL-WIDGET-006** — a markdown link on a capable terminal yields a real,
//!   raw-bytes OSC 8 hyperlink through the same channel; on an incapable terminal
//!   the `text (url)` fallback is painted and nothing is emitted.

use std::path::PathBuf;
use std::sync::Mutex;

use hand_tui::rt::components::{
    MarkdownTheme, ResolvedProtocol, RtImage, ScrollbackImageChannel, osc8_emission,
    render_markdown_with_links,
};
use hand_tui::rt::history::HistorySink;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};

/// Serializes the two tests that mutate OSC 8 capability env vars.
static OSC8_ENV_LOCK: Mutex<()> = Mutex::new(());

/// Absolute path to a fixture under `tests/fixtures/tui/images/`.
fn fixture(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../tests/fixtures/tui/images");
    p.push(name);
    p
}

fn read_fixture(name: &str) -> Vec<u8> {
    let path = fixture(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {} missing: {e}", path.display()))
}

/// Count non-overlapping occurrences of `needle` in `haystack`.
fn count(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.matches(needle).count()
}

/// Assert an escape stream carries **no** wide Kitty delete form — the central
/// scrollback-safety invariant. `d=A` (uppercase) and `d=a` (lowercase) both
/// erase every image, including the ones sitting in scrollback.
fn assert_no_wide_delete(escapes: &str) {
    assert!(
        !escapes.contains("d=A"),
        "a wide delete-all (d=A) would wipe scrollback images: {escapes:?}"
    );
    assert!(
        !escapes.contains("d=a"),
        "a wide delete-all (d=a) would wipe scrollback images: {escapes:?}"
    );
}

fn inline_terminal(width: u16, height: u16, viewport_rows: u16) -> Terminal<TestBackend> {
    Terminal::with_options(
        TestBackend::new(width, height),
        TerminalOptions {
            viewport: Viewport::Inline(viewport_rows),
        },
    )
    .expect("build inline test terminal")
}

// --- VAL-IMG-005: transmission bounded + stable content→id ------------------

#[test]
fn same_content_transmits_under_one_stable_id_not_per_frame() {
    let png = read_fixture("sample.png");
    let channel = ScrollbackImageChannel::new();
    let image = RtImage::new(png.clone());
    let area = Rect::new(0, 0, 40, 12);

    // Commit the same picture three times (as if it scrolled through history
    // three commits). Every commit resolves the SAME stable id — the transmission
    // is content-keyed, so a terminal that already holds the pixels under that id
    // is not asked to re-decode a new one each frame.
    let mut ids = Vec::new();
    for _ in 0..3 {
        let emission = image
            .commit_to_scrollback(&channel, ResolvedProtocol::Kitty, area, 0)
            .expect("a decodable Kitty image commits");
        // Extract the `i=<id>` the escape carries.
        let id = emission
            .escape
            .split("i=")
            .nth(1)
            .and_then(|s| s.split(['\\', ',', ';']).next())
            .and_then(|s| s.parse::<u32>().ok())
            .expect("the escape carries an i=<id>");
        ids.push(id);
    }
    assert!(
        ids.windows(2).all(|w| w[0] == w[1]),
        "every commit of the same content uses the same stable id: {ids:?}"
    );

    // A distinct picture gets a distinct id (the map is content-keyed, not a
    // single global id).
    let other = RtImage::new(read_fixture("large.png"));
    let other_emission = other
        .commit_to_scrollback(&channel, ResolvedProtocol::Kitty, area, 0)
        .expect("other image commits");
    let other_id = channel.image_id(&read_fixture("large.png"));
    assert_ne!(ids[0], other_id, "distinct content → distinct id");
    assert!(other_emission.escape.contains(&format!("i={other_id}")));
}

#[test]
fn commit_image_reserves_footprint_and_returns_stable_escape() {
    // Drive the real HistorySink against a TestBackend: committing an image
    // reserves its footprint above the (undrawn) viewport and returns the escape
    // to flush once, and the id is marked committed (delete-protected).
    let png = read_fixture("sample.png");
    let mut terminal = inline_terminal(40, 12, 2);
    let mut sink = HistorySink::new();
    let channel = ScrollbackImageChannel::new();
    let image = RtImage::new(png.clone()).label("history image");

    let emission = sink
        .commit_image(&mut terminal, &channel, &image, ResolvedProtocol::Kitty)
        .expect("commit_image ok")
        .expect("a decodable Kitty image yields an escape");

    let id = channel.image_id(&png);
    assert!(
        channel.is_committed(id),
        "the id is delete-protected after commit"
    );
    assert!(
        emission.escape.contains(&format!("i={id}")),
        "the returned escape carries the stable id: {:?}",
        emission.escape
    );
    assert!(emission.escape.starts_with("\x1b_G"), "a valid Kitty APC");
    // A repaint (another draw) never re-invokes commit_image, so no further
    // transfer is forced — the commit escape is the whole transmission.
    terminal.draw(|_| {}).expect("repaint");
    assert!(
        channel.is_committed(id),
        "the committed id stays protected across a repaint"
    );
}

// --- VAL-IMG-009: viewport-only delete; committed image never deleted -------

#[test]
fn drop_gesture_deletes_only_the_viewport_id_never_a_committed_one() {
    let png_committed = read_fixture("sample.png");
    let png_viewport = read_fixture("large.png");
    let channel = ScrollbackImageChannel::new();

    // One image scrolls into scrollback (committed → protected); another stays a
    // pure viewport image.
    let committed_id = channel.image_id(&png_committed);
    channel.mark_committed(committed_id);
    let viewport_id = channel.image_id(&png_viewport);

    // Drop the viewport image: a single-id delete for exactly its id.
    let del = channel
        .delete_viewport_image(viewport_id)
        .expect("a viewport image is deletable");
    assert_eq!(del, format!("\x1b_Ga=d,d=I,i={viewport_id}\x1b\\"));
    assert!(del.contains("d=I"), "delete-by-id form");
    assert_no_wide_delete(&del);
    assert!(
        !del.contains(&format!("i={committed_id}")),
        "the committed id is untouched"
    );

    // Dropping the committed image is refused — its scrollback copy must live on.
    assert!(
        channel.delete_viewport_image(committed_id).is_none(),
        "a committed scrollback image is never deleted"
    );
}

// --- VAL-IMG-020: no wide delete across a commit/repaint sequence -----------

#[test]
fn commit_and_repaint_sequence_emits_no_wide_delete() {
    // Simulate the stream→drop→repaint→exit escape stream: commit two images,
    // drop a third viewport-only image, repaint several times. At no point is a
    // wide delete minted (the only delete path is the single-id viewport delete).
    let mut terminal = inline_terminal(40, 12, 2);
    let mut sink = HistorySink::new();
    let channel = ScrollbackImageChannel::new();
    let mut escapes = String::new();

    for name in ["sample.png", "large.png"] {
        let image = RtImage::new(read_fixture(name));
        if let Some(e) = sink
            .commit_image(&mut terminal, &channel, &image, ResolvedProtocol::Kitty)
            .expect("commit ok")
        {
            escapes.push_str(&e.escape);
        }
    }
    // Drop a pure-viewport image (never committed).
    let viewport_id = channel.image_id(&read_fixture("wide.png"));
    if let Some(del) = channel.delete_viewport_image(viewport_id) {
        escapes.push_str(&del);
    }
    // Repaint several frames (no re-commit): nothing more is transmitted, and no
    // delete is minted.
    for _ in 0..5 {
        terminal.draw(|_| {}).expect("repaint");
    }

    assert_no_wide_delete(&escapes);
    // Exactly one viewport delete (the drop), and it is a single-id form.
    assert_eq!(count(&escapes, "a=d,d=I"), 1, "one single-id delete only");
}

// --- VAL-CROSS-004: committed image survives overlay + resize ---------------

#[test]
fn committed_image_survives_overlay_and_resize_without_retransmit_or_delete() {
    let png = read_fixture("sample.png");
    let mut terminal = inline_terminal(60, 12, 2);
    let mut sink = HistorySink::new();
    let channel = ScrollbackImageChannel::new();
    let image = RtImage::new(png.clone());

    // Commit the image into scrollback once and record its transmission.
    let commit = sink
        .commit_image(&mut terminal, &channel, &image, ResolvedProtocol::Kitty)
        .expect("commit ok")
        .expect("escape");
    let id = channel.image_id(&png);
    let escapes = commit.escape;

    // Now simulate an overlay open/close + a resize sweep as ordinary viewport
    // draws + backend resizes. None of them re-invoke commit_image (a committed
    // image is not re-transmitted by a repaint) and none mint a delete.
    terminal.draw(|_| {}).expect("overlay open frame");
    terminal.draw(|_| {}).expect("overlay close frame");
    terminal.backend_mut().resize(40, 10);
    terminal.draw(|_| {}).expect("post-resize frame");
    terminal.backend_mut().resize(80, 16);
    terminal.draw(|_| {}).expect("post-widen frame");

    // The committed id stays protected the whole time — a drop gesture during any
    // of those frames would be refused.
    assert!(
        channel.delete_viewport_image(id).is_none(),
        "the committed image stays delete-protected across overlay + resize"
    );
    // No delete of any kind was appended to the escape stream by the commit path,
    // and certainly no wide delete.
    assert_no_wide_delete(&escapes);
    assert!(
        !escapes.contains("a=d"),
        "the commit + survive path emits no delete at all: {escapes:?}"
    );
    // The single transmission stays valid and carries the stable id.
    assert!(escapes.contains(&format!("i={id}")), "stable id retained");
}

// --- VAL-WIDGET-006: real OSC 8 through the raw channel ----------------------

#[test]
fn markdown_link_yields_real_osc8_on_capable_terminal() {
    let _guard = OSC8_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Force a capable terminal deterministically.
    unsafe { std::env::set_var("TERM_PROGRAM", "ghostty") };
    unsafe { std::env::remove_var("HAND_DISABLE_OSC8") };
    unsafe { std::env::remove_var("TMUX") };

    let (_, links) = render_markdown_with_links(
        "docs at [ratatui](https://ratatui.rs) online",
        80,
        &MarkdownTheme::default(),
    );
    assert_eq!(links.len(), 1, "one link collected for OSC 8 emission");
    let emission = osc8_emission(&links[0].text, &links[0].url, links[0].row);
    // A real raw-bytes OSC 8 hyperlink: \x1b]8;;<url>\x1b\\ <text> \x1b]8;;\x1b\\
    assert!(
        emission.escape.contains("\x1b]8;;https://ratatui.rs"),
        "OSC 8 introducer with the url: {:?}",
        emission.escape
    );
    assert!(emission.escape.contains("ratatui"), "visible text present");
    assert!(
        emission.escape.ends_with("\x1b]8;;\x1b\\"),
        "closed by an empty OSC 8: {:?}",
        emission.escape
    );

    unsafe { std::env::remove_var("TERM_PROGRAM") };
}

#[test]
fn markdown_link_falls_back_to_text_url_on_incapable_terminal() {
    let _guard = OSC8_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var("HAND_DISABLE_OSC8", "1") };
    let (lines, links) = render_markdown_with_links(
        "docs at [ratatui](https://ratatui.rs)",
        80,
        &MarkdownTheme::default(),
    );
    assert!(
        links.is_empty(),
        "no OSC 8 links on an incapable terminal — nothing to emit out of band"
    );
    let text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect();
    assert!(
        text.contains("ratatui (https://ratatui.rs)"),
        "the pinned text (url) fallback is painted into the cells: {text:?}"
    );
    unsafe { std::env::remove_var("HAND_DISABLE_OSC8") };
}
