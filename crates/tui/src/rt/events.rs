//! Input event pipeline for the ratatui runtime.
//!
//! Reads crossterm events off an async [`EventStream`] and translates them
//! into the unified [`RtInputEvent`] type consumed by the runtime. Three
//! behaviours matter here and are pinned by unit tests:
//!
//! - **Release/repeat filtering.** Under the kitty keyboard protocol a single
//!   physical keypress reports `Press`, `Repeat`, and `Release` events. Only
//!   `Press` is dispatched as an action ([`should_dispatch`]) so a key never
//!   double-fires.
//! - **Canonical key ids.** [`key_event_to_key_id`] maps a structured
//!   crossterm [`KeyEvent`] to a canonical [`KeyId`] string (modifier order
//!   `shift, ctrl, alt, super`), so the keybindings registry keeps resolving
//!   chords like `"ctrl+shift+p"` unchanged.
//! - **Esc / alt-chord disambiguation.** crossterm already delivers the Alt
//!   modifier as a flag, so a lone Esc maps to `"escape"` immediately and an
//!   `Alt+<key>` chord maps to a single `"alt+<key>"` event — the legacy 50 ms
//!   ESC-flush heuristic is unnecessary and never splits a chord into
//!   `escape` + `key`.
//!
//! Bracketed paste arrives as one [`Event::Paste`] carrying the whole payload,
//! which becomes a single [`RtInputEvent::Paste`] rather than a burst of key
//! actions.
//!
//! Terminal focus changes pass through untouched: [`Event::FocusGained`] and
//! [`Event::FocusLost`] map one-to-one to [`RtInputEvent::FocusGained`] and
//! [`RtInputEvent::FocusLost`] so the runtime can react to the window losing or
//! regaining focus (e.g. pausing/resuming a blink) without a separate channel.

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::keys::KeyId;

/// A key press translated for the runtime.
///
/// Carries the canonical [`KeyId`] string (registry-compatible) plus the raw
/// crossterm [`KeyEvent`] for consumers that need the structured detail
/// (event kind, exact code, modifier state).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtKey {
    /// Canonical key identifier, e.g. `"ctrl+c"`, `"shift+ctrl+p"`, `"escape"`.
    ///
    /// `None` when the key has no canonical representation (e.g. a bare
    /// modifier key, or a media/lock key with no legacy equivalent).
    pub key_id: Option<KeyId>,
    /// The original crossterm key event, unmodified.
    pub raw: KeyEvent,
}

/// A unified runtime input event.
///
/// Deliberately distinct from the legacy `hand_tui::InputEvent`: the runtime
/// consumes structured crossterm events rather than raw byte strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtInputEvent {
    /// A dispatched key press (release/repeat already filtered out).
    Key(RtKey),
    /// A bracketed-paste payload, delivered whole.
    Paste(String),
    /// A terminal resize. Fields mirror the crossterm `(cols, rows)` order.
    Resize { cols: u16, rows: u16 },
    /// The terminal window gained focus.
    FocusGained,
    /// The terminal window lost focus.
    FocusLost,
}

/// Whether a key event of the given [`KeyEventKind`] should be dispatched as an
/// action.
///
/// Only `Press` dispatches. `Release` and `Repeat` are filtered so that, under
/// the kitty keyboard protocol's event-type reporting, one physical press
/// yields exactly one action.
#[must_use]
pub const fn should_dispatch(kind: KeyEventKind) -> bool {
    matches!(kind, KeyEventKind::Press)
}

/// Map a structured crossterm [`KeyEvent`] to the canonical [`KeyId`] string.
///
/// Emits the canonical id for the logical key, with modifiers in the order
/// `shift, ctrl, alt, super`. Returns `None` for keys with no canonical
/// representation (bare modifiers, lock/media keys, `KeyCode::Null`).
///
/// Alt is a plain modifier here: `Alt+a` is a single `"alt+a"` id, never an
/// `escape` followed by `a`. A lone Esc is `KeyCode::Esc` with no modifiers and
/// maps to `"escape"`.
#[must_use]
pub fn key_event_to_key_id(event: &KeyEvent) -> Option<KeyId> {
    let base = base_key_name(event.code)?;
    Some(format_key_id(&base, event.modifiers))
}

/// The base (unmodified) name of a key code. Returns `None` for codes with no
/// canonical legacy name.
///
/// A shifted ASCII letter that arrives as an uppercase `Char` is lowered to its
/// identity so the modifier prefix carries the shift, matching the legacy CSI-u
/// canonicalization (`"shift+p"`, not `"P"` — both resolve through the registry,
/// and the structured form is the one crossterm's flags make unambiguous).
fn base_key_name(code: KeyCode) -> Option<String> {
    Some(match code {
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::PageUp => "pageUp".to_string(),
        KeyCode::PageDown => "pageDown".to_string(),
        KeyCode::Tab => "tab".to_string(),
        // BackTab is Shift+Tab; the shift is folded into the modifier prefix by
        // the caller via `KeyModifiers`, so the base name is just "tab".
        KeyCode::BackTab => "tab".to_string(),
        KeyCode::Delete => "delete".to_string(),
        KeyCode::Insert => "insert".to_string(),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => c.to_ascii_lowercase().to_string(),
        KeyCode::Esc => "escape".to_string(),
        // Keys with no canonical legacy KeyId.
        KeyCode::Null
        | KeyCode::CapsLock
        | KeyCode::ScrollLock
        | KeyCode::NumLock
        | KeyCode::PrintScreen
        | KeyCode::Pause
        | KeyCode::Menu
        | KeyCode::KeypadBegin
        | KeyCode::Media(_)
        | KeyCode::Modifier(_) => return None,
    })
}

/// Prefix a base key name with its modifiers in the canonical legacy order:
/// `shift, ctrl, alt, super`. HYPER and META collapse into `super` to match the
/// legacy modifier vocabulary; NONE bits are ignored.
fn format_key_id(base: &str, modifiers: KeyModifiers) -> KeyId {
    let shift = modifiers.contains(KeyModifiers::SHIFT);
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let alt = modifiers.contains(KeyModifiers::ALT);
    let super_key = modifiers.contains(KeyModifiers::SUPER)
        || modifiers.contains(KeyModifiers::HYPER)
        || modifiers.contains(KeyModifiers::META);

    // Legacy suppresses the explicit `shift+` prefix for a printable base when
    // shift is the *only* modifier and the character already encodes the shift
    // (uppercase byte). crossterm normalizes letters to their pressed form; to
    // stay registry-compatible we keep the structured `shift+<lower>` form,
    // which `matches_key` accepts identically. So: always emit the prefix.
    let mut out = String::new();
    if shift {
        out.push_str("shift+");
    }
    if ctrl {
        out.push_str("ctrl+");
    }
    if alt {
        out.push_str("alt+");
    }
    if super_key {
        out.push_str("super+");
    }
    out.push_str(base);
    out
}

/// Translate a single crossterm [`Event`] into an [`RtInputEvent`].
///
/// Returns `None` for events that carry no runtime action: filtered key events
/// (release/repeat), mouse events, and any other unhandled variant. Paste is a
/// single event; resize maps `(cols, rows)`.
#[must_use]
pub fn translate_event(event: Event) -> Option<RtInputEvent> {
    match event {
        Event::Key(key) => {
            if !should_dispatch(key.kind) {
                return None;
            }
            let key_id = key_event_to_key_id(&key);
            Some(RtInputEvent::Key(RtKey { key_id, raw: key }))
        }
        Event::Paste(payload) => Some(RtInputEvent::Paste(payload)),
        Event::Resize(cols, rows) => Some(RtInputEvent::Resize { cols, rows }),
        Event::FocusGained => Some(RtInputEvent::FocusGained),
        Event::FocusLost => Some(RtInputEvent::FocusLost),
        Event::Mouse(_) => None,
    }
    // TODO(cell-size): the terminal's cell-size reply (`CSI 6 ; H ; W t`, the
    // answer to [`write_cell_size_query`]) does not surface here. crossterm's
    // typed `EventStream` parses only recognised events and silently drops this
    // window-op report before it reaches `translate_event` — there is no `Event`
    // variant that carries the raw bytes to feed
    // [`parse_cell_size_reply`](crate::rt::components::parse_cell_size_reply).
    // Folding a live reply into row scaling therefore needs a raw-byte input path
    // (or a crossterm passthrough hook), which is a rearchitecture of the input
    // pump rather than a local tweak, so it is deferred. Query + parse stay
    // unit-tested and the 8x16 default cell size is used until then.
}

/// Drive an [`EventStream`] to completion, translating each event and pushing
/// the runtime events onto `sink`.
///
/// Runs until the stream ends (EOF), the receiver is dropped (send fails), or a
/// read error surfaces. Untranslatable events (filtered keys, mouse) are
/// skipped silently. This is input only: it does not schedule frames.
pub async fn run_event_loop(
    mut events: EventStream,
    sink: mpsc::Sender<RtInputEvent>,
) -> std::io::Result<()> {
    while let Some(next) = events.next().await {
        let event = next?;
        if let Some(rt_event) = translate_event(event)
            && sink.send(rt_event).await.is_err()
        {
            // Receiver hung up: consumer is gone, stop pumping.
            break;
        }
    }
    Ok(())
}

/// Spawn [`run_event_loop`] on the current tokio runtime, returning the
/// receiving end of the event channel and the join handle.
///
/// Convenience for consumers (the demo) that just want a stream of
/// [`RtInputEvent`]s without owning the pump loop. `capacity` bounds the
/// channel.
#[must_use]
pub fn spawn_event_pump(
    capacity: usize,
) -> (
    mpsc::Receiver<RtInputEvent>,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let (tx, rx) = mpsc::channel(capacity);
    let handle = tokio::spawn(run_event_loop(EventStream::new(), tx));
    (rx, handle)
}
