//! CHANGELOG.md section parser.
//!
//! Mirrors `upstream coding-agent`'s `changelog.ts`: scans `## [x.y.z] ...`
//! headings and groups subsequent lines into per-version entries until the
//! next `##` heading or EOF. Lines that come before any version heading are
//! ignored; lines that follow a non-versioned `##` heading start a new
//! "skip" zone until another versioned heading is found.
//!
//! The parser deliberately *does not* validate semver — anything matching
//! `## [?]MAJOR.MINOR.PATCH[?]` (digits with two dots) opens a new entry.

use std::cmp::Ordering;
use std::path::Path;

use thiserror::Error;

/// One entry in a CHANGELOG, with the version triple and the raw markdown
/// content (including the heading line itself, with surrounding whitespace
/// trimmed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogEntry {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    /// Body of the entry, including its `##` heading. Leading and trailing
    /// whitespace is trimmed.
    pub content: String,
}

/// Errors raised while loading a changelog file.
#[derive(Debug, Error)]
pub enum ChangelogError {
    /// I/O error reading the file. `parse_changelog_file` swallows
    /// not-found into an empty `Vec`; this variant covers everything else.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Read and parse a CHANGELOG.md file. Returns an empty vector when the
/// file does not exist (mirroring the TS contract).
pub fn parse_changelog_file(path: impl AsRef<Path>) -> Result<Vec<ChangelogEntry>, ChangelogError> {
    let path = path.as_ref();
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(parse_changelog(&contents)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

/// Parse already-loaded changelog text. Pure function, no I/O.
pub fn parse_changelog(content: &str) -> Vec<ChangelogEntry> {
    let mut entries: Vec<ChangelogEntry> = Vec::new();
    let mut current_lines: Vec<&str> = Vec::new();
    let mut current_version: Option<(u32, u32, u32)> = None;

    let flush =
        |entries: &mut Vec<ChangelogEntry>, version: Option<(u32, u32, u32)>, lines: &[&str]| {
            if let Some((major, minor, patch)) = version
                && !lines.is_empty()
            {
                let content = lines.join("\n").trim().to_string();
                entries.push(ChangelogEntry {
                    major,
                    minor,
                    patch,
                    content,
                });
            }
        };

    for line in content.split('\n') {
        if line.starts_with("## ") {
            flush(&mut entries, current_version, &current_lines);

            if let Some(version) = parse_version_heading(line) {
                current_version = Some(version);
                current_lines = vec![line];
            } else {
                // A `##` heading that isn't a version — drop into "skip"
                // mode until the next versioned heading.
                current_version = None;
                current_lines.clear();
            }
        } else if current_version.is_some() {
            current_lines.push(line);
        }
    }

    flush(&mut entries, current_version, &current_lines);
    entries
}

/// Compare two changelog entries by their version triple.
pub fn compare_versions(left: &ChangelogEntry, right: &ChangelogEntry) -> Ordering {
    left.major
        .cmp(&right.major)
        .then(left.minor.cmp(&right.minor))
        .then(left.patch.cmp(&right.patch))
}

/// Filter `entries` down to those strictly newer than `last_version`.
///
/// `last_version` is parsed loosely — missing minor/patch components default
/// to 0, mirroring the TS `lastVersion.split('.').map(Number)` behavior.
pub fn get_new_entries(entries: &[ChangelogEntry], last_version: &str) -> Vec<ChangelogEntry> {
    let parts: Vec<u32> = last_version
        .split('.')
        .map(|p| p.parse::<u32>().unwrap_or(0))
        .collect();
    let last = ChangelogEntry {
        major: parts.first().copied().unwrap_or(0),
        minor: parts.get(1).copied().unwrap_or(0),
        patch: parts.get(2).copied().unwrap_or(0),
        content: String::new(),
    };
    entries
        .iter()
        .filter(|e| compare_versions(e, &last) == Ordering::Greater)
        .cloned()
        .collect()
}

/// Match a `##` line and pull out the first three dotted-number tokens,
/// optionally bracketed: `## [1.2.3] - 2024-...` or `## 1.2.3 - 2024-...`.
fn parse_version_heading(line: &str) -> Option<(u32, u32, u32)> {
    // Skip the leading `## ` (3 bytes; ASCII).
    let rest = line.strip_prefix("## ")?.trim_start();
    // Permit a leading `[`.
    let rest = rest.strip_prefix('[').unwrap_or(rest);

    // Pull the first three numeric tokens separated by `.`.
    let mut iter = rest.splitn(3, '.');
    let major = parse_numeric_prefix(iter.next()?)?;
    let minor = parse_numeric_prefix(iter.next()?)?;
    let tail = iter.next()?;
    let patch = parse_numeric_prefix(tail)?;
    Some((major, minor, patch))
}

/// Parse the leading run of digits in `s` as a `u32`. Returns `None` when
/// `s` doesn't start with at least one digit.
fn parse_numeric_prefix(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn entry(major: u32, minor: u32, patch: u32, body: &str) -> ChangelogEntry {
        ChangelogEntry {
            major,
            minor,
            patch,
            content: body.to_string(),
        }
    }

    /// Regression: the shipped `CHANGELOG.md` at the repo root must
    /// remain parseable. `apply_changelog` and the M5.4 startup
    /// banner both read it; a header that drifts away from `## [x.y.z]`
    /// would silently produce 0 entries and a "(no changelog entries
    /// found)" banner — caught here instead of at runtime.
    #[test]
    fn shipped_changelog_at_repo_root_parses_non_empty() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let repo_root = std::path::Path::new(manifest_dir)
            .ancestors()
            .nth(2)
            .expect("crates/coding-agent has two ancestors above");
        let changelog = repo_root.join("CHANGELOG.md");
        if !changelog.is_file() {
            // Workspace without a CHANGELOG.md is allowed (e.g. a
            // bare check-out of just the model crate). The test
            // exists to guard the file when it IS present.
            return;
        }
        let entries = parse_changelog_file(&changelog).expect("CHANGELOG.md is readable");
        assert!(
            !entries.is_empty(),
            "CHANGELOG.md at {} produced zero parsed entries — \
             check that headers match `## [x.y.z] - ...`",
            changelog.display()
        );
        // Sanity-check that the newest entry starts with `## [`.
        assert!(
            entries[0].content.starts_with("## ["),
            "first entry content lost its bracketed-version header: {:?}",
            entries[0].content.lines().next()
        );
    }

    #[test]
    fn parses_basic_changelog() {
        let input = "\
# Changelog

## [1.0.0] - 2024-01-01

- Initial release.

## [0.9.0] - 2023-12-01

- Beta features.
";
        let entries = parse_changelog(input);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].major, 1);
        assert_eq!(entries[0].minor, 0);
        assert_eq!(entries[0].patch, 0);
        assert!(entries[0].content.starts_with("## [1.0.0]"));
        assert!(entries[0].content.contains("Initial release."));
        assert_eq!(
            (entries[1].major, entries[1].minor, entries[1].patch),
            (0, 9, 0)
        );
    }

    #[test]
    fn unbracketed_version_headings_are_recognized() {
        let input = "## 2.1.3 - notes\n- entry\n";
        let entries = parse_changelog(input);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            (entries[0].major, entries[0].minor, entries[0].patch),
            (2, 1, 3)
        );
    }

    #[test]
    fn non_version_h2_skips_until_next_version() {
        let input = "\
## Notes

This section should be ignored.

## [1.0.0]

- entry
";
        let entries = parse_changelog(input);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content.split('\n').next(), Some("## [1.0.0]"));
    }

    #[test]
    fn empty_input_returns_empty_vec() {
        assert!(parse_changelog("").is_empty());
    }

    #[test]
    fn lines_before_first_version_are_dropped() {
        let input = "Some prose\nMore prose\n## [0.1.0]\n- entry";
        let entries = parse_changelog(input);
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].content.contains("Some prose"));
    }

    #[test]
    fn compare_versions_orders_correctly() {
        let a = entry(1, 0, 0, "");
        let b = entry(1, 1, 0, "");
        let c = entry(2, 0, 0, "");
        assert_eq!(compare_versions(&a, &b), Ordering::Less);
        assert_eq!(compare_versions(&c, &b), Ordering::Greater);
        assert_eq!(compare_versions(&a, &a), Ordering::Equal);
    }

    #[test]
    fn get_new_entries_filters_strictly_greater() {
        let entries = vec![
            entry(2, 0, 0, "two"),
            entry(1, 5, 0, "one-five"),
            entry(1, 0, 0, "one"),
            entry(0, 9, 0, "zero-nine"),
        ];
        let newer = get_new_entries(&entries, "1.0.0");
        assert_eq!(newer.len(), 2);
        assert!(newer.iter().any(|e| e.content == "two"));
        assert!(newer.iter().any(|e| e.content == "one-five"));
    }

    #[test]
    fn get_new_entries_handles_partial_version() {
        let entries = vec![entry(1, 0, 0, "one"), entry(0, 5, 0, "half")];
        // "1" parses as 1.0.0 — strictly greater means nothing.
        let newer = get_new_entries(&entries, "1");
        assert!(newer.is_empty());
        // "0" parses as 0.0.0 — both entries are newer.
        let newer = get_new_entries(&entries, "0");
        assert_eq!(newer.len(), 2);
    }

    #[test]
    fn get_new_entries_ignores_non_numeric_components() {
        let entries = vec![entry(1, 0, 0, "x")];
        // "abc" parses as 0.0.0 fallback.
        let newer = get_new_entries(&entries, "abc");
        assert_eq!(newer.len(), 1);
    }

    #[test]
    fn parse_changelog_file_missing_returns_empty() {
        let result = parse_changelog_file("/this/path/does/not/exist/CHANGELOG.md").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_changelog_file_reads_disk() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "## [3.2.1]\n- something").unwrap();
        f.flush().unwrap();
        let entries = parse_changelog_file(f.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            (entries[0].major, entries[0].minor, entries[0].patch),
            (3, 2, 1)
        );
    }

    #[test]
    fn malformed_h2_with_partial_version_is_ignored() {
        // `## 1.2` has only two dotted numbers — not a version heading.
        let input = "## 1.2\n- ignored\n## [1.2.3]\n- kept";
        let entries = parse_changelog(input);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            (entries[0].major, entries[0].minor, entries[0].patch),
            (1, 2, 3)
        );
    }

    #[test]
    fn version_heading_strips_trailing_whitespace_from_content() {
        let input = "## [1.0.0]\n\n- entry\n\n";
        let entries = parse_changelog(input);
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].content.ends_with('\n'));
    }
}
