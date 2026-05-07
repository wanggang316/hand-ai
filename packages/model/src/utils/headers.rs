//! Case-insensitive HTTP header merge.
//!
//! HTTP header names are case-insensitive. When a caller passes overrides
//! we want them to take precedence regardless of the casing the defaults
//! used. The merge keeps the override's casing for any header it provides
//! and otherwise preserves the default's casing.

use std::collections::HashMap;

/// Merge `default` and `override_h`. Comparison of header names is
/// case-insensitive; on conflict the override value wins and the override's
/// casing is used for the merged key.
pub fn merge_headers(
    default: &HashMap<String, String>,
    override_h: Option<&HashMap<String, String>>,
) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = default.clone();

    let Some(overrides) = override_h else {
        return out;
    };

    // Build a lower-cased index over the existing keys so we can detect
    // case-insensitive collisions and remove the original entry before
    // inserting the override under its preferred casing.
    let mut lower_to_existing: HashMap<String, String> = out
        .keys()
        .map(|k| (k.to_ascii_lowercase(), k.clone()))
        .collect();

    for (k, v) in overrides {
        let lower = k.to_ascii_lowercase();
        if let Some(existing_key) = lower_to_existing.remove(&lower) {
            out.remove(&existing_key);
        }
        out.insert(k.clone(), v.clone());
        lower_to_existing.insert(lower, k.clone());
    }

    out
}
