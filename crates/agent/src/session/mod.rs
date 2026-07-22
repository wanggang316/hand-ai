//! Persistent session storage for library consumers.
//!
//! Embedders that drive [`crate::Agent`] directly get durable session
//! logs without depending on the `hand` binary: [`SessionStore`] is
//! the storage contract, [`JsonlStore`] persists to a directory of
//! JSONL files, and [`InMemoryStore`] backs ephemeral sessions and
//! tests. The JSONL layout is the same format the `hand` binary
//! writes, so session files are interchangeable between the two.
//! With the `sqlite` cargo feature, `SqliteStore` adds a single-file
//! database backend that can import an existing JSONL directory.
//!
//! Entries are open-ended `{"type": <kind>, "data": <payload>}`
//! envelopes ([`SessionEntry`]); the store never enumerates kinds, so
//! embedders can persist their own entry types. [`ContextProjection`]
//! is the seam that turns raw entries back into [`model::Message`]s
//! for context assembly, with per-kind projectors.
//!
//! Stores mint no ids and read no clocks — callers supply session ids,
//! entry ids, and timestamps.

pub mod jsonl;
pub mod memory;
pub mod projection;
#[cfg(feature = "sqlite")]
pub mod sqlite;
pub mod store;
pub mod types;

pub use jsonl::JsonlStore;
pub use memory::InMemoryStore;
pub use projection::{ContextProjection, Projector};
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteStore;
pub use store::SessionStore;
pub use types::{
    SESSION_FORMAT_VERSION, SessionEntry, SessionHeader, SessionStoreError, SessionSummary,
};
