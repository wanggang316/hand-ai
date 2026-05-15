//! Path-resolution helpers shared by the read-side tools.
//!
//! The module addresses real-world path-input quirks:
//!
//! - macOS screenshot filenames use a U+202F NARROW NO-BREAK SPACE before
//!   `AM`/`PM`, but users typing the path use a regular space.
//! - macOS HFS+ / APFS stores filenames in NFD (decomposed) form. A user
//!   pasting an NFC-normalized string (e.g. from a JS string literal) will
//!   miss otherwise-valid files.
//! - Some macOS locales (notably French) use a curly quote U+2019 in
//!   default screenshot names like `Capture d’écran` instead of the ASCII
//!   apostrophe a user is likely to type.
//!
//! When the literal path resolves we return it untouched. Only on a miss
//! do we probe the four variants (AM/PM, NFD, curly, NFD+curly).
//!
//! Other normalizations:
//!
//! - All exotic Unicode space code points (U+00A0, U+2000..U+200A, U+202F,
//!   U+205F, U+3000) collapse to a regular space *before* tilde expansion.
//!   This matters because users often paste paths through chat clients
//!   that helpfully replace spaces with non-breaking variants.
//! - A leading `@` sigil is stripped because the CLI sometimes passes
//!   `@<path>` arguments through unmodified.

use std::fs;
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

const NARROW_NO_BREAK_SPACE: char = '\u{202F}';
const CURLY_RIGHT_SINGLE_QUOTE: char = '\u{2019}';

/// Replace exotic Unicode whitespace with a plain ASCII space.
fn normalize_unicode_spaces(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

/// Strip a leading `@` sigil if present (CLI pass-through convenience).
fn normalize_at_prefix(s: &str) -> &str {
    s.strip_prefix('@').unwrap_or(s)
}

/// macOS screenshot variant: replace ` AM.`/` PM.` (case-insensitive) with
/// `U+202F AM.`/`U+202F PM.`.
fn try_macos_screenshot_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Look for ` AM.` or ` PM.` (4 ASCII bytes).
        if i + 4 <= bytes.len() && bytes[i] == b' ' && bytes[i + 3] == b'.' {
            let m1 = bytes[i + 1];
            let m2 = bytes[i + 2];
            let is_am = (m1 == b'A' || m1 == b'a') && (m2 == b'M' || m2 == b'm');
            let is_pm = (m1 == b'P' || m1 == b'p') && (m2 == b'M' || m2 == b'm');
            if is_am || is_pm {
                out.push(NARROW_NO_BREAK_SPACE);
                out.push(m1 as char);
                out.push(m2 as char);
                out.push('.');
                i += 4;
                continue;
            }
        }
        // Copy a single UTF-8 character.
        let ch_start = i;
        let first = bytes[i];
        // ASCII (`first < 0x80`) is a 1-byte char. A lead byte in
        // `0x80..0xC0` is invalid (continuation byte in lead position);
        // copy a single byte to avoid panicking.
        let len = if first < 0xC0 {
            1
        } else if first < 0xE0 {
            2
        } else if first < 0xF0 {
            3
        } else {
            4
        };
        let end = (ch_start + len).min(bytes.len());
        // Safety: input is &str so it's valid UTF-8; this slice ends on a
        // char boundary for well-formed input.
        out.push_str(std::str::from_utf8(&bytes[ch_start..end]).unwrap_or(""));
        i = end;
    }
    out
}

/// NFD-normalize the entire path.
fn try_nfd_variant(s: &str) -> String {
    s.nfd().collect()
}

/// Replace ASCII apostrophes with the curly U+2019 used on French macOS.
fn try_curly_quote_variant(s: &str) -> String {
    s.replace('\'', &CURLY_RIGHT_SINGLE_QUOTE.to_string())
}

fn file_exists(path: &Path) -> bool {
    fs::metadata(path).is_ok()
}

/// Expand a leading `~` / `~/` to the user's home directory and strip the
/// optional `@` CLI sigil.
///
/// Mirrors TS `expandPath`. Returns the input unchanged (as a `PathBuf`) if
/// the home directory cannot be determined and the path needs expansion.
pub fn expand_path(path: &str) -> PathBuf {
    let normalized = normalize_unicode_spaces(normalize_at_prefix(path));
    if normalized == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = normalized.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
        return PathBuf::from(format!("~/{}", rest));
    }
    PathBuf::from(normalized)
}

/// Resolve a path relative to `cwd`, expanding `~` and stripping `@` first.
///
/// Absolute paths are returned untouched (after expansion).
pub fn resolve_to_cwd(path: &str, cwd: &Path) -> PathBuf {
    let expanded = expand_path(path);
    if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    }
}

/// Resolve a read-side path with macOS Unicode-variant probing.
///
/// Tries, in order:
///   1. The literal resolved path.
///   2. AM/PM narrow-no-break-space variant.
///   3. NFD-normalized variant.
///   4. Curly-apostrophe variant.
///   5. NFD + curly-apostrophe combined.
///
/// Returns the first variant that points at an existing file, or the
/// original resolved path if none match (the caller still gets a sensible
/// path to surface in error messages).
pub fn resolve_read_path(path: &str, cwd: &Path) -> PathBuf {
    let resolved = resolve_to_cwd(path, cwd);

    if file_exists(&resolved) {
        return resolved;
    }

    let resolved_str = resolved.to_string_lossy().into_owned();

    // 1. AM/PM variant.
    let am_pm = try_macos_screenshot_path(&resolved_str);
    if am_pm != resolved_str {
        let candidate = PathBuf::from(&am_pm);
        if file_exists(&candidate) {
            return candidate;
        }
    }

    // 2. NFD variant.
    let nfd = try_nfd_variant(&resolved_str);
    if nfd != resolved_str {
        let candidate = PathBuf::from(&nfd);
        if file_exists(&candidate) {
            return candidate;
        }
    }

    // 3. Curly quote variant (against the original).
    let curly = try_curly_quote_variant(&resolved_str);
    if curly != resolved_str {
        let candidate = PathBuf::from(&curly);
        if file_exists(&candidate) {
            return candidate;
        }
    }

    // 4. NFD + curly combined.
    let nfd_curly = try_curly_quote_variant(&nfd);
    if nfd_curly != resolved_str {
        let candidate = PathBuf::from(&nfd_curly);
        if file_exists(&candidate) {
            return candidate;
        }
    }

    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::TempDir;

    #[test]
    fn expand_path_handles_tilde_alone() {
        let result = expand_path("~");
        assert_eq!(result, dirs::home_dir().expect("home dir"));
    }

    #[test]
    fn expand_path_handles_tilde_slash() {
        let result = expand_path("~/foo/bar");
        let expected = dirs::home_dir().expect("home dir").join("foo/bar");
        assert_eq!(result, expected);
    }

    #[test]
    fn expand_path_strips_at_prefix() {
        let result = expand_path("@/abs/path");
        assert_eq!(result, PathBuf::from("/abs/path"));
    }

    #[test]
    fn expand_path_strips_at_then_expands_tilde() {
        let result = expand_path("@~/foo");
        let expected = dirs::home_dir().expect("home dir").join("foo");
        assert_eq!(result, expected);
    }

    #[test]
    fn expand_path_leaves_plain_relative() {
        let result = expand_path("foo/bar");
        assert_eq!(result, PathBuf::from("foo/bar"));
    }

    #[test]
    fn expand_path_normalizes_unicode_spaces() {
        // U+00A0 NO-BREAK SPACE between "foo" and "bar".
        let input = "foo\u{00A0}bar";
        let result = expand_path(input);
        assert_eq!(result, PathBuf::from("foo bar"));
    }

    #[test]
    fn resolve_to_cwd_joins_relative() {
        let cwd = PathBuf::from("/tmp/work");
        let result = resolve_to_cwd("file.txt", &cwd);
        assert_eq!(result, PathBuf::from("/tmp/work/file.txt"));
    }

    #[test]
    fn resolve_to_cwd_preserves_absolute() {
        let cwd = PathBuf::from("/tmp/work");
        let result = resolve_to_cwd("/abs/path.txt", &cwd);
        assert_eq!(result, PathBuf::from("/abs/path.txt"));
    }

    #[test]
    fn resolve_read_path_returns_literal_when_present() {
        let dir = TempDir::new().unwrap();
        File::create(dir.path().join("a.txt")).unwrap();
        let result = resolve_read_path("a.txt", dir.path());
        assert_eq!(result, dir.path().join("a.txt"));
    }

    #[test]
    fn resolve_read_path_probes_am_pm_variant() {
        let dir = TempDir::new().unwrap();
        // Real file uses the narrow no-break space; user input uses a normal space.
        let real = "Screenshot 2024-01-01 at 10.00.00\u{202F}AM.png".to_string();
        File::create(dir.path().join(&real)).unwrap();

        let typed = "Screenshot 2024-01-01 at 10.00.00 AM.png";
        let result = resolve_read_path(typed, dir.path());
        assert_eq!(result, dir.path().join(&real));
    }

    #[test]
    fn resolve_read_path_probes_nfd_plus_curly() {
        let dir = TempDir::new().unwrap();
        // Build the real filename in NFD with a curly apostrophe — mimics
        // the French "Capture d'écran" macOS default. On case-preserving
        // Unicode-NFD-storing filesystems (HFS+ / APFS) the on-disk name
        // may be either the literal NFD bytes or the NFC equivalent
        // depending on volume settings; the resolver only needs to land
        // on a path that resolves to the same inode.
        let real_nfd_curly: String = "Capture d\u{2019}e\u{0301}cran.png".to_string();
        File::create(dir.path().join(&real_nfd_curly)).unwrap();

        // Find the actual on-disk path (may have been renormalised).
        let real_on_disk: PathBuf = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .next()
            .expect("file created");

        // User types NFC composed form with an ASCII apostrophe.
        let typed = "Capture d'\u{00E9}cran.png";
        let result = resolve_read_path(typed, dir.path());

        // The resolver should have probed at least one variant that
        // points at an existing file.
        assert!(
            std::fs::metadata(&result).is_ok(),
            "resolver must return an existing path, got {:?}",
            result
        );
        // canonicalize() collapses both byte-equivalent strings to the
        // OS's storage form.
        let resolved_canon = std::fs::canonicalize(&result).expect("canon resolved");
        let real_canon = std::fs::canonicalize(&real_on_disk).expect("canon real");
        assert_eq!(
            resolved_canon, real_canon,
            "resolver should land on the same file as the real on-disk entry"
        );
    }

    /// Lowercase `am`/`pm` (en_AU and similar locales) must also probe
    /// the narrow-no-break-space variant. The matcher already
    /// case-insensitively checks A/a + M/m; this test pins that
    /// surface so a refactor can't quietly tighten it to uppercase
    /// only.
    #[test]
    fn resolve_read_path_probes_lowercase_am_pm_variant() {
        let dir = TempDir::new().unwrap();
        // Real file uses lowercase `am` with the narrow no-break space —
        // mirrors what macOS produces under the en_AU locale.
        let real = "screenshot 10.00.00\u{202F}am.png".to_string();
        File::create(dir.path().join(&real)).unwrap();

        let typed = "screenshot 10.00.00 am.png";
        let result = resolve_read_path(typed, dir.path());
        assert_eq!(result, dir.path().join(&real));
    }

    /// Standalone curly-quote variant (no NFD needed). A filename
    /// that uses U+2019 RIGHT SINGLE QUOTATION MARK on disk must
    /// resolve from a typed ASCII apostrophe. Different from the
    /// NFD+curly French screenshot case, which combines two
    /// normalisations.
    #[test]
    fn resolve_read_path_probes_curly_quote_alone() {
        let dir = TempDir::new().unwrap();
        let real = "it\u{2019}s mine.txt"; // U+2019 curly apostrophe
        File::create(dir.path().join(real)).unwrap();

        let typed = "it's mine.txt"; // U+0027 ASCII straight apostrophe
        let result = resolve_read_path(typed, dir.path());
        assert!(
            std::fs::metadata(&result).is_ok(),
            "curly-quote-only variant must resolve, got {:?}",
            result
        );
    }

    /// NFC vs NFD probing must work even without a curly-quote
    /// complication. macOS HFS+/APFS may store filenames in NFD
    /// (decomposed) form; a user typing an NFC string (e.g. from a
    /// chat client) must still find the file.
    #[test]
    fn resolve_read_path_probes_nfd_alone() {
        let dir = TempDir::new().unwrap();
        // Real file stored as NFD: "é" decomposed to e + U+0301.
        let real = "caf\u{0065}\u{0301}.txt";
        File::create(dir.path().join(real)).unwrap();

        // Find what landed on disk (FS may renormalise).
        let on_disk: PathBuf = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .next()
            .expect("file created");

        // User types NFC composed form.
        let typed = "caf\u{00E9}.txt";
        let result = resolve_read_path(typed, dir.path());
        assert!(
            std::fs::metadata(&result).is_ok(),
            "NFD-only variant must resolve, got {:?}",
            result
        );
        let r = std::fs::canonicalize(&result).expect("canon resolved");
        let d = std::fs::canonicalize(&on_disk).expect("canon disk");
        assert_eq!(r, d, "resolver lands on same file");
    }

    #[test]
    fn resolve_read_path_returns_resolved_when_no_variant_matches() {
        let dir = TempDir::new().unwrap();
        let result = resolve_read_path("does-not-exist.txt", dir.path());
        assert_eq!(result, dir.path().join("does-not-exist.txt"));
    }
}
