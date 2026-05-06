//! Keyboard input parsing.
//!
//! Supports standard escape sequences and Kitty keyboard protocol.

use std::fmt;

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

    /// Parse Kitty modifier bitmask.
    pub fn from_kitty_bits(bits: u32) -> Self {
        let bits = bits.saturating_sub(1); // Kitty uses 1-based
        Self {
            shift: bits & 1 != 0,
            alt: bits & 2 != 0,
            ctrl: bits & 4 != 0,
            super_key: bits & 8 != 0,
        }
    }
}

/// Parsed key event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub name: KeyName,
    pub modifiers: KeyModifiers,
    pub is_release: bool,
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
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
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
            KeyName::Up => write!(f, "Up"),
            KeyName::Down => write!(f, "Down"),
            KeyName::Left => write!(f, "Left"),
            KeyName::Right => write!(f, "Right"),
            KeyName::Home => write!(f, "Home"),
            KeyName::End => write!(f, "End"),
            KeyName::PageUp => write!(f, "PageUp"),
            KeyName::PageDown => write!(f, "PageDown"),
            KeyName::Insert => write!(f, "Insert"),
            KeyName::F(n) => write!(f, "F{}", n),
            KeyName::Unknown(s) => write!(f, "Unknown({})", s),
        }
    }
}

/// Parse raw terminal input into a Key event.
pub fn parse_key(data: &str) -> Key {
    let bytes = data.as_bytes();

    // Single character
    if bytes.len() == 1 {
        return match bytes[0] {
            0x0d => Key {
                name: KeyName::Enter,
                modifiers: KeyModifiers::none(),
                is_release: false,
            },
            0x09 => Key {
                name: KeyName::Tab,
                modifiers: KeyModifiers::none(),
                is_release: false,
            },
            0x7f => Key {
                name: KeyName::Backspace,
                modifiers: KeyModifiers::none(),
                is_release: false,
            },
            0x1b => Key {
                name: KeyName::Escape,
                modifiers: KeyModifiers::none(),
                is_release: false,
            },
            // Ctrl+A through Ctrl+Z
            b if b <= 0x1a => Key {
                name: KeyName::Char((b'a' + b - 1) as char),
                modifiers: KeyModifiers::ctrl(),
                is_release: false,
            },
            b if b >= 0x20 => Key {
                name: KeyName::Char(b as char),
                modifiers: KeyModifiers::none(),
                is_release: false,
            },
            _ => Key {
                name: KeyName::Unknown(format!("{:02x}", bytes[0])),
                modifiers: KeyModifiers::none(),
                is_release: false,
            },
        };
    }

    // Multi-byte: could be UTF-8 character
    if !data.starts_with('\x1b') {
        let ch = data.chars().next().unwrap_or('?');
        return Key {
            name: KeyName::Char(ch),
            modifiers: KeyModifiers::none(),
            is_release: false,
        };
    }

    // ESC + single char = Alt+key
    if bytes.len() == 2 && bytes[0] == 0x1b {
        return Key {
            name: KeyName::Char(bytes[1] as char),
            modifiers: KeyModifiers::alt(),
            is_release: false,
        };
    }

    // CSI sequences: ESC [ ...
    if let Some(csi_body) = data.strip_prefix("\x1b[") {
        return parse_csi_sequence(csi_body);
    }

    // SS3 sequences: ESC O ...
    if data.starts_with("\x1bO") && bytes.len() == 3 {
        return match bytes[2] {
            b'P' => Key {
                name: KeyName::F(1),
                modifiers: KeyModifiers::none(),
                is_release: false,
            },
            b'Q' => Key {
                name: KeyName::F(2),
                modifiers: KeyModifiers::none(),
                is_release: false,
            },
            b'R' => Key {
                name: KeyName::F(3),
                modifiers: KeyModifiers::none(),
                is_release: false,
            },
            b'S' => Key {
                name: KeyName::F(4),
                modifiers: KeyModifiers::none(),
                is_release: false,
            },
            _ => Key {
                name: KeyName::Unknown(data.to_string()),
                modifiers: KeyModifiers::none(),
                is_release: false,
            },
        };
    }

    Key {
        name: KeyName::Unknown(data.to_string()),
        modifiers: KeyModifiers::none(),
        is_release: false,
    }
}

/// Parse a CSI sequence (after ESC [).
fn parse_csi_sequence(seq: &str) -> Key {
    // Kitty protocol: CSI <number> ; <modifiers> ; <event_type> u
    if seq.ends_with('u') {
        return parse_kitty_sequence(seq);
    }

    // Arrow keys and navigation
    match seq {
        "A" => return key(KeyName::Up),
        "B" => return key(KeyName::Down),
        "C" => return key(KeyName::Right),
        "D" => return key(KeyName::Left),
        "H" => return key(KeyName::Home),
        "F" => return key(KeyName::End),
        "2~" => return key(KeyName::Insert),
        "3~" => return key(KeyName::Delete),
        "5~" => return key(KeyName::PageUp),
        "6~" => return key(KeyName::PageDown),
        _ => {}
    }

    // Arrow keys with modifiers: CSI 1 ; <mod> <letter>
    if seq.len() >= 3 && seq.starts_with("1;") {
        let parts: Vec<&str> = seq.splitn(3, ';').collect();
        if parts.len() >= 2 {
            let last = parts.last().unwrap();
            let mod_char = &last[..last.len().saturating_sub(1)];
            let key_char = last.chars().last().unwrap_or('?');
            let mods = mod_char
                .parse::<u32>()
                .map(KeyModifiers::from_kitty_bits)
                .unwrap_or_default();

            let name = match key_char {
                'A' => KeyName::Up,
                'B' => KeyName::Down,
                'C' => KeyName::Right,
                'D' => KeyName::Left,
                'H' => KeyName::Home,
                'F' => KeyName::End,
                _ => KeyName::Unknown(seq.to_string()),
            };

            return Key {
                name,
                modifiers: mods,
                is_release: false,
            };
        }
    }

    // F keys: CSI <n> ~
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
            _ => KeyName::Unknown(seq.to_string()),
        };
        return Key {
            name,
            modifiers: KeyModifiers::none(),
            is_release: false,
        };
    }

    Key {
        name: KeyName::Unknown(seq.to_string()),
        modifiers: KeyModifiers::none(),
        is_release: false,
    }
}

/// Parse Kitty keyboard protocol sequence.
fn parse_kitty_sequence(seq: &str) -> Key {
    let inner = &seq[..seq.len() - 1]; // Remove trailing 'u'
    let parts: Vec<&str> = inner.split(';').collect();

    let codepoint = parts
        .first()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    let modifiers = parts
        .get(1)
        .and_then(|s| {
            // May contain event type after ':'
            let mod_str = s.split(':').next().unwrap_or(s);
            mod_str.parse::<u32>().ok()
        })
        .map(KeyModifiers::from_kitty_bits)
        .unwrap_or_default();

    let event_type = parts
        .get(1)
        .and_then(|s| s.split(':').nth(1))
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1); // 1 = press

    let is_release = event_type == 3;

    let name = match codepoint {
        13 => KeyName::Enter,
        9 => KeyName::Tab,
        127 => KeyName::Backspace,
        27 => KeyName::Escape,
        cp if (32..=126).contains(&cp) => KeyName::Char(cp as u8 as char),
        cp => {
            if let Some(ch) = char::from_u32(cp) {
                KeyName::Char(ch)
            } else {
                KeyName::Unknown(format!("U+{:04X}", cp))
            }
        }
    };

    Key {
        name,
        modifiers,
        is_release,
    }
}

fn key(name: KeyName) -> Key {
    Key {
        name,
        modifiers: KeyModifiers::none(),
        is_release: false,
    }
}

/// Check if input matches a specific key.
pub fn matches_key(data: &str, name: KeyName, modifiers: KeyModifiers) -> bool {
    let key = parse_key(data);
    key.name == name && key.modifiers == modifiers && !key.is_release
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_char() {
        let key = parse_key("a");
        assert_eq!(key.name, KeyName::Char('a'));
        assert_eq!(key.modifiers, KeyModifiers::none());
    }

    #[test]
    fn test_parse_enter() {
        let key = parse_key("\r");
        assert_eq!(key.name, KeyName::Enter);
    }

    #[test]
    fn test_parse_tab() {
        let key = parse_key("\t");
        assert_eq!(key.name, KeyName::Tab);
    }

    #[test]
    fn test_parse_backspace() {
        let key = parse_key("\x7f");
        assert_eq!(key.name, KeyName::Backspace);
    }

    #[test]
    fn test_parse_escape() {
        let key = parse_key("\x1b");
        assert_eq!(key.name, KeyName::Escape);
    }

    #[test]
    fn test_parse_ctrl_c() {
        let key = parse_key("\x03");
        assert_eq!(key.name, KeyName::Char('c'));
        assert!(key.modifiers.ctrl);
    }

    #[test]
    fn test_parse_arrow_keys() {
        assert_eq!(parse_key("\x1b[A").name, KeyName::Up);
        assert_eq!(parse_key("\x1b[B").name, KeyName::Down);
        assert_eq!(parse_key("\x1b[C").name, KeyName::Right);
        assert_eq!(parse_key("\x1b[D").name, KeyName::Left);
    }

    #[test]
    fn test_parse_home_end() {
        assert_eq!(parse_key("\x1b[H").name, KeyName::Home);
        assert_eq!(parse_key("\x1b[F").name, KeyName::End);
    }

    #[test]
    fn test_parse_page_up_down() {
        assert_eq!(parse_key("\x1b[5~").name, KeyName::PageUp);
        assert_eq!(parse_key("\x1b[6~").name, KeyName::PageDown);
    }

    #[test]
    fn test_parse_delete() {
        assert_eq!(parse_key("\x1b[3~").name, KeyName::Delete);
    }

    #[test]
    fn test_parse_alt_key() {
        let key = parse_key("\x1ba");
        assert_eq!(key.name, KeyName::Char('a'));
        assert!(key.modifiers.alt);
    }

    #[test]
    fn test_parse_f_keys_ss3() {
        assert_eq!(parse_key("\x1bOP").name, KeyName::F(1));
        assert_eq!(parse_key("\x1bOQ").name, KeyName::F(2));
        assert_eq!(parse_key("\x1bOR").name, KeyName::F(3));
        assert_eq!(parse_key("\x1bOS").name, KeyName::F(4));
    }

    #[test]
    fn test_parse_kitty_basic() {
        // Kitty: 'a' key press
        let key = parse_key("\x1b[97u");
        assert_eq!(key.name, KeyName::Char('a'));
        assert!(!key.is_release);
    }

    #[test]
    fn test_parse_kitty_ctrl() {
        // Kitty: Ctrl+a (97;5u means codepoint 97, modifiers 5 = ctrl)
        let key = parse_key("\x1b[97;5u");
        assert_eq!(key.name, KeyName::Char('a'));
        assert!(key.modifiers.ctrl);
    }

    #[test]
    fn test_parse_kitty_release() {
        // Kitty: 'a' release (event type 3)
        let key = parse_key("\x1b[97;1:3u");
        assert_eq!(key.name, KeyName::Char('a'));
        assert!(key.is_release);
    }

    #[test]
    fn test_matches_key() {
        assert!(matches_key("a", KeyName::Char('a'), KeyModifiers::none()));
        assert!(!matches_key("b", KeyName::Char('a'), KeyModifiers::none()));
        assert!(matches_key(
            "\x03",
            KeyName::Char('c'),
            KeyModifiers::ctrl()
        ));
    }

    #[test]
    fn test_modifier_from_kitty_bits() {
        let mods = KeyModifiers::from_kitty_bits(5); // 4 = ctrl (1-based: 5-1=4)
        assert!(mods.ctrl);
        assert!(!mods.shift);
        assert!(!mods.alt);

        let mods = KeyModifiers::from_kitty_bits(2); // 1 = shift
        assert!(mods.shift);
        assert!(!mods.ctrl);

        let mods = KeyModifiers::from_kitty_bits(3); // 2 = alt
        assert!(mods.alt);
    }

    #[test]
    fn test_unicode_input() {
        let key = parse_key("你");
        assert_eq!(key.name, KeyName::Char('你'));
    }
}
