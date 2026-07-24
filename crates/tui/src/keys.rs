//! Canonical key identifiers for the ratatui runtime.
//!
//! [`KeyId`] is a canonical lowercase string of the form
//! `"<modifier>+...+<base>"`, e.g. `"ctrl+shift+p"`, `"alt+enter"`, `"f12"`,
//! `"a"`, `"/"`. Modifiers are emitted in the order `shift, ctrl, alt, super`.
//!
//! The rt input pipeline ([`crate::rt::events`]) produces these strings from
//! structured crossterm key events; downstream keybinding tables key off the
//! same canonical form.

/// Canonical key identifier. See module-level docs for the format.
pub type KeyId = String;
