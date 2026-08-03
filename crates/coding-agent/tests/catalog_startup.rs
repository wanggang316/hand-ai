//! Startup catalog priming: a locally cached catalog must be visible to
//! model resolution in the same launch, without any network.
//!
//! Lives in its own integration binary because it points `HOME` at a temp
//! directory and swaps the process-wide model catalog — mutations other
//! tests in a shared binary would not survive. Keep this file to a single
//! `#[test]` for the same reason.

use hand_coding_agent::core::auth_storage::AuthStorage;
use hand_coding_agent::core::model_registry::{ModelRegistry, prime_model_catalog};
use hand_coding_agent::core::model_resolver;

/// Marker model id present ONLY in the cache written below — never in the
/// embedded baseline — so seeing it proves the cached catalog was installed.
const MARKER_ID: &str = "hand-catalog-cache-marker";

/// Embedded baseline with a marker model grafted under `openai`. Cloning a
/// real entry keeps the payload schema-valid without hand-rolling every
/// `Model` field.
fn baseline_with_marker() -> String {
    let mut catalog: serde_json::Value =
        serde_json::from_str(model::models::MODELS_JSON).expect("baseline parses");
    let mut marker = {
        let openai = catalog["openai"].as_object().expect("baseline has openai");
        openai.values().next().expect("openai non-empty").clone()
    };
    marker["id"] = serde_json::Value::String(MARKER_ID.to_string());
    marker["name"] = serde_json::Value::String("Catalog Cache Marker".to_string());
    catalog["openai"][MARKER_ID] = marker;
    serde_json::to_string(&catalog).expect("catalog serializes")
}

#[test]
fn cached_catalog_is_visible_to_fresh_and_refreshed_registries_without_network() {
    let home = tempfile::tempdir().unwrap();
    // Point HOME at the temp dir so the cache loader reads it, and force
    // offline so priming can never reach for the network.
    // SAFETY: this is the only test in this binary, so no other thread
    // observes the mutation.
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("HAND_OFFLINE", "1");
    }

    // A registry snapshotted BEFORE the cache exists serves the baseline.
    // (`in_memory` + an explicit auth path keeps the test hermetic and avoids
    // the TLS cert-store cost `model::Client::new()` pays per provider.)
    let auth = || AuthStorage::at(home.path().join("auth.json"));
    let mut stale = ModelRegistry::in_memory(auth());
    assert!(
        stale.all().iter().all(|m| m.id != MARKER_ID),
        "marker must be absent from the embedded baseline"
    );

    // Drop the cache in place and prime — local IO only (offline is forced
    // above, so no background fetch is spawned).
    let cache_dir = home.path().join(".hand-ai");
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(cache_dir.join("models.json"), baseline_with_marker()).unwrap();
    assert!(prime_model_catalog(), "a valid cache must install");

    // A fresh registry — what every mode builds after main() primes — sees
    // the cached marker model...
    let fresh = ModelRegistry::in_memory(auth());
    assert!(
        fresh.all().iter().any(|m| m.id == MARKER_ID),
        "a registry built after priming must see the cached model"
    );

    // ...and so does direct model resolution.
    let resolved = model_resolver::resolve_model(Some("openai"), MARKER_ID);
    assert_eq!(
        resolved.model.id, MARKER_ID,
        "model resolution must resolve a cache-only model"
    );

    // A PRE-existing registry stays on its construction-time snapshot until
    // refreshed — the same synchronous reload `/model` performs on open.
    assert!(
        stale.all().iter().all(|m| m.id != MARKER_ID),
        "a pre-prime registry keeps its snapshot until refresh()"
    );
    stale.refresh();
    assert!(
        stale.all().iter().any(|m| m.id == MARKER_ID),
        "refresh() must pick up the hot-swapped catalog"
    );
}
