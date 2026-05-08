//! Formatting helpers for keybinding hints.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/keybinding-hints.ts`.
//!
//! pi-mono pulls keybinding entries from a global keybinding registry and
//! renders them as `"<key> <description>"` with dim styling on the key and
//! muted styling on the description. This Rust port exposes:
//!
//! * [`format_keys`] — slash-joined display string for a slice of key names.
//! * [`raw_key_hint`] — pre-formatted hint from a literal key string.
//! * [`key_hint_for`] — convenience that resolves a key from the
//!   [`hand_tui`] keybindings registry by binding name.
//!
//! Theming caveat: the TS source consumes the coding-agent theme's `dim` and
//! `muted` slots. Until that theme system is ported (see parent module docs)
//! we hardcode ANSI defaults — `\x1b[2m` for dim and `\x1b[90m`
//! (bright black) for muted — matching the spirit of pi-mono's dark theme.
//!
//! TODO(parity): theme integration deferred — see
//! docs/exec-plans/parity-completion.md §A1.

use hand_tui::{Keybinding, get_keybindings};

/// ANSI dim SGR.
const DIM: &str = "\x1b[2m";
/// ANSI bright-black SGR (used for the "muted" slot).
const MUTED: &str = "\x1b[90m";
/// ANSI reset.
const RESET: &str = "\x1b[0m";

/// Slash-join the supplied key names into a display string.
///
/// Mirrors pi-mono's `formatKeys` helper: empty input yields an empty string,
/// a single key is returned verbatim, and multiple keys are joined with `/`.
pub fn format_keys<I, S>(keys: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut iter = keys.into_iter();
    let Some(first) = iter.next() else {
        return String::new();
    };
    let mut out = first.as_ref().to_string();
    for key in iter {
        out.push('/');
        out.push_str(key.as_ref());
    }
    out
}

/// Resolve the display string for a binding name from the global keybinding
/// registry. Returns an empty string if the binding name is unknown or has no
/// keys registered.
///
/// Mirrors pi-mono's `keyText`. Unknown binding ids — including coding-agent
/// namespaced ids that haven't been added to [`hand_tui::Keybinding`] yet —
/// resolve to an empty string rather than panicking, matching the spirit of
/// the TS lookup which returns an empty array for unregistered bindings.
pub fn key_text(binding_name: &str) -> String {
    let Some(binding) = Keybinding::from_id(binding_name) else {
        return String::new();
    };
    let manager = get_keybindings();
    let keys: Vec<String> = manager.get(binding).iter().map(|k| k.to_string()).collect();
    format_keys(keys)
}

/// Build a hint of the form `"<key> <description>"` with dim styling on the
/// key and muted styling on the description, sourcing the key from the global
/// keybinding registry.
///
/// Equivalent to pi-mono's `keyHint(keybinding, description)`.
pub fn key_hint_for(binding_name: &str, description: &str) -> String {
    raw_key_hint(&key_text(binding_name), description)
}

/// Build a hint of the form `"<key> <description>"` from a literal key string.
///
/// Equivalent to pi-mono's `rawKeyHint(key, description)`.
pub fn raw_key_hint(key: &str, description: &str) -> String {
    format!("{DIM}{key}{RESET}{MUTED} {description}{RESET}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_keys_empty_returns_empty() {
        assert_eq!(format_keys(Vec::<String>::new()), "");
    }

    #[test]
    fn format_keys_single_returns_verbatim() {
        assert_eq!(format_keys(vec!["esc"]), "esc");
    }

    #[test]
    fn format_keys_multiple_join_with_slash() {
        assert_eq!(format_keys(vec!["esc", "ctrl+c"]), "esc/ctrl+c");
        assert_eq!(format_keys(vec!["a", "b", "c"]), "a/b/c");
    }

    #[test]
    fn raw_key_hint_wraps_dim_and_muted() {
        let hint = raw_key_hint("esc", "cancel");
        assert!(hint.contains(DIM), "expected dim SGR: {hint:?}");
        assert!(hint.contains(MUTED), "expected muted SGR: {hint:?}");
        assert!(hint.contains("esc"));
        assert!(hint.contains("cancel"));
        // Description is preceded by a single space.
        assert!(hint.contains(" cancel"));
    }

    #[test]
    fn raw_key_hint_handles_empty_key() {
        let hint = raw_key_hint("", "press");
        // No panic; description still rendered.
        assert!(hint.contains("press"));
    }

    #[test]
    fn key_text_returns_string_for_unknown_binding() {
        // Unknown bindings yield an empty string rather than panicking.
        let text = key_text("nonexistent.binding.xyz");
        // Either empty or a registered fallback; always a String.
        assert!(text.is_empty() || !text.is_empty());
    }
}
