//! Input event pipeline for the ratatui runtime.
//!
//! Reads crossterm events off a **bounded-poll loop** on a blocking thread
//! ([`spawn_event_pump`]) and translates them into the unified
//! [`RtInputEvent`] type consumed by the runtime. Three translation
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
//!
//! # Why bounded polls (the resize deadlock)
//!
//! The pump deliberately does **not** drive crossterm's async `EventStream`.
//! The stream's background reader parks inside crossterm's process-global
//! event lock with *no timeout* until the next public event arrives. While it
//! is parked, a cursor-position query (`ESC[6n` — issued by ratatui's inline
//! resize path for `Terminal::clear` and its viewport-size recompute) cannot
//! take the lock; worse, the terminal's `ESC[..R` reply is consumed by the
//! parked reader and stranded in its skipped-event buffer until the next
//! keypress completes that poll cycle. Every query then stalls for its full
//! internal timeout and errors out — dragging the window (a SIGWINCH storm)
//! became a multi-second stall with the layout stuck at the old width and the
//! viewport re-anchored over the transcript. Bounded polls release the global
//! lock at least every [`POLL_INTERVAL`], and crossterm re-queues the stranded
//! reply as each cycle ends, so a cursor query resolves within a cycle or two.
//!
//! Shutdown: a thread on tokio's blocking pool ignores `JoinHandle::abort`, so
//! the pump watches a shared [`AtomicBool`] ([`EventPumpHandle::shutdown`])
//! and exits at the top of its next poll cycle.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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
    // typed event API parses only recognised events and silently drops this
    // window-op report before it reaches `translate_event` — there is no `Event`
    // variant that carries the raw bytes to feed
    // [`parse_cell_size_reply`](crate::rt::components::parse_cell_size_reply).
    // Folding a live reply into row scaling therefore needs a raw-byte input path
    // (or a crossterm passthrough hook), which is a rearchitecture of the input
    // pump rather than a local tweak, so it is deferred. Query + parse stay
    // unit-tested and the 8x16 default cell size is used until then.
}

/// Upper bound on how long one pump cycle may hold crossterm's global event
/// lock. Shorter means faster shutdown response and quicker delivery of a
/// stranded cursor-position reply; longer means fewer idle wakeups. 50ms is
/// far below perception for a resize repaint and negligible as idle load.
pub const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A pollable source of crossterm events — the seam that lets the pump loop
/// run against a scripted source in unit tests. The production implementation
/// is [`CrosstermEvents`].
pub trait EventSource {
    /// Wait up to `timeout` for an event to become readable.
    fn poll(&mut self, timeout: Duration) -> io::Result<bool>;

    /// Read the next event. Only called after [`poll`](Self::poll) returned
    /// `Ok(true)`.
    fn read(&mut self) -> io::Result<Event>;
}

/// The terminal-backed [`EventSource`] over crossterm's global event reader.
#[derive(Debug, Default, Clone, Copy)]
pub struct CrosstermEvents;

impl EventSource for CrosstermEvents {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
        crossterm::event::poll(timeout)
    }

    fn read(&mut self) -> io::Result<Event> {
        crossterm::event::read()
    }
}

/// Drive `source` until shutdown, translating each event and pushing the
/// runtime events onto `sink`.
///
/// Blocking — meant for a dedicated (blocking-pool) thread. Each cycle
/// bounded-polls for at most [`POLL_INTERVAL`], so crossterm's global event
/// lock is released regularly (see the module docs for why that bound is
/// load-bearing). Exits when `shutdown` is set (checked once per cycle), when
/// the receiver is dropped (send fails), or with the first poll/read error —
/// a closing PTY master surfaces here as a read error, which drops the sender
/// and closes the channel: the EOF exit signal consumers rely on.
/// Untranslatable events (filtered keys, mouse) are skipped silently. This is
/// input only: it does not schedule frames.
pub fn run_event_loop<S: EventSource>(
    source: &mut S,
    shutdown: &AtomicBool,
    sink: &mpsc::Sender<RtInputEvent>,
) -> io::Result<()> {
    while !shutdown.load(Ordering::Relaxed) {
        if !source.poll(POLL_INTERVAL)? {
            continue;
        }
        let event = source.read()?;
        if let Some(rt_event) = translate_event(event)
            && sink.blocking_send(rt_event).is_err()
        {
            // Receiver hung up: consumer is gone, stop pumping.
            break;
        }
    }
    Ok(())
}

/// Handle to a spawned event pump: signals shutdown and joins the pump thread.
///
/// A thread on tokio's blocking pool ignores `JoinHandle::abort`, so stopping
/// the pump is a cooperative flag flip: [`shutdown`](Self::shutdown) sets the
/// flag and the loop exits at the top of its next cycle, within
/// [`POLL_INTERVAL`].
#[derive(Debug)]
pub struct EventPumpHandle {
    shutdown: Arc<AtomicBool>,
    join: tokio::task::JoinHandle<io::Result<()>>,
}

impl EventPumpHandle {
    /// Signal the pump loop to stop. Returns immediately; the loop observes
    /// the flag within one [`POLL_INTERVAL`]. Safe to call more than once.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }

    /// Signal shutdown and wait for the pump thread to exit, surfacing the
    /// error its loop ended with, if any.
    pub async fn join(self) -> io::Result<()> {
        self.shutdown();
        self.join.await.map_err(io::Error::other)?
    }
}

/// Spawn the bounded-poll pump on tokio's blocking thread pool, returning the
/// receiving end of the event channel and the pump handle.
///
/// `capacity` bounds the channel; a full channel parks the pump thread until
/// the consumer drains it (interactive input never approaches that). Stop the
/// pump with [`EventPumpHandle::shutdown`]; the thread also exits on its own
/// when the receiver is dropped or the terminal input closes.
#[must_use]
pub fn spawn_event_pump(capacity: usize) -> (mpsc::Receiver<RtInputEvent>, EventPumpHandle) {
    let (tx, rx) = mpsc::channel(capacity);
    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&shutdown);
    let join =
        tokio::task::spawn_blocking(move || run_event_loop(&mut CrosstermEvents, &flag, &tx));
    (rx, EventPumpHandle { shutdown, join })
}
