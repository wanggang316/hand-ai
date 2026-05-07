//! Pure-function utility helpers shared across the coding agent.
//!
//! Modules under `utils` are intentionally leaf-level: they have no I/O,
//! no agent state, and depend only on `serde`/`thiserror` style primitives.
//! They are not re-exported through the prelude — call sites should import
//! the specific helper they need.

pub mod changelog;
pub mod child_process;
pub mod clipboard;
pub mod clipboard_image;
pub mod frontmatter;
pub mod mime;
pub mod paths;
pub mod pi_user_agent;
pub mod shell;
pub mod sleep;
pub mod version_check;
