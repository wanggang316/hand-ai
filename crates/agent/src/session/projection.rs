//! Projection of raw session entries into model messages.

use std::collections::HashMap;

use super::types::SessionEntry;
use model::Message;

/// Per-kind projection function: raw entry in, zero or more model
/// messages out.
pub type Projector = Box<dyn Fn(&SessionEntry) -> Vec<Message> + Send + Sync>;

/// Turn raw entries into model messages for context assembly. The
/// default projection maps kind `"message"` payloads
/// (`{.., "message": <model::Message>}`) through serde and skips every
/// other kind. Callers override or extend per-kind behavior via
/// [`ContextProjection::with_projector`].
pub struct ContextProjection {
    projectors: HashMap<String, Projector>,
}

impl ContextProjection {
    /// Projection with the default `"message"` projector registered.
    pub fn new() -> Self {
        let mut projectors: HashMap<String, Projector> = HashMap::new();
        projectors.insert("message".to_string(), Box::new(project_message_entry));
        Self { projectors }
    }

    /// Register (or override) the projector for `kind`.
    pub fn with_projector(mut self, kind: impl Into<String>, p: Projector) -> Self {
        self.projectors.insert(kind.into(), p);
        self
    }

    /// Project `entries` in order; kinds without a registered projector
    /// contribute nothing.
    pub fn project(&self, entries: &[SessionEntry]) -> Vec<Message> {
        entries
            .iter()
            .flat_map(|entry| {
                self.projectors
                    .get(&entry.kind)
                    .map(|projector| projector(entry))
                    .unwrap_or_default()
            })
            .collect()
    }
}

impl Default for ContextProjection {
    fn default() -> Self {
        Self::new()
    }
}

/// Default projector for kind `"message"`: deserialize
/// `payload["message"]` as a [`model::Message`]. Entries whose payload
/// lacks the field or does not deserialize are skipped rather than
/// erroring — projection is best-effort context assembly.
fn project_message_entry(entry: &SessionEntry) -> Vec<Message> {
    entry
        .payload
        .get("message")
        .and_then(|v| serde_json::from_value::<Message>(v.clone()).ok())
        .map(|message| vec![message])
        .unwrap_or_default()
}
