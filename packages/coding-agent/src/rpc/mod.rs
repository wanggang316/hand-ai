//! RPC mode wire protocol.
//!
//! This module owns the JSON-line protocol spoken by `hand --mode rpc`
//! (port of `pi-coding-agent --mode rpc`). It defines:
//!
//! - [`types`]: serde-compatible command, response, and event types that
//!   round-trip the camelCase JSONL wire format.
//!
//! The codec (line framing, stdin/stdout pumps) lives elsewhere; this
//! module is types-only.
//!
//! Phase 1 dispatches a small subset of commands; the full protocol
//! surface is defined here so handlers can be filled in incrementally
//! without changing the public types.

pub mod jsonl;
pub mod types;

pub use jsonl::{JsonlReadError, read_jsonl, write_jsonl};
pub use types::{
    RpcCommand, RpcExtensionUiRequest, RpcExtensionUiResponse, RpcResponse, RpcResponseBody,
    RpcResultEmpty, RpcResultWithData, RpcSessionState, RpcSlashCommand, ResponseTag,
};
