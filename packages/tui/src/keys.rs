//! Keyboard input handling for terminal applications.
//!
//! Supports both legacy terminal sequences and the Kitty keyboard protocol.
//! See: <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>
//!
//! Symbol keys are also supported, however some `ctrl+symbol` combos overlap
//! with ASCII control codes, e.g. `ctrl+[ == ESC`.
//! See: <https://sw.kovidgoyal.net/kitty/keyboard-protocol/#legacy-ctrl-mapping-of-ascii-keys>
//!
//! Public API mirrors the upstream TypeScript implementation:
//! - [`matches_key`] — check whether raw input matches a key identifier
//! - [`parse_key`]   — structured parse used by the built-in components
//! - [`parse_key_id`] — canonical key-id string (mirrors TS `parseKey`)
//! - [`decode_kitty_printable`] / [`decode_printable_key`]
//! - [`is_kitty_protocol_active`] / [`set_kitty_protocol_active`]
//! - [`is_key_release`] / [`is_key_repeat`]
//!
//! `KeyId` is a canonical lowercase string of the form
//! `"<modifier>+...+<base>"`, e.g. `"ctrl+shift+p"`, `"alt+enter"`, `"f12"`,
//! `"a"`, `"/"`. The string form is chosen to mirror the TS API one-to-one,
//! keeping downstream keybinding tables (M1.T4) trivially portable. Modifier
//! order is *not* canonicalized on input — `matches_key` parses the modifier
//! set into a bitmask before comparing, so `"shift+ctrl+a"` and
//! `"ctrl+shift+a"` are equivalent. When this module *produces* a `KeyId`
//! (via [`parse_key_id`]), it always emits modifiers in the order
//! `shift, ctrl, alt, super` to match the upstream implementation.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

// =============================================================================
// Public types
// =============================================================================

/// Modifier key flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyModifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub super_key: bool,
}

impl KeyModifiers {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn ctrl() -> Self {
        Self {
            ctrl: true,
            ..Default::default()
        }
    }

    pub fn alt() -> Self {
        Self {
            alt: true,
            ..Default::default()
        }
    }

    pub fn shift() -> Self {
        Self {
            shift: true,
            ..Default::default()
        }
    }

    /// Convert from the Kitty modifier value (1-based; bit 0 = shift, bit 1 =
    /// alt, bit 2 = ctrl, bit 3 = super, plus caps/num lock bits we ignore).
    pub fn from_kitty_bits(bits: u32) -> Self {
        let value = bits.saturating_sub(1) & !LOCK_MASK;
        Self::from_modifier_mask(value)
    }

    fn from_modifier_mask(mask: u32) -> Self {
        Self {
            shift: mask & MOD_SHIFT != 0,
            alt: mask & MOD_ALT != 0,
            ctrl: mask & MOD_CTRL != 0,
            super_key: mask & MOD_SUPER != 0,
        }
    }
}

/// Kitty event type. Only meaningful when Kitty keyboard protocol with flag 2
/// (report event types) is active; otherwise every event is a `Press`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventType {
    Press,
    Repeat,
    Release,
}

impl Default for KeyEventType {
    fn default() -> Self {
        Self::Press
    }
}

/// Parsed key event. Mirrors the legacy `Key` struct used by built-in
/// components plus optional Kitty-protocol fields (`event_type`,
/// `base_layout_key`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub name: KeyName,
    pub modifiers: KeyModifiers,
    /// `true` when `event_type == Release`. Kept as a flat field for
    /// backward compatibility with existing component code.
    pub is_release: bool,
    pub event_type: KeyEventType,
    /// Kitty protocol flag 4 (Report alternate keys) base-layout key, when
    /// present. The "base layout key" is the key in the standard PC-101
    /// layout — used to disambiguate non-Latin keyboard layouts.
    pub base_layout_key: Option<u32>,
}

/// Named keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyName {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Delete,
    Escape,
    Space,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Clear,
    F(u8),
    Unknown(String),
}

impl fmt::Display for KeyName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyName::Char(c) => write!(f, "{}", c),
            KeyName::Enter => write!(f, "Enter"),
            KeyName::Tab => write!(f, "Tab"),
            KeyName::Backspace => write!(f, "Backspace"),
            KeyName::Delete => write!(f, "Delete"),
            KeyName::Escape => write!(f, "Escape"),
            KeyName::Space => write!(f, "Space"),
            KeyName::Up => write!(f, "Up"),
            KeyName::Down => write!(f, "Down"),
            KeyName::Left => write!(f, "Left"),
            KeyName::Right => write!(f, "Right"),
            KeyName::Home => write!(f, "Home"),
            KeyName::End => write!(f, "End"),
            KeyName::PageUp => write!(f, "PageUp"),
            KeyName::PageDown => write!(f, "PageDown"),
            KeyName::Insert => write!(f, "Insert"),
            KeyName::Clear => write!(f, "Clear"),
            KeyName::F(n) => write!(f, "F{}", n),
            KeyName::Unknown(s) => write!(f, "Unknown({})", s),
        }
    }
}

/// Canonical key identifier. See module-level docs for the format.
pub type KeyId = String;

// =============================================================================
// Kitty protocol active flag
// =============================================================================

static KITTY_PROTOCOL_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Set the global Kitty keyboard protocol state. Called by the terminal layer
/// after detecting protocol support.
pub fn set_kitty_protocol_active(active: bool) {
    KITTY_PROTOCOL_ACTIVE.store(active, Ordering::Relaxed);
}

/// Query whether Kitty keyboard protocol is currently active.
pub fn is_kitty_protocol_active() -> bool {
    KITTY_PROTOCOL_ACTIVE.load(Ordering::Relaxed)
}

// =============================================================================
// Constants
// =============================================================================

// Modifier bitmask values (after subtracting the 1-based Kitty offset).
const MOD_SHIFT: u32 = 1;
const MOD_ALT: u32 = 2;
const MOD_CTRL: u32 = 4;
const MOD_SUPER: u32 = 8;
const LOCK_MASK: u32 = 64 + 128; // Caps Lock + Num Lock

// Key codepoints. Negative values are sentinels for non-codepoint keys
// (arrows, navigation). They never collide with real Unicode codepoints,
// which are non-negative.
const CP_ESCAPE: i32 = 27;
const CP_TAB: i32 = 9;
const CP_ENTER: i32 = 13;
const CP_SPACE: i32 = 32;
const CP_BACKSPACE: i32 = 127;
const CP_KP_ENTER: i32 = 57414;

const CP_UP: i32 = -1;
const CP_DOWN: i32 = -2;
const CP_RIGHT: i32 = -3;
const CP_LEFT: i32 = -4;

const CP_DELETE: i32 = -10;
const CP_INSERT: i32 = -11;
const CP_PAGE_UP: i32 = -12;
const CP_PAGE_DOWN: i32 = -13;
const CP_HOME: i32 = -14;
const CP_END: i32 = -15;

/// Symbol keys recognized when matching shifted/ctrl combos.
const SYMBOL_KEYS: &[char] = &[
    '`', '-', '=', '[', ']', '\\', ';', '\'', ',', '.', '/', '!', '@', '#', '$', '%', '^', '&',
    '*', '(', ')', '_', '+', '|', '~', '{', '}', ':', '<', '>', '?',
];

fn is_symbol_key(c: char) -> bool {
    SYMBOL_KEYS.contains(&c)
}

/// Map Kitty functional codepoints (numpad keys, etc.) to their logical
/// equivalents (digits, symbols, navigation sentinels).
fn normalize_kitty_functional_codepoint(cp: i32) -> i32 {
    match cp {
        57399 => 48, // KP_0
        57400 => 49, // KP_1
        57401 => 50, // KP_2
        57402 => 51, // KP_3
        57403 => 52, // KP_4
        57404 => 53, // KP_5
        57405 => 54, // KP_6
        57406 => 55, // KP_7
        57407 => 56, // KP_8
        57408 => 57, // KP_9
        57409 => 46, // KP_DECIMAL  -> .
        57410 => 47, // KP_DIVIDE   -> /
        57411 => 42, // KP_MULTIPLY -> *
        57412 => 45, // KP_SUBTRACT -> -
        57413 => 43, // KP_ADD      -> +
        57415 => 61, // KP_EQUAL    -> =
        57416 => 44, // KP_SEPARATOR-> ,
        57417 => CP_LEFT,
        57418 => CP_RIGHT,
        57419 => CP_UP,
        57420 => CP_DOWN,
        57421 => CP_PAGE_UP,
        57422 => CP_PAGE_DOWN,
        57423 => CP_HOME,
        57424 => CP_END,
        57425 => CP_INSERT,
        57426 => CP_DELETE,
        other => other,
    }
}

/// When Shift is held, normalize an ASCII uppercase letter codepoint to its
/// lowercase identity. Mirrors TS `normalizeShiftedLetterIdentityCodepoint`.
fn normalize_shifted_letter_identity_codepoint(cp: i32, modifier: u32) -> i32 {
    let effective = modifier & !LOCK_MASK;
    if (effective & MOD_SHIFT) != 0 && (65..=90).contains(&cp) {
        cp + 32
    } else {
        cp
    }
}

// =============================================================================
// Legacy sequence tables
// =============================================================================

/// Returns the canonical KeyId for a known legacy escape sequence, if any.
/// Mirrors TS `LEGACY_SEQUENCE_KEY_IDS` with a few additional entries used
/// only by `matchesKey` (the lookup tables for arrow/function keys).
fn legacy_sequence_key_id(data: &str) -> Option<&'static str> {
    let id = match data {
        // SS3 arrows / nav
        "\x1bOA" => "up",
        "\x1bOB" => "down",
        "\x1bOC" => "right",
        "\x1bOD" => "left",
        "\x1bOH" => "home",
        "\x1bOF" => "end",
        // Clear
        "\x1b[E" => "clear",
        "\x1bOE" => "clear",
        "\x1bOe" => "ctrl+clear",
        "\x1b[e" => "shift+clear",
        // Insert / Delete
        "\x1b[2~" => "insert",
        "\x1b[2$" => "shift+insert",
        "\x1b[2^" => "ctrl+insert",
        "\x1b[3$" => "shift+delete",
        "\x1b[3^" => "ctrl+delete",
        // Double-bracket pageUp/pageDown
        "\x1b[[5~" => "pageUp",
        "\x1b[[6~" => "pageDown",
        // rxvt shifted/ctrl arrows
        "\x1b[a" => "shift+up",
        "\x1b[b" => "shift+down",
        "\x1b[c" => "shift+right",
        "\x1b[d" => "shift+left",
        "\x1bOa" => "ctrl+up",
        "\x1bOb" => "ctrl+down",
        "\x1bOc" => "ctrl+right",
        "\x1bOd" => "ctrl+left",
        // rxvt shifted/ctrl nav
        "\x1b[5$" => "shift+pageUp",
        "\x1b[6$" => "shift+pageDown",
        "\x1b[7$" => "shift+home",
        "\x1b[8$" => "shift+end",
        "\x1b[5^" => "ctrl+pageUp",
        "\x1b[6^" => "ctrl+pageDown",
        "\x1b[7^" => "ctrl+home",
        "\x1b[8^" => "ctrl+end",
        // Function keys
        "\x1bOP" => "f1",
        "\x1bOQ" => "f2",
        "\x1bOR" => "f3",
        "\x1bOS" => "f4",
        "\x1b[11~" => "f1",
        "\x1b[12~" => "f2",
        "\x1b[13~" => "f3",
        "\x1b[14~" => "f4",
        "\x1b[[A" => "f1",
        "\x1b[[B" => "f2",
        "\x1b[[C" => "f3",
        "\x1b[[D" => "f4",
        "\x1b[[E" => "f5",
        "\x1b[15~" => "f5",
        "\x1b[17~" => "f6",
        "\x1b[18~" => "f7",
        "\x1b[19~" => "f8",
        "\x1b[20~" => "f9",
        "\x1b[21~" => "f10",
        "\x1b[23~" => "f11",
        "\x1b[24~" => "f12",
        // Alt-prefixed legacy arrows
        "\x1bb" => "alt+left",
        "\x1bf" => "alt+right",
        "\x1bp" => "alt+up",
        "\x1bn" => "alt+down",
        _ => return None,
    };
    Some(id)
}

/// Sequences that map directly to a single key id (no modifier variants).
fn legacy_sequence_for_key(key: &str) -> &'static [&'static str] {
    match key {
        "up" => &["\x1b[A", "\x1bOA"],
        "down" => &["\x1b[B", "\x1bOB"],
        "right" => &["\x1b[C", "\x1bOC"],
        "left" => &["\x1b[D", "\x1bOD"],
        "home" => &["\x1b[H", "\x1bOH", "\x1b[1~", "\x1b[7~"],
        "end" => &["\x1b[F", "\x1bOF", "\x1b[4~", "\x1b[8~"],
        "insert" => &["\x1b[2~"],
        "delete" => &["\x1b[3~"],
        "pageup" => &["\x1b[5~", "\x1b[[5~"],
        "pagedown" => &["\x1b[6~", "\x1b[[6~"],
        "clear" => &["\x1b[E", "\x1bOE"],
        "f1" => &["\x1bOP", "\x1b[11~", "\x1b[[A"],
        "f2" => &["\x1bOQ", "\x1b[12~", "\x1b[[B"],
        "f3" => &["\x1bOR", "\x1b[13~", "\x1b[[C"],
        "f4" => &["\x1bOS", "\x1b[14~", "\x1b[[D"],
        "f5" => &["\x1b[15~", "\x1b[[E"],
        "f6" => &["\x1b[17~"],
        "f7" => &["\x1b[18~"],
        "f8" => &["\x1b[19~"],
        "f9" => &["\x1b[20~"],
        "f10" => &["\x1b[21~"],
        "f11" => &["\x1b[23~"],
        "f12" => &["\x1b[24~"],
        _ => &[],
    }
}

fn legacy_shift_sequence_for_key(key: &str) -> &'static [&'static str] {
    match key {
        "up" => &["\x1b[a"],
        "down" => &["\x1b[b"],
        "right" => &["\x1b[c"],
        "left" => &["\x1b[d"],
        "clear" => &["\x1b[e"],
        "insert" => &["\x1b[2$"],
        "delete" => &["\x1b[3$"],
        "pageup" => &["\x1b[5$"],
        "pagedown" => &["\x1b[6$"],
        "home" => &["\x1b[7$"],
        "end" => &["\x1b[8$"],
        _ => &[],
    }
}

fn legacy_ctrl_sequence_for_key(key: &str) -> &'static [&'static str] {
    match key {
        "up" => &["\x1bOa"],
        "down" => &["\x1bOb"],
        "right" => &["\x1bOc"],
        "left" => &["\x1bOd"],
        "clear" => &["\x1bOe"],
        "insert" => &["\x1b[2^"],
        "delete" => &["\x1b[3^"],
        "pageup" => &["\x1b[5^"],
        "pagedown" => &["\x1b[6^"],
        "home" => &["\x1b[7^"],
        "end" => &["\x1b[8^"],
        _ => &[],
    }
}

fn matches_legacy_modifier_sequence(data: &str, key: &str, modifier: u32) -> bool {
    if modifier == MOD_SHIFT {
        return legacy_shift_sequence_for_key(key).contains(&data);
    }
    if modifier == MOD_CTRL {
        return legacy_ctrl_sequence_for_key(key).contains(&data);
    }
    false
}

// =============================================================================
// Kitty CSI-u parsing
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedKittySequence {
    codepoint: i32,
    shifted_key: Option<u32>,
    base_layout_key: Option<u32>,
    /// Modifier bitmask (already 0-based, with lock bits cleared).
    modifier: u32,
    event_type: KeyEventType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedModifyOtherKeys {
    codepoint: u32,
    modifier: u32,
}

/// Parse the four Kitty CSI-u sequence shapes:
/// - `CSI <cp>[:<shifted>[:<base>]];<mod>[:<event>]u`
/// - `CSI 1;<mod>[:<event>][ABCD]`        (arrow keys)
/// - `CSI <num>[;<mod>][:<event>]~`       (functional keys)
/// - `CSI 1;<mod>[:<event>][HF]`          (home/end)
fn parse_kitty_sequence(data: &str) -> Option<ParsedKittySequence> {
    let body = data.strip_prefix("\x1b[")?;

    // CSI-u with optional shifted/base/event parts.
    if let Some(inner) = body.strip_suffix('u') {
        return parse_csi_u(inner);
    }

    // Arrow keys with modifier: `1;<mod>[:<event>][ABCD]`
    if let Some(last) = body.chars().last()
        && matches!(last, 'A' | 'B' | 'C' | 'D')
        && let Some(rest) = body.strip_prefix("1;")
        && let Some(prefix) = rest.strip_suffix(last)
        && let Some((mod_str, event_str)) = split_optional_colon(prefix)
        && let Ok(mod_value) = mod_str.parse::<u32>()
    {
        let event_type = parse_event_type_str(event_str);
        let cp = match last {
            'A' => CP_UP,
            'B' => CP_DOWN,
            'C' => CP_RIGHT,
            'D' => CP_LEFT,
            _ => unreachable!(),
        };
        return Some(ParsedKittySequence {
            codepoint: cp,
            shifted_key: None,
            base_layout_key: None,
            modifier: mod_value.saturating_sub(1) & !LOCK_MASK,
            event_type,
        });
    }

    // Home/End with modifier: `1;<mod>[:<event>][HF]`
    if let Some(last) = body.chars().last()
        && matches!(last, 'H' | 'F')
        && let Some(rest) = body.strip_prefix("1;")
        && let Some(prefix) = rest.strip_suffix(last)
        && let Some((mod_str, event_str)) = split_optional_colon(prefix)
        && let Ok(mod_value) = mod_str.parse::<u32>()
    {
        let event_type = parse_event_type_str(event_str);
        let cp = if last == 'H' { CP_HOME } else { CP_END };
        return Some(ParsedKittySequence {
            codepoint: cp,
            shifted_key: None,
            base_layout_key: None,
            modifier: mod_value.saturating_sub(1) & !LOCK_MASK,
            event_type,
        });
    }

    // Functional keys: `<num>[;<mod>][:<event>]~`
    if let Some(inner) = body.strip_suffix('~') {
        return parse_functional_tilde(inner);
    }

    None
}

/// Split a `"<a>[:<b>]"` string into `(<a>, Some(<b>))` or `(<a>, None)`.
fn split_optional_colon(s: &str) -> Option<(&str, Option<&str>)> {
    let mut it = s.splitn(2, ':');
    let a = it.next()?;
    let b = it.next();
    Some((a, b))
}

fn parse_event_type_str(s: Option<&str>) -> KeyEventType {
    match s.and_then(|v| v.parse::<u32>().ok()) {
        Some(2) => KeyEventType::Repeat,
        Some(3) => KeyEventType::Release,
        _ => KeyEventType::Press,
    }
}

/// Parse the body of a CSI-u sequence (everything between `\x1b[` and `u`).
///
/// Format groups (all optional except `<cp>`):
/// `<cp>[":"<shifted?>][":"<base?>]";"<mod>[":"<event>]`
fn parse_csi_u(inner: &str) -> Option<ParsedKittySequence> {
    // Split off the modifier section first.
    let (cp_part, mod_part) = match inner.split_once(';') {
        Some((cp, m)) => (cp, Some(m)),
        None => (inner, None),
    };

    // Codepoint part may be `cp`, `cp:shifted`, `cp:shifted:base`, or `cp::base`.
    let mut cp_iter = cp_part.split(':');
    let cp_str = cp_iter.next()?;
    let codepoint = cp_str.parse::<u32>().ok()? as i32;
    let shifted_str = cp_iter.next();
    let base_str = cp_iter.next();
    if cp_iter.next().is_some() {
        return None;
    }

    let shifted_key = match shifted_str {
        Some(s) if !s.is_empty() => Some(s.parse::<u32>().ok()?),
        _ => None,
    };
    let base_layout_key = match base_str {
        Some(s) if !s.is_empty() => Some(s.parse::<u32>().ok()?),
        _ => None,
    };

    // Modifier part may be `mod`, `mod:event`, or absent.
    let (modifier, event_type) = match mod_part {
        None => (0, KeyEventType::Press),
        Some(m) => {
            let mut parts = m.splitn(2, ':');
            let mod_str = parts.next()?;
            let event_str = parts.next();
            let mod_value = mod_str.parse::<u32>().ok()?;
            (
                mod_value.saturating_sub(1) & !LOCK_MASK,
                parse_event_type_str(event_str),
            )
        }
    };

    Some(ParsedKittySequence {
        codepoint,
        shifted_key,
        base_layout_key,
        modifier,
        event_type,
    })
}

/// Parse `<num>[;<mod>][:<event>]` (the body of a CSI `~` sequence).
fn parse_functional_tilde(inner: &str) -> Option<ParsedKittySequence> {
    // Split off any event-type colon first.
    let (head, event_str) = match inner.split_once(':') {
        Some((h, e)) => (h, Some(e)),
        None => (inner, None),
    };
    let (num_str, mod_str) = match head.split_once(';') {
        Some((n, m)) => (n, Some(m)),
        None => (head, None),
    };
    let num: u32 = num_str.parse().ok()?;
    let mod_value = match mod_str {
        Some(m) => m.parse::<u32>().ok()?,
        None => 1,
    };
    let event_type = parse_event_type_str(event_str);

    let cp = match num {
        2 => CP_INSERT,
        3 => CP_DELETE,
        5 => CP_PAGE_UP,
        6 => CP_PAGE_DOWN,
        7 => CP_HOME,
        8 => CP_END,
        _ => return None,
    };

    Some(ParsedKittySequence {
        codepoint: cp,
        shifted_key: None,
        base_layout_key: None,
        modifier: mod_value.saturating_sub(1) & !LOCK_MASK,
        event_type,
    })
}

fn parse_modify_other_keys(data: &str) -> Option<ParsedModifyOtherKeys> {
    // Format: `\x1b[27;<modifier>;<keycode>~`
    let body = data.strip_prefix("\x1b[27;")?.strip_suffix('~')?;
    let (mod_str, cp_str) = body.split_once(';')?;
    let mod_value: u32 = mod_str.parse().ok()?;
    let codepoint: u32 = cp_str.parse().ok()?;
    Some(ParsedModifyOtherKeys {
        codepoint,
        modifier: mod_value.saturating_sub(1) & !LOCK_MASK,
    })
}

// =============================================================================
// Kitty matching helpers
// =============================================================================

fn matches_kitty_sequence(data: &str, expected_cp: i32, expected_mod: u32) -> bool {
    let Some(parsed) = parse_kitty_sequence(data) else {
        return false;
    };
    if parsed.modifier != expected_mod {
        return false;
    }

    let normalized_actual = normalize_shifted_letter_identity_codepoint(
        normalize_kitty_functional_codepoint(parsed.codepoint),
        parsed.modifier,
    );
    let normalized_expected = normalize_shifted_letter_identity_codepoint(
        normalize_kitty_functional_codepoint(expected_cp),
        expected_mod,
    );

    if normalized_actual == normalized_expected {
        return true;
    }

    // Base-layout-key fallback for non-Latin layouts. Only applied when the
    // primary codepoint isn't already a recognized Latin letter or symbol.
    if let Some(base) = parsed.base_layout_key
        && (base as i32) == expected_cp
    {
        let cp = normalized_actual;
        let is_latin_letter = (97..=122).contains(&cp);
        let is_known_symbol = char::from_u32(cp as u32)
            .map(is_symbol_key)
            .unwrap_or(false);
        if !is_latin_letter && !is_known_symbol {
            return true;
        }
    }

    false
}

fn matches_modify_other_keys_exact(data: &str, expected_cp: u32, expected_mod: u32) -> bool {
    let Some(parsed) = parse_modify_other_keys(data) else {
        return false;
    };
    parsed.codepoint == expected_cp && parsed.modifier == expected_mod
}

fn matches_printable_modify_other_keys(data: &str, expected_cp: u32, expected_mod: u32) -> bool {
    if expected_mod == 0 {
        return false;
    }
    let Some(parsed) = parse_modify_other_keys(data) else {
        return false;
    };
    if parsed.modifier != expected_mod {
        return false;
    }
    normalize_shifted_letter_identity_codepoint(parsed.codepoint as i32, parsed.modifier)
        == normalize_shifted_letter_identity_codepoint(expected_cp as i32, expected_mod)
}

// =============================================================================
// Environment helpers
// =============================================================================

fn is_windows_terminal_session() -> bool {
    if std::env::var_os("WT_SESSION").is_none() {
        return false;
    }
    std::env::var_os("SSH_CONNECTION").is_none()
        && std::env::var_os("SSH_CLIENT").is_none()
        && std::env::var_os("SSH_TTY").is_none()
}

/// Raw `0x08` (`BS`) is ambiguous: Windows Terminal sends it for
/// `Ctrl+Backspace`, while many legacy terminals send it for plain
/// `Backspace`. `0x7f` is unambiguously plain `Backspace`.
fn matches_raw_backspace(data: &str, expected_mod: u32) -> bool {
    if data == "\x7f" {
        return expected_mod == 0;
    }
    if data != "\x08" {
        return false;
    }
    if is_windows_terminal_session() {
        expected_mod == MOD_CTRL
    } else {
        expected_mod == 0
    }
}

// =============================================================================
// Generic key matching
// =============================================================================

/// Universal `Ctrl+<key>` mapping: `code & 0x1f`.
fn raw_ctrl_char(key: char) -> Option<char> {
    let lower = key.to_ascii_lowercase();
    let code = lower as u32;
    if (97..=122).contains(&code) || matches!(lower, '[' | '\\' | ']' | '_') {
        char::from_u32(code & 0x1f)
    } else if lower == '-' {
        // `-` shares its physical key with `_` on US keyboards.
        char::from_u32(31)
    } else {
        None
    }
}

fn is_digit_key(c: char) -> bool {
    c.is_ascii_digit()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedKeyId<'a> {
    key: &'a str,
    modifier: u32,
}

/// Parse a `KeyId` string into a `(<base>, <modifier-bitmask>)` pair.
/// Modifier order is irrelevant; unknown modifier names are silently ignored
/// (not flagged as errors), matching the upstream TS behavior.
fn parse_key_id_components(key_id: &str) -> Option<ParsedKeyId<'_>> {
    let trimmed = key_id.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Trailing `++` means the base key is literally `+` (e.g. `shift++`),
    // so we must split on the *last* `+` rather than on every `+`.
    // The bare `"+"` is also the literal `+` key with no modifiers.
    let (mod_section, key) = if trimmed == "+" {
        ("", "+")
    } else if let Some(rest) = trimmed.strip_suffix("++") {
        (rest, "+")
    } else {
        match trimmed.rfind('+') {
            Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
            None => ("", trimmed),
        }
    };
    if key.is_empty() {
        return None;
    }
    let mut modifier = 0u32;
    if !mod_section.is_empty() {
        for raw in mod_section.split('+') {
            match raw.to_ascii_lowercase().as_str() {
                "ctrl" => modifier |= MOD_CTRL,
                "shift" => modifier |= MOD_SHIFT,
                "alt" => modifier |= MOD_ALT,
                "super" | "meta" | "cmd" => modifier |= MOD_SUPER,
                _ => {}
            }
        }
    }
    Some(ParsedKeyId { key, modifier })
}

/// Match raw input data against a `KeyId`. Mirrors TS `matchesKey`.
pub fn matches_key(data: &str, key_id: &str) -> bool {
    let Some(parsed) = parse_key_id_components(key_id) else {
        return false;
    };
    matches_key_inner(data, parsed.key, parsed.modifier)
}

fn matches_key_inner(data: &str, key: &str, modifier: u32) -> bool {
    let key_lower = key.to_ascii_lowercase();
    match key_lower.as_str() {
        "escape" | "esc" => {
            if modifier != 0 {
                return false;
            }
            data == "\x1b"
                || matches_kitty_sequence(data, CP_ESCAPE, 0)
                || matches_modify_other_keys_exact(data, CP_ESCAPE as u32, 0)
        }

        "space" => {
            if !is_kitty_protocol_active() {
                if modifier == MOD_CTRL && data == "\x00" {
                    return true;
                }
                if modifier == MOD_ALT && data == "\x1b " {
                    return true;
                }
            }
            if modifier == 0 {
                return data == " "
                    || matches_kitty_sequence(data, CP_SPACE, 0)
                    || matches_modify_other_keys_exact(data, CP_SPACE as u32, 0);
            }
            matches_kitty_sequence(data, CP_SPACE, modifier)
                || matches_modify_other_keys_exact(data, CP_SPACE as u32, modifier)
        }

        "tab" => {
            if modifier == MOD_SHIFT {
                return data == "\x1b[Z"
                    || matches_kitty_sequence(data, CP_TAB, MOD_SHIFT)
                    || matches_modify_other_keys_exact(data, CP_TAB as u32, MOD_SHIFT);
            }
            if modifier == 0 {
                return data == "\t" || matches_kitty_sequence(data, CP_TAB, 0);
            }
            matches_kitty_sequence(data, CP_TAB, modifier)
                || matches_modify_other_keys_exact(data, CP_TAB as u32, modifier)
        }

        "enter" | "return" => {
            if modifier == MOD_SHIFT {
                if matches_kitty_sequence(data, CP_ENTER, MOD_SHIFT)
                    || matches_kitty_sequence(data, CP_KP_ENTER, MOD_SHIFT)
                    || matches_modify_other_keys_exact(data, CP_ENTER as u32, MOD_SHIFT)
                {
                    return true;
                }
                if is_kitty_protocol_active() {
                    return data == "\x1b\r" || data == "\n";
                }
                return false;
            }
            if modifier == MOD_ALT {
                if matches_kitty_sequence(data, CP_ENTER, MOD_ALT)
                    || matches_kitty_sequence(data, CP_KP_ENTER, MOD_ALT)
                    || matches_modify_other_keys_exact(data, CP_ENTER as u32, MOD_ALT)
                {
                    return true;
                }
                if !is_kitty_protocol_active() {
                    return data == "\x1b\r";
                }
                return false;
            }
            if modifier == 0 {
                return data == "\r"
                    || (!is_kitty_protocol_active() && data == "\n")
                    || data == "\x1bOM"
                    || matches_kitty_sequence(data, CP_ENTER, 0)
                    || matches_kitty_sequence(data, CP_KP_ENTER, 0);
            }
            matches_kitty_sequence(data, CP_ENTER, modifier)
                || matches_kitty_sequence(data, CP_KP_ENTER, modifier)
                || matches_modify_other_keys_exact(data, CP_ENTER as u32, modifier)
        }

        "backspace" => {
            if modifier == MOD_ALT {
                if data == "\x1b\x7f" || data == "\x1b\x08" {
                    return true;
                }
                return matches_kitty_sequence(data, CP_BACKSPACE, MOD_ALT)
                    || matches_modify_other_keys_exact(data, CP_BACKSPACE as u32, MOD_ALT);
            }
            if modifier == MOD_CTRL {
                if matches_raw_backspace(data, MOD_CTRL) {
                    return true;
                }
                return matches_kitty_sequence(data, CP_BACKSPACE, MOD_CTRL)
                    || matches_modify_other_keys_exact(data, CP_BACKSPACE as u32, MOD_CTRL);
            }
            if modifier == 0 {
                return matches_raw_backspace(data, 0)
                    || matches_kitty_sequence(data, CP_BACKSPACE, 0)
                    || matches_modify_other_keys_exact(data, CP_BACKSPACE as u32, 0);
            }
            matches_kitty_sequence(data, CP_BACKSPACE, modifier)
                || matches_modify_other_keys_exact(data, CP_BACKSPACE as u32, modifier)
        }

        "insert" => match_legacy_or_kitty(data, "insert", modifier, CP_INSERT),
        "delete" => match_legacy_or_kitty(data, "delete", modifier, CP_DELETE),
        "home" => match_legacy_or_kitty(data, "home", modifier, CP_HOME),
        "end" => match_legacy_or_kitty(data, "end", modifier, CP_END),
        "pageup" => match_legacy_or_kitty(data, "pageup", modifier, CP_PAGE_UP),
        "pagedown" => match_legacy_or_kitty(data, "pagedown", modifier, CP_PAGE_DOWN),

        "clear" => {
            if modifier == 0 {
                return legacy_sequence_for_key("clear").contains(&data);
            }
            matches_legacy_modifier_sequence(data, "clear", modifier)
        }

        "up" | "down" => match_arrow_no_legacy_extras(data, &key_lower, modifier),
        "left" | "right" => match_arrow_with_legacy_extras(data, &key_lower, modifier),

        "f1" | "f2" | "f3" | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12" => {
            if modifier != 0 {
                return false;
            }
            legacy_sequence_for_key(&key_lower).contains(&data)
        }

        _ => match_printable_key(data, key, modifier),
    }
}

fn match_legacy_or_kitty(data: &str, key: &str, modifier: u32, kitty_cp: i32) -> bool {
    if modifier == 0 {
        return legacy_sequence_for_key(key).contains(&data)
            || matches_kitty_sequence(data, kitty_cp, 0);
    }
    if matches_legacy_modifier_sequence(data, key, modifier) {
        return true;
    }
    matches_kitty_sequence(data, kitty_cp, modifier)
}

fn match_arrow_no_legacy_extras(data: &str, key: &str, modifier: u32) -> bool {
    let cp = match key {
        "up" => CP_UP,
        "down" => CP_DOWN,
        _ => return false,
    };
    if modifier == MOD_ALT {
        let legacy = match key {
            "up" => "\x1bp",
            "down" => "\x1bn",
            _ => "",
        };
        return data == legacy || matches_kitty_sequence(data, cp, MOD_ALT);
    }
    if modifier == 0 {
        return legacy_sequence_for_key(key).contains(&data) || matches_kitty_sequence(data, cp, 0);
    }
    if matches_legacy_modifier_sequence(data, key, modifier) {
        return true;
    }
    matches_kitty_sequence(data, cp, modifier)
}

fn match_arrow_with_legacy_extras(data: &str, key: &str, modifier: u32) -> bool {
    let (cp, alt_csi, alt_legacy_uppercase, alt_legacy_lowercase, ctrl_csi) = match key {
        "left" => (CP_LEFT, "\x1b[1;3D", "\x1bB", "\x1bb", "\x1b[1;5D"),
        "right" => (CP_RIGHT, "\x1b[1;3C", "\x1bF", "\x1bf", "\x1b[1;5C"),
        _ => return false,
    };
    if modifier == MOD_ALT {
        return data == alt_csi
            || (!is_kitty_protocol_active() && data == alt_legacy_uppercase)
            || data == alt_legacy_lowercase
            || matches_kitty_sequence(data, cp, MOD_ALT);
    }
    if modifier == MOD_CTRL {
        return data == ctrl_csi
            || matches_legacy_modifier_sequence(data, key, MOD_CTRL)
            || matches_kitty_sequence(data, cp, MOD_CTRL);
    }
    if modifier == 0 {
        return legacy_sequence_for_key(key).contains(&data) || matches_kitty_sequence(data, cp, 0);
    }
    if matches_legacy_modifier_sequence(data, key, modifier) {
        return true;
    }
    matches_kitty_sequence(data, cp, modifier)
}

fn match_printable_key(data: &str, key: &str, modifier: u32) -> bool {
    let mut chars = key.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    if chars.next().is_some() {
        return false; // multi-character key id we don't understand
    }
    let lower = first.to_ascii_lowercase();
    let is_letter = lower.is_ascii_lowercase();
    let is_digit = is_digit_key(lower);
    if !is_letter && !is_digit && !is_symbol_key(lower) {
        return false;
    }
    let codepoint = lower as u32;
    let raw_ctrl = raw_ctrl_char(lower);

    // Legacy ctrl+alt+letter/symbol = ESC + control char.
    if modifier == (MOD_CTRL | MOD_ALT)
        && !is_kitty_protocol_active()
        && let Some(rc) = raw_ctrl
    {
        let mut s = String::with_capacity(2);
        s.push('\x1b');
        s.push(rc);
        if data == s {
            return true;
        }
        // fall through to Kitty / modifyOtherKeys forms
    }

    // Legacy alt+letter/digit = ESC + key.
    if modifier == MOD_ALT && !is_kitty_protocol_active() && (is_letter || is_digit) {
        let mut s = String::with_capacity(2);
        s.push('\x1b');
        s.push(lower);
        if data == s {
            return true;
        }
    }

    if modifier == MOD_CTRL {
        if let Some(rc) = raw_ctrl
            && data.len() == 1
            && data.as_bytes()[0] as u32 == rc as u32
        {
            return true;
        }
        return matches_kitty_sequence(data, codepoint as i32, MOD_CTRL)
            || matches_printable_modify_other_keys(data, codepoint, MOD_CTRL);
    }

    if modifier == (MOD_SHIFT | MOD_CTRL) {
        return matches_kitty_sequence(data, codepoint as i32, MOD_SHIFT | MOD_CTRL)
            || matches_printable_modify_other_keys(data, codepoint, MOD_SHIFT | MOD_CTRL);
    }

    if modifier == MOD_SHIFT {
        if is_letter && data.len() == 1 && data == lower.to_ascii_uppercase().to_string() {
            return true;
        }
        return matches_kitty_sequence(data, codepoint as i32, MOD_SHIFT)
            || matches_printable_modify_other_keys(data, codepoint, MOD_SHIFT);
    }

    if modifier != 0 {
        return matches_kitty_sequence(data, codepoint as i32, modifier)
            || matches_printable_modify_other_keys(data, codepoint, modifier);
    }

    // No modifiers — accept the raw key character or a Kitty CSI-u press.
    (data.len() == 1 && data.starts_with(lower))
        || matches_kitty_sequence(data, codepoint as i32, 0)
}

// =============================================================================
// parse_key_id (TS parseKey equivalent)
// =============================================================================

/// Format a key name with a modifier prefix in the canonical order
/// `shift, ctrl, alt, super`. Returns `None` if the modifier set contains
/// unsupported bits.
fn format_key_name_with_modifiers(key_name: &str, modifier: u32) -> Option<String> {
    let effective = modifier & !LOCK_MASK;
    let supported = MOD_SHIFT | MOD_CTRL | MOD_ALT | MOD_SUPER;
    if (effective & !supported) != 0 {
        return None;
    }
    let mut out = String::new();
    if effective & MOD_SHIFT != 0 {
        out.push_str("shift+");
    }
    if effective & MOD_CTRL != 0 {
        out.push_str("ctrl+");
    }
    if effective & MOD_ALT != 0 {
        out.push_str("alt+");
    }
    if effective & MOD_SUPER != 0 {
        out.push_str("super+");
    }
    out.push_str(key_name);
    Some(out)
}

/// Map an effective codepoint (post functional/shift normalization) to a
/// human-readable key name (for use in canonical KeyId strings).
fn codepoint_to_key_name(cp: i32) -> Option<&'static str> {
    Some(match cp {
        CP_ESCAPE => "escape",
        CP_TAB => "tab",
        CP_ENTER | CP_KP_ENTER => "enter",
        CP_SPACE => "space",
        CP_BACKSPACE => "backspace",
        CP_DELETE => "delete",
        CP_INSERT => "insert",
        CP_HOME => "home",
        CP_END => "end",
        CP_PAGE_UP => "pageUp",
        CP_PAGE_DOWN => "pageDown",
        CP_UP => "up",
        CP_DOWN => "down",
        CP_LEFT => "left",
        CP_RIGHT => "right",
        _ => return None,
    })
}

fn format_parsed_key(
    codepoint: i32,
    modifier: u32,
    base_layout_key: Option<u32>,
) -> Option<String> {
    let normalized_cp = normalize_kitty_functional_codepoint(codepoint);
    let identity_cp = normalize_shifted_letter_identity_codepoint(normalized_cp, modifier);

    let is_latin_letter = (97..=122).contains(&identity_cp);
    let is_digit_cp = (48..=57).contains(&identity_cp);
    let is_known_symbol = char::from_u32(identity_cp as u32)
        .map(is_symbol_key)
        .unwrap_or(false);

    let effective_cp = if is_latin_letter || is_digit_cp || is_known_symbol {
        identity_cp
    } else {
        match base_layout_key {
            Some(b) => b as i32,
            None => identity_cp,
        }
    };

    // Named key first.
    if let Some(name) = codepoint_to_key_name(effective_cp) {
        return format_key_name_with_modifiers(name, modifier);
    }

    // Single-character key.
    if (48..=57).contains(&effective_cp) || (97..=122).contains(&effective_cp) {
        let ch = char::from_u32(effective_cp as u32)?;
        return format_key_name_with_modifiers(&ch.to_string(), modifier);
    }
    if let Some(ch) = char::from_u32(effective_cp as u32)
        && is_symbol_key(ch)
    {
        return format_key_name_with_modifiers(&ch.to_string(), modifier);
    }

    None
}

/// Parse raw input data and return the canonical [`KeyId`] string if
/// recognized. Mirrors TS `parseKey`.
pub fn parse_key_id(data: &str) -> Option<KeyId> {
    if let Some(kitty) = parse_kitty_sequence(data) {
        return format_parsed_key(kitty.codepoint, kitty.modifier, kitty.base_layout_key);
    }

    if let Some(modify) = parse_modify_other_keys(data) {
        return format_parsed_key(modify.codepoint as i32, modify.modifier, None);
    }

    // Mode-aware legacy sequences.
    if is_kitty_protocol_active() && (data == "\x1b\r" || data == "\n") {
        return Some("shift+enter".to_string());
    }

    if let Some(id) = legacy_sequence_key_id(data) {
        return Some(id.to_string());
    }

    if data == "\x1b" {
        return Some("escape".to_string());
    }
    if data == "\x1c" {
        return Some("ctrl+\\".to_string());
    }
    if data == "\x1d" {
        return Some("ctrl+]".to_string());
    }
    if data == "\x1f" {
        return Some("ctrl+-".to_string());
    }
    if data == "\x1b\x1b" {
        return Some("ctrl+alt+[".to_string());
    }
    if data == "\x1b\x1c" {
        return Some("ctrl+alt+\\".to_string());
    }
    if data == "\x1b\x1d" {
        return Some("ctrl+alt+]".to_string());
    }
    if data == "\x1b\x1f" {
        return Some("ctrl+alt+-".to_string());
    }
    if data == "\t" {
        return Some("tab".to_string());
    }
    if data == "\r" || (!is_kitty_protocol_active() && data == "\n") || data == "\x1bOM" {
        return Some("enter".to_string());
    }
    if data == "\x00" {
        return Some("ctrl+space".to_string());
    }
    if data == " " {
        return Some("space".to_string());
    }
    if data == "\x7f" {
        return Some("backspace".to_string());
    }
    if data == "\x08" {
        return Some(if is_windows_terminal_session() {
            "ctrl+backspace".to_string()
        } else {
            "backspace".to_string()
        });
    }
    if data == "\x1b[Z" {
        return Some("shift+tab".to_string());
    }
    if !is_kitty_protocol_active() && data == "\x1b\r" {
        return Some("alt+enter".to_string());
    }
    if !is_kitty_protocol_active() && data == "\x1b " {
        return Some("alt+space".to_string());
    }
    if data == "\x1b\x7f" || data == "\x1b\x08" {
        return Some("alt+backspace".to_string());
    }
    if !is_kitty_protocol_active() && data == "\x1bB" {
        return Some("alt+left".to_string());
    }
    if !is_kitty_protocol_active() && data == "\x1bF" {
        return Some("alt+right".to_string());
    }

    if !is_kitty_protocol_active() && data.len() == 2 {
        let bytes = data.as_bytes();
        if bytes[0] == 0x1b {
            let code = bytes[1];
            if (1..=26).contains(&code) {
                let letter = (code + b'a' - 1) as char;
                return Some(format!("ctrl+alt+{}", letter));
            }
            if (97..=122).contains(&code) || (48..=57).contains(&code) {
                return Some(format!("alt+{}", code as char));
            }
        }
    }

    if data == "\x1b[A" {
        return Some("up".to_string());
    }
    if data == "\x1b[B" {
        return Some("down".to_string());
    }
    if data == "\x1b[C" {
        return Some("right".to_string());
    }
    if data == "\x1b[D" {
        return Some("left".to_string());
    }
    if data == "\x1b[H" || data == "\x1bOH" {
        return Some("home".to_string());
    }
    if data == "\x1b[F" || data == "\x1bOF" {
        return Some("end".to_string());
    }
    if data == "\x1b[3~" {
        return Some("delete".to_string());
    }
    if data == "\x1b[5~" {
        return Some("pageUp".to_string());
    }
    if data == "\x1b[6~" {
        return Some("pageDown".to_string());
    }

    // Single-byte fallbacks.
    if data.len() == 1 {
        let code = data.as_bytes()[0];
        if (1..=26).contains(&code) {
            let letter = (code + b'a' - 1) as char;
            return Some(format!("ctrl+{}", letter));
        }
        if (32..=126).contains(&code) {
            return Some(data.to_string());
        }
    }

    None
}

// =============================================================================
// Kitty CSI-u printable decoding
// =============================================================================

const KITTY_PRINTABLE_ALLOWED_MODIFIERS: u32 = MOD_SHIFT;

/// Decode a Kitty CSI-u sequence into a printable character, if applicable.
/// Mirrors TS `decodeKittyPrintable`.
pub fn decode_kitty_printable(data: &str) -> Option<String> {
    let parsed = parse_kitty_sequence(data)?;
    // Only accept CSI-u (codepoint-based) sequences. The functional/arrow
    // shapes never produce printable text.
    if parsed.codepoint < 0 {
        return None;
    }

    let modifier = parsed.modifier;
    if (modifier & !KITTY_PRINTABLE_ALLOWED_MODIFIERS) != 0 {
        return None;
    }
    if modifier & (MOD_ALT | MOD_CTRL) != 0 {
        return None;
    }

    let mut effective = parsed.codepoint;
    if (modifier & MOD_SHIFT) != 0
        && let Some(s) = parsed.shifted_key
    {
        effective = s as i32;
    }
    effective = normalize_kitty_functional_codepoint(effective);
    if effective < 32 {
        return None;
    }
    let ch = char::from_u32(effective as u32)?;
    Some(ch.to_string())
}

fn decode_modify_other_keys_printable(data: &str) -> Option<String> {
    let parsed = parse_modify_other_keys(data)?;
    let modifier = parsed.modifier & !LOCK_MASK;
    if (modifier & !MOD_SHIFT) != 0 {
        return None;
    }
    if parsed.codepoint < 32 {
        return None;
    }
    let ch = char::from_u32(parsed.codepoint)?;
    Some(ch.to_string())
}

/// Decode a printable character from either a Kitty CSI-u or modifyOtherKeys
/// sequence. Mirrors TS `decodePrintableKey`.
pub fn decode_printable_key(data: &str) -> Option<String> {
    decode_kitty_printable(data).or_else(|| decode_modify_other_keys_printable(data))
}

// =============================================================================
// is_key_release / is_key_repeat
// =============================================================================

const RELEASE_PATTERNS: &[&str] = &[":3u", ":3~", ":3A", ":3B", ":3C", ":3D", ":3H", ":3F"];
const REPEAT_PATTERNS: &[&str] = &[":2u", ":2~", ":2A", ":2B", ":2C", ":2D", ":2H", ":2F"];

/// Return whether the input is a Kitty key-release event (flag 2). Bracketed
/// paste content is excluded so MAC-address-like payloads don't trigger.
pub fn is_key_release(data: &str) -> bool {
    if data.contains("\x1b[200~") {
        return false;
    }
    RELEASE_PATTERNS.iter().any(|p| data.contains(p))
}

/// Return whether the input is a Kitty key-repeat event (flag 2).
pub fn is_key_repeat(data: &str) -> bool {
    if data.contains("\x1b[200~") {
        return false;
    }
    REPEAT_PATTERNS.iter().any(|p| data.contains(p))
}

// =============================================================================
// Structured parse_key
// =============================================================================

/// Parse raw terminal input into a structured [`Key`] event. Always returns a
/// `Key`; unrecognized input becomes [`KeyName::Unknown`]. This is the legacy
/// shape used by built-in components; for canonical KeyId strings, use
/// [`parse_key_id`].
pub fn parse_key(data: &str) -> Key {
    if let Some(kitty) = parse_kitty_sequence(data) {
        return key_from_codepoint(
            kitty.codepoint,
            kitty.modifier,
            kitty.event_type,
            kitty.base_layout_key,
        );
    }

    if let Some(m) = parse_modify_other_keys(data) {
        return key_from_codepoint(m.codepoint as i32, m.modifier, KeyEventType::Press, None);
    }

    // Fall through to the legacy single-/multi-byte handlers below.
    let bytes = data.as_bytes();

    if bytes.is_empty() {
        return unknown_key(data);
    }

    if bytes.len() == 1 {
        return parse_single_byte(bytes[0]);
    }

    if !data.starts_with('\x1b') {
        let ch = data.chars().next().unwrap_or('?');
        return key_simple(KeyName::Char(ch), KeyModifiers::none());
    }

    // ESC + single char (alt+key) — only when Kitty protocol is inactive.
    if bytes.len() == 2 && bytes[0] == 0x1b {
        let second = bytes[1];
        if !is_kitty_protocol_active() {
            return key_simple(KeyName::Char(second as char), KeyModifiers::alt());
        }
    }

    if let Some(rest) = data.strip_prefix("\x1b[") {
        return parse_csi_legacy(rest).unwrap_or_else(|| unknown_key(data));
    }
    if data.starts_with("\x1bO") && bytes.len() == 3 {
        return parse_ss3(bytes[2]);
    }

    unknown_key(data)
}

fn parse_single_byte(byte: u8) -> Key {
    match byte {
        0x0d => key_simple(KeyName::Enter, KeyModifiers::none()),
        0x09 => key_simple(KeyName::Tab, KeyModifiers::none()),
        0x7f => key_simple(KeyName::Backspace, KeyModifiers::none()),
        0x1b => key_simple(KeyName::Escape, KeyModifiers::none()),
        0x00 => key_simple(KeyName::Space, KeyModifiers::ctrl()),
        b @ 1..=0x1a => key_simple(KeyName::Char((b'a' + b - 1) as char), KeyModifiers::ctrl()),
        b if b >= 0x20 => key_simple(KeyName::Char(b as char), KeyModifiers::none()),
        _ => unknown_key(&format!("{:02x}", byte)),
    }
}

fn parse_csi_legacy(seq: &str) -> Option<Key> {
    // Plain arrow / nav.
    let key = match seq {
        "A" => Some(KeyName::Up),
        "B" => Some(KeyName::Down),
        "C" => Some(KeyName::Right),
        "D" => Some(KeyName::Left),
        "H" => Some(KeyName::Home),
        "F" => Some(KeyName::End),
        "2~" => Some(KeyName::Insert),
        "3~" => Some(KeyName::Delete),
        "5~" => Some(KeyName::PageUp),
        "6~" => Some(KeyName::PageDown),
        "Z" => return Some(key_simple(KeyName::Tab, KeyModifiers::shift())),
        _ => None,
    };
    if let Some(name) = key {
        return Some(key_simple(name, KeyModifiers::none()));
    }

    // `1;<mod><letter>` arrow/home/end with modifiers.
    if let Some(rest) = seq.strip_prefix("1;")
        && let Some(last) = rest.chars().last()
        && let Some(mod_str) = rest.get(..rest.len() - last.len_utf8())
        && let Ok(mod_value) = mod_str.parse::<u32>()
    {
        let modifiers = KeyModifiers::from_kitty_bits(mod_value);
        let name = match last {
            'A' => KeyName::Up,
            'B' => KeyName::Down,
            'C' => KeyName::Right,
            'D' => KeyName::Left,
            'H' => KeyName::Home,
            'F' => KeyName::End,
            _ => return None,
        };
        return Some(Key {
            name,
            modifiers,
            is_release: false,
            event_type: KeyEventType::Press,
            base_layout_key: None,
        });
    }

    // `<n>~` extended function keys.
    if let Some(num_str) = seq.strip_suffix('~')
        && let Ok(num) = num_str.parse::<u8>()
    {
        let name = match num {
            15 => KeyName::F(5),
            17 => KeyName::F(6),
            18 => KeyName::F(7),
            19 => KeyName::F(8),
            20 => KeyName::F(9),
            21 => KeyName::F(10),
            23 => KeyName::F(11),
            24 => KeyName::F(12),
            _ => return None,
        };
        return Some(key_simple(name, KeyModifiers::none()));
    }

    None
}

fn parse_ss3(byte: u8) -> Key {
    match byte {
        b'P' => key_simple(KeyName::F(1), KeyModifiers::none()),
        b'Q' => key_simple(KeyName::F(2), KeyModifiers::none()),
        b'R' => key_simple(KeyName::F(3), KeyModifiers::none()),
        b'S' => key_simple(KeyName::F(4), KeyModifiers::none()),
        b'A' => key_simple(KeyName::Up, KeyModifiers::none()),
        b'B' => key_simple(KeyName::Down, KeyModifiers::none()),
        b'C' => key_simple(KeyName::Right, KeyModifiers::none()),
        b'D' => key_simple(KeyName::Left, KeyModifiers::none()),
        b'H' => key_simple(KeyName::Home, KeyModifiers::none()),
        b'F' => key_simple(KeyName::End, KeyModifiers::none()),
        b'M' => key_simple(KeyName::Enter, KeyModifiers::none()),
        other => unknown_key(&format!("\x1bO{}", other as char)),
    }
}

/// Build a [`Key`] from a Kitty-style codepoint plus modifier bitmask. Used
/// by both `parse_kitty_sequence` and `parse_modify_other_keys` paths.
fn key_from_codepoint(
    codepoint: i32,
    modifier: u32,
    event_type: KeyEventType,
    base_layout_key: Option<u32>,
) -> Key {
    let normalized_cp = normalize_kitty_functional_codepoint(codepoint);
    let identity_cp = normalize_shifted_letter_identity_codepoint(normalized_cp, modifier);
    let modifiers = KeyModifiers::from_modifier_mask(modifier & !LOCK_MASK);

    let name = match identity_cp {
        CP_ESCAPE => KeyName::Escape,
        CP_TAB => KeyName::Tab,
        CP_ENTER | CP_KP_ENTER => KeyName::Enter,
        CP_SPACE => KeyName::Space,
        CP_BACKSPACE => KeyName::Backspace,
        CP_DELETE => KeyName::Delete,
        CP_INSERT => KeyName::Insert,
        CP_HOME => KeyName::Home,
        CP_END => KeyName::End,
        CP_PAGE_UP => KeyName::PageUp,
        CP_PAGE_DOWN => KeyName::PageDown,
        CP_UP => KeyName::Up,
        CP_DOWN => KeyName::Down,
        CP_LEFT => KeyName::Left,
        CP_RIGHT => KeyName::Right,
        cp if cp >= 0 => match char::from_u32(cp as u32) {
            Some(ch) => KeyName::Char(ch),
            None => KeyName::Unknown(format!("U+{:04X}", cp)),
        },
        cp => KeyName::Unknown(format!("cp:{}", cp)),
    };

    Key {
        name,
        modifiers,
        is_release: matches!(event_type, KeyEventType::Release),
        event_type,
        base_layout_key,
    }
}

fn key_simple(name: KeyName, modifiers: KeyModifiers) -> Key {
    Key {
        name,
        modifiers,
        is_release: false,
        event_type: KeyEventType::Press,
        base_layout_key: None,
    }
}

fn unknown_key(data: &str) -> Key {
    Key {
        name: KeyName::Unknown(data.to_string()),
        modifiers: KeyModifiers::none(),
        is_release: false,
        event_type: KeyEventType::Press,
        base_layout_key: None,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Test guard ensuring `set_kitty_protocol_active` is reset on drop.
    struct KittyGuard;
    impl KittyGuard {
        fn enable() -> Self {
            set_kitty_protocol_active(true);
            Self
        }
    }
    impl Drop for KittyGuard {
        fn drop(&mut self) {
            set_kitty_protocol_active(false);
        }
    }

    // ---- Existing structured-parse coverage --------------------------------

    #[test]
    fn parse_single_char() {
        let key = parse_key("a");
        assert_eq!(key.name, KeyName::Char('a'));
        assert_eq!(key.modifiers, KeyModifiers::none());
    }

    #[test]
    fn parse_basic_specials() {
        assert_eq!(parse_key("\r").name, KeyName::Enter);
        assert_eq!(parse_key("\t").name, KeyName::Tab);
        assert_eq!(parse_key("\x7f").name, KeyName::Backspace);
        assert_eq!(parse_key("\x1b").name, KeyName::Escape);
    }

    #[test]
    fn parse_ctrl_letter() {
        let key = parse_key("\x03");
        assert_eq!(key.name, KeyName::Char('c'));
        assert!(key.modifiers.ctrl);
    }

    #[test]
    fn parse_arrow_keys_legacy() {
        assert_eq!(parse_key("\x1b[A").name, KeyName::Up);
        assert_eq!(parse_key("\x1b[B").name, KeyName::Down);
        assert_eq!(parse_key("\x1b[C").name, KeyName::Right);
        assert_eq!(parse_key("\x1b[D").name, KeyName::Left);
    }

    #[test]
    fn parse_alt_key_legacy() {
        // Default state: kitty inactive
        let key = parse_key("\x1ba");
        assert_eq!(key.name, KeyName::Char('a'));
        assert!(key.modifiers.alt);
    }

    #[test]
    fn parse_kitty_basic() {
        let key = parse_key("\x1b[97u");
        assert_eq!(key.name, KeyName::Char('a'));
        assert!(!key.is_release);
        assert_eq!(key.event_type, KeyEventType::Press);
    }

    #[test]
    fn parse_kitty_release_event() {
        let key = parse_key("\x1b[97;1:3u");
        assert_eq!(key.name, KeyName::Char('a'));
        assert!(key.is_release);
        assert_eq!(key.event_type, KeyEventType::Release);
    }

    #[test]
    fn parse_kitty_repeat_event() {
        let key = parse_key("\x1b[97;1:2u");
        assert_eq!(key.name, KeyName::Char('a'));
        assert_eq!(key.event_type, KeyEventType::Repeat);
        assert!(!key.is_release);
    }

    #[test]
    fn parse_kitty_with_base_layout_key() {
        // Cyrillic 'с' codepoint 1089, Latin 'c' = 99. base_layout_key should be present.
        let key = parse_key("\x1b[1089::99;5u");
        assert_eq!(key.base_layout_key, Some(99));
        assert!(key.modifiers.ctrl);
    }

    #[test]
    fn parse_kitty_kp_enter_normalized() {
        let key = parse_key("\x1b[57414u");
        assert_eq!(key.name, KeyName::Enter);
    }

    // ---- New: matches_key tests --------------------------------------------

    #[test]
    fn matches_key_basic_letter() {
        assert!(matches_key("a", "a"));
        assert!(!matches_key("b", "a"));
    }

    #[test]
    fn matches_key_legacy_ctrl_c() {
        set_kitty_protocol_active(false);
        assert!(matches_key("\x03", "ctrl+c"));
        assert!(matches_key("\x04", "ctrl+d"));
    }

    #[test]
    fn matches_key_escape() {
        assert!(matches_key("\x1b", "escape"));
        assert!(matches_key("\x1b", "esc"));
    }

    #[test]
    fn matches_key_kitty_super_combinations() {
        let _g = KittyGuard::enable();
        assert!(matches_key("\x1b[107;9u", "super+k"));
        assert!(matches_key("\x1b[13;9u", "super+enter"));
        assert!(matches_key("\x1b[107;13u", "ctrl+super+k"));
        assert!(matches_key("\x1b[107;14u", "ctrl+shift+super+k"));
        assert!(!matches_key("\x1b[107;13u", "super+k"));
    }

    #[test]
    fn matches_key_kitty_ctrl_shift_letter() {
        let _g = KittyGuard::enable();
        // ctrl+shift+p — Cyrillic-aware base layout match
        let cyrillic_ctrl_shift_p = "\x1b[1079::112;6u";
        assert!(matches_key(cyrillic_ctrl_shift_p, "ctrl+shift+p"));
        // Order independence
        assert!(matches_key(cyrillic_ctrl_shift_p, "shift+ctrl+p"));
    }

    #[test]
    fn matches_key_kitty_base_layout_non_latin() {
        let _g = KittyGuard::enable();
        let cyrillic_ctrl_c = "\x1b[1089::99;5u";
        assert!(matches_key(cyrillic_ctrl_c, "ctrl+c"));
        assert!(!matches_key(cyrillic_ctrl_c, "ctrl+d"));
        assert!(!matches_key(cyrillic_ctrl_c, "ctrl+shift+c"));
    }

    #[test]
    fn matches_key_kitty_dvorak_codepoint_authoritative() {
        let _g = KittyGuard::enable();
        // Dvorak Ctrl+K reports codepoint 'k' but base layout 'v' — codepoint wins.
        assert!(matches_key("\x1b[107::118;5u", "ctrl+k"));
        assert!(!matches_key("\x1b[107::118;5u", "ctrl+v"));
    }

    #[test]
    fn matches_key_kitty_digit() {
        let _g = KittyGuard::enable();
        assert!(matches_key("\x1b[49u", "1"));
        assert!(matches_key("\x1b[49;5u", "ctrl+1"));
        assert!(!matches_key("\x1b[49;5u", "ctrl+2"));
    }

    #[test]
    fn matches_key_kitty_keypad_normalization() {
        let _g = KittyGuard::enable();
        assert!(matches_key("\x1b[57400u", "1"));
        assert!(matches_key("\x1b[57410u", "/"));
        assert!(matches_key("\x1b[57417u", "left"));
        assert!(matches_key("\x1b[57426u", "delete"));
    }

    #[test]
    fn matches_key_kitty_release_still_matches() {
        let _g = KittyGuard::enable();
        let release = "\x1b[1089::99;5:3u";
        assert!(matches_key(release, "ctrl+c"));
    }

    #[test]
    fn matches_key_modify_other_keys_letters() {
        set_kitty_protocol_active(false);
        assert!(matches_key("\x1b[27;5;99~", "ctrl+c"));
        assert!(matches_key("\x1b[27;5;100~", "ctrl+d"));
        assert!(matches_key("\x1b[27;5;122~", "ctrl+z"));
    }

    #[test]
    fn matches_key_modify_other_keys_enter_variants() {
        set_kitty_protocol_active(false);
        assert!(matches_key("\x1b[27;5;13~", "ctrl+enter"));
        assert!(matches_key("\x1b[27;2;13~", "shift+enter"));
        assert!(matches_key("\x1b[27;3;13~", "alt+enter"));
    }

    #[test]
    fn matches_key_modify_other_keys_tab_variants() {
        set_kitty_protocol_active(false);
        assert!(matches_key("\x1b[27;2;9~", "shift+tab"));
        assert!(matches_key("\x1b[27;5;9~", "ctrl+tab"));
        assert!(matches_key("\x1b[27;3;9~", "alt+tab"));
    }

    #[test]
    fn matches_key_modify_other_keys_backspace_variants() {
        set_kitty_protocol_active(false);
        assert!(matches_key("\x1b[27;1;127~", "backspace"));
        assert!(matches_key("\x1b[27;5;127~", "ctrl+backspace"));
        assert!(matches_key("\x1b[27;3;127~", "alt+backspace"));
    }

    #[test]
    fn matches_key_modify_other_keys_shifted_uppercase() {
        set_kitty_protocol_active(false);
        assert!(matches_key("\x1b[27;2;69~", "shift+e"));
        assert!(matches_key("\x1b[27;6;69~", "ctrl+shift+e"));
    }

    #[test]
    fn matches_key_modify_other_keys_ctrl_alt_letter() {
        set_kitty_protocol_active(false);
        assert!(matches_key("\x1b[104;7u", "ctrl+alt+h"));
        assert!(matches_key("\x1b[27;7;104~", "ctrl+alt+h"));
    }

    #[test]
    fn matches_key_legacy_arrows_and_ss3() {
        assert!(matches_key("\x1b[A", "up"));
        assert!(matches_key("\x1b[B", "down"));
        assert!(matches_key("\x1b[C", "right"));
        assert!(matches_key("\x1b[D", "left"));
        assert!(matches_key("\x1bOA", "up"));
        assert!(matches_key("\x1bOH", "home"));
        assert!(matches_key("\x1bOF", "end"));
    }

    #[test]
    fn matches_key_function_keys_f1_to_f12() {
        assert!(matches_key("\x1bOP", "f1"));
        assert!(matches_key("\x1bOQ", "f2"));
        assert!(matches_key("\x1bOR", "f3"));
        assert!(matches_key("\x1bOS", "f4"));
        assert!(matches_key("\x1b[15~", "f5"));
        assert!(matches_key("\x1b[17~", "f6"));
        assert!(matches_key("\x1b[18~", "f7"));
        assert!(matches_key("\x1b[19~", "f8"));
        assert!(matches_key("\x1b[20~", "f9"));
        assert!(matches_key("\x1b[21~", "f10"));
        assert!(matches_key("\x1b[23~", "f11"));
        assert!(matches_key("\x1b[24~", "f12"));
        assert!(!matches_key("\x1b[24~", "ctrl+f12"));
    }

    #[test]
    fn matches_key_legacy_ctrl_symbols() {
        set_kitty_protocol_active(false);
        assert!(matches_key("\x1c", "ctrl+\\"));
        assert!(matches_key("\x1d", "ctrl+]"));
        assert!(matches_key("\x1f", "ctrl+_"));
        assert!(matches_key("\x1f", "ctrl+-"));
    }

    #[test]
    fn matches_key_legacy_ctrl_alt_symbols() {
        set_kitty_protocol_active(false);
        assert!(matches_key("\x1b\x1b", "ctrl+alt+["));
        assert!(matches_key("\x1b\x1c", "ctrl+alt+\\"));
        assert!(matches_key("\x1b\x1d", "ctrl+alt+]"));
        assert!(matches_key("\x1b\x1f", "ctrl+alt+-"));
    }

    #[test]
    fn matches_key_alt_arrows() {
        set_kitty_protocol_active(false);
        assert!(matches_key("\x1bp", "alt+up"));
        assert!(matches_key("\x1bn", "alt+down"));
        assert!(matches_key("\x1bb", "alt+left"));
        assert!(matches_key("\x1bf", "alt+right"));
        assert!(!matches_key("\x1bp", "up"));
    }

    #[test]
    fn matches_key_rxvt_modifier_arrows() {
        assert!(matches_key("\x1b[a", "shift+up"));
        assert!(matches_key("\x1bOa", "ctrl+up"));
        assert!(matches_key("\x1b[2$", "shift+insert"));
        assert!(matches_key("\x1b[2^", "ctrl+insert"));
        assert!(matches_key("\x1b[7$", "shift+home"));
    }

    #[test]
    fn matches_key_modifier_order_independence() {
        let _g = KittyGuard::enable();
        let data = "\x1b[107;13u"; // ctrl+super+k
        assert!(matches_key(data, "ctrl+super+k"));
        assert!(matches_key(data, "super+ctrl+k"));
    }

    #[test]
    fn matches_key_unknown_modifier_silently_ignored() {
        // Unknown modifier names are stripped; remaining base key still matches.
        assert!(matches_key("a", "fakemod+a"));
        // Typo'd modifier is treated as unknown; base key "a" still matches.
        assert!(matches_key("a", "ctrll+a"));
        // Empty key id remains rejected.
        assert!(!matches_key("a", ""));
    }

    #[test]
    fn parse_key_id_literal_plus_under_shift() {
        // `shift++` round-trips: `format_key_name_with_modifiers("+", MOD_SHIFT)`
        // produces it, so parse must accept it.
        let parsed = parse_key_id_components("shift++").unwrap();
        assert_eq!(parsed.key, "+");
        assert_eq!(parsed.modifier, MOD_SHIFT);
    }

    #[test]
    fn parse_key_id_plain_plus() {
        let parsed = parse_key_id_components("+").unwrap();
        assert_eq!(parsed.key, "+");
        assert_eq!(parsed.modifier, 0);
    }

    // ---- New: parse_key_id (TS parseKey) tests -----------------------------

    #[test]
    fn parse_key_id_specials_and_letters() {
        set_kitty_protocol_active(false);
        assert_eq!(parse_key_id("\x1b").as_deref(), Some("escape"));
        assert_eq!(parse_key_id("\t").as_deref(), Some("tab"));
        assert_eq!(parse_key_id("\r").as_deref(), Some("enter"));
        assert_eq!(parse_key_id("\n").as_deref(), Some("enter"));
        assert_eq!(parse_key_id("\x00").as_deref(), Some("ctrl+space"));
        assert_eq!(parse_key_id(" ").as_deref(), Some("space"));
        assert_eq!(parse_key_id("1").as_deref(), Some("1"));
        assert_eq!(parse_key_id("\x03").as_deref(), Some("ctrl+c"));
    }

    #[test]
    fn parse_key_id_kitty_super_combinations() {
        let _g = KittyGuard::enable();
        assert_eq!(parse_key_id("\x1b[107;9u").as_deref(), Some("super+k"));
        assert_eq!(parse_key_id("\x1b[13;9u").as_deref(), Some("super+enter"));
        assert_eq!(
            parse_key_id("\x1b[107;13u").as_deref(),
            Some("ctrl+super+k")
        );
        assert_eq!(
            parse_key_id("\x1b[107;14u").as_deref(),
            Some("shift+ctrl+super+k")
        );
    }

    #[test]
    fn parse_key_id_kitty_keypad_to_logical() {
        let _g = KittyGuard::enable();
        assert_eq!(parse_key_id("\x1b[57399u").as_deref(), Some("0"));
        assert_eq!(parse_key_id("\x1b[57409u").as_deref(), Some("."));
        assert_eq!(parse_key_id("\x1b[57413u").as_deref(), Some("+"));
        assert_eq!(parse_key_id("\x1b[57416u").as_deref(), Some(","));
        assert_eq!(parse_key_id("\x1b[57417u").as_deref(), Some("left"));
        assert_eq!(parse_key_id("\x1b[57418u").as_deref(), Some("right"));
        assert_eq!(parse_key_id("\x1b[57419u").as_deref(), Some("up"));
        assert_eq!(parse_key_id("\x1b[57420u").as_deref(), Some("down"));
        assert_eq!(parse_key_id("\x1b[57421u").as_deref(), Some("pageUp"));
        assert_eq!(parse_key_id("\x1b[57422u").as_deref(), Some("pageDown"));
        assert_eq!(parse_key_id("\x1b[57423u").as_deref(), Some("home"));
        assert_eq!(parse_key_id("\x1b[57424u").as_deref(), Some("end"));
        assert_eq!(parse_key_id("\x1b[57425u").as_deref(), Some("insert"));
        assert_eq!(parse_key_id("\x1b[57426u").as_deref(), Some("delete"));
    }

    #[test]
    fn parse_key_id_modify_other_keys() {
        set_kitty_protocol_active(false);
        assert_eq!(parse_key_id("\x1b[27;5;99~").as_deref(), Some("ctrl+c"));
        assert_eq!(parse_key_id("\x1b[27;5;13~").as_deref(), Some("ctrl+enter"));
        assert_eq!(
            parse_key_id("\x1b[27;2;13~").as_deref(),
            Some("shift+enter")
        );
        assert_eq!(parse_key_id("\x1b[27;1;127~").as_deref(), Some("backspace"));
    }

    #[test]
    fn parse_key_id_kitty_unsupported_modifier_rejected() {
        let _g = KittyGuard::enable();
        assert_eq!(parse_key_id("\x1b[99;17u"), None);
    }

    #[test]
    fn parse_key_id_dvorak_codepoint_authoritative() {
        let _g = KittyGuard::enable();
        assert_eq!(parse_key_id("\x1b[107::118;5u").as_deref(), Some("ctrl+k"));
        assert_eq!(parse_key_id("\x1b[47::91;5u").as_deref(), Some("ctrl+/"));
    }

    #[test]
    fn parse_key_id_legacy_function_keys() {
        assert_eq!(parse_key_id("\x1bOP").as_deref(), Some("f1"));
        assert_eq!(parse_key_id("\x1b[24~").as_deref(), Some("f12"));
        assert_eq!(parse_key_id("\x1b[E").as_deref(), Some("clear"));
        assert_eq!(parse_key_id("\x1b[2^").as_deref(), Some("ctrl+insert"));
    }

    #[test]
    fn parse_key_id_double_bracket_pageup() {
        assert_eq!(parse_key_id("\x1b[[5~").as_deref(), Some("pageUp"));
    }

    #[test]
    fn parse_key_id_alt_legacy_when_kitty_inactive() {
        set_kitty_protocol_active(false);
        assert_eq!(parse_key_id("\x1b ").as_deref(), Some("alt+space"));
        assert_eq!(parse_key_id("\x1b\x08").as_deref(), Some("alt+backspace"));
        assert_eq!(parse_key_id("\x1b\x03").as_deref(), Some("ctrl+alt+c"));
        assert_eq!(parse_key_id("\x1bB").as_deref(), Some("alt+left"));
        assert_eq!(parse_key_id("\x1ba").as_deref(), Some("alt+a"));
        assert_eq!(parse_key_id("\x1b1").as_deref(), Some("alt+1"));
    }

    #[test]
    fn parse_key_id_alt_legacy_suppressed_when_kitty_active() {
        let _g = KittyGuard::enable();
        assert_eq!(parse_key_id("\x1b "), None);
        assert_eq!(parse_key_id("\x1b\x03"), None);
        assert_eq!(parse_key_id("\x1bB"), None);
        assert_eq!(parse_key_id("\x1ba"), None);
        // alt+backspace is unambiguous in both modes.
        assert_eq!(parse_key_id("\x1b\x08").as_deref(), Some("alt+backspace"));
    }

    #[test]
    fn parse_key_id_kitty_active_linefeed_is_shift_enter() {
        let _g = KittyGuard::enable();
        assert_eq!(parse_key_id("\n").as_deref(), Some("shift+enter"));
        assert!(matches_key("\n", "shift+enter"));
        assert!(!matches_key("\n", "enter"));
    }

    #[test]
    fn parse_key_id_shifted_uppercase_letter() {
        let _g = KittyGuard::enable();
        assert_eq!(parse_key_id("\x1b[69;2u").as_deref(), Some("shift+e"));
    }

    // ---- New: decode_kitty_printable / decode_printable_key ----------------

    #[test]
    fn decode_kitty_printable_keypad() {
        assert_eq!(decode_kitty_printable("\x1b[57399u").as_deref(), Some("0"));
        assert_eq!(decode_kitty_printable("\x1b[57400u").as_deref(), Some("1"));
        assert_eq!(decode_kitty_printable("\x1b[57409u").as_deref(), Some("."));
        assert_eq!(decode_kitty_printable("\x1b[57410u").as_deref(), Some("/"));
        assert_eq!(decode_kitty_printable("\x1b[57411u").as_deref(), Some("*"));
        assert_eq!(decode_kitty_printable("\x1b[57412u").as_deref(), Some("-"));
        assert_eq!(decode_kitty_printable("\x1b[57413u").as_deref(), Some("+"));
        assert_eq!(decode_kitty_printable("\x1b[57415u").as_deref(), Some("="));
        assert_eq!(decode_kitty_printable("\x1b[57416u").as_deref(), Some(","));
        // Arrow keypad should not decode as printable.
        assert_eq!(decode_kitty_printable("\x1b[57417u"), None);
    }

    #[test]
    fn decode_kitty_printable_rejects_ctrl_alt() {
        // ctrl+a should not decode as printable
        assert_eq!(decode_kitty_printable("\x1b[97;5u"), None);
        // alt+a likewise
        assert_eq!(decode_kitty_printable("\x1b[97;3u"), None);
    }

    #[test]
    fn decode_printable_key_modify_other_keys() {
        assert_eq!(decode_printable_key("\x1b[27;2;69~").as_deref(), Some("E"));
        assert_eq!(decode_printable_key("\x1b[27;2;196~").as_deref(), Some("Ä"));
        assert_eq!(decode_printable_key("\x1b[27;2;32~").as_deref(), Some(" "));
        assert_eq!(decode_printable_key("\x1b[27;2;13~"), None);
        assert_eq!(decode_printable_key("\x1b[27;6;69~"), None);
    }

    // ---- New: is_key_release / is_key_repeat -------------------------------

    #[test]
    fn is_key_release_detects_csi_u_release() {
        assert!(is_key_release("\x1b[97;1:3u"));
        assert!(is_key_release("\x1b[1;2:3A"));
        assert!(!is_key_release("\x1b[97u"));
    }

    #[test]
    fn is_key_repeat_detects_csi_u_repeat() {
        assert!(is_key_repeat("\x1b[97;1:2u"));
        assert!(is_key_repeat("\x1b[1;2:2C"));
        assert!(!is_key_repeat("\x1b[97u"));
    }

    #[test]
    fn is_key_release_excludes_bracketed_paste() {
        // Bracketed paste payload that happens to contain ":3F" must not be
        // misclassified as a release event.
        let pasted = "\x1b[200~90:62:3F:A5:00:01\x1b[201~";
        assert!(!is_key_release(pasted));
        assert!(!is_key_repeat(pasted));
    }

    // ---- New: kitty protocol active flag -----------------------------------

    #[test]
    fn kitty_protocol_active_toggle() {
        let prev = is_kitty_protocol_active();
        set_kitty_protocol_active(true);
        assert!(is_kitty_protocol_active());
        set_kitty_protocol_active(false);
        assert!(!is_kitty_protocol_active());
        set_kitty_protocol_active(prev);
    }

    // ---- New: backspace handling under WT_SESSION --------------------------
    //
    // Note: these tests mutate `WT_SESSION` and run serially via `--test-threads=1`
    // which is the cargo default for `--lib`. The `KittyGuard`-style scoping
    // ensures the env var is restored even on panic.

    struct EnvGuard {
        name: &'static str,
        prev: Option<std::ffi::OsString>,
    }
    impl EnvGuard {
        fn set(name: &'static str, value: Option<&str>) -> Self {
            let prev = std::env::var_os(name);
            // SAFETY: tests in this module run serially; we restore on Drop.
            unsafe {
                match value {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
            Self { name, prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see above.
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var(self.name, v),
                    None => std::env::remove_var(self.name),
                }
            }
        }
    }

    #[test]
    fn windows_terminal_raw_backspace_when_local() {
        set_kitty_protocol_active(false);
        let _wt = EnvGuard::set("WT_SESSION", Some("test-session"));
        let _ssh1 = EnvGuard::set("SSH_CONNECTION", None);
        let _ssh2 = EnvGuard::set("SSH_CLIENT", None);
        let _ssh3 = EnvGuard::set("SSH_TTY", None);
        assert!(matches_key("\x08", "ctrl+backspace"));
        assert!(!matches_key("\x08", "backspace"));
        assert_eq!(parse_key_id("\x08").as_deref(), Some("ctrl+backspace"));
        // ctrl+h shares the legacy code with raw backspace.
        assert!(matches_key("\x08", "ctrl+h"));
    }

    #[test]
    fn windows_terminal_raw_backspace_over_ssh_is_plain() {
        set_kitty_protocol_active(false);
        let _wt = EnvGuard::set("WT_SESSION", Some("test-session"));
        let _ssh1 = EnvGuard::set("SSH_CONNECTION", Some("1 2 3 4"));
        let _ssh2 = EnvGuard::set("SSH_CLIENT", Some("1 2 3"));
        let _ssh3 = EnvGuard::set("SSH_TTY", Some("/dev/pts/1"));
        assert!(matches_key("\x08", "backspace"));
        assert!(!matches_key("\x08", "ctrl+backspace"));
    }

    #[test]
    fn raw_backspace_outside_windows_terminal() {
        set_kitty_protocol_active(false);
        let _wt = EnvGuard::set("WT_SESSION", None);
        assert!(matches_key("\x7f", "backspace"));
        assert!(!matches_key("\x7f", "ctrl+backspace"));
        assert!(matches_key("\x08", "backspace"));
        assert!(matches_key("\x08", "ctrl+h"));
    }

    // ---- New: KeyId parse roundtrip (modifier ordering) --------------------

    #[test]
    fn key_id_roundtrip_via_parse_key_id() {
        let _g = KittyGuard::enable();
        // Generate a KeyId from input, then check matches_key accepts it back.
        let inputs = [
            ("\x1b[107;9u", "super+k"),
            ("\x1b[13;9u", "super+enter"),
            ("\x1b[107;13u", "ctrl+super+k"),
            ("\x1b[107;14u", "shift+ctrl+super+k"),
            ("\x1b[97u", "a"),
            ("\x1b[49;5u", "ctrl+1"),
        ];
        for (data, expected) in inputs {
            let id = parse_key_id(data).unwrap_or_else(|| panic!("no parse for {:?}", data));
            assert_eq!(id, expected, "parse_key_id mismatch for {:?}", data);
            assert!(
                matches_key(data, &id),
                "matches_key roundtrip failed for {:?}",
                data
            );
        }
    }
}
