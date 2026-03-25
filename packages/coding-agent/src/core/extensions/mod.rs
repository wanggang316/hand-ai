//! Extension system for lifecycle events and custom tools.
//!
//! Extensions can subscribe to agent events, register tools, commands,
//! keyboard shortcuts, and CLI flags. The system supports auto-discovery
//! from standard locations and explicit path configuration.

pub mod loader;
pub mod runner;
pub mod types;
pub mod wrapper;

// Re-export primary types for convenience.
pub use loader::{LoadExtensionsResult, discover_extensions, load_extension, load_extensions};
pub use runner::{ExtensionRunner, ResolvedCommand};
pub use types::{
    DiscoveryReason, Extension, ExtensionConfig, ExtensionError, ExtensionFlag, ExtensionFlagType,
    ExtensionHookContext, ExtensionHookResult, ExtensionHookType, ExtensionManifest,
    ExtensionShortcut, RegisteredCommand, RegisteredTool, ResourcesDiscoverResult, WidgetPlacement,
};
pub use wrapper::{WrappedTool, wrap_registered_tool, wrap_registered_tools};
