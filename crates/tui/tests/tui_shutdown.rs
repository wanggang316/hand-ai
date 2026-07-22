//! Teardown contract: the run loop's exit path must erase the painted
//! reverse-video cursor cell, otherwise the user's shell prompt appears
//! next to a leftover inverted block — two cursors on screen.
//!
//! The guarantee is positional. Every frame the Tui parks the hardware
//! cursor on the [`CURSOR_MARKER`] cell — the same cell the focused
//! component paints its reverse-video block on — and `shutdown_terminal`
//! erases from the cursor to the end of the display (`\x1b[J`, cursor
//! cell inclusive) before cooked mode is restored. This test pins the
//! byte-stream ordering that makes the guarantee hold: after the final
//! cursor-parking write, nothing may move the cursor before the shutdown
//! erase fires.

use std::sync::{Arc, Mutex};

use hand_tui::{CURSOR_MARKER, Component, Terminal, TerminalCapabilities, Tui};
use tokio::sync::mpsc;

// ---- Terminal that lets the test inspect emitted bytes after `Tui`
// consumes it. Mirrors the helper in `tui_overlay_style_leak.rs`.
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

/// A focused prompt line: text, then the cursor marker, then the
/// reverse-video cursor cell — the shape `EditorComponent` and
/// `InputComponent` emit while focused.
struct PromptLine;

impl Component for PromptLine {
    fn render(&self, _w: u16) -> Vec<String> {
        vec![format!("ab{CURSOR_MARKER}\x1b[7m \x1b[0m")]
    }
}

#[tokio::test]
async fn shutdown_erase_starts_on_the_parked_cursor_cell() {
    let (term, output) = SharedTerminal::new();
    let mut tui = Tui::new(Box::new(term));
    tui.root_mut().add_child_with_id(Box::new(PromptLine));

    // Close stdin immediately: the first-frame guarantee paints once, then
    // the loop exits through the shutdown path.
    let (tx, rx) = mpsc::unbounded_channel();
    drop(tx);

    tokio::time::timeout(
        std::time::Duration::from_millis(200),
        tui.run_with_events(rx),
    )
    .await
    .expect("run loop did not exit on closed stdin")
    .expect("run errored");

    let writes = output.lock().unwrap().clone();

    // Sanity: a reverse-video cursor cell was painted on screen.
    assert!(
        writes.iter().any(|w| w.contains("\x1b[7m")),
        "expected a reverse-video cursor cell in the frame: {writes:?}"
    );

    // The teardown erase is the last write before cooked mode is restored.
    assert_eq!(
        writes.last().map(String::as_str),
        Some("\x1b[J"),
        "shutdown must end with an erase-below: {writes:?}"
    );

    // The frame parked the hardware cursor on the marker cell: the last
    // cursor-movement write is the park (cursor-up + absolute column).
    let park_idx = writes
        .iter()
        .rposition(|w| w.contains("\x1b[") && w.ends_with('G'))
        .expect("no cursor-parking write found");

    // Between parking the cursor on the cell and the shutdown erase, only
    // cursor-visibility toggles may be written — any movement would shift
    // the erase origin off the cell and leave it painted after exit.
    let tail = &writes[park_idx + 1..writes.len() - 1];
    assert!(
        tail.iter().all(|w| w == "\x1b[?25h"),
        "cursor must not move between parking and the shutdown erase: {writes:?}"
    );
}
