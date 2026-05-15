//! Run-mode dispatch for the `hand` binary.
//!
//! Each submodule owns one entry point invoked by `main.rs` based on the
//! parsed [`crate::cli::Args`]. Today the only extracted mode is the
//! `print` submodule; the headless `--rpc` mode and the
//! interactive flow still live inline in
//! `main.rs` but share [`session_setup::SessionSetup`] for argument
//! resolution.

pub mod interactive;
pub mod print;
pub mod session_setup;
