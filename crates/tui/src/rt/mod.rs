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
//! - [`components`] — primitive display widgets (text, box, spacer, status bar,
//!   progress bar) painting into a ratatui `Buffer`

pub mod components;
pub mod events;
pub mod history;
pub mod overlay;
pub mod scheduler;
pub mod session;
pub mod view;

pub use components::{
    CodeHighlighter, MarkdownTheme, MarkdownView, ProgressBar, Spacer, StatusBar, TextBlock,
    TruncatedText, WidgetBox, plain_code_highlighter, render_markdown,
};
pub use events::{
    RtInputEvent, RtKey, key_event_to_key_id, run_event_loop, should_dispatch, spawn_event_pump,
    translate_event,
};
pub use history::{HistorySink, wrap_lines};
pub use overlay::{
    Overlay, OverlayAnchor, OverlayHandle, OverlayId, OverlayMargin, OverlayOptions, OverlayStack,
    anchor_rect,
};
pub use scheduler::{
    BSU, ESU, FrameClock, FrameDecision, FrameRequester, FrameScheduler, MAX_BURSTS_PER_SECOND,
    MIN_FRAME_INTERVAL, close_synchronized, draw_synchronized,
};
pub use view::{
    BORDER_ROWS, BottomGeometry, FocusView, HandleOutcome, LOADER_ROWS, MAX_INPUT_ROWS,
    MAX_VIEWPORT_ROWS, MIN_INPUT_ROWS, RtComponent, TerminalSize, bottom_area_geometry,
    clamp_input_rows,
};
