//! Startup chrome and terminal-integration markers for the rt interactive driver.
//!
//! This module owns everything that frames a session on the rt stack but is not
//! chat content itself:
//!
//! - the **welcome header** — a `hand v<version> <provider>/<model>` title plus a
//!   one-line key-hint, committed to the top of scrollback at startup;
//! - the **tmux keyboard warning** — a yellow notice when running inside a tmux
//!   whose extended-keys plumbing eats Modified Enter / Alt keys;
//! - the **changelog / update banner** three-state decision — display, silently
//!   record, or skip, keyed off the recorded `last_changelog_version`;
//! - the **OSC 133** shell-integration prompt marks (`A`/`B`/`C`) and the
//!   **OSC 9;4** progress indicator, both raw control sequences that cannot ride a
//!   ratatui `Buffer` cell and so are emitted through the driver's raw-write seam
//!   on the terminal-owning task (mirroring the M2 image / OSC 8 mechanism).
//!
//! Everything here that decides *what* to show is a pure function so the
//! version-compare, banner three-state, and OSC-generation logic are unit-tested
//! without a live terminal, a `SettingsManager`, or the network. The driver wires
//! the pure decisions into scrollback commits and raw writes.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::modes::interactive::theme::ThemePalette;

/// The product version, baked in at compile time.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

// ---------------------------------------------------------------------------
// Welcome header
// ---------------------------------------------------------------------------

/// A single key-hint pairing a keystroke glyph with the action it performs. The
/// hint row is built from these so the advertised keys and their descriptions
/// live in one honest list (VAL-CHAT-007: every key the hint advertises must map
/// to a real action).
struct KeyHint {
    key: &'static str,
    action: &'static str,
}

/// The key hints the welcome row advertises, in display order.
///
/// Honesty contract (VAL-CHAT-007): each entry names a key the interactive driver
/// actually binds. `↵`/`⇧↵` and `↑`/`↓` are handled by the M2 editor; `^D` quits
/// unconditionally in the input loop; `/` and `!` open the command / bash affordances
/// the editor recognises. `^C` (interrupt an in-flight turn) is delivered by the
/// turn-control feature later in M3 — it is advertised here because it is a
/// permanent part of the chrome and the key is already intercepted by the input
/// loop; its full cancel behaviour lands with that feature.
const KEY_HINTS: &[KeyHint] = &[
    KeyHint {
        key: "↵",
        action: "send",
    },
    KeyHint {
        key: "⇧↵",
        action: "newline",
    },
    KeyHint {
        key: "↑↓",
        action: "history",
    },
    KeyHint {
        key: "/",
        action: "commands",
    },
    KeyHint {
        key: "!",
        action: "bash",
    },
    KeyHint {
        key: "^C",
        action: "interrupt",
    },
    KeyHint {
        key: "^D",
        action: "quit",
    },
];

/// Style for the bold product name in the title, coloured from the palette's
/// accent (the default palette keeps the historical cyan).
fn title_name_style(palette: &ThemePalette) -> Style {
    Style::default()
        .fg(palette.accent)
        .add_modifier(Modifier::BOLD)
}

/// Style for the dim version / model segment in the title.
fn title_dim_style() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// Style for a hint's key glyph (dim) and separator.
fn hint_key_style() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// Style for a hint's action description, coloured from the palette's dim
/// slot (the default palette keeps the historical dark grey).
fn hint_action_style(palette: &ThemePalette) -> Style {
    Style::default().fg(palette.dim)
}

/// Build the welcome-header scrollback lines: a title line
/// (`hand v<version> <provider>/<model>`) and a trailing blank so the header
/// does not crowd the first chat entry.
///
/// The key-hint row is no longer part of the header — it renders persistently in
/// the bottom chrome directly below the input box (see [`key_hint_line`]), so the
/// gestures sit with the box they describe instead of being stranded at the top
/// of scrollback where they scroll away.
///
/// Pure over `(provider, model, version)` so the exact rendered text is asserted
/// in a unit test without a live session.
#[must_use]
pub fn welcome_header_lines(
    provider: &str,
    model: &str,
    version: &str,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let title = Line::from(vec![
        Span::styled("hand".to_string(), title_name_style(palette)),
        Span::styled(format!(" v{version}"), title_dim_style()),
        Span::styled(format!("   {provider}/{model}"), title_dim_style()),
    ]);

    vec![title, Line::default()]
}

/// Build the persistent key-hint line, drawn in the bottom chrome directly below
/// the input box: every advertised key glyph paired with its action, ` · `
/// separated (`↵ send · ⇧↵ newline · …`). The dim glyphs and palette-coloured
/// actions match the look the welcome header used to carry.
///
/// Kept next to the input (not in the welcome header) so the gestures stay
/// on-screen with the box they describe. The advertised set is [`KEY_HINTS`],
/// the same honest list [`advertised_hint_keys`] cross-checks against the real
/// bindings (VAL-CHAT-007). Pure so the exact text is asserted without a live
/// session.
#[must_use]
pub fn key_hint_line(palette: &ThemePalette) -> Line<'static> {
    let mut hint_spans: Vec<Span<'static>> = Vec::new();
    for (i, hint) in KEY_HINTS.iter().enumerate() {
        if i > 0 {
            hint_spans.push(Span::styled(" · ".to_string(), hint_action_style(palette)));
        }
        hint_spans.push(Span::styled(hint.key.to_string(), hint_key_style()));
        hint_spans.push(Span::styled(
            format!(" {}", hint.action),
            hint_action_style(palette),
        ));
    }
    Line::from(hint_spans)
}

/// The set of keys the welcome hint advertises, as canonical lower-case ids. The
/// honesty test cross-checks this against the keys the driver actually acts on so
/// the advertised chrome never drifts from the real bindings.
#[must_use]
pub fn advertised_hint_keys() -> Vec<&'static str> {
    KEY_HINTS.iter().map(|h| h.key).collect()
}

// ---------------------------------------------------------------------------
// tmux keyboard warning
// ---------------------------------------------------------------------------

/// The warning text for a tmux `extended-keys` misconfiguration.
pub const TMUX_EXTENDED_KEYS_OFF: &str = "tmux extended-keys is off. Modified Enter keys may not work. \
     Add `set -g extended-keys on` to ~/.tmux.conf and reload tmux.";

/// The warning text for a tmux `extended-keys-format` misconfiguration.
pub const TMUX_EXTENDED_KEYS_FORMAT_XTERM: &str = "tmux extended-keys-format is xterm. hand-ai works best with csi-u. \
     Add `set -g extended-keys-format csi-u` to ~/.tmux.conf and reload tmux.";

/// Decide whether a tmux keyboard warning applies, given the resolved
/// `extended-keys` and `extended-keys-format` option values (as tmux reports
/// them). `None` means the plumbing is correct or we are not inside tmux.
///
/// Pure over the two option strings so the decision is unit-tested without a live
/// tmux server. `extended_keys` is the raw value of `tmux show -gv extended-keys`;
/// `extended_keys_format` is the raw value of `extended-keys-format` (or `None`
/// when the option is unset).
#[must_use]
pub fn tmux_warning(
    extended_keys: &str,
    extended_keys_format: Option<&str>,
) -> Option<&'static str> {
    if extended_keys != "on" && extended_keys != "always" {
        return Some(TMUX_EXTENDED_KEYS_OFF);
    }
    if extended_keys_format == Some("xterm") {
        return Some(TMUX_EXTENDED_KEYS_FORMAT_XTERM);
    }
    None
}

/// Probe the running tmux for a keyboard-configuration warning, or `None` when
/// not inside tmux / correctly configured.
///
/// Shells out to `tmux show -gv <opt>` with a 2s ceiling so a hung tmux server
/// cannot delay startup, then defers the decision to [`tmux_warning`]. Kept thin
/// (only the I/O) so the branch logic stays pure and testable.
#[must_use]
pub fn check_tmux_keyboard_setup() -> Option<&'static str> {
    // Only inside tmux — the `?` short-circuits to `None` when `$TMUX` is unset.
    std::env::var_os("TMUX")?;
    let extended_keys = tmux_show("extended-keys")?;
    let extended_keys_format = tmux_show("extended-keys-format");
    tmux_warning(&extended_keys, extended_keys_format.as_deref())
}

/// Read a single global tmux option value, timing out at 2s. `None` on any
/// failure or timeout so a hung server never blocks startup.
fn tmux_show(opt: &str) -> Option<String> {
    use std::io::Read as _;

    let mut child = std::process::Command::new("tmux")
        .args(["show", "-gv", opt])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut out = String::new();
                let _ = child.stdout.as_mut()?.read_to_string(&mut out);
                return Some(out.trim().to_string());
            }
            Ok(Some(_)) => return None,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// Changelog / update banner three-state
// ---------------------------------------------------------------------------

/// The startup changelog decision (VAL-CHAT-031): given the session state and the
/// recorded last-seen version, either stay quiet, silently record the current
/// version, or display the new entries and then record.
///
/// Pure over its inputs so the three-state is unit-tested without a
/// `SettingsManager` or filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangelogStartupAction {
    /// Do nothing — resumed session, or no new entries to catch up on.
    Skip,
    /// Record the current version as last-seen; display nothing (fresh install).
    RecordOnly,
    /// Mount the supplied body in scrollback, then record the current version.
    Display(String),
}

/// Decide the startup changelog action.
///
/// - A resumed session (non-empty history) always skips: the user saw the
///   changelog when they first upgraded.
/// - A fresh install (`last_version == None`) records the current version and
///   stays quiet — there is nothing to catch up on.
/// - Otherwise, show the entries newer than `last_version`, or skip when there
///   are none.
#[must_use]
pub fn decide_changelog_startup(
    messages_empty: bool,
    last_version: Option<&str>,
    entries: &[crate::utils::changelog::ChangelogEntry],
) -> ChangelogStartupAction {
    if !messages_empty {
        return ChangelogStartupAction::Skip;
    }
    match last_version {
        None => ChangelogStartupAction::RecordOnly,
        Some(last) => {
            let new_entries = crate::utils::changelog::get_new_entries(entries, last);
            if new_entries.is_empty() {
                ChangelogStartupAction::Skip
            } else {
                let body = new_entries
                    .iter()
                    .map(|e| e.content.clone())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                ChangelogStartupAction::Display(body)
            }
        }
    }
}

/// Locate the on-disk CHANGELOG.md, trying the conventional in-repo candidates.
/// Returns the first existing path or `None`.
#[must_use]
pub fn locate_changelog_file() -> Option<std::path::PathBuf> {
    let candidates = [
        std::path::PathBuf::from("CHANGELOG.md"),
        std::path::PathBuf::from("crates/coding-agent/CHANGELOG.md"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

/// Format an "update available" banner body from the current and latest versions.
///
/// Pure so the banner copy is asserted without the network. Emitting the banner
/// at all is gated upstream by [`crate::utils::version_check::check_for_new_version`],
/// which returns `None` when offline (`HAND_OFFLINE`) — so an offline start never
/// reaches this and the banner never appears (VAL-CHAT-031).
#[must_use]
pub fn update_available_banner(current: &str, latest: &str) -> String {
    format!(
        "[update available] hand-coding-agent {latest} is newer than {current}. \
Run `cargo install --git https://github.com/badlogic/hand-ai hand-coding-agent` to upgrade. \
Changelog: https://github.com/badlogic/hand-ai/blob/main/crates/coding-agent/CHANGELOG.md"
    )
}

// ---------------------------------------------------------------------------
// OSC 133 shell-integration prompt marks
// ---------------------------------------------------------------------------

/// The OSC 133 semantic-prompt marks a turn is bracketed with, so a supporting
/// terminal can offer prompt-jump / command navigation over the transcript.
///
/// A turn emits exactly one balanced `A` → `B` → `C` sequence regardless of how
/// many tool calls it contains (VAL-CHAT-017 / VAL-CHAT-034): the user prompt is
/// `A` (prompt start) then `B` (prompt end / command start), and the whole
/// assistant response — text and any interleaved tool output — closes with a
/// single `C` (command end / output start). Never a bare, unclosed region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMark {
    /// `OSC 133 ; A ST` — prompt start (before the user's input echo).
    PromptStart,
    /// `OSC 133 ; B ST` — prompt end / command start (after the user's input).
    CommandStart,
    /// `OSC 133 ; C ST` — command end / output start (after the assistant's reply).
    CommandEnd,
}

impl PromptMark {
    /// The raw OSC 133 escape sequence for this mark, BEL-terminated (`\x07`),
    /// the widely-supported ST form.
    #[must_use]
    pub fn sequence(self) -> &'static str {
        match self {
            PromptMark::PromptStart => "\x1b]133;A\x07",
            PromptMark::CommandStart => "\x1b]133;B\x07",
            PromptMark::CommandEnd => "\x1b]133;C\x07",
        }
    }
}

// ---------------------------------------------------------------------------
// OSC 9;4 progress indicator
// ---------------------------------------------------------------------------

/// The OSC 9;4 terminal-progress state (VAL-CHAT-018). Supporting terminals
/// (ConEmu / WezTerm / iTerm2 / Windows Terminal) show a task-bar / titlebar
/// indicator while the agent works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressState {
    /// Reset: hide the indicator.
    Clear,
    /// Indeterminate spinner — used when we have no percentage.
    Indeterminate,
    /// Error / failure state — red bar.
    Error,
}

impl ProgressState {
    /// The raw OSC 9;4 escape sequence for this state, in the
    /// `ESC ] 9 ; 4 ; <state> ; <progress> BEL` form. State codes: `0` hide,
    /// `2` error, `3` indeterminate.
    #[must_use]
    pub fn sequence(self) -> &'static str {
        match self {
            ProgressState::Clear => "\x1b]9;4;0;0\x07",
            ProgressState::Indeterminate => "\x1b]9;4;3;0\x07",
            ProgressState::Error => "\x1b]9;4;2;0\x07",
        }
    }
}

// ---------------------------------------------------------------------------
// Changelog scrollback rendering
// ---------------------------------------------------------------------------

/// Render a changelog banner body into scrollback lines with a header.
///
/// A dim `[changelog]` header line, then the body split into logical lines, then
/// a trailing blank so it does not crowd the editor. Kept here (rather than in
/// `chat.rs`) because it is startup chrome, not a chat update.
#[must_use]
pub fn changelog_lines(body: &str, palette: &ThemePalette) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        "[changelog]".to_string(),
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    ))];
    for line in body.split('\n') {
        lines.push(Line::from(line.to_string()));
    }
    lines.push(Line::default());
    lines
}

/// Render a warning banner (tmux warning, update-available) into scrollback
/// lines, coloured from the palette's warning slot (the default palette keeps
/// the historical yellow). Mirrors `chat::status_lines` styling but lives with
/// the chrome so the startup path does not reach into the chat renderer's
/// private helpers.
#[must_use]
pub fn warning_lines(text: &str, palette: &ThemePalette) -> Vec<Line<'static>> {
    text.split('\n')
        .map(|line| Line::from(line.to_string()).style(Style::default().fg(palette.warning)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    /// The default palette — the historical chrome look.
    fn pal() -> ThemePalette {
        ThemePalette::default()
    }

    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn welcome_header_renders_title_and_blank() {
        // The hint row moved to the bottom chrome (see `key_hint_line`); the
        // header is now just the title and a trailing blank.
        let lines = welcome_header_lines("openai", "mock-model", "1.2.3", &pal());
        assert_eq!(lines.len(), 2, "title + trailing blank");
        assert_eq!(text_of(&lines[0]), "hand v1.2.3   openai/mock-model");
        assert!(text_of(&lines[1]).is_empty(), "trailing line is blank");
    }

    #[test]
    fn key_hint_line_lists_every_advertised_key() {
        // The persistent bottom hint lists every advertised key and its action.
        let hint = text_of(&key_hint_line(&pal()));
        for h in KEY_HINTS {
            assert!(hint.contains(h.key), "hint missing key {}: {hint:?}", h.key);
            assert!(
                hint.contains(h.action),
                "hint missing action {}: {hint:?}",
                h.action
            );
        }
    }

    #[test]
    fn welcome_header_accent_takes_the_palette() {
        // The default palette keeps the historical cyan title; a custom palette
        // recolours the header accent, so a custom theme colours the chrome
        // (VAL-COMPAT-004).
        let default = welcome_header_lines("openai", "m", "1.0", &pal());
        assert!(
            default[0]
                .spans
                .iter()
                .any(|s| s.content == "hand" && s.style.fg == Some(Color::Cyan)),
            "default title is cyan"
        );
        let neon = ThemePalette {
            accent: Color::Rgb(0xff, 0x00, 0xff),
            ..ThemePalette::default()
        };
        let themed = welcome_header_lines("openai", "m", "1.0", &neon);
        assert!(
            themed[0]
                .spans
                .iter()
                .any(|s| s.content == "hand" && s.style.fg == Some(Color::Rgb(0xff, 0x00, 0xff))),
            "custom palette recolours the header accent"
        );
    }

    #[test]
    fn welcome_hint_advertises_send_newline_and_quit_honestly() {
        // The honesty contract (VAL-CHAT-007): the advertised keys are exactly the
        // permanent chrome set, no more. If a key is added to the hint it must be
        // a real binding; this pins the current honest set.
        let keys = advertised_hint_keys();
        for expected in ["↵", "⇧↵", "↑↓", "/", "!", "^C", "^D"] {
            assert!(
                keys.contains(&expected),
                "hint must advertise {expected}, got {keys:?}"
            );
        }
    }

    #[test]
    fn tmux_warning_flags_extended_keys_off() {
        assert_eq!(tmux_warning("off", None), Some(TMUX_EXTENDED_KEYS_OFF));
        assert_eq!(tmux_warning("", None), Some(TMUX_EXTENDED_KEYS_OFF));
    }

    #[test]
    fn tmux_warning_accepts_on_and_always() {
        assert_eq!(tmux_warning("on", None), None);
        assert_eq!(tmux_warning("always", None), None);
    }

    #[test]
    fn tmux_warning_flags_xterm_format_when_extended_keys_ok() {
        assert_eq!(
            tmux_warning("on", Some("xterm")),
            Some(TMUX_EXTENDED_KEYS_FORMAT_XTERM)
        );
        // csi-u (the recommended format) is fine.
        assert_eq!(tmux_warning("on", Some("csi-u")), None);
        assert_eq!(tmux_warning("always", None), None);
    }

    #[test]
    fn changelog_skips_a_resumed_session() {
        let action = decide_changelog_startup(false, Some("1.0.0"), &[]);
        assert_eq!(action, ChangelogStartupAction::Skip);
    }

    #[test]
    fn changelog_records_only_on_fresh_install() {
        // No recorded version + empty session = fresh install: record, stay quiet.
        let action = decide_changelog_startup(true, None, &[]);
        assert_eq!(action, ChangelogStartupAction::RecordOnly);
    }

    #[test]
    fn changelog_displays_new_entries_when_behind() {
        use crate::utils::changelog::ChangelogEntry;
        let entries = vec![
            ChangelogEntry {
                major: 1,
                minor: 1,
                patch: 0,
                content: "## 1.1.0\n- new thing".to_string(),
            },
            ChangelogEntry {
                major: 1,
                minor: 0,
                patch: 0,
                content: "## 1.0.0\n- old thing".to_string(),
            },
        ];
        let action = decide_changelog_startup(true, Some("1.0.0"), &entries);
        match action {
            ChangelogStartupAction::Display(body) => {
                assert!(body.contains("1.1.0"), "shows the newer entry: {body:?}");
                assert!(!body.contains("old thing"), "omits the already-seen entry");
            }
            other => panic!("expected Display, got {other:?}"),
        }
    }

    #[test]
    fn changelog_skips_when_caught_up() {
        use crate::utils::changelog::ChangelogEntry;
        let entries = vec![ChangelogEntry {
            major: 1,
            minor: 0,
            patch: 0,
            content: "## 1.0.0\n- thing".to_string(),
        }];
        let action = decide_changelog_startup(true, Some("1.0.0"), &entries);
        assert_eq!(action, ChangelogStartupAction::Skip);
    }

    #[test]
    fn prompt_marks_are_balanced_a_b_c() {
        // A turn's three marks are distinct and each is a complete OSC 133 escape.
        assert_eq!(PromptMark::PromptStart.sequence(), "\x1b]133;A\x07");
        assert_eq!(PromptMark::CommandStart.sequence(), "\x1b]133;B\x07");
        assert_eq!(PromptMark::CommandEnd.sequence(), "\x1b]133;C\x07");
        // Balanced: one A, one B, one C — no bare region.
        let turn = format!(
            "{}{}{}",
            PromptMark::PromptStart.sequence(),
            PromptMark::CommandStart.sequence(),
            PromptMark::CommandEnd.sequence(),
        );
        assert_eq!(turn.matches("\x1b]133;A").count(), 1);
        assert_eq!(turn.matches("\x1b]133;B").count(), 1);
        assert_eq!(turn.matches("\x1b]133;C").count(), 1);
    }

    #[test]
    fn progress_sequences_match_osc_9_4_states() {
        assert_eq!(ProgressState::Clear.sequence(), "\x1b]9;4;0;0\x07");
        assert_eq!(ProgressState::Indeterminate.sequence(), "\x1b]9;4;3;0\x07");
        assert_eq!(ProgressState::Error.sequence(), "\x1b]9;4;2;0\x07");
    }

    #[test]
    fn update_banner_names_both_versions() {
        let banner = update_available_banner("1.0.0", "1.1.0");
        assert!(banner.contains("1.0.0"));
        assert!(banner.contains("1.1.0"));
        assert!(banner.contains("update available"));
    }

    #[test]
    fn changelog_lines_have_header_and_trailing_blank() {
        let lines = changelog_lines("- a\n- b", &pal());
        assert_eq!(text_of(&lines[0]), "[changelog]");
        assert_eq!(text_of(&lines[1]), "- a");
        assert_eq!(text_of(&lines[2]), "- b");
        assert!(text_of(lines.last().unwrap()).is_empty());
    }

    #[test]
    fn warning_lines_style_yellow() {
        let lines = warning_lines("careful\nnow", &pal());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].style.fg, Some(Color::Yellow));
    }
}
