//! Utility modules used across the model crate.
//!
//! This module groups small, dependency-free helpers: streaming primitives,
//! diagnostics, JSON repair, Unicode sanitization, request validation, header
//! merging, hashing, UUID generation, and context-overflow detection.

pub mod diagnostics;
pub mod event_stream;
pub mod hash;
pub mod headers;
pub mod json_parse;
pub mod overflow;
pub mod sanitize_unicode;
pub mod uuid;
pub mod validation;

pub use diagnostics::{AssistantMessageDiagnostic, DiagnosticKind};
pub use event_stream::{EventStream, Provenance};
pub use hash::sha256_hex;
pub use headers::merge_headers;
pub use json_parse::{safe_parse_partial, try_parse_strict};
pub use overflow::is_context_overflow;
pub use sanitize_unicode::{sanitize, sanitize_bytes};
pub use uuid::uuid_v7;
pub use validation::{ValidationIssue, ValidationIssueKind, validate_context};
