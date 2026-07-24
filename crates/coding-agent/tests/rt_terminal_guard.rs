//! Terminal-guard restore assertions for the rt interactive driver
//! (VAL-COMPAT-011 / VAL-COMPAT-012), from the `hand-coding-agent` side.
//!
//! The interactive driver (`modes::interactive::rt_driver`) runs entirely on
//! the rt [`SessionGuard`](hand_tui::rt::session), whose one-shot restore fires
//! on the normal exit, on `Drop`, and — crucially — from a panic hook, and pops
//! the kitty keyboard-enhancement flags it pushed. These tests pin the two
//! hand-level regressions that guard the migration:
//!
//! - **VAL-COMPAT-012 (panic-restore + pop flags).** On the crash path the guard
//!   must restore cooked state (disable bracketed paste, show the cursor) and pop
//!   the kitty flags it pushed, so a panic leaves a usable terminal. Here we
//!   assert the exact restore byte sequence the guard's Drop / panic hook emits,
//!   via the public [`write_restore_sequences`]. The *full* fork-a-child-into-a-
//!   PTY-and-panic guard test, and the `restore_once` idempotence across
//!   restore / Drop / panic hook, are owned by the rt layer
//!   (`crates/tui/tests/rt_session.rs`); this is the coding-agent-side contract
//!   the driver depends on.
//! - **VAL-COMPAT-011 (0x0 PTY operability).** A degenerate 0x0 PTY resolves to
//!   the 80x24 fallback via [`effective_size`], so the driver keeps rendering
//!   rather than collapsing. (The driver's own `draw` over that fallback is
//!   exercised in the driver's unit tests; here we pin the resolution contract.)

use hand_tui::rt::session::{
    FALLBACK_COLS, FALLBACK_ROWS, effective_size, write_enter_sequences, write_restore_sequences,
};

/// Bracketed-paste enable / disable — the guard toggles paste around a session.
const ENABLE_PASTE: &[u8] = b"\x1b[?2004h";
const DISABLE_PASTE: &[u8] = b"\x1b[?2004l";
/// Kitty keyboard-enhancement pop (`CSI < 1 u`) — emitted only when the session
/// pushed the flags.
const KITTY_POP: &[u8] = b"\x1b[<1u";
/// Cursor show — the guard makes the cursor visible again on restore.
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Position of a sub-slice within a byte buffer, or `None`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[test]
fn panic_restore_pops_kitty_flags_and_restores_cooked_state() {
    // VAL-COMPAT-012: the exact bytes the guard's Drop / panic hook writes when a
    // kitty-enabled session tears down. A crash must leave the terminal usable:
    // flags popped, paste disabled, cursor shown.
    let mut restore = Vec::new();
    write_restore_sequences(&mut restore, true).expect("write restore sequences");

    assert!(
        contains(&restore, KITTY_POP),
        "the panic/Drop restore must pop the kitty keyboard flags it pushed"
    );
    assert!(
        contains(&restore, DISABLE_PASTE),
        "the restore must disable bracketed paste (leave cooked state)"
    );
    assert!(
        contains(&restore, SHOW_CURSOR),
        "the restore must show the cursor so the shell prompt is visible"
    );

    // The pop must precede the paste-disable: crossterm's pop closes the kitty
    // protocol layer before the outer terminal modes are cooked again, matching
    // the guard's teardown order.
    let pop_at = find(&restore, KITTY_POP).expect("kitty pop present");
    let paste_at = find(&restore, DISABLE_PASTE).expect("paste disable present");
    assert!(
        pop_at < paste_at,
        "kitty flags must be popped before bracketed paste is disabled"
    );
}

#[test]
fn plain_session_restore_shows_cursor_without_popping_kitty() {
    // A session that never pushed kitty flags (a plain / degraded terminal) must
    // not emit a stray pop on the crash path — that would corrupt a terminal that
    // never entered the protocol.
    let mut restore = Vec::new();
    write_restore_sequences(&mut restore, false).expect("write restore sequences");

    assert!(
        !contains(&restore, KITTY_POP),
        "a plain session must not pop kitty flags it never pushed"
    );
    assert!(contains(&restore, DISABLE_PASTE), "paste still disabled");
    assert!(contains(&restore, SHOW_CURSOR), "cursor still shown");
}

#[test]
fn enter_then_restore_is_byte_symmetric_for_a_kitty_session() {
    // The enter path pushes what the restore path pops: a kitty session enables
    // paste + pushes flags on enter, and pops flags + disables paste on restore.
    // If a future change added an enter escape without a matching restore, a crash
    // would strand the terminal in that mode — this pins the pair.
    let mut enter = Vec::new();
    write_enter_sequences(&mut enter, true).expect("write enter sequences");
    assert!(
        contains(&enter, ENABLE_PASTE),
        "enter enables bracketed paste"
    );
    // The kitty push uses the `CSI > <bits> u` form; its matching pop is `CSI < 1 u`.
    assert!(
        contains(&enter, b"\x1b[>"),
        "a kitty session pushes the keyboard-enhancement flags on enter"
    );

    let mut restore = Vec::new();
    write_restore_sequences(&mut restore, true).expect("write restore sequences");
    assert!(
        contains(&restore, KITTY_POP) && contains(&restore, DISABLE_PASTE),
        "restore pops the flags and disables the paste that enter set"
    );
}

#[test]
fn zero_sized_pty_resolves_to_the_operable_fallback_geometry() {
    // VAL-COMPAT-011: a 0x0 PTY (unknown geometry) resolves to the 80x24 fallback
    // the driver renders at, so the session stays operable instead of collapsing
    // to a zero-area frame. A single non-zero dimension is still degenerate and
    // also falls back; a fully known size passes through untouched.
    assert_eq!(effective_size(0, 0), (FALLBACK_COLS, FALLBACK_ROWS));
    assert_eq!(effective_size(0, 30), (FALLBACK_COLS, FALLBACK_ROWS));
    assert_eq!(effective_size(120, 0), (FALLBACK_COLS, FALLBACK_ROWS));
    assert_eq!(
        effective_size(132, 43),
        (132, 43),
        "a fully known size is used as-is"
    );
}
