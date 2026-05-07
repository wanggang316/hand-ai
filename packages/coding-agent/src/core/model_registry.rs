//! Aggregate model catalog across providers + extension-contributed providers.
//!
//! [`ModelRegistry`] is a per-session, read-only snapshot of the models
//! available for a [`model::Client`]. It sits next to [`crate::core::model_resolver`]:
//! the resolver maps a model *pattern* (`"sonnet"`, `"openai/gpt-4o:high"`) to
//! a single [`model::Model`]; the registry lists every known model so the RPC
//! `set_model` / `cycle_model` / `get_available_models` handlers can drive a UI
//! that browses the full catalog.
//!
//! ## Iteration order
//!
//! Models are sorted alphabetically by `(provider.as_str(), id)`. The order is
//! stable: rebuilding from the same client yields the same sequence. This makes
//! `cycle_model` produce a predictable rotation.
//!
//! ## Construction
//!
//! [`ModelRegistry::build`] is eager — it materializes the catalog once and
//! caches it on the registry. Callers (typically [`AgentSession`](crate::core::agent_session::AgentSession))
//! decide when to rebuild (e.g. on extension registration or config change);
//! the registry itself is immutable after construction.
//!
//! ## Extension-contributed providers (TODO, Phase 3.x)
//!
//! Extensions may eventually contribute additional providers / models. The
//! `build` signature already takes a [`model::Client`] reference so the hook
//! point is in place; for v1 we pull from the static catalog only and ignore
//! the client's per-session provider registry (which carries no extra
//! [`model::Model`] entries today). When extension-contributed models land,
//! merge them into `models` here and re-sort.

use model::Model;

/// Aggregate, sorted catalog of [`model::Model`]s available to a session.
///
/// See the module-level docs for the iteration-order contract and the rebuild
/// model.
#[derive(Debug, Clone)]
pub struct ModelRegistry {
    /// Sorted by `(provider.as_str(), id)`.
    models: Vec<Model>,
}

impl ModelRegistry {
    /// Build a registry from a [`model::Client`].
    ///
    /// Pulls models from the built-in static catalog
    /// ([`crate::core::model_resolver::list_models`]) and merges (in v1: just
    /// uses) the result. The `_client` parameter is currently unused but
    /// reserved for Phase 3.x extension-contributed providers — see the module
    /// docs.
    pub fn build(_client: &model::Client) -> Self {
        // TODO(phase 3.x): merge extension-contributed models from
        // `_client`'s registry once extensions can contribute new
        // `Model` entries. For now the static catalog is the full set.
        let mut models = crate::core::model_resolver::list_models(None);
        models.sort_by(|a, b| {
            (a.provider.as_str(), a.id.as_str()).cmp(&(b.provider.as_str(), b.id.as_str()))
        });
        Self { models }
    }

    /// All models in stable sorted order.
    pub fn all(&self) -> &[Model] {
        &self.models
    }

    /// Look up a model by `(provider, id)` exact match.
    pub fn find(&self, provider: &str, id: &str) -> Option<&Model> {
        self.models
            .iter()
            .find(|m| m.provider.as_str() == provider && m.id == id)
    }

    /// Find the next model after `current` in the iteration order.
    ///
    /// Wraps to the first model when at the end. Returns `None` if the
    /// registry is empty or `current` isn't present (matched by
    /// `(provider, id)`).
    pub fn next(&self, current: &Model) -> Option<&Model> {
        if self.models.is_empty() {
            return None;
        }
        let idx = self.models.iter().position(|m| {
            m.provider.as_str() == current.provider.as_str() && m.id == current.id
        })?;
        let next_idx = (idx + 1) % self.models.len();
        Some(&self.models[next_idx])
    }

    /// Number of models in the registry.
    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// Whether the registry has no models.
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_model(provider: model::types::Provider, id: &str) -> Model {
        Model {
            id: id.into(),
            name: id.into(),
            api: model::types::Api::AnthropicMessages,
            provider,
            base_url: String::new(),
            reasoning: false,
            input: vec![model::InputType::Text],
            cost: model::Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 200_000,
            max_tokens: 8192,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    #[test]
    fn build_from_default_client_returns_non_empty_registry() {
        let client = model::Client::new();
        let registry = ModelRegistry::build(&client);
        assert!(
            !registry.is_empty(),
            "registry built from default client must surface the static catalog"
        );
        assert!(!registry.is_empty());
    }

    #[test]
    fn build_is_stable_across_calls() {
        let client = model::Client::new();
        let a = ModelRegistry::build(&client);
        let b = ModelRegistry::build(&client);
        assert_eq!(a.len(), b.len());
        let a_keys: Vec<_> = a.all().iter().map(|m| (m.provider.as_str(), m.id.as_str())).collect();
        let b_keys: Vec<_> = b.all().iter().map(|m| (m.provider.as_str(), m.id.as_str())).collect();
        assert_eq!(a_keys, b_keys, "iteration order must be stable across rebuilds");
    }

    #[test]
    fn find_returns_known_model_by_provider_and_id() {
        let client = model::Client::new();
        let registry = ModelRegistry::build(&client);

        // Pick the first model out of the registry as a known target — using
        // the registry itself as the source of truth keeps this test robust
        // against catalog churn.
        let probe = registry.all().first().expect("registry non-empty").clone();
        let found = registry.find(probe.provider.as_str(), &probe.id);
        assert!(found.is_some(), "find must locate a model that exists in all()");
        let found = found.unwrap();
        assert_eq!(found.id, probe.id);
        assert_eq!(found.provider.as_str(), probe.provider.as_str());
        assert_eq!(found.name, probe.name);
    }

    #[test]
    fn find_missing_returns_none() {
        let client = model::Client::new();
        let registry = ModelRegistry::build(&client);
        assert!(registry.find("nonexistent", "nope").is_none());
    }

    #[test]
    fn next_cycles_through_all_models_and_wraps() {
        let client = model::Client::new();
        let registry = ModelRegistry::build(&client);
        assert!(registry.len() >= 2, "test requires at least 2 models in the static catalog");

        let first = &registry.all()[0];
        let second = &registry.all()[1];
        let next = registry.next(first).expect("next of first must be Some");
        assert_eq!(next.id, second.id);
        assert_eq!(next.provider.as_str(), second.provider.as_str());

        // Wrap-around: next of last is first.
        let last = registry.all().last().unwrap();
        let wrapped = registry.next(last).expect("next of last wraps");
        assert_eq!(wrapped.id, first.id);
        assert_eq!(wrapped.provider.as_str(), first.provider.as_str());
    }

    #[test]
    fn next_with_unknown_current_returns_none() {
        let client = model::Client::new();
        let registry = ModelRegistry::build(&client);
        // Use a synthetic model that cannot match anything in the catalog.
        let phantom = fake_model(model::types::Provider::Anthropic, "definitely-not-a-real-model-id");
        assert!(registry.next(&phantom).is_none());
    }
}
