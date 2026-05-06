//! Interactive demo for hand-tui — exercises editor + markdown + autocomplete + overlay.
//!
//! Run with: `cargo run -p model-examples --bin tui-demo`
//! Press Ctrl+C twice to quit.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use hand_tui::{
    EditorComponent, KeyName, ListenerResult, MarkdownComponent, OverlayAnchor, OverlayMargin,
    OverlayOptions, ProcessTerminal, SlashCommand, SlashCommandProvider, TextComponent, Tui,
    TuiError,
};
use hand_tui::tui::InputEvent;

#[tokio::main]
async fn main() -> Result<(), TuiError> {
    // Best-effort raw mode + alternate screen so the demo behaves like a real TUI.
    // Failures are non-fatal — `cargo run < /dev/null` should still exit cleanly.
    let mut term = ProcessTerminal::new()?;
    let _ = term.enter_raw_mode();
    let _ = term.enter_alternate_screen();

    let mut tui = Tui::new(Box::new(term));

    // Markdown header with key hints.
    tui.root_mut().add_child(Box::new(MarkdownComponent::new(
        "# hand-tui demo\n\n\
         - Type freely.\n\
         - Press `/` to trigger slash-command autocomplete.\n\
         - Press `Ctrl+O` to toggle a centered overlay.\n\
         - Press `Ctrl+C` twice to quit.\n",
    )));

    // Editor with a small slash-command catalogue.
    let mut editor = EditorComponent::new();
    editor.set_viewport_height(10);
    editor.set_autocomplete_provider(Arc::new(SlashCommandProvider::new(vec![
        SlashCommand::new("help", "Show help"),
        SlashCommand::new("save", "Save buffer").with_arguments("<path>"),
        SlashCommand::new("quit", "Exit demo"),
    ])));

    let editor_id = tui.root_mut().add_child_with_id(Box::new(editor));
    tui.set_focus(Some(editor_id));

    // Track Ctrl+C presses (two to quit) and Ctrl+O overlay state. Listeners are
    // `FnMut + Send`, so shared state must go through `Arc`.
    let ctrl_c_count = Arc::new(AtomicU8::new(0));
    let overlay_visible = Arc::new(AtomicU8::new(0));

    let ctrl_c_count_for_listener = ctrl_c_count.clone();
    let overlay_visible_for_listener = overlay_visible.clone();
    tui.add_input_listener(Box::new(move |event: &InputEvent| {
        if let InputEvent::Key(key) = event {
            // Reset the Ctrl+C counter on any non-Ctrl-C key.
            let is_ctrl_c =
                matches!(key.name, KeyName::Char('c')) && key.modifiers.ctrl;
            let is_ctrl_o =
                matches!(key.name, KeyName::Char('o')) && key.modifiers.ctrl;

            if is_ctrl_c {
                let n = ctrl_c_count_for_listener.fetch_add(1, Ordering::Relaxed) + 1;
                if n >= 2 {
                    // Best-effort restore happens via ProcessTerminal::Drop in
                    // the parent scope. exit() is the simplest way out from a
                    // listener without plumbing a stop-handle through the Tui.
                    std::process::exit(0);
                }
                return ListenerResult::consume();
            } else {
                ctrl_c_count_for_listener.store(0, Ordering::Relaxed);
            }

            if is_ctrl_o {
                // Toggle a flag the main task picks up. We can't call
                // `Tui::show_overlay` from inside a listener (it borrows `&mut
                // Tui`), so the listener just records intent.
                overlay_visible_for_listener.fetch_xor(1, Ordering::Relaxed);
                return ListenerResult::consume();
            }
        }
        ListenerResult::pass()
    }));

    // The overlay-toggle flag is observed but not yet acted upon — wiring it
    // into the run loop would require either a custom event-driven main loop
    // or a public Tui hook for "before each render". For a smoke test we
    // accept the limitation: pressing Ctrl+O is parsed and consumed without
    // panicking, which is what the demo is meant to verify.
    //
    // TODO(api): expose a per-tick callback on `Tui` so demos can mutate the
    // tree between frames. For now, show a non-capturing static overlay up
    // front so the overlay rendering path is exercised at least once.
    let _ = overlay_visible; // silence unused warning when not wired further.
    tui.show_overlay(
        Box::new(TextComponent::new(
            "[overlay] Ctrl+C twice to quit",
        )),
        OverlayOptions {
            anchor: OverlayAnchor::BottomRight,
            margin: OverlayMargin::uniform(1),
            capture_input: false,
            dim_background: false,
            border: true,
        },
    );

    tui.run().await?;
    Ok(())
}
