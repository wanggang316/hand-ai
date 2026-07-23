//! Input event pipeline (skeleton).
//!
//! Will read crossterm events and translate them into runtime events:
//! filtering key release/repeat on enhanced-keyboard terminals, handling
//! paste and focus events, and forwarding resize notifications to the
//! scheduler.
