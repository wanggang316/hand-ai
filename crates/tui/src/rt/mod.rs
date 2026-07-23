//! Ratatui-based runtime (skeleton).
//!
//! New rendering runtime built on ratatui 0.30 + crossterm 0.29. It lives
//! side by side with the legacy differential renderer and is intentionally
//! not wired into any legacy module yet: legacy code keeps running unchanged
//! while this tree is filled in feature by feature.
//!
//! Planned layout:
//! - [`session`] — terminal session lifecycle (raw mode, viewport, restore)
//! - [`events`] — input pipeline mapping crossterm events to runtime events
//! - [`scheduler`] — frame scheduling: coalesced, rate-limited redraws
//! - [`history`] — finalized output inserted into native scrollback
//! - [`view`] — view composition for the inline viewport
//! - [`overlay`] — layered overlays/modals inside the viewport

pub mod events;
pub mod history;
pub mod overlay;
pub mod scheduler;
pub mod session;
pub mod view;

pub use events::{
    RtInputEvent, RtKey, key_event_to_key_id, run_event_loop, should_dispatch, spawn_event_pump,
    translate_event,
};
