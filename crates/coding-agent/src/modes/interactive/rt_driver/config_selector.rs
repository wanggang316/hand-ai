//! The rt-native **resource-configuration** selector — the `hand config` dialog
//! migrated onto the [overlay runtime](super::overlay).
//!
//! Like the [`SessionPicker`](super::session_picker::SessionPicker) it is a
//! [`SelectorController`]: it produces rt-native styled [`Line`]s and consumes keys
//! while it is the mounted modal overlay. Its behaviour is the legacy config
//! dialog's, ported off the old `hand_tui::Component` model:
//!
//! - it flattens the resolved resources from
//!   [`ResolvedPaths`](crate::core::extensions::source_registry::ResolvedPaths) into
//!   group / subgroup / item rows (packages before top-level; user before project;
//!   by source, then resource kind, then name);
//! - **↑/↓ navigate** the *item* rows only (group and subgroup headers are skipped);
//! - **Space** (or the confirm key) **toggles** the highlighted item's checkbox in
//!   place — the flip is immediate visual feedback (VAL-CHAT-037) — and emits a
//!   [`ConfigOutcome::Toggled`] so a driver can persist the change;
//! - **Esc** dismisses the dialog ([`ConfigOutcome::Cancelled`]) and **Ctrl+C**
//!   raises a distinct [`ConfigOutcome::Exit`] (a hard exit rather than a dialog
//!   close). Each terminal key raises the [`DoneSignal`] so the runtime unmounts it.
//!
//! The write-back path (translating a toggle into a YAML settings edit) is the
//! driver's concern — the same construct-in / channel-out shape the session picker
//! uses; this component is pure UI + toggle logic over its constructor inputs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::HandleOutcome;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::mpsc;

use crate::core::extensions::source_registry::{
    InstallScope, ResolvedPaths, ResolvedResource, ResourceOrigin,
};

use super::keys::NavKeys;
use super::overlay::{DoneSignal, SelectorController};
use crate::modes::interactive::theme::ThemePalette;

/// The most rows shown at once; the window scrolls to keep the selection visible.
const MAX_VISIBLE: usize = 15;

/// One of the four resource kinds a source can contribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    Extensions,
    Skills,
    Prompts,
    Themes,
}

impl ResourceKind {
    fn label(self) -> &'static str {
        match self {
            ResourceKind::Extensions => "Extensions",
            ResourceKind::Skills => "Skills",
            ResourceKind::Prompts => "Prompts",
            ResourceKind::Themes => "Themes",
        }
    }

    /// Stable order used when sorting subgroups within a group.
    fn order(self) -> u8 {
        match self {
            ResourceKind::Extensions => 0,
            ResourceKind::Skills => 1,
            ResourceKind::Prompts => 2,
            ResourceKind::Themes => 3,
        }
    }
}

/// The outcome the config selector emits on its channel. A toggle emits one
/// [`ConfigOutcome::Toggled`] and keeps the dialog open; Esc/Ctrl+C emits a terminal
/// outcome and closes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigOutcome {
    /// The user toggled `path` (of `kind`) to `enabled`. The component has already
    /// flipped its in-memory checkbox so the user sees immediate feedback; a driver
    /// should reflect the change in the settings file.
    Toggled {
        path: PathBuf,
        kind: ResourceKind,
        enabled: bool,
    },
    /// The user dismissed the dialog (Esc).
    Cancelled,
    /// The user pressed Ctrl+C — a hard exit, not just a dialog close.
    Exit,
}

/// One resource entry shown in the dialog.
#[derive(Debug, Clone)]
pub struct ResourceItem {
    pub path: PathBuf,
    pub enabled: bool,
    pub kind: ResourceKind,
    pub display_name: String,
    group_key: String,
    group_label: String,
    origin: ResourceOrigin,
    scope: InstallScope,
    source: String,
}

/// One row in the flattened render list.
#[derive(Debug, Clone)]
enum FlatRow {
    Group { label: String },
    Subgroup { label: String },
    Item { item_index: usize },
}

/// The rt-native resource-configuration selector component.
pub struct ConfigSelector {
    items: Vec<ResourceItem>,
    flat: Vec<FlatRow>,
    selected_index: usize,
    title: String,
    tx: mpsc::UnboundedSender<ConfigOutcome>,
    done: DoneSignal,
    nav: NavKeys,
}

impl ConfigSelector {
    /// Build a selector from a [`ResolvedPaths`] snapshot with the default
    /// navigation keys.
    #[must_use]
    pub fn new(
        resolved: &ResolvedPaths,
        tx: mpsc::UnboundedSender<ConfigOutcome>,
        done: DoneSignal,
    ) -> Self {
        Self::with_nav(resolved, tx, done, NavKeys::default())
    }

    /// Build a selector with the given resolved navigation keys.
    #[must_use]
    pub fn with_nav(
        resolved: &ResolvedPaths,
        tx: mpsc::UnboundedSender<ConfigOutcome>,
        done: DoneSignal,
        nav: NavKeys,
    ) -> Self {
        let items = build_items(resolved);
        let flat = build_flat(&items);
        let selected_index = flat
            .iter()
            .position(|row| matches!(row, FlatRow::Item { .. }))
            .unwrap_or(0);
        Self {
            items,
            flat,
            selected_index,
            title: "Resource Configuration".to_string(),
            tx,
            done,
            nav,
        }
    }

    /// Borrow the in-memory item list (post-toggle state included). Useful for tests
    /// and drivers that read state without re-walking the registry.
    #[must_use]
    pub fn items(&self) -> &[ResourceItem] {
        &self.items
    }

    /// The currently-highlighted item, if the cursor is on an item row.
    #[must_use]
    pub fn selected_item(&self) -> Option<&ResourceItem> {
        self.flat
            .get(self.selected_index)
            .and_then(|row| match row {
                FlatRow::Item { item_index } => self.items.get(*item_index),
                _ => None,
            })
    }

    /// Move the cursor to the next item row in `step` direction, skipping headers.
    /// Stays put when no further item exists (clamped ends, no wrap).
    fn move_to_next_item(&mut self, step: isize) {
        let len = self.flat.len() as isize;
        if len == 0 {
            return;
        }
        let mut idx = self.selected_index as isize + step;
        while (0..len).contains(&idx) {
            if matches!(self.flat[idx as usize], FlatRow::Item { .. }) {
                self.selected_index = idx as usize;
                return;
            }
            idx += step;
        }
    }

    /// Toggle the highlighted item's checkbox in place and emit the outcome. A no-op
    /// on a header row.
    fn toggle_selected(&mut self) {
        let item_index = match self.flat.get(self.selected_index) {
            Some(FlatRow::Item { item_index }) => *item_index,
            _ => return,
        };
        let item = &mut self.items[item_index];
        item.enabled = !item.enabled;
        let _ = self.tx.send(ConfigOutcome::Toggled {
            path: item.path.clone(),
            kind: item.kind,
            enabled: item.enabled,
        });
    }

    /// The dialog body as styled lines: the title + hint, then the windowed
    /// group/subgroup/item rows (or the empty placeholder).
    fn body_lines(&self, _width: u16, palette: &ThemePalette) -> Vec<Line<'static>> {
        let accent = Style::default().fg(palette.accent);
        let muted = Style::default().fg(palette.dim);
        let success = Style::default().fg(palette.success);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let bold = Style::default().add_modifier(Modifier::BOLD);

        let mut lines: Vec<Line<'static>> = Vec::new();

        // Title + key hint.
        lines.push(Line::from(Span::styled(
            self.title.clone(),
            accent.add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!(
                "space toggle   {} close",
                super::keys::hint_label(&self.nav.cancel)
            ),
            muted,
        )));
        lines.push(Line::from(String::new()));

        if self.flat.is_empty() {
            lines.push(Line::from(Span::styled(
                "No resources configured".to_string(),
                muted,
            )));
            return lines;
        }

        let (start, end) = self.visible_window();
        for absolute in start..end {
            lines.push(self.render_row(absolute, &self.flat[absolute], palette));
        }

        // Scroll counter when the list overflows the window.
        if start > 0 || end < self.flat.len() {
            let item_count = self
                .flat
                .iter()
                .filter(|r| matches!(r, FlatRow::Item { .. }))
                .count();
            let item_pos = self.flat[..=self.selected_index]
                .iter()
                .filter(|r| matches!(r, FlatRow::Item { .. }))
                .count();
            lines.push(Line::from(Span::styled(
                format!("  ({item_pos}/{item_count})"),
                dim,
            )));
        }

        let _ = (success, bold);
        lines
    }

    /// The visible slice `[start, end)` of the flattened rows, windowed so the
    /// selection stays on screen.
    fn visible_window(&self) -> (usize, usize) {
        let total = self.flat.len();
        if total <= MAX_VISIBLE {
            return (0, total);
        }
        let half = MAX_VISIBLE / 2;
        let start = self
            .selected_index
            .saturating_sub(half)
            .min(total.saturating_sub(MAX_VISIBLE));
        (start, (start + MAX_VISIBLE).min(total))
    }

    /// Render one flattened row (group header, subgroup header, or item).
    fn render_row(&self, absolute: usize, row: &FlatRow, palette: &ThemePalette) -> Line<'static> {
        let accent = Style::default().fg(palette.accent);
        let muted = Style::default().fg(palette.dim);
        let success = Style::default().fg(palette.success);
        let dim = Style::default().add_modifier(Modifier::DIM);

        match row {
            FlatRow::Group { label } => Line::from(Span::styled(
                format!("  {label}"),
                accent.add_modifier(Modifier::BOLD),
            )),
            FlatRow::Subgroup { label } => Line::from(Span::styled(format!("    {label}"), muted)),
            FlatRow::Item { item_index } => {
                let item = &self.items[*item_index];
                let selected = absolute == self.selected_index;
                let cursor = if selected { "> " } else { "  " };
                let (checkbox, checkbox_style) = if item.enabled {
                    ("[x]", success)
                } else {
                    ("[ ]", dim)
                };
                let name_style = if selected {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Line::from(vec![
                    Span::raw(format!("{cursor}    ")),
                    Span::styled(checkbox.to_string(), checkbox_style),
                    Span::raw(" ".to_string()),
                    Span::styled(item.display_name.clone(), name_style),
                ])
            }
        }
    }
}

impl SelectorController for ConfigSelector {
    fn render_lines(&self, width: u16, palette: &ThemePalette) -> Vec<Line<'static>> {
        self.body_lines(width, palette)
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        let Some(id) = key.key_id.as_deref() else {
            // A modal selector owns even a bare-modifier key so it never reaches the
            // editor beneath (VAL-OVERLAY-005).
            return HandleOutcome::Consumed;
        };

        // Ctrl+C is a distinct hard exit (checked before the nav cancel, which may
        // also be bound to escape).
        if id == "ctrl+c" {
            let _ = self.tx.send(ConfigOutcome::Exit);
            self.done.store(true, Ordering::SeqCst);
            return HandleOutcome::Consumed;
        }
        if self.nav.is_up(id) {
            self.move_to_next_item(-1);
        } else if self.nav.is_down(id) {
            self.move_to_next_item(1);
        } else if self.nav.is_cancel(id) {
            let _ = self.tx.send(ConfigOutcome::Cancelled);
            self.done.store(true, Ordering::SeqCst);
        } else if id == "space" || self.nav.is_confirm(id) {
            // Space (or the confirm key) toggles in place — the dialog stays open so
            // the user can toggle several items before dismissing.
            self.toggle_selected();
        }
        // A modal selector owns every key (VAL-OVERLAY-005).
        HandleOutcome::Consumed
    }
}

// ---------------------------------------------------------------------------
// Group / item assembly (ported from the legacy component — pure logic)
// ---------------------------------------------------------------------------

fn group_label(scope: InstallScope, origin: ResourceOrigin, source: &str) -> String {
    match origin {
        ResourceOrigin::Package => format!("{source} ({})", scope.as_str()),
        ResourceOrigin::TopLevel => match (source, scope) {
            ("auto", InstallScope::User) => "User (~/.hand/agent/)".into(),
            ("auto", InstallScope::Project) => "Project (.hand/)".into(),
            (_, InstallScope::User) => "User settings".into(),
            (_, InstallScope::Project) => "Project settings".into(),
            (_, InstallScope::Temporary) => "Temporary (CLI override)".into(),
        },
    }
}

fn display_name(kind: ResourceKind, path: &std::path::Path) -> String {
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent_name = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match kind {
        ResourceKind::Extensions if !parent_name.is_empty() && parent_name != "extensions" => {
            format!("{parent_name}/{file_name}")
        }
        ResourceKind::Skills if file_name == "SKILL.md" => parent_name,
        _ => file_name,
    }
}

fn build_items(resolved: &ResolvedPaths) -> Vec<ResourceItem> {
    let mut items = Vec::new();

    let mut push = |kind: ResourceKind, list: &[ResolvedResource]| {
        for r in list {
            let scope = r.metadata.scope;
            let origin = r.metadata.origin;
            let source = r.metadata.source.clone();
            let group_key = format!(
                "{}:{}:{}",
                match origin {
                    ResourceOrigin::Package => "package",
                    ResourceOrigin::TopLevel => "top-level",
                },
                scope.as_str(),
                source,
            );
            let group_label_str = group_label(scope, origin, &source);
            items.push(ResourceItem {
                path: r.path.clone(),
                enabled: r.enabled,
                kind,
                display_name: display_name(kind, &r.path),
                group_key,
                group_label: group_label_str,
                origin,
                scope,
                source,
            });
        }
    };
    push(ResourceKind::Extensions, &resolved.extensions);
    push(ResourceKind::Skills, &resolved.skills);
    push(ResourceKind::Prompts, &resolved.prompts);
    push(ResourceKind::Themes, &resolved.themes);

    items
}

/// Flatten `items` into render-ready rows with group / subgroup headers inserted:
/// packages before top-level; user before project (then temporary); by source
/// within scope; by resource kind inside each group; alphabetical inside each
/// subgroup.
fn build_flat(items: &[ResourceItem]) -> Vec<FlatRow> {
    if items.is_empty() {
        return Vec::new();
    }

    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    let mut group_order: Vec<String> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        groups
            .entry(item.group_key.clone())
            .or_insert_with(|| {
                group_order.push(item.group_key.clone());
                Vec::new()
            })
            .push(idx);
    }

    group_order.sort_by(|a, b| {
        let ai = &items[groups[a][0]];
        let bi = &items[groups[b][0]];
        let origin_rank = |o: ResourceOrigin| match o {
            ResourceOrigin::Package => 0,
            ResourceOrigin::TopLevel => 1,
        };
        let scope_rank = |s: InstallScope| match s {
            InstallScope::User => 0,
            InstallScope::Project => 1,
            InstallScope::Temporary => 2,
        };
        origin_rank(ai.origin)
            .cmp(&origin_rank(bi.origin))
            .then_with(|| scope_rank(ai.scope).cmp(&scope_rank(bi.scope)))
            .then_with(|| ai.source.cmp(&bi.source))
    });

    let mut flat = Vec::new();
    for group_key in &group_order {
        let indices = &groups[group_key];
        let group_label = items[indices[0]].group_label.clone();
        flat.push(FlatRow::Group { label: group_label });

        let mut by_kind: HashMap<ResourceKind, Vec<usize>> = HashMap::new();
        for &i in indices {
            by_kind.entry(items[i].kind).or_default().push(i);
        }
        let mut kinds: Vec<ResourceKind> = by_kind.keys().copied().collect();
        kinds.sort_by_key(|k| k.order());
        for k in kinds {
            flat.push(FlatRow::Subgroup {
                label: k.label().to_string(),
            });
            let mut item_indices = by_kind.remove(&k).expect("kind known");
            item_indices.sort_by(|&a, &b| items[a].display_name.cmp(&items[b].display_name));
            for ii in item_indices {
                flat.push(FlatRow::Item { item_index: ii });
            }
        }
    }
    flat
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extensions::source_registry::PathMetadata;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn make_resolved(
        spec: &[(&str, ResourceKind, InstallScope, ResourceOrigin, &str, bool)],
    ) -> ResolvedPaths {
        let mut paths = ResolvedPaths::default();
        for (p, kind, scope, origin, source, enabled) in spec.iter().copied() {
            let res = ResolvedResource {
                path: PathBuf::from(p),
                enabled,
                metadata: PathMetadata {
                    source: source.to_string(),
                    scope,
                    origin,
                    base_dir: None,
                },
            };
            match kind {
                ResourceKind::Extensions => paths.extensions.push(res),
                ResourceKind::Skills => paths.skills.push(res),
                ResourceKind::Prompts => paths.prompts.push(res),
                ResourceKind::Themes => paths.themes.push(res),
            }
        }
        paths
    }

    fn key_id(id: &str) -> RtKey {
        RtKey {
            key_id: Some(id.to_string()),
            raw: KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        }
    }

    fn selector(
        resolved: &ResolvedPaths,
    ) -> (
        ConfigSelector,
        mpsc::UnboundedReceiver<ConfigOutcome>,
        DoneSignal,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let done = super::super::overlay::new_done_signal();
        let sel = ConfigSelector::new(resolved, tx, done.clone());
        (sel, rx, done)
    }

    fn body_text(sel: &ConfigSelector) -> String {
        sel.body_lines(80, &ThemePalette::default())
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

    fn drain(rx: &mut mpsc::UnboundedReceiver<ConfigOutcome>) -> Option<ConfigOutcome> {
        rx.try_recv().ok()
    }

    // --- rendering -------------------------------------------------------

    #[test]
    fn empty_resolved_renders_placeholder() {
        let (sel, _rx, _done) = selector(&ResolvedPaths::default());
        assert!(body_text(&sel).contains("No resources configured"));
    }

    #[test]
    fn renders_group_subgroup_and_item_rows() {
        let resolved = make_resolved(&[
            (
                "/proj/.hand/skills/my-skill/SKILL.md",
                ResourceKind::Skills,
                InstallScope::Project,
                ResourceOrigin::TopLevel,
                "auto",
                true,
            ),
            (
                "/proj/.hand/prompts/foo.md",
                ResourceKind::Prompts,
                InstallScope::Project,
                ResourceOrigin::TopLevel,
                "auto",
                false,
            ),
        ]);
        let (sel, _rx, _done) = selector(&resolved);
        let body = body_text(&sel);
        assert!(body.contains("Project (.hand/)"), "{body}");
        assert!(body.contains("Skills"), "{body}");
        assert!(body.contains("Prompts"), "{body}");
        // Skill display uses the parent dir name when the filename is SKILL.md.
        assert!(body.contains("my-skill"), "{body}");
        assert!(body.contains("foo.md"), "{body}");
    }

    #[test]
    fn package_groups_render_before_top_level() {
        let resolved = make_resolved(&[
            (
                "/proj/.hand/extensions/local.ts",
                ResourceKind::Extensions,
                InstallScope::Project,
                ResourceOrigin::TopLevel,
                "auto",
                true,
            ),
            (
                "/agent/npm/node_modules/foo/extensions/index.ts",
                ResourceKind::Extensions,
                InstallScope::User,
                ResourceOrigin::Package,
                "npm:foo",
                true,
            ),
        ]);
        let (sel, _rx, _done) = selector(&resolved);
        let lines: Vec<String> = sel
            .body_lines(80, &ThemePalette::default())
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let pkg_idx = lines
            .iter()
            .position(|l| l.contains("npm:foo"))
            .expect("package header rendered");
        let proj_idx = lines
            .iter()
            .position(|l| l.contains("Project (.hand/)"))
            .expect("project header rendered");
        assert!(pkg_idx < proj_idx, "packages must sort before top-level");
    }

    // --- navigation (clamped, skips headers) -----------------------------

    #[test]
    fn down_advances_to_next_item_skipping_headers() {
        let resolved = make_resolved(&[
            (
                "/a.md",
                ResourceKind::Skills,
                InstallScope::Project,
                ResourceOrigin::TopLevel,
                "auto",
                true,
            ),
            (
                "/b.md",
                ResourceKind::Skills,
                InstallScope::Project,
                ResourceOrigin::TopLevel,
                "auto",
                true,
            ),
        ]);
        let (mut sel, _rx, _done) = selector(&resolved);
        assert_eq!(sel.selected_item().unwrap().path, PathBuf::from("/a.md"));
        sel.handle_key(&key_id("down"));
        assert_eq!(sel.selected_item().unwrap().path, PathBuf::from("/b.md"));
    }

    #[test]
    fn up_clamps_at_first_item() {
        let resolved = make_resolved(&[(
            "/a.md",
            ResourceKind::Skills,
            InstallScope::Project,
            ResourceOrigin::TopLevel,
            "auto",
            true,
        )]);
        let (mut sel, _rx, _done) = selector(&resolved);
        sel.handle_key(&key_id("up"));
        assert_eq!(sel.selected_item().unwrap().path, PathBuf::from("/a.md"));
    }

    // --- toggle (VAL-CHAT-037) -------------------------------------------

    #[test]
    fn space_emits_toggle_and_flips_in_memory_state() {
        let resolved = make_resolved(&[(
            "/skills/alpha.md",
            ResourceKind::Skills,
            InstallScope::Project,
            ResourceOrigin::TopLevel,
            "auto",
            true,
        )]);
        let (mut sel, mut rx, done) = selector(&resolved);
        sel.handle_key(&key_id("space"));
        match drain(&mut rx) {
            Some(ConfigOutcome::Toggled {
                path,
                kind,
                enabled,
            }) => {
                assert_eq!(path, PathBuf::from("/skills/alpha.md"));
                assert_eq!(kind, ResourceKind::Skills);
                assert!(!enabled, "toggling an enabled item disables it");
            }
            other => panic!("expected Toggled, got {other:?}"),
        }
        // The in-memory state flips so the next render shows the new checkbox.
        assert!(!sel.items()[0].enabled);
        // A toggle keeps the dialog open.
        assert!(
            !done.load(Ordering::SeqCst),
            "a toggle must not close the dialog"
        );
    }

    #[test]
    fn toggle_flips_checkbox_glyph_in_render() {
        let resolved = make_resolved(&[(
            "/skills/alpha.md",
            ResourceKind::Skills,
            InstallScope::Project,
            ResourceOrigin::TopLevel,
            "auto",
            true,
        )]);
        let (mut sel, _rx, _done) = selector(&resolved);
        assert!(body_text(&sel).contains("[x]"), "enabled shows [x]");
        sel.handle_key(&key_id("space"));
        assert!(
            body_text(&sel).contains("[ ]"),
            "toggling shows the unchecked box immediately"
        );
    }

    // --- terminal keys (VAL-CHAT-037: clean exit) ------------------------

    #[test]
    fn escape_emits_cancelled_and_raises_done() {
        let resolved = make_resolved(&[(
            "/a.md",
            ResourceKind::Skills,
            InstallScope::Project,
            ResourceOrigin::TopLevel,
            "auto",
            true,
        )]);
        let (mut sel, mut rx, done) = selector(&resolved);
        sel.handle_key(&key_id("escape"));
        assert!(done.load(Ordering::SeqCst), "escape raises the done flag");
        assert_eq!(drain(&mut rx), Some(ConfigOutcome::Cancelled));
    }

    #[test]
    fn ctrl_c_emits_exit_and_raises_done() {
        let resolved = make_resolved(&[(
            "/a.md",
            ResourceKind::Skills,
            InstallScope::Project,
            ResourceOrigin::TopLevel,
            "auto",
            true,
        )]);
        let (mut sel, mut rx, done) = selector(&resolved);
        sel.handle_key(&key_id("ctrl+c"));
        assert!(done.load(Ordering::SeqCst), "ctrl+c raises the done flag");
        assert_eq!(drain(&mut rx), Some(ConfigOutcome::Exit));
    }

    #[test]
    fn a_modal_key_is_consumed() {
        let resolved = make_resolved(&[(
            "/a.md",
            ResourceKind::Skills,
            InstallScope::Project,
            ResourceOrigin::TopLevel,
            "auto",
            true,
        )]);
        let (mut sel, _rx, _done) = selector(&resolved);
        // A bare-modifier key (no id) is still owned by the modal selector.
        let bare = RtKey {
            key_id: None,
            raw: KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        };
        assert_eq!(sel.handle_key(&bare), HandleOutcome::Consumed);
    }
}
