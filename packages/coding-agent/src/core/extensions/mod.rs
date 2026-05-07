//! Extensions runtime (Tier 1 trait + Tier 2 subprocess).
//!
//! See ADR-001 for the design.

pub mod api;
pub mod dispatch;
pub mod manifest;
pub mod registry;
pub mod subprocess;

pub use api::{
    Extension, ExtensionCapabilities, ExtensionContext, ExtensionError, ExtensionManifest,
    HookDecision, ManifestError, SlashCommandSpec, ToolCallEvent, ToolResultEvent,
};
pub use manifest::load_manifest;
pub use registry::builtin_tier1_extensions;
pub use subprocess::{SubprocessExtension, discover_subprocess_extensions};
