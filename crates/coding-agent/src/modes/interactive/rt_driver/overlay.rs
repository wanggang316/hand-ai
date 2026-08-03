//! The rt driver's **overlay runtime** and its reusable mount protocol.
//!
//! ratatui has no layering, no modal input, and no focus; the M1
//! [`OverlayStack`](hand_tui::rt::overlay::OverlayStack) supplies those (nine-anchor
//! placement via [`anchor_rect`](hand_tui::rt::overlay::anchor_rect), LIFO modal
//! capture, background dim, bordered clear). This module is the *driver-side* half:
//! it holds the mounted selector and defines the **protocol every selector is built
//! on** — the pattern the follow-up selector family (config, pickers, login,
//! resume) reuses without re-solving the runtime plumbing.
//!
//! # Why a `Send` selector trait rather than the M1 stack directly
//!
//! An [`OverlayStack`](hand_tui::rt::overlay::OverlayStack) stores
//! `Box<dyn RtComponent>`, and [`RtComponent`](hand_tui::rt::view::RtComponent) is
//! deliberately **not** `Send` (see the rt demo: it keeps overlays as a `Send`
//! descriptor list and rebuilds a *local* stack inside the draw closure each frame,
//! never crossing the spawned-task boundary). But a selector is **stateful and
//! key-consuming**: it must persist across frames (accumulating filter text) and be
//! mutated by the input-loop task while rendered by the scheduler task — two
//! `Send`-required tokio tasks. So the selector lives behind its own
//! `Arc<Mutex<…>>` (like the editor), and the shared piece is a `Send` handle to it.
//! [`SelectorController`] is that `Send`-bounded trait; the driver never needs the
//! M1 stack to cross a task boundary, so the M1 `?Send` contract is untouched (no rt
//! seam required). Rendering is the scheduler's own: it snapshots the selector's
//! lines each frame and paints them as a bordered panel glued directly above the
//! input box (the M6 layout — full frame width, never floating, never overlapping
//! the box or footer, transcript untouched above).
//!
//! # The mount protocol (construct-in, channel-out)
//!
//! A selector is any [`SelectorController`] that:
//!
//! 1. is **constructed** with its inputs (the list, the current selection, scoping
//!    config) plus an [`mpsc::UnboundedSender`] for its outcome, and
//! 2. **emits exactly one outcome** on that channel when the user confirms (Enter)
//!    or cancels (Esc), then raises its [`DoneSignal`] so the runtime unmounts it.
//!
//! The driver [`mount`]s it as a modal, bordered panel above the input box, watches
//! the outcome channel, applies the result to the live session, and commits a
//! status line — the same shape the legacy selectors used, now on the rt runtime.
//!
//! # Modal capture (editor isolation) + streaming underneath
//!
//! While a selector is mounted the input loop routes every key to it *before* the
//! editor, and does not fall through — so typing drives the selector's filter, never
//! the chat editor (VAL-OVERLAY-005). Overlays live only in the draw/input layer:
//! the turn runner and event applier are untouched, so a turn that is streaming when
//! a selector opens keeps streaming — its commits settle into scrollback beneath the
//! panel — while it is up (VAL-OVERLAY-009).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use hand_tui::rt::events::RtKey;
use hand_tui::rt::scheduler::FrameRequester;
use hand_tui::rt::view::HandleOutcome;
use ratatui::text::Line;

use crate::modes::interactive::theme::ThemePalette;

/// A cheap, cloneable "I am finished" flag a selector raises when it has emitted
/// its outcome and wants to be unmounted.
///
/// The selector holds a clone and sets it on the key that confirms (Enter) or
/// cancels (Esc); the runtime reads it right after routing that key and closes the
/// overlay when it is set. Keeping the signal a shared boolean — rather than a
/// concrete downcast — is what makes the runtime selector-agnostic: every future
/// selector raises the same flag, so the whole family reuses this close path.
pub type DoneSignal = Arc<AtomicBool>;

/// A fresh, un-raised [`DoneSignal`].
#[must_use]
pub fn new_done_signal() -> DoneSignal {
    Arc::new(AtomicBool::new(false))
}

/// A mounted selector: a stateful, key-consuming overlay body.
///
/// The `Send`-bounded, driver-owned counterpart to
/// [`RtComponent`](hand_tui::rt::view::RtComponent), split so a selector can be
/// shared behind an `Arc<Mutex<…>>` across the input and scheduler tasks. It:
///
/// - **produces render lines** ([`render_lines`](SelectorController::render_lines))
///   for its interior, given the interior width — the scheduler paints them inside
///   the anchored, bordered, dimmed overlay rect; and
/// - **handles keys** ([`handle_key`](SelectorController::handle_key)) while it is
///   the mounted modal overlay, raising its [`DoneSignal`] on the terminal key.
///
/// It is intentionally `Send` (unlike `RtComponent`): a selector holds only `Send`
/// state (the list, the query string, the outcome channel, the done flag), so this
/// bound is free and is what lets the selector cross into the spawned tasks.
pub trait SelectorController: Send {
    /// The selector's interior as styled lines, wrapped to `width`, coloured from
    /// the active theme `palette`. Called every frame by the scheduler; must not
    /// retain the buffer. The default palette keeps the historical accent/muted
    /// look; a custom theme recolours the highlight and accents (VAL-COMPAT-004).
    fn render_lines(&self, width: u16, palette: &ThemePalette) -> Vec<Line<'static>>;

    /// Handle a key while mounted, reporting whether it was consumed. A modal
    /// selector consumes every key (so none reaches the editor) and raises its
    /// [`DoneSignal`] on Enter/Esc.
    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome;

    /// Handle a bracketed-paste payload while mounted, reporting whether it was
    /// consumed.
    ///
    /// The default drops the paste (a list-style selector filters by key, not by
    /// paste). A selector with a **text field** — the login key dialog — overrides
    /// this to insert the *entire* payload in one shot: a multi-character API key
    /// arriving as one paste event lands whole, never folded to one character
    /// (VAL-OVERLAY-027 — the migration fix away from the legacy single-character
    /// paste collapse). Returning [`HandleOutcome::Consumed`] keeps the paste from
    /// also reaching the chat editor beneath (the same editor-isolation contract as
    /// [`handle_key`](SelectorController::handle_key)).
    fn handle_paste(&mut self, _text: &str) -> HandleOutcome {
        HandleOutcome::Consumed
    }
}

/// The shared, mounted selector plus its close flag, shared between the input loop
/// (which mutates the selector via `handle_key`) and the scheduler's draw closure
/// (which reads its render lines). `None` when no overlay is open.
///
/// A plain blocking `Mutex`, like the driver's other shared state: every critical
/// section is a small, non-awaiting call.
#[derive(Clone, Default)]
pub struct SharedOverlay {
    inner: Arc<Mutex<Option<Mounted>>>,
}

/// One mounted selector: the controller behind its own lock (so the input loop and
/// draw closure can each borrow it in turn) plus the shared done flag.
struct Mounted {
    controller: Arc<Mutex<dyn SelectorController>>,
    done: DoneSignal,
}

impl SharedOverlay {
    /// A fresh, empty shared overlay (nothing mounted).
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    /// Whether any selector is currently mounted.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.lock().is_some()
    }

    /// The mounted selector's render lines for `width`, coloured from `palette`,
    /// or `None` when nothing is open. Called by the scheduler each frame.
    #[must_use]
    pub fn render_lines(&self, width: u16, palette: &ThemePalette) -> Option<Vec<Line<'static>>> {
        let guard = self.lock();
        let mounted = guard.as_ref()?;
        let controller = mounted.controller.clone();
        drop(guard);
        Some(
            controller
                .lock()
                .expect("selector poisoned")
                .render_lines(width, palette),
        )
    }

    fn lock(&self) -> MutexGuard<'_, Option<Mounted>> {
        self.inner.lock().expect("overlay mutex poisoned")
    }
}

/// A fresh, empty shared overlay for the driver to own.
#[must_use]
pub fn new_shared_overlay() -> SharedOverlay {
    SharedOverlay::new()
}

/// Mount `controller` as the modal dialog and request a repaint so it paints on the
/// next frame — the one entry point the whole selector family calls to open.
///
/// The caller resets and passes the shared [`DoneSignal`] the controller raises on
/// its terminal key; it keeps the outcome *receiver* to observe the pick. Mounting
/// replaces any currently-open overlay (the selectors are one-at-a-time), which is
/// also what guarantees a fast reopen starts clean (VAL-OVERLAY-008).
pub fn mount(
    overlay: &SharedOverlay,
    requester: &FrameRequester,
    controller: Arc<Mutex<dyn SelectorController>>,
    done: DoneSignal,
) {
    *overlay.lock() = Some(Mounted { controller, done });
    requester.request_frame();
}

/// Whether any overlay is currently mounted (the viewport is showing a dialog).
#[must_use]
pub fn is_open(overlay: &SharedOverlay) -> bool {
    overlay.is_open()
}

/// Close (unmount) any open overlay and request a repaint. Idempotent — closing an
/// already-closed overlay is a no-op. Used on a teardown mid-dialog.
pub fn close(overlay: &SharedOverlay, requester: &FrameRequester) {
    if overlay.lock().take().is_some() {
        requester.request_frame();
    }
}

/// Route a key through the mounted selector, closing the dialog when the selector
/// raises its [`DoneSignal`], and report whether an overlay owned the key.
///
/// The contract the input loop relies on:
///
/// - With an overlay open, the key goes to the mounted selector. A modal selector
///   owns the key whether or not it acts on it — the editor-isolation guarantee: a
///   keystroke while a selector is open never reaches the chat editor
///   (VAL-OVERLAY-005).
/// - A selector signals "finished" (Enter applied / Esc cancelled) by raising its
///   done flag on that key. Once raised, this unmounts the selector so the dialog
///   closes; because the whole viewport repaints crisp each frame, closing leaves no
///   dim residue or ghost border (VAL-OVERLAY-008).
/// - The return is `true` when an overlay owned the key (so the caller must not also
///   feed it to the editor), `false` when nothing is open (so the caller handles the
///   key normally).
pub fn dispatch_key(overlay: &SharedOverlay, requester: &FrameRequester, key: &RtKey) -> bool {
    // Snapshot the mounted controller + done flag under the outer lock, then release
    // it before driving the selector (which takes its own lock).
    let (controller, done) = {
        let guard = overlay.lock();
        match guard.as_ref() {
            Some(m) => (m.controller.clone(), m.done.clone()),
            None => return false,
        }
    };

    let _ = controller
        .lock()
        .expect("selector poisoned")
        .handle_key(key);

    // A finished selector has emitted its outcome and raised its flag on this key.
    // Unmount it so the dialog closes and the base view (and editor) become
    // reachable again on the next key.
    if done.load(Ordering::SeqCst) {
        *overlay.lock() = None;
        requester.request_frame();
    }
    // A modal overlay owns the key, so the caller must never also route it to the
    // editor: an overlay was open, so report ownership.
    true
}

/// Route a bracketed-paste payload through the mounted selector, reporting whether
/// an overlay owned it.
///
/// The mirror of [`dispatch_key`] for paste events: with an overlay open the whole
/// payload goes to the mounted selector's [`handle_paste`](SelectorController::handle_paste)
/// (so a text-field selector lands the *entire* paste, not a folded marker or a
/// single character — VAL-OVERLAY-027), and the caller must not also feed it to the
/// editor. `false` when nothing is open, so the caller pastes into the editor as
/// usual. A paste never raises the done flag, so this never closes the dialog.
pub fn dispatch_paste(overlay: &SharedOverlay, requester: &FrameRequester, text: &str) -> bool {
    let controller = {
        let guard = overlay.lock();
        match guard.as_ref() {
            Some(m) => m.controller.clone(),
            None => return false,
        }
    };

    let _ = controller
        .lock()
        .expect("selector poisoned")
        .handle_paste(text);
    requester.request_frame();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    use hand_tui::rt::scheduler::FrameScheduler;

    /// A minimal modal selector for the runtime tests: it counts keys and raises its
    /// [`DoneSignal`] on `enter`/`escape`, exactly as a real selector does after
    /// emitting its outcome — so the runtime's close decision is testable without a
    /// concrete selector.
    struct StubSelector {
        keys_seen: usize,
        pasted: Arc<Mutex<String>>,
        done: DoneSignal,
    }

    impl SelectorController for StubSelector {
        fn render_lines(&self, _width: u16, _palette: &ThemePalette) -> Vec<Line<'static>> {
            vec![Line::from("stub")]
        }

        fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
            self.keys_seen += 1;
            if matches!(key.key_id.as_deref(), Some("enter") | Some("escape")) {
                self.done.store(true, Ordering::SeqCst);
            }
            HandleOutcome::Consumed
        }

        fn handle_paste(&mut self, text: &str) -> HandleOutcome {
            // Accumulate the *whole* payload — the runtime must hand it over intact
            // (VAL-OVERLAY-027), never split into per-character events.
            self.pasted.lock().expect("pasted poisoned").push_str(text);
            HandleOutcome::Consumed
        }
    }

    fn test_requester() -> FrameRequester {
        let (requester, _handle) = FrameScheduler::spawn(|| Ok(()));
        requester
    }

    fn key(id: &str) -> RtKey {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        RtKey {
            key_id: Some(id.to_string()),
            raw: KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        }
    }

    fn mount_stub(overlay: &SharedOverlay, requester: &FrameRequester) -> DoneSignal {
        let (done, _) = mount_stub_with_paste(overlay, requester);
        done
    }

    fn mount_stub_with_paste(
        overlay: &SharedOverlay,
        requester: &FrameRequester,
    ) -> (DoneSignal, Arc<Mutex<String>>) {
        let done = new_done_signal();
        let pasted = Arc::new(Mutex::new(String::new()));
        let controller: Arc<Mutex<dyn SelectorController>> = Arc::new(Mutex::new(StubSelector {
            keys_seen: 0,
            pasted: pasted.clone(),
            done: done.clone(),
        }));
        mount(overlay, requester, controller, done.clone());
        (done, pasted)
    }

    #[tokio::test]
    async fn dispatch_with_no_overlay_reports_unhandled() {
        let overlay = new_shared_overlay();
        let requester = test_requester();
        assert!(
            !dispatch_key(&overlay, &requester, &key("a")),
            "an empty overlay must not own the key"
        );
    }

    #[tokio::test]
    async fn a_modal_overlay_owns_every_key_isolating_the_editor() {
        // VAL-OVERLAY-005: while a modal selector is open, even a plain printable
        // key is owned by the overlay and never falls through to the editor.
        let overlay = new_shared_overlay();
        let requester = test_requester();
        mount_stub(&overlay, &requester);

        assert!(
            dispatch_key(&overlay, &requester, &key("a")),
            "a modal overlay owns the key"
        );
        assert!(
            is_open(&overlay),
            "a non-terminal key keeps the dialog open"
        );
    }

    #[tokio::test]
    async fn a_finished_selector_closes_the_dialog() {
        // VAL-OVERLAY-003 / -008: Enter (or Esc) makes the selector raise its done
        // flag; the runtime unmounts it so the dialog closes and the base becomes
        // reachable — ghost-free because the whole viewport repaints crisp.
        let overlay = new_shared_overlay();
        let requester = test_requester();
        mount_stub(&overlay, &requester);
        assert!(is_open(&overlay), "mounted");

        assert!(dispatch_key(&overlay, &requester, &key("enter")));
        assert!(!is_open(&overlay), "a finished selector closes the dialog");
    }

    #[tokio::test]
    async fn reopen_after_close_starts_clean() {
        // VAL-OVERLAY-008: a fast close-then-reopen leaves no lingering overlay —
        // each mount replaces, each close clears, so the next open starts empty.
        let overlay = new_shared_overlay();
        let requester = test_requester();
        for _ in 0..3 {
            mount_stub(&overlay, &requester);
            assert!(is_open(&overlay), "exactly one overlay open");
            dispatch_key(&overlay, &requester, &key("escape"));
            assert!(!is_open(&overlay), "closed cleanly before reopen");
        }
    }

    #[tokio::test]
    async fn a_paste_with_no_overlay_reports_unhandled() {
        // No overlay open → the paste falls through to the editor (the caller pastes
        // it), so dispatch_paste reports it did not own the payload.
        let overlay = new_shared_overlay();
        let requester = test_requester();
        assert!(!dispatch_paste(&overlay, &requester, "sk-abc123"));
    }

    #[tokio::test]
    async fn a_paste_lands_in_the_mounted_selector_whole() {
        // VAL-OVERLAY-027: a multi-character paste reaches the mounted selector as a
        // single intact payload (never folded to one character), and the overlay owns
        // it so the editor beneath never sees it.
        let overlay = new_shared_overlay();
        let requester = test_requester();
        let (_done, pasted) = mount_stub_with_paste(&overlay, &requester);

        let key = "sk-ant-api03-THE-WHOLE-KEY-arrives-as-one-paste-event";
        assert!(
            dispatch_paste(&overlay, &requester, key),
            "an open overlay owns the paste"
        );
        assert_eq!(
            pasted.lock().unwrap().as_str(),
            key,
            "the selector received the entire payload intact"
        );
        assert!(is_open(&overlay), "a paste never closes the dialog");
    }

    #[tokio::test]
    async fn render_lines_reflects_the_mounted_selector() {
        let overlay = new_shared_overlay();
        let requester = test_requester();
        let palette = ThemePalette::default();
        assert!(
            overlay.render_lines(40, &palette).is_none(),
            "nothing mounted"
        );
        mount_stub(&overlay, &requester);
        let lines = overlay
            .render_lines(40, &palette)
            .expect("mounted selector renders");
        assert_eq!(lines.len(), 1);
    }
}
