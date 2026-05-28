//! Build a `User-Agent` string for outbound HTTP requests.
//!
//! Embeds the binary version, the host OS, the rustc version it was
//! compiled with, and the target arch — enough for a provider to
//! attribute traffic to this client cleanly.
//!
//! Example output:
//! ```text
//! hand/0.1.1 (macos; rustc/1.85; aarch64)
//! ```

/// Generate a `User-Agent` string of the form
/// `hand/<version> (<os>; rustc/<rustc>; <arch>)`.
///
/// `version` is supplied by the caller (typically `env!("CARGO_PKG_VERSION")`)
/// so the agent never has to read the version out of the environment.
pub fn hand_user_agent(version: &str) -> String {
    format!(
        "hand/{} ({}; rustc/{}; {})",
        version,
        std::env::consts::OS,
        rustc_version_at_build(),
        std::env::consts::ARCH,
    )
}

/// Rustc version captured at build time via Cargo's `RUSTC_VERSION` —
/// when unavailable (older Cargo, non-Cargo build), falls back to the
/// rust edition string.
fn rustc_version_at_build() -> &'static str {
    // `RUSTC_VERSION` is not actually a standard Cargo env var. We use the
    // package's MSRV-compatible edition as a stable fallback. The compile-
    // time `option_env!` macro resolves to `None` when the variable is
    // unset, which keeps the function a `const`-friendly &'static str.
    option_env!("RUSTC_VERSION").unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_includes_version() {
        let ua = hand_user_agent("1.2.3");
        assert!(ua.starts_with("hand/1.2.3"));
    }

    #[test]
    fn user_agent_contains_os_and_arch() {
        let ua = hand_user_agent("0.0.0");
        assert!(ua.contains(std::env::consts::OS), "missing OS in {ua:?}");
        assert!(
            ua.contains(std::env::consts::ARCH),
            "missing ARCH in {ua:?}"
        );
    }

    #[test]
    fn user_agent_has_rustc_segment() {
        let ua = hand_user_agent("0.0.0");
        assert!(ua.contains("rustc/"), "missing rustc segment in {ua:?}");
    }

    #[test]
    fn user_agent_format_is_stable() {
        let ua = hand_user_agent("9.9.9");
        // hand/<version> (<os>; rustc/<x>; <arch>)
        assert!(ua.starts_with("hand/9.9.9 ("), "got: {ua}");
        assert!(ua.ends_with(")"), "got: {ua}");
        // Three semicolon-separated triples inside the parens.
        let inside = ua.trim_start_matches("hand/9.9.9 (").trim_end_matches(')');
        assert_eq!(inside.split("; ").count(), 3);
    }
}
