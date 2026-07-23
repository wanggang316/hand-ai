//! Terminal session lifecycle (skeleton).
//!
//! Will own the ratatui `Terminal` and the crossterm terminal state around
//! it: entering/leaving raw mode, configuring the inline viewport, pushing
//! and popping keyboard enhancement flags, and restoring the terminal on
//! both normal exit and panic paths.
