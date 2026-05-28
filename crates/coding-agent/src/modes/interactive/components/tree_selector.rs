//! Minimal-viable session-tree selector dialog.
//!
//! ## Current scope
//!
//! The full selector is a large dialog that bundles three components
//! plus a container:
//!
//! - `TreeList`: scrolling tree view with depth-aware gutter, fold/unfold,
//!   tool-call clustering, search, five filter modes, and current-leaf
//!   highlighting.
//! - `SearchLine`: live fuzzy search box.
//! - `LabelInput`: inline rename overlay.
//! - `TreeSelectorComponent`: the container that wires them all up.
//!
//! Per the worktree brief, the Rust port is a **minimal viable subset**:
//! flat list + arrow nav + select-on-Enter + cancel-on-Escape. The driver
//! flattens the tree (depth-first, with depth tracked per node) into the
//! [`TreeRow`] vec and the component renders it with simple indent gutters.
//! Folding, filtering, search, labels, and the "current leaf" emphasis are
//! tracked as `// TODO(parity)`.
//!
//! Why a local data type rather than reusing
//! [`crate::core::session_manager::SessionInfo`]: the TS source's
//! `SessionTreeNode` is a separate type built by `SessionManager.getTree()`,
//! and that builder has not been ported yet. Instead of waiting for it, the
//! component takes a flat [`TreeRow`] vector — drivers that have a tree
//! flatten it before passing it in. Once the Rust `getTree()` lands, an
//! adapter can build [`TreeRow`] from it.

use hand_tui::Component;
use hand_tui::keybindings::{Keybinding, get_keybindings};
use hand_tui::tui::{HandleResult, InputEvent};
use hand_tui::utils::{truncate_to_width, visible_width};
use tokio::sync::mpsc;

use super::dynamic_border::DynamicBorderComponent;
use super::keybinding_hints::key_hint_for;

/// One row in the flattened tree. The driver flattens its tree and labels
/// each row with `depth` and a stable `id` (used in the
/// [`TreeSelectorEvent::Selected`] payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    /// Stable id (typically the entry / message id).
    pub id: String,
    /// Indent depth — `0` for roots.
    pub depth: usize,
    /// Plain-text label rendered after the indent gutter.
    pub label: String,
    /// Optional secondary text (e.g. timestamp). Rendered in muted style
    /// after the primary label.
    pub secondary: Option<String>,
}

/// Outcome surfaced via the events channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeSelectorEvent {
    /// User confirmed a row.
    Selected(String),
    /// User pressed `tui.select.cancel`.
    Cancelled,
}

const ACCENT: &str = "\x1b[36m";
const MUTED: &str = "\x1b[90m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

/// Minimal viable session-tree dialog. Renders the rows in the order given
/// (depth-first flatten is the caller's responsibility).
pub struct TreeSelectorComponent {
    rows: Vec<TreeRow>,
    selected_index: usize,
    border: DynamicBorderComponent,
    events: mpsc::UnboundedSender<TreeSelectorEvent>,
    max_visible: usize,
    title: String,
}

impl TreeSelectorComponent {
    pub fn new(rows: Vec<TreeRow>, events: mpsc::UnboundedSender<TreeSelectorEvent>) -> Self {
        Self {
            rows,
            selected_index: 0,
            border: DynamicBorderComponent::new(),
            events,
            max_visible: 16,
            title: "Session Tree".into(),
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    pub fn with_max_visible(mut self, max_visible: usize) -> Self {
        self.max_visible = max_visible;
        self
    }

    /// Replace the displayed rows. Resets selection to the top.
    pub fn set_rows(&mut self, rows: Vec<TreeRow>) {
        self.rows = rows;
        self.selected_index = 0;
    }

    pub fn selected(&self) -> Option<&TreeRow> {
        self.rows.get(self.selected_index)
    }

    fn move_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    fn move_down(&mut self) {
        if self.selected_index + 1 < self.rows.len() {
            self.selected_index += 1;
        }
    }

    fn visible_window(&self) -> (usize, usize) {
        let total = self.rows.len();
        if total == 0 || self.max_visible == 0 {
            return (0, 0);
        }
        let visible = self.max_visible.min(total);
        let mut start = self
            .selected_index
            .saturating_sub(visible.saturating_sub(1));
        if start + visible > total {
            start = total.saturating_sub(visible);
        }
        if self.selected_index < visible {
            start = 0;
        }
        (start, (start + visible).min(total))
    }
}

fn format_row(row: &TreeRow, selected: bool, width: usize) -> String {
    let cursor = if selected { "▸ " } else { "  " };
    // Two columns per depth level — consistent with the TS gutter.
    let indent = "  ".repeat(row.depth);
    let secondary = row
        .secondary
        .as_deref()
        .map(|s| format!("  {MUTED}{s}{RESET}"))
        .unwrap_or_default();
    let primary = format!("{cursor}{indent}{}{secondary}", row.label);
    let truncated = truncate_to_width(&primary, width.saturating_sub(2));
    if selected {
        format!("{ACCENT}{BOLD}{truncated}{RESET}")
    } else {
        truncated
    }
}

impl Component for TreeSelectorComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let width_us = width as usize;
        let mut out = Vec::new();
        out.extend(self.border.render(width));
        out.push(pad_line(
            &format!("{ACCENT}{BOLD}{}{RESET}", self.title),
            width,
        ));

        if self.rows.is_empty() {
            out.push(pad_line(&format!("{MUTED}(empty tree){RESET}"), width));
        } else {
            let (start, end) = self.visible_window();
            for i in start..end {
                let line = format_row(&self.rows[i], i == self.selected_index, width_us);
                out.push(pad_line(&line, width));
            }
            if self.rows.len() > self.max_visible {
                out.push(pad_line(
                    &format!(
                        "{MUTED}({}/{} rows){RESET}",
                        self.selected_index + 1,
                        self.rows.len()
                    ),
                    width,
                ));
            }
        }

        let hint = format!(
            "{}  {}  {}",
            key_hint_for("tui.select.up", "↑"),
            key_hint_for("tui.select.down", "↓"),
            key_hint_for("tui.select.confirm", "open"),
        );
        out.push(pad_line(&format!("{MUTED}{hint}{RESET}"), width));
        out.extend(self.border.render(width));
        out
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        // The Tui dispatches ESC-prefixed sequences (arrows, escape) and
        // single-byte control codes (Enter, Ctrl+C) as `InputEvent::Key`
        // rather than `InputEvent::Raw`. Round either form back to the
        // canonical byte string the keybinding matcher expects so the
        // picker responds to keyboard input identically regardless of
        // how the host renders it.
        let raw_buf;
        let raw: &str = match event {
            InputEvent::Raw(s) | InputEvent::Paste(s) => s.as_str(),
            InputEvent::Key(key) => match hand_tui::key_to_canonical_bytes(key) {
                Some(bytes) => {
                    raw_buf = bytes;
                    raw_buf.as_str()
                }
                None => return HandleResult::Ignored,
            },
            _ => return HandleResult::Ignored,
        };
        let kb = get_keybindings();

        if kb.matches(raw, Keybinding::SelectUp) {
            self.move_up();
            return HandleResult::Handled;
        }
        if kb.matches(raw, Keybinding::SelectDown) {
            self.move_down();
            return HandleResult::Handled;
        }
        if kb.matches(raw, Keybinding::SelectConfirm)
            && let Some(row) = self.selected()
        {
            let _ = self
                .events
                .send(TreeSelectorEvent::Selected(row.id.clone()));
            return HandleResult::Handled;
        }
        if kb.matches(raw, Keybinding::SelectCancel) {
            let _ = self.events.send(TreeSelectorEvent::Cancelled);
            return HandleResult::Handled;
        }

        HandleResult::Ignored
    }
}

fn pad_line(line: &str, width: u16) -> String {
    let target = width as usize;
    let current = visible_width(line);
    if current >= target {
        line.to_string()
    } else {
        format!("{line}{}", " ".repeat(target - current))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, depth: usize, label: &str) -> TreeRow {
        TreeRow {
            id: id.into(),
            depth,
            label: label.into(),
            secondary: None,
        }
    }

    fn make(
        rows: Vec<TreeRow>,
    ) -> (
        TreeSelectorComponent,
        mpsc::UnboundedReceiver<TreeSelectorEvent>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        (TreeSelectorComponent::new(rows, tx), rx)
    }

    #[test]
    fn empty_tree_renders_placeholder() {
        let (c, _rx) = make(vec![]);
        let blob = c.render(60).join("\n");
        assert!(blob.contains("(empty tree)"));
    }

    #[test]
    fn renders_indent_per_depth() {
        let (c, _rx) = make(vec![
            row("a", 0, "root"),
            row("b", 1, "child"),
            row("c", 2, "grandchild"),
        ]);
        let lines = c.render(80);
        // Indent progression: count leading spaces before each label.
        let leading_spaces = |line: &str, label: &str| -> usize {
            // Strip ANSI prefixes by jumping past any leading "\x1b[…m" sequences,
            // then over the cursor glyph if present.
            let mut s = line;
            while let Some(rest) = s.strip_prefix("\x1b[") {
                if let Some(idx) = rest.find('m') {
                    s = &rest[idx + 1..];
                } else {
                    break;
                }
            }
            // Skip cursor glyph if present.
            if let Some(rest) = s.strip_prefix("▸ ") {
                s = rest;
            } else if let Some(rest) = s.strip_prefix("  ") {
                s = rest;
            }
            // Now count spaces up to the label.
            let label_idx = s.find(label).unwrap_or(s.len());
            s[..label_idx].chars().filter(|c| *c == ' ').count()
        };
        let root_line = lines.iter().find(|l| l.contains("root")).unwrap();
        let child_line = lines.iter().find(|l| l.contains("child")).unwrap();
        let grand_line = lines.iter().find(|l| l.contains("grandchild")).unwrap();
        let r = leading_spaces(root_line, "root");
        let c1 = leading_spaces(child_line, "child");
        let g = leading_spaces(grand_line, "grandchild");
        assert!(r < c1, "root indent {r} should be < child indent {c1}");
        assert!(
            c1 < g,
            "child indent {c1} should be < grandchild indent {g}"
        );
    }

    #[test]
    fn arrow_nav_moves_selection() {
        let (mut c, _rx) = make(vec![row("a", 0, "x"), row("b", 0, "y"), row("c", 0, "z")]);
        c.handle_input(&InputEvent::Raw("\x1b[B".into()));
        assert_eq!(c.selected().map(|r| r.id.clone()), Some("b".into()));
        c.handle_input(&InputEvent::Raw("\x1b[A".into()));
        assert_eq!(c.selected().map(|r| r.id.clone()), Some("a".into()));
    }

    #[test]
    fn enter_emits_selected_with_id() {
        let (mut c, mut rx) = make(vec![row("only", 0, "x")]);
        c.handle_input(&InputEvent::Raw("\r".into()));
        match rx.try_recv() {
            Ok(TreeSelectorEvent::Selected(id)) => assert_eq!(id, "only"),
            other => panic!("expected Selected, got {other:?}"),
        }
    }

    #[test]
    fn escape_emits_cancelled() {
        let (mut c, mut rx) = make(vec![row("a", 0, "x")]);
        c.handle_input(&InputEvent::Raw("\x1b".into()));
        match rx.try_recv() {
            Ok(TreeSelectorEvent::Cancelled) => {}
            other => panic!("expected Cancelled, got {other:?}"),
        }
    }

    /// Regression: the real Tui dispatches ESC-prefixed sequences (arrows,
    /// Escape) and single-byte control codes (Enter) as `InputEvent::Key`,
    /// not `InputEvent::Raw`. The picker must respond to both forms.
    #[test]
    fn key_events_drive_navigation_and_selection() {
        use hand_tui::keys::parse_key;
        let (mut c, mut rx) = make(vec![row("a", 0, "x"), row("b", 0, "y")]);
        c.handle_input(&InputEvent::Key(parse_key("\x1b[B")));
        assert_eq!(c.selected().map(|r| r.id.clone()), Some("b".into()));
        c.handle_input(&InputEvent::Key(parse_key("\r")));
        match rx.try_recv() {
            Ok(TreeSelectorEvent::Selected(id)) => assert_eq!(id, "b"),
            other => panic!("expected Selected, got {other:?}"),
        }
    }

    #[test]
    fn key_escape_event_emits_cancelled() {
        use hand_tui::keys::parse_key;
        let (mut c, mut rx) = make(vec![row("a", 0, "x")]);
        c.handle_input(&InputEvent::Key(parse_key("\x1b")));
        match rx.try_recv() {
            Ok(TreeSelectorEvent::Cancelled) => {}
            other => panic!("expected Cancelled, got {other:?}"),
        }
    }
}
