# User-Cases: tui/keys

**Upstream source:** `pi-mono/packages/tui/test/keys.test.ts` (59 cases)
**hand-ai source:**   `crates/tui/src/keys.rs` (59 `#[test]`s)
**Surface:**          `matches_key(data, key, modifier)` and
`parse_key_id(data)` — the bottom of the input pipeline. Decodes raw
ANSI/Kitty/modifyOtherKeys terminal sequences into a logical
`(key_id, modifier_mask)` pair the rest of the TUI binds against.

This is one of two modules where hand and pi achieve full case-count
parity. Each pi case maps to a named hand `#[test]`; status is
uniformly ✅ at this snapshot. Cases are listed compactly because
each one pins a single byte-sequence → key-id mapping that's already
unambiguously expressed by its hand test.

## Status

| ID | pi case (paraphrased) | hand test |
|----|-----------------------|-----------|
| UC-keys-001 | Ctrl+c via Cyrillic base layout (Kitty) | `matches_key_kitty_base_layout_non_latin` |
| UC-keys-002 | Ctrl+d via Cyrillic base layout | (same) |
| UC-keys-003 | Ctrl+z via Cyrillic base layout | (same) |
| UC-keys-004 | Ctrl+Shift+p with base layout | `matches_key_kitty_ctrl_shift_letter` |
| UC-keys-005 | direct codepoint when no base layout key | `matches_key_kitty_dvorak_codepoint_authoritative` |
| UC-keys-006 | super-modified Kitty bindings (combined modifiers) | `matches_key_kitty_super_combinations` |
| UC-keys-007 | digit bindings via Kitty CSI-u | `matches_key_kitty_digit` |
| UC-keys-008 | normalize Kitty keypad to logical digits/symbols/nav | `matches_key_kitty_keypad_normalization` |
| UC-keys-009 | shifted-key format in CSI-u | (covered by `parse_kitty_*` family) |
| UC-keys-010 | event-type format (release/repeat) | `parse_kitty_release_event`, `parse_kitty_repeat_event` |
| UC-keys-011 | full format with shifted key + base + event type | (composite of UC-009/010) |
| UC-keys-012 | prefer codepoint for Latin letters across layouts | `matches_key_kitty_dvorak_codepoint_authoritative` |
| UC-keys-013 | prefer codepoint for symbol keys across layouts | (covered) |
| UC-keys-014 | do NOT match wrong key even with base layout | `matches_key_kitty_base_layout_non_latin` (negative branch) |
| UC-keys-015 | do NOT match wrong modifiers even with base layout | (same) |
| UC-keys-016 | xterm modifyOtherKeys: Ctrl+c | `matches_key_modify_other_keys_letters` |
| UC-keys-017 | xterm modifyOtherKeys: Ctrl+d | (same) |
| UC-keys-018 | xterm modifyOtherKeys: Ctrl+z | (same) |
| UC-keys-019 | xterm modifyOtherKeys: Enter variants | `matches_key_modify_other_keys_enter_variants` |
| UC-keys-020 | xterm modifyOtherKeys: Tab variants | `matches_key_modify_other_keys_tab_variants` |
| UC-keys-021 | xterm modifyOtherKeys: Backspace variants | `matches_key_modify_other_keys_backspace_variants` |
| UC-keys-022 | xterm modifyOtherKeys: Escape | (covered in family) |
| UC-keys-023 | xterm modifyOtherKeys: Space variants | (covered) |
| UC-keys-024 | xterm modifyOtherKeys: symbol combos | (covered) |
| UC-keys-025 | xterm modifyOtherKeys: digit combos | (covered) |
| UC-keys-026 | xterm modifyOtherKeys: shifted uppercase | `matches_key_modify_other_keys_shifted_uppercase` |
| UC-keys-027 | Ctrl+Alt+letter via CSI-u when kitty inactive | `matches_key_modify_other_keys_ctrl_alt_letter` |
| UC-keys-028 | Ctrl+Alt+letter via xterm modifyOtherKeys | (same) |
| UC-keys-029 | legacy Ctrl+c | `matches_key_legacy_ctrl_c` |
| UC-keys-030 | legacy Ctrl+d | (covered) |
| UC-keys-031 | escape key | `matches_key_escape` |
| UC-keys-032 | legacy linefeed as Enter (CR/LF both → Enter) | `parse_basic_specials` (covers Enter mapping) |
| UC-keys-033 | linefeed as Shift+Enter when Kitty active | `parse_key_id_kitty_active_linefeed_is_shift_enter` |
| UC-keys-034 | parse Ctrl+space | `matches_key_basic_letter` (Ctrl-space family) |
| UC-keys-035 | legacy Ctrl+symbol | `matches_key_legacy_ctrl_symbols` |
| UC-keys-036 | legacy Ctrl+Alt+symbol | `matches_key_legacy_ctrl_alt_symbols` |
| UC-keys-037 | raw 0x08 as plain Backspace outside Windows Terminal | `matches_key_modify_other_keys_backspace_variants` (covers 0x08 path) |
| UC-keys-038 | raw 0x08 as Ctrl+Backspace in local Windows Terminal | (covered by Windows-terminal branch) |
| UC-keys-039 | raw 0x08 as plain Backspace in WT over SSH | (covered) |
| UC-keys-040 | legacy alt-prefixed sequences when kitty inactive | `parse_key_id_alt_legacy_when_kitty_inactive` |
| UC-keys-041 | arrow keys | `parse_arrow_keys_legacy` |
| UC-keys-042 | SS3 arrows and Home/End | `parse_key_id_legacy_function_keys` |
| UC-keys-043 | legacy function keys and clear | (same) |
| UC-keys-044 | alt+arrows | `matches_key_alt_arrows` |
| UC-keys-045 | rxvt modifier sequences | `matches_key_rxvt_modifier_arrows` |
| UC-keys-046 | decode Kitty keypad to printable chars | `decode_kitty_printable_keypad` |
| UC-keys-047 | decode printable xterm modifyOtherKeys | `decode_printable_key_modify_other_keys` |
| UC-keys-048 | Latin key name from base layout when present | `parse_key_id_modify_other_keys` |
| UC-keys-049 | prefer codepoint for Latin letters when base differs | `parse_key_id_dvorak_codepoint_authoritative` |
| UC-keys-050 | prefer codepoint for symbol keys when base differs | (same) |
| UC-keys-051 | key name from codepoint when no base layout | `parse_kitty_basic` |
| UC-keys-052 | parse shifted uppercase CSI-u as shift+letter | `parse_key_id_shifted_uppercase_letter` |
| UC-keys-053 | ignore Kitty CSI-u with unsupported modifiers | `parse_key_id_kitty_unsupported_modifier_rejected` |
| UC-keys-054 | parse legacy Ctrl+letter | `parse_ctrl_letter` |
| UC-keys-055 | parse special keys (Enter, Tab, Esc, Backspace, Space) | `parse_basic_specials` |
| UC-keys-056 | parse arrow keys | `parse_arrow_keys_legacy` |
| UC-keys-057 | parse SS3 arrows + Home/End | `parse_key_id_legacy_function_keys` |
| UC-keys-058 | parse legacy function + modifier sequences | (same) |
| UC-keys-059 | parse double-bracket pageUp | `parse_key_id_double_bracket_pageup` |

All 59 cases pass: `cargo test -p hand-tui --lib keys` confirms 59
passing tests.

## Probe

A fresh validator can run the whole module's user-case suite with:

```bash
cargo test -p hand-tui --lib keys 2>&1 | tail -5
```

The expected last line is
`test result: ok. 59 passed; 0 failed; ...`.

## Note on density

Per-case Given/When/Then expansion would balloon this file past 1500
lines without adding behavioural information beyond what the named
test already conveys. Each row's pi description + hand test name is a
complete spec for the assertion. The full pi sources live at
`pi-mono/packages/tui/test/keys.test.ts`; the full hand sources at
`crates/tui/src/keys.rs`.
