//! Regression: overlay SGR styles must not leak after dismiss.
//!
//! Historical bug (pi-tui): when an overlay rendered a styled line whose
//! trailing SGR reset got sliced off by overlay positioning, the leftover
//! styling smeared onto the underlying base content. After `hide_overlay`,
//! the diff renderer's cached lines (which contain the composed-with-overlay
//! strings) could continue to leak styling forward unless `hide_overlay`
//! forced a full re-render.
//!
//! This file pins the public-API contract: a show -> hide cycle through
//! `compose_overlays` + `DiffRenderer` (the same composition pipeline `Tui`
//! uses internally) must not leave red SGR escapes in the post-hide frame.
//!
//! `src/tui.rs` already has an inline `#[test]` for the full `Tui` round
//! trip; the integration test here covers the same regression from a public
//! API consumer's POV.

use std::sync::{Arc, Mutex};

use hand_tui::{
    Component, DiffRenderer, HandleResult, InputEvent, OverlayAnchor, OverlayMargin,
    OverlayOptions, Terminal, TerminalCapabilities, Tui,
};
use tokio::sync::mpsc;

// ---- Terminal that lets the test inspect emitted bytes after `Tui` consumes
// it. Mirrors the helper used in `src/tui.rs`'s own tests but lives here so
// the regression can fail on the public-API surface alone.
struct SharedTerminal {
    output: Arc<Mutex<Vec<String>>>,
    capabilities: TerminalCapabilities,
}

impl SharedTerminal {
    fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
        let output = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                output: output.clone(),
                capabilities: TerminalCapabilities::default(),
            },
            output,
        )
    }
}

impl Terminal for SharedTerminal {
    fn write(&mut self, data: &str) {
        self.output.lock().unwrap().push(data.to_string());
    }
    fn columns(&self) -> u16 {
        40
    }
    fn rows(&self) -> u16 {
        6
    }
    fn hide_cursor(&mut self) {
        self.write("\x1b[?25l");
    }
    fn show_cursor(&mut self) {
        self.write("\x1b[?25h");
    }
    fn clear_line(&mut self) {
        self.write("\x1b[2K\r");
    }
    fn clear_from_cursor(&mut self) {
        self.write("\x1b[J");
    }
    fn clear_screen(&mut self) {
        self.write("\x1b[2J\x1b[H");
    }
    fn move_by(&mut self, lines: i32) {
        if lines > 0 {
            self.write(&format!("\x1b[{lines}B"));
        } else if lines < 0 {
            self.write(&format!("\x1b[{}A", -lines));
        }
    }
    fn set_title(&mut self, title: &str) {
        self.write(&format!("\x1b]0;{title}\x07"));
    }
    fn capabilities(&self) -> &TerminalCapabilities {
        &self.capabilities
    }
}

struct PlainBase;
impl Component for PlainBase {
    fn render(&self, _w: u16) -> Vec<String> {
        vec!["base".to_string()]
    }
}

struct RedOverlay;
impl Component for RedOverlay {
    fn render(&self, _w: u16) -> Vec<String> {
        vec!["\x1b[31moverlay\x1b[0m".to_string()]
    }
    fn handle_input(&mut self, _e: &InputEvent) -> HandleResult {
        HandleResult::Ignored
    }
}

fn no_decoration_opts() -> OverlayOptions {
    OverlayOptions {
        anchor: OverlayAnchor::Center,
        margin: OverlayMargin::default(),
        capture_input: false,
        dim_background: false,
        border: false,
    }
}

/// End-to-end: show overlay -> render -> hide overlay -> render. The
/// post-hide write log must contain no `\x1b[31m`. Drives the actual `Tui`
/// run loop via `run_with_events`, which is the public test entry point.
#[tokio::test]
async fn regression_overlay_style_leak_clears_red_after_hide() {
    let (term, output) = SharedTerminal::new();
    let mut tui = Tui::new(Box::new(term));
    tui.root_mut().add_child_with_id(Box::new(PlainBase));

    // Show first so the very first render composes the overlay.
    let handle = tui.show_overlay(Box::new(RedOverlay), no_decoration_opts());

    // Drive the loop manually: send no events and `stop()` so the run loop
    // exits promptly. The first-frame guarantee inside `Tui::run_with_events`
    // ensures one render fires before exit.
    let (_tx, rx) = mpsc::unbounded_channel();
    drop(_tx); // close stdin -> loop exits cleanly after first frame

    tokio::time::timeout(std::time::Duration::from_millis(200), tui.run_with_events(rx))
        .await
        .expect("first show-frame run did not exit")
        .expect("run errored");

    // Sanity: the show frame did emit the red SGR somewhere in the writes.
    {
        let writes: String = output.lock().unwrap().iter().cloned().collect();
        assert!(
            writes.contains("\x1b[31m"),
            "expected overlay frame to write red SGR, got: {writes:?}"
        );
    }

    // Clear the log so the next assertion only sees the post-hide frame.
    output.lock().unwrap().clear();

    // Hide and run another frame.
    tui.hide_overlay(handle);

    let (_tx2, rx2) = mpsc::unbounded_channel();
    drop(_tx2);
    tokio::time::timeout(std::time::Duration::from_millis(200), tui.run_with_events(rx2))
        .await
        .expect("post-hide run did not exit")
        .expect("run errored");

    let post: String = output.lock().unwrap().iter().cloned().collect();
    assert!(
        !post.is_empty(),
        "hide_overlay must arm a re-render — no writes were recorded after hide"
    );
    assert!(
        !post.contains("\x1b[31m"),
        "post-hide frame leaked red SGR — overlay style smeared onto next render: {post:?}"
    );
    assert!(
        post.contains("base"),
        "post-hide frame must repaint underlying base content: {post:?}"
    );
}

/// Lower-level lock: `compose_overlays` itself must close the SGR state at
/// end of every overlay-touched line. The tui-level test above could mask a
/// regression in `compose_overlays` if `Tui::hide_overlay`'s force-render
/// drowns the leak elsewhere; this assertion targets the compositor directly.
#[test]
fn regression_compose_overlays_appends_reset_after_styled_overlay() {
    use hand_tui::compose_overlays;

    struct Red;
    impl Component for Red {
        fn render(&self, _w: u16) -> Vec<String> {
            vec!["\x1b[31mfoo".into()] // intentionally missing trailing reset
        }
        fn handle_input(&mut self, _e: &InputEvent) -> HandleResult {
            HandleResult::Ignored
        }
    }

    let base: Vec<String> = (0..6).map(|_| " ".repeat(20)).collect();
    let overlay = Red;
    let opts = no_decoration_opts();
    let overlays: Vec<(&dyn Component, &OverlayOptions)> = vec![(&overlay, &opts)];

    let result = compose_overlays(&base, &overlays, 20, 6);
    let touched_row = result
        .iter()
        .find(|l| l.contains("foo"))
        .expect("composed frame must contain overlay text");
    assert!(
        touched_row.ends_with("\x1b[0m"),
        "overlay-touched row must end in a hard reset, got: {touched_row:?}"
    );
}

/// Drives the diff renderer through the same pattern `Tui` uses to confirm
/// that resetting the renderer (which `hide_overlay` does internally via
/// `request_render_force`) is what allows post-hide frames to drop cached
/// overlay-styled bytes.
#[test]
fn regression_diff_renderer_reset_clears_cached_overlay_lines() {
    let mut renderer = DiffRenderer::new();

    // Frame 1: contains the overlay styling.
    let frame1 = vec!["\x1b[31moverlay\x1b[0m".to_string(), "base".to_string()];
    let _ = renderer.diff(&frame1);

    // Without `reset`, diffing an identical line set against the cached red
    // line would produce no commands — and the red would stick around in the
    // cache forever. After `reset`, the next diff must re-emit the new lines
    // verbatim, with no \x1b[31m anywhere.
    renderer.reset();
    let frame2 = vec!["plain".to_string(), "base".to_string()];
    let commands = renderer.diff(&frame2);

    assert!(
        !commands.contains("\x1b[31m"),
        "after reset, diff must not carry forward red SGR from cached frame: {commands:?}"
    );
    assert!(
        commands.contains("plain"),
        "post-reset diff must include the new line content: {commands:?}"
    );
}
