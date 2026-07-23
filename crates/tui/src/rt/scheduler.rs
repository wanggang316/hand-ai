//! Frame scheduler (skeleton).
//!
//! Will coalesce redraw requests from concurrent tasks into rate-limited
//! `draw()` calls: producers request frames instead of drawing directly,
//! and the scheduler batches high-frequency updates (e.g. token streams)
//! into at most one draw per tick.
