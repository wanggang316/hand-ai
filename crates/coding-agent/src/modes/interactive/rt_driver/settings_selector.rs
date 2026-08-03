//! The rt-native `/settings` selector — the editable settings dialog built on the
//! [overlay runtime](super::overlay) and the M2
//! [`SettingsList`](hand_tui::rt::components::SettingsList).
//!
//! It is a [`SelectorController`] that **embeds the M2 `SettingsList`** for the
//! interactive state machine (clamp navigation, Tab-cycle a bool / enum /
//! inline-edit a string) and layers the driver semantics on top:
//!
//! - the top three rows show the **merged effective defaults**
//!   (`default_provider` / `default_model` / `default_thinking_level`) so a
//!   project override is *visible* here (VAL-OVERLAY-036 — the pinned UAT #16
//!   regression); the driver builds those entries from `settings().current()`;
//! - **each change persists but the dialog stays open** (VAL-OVERLAY-013): Tab
//!   cycles the highlighted enum/bool, and the moment a value diverges the
//!   selector emits a [`SettingsOutcome::Changed`] and **re-snapshots** — it does
//!   *not* raise its [`DoneSignal`](super::overlay::DoneSignal), so the user can
//!   keep adjusting rows without reopening. The driver persists each change
//!   quietly (footer live, no status spam);
//! - the two **big-list rows** (`default_provider` / `default_model`) can't be
//!   Tab-cycled — a catalog carries dozens of providers and hundreds of models —
//!   so Enter/Tab on them emits [`SettingsOutcome::OpenProviderPicker`] /
//!   [`SettingsOutcome::OpenModelPicker`] and raises the done flag, handing off to
//!   a **second-level picker** the driver mounts in place (returning to this
//!   dialog afterwards);
//! - **Enter on a normal row confirms and closes** — it never mutates a value
//!   (Tab/Shift+Tab own that), so the footer's `↵ select` never lies about Enter
//!   changing the highlighted enum;
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
use crate::modes::interactive::theme::ThemePalette;

/// The most rows of the settings body rendered into the scratch buffer at once —
/// generous so the whole (short) settings list plus its description/hint chrome
/// always fits; the overlay clips anything past the dialog interior.
const RENDER_ROWS: u16 = 24;

/// An outcome the selector emits on its channel. Unlike the one-shot pickers,
/// the settings dialog stays open across value changes, so it may emit **many**
/// [`Changed`](SettingsOutcome::Changed) before a terminal
/// [`Closed`](SettingsOutcome::Closed) / picker hand-off.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsOutcome {
    /// The user changed a setting: its id (the entry key) and the post-change
    /// string value. The driver persists it quietly and the dialog stays open
    /// (VAL-OVERLAY-013).
    Changed { id: String, value: String },
    /// Enter/Tab on the `default_model` row — the driver opens the model
    /// second-level picker, then re-mounts this dialog.
    OpenModelPicker,
    /// Enter/Tab on the `default_provider` row — the driver opens the provider
    /// second-level picker, then re-mounts this dialog.
    OpenProviderPicker,
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
        Self::with_nav_at(entries, tx, done, nav, 0)
    }

    /// Like [`with_nav`](Self::with_nav) but starts with row `selected`
    /// highlighted — the driver uses this to restore the cursor after a
    /// second-level picker returns to the dialog.
    #[must_use]
    pub fn with_nav_at(
        entries: Vec<SettingEntry>,
        tx: mpsc::UnboundedSender<SettingsOutcome>,
        done: DoneSignal,
        nav: NavKeys,
        selected: usize,
    ) -> Self {
        let snapshot = snapshot_of(&entries);
        let list = SettingsList::new(entries)
            .max_visible(10)
            .show_description(true)
            .show_hint(true)
            .hint_text("⇥ change · ↵ select · esc close")
            .selected(selected);
        Self {
            list,
            snapshot,
            tx,
            done,
            nav,
        }
    }

    /// The index of the highlighted row (into the entry list) — the driver reads
    /// it when a picker hand-off fires so it can restore the cursor on re-mount.
    #[must_use]
    pub fn selected_index(&self) -> usize {
        self.list.selected_index()
    }

    /// The entries the dialog is showing (test/introspection aid).
    #[must_use]
    pub fn entries(&self) -> &[SettingEntry] {
        self.list.entries()
    }

    /// Emit the `(id, value)` of the row that just diverged from the snapshot, if
    /// any, then **re-snapshot** so the next keystroke is compared afresh. The
    /// dialog stays open across changes (VAL-OVERLAY-013), so re-snapshotting is
    /// what keeps a second change on a *different* row from re-reporting the first
    /// one. A single keystroke mutates only the highlighted row, so at most one
    /// row can diverge here.
    fn emit_first_change(&mut self) {
        let mut changed = None;
        for (idx, entry) in self.list.entries().iter().enumerate() {
            let new_value = value_string(&entry.value);
            if let Some((id, old)) = self.snapshot.get(idx)
                && id == &entry.key
                && old != &new_value
            {
                changed = Some((entry.key.clone(), new_value));
                break;
            }
        }
        if let Some((id, value)) = changed {
            let _ = self.tx.send(SettingsOutcome::Changed { id, value });
            self.snapshot = snapshot_of(self.list.entries());
        }
    }

    /// Render the embedded list into a scratch buffer and lift each painted row
    /// back into a styled [`Line`], preserving the list's per-cell styling (the
    /// bold selected row, the dim description/hint, the edit caret block).
    ///
    /// This selector paints through the M2 [`SelectList`] component, which owns
    /// its own theming; the driver-side palette is therefore not applied here
    /// (the component is a read-only reuse).
    fn body_lines(&self, width: u16) -> Vec<Line<'static>> {
        let width = width.max(1);
        let area = Rect::new(0, 0, width, RENDER_ROWS);
        let mut buf = Buffer::empty(area);
        self.list.render(area, &mut buf);
        buffer_to_lines(&buf, area)
    }
}

impl SelectorController for SettingsSelector {
    fn render_lines(&self, width: u16, _palette: &ThemePalette) -> Vec<Line<'static>> {
        self.body_lines(width)
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        let id = key.key_id.as_deref();

        // The cancel key (default Esc, remappable via `select-cancel`) while *not*
        // inline-editing closes the whole dialog with the specific `[/settings
        // closed]` outcome (VAL-OVERLAY-004 exception). While editing, an Esc only
        // discards the edit buffer, so let it fall through to the list.
        if id.is_some_and(|id| self.nav.is_cancel(id)) && !self.list.is_editing() {
            let _ = self.tx.send(SettingsOutcome::Closed);
            self.done.store(true, Ordering::SeqCst);
            return HandleOutcome::Consumed;
        }

        // An activation gesture (Enter / Tab / Shift+Tab) on a big-list row hands
        // off to a second-level picker instead of cycling in place: raise done so
        // the driver swaps this overlay for the picker, then re-mounts. Guarded to
        // the non-editing state so it can never fire mid-edit.
        if !self.list.is_editing()
            && matches!(id, Some("enter" | "tab" | "shift+tab"))
            && let Some(entry) = self.list.selected_entry()
            && let Some(outcome) = picker_outcome_for(&entry.key)
        {
            let _ = self.tx.send(outcome);
            self.done.store(true, Ordering::SeqCst);
            return HandleOutcome::Consumed;
        }

        // Enter on a normal (non-picker) row **confirms and closes** — it must
        // never change a value (Tab / Shift+Tab are the only change gestures, so
        // the footer's `↵ select` stays honest). Guarded to non-editing so an
        // inline string/number edit still commits with Enter via the list below.
        if id == Some("enter") && !self.list.is_editing() {
            let _ = self.tx.send(SettingsOutcome::Closed);
            self.done.store(true, Ordering::SeqCst);
            return HandleOutcome::Consumed;
        }

        // Drive the embedded list, then detect whether this key changed a value.
        self.list.handle_key(key);

        // A toggle/cycle/commit that changed a value persists, but the dialog
        // stays open (VAL-OVERLAY-013) so the user can keep adjusting rows. While
        // the inline editor is still open (a string/number mid-edit), no change
        // has committed yet, so nothing is emitted until Enter commits it.
        if !self.list.is_editing() {
            self.emit_first_change();
        }

        // A modal selector owns every key so none reaches the editor beneath
        // (VAL-OVERLAY-005).
        HandleOutcome::Consumed
    }
}

/// The second-level picker outcome for a big-list row key, or `None` for a
/// normal in-place-cyclable row.
fn picker_outcome_for(key: &str) -> Option<SettingsOutcome> {
    match key {
        "default_provider" => Some(SettingsOutcome::OpenProviderPicker),
        "default_model" => Some(SettingsOutcome::OpenModelPicker),
        _ => None,
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

    /// Entries mirroring the production shape: the three merged-default rows
    /// first (provider and model are display-only strings whose activation opens
    /// a second-level picker; thinking level is a cycle enum seeded at the
    /// effective value) then a toggle and an enum.
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
                "Effective default model. Pick interactively via /model.",
            ),
            SettingEntry::new(
                "default_thinking_level",
                SettingValue::Enum {
                    choices: vec![
                        "off".into(),
                        "minimal".into(),
                        "low".into(),
                        "medium".into(),
                        "high".into(),
                        "xhigh".into(),
                        "max".into(),
                    ],
                    selected: 4, // "high"
                },
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

    // --- each change persists but the dialog stays open (VAL-OVERLAY-013) --

    #[test]
    fn toggling_a_bool_emits_change_and_stays_open() {
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

        sel.handle_key(&key_id("tab", KeyCode::Tab)); // toggle bool true->false
        assert!(
            !done.load(Ordering::SeqCst),
            "a change keeps the dialog open"
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
    fn tab_cycles_an_enum_and_stays_open() {
        let (mut sel, mut rx, done) = selector();
        // Move to the theme enum row (index 4) and cycle it with Tab.
        for _ in 0..4 {
            sel.handle_key(&key_id("down", KeyCode::Down));
        }
        sel.handle_key(&key_id("tab", KeyCode::Tab)); // dark -> light
        assert!(
            !done.load(Ordering::SeqCst),
            "a change keeps the dialog open"
        );
        match drain(&mut rx) {
            Some(SettingsOutcome::Changed { id, value }) => {
                assert_eq!(id, "theme");
                assert_eq!(value, "light");
            }
            other => panic!("expected Changed, got {other:?}"),
        }
    }

    #[test]
    fn tab_walks_the_thinking_ladder_across_multiple_changes() {
        // The dialog stays open, so a second Tab on the same row must report the
        // *next* value — the re-snapshot after each emit is what makes this work.
        let (mut sel, mut rx, done) = selector();
        sel.handle_key(&key_id("down", KeyCode::Down));
        sel.handle_key(&key_id("down", KeyCode::Down)); // thinking row (index 2)

        sel.handle_key(&key_id("tab", KeyCode::Tab)); // high -> xhigh
        assert_eq!(
            drain(&mut rx),
            Some(SettingsOutcome::Changed {
                id: "default_thinking_level".into(),
                value: "xhigh".into(),
            })
        );
        sel.handle_key(&key_id("tab", KeyCode::Tab)); // xhigh -> max
        assert_eq!(
            drain(&mut rx),
            Some(SettingsOutcome::Changed {
                id: "default_thinking_level".into(),
                value: "max".into(),
            })
        );
        // Shift+Tab steps back: max -> xhigh.
        sel.handle_key(&key_id("shift+tab", KeyCode::BackTab));
        assert_eq!(
            drain(&mut rx),
            Some(SettingsOutcome::Changed {
                id: "default_thinking_level".into(),
                value: "xhigh".into(),
            })
        );
        assert!(
            !done.load(Ordering::SeqCst),
            "cycling never closes the dialog"
        );
    }

    #[test]
    fn changing_two_rows_reports_each_row_once() {
        // After the first change the snapshot is refreshed, so changing a second
        // row reports *that* row — never a stale re-report of the first.
        let (mut sel, mut rx, _done) = selector();
        for _ in 0..3 {
            sel.handle_key(&key_id("down", KeyCode::Down));
        }
        sel.handle_key(&key_id("tab", KeyCode::Tab)); // auto_compact true->false
        assert_eq!(
            drain(&mut rx),
            Some(SettingsOutcome::Changed {
                id: "auto_compact".into(),
                value: "false".into(),
            })
        );
        sel.handle_key(&key_id("down", KeyCode::Down)); // theme row
        sel.handle_key(&key_id("tab", KeyCode::Tab)); // dark -> light
        assert_eq!(
            drain(&mut rx),
            Some(SettingsOutcome::Changed {
                id: "theme".into(),
                value: "light".into(),
            })
        );
    }

    // --- big-list rows hand off to a second-level picker ------------------

    #[test]
    fn enter_on_provider_row_opens_the_provider_picker() {
        let (mut sel, mut rx, done) = selector();
        // The provider row is the first, already selected.
        sel.handle_key(&key_id("enter", KeyCode::Enter));
        assert!(
            done.load(Ordering::SeqCst),
            "a picker hand-off raises done so the driver can swap overlays"
        );
        assert_eq!(drain(&mut rx), Some(SettingsOutcome::OpenProviderPicker));
    }

    #[test]
    fn tab_on_model_row_opens_the_model_picker() {
        let (mut sel, mut rx, done) = selector();
        sel.handle_key(&key_id("down", KeyCode::Down)); // model row (index 1)
        sel.handle_key(&key_id("tab", KeyCode::Tab));
        assert!(done.load(Ordering::SeqCst), "picker hand-off raises done");
        assert_eq!(drain(&mut rx), Some(SettingsOutcome::OpenModelPicker));
    }

    #[test]
    fn provider_row_never_inline_edits_or_reports_a_change() {
        // Enter on the display-only provider string must open the picker, not the
        // inline editor, and must not emit a Changed for it.
        let (mut sel, mut rx, _done) = selector();
        sel.handle_key(&key_id("enter", KeyCode::Enter));
        assert_eq!(drain(&mut rx), Some(SettingsOutcome::OpenProviderPicker));
        assert!(drain(&mut rx).is_none(), "no trailing Changed");
    }

    #[test]
    fn enter_on_a_value_row_confirms_and_closes_without_changing_it() {
        // The user's complaint: Enter on the thinking enum used to *cycle* it.
        // Now Enter never mutates a value — it confirms and closes (Tab is the
        // only change gesture), so the footer's `↵ select` stays honest.
        let (mut sel, mut rx, done) = selector();
        sel.handle_key(&key_id("down", KeyCode::Down));
        sel.handle_key(&key_id("down", KeyCode::Down)); // thinking row (index 2)
        let before = sel.entries()[2].value.to_string();

        sel.handle_key(&key_id("enter", KeyCode::Enter));
        assert!(done.load(Ordering::SeqCst), "Enter closes the dialog");
        assert_eq!(
            drain(&mut rx),
            Some(SettingsOutcome::Closed),
            "Enter on a value row confirms-and-closes, not a Changed"
        );
        assert_eq!(
            sel.entries()[2].value.to_string(),
            before,
            "Enter must not cycle the value"
        );
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

    // --- cursor restore after a picker hand-off ---------------------------

    #[test]
    fn with_nav_at_restores_the_cursor_row() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let done = super::super::overlay::new_done_signal();
        let sel = SettingsSelector::with_nav_at(entries(), tx, done, NavKeys::default(), 2);
        assert_eq!(sel.selected_index(), 2);
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
