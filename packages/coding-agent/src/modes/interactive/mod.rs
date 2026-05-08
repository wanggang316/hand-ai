//! Interactive TUI mode for the coding agent.
//!
//! Status: skeleton only — phase-1 components are ported; the main driver
//! (5500 lines in TS) is queued for later batches. See
//! `docs/exec-plans/parity-completion.md` §A1.
//!
//! # Theming
//!
//! pi-mono's interactive components consume a coding-agent–specific `theme`
//! object (semantic color slots like `userMessageBg`, `customMessageLabel`,
//! etc.). That theme system is *not* yet ported — it lives in
//! `modes/interactive/theme/` upstream and depends on JSON config loading +
//! the Markdown theme bridge.
//!
//! Until the theme port lands, the phase-1 components hardcode reasonable
//! ANSI defaults (chosen to match the spirit of pi-mono's dark theme) and
//! consume the existing [`hand_tui::theme`] primitives directly. Each
//! component documents the slots it would otherwise read so the eventual
//! theme port has a clear surface to wire into.

pub mod components;
pub mod theme;

// TODO(parity): port modes/interactive/interactive-mode.ts driver
// TODO(parity): port the remaining ~29 components per
//   docs/exec-plans/parity-completion.md §A1
