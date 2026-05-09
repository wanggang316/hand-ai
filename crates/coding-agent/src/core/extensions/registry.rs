//! Tier 1 in-process extension registry.
//!
//! At construction, the host gathers a `Vec<Arc<dyn Extension>>` from compile-
//! time-registered Tier 1 implementations. This module is the registration
//! surface; concrete extensions live in `examples/extensions/<name>/` crates
//! and are wired in via cargo features in the binary's Cargo.toml.

use super::api::Extension;
use std::sync::Arc;

/// Collect all Tier 1 extensions compiled into this binary.
///
/// Stub for Phase 3.2: returns an empty list. Tasks T3.4 / T3.5 (fixture
/// extensions) will populate this as they land. If the list grows enough that
/// a hand-written enumeration becomes painful, refactor to use the `inventory`
/// crate (see ADR-001 implementation notes).
pub fn builtin_tier1_extensions() -> Vec<Arc<dyn Extension>> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_empty_in_v1() {
        let exts = builtin_tier1_extensions();
        assert!(exts.is_empty(), "no Tier 1 fixtures wired yet");
    }
}
