//! The rt-native `/settings` selector — the editable settings dialog built on the
//! [overlay runtime](super::overlay) and the M2
//! [`SettingsList`](hand_tui::rt::components::SettingsList).
//!
//! It is a [`SelectorController`] that **embeds the M2 `SettingsList`** for the
//! interactive state machine (clamp navigation, toggle a bool / cycle an enum /
//! inline-edit a string) and layers the driver semantics on top:
//!
//! - the top three rows show the **merged effective defaults**
//!   (`default_provider` / `default_model` / `default_thinking_level`) so a
//!   project override is *visible* here (VAL-OVERLAY-036 — the pinned UAT #16
//!   regression); the driver builds those entries from `settings().current()`;
//! - the **first change persists** immediately and closes the dialog
//!   (VAL-OVERLAY-013): the selector watches the list's values around each key and
//!   emits a [`SettingsOutcome::Changed`] the moment one diverges, then raises its
//!   [`DoneSignal`](super::overlay::DoneSignal);
//! - **Esc** closes with [`SettingsOutcome::Closed`] — the driver lands the
//!   specific `[/settings closed]` line, *not* the generic "cancelled" (the
//!   VAL-OVERLAY-004 exception).
//!
//! Rendering delegates to the embedded `SettingsList`: it paints itself into a
//! scratch [`Buffer`] sized to the overlay interior, and this component lifts each
//! painted row back into a styled [`Line`] the scheduler layers into the dialog —
//! so the list's own selection/description/hint chrome (and its edit caret block)
//! survives the overlay's `Vec<Line>` transport unchanged.

use std::sync::atomic::Ordering;

use hand_tui::rt::components::{SettingEntry, SettingValue, SettingsList};
use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::{HandleOutcome, RtComponent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use tokio::sync::mpsc;

use super::keys::NavKeys;
use super::overlay::{DoneSignal, SelectorController};

/// The most rows of the settings body rendered into the scratch buffer at once —
/// generous so the whole (short) settings list plus its description/hint chrome
/// always fits; the overlay clips anything past the dialog interior.
const RENDER_ROWS: u16 = 24;

/// The outcome the selector emits on its channel — exactly one per open.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsOutcome {
    /// The user changed a setting: its id (the entry key) and the post-change
    /// string value. The driver persists it and closes the dialog
    /// (VAL-OVERLAY-013).
    Changed { id: String, value: String },
    /// The user pressed Esc — the dialog closes with the specific
    /// `[/settings closed]` line (VAL-OVERLAY-004 exception), not the generic
    /// cancelled path.
    Closed,
}

/// The rt-native `/settings` dialog component, wrapping an M2 [`SettingsList`].
pub struct SettingsSelector {
    /// The embedded M2 list — owns navigation, edit, and the toggle/cycle/commit
    /// state machine.
    list: SettingsList,
    /// The pre-key snapshot of each entry's `(key, value-string)`, used to detect
    /// the first divergence so a change persists on the keystroke that caused it.
    snapshot: Vec<(String, String)>,
    /// The outcome channel; exactly one [`SettingsOutcome`] on change/close.
    tx: mpsc::UnboundedSender<SettingsOutcome>,
    /// Raised on the terminal key (first change / Esc) so the runtime unmounts this.
    done: DoneSignal,
    /// The resolved cancel key, snapshotted from the live app-layer table. Only
    /// the *cancel* key is app-layer here: up / down / confirm are owned by the
    /// embedded M2 [`SettingsList`] (crates/tui, read-only), so they keep their
    /// built-in keys — the app-layer surface the driver owns is the close gesture
    /// (VAL-OVERLAY-021).
    nav: NavKeys,
}

impl SettingsSelector {
    /// Build a selector over `entries` with the default navigation keys.
    #[must_use]
    pub fn new(
        entries: Vec<SettingEntry>,
        tx: mpsc::UnboundedSender<SettingsOutcome>,
        done: DoneSignal,
    ) -> Self {
        Self::with_nav(entries, tx, done, NavKeys::default())
    }

    /// Build a selector over `entries` with the given resolved navigation keys.
    /// The caller (the driver) builds the entries from the merged effective
    /// settings, with the three default-* rows first so a project override is
    /// visible (VAL-OVERLAY-036).
    #[must_use]
    pub fn with_nav(
        entries: Vec<SettingEntry>,
        tx: mpsc::UnboundedSender<SettingsOutcome>,
        done: DoneSignal,
        nav: NavKeys,
    ) -> Self {
        let snapshot = snapshot_of(&entries);
        let list = SettingsList::new(entries)
            .max_visible(10)
            .show_description(true)
            .show_hint(true);
        Self {
            list,
            snapshot,
            tx,
            done,
            nav,
        }
    }

    /// The entries the dialog is showing (test/introspection aid).
    #[must_use]
    pub fn entries(&self) -> &[SettingEntry] {
        self.list.entries()
    }

    /// Emit the first `(id, value)` that diverged from the snapshot, if any. On a
    /// change this returns `true` so the caller closes the dialog — the first
    /// change persists and the overlay unmounts (VAL-OVERLAY-013).
    fn emit_first_change(&mut self) -> bool {
        for (idx, entry) in self.list.entries().iter().enumerate() {
            let new_value = value_string(&entry.value);
            if let Some((id, old)) = self.snapshot.get(idx)
                && id == &entry.key
                && old != &new_value
            {
                let _ = self.tx.send(SettingsOutcome::Changed {
                    id: entry.key.clone(),
                    value: new_value,
                });
                return true;
            }
        }
        false
    }

    /// Render the embedded list into a scratch buffer and lift each painted row
    /// back into a styled [`Line`], preserving the list's per-cell styling (the
    /// bold selected row, the dim description/hint, the edit caret block).
    fn body_lines(&self, width: u16) -> Vec<Line<'static>> {
        let width = width.max(1);
        let area = Rect::new(0, 0, width, RENDER_ROWS);
        let mut buf = Buffer::empty(area);
        self.list.render(area, &mut buf);
        buffer_to_lines(&buf, area)
    }
}

impl SelectorController for SettingsSelector {
    fn render_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.body_lines(width)
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        // The cancel key (default Esc, remappable via `select-cancel`) while *not*
        // inline-editing closes the whole dialog with the specific `[/settings
        // closed]` outcome (VAL-OVERLAY-004 exception). While editing, an Esc only
        // discards the edit buffer, so let it fall through to the list.
        if key
            .key_id
            .as_deref()
            .is_some_and(|id| self.nav.is_cancel(id))
            && !self.list.is_editing()
        {
            let _ = self.tx.send(SettingsOutcome::Closed);
            self.done.store(true, Ordering::SeqCst);
            return HandleOutcome::Consumed;
        }

        // Drive the embedded list, then detect whether this key changed a value.
        self.list.handle_key(key);

        // A toggle/cycle/commit that changed a value persists and closes the dialog.
        // While the inline editor is still open (a string/number mid-edit), no
        // change has committed yet, so nothing is emitted until Enter commits it.
        if !self.list.is_editing() && self.emit_first_change() {
            self.done.store(true, Ordering::SeqCst);
        }

        // A modal selector owns every key so none reaches the editor beneath
        // (VAL-OVERLAY-005).
        HandleOutcome::Consumed
    }
}

/// The `(key, value-string)` snapshot of `entries`, in order.
fn snapshot_of(entries: &[SettingEntry]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|e| (e.key.clone(), value_string(&e.value)))
        .collect()
}

/// The post-change string rendering of a [`SettingValue`], matching the value the
/// driver persists (`true` / `false` / the enum choice / the number / the string).
fn value_string(v: &SettingValue) -> String {
    match v {
        SettingValue::Bool(b) => b.to_string(),
        SettingValue::Enum { choices, selected } => {
            choices.get(*selected).cloned().unwrap_or_default()
        }
        SettingValue::String(s) => s.clone(),
        SettingValue::Number(n) => n.to_string(),
    }
}

/// Lift a painted [`Buffer`] into one [`Line`] per row, grouping runs of cells with
/// an equal [`Style`] into [`Span`]s so the list's styling survives. Trailing empty
/// rows are dropped so the dialog only sizes to its content.
fn buffer_to_lines(buf: &Buffer, area: Rect) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for y in area.top()..area.bottom() {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut run = String::new();
        let mut run_style: Option<Style> = None;
        for x in area.left()..area.right() {
            let (symbol, style) = match buf.cell((x, y)) {
                Some(cell) => (cell.symbol().to_string(), cell.style()),
                None => (" ".to_string(), Style::default()),
            };
            match run_style {
                Some(s) if s == style => run.push_str(&symbol),
                _ => {
                    if !run.is_empty() {
                        spans.push(Span::styled(
                            std::mem::take(&mut run),
                            run_style.unwrap_or_default(),
                        ));
                    }
                    run = symbol;
                    run_style = Some(style);
                }
            }
        }
        if !run.is_empty() {
            spans.push(Span::styled(run, run_style.unwrap_or_default()));
        }
        lines.push(Line::from(spans));
    }
    // Drop trailing blank rows so the overlay only spans the real content.
    while lines
        .last()
        .is_some_and(|l| l.spans.iter().all(|s| s.content.trim().is_empty()))
    {
        lines.pop();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key_id(id: &str, code: KeyCode) -> RtKey {
        RtKey {
            key_id: Some(id.to_string()),
            raw: KeyEvent::new(code, KeyModifiers::NONE),
        }
    }

    /// Entries with the three merged-default display rows first (as strings, so the
    /// effective value is visible) then a couple of editable toggles/enums.
    fn entries() -> Vec<SettingEntry> {
        vec![
            SettingEntry::new(
                "default_provider",
                SettingValue::String("anthropic".into()),
                "Effective default provider.",
            ),
            SettingEntry::new(
                "default_model",
                SettingValue::String("claude-opus".into()),
                "Effective default model.",
            ),
            SettingEntry::new(
                "default_thinking_level",
                SettingValue::String("high".into()),
                "Effective default thinking level.",
            ),
            SettingEntry::new("auto_compact", SettingValue::Bool(true), "Auto-compact"),
            SettingEntry::new(
                "theme",
                SettingValue::Enum {
                    choices: vec!["dark".into(), "light".into()],
                    selected: 0,
                },
                "Color theme",
            ),
        ]
    }

    fn selector() -> (
        SettingsSelector,
        mpsc::UnboundedReceiver<SettingsOutcome>,
        DoneSignal,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let done = super::super::overlay::new_done_signal();
        (SettingsSelector::new(entries(), tx, done.clone()), rx, done)
    }

    fn body_text(sel: &SettingsSelector) -> String {
        sel.body_lines(80)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<SettingsOutcome>) -> Option<SettingsOutcome> {
        rx.try_recv().ok()
    }

    // --- merged effective defaults are visible (VAL-OVERLAY-036) ----------

    #[test]
    fn renders_the_merged_effective_defaults() {
        // The three default-* rows (project-merged values) must be visible so a
        // project override shows in the dialog (issue #16 UAT regression).
        let (sel, _rx, _done) = selector();
        let body = body_text(&sel);
        assert!(
            body.contains("default_provider") && body.contains("anthropic"),
            "provider default missing: {body}"
        );
        assert!(
            body.contains("default_model") && body.contains("claude-opus"),
            "model default missing: {body}"
        );
        assert!(
            body.contains("default_thinking_level") && body.contains("high"),
            "thinking default missing: {body}"
        );
    }

    // --- first change persists + closes (VAL-OVERLAY-013) -----------------

    #[test]
    fn first_toggle_emits_change_and_raises_done() {
        let (mut sel, mut rx, done) = selector();
        // Move to the bool row (index 3) and toggle it.
        for _ in 0..3 {
            sel.handle_key(&key_id("down", KeyCode::Down));
        }
        assert!(!done.load(Ordering::SeqCst), "navigation must not close");
        assert!(
            drain(&mut rx).is_none(),
            "navigation must not emit a change"
        );

        sel.handle_key(&key_id("enter", KeyCode::Enter)); // toggle bool true->false
        assert!(
            done.load(Ordering::SeqCst),
            "first change closes the dialog"
        );
        match drain(&mut rx) {
            Some(SettingsOutcome::Changed { id, value }) => {
                assert_eq!(id, "auto_compact");
                assert_eq!(value, "false");
            }
            other => panic!("expected Changed, got {other:?}"),
        }
    }

    #[test]
    fn cycling_an_enum_emits_change() {
        let (mut sel, mut rx, _done) = selector();
        // Move to the enum row (index 4) and cycle it.
        for _ in 0..4 {
            sel.handle_key(&key_id("down", KeyCode::Down));
        }
        sel.handle_key(&key_id("enter", KeyCode::Enter)); // dark -> light
        match drain(&mut rx) {
            Some(SettingsOutcome::Changed { id, value }) => {
                assert_eq!(id, "theme");
                assert_eq!(value, "light");
            }
            other => panic!("expected Changed, got {other:?}"),
        }
    }

    #[test]
    fn navigation_alone_does_not_persist_or_close() {
        let (mut sel, mut rx, done) = selector();
        sel.handle_key(&key_id("down", KeyCode::Down));
        sel.handle_key(&key_id("up", KeyCode::Up));
        assert!(
            !done.load(Ordering::SeqCst),
            "navigation keeps the dialog open"
        );
        assert!(drain(&mut rx).is_none(), "navigation emits nothing");
    }

    // --- Esc closes with the specific outcome (VAL-OVERLAY-004 exception) --

    #[test]
    fn escape_emits_closed_not_cancelled() {
        let (mut sel, mut rx, done) = selector();
        sel.handle_key(&key_id("escape", KeyCode::Esc));
        assert!(done.load(Ordering::SeqCst), "escape closes the dialog");
        assert_eq!(
            drain(&mut rx),
            Some(SettingsOutcome::Closed),
            "escape must emit the specific Closed outcome, not a generic cancel"
        );
    }
}
