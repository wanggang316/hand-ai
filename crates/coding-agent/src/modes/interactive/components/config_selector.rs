//! Resource configuration TUI dialog.
//!
//! Renders the resolved resources from
//! [`crate::core::extensions::source_registry::ResolvedPaths`] grouped by
//! configured package source, then by resource kind. The user navigates
//! the flat list with the standard `tui.select.up`/`tui.select.down`
//! keybindings and toggles enabled state on the highlighted item with
//! `space` (or `tui.select.confirm`).
//!
//! ## Action surface vs. underlying capabilities
//!
//! The TS reference reaches into `SettingsManager` to push an override
//! pattern (`+/-/!`-prefixed) into the appropriate list and persist it.
//! In the Rust port:
//!
//! - The component **always** updates its in-memory `enabled` flag for
//!   the toggled item so the user sees immediate feedback.
//! - It surfaces a [`ConfigSelectorEvent::ToggleRequested`] event over
//!   the supplied [`mpsc::UnboundedSender`]. The driver is responsible
//!   for translating that into the right call against
//!   `core::extensions::source_registry`. The registry's
//!   `add_source_to_settings` / `remove_source_from_settings` (and the
//!   matching `install_and_persist` / `remove_and_persist` /
//!   `update`) are now backed by real settings I/O and npm/git
//!   shell-out; failure modes from the underlying calls should be
//!   surfaced as a toast rather than swallowed.
//!
//! Driver wiring for install / remove / update key bindings (extra
//! `ConfigSelectorEvent` variants, an in-component menu) is still
//! follow-up work — the registry capability is in place, the dialog
//! does not yet expose it. See `parity-completion.md`.

use std::collections::HashMap;

use hand_tui::Component;
use hand_tui::keybindings::{Keybinding, get_keybindings};
use hand_tui::tui::{HandleResult, InputEvent};
use hand_tui::utils::{truncate_to_width, visible_width};
use tokio::sync::mpsc;

use super::dynamic_border::DynamicBorderComponent;
use super::keybinding_hints::raw_key_hint;
use crate::core::extensions::source_registry::{
    InstallScope, ResolvedPaths, ResolvedResource, ResourceOrigin,
};

const ACCENT: &str = "\x1b[36m";
const SUCCESS: &str = "\x1b[32m";
const MUTED: &str = "\x1b[90m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

/// One of the four pi-extension resource kinds.
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

    /// Stable order used by [`ConfigSelectorComponent`] when sorting
    /// subgroups within a group.
    fn order(self) -> u8 {
        match self {
            ResourceKind::Extensions => 0,
            ResourceKind::Skills => 1,
            ResourceKind::Prompts => 2,
            ResourceKind::Themes => 3,
        }
    }
}

/// One resource entry shown in the dialog.
#[derive(Debug, Clone)]
pub struct ResourceItem {
    pub path: std::path::PathBuf,
    pub enabled: bool,
    pub kind: ResourceKind,
    pub display_name: String,
    /// Group key — `<origin>:<scope>:<source>`. Used by the renderer
    /// to insert a single header row per group.
    pub group_key: String,
    pub group_label: String,
    /// Origin from the underlying `PathMetadata` — packages are sorted
    /// before top-level groups.
    pub origin: ResourceOrigin,
    pub scope: InstallScope,
    pub source: String,
}

/// Outcome surfaced via the events channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSelectorEvent {
    /// User toggled `path` (of `kind`) to `enabled`. The component has
    /// already updated its visual state; the driver should reflect the
    /// change in `Settings` (currently best-effort — see module docs).
    ToggleRequested {
        path: std::path::PathBuf,
        kind: ResourceKind,
        enabled: bool,
    },
    /// User dismissed the dialog (`tui.select.cancel` → escape).
    Cancelled,
    /// User pressed `ctrl+c` while focused. The driver is expected to
    /// teardown the wider TUI session, not just close the dialog.
    Exit,
}

/// One row in the flattened render list.
#[derive(Debug, Clone)]
enum FlatRow {
    Group { label: String },
    Subgroup { label: String },
    Item { item_index: usize },
}

/// Resource-configuration dialog. See module docs.
pub struct ConfigSelectorComponent {
    items: Vec<ResourceItem>,
    flat: Vec<FlatRow>,
    selected_index: usize,
    border: DynamicBorderComponent,
    events: mpsc::UnboundedSender<ConfigSelectorEvent>,
    max_visible: usize,
    title: String,
}

impl ConfigSelectorComponent {
    /// Build a dialog from a [`ResolvedPaths`] snapshot. The snapshot is
    /// typically obtained from
    /// [`crate::core::extensions::source_registry::SourceRegistry::resolve`].
    pub fn new(
        resolved: &ResolvedPaths,
        events: mpsc::UnboundedSender<ConfigSelectorEvent>,
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
            border: DynamicBorderComponent::new(),
            events,
            max_visible: 15,
            title: "Resource Configuration".into(),
        }
    }

    /// Override the default max-visible row count (15).
    pub fn with_max_visible(mut self, max_visible: usize) -> Self {
        self.max_visible = max_visible.max(1);
        self
    }

    /// Borrow the in-memory item list. Useful for tests and for drivers
    /// that want to read post-toggle state without re-walking the
    /// source registry.
    pub fn items(&self) -> &[ResourceItem] {
        &self.items
    }

    /// Currently-highlighted item index, if the cursor is on an item
    /// row (rather than a group/subgroup header).
    pub fn selected_item(&self) -> Option<&ResourceItem> {
        self.flat
            .get(self.selected_index)
            .and_then(|row| match row {
                FlatRow::Item { item_index } => self.items.get(*item_index),
                _ => None,
            })
    }

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
        // No further item — stay where we are.
    }

    fn jump_by_pages(&mut self, step: isize) {
        let len = self.flat.len() as isize;
        let mut target =
            (self.selected_index as isize + step * self.max_visible as isize).clamp(0, len - 1);
        // Snap to the nearest item row. Search in the same direction first.
        let mut probe = target;
        while (0..len).contains(&probe) {
            if matches!(self.flat[probe as usize], FlatRow::Item { .. }) {
                self.selected_index = probe as usize;
                return;
            }
            probe += step.signum();
        }
        // Fall back to scanning the opposite direction.
        probe = target;
        while (0..len).contains(&probe) {
            if matches!(self.flat[probe as usize], FlatRow::Item { .. }) {
                self.selected_index = probe as usize;
                return;
            }
            probe -= step.signum();
            target = probe;
        }
        let _ = target;
    }

    fn toggle_selected(&mut self) {
        let item_index = match self.flat.get(self.selected_index) {
            Some(FlatRow::Item { item_index }) => *item_index,
            _ => return,
        };
        let item = &mut self.items[item_index];
        item.enabled = !item.enabled;
        let _ = self.events.send(ConfigSelectorEvent::ToggleRequested {
            path: item.path.clone(),
            kind: item.kind,
            enabled: item.enabled,
        });
    }

    fn raw_key(event: &InputEvent) -> Option<&str> {
        match event {
            InputEvent::Raw(s) | InputEvent::Paste(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

impl Component for ConfigSelectorComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let mut out = Vec::new();
        out.extend(self.border.render(width));
        out.push(String::new());
        // Header line: title left, hints right, padded to width.
        let title_part = format!("{BOLD}{}{RESET}", self.title);
        let hint = format!(
            "{}  {}",
            raw_key_hint("space", "toggle"),
            raw_key_hint("esc", "close"),
        );
        out.push(render_header_line(&title_part, &hint, width));
        out.push(String::new());

        if self.flat.is_empty() {
            out.push(pad_line(
                &format!("  {MUTED}No resources configured{RESET}"),
                width,
            ));
            out.push(String::new());
            out.extend(self.border.render(width));
            return out;
        }

        // Compute the visible window centred on the selection.
        let total = self.flat.len();
        let half = self.max_visible / 2;
        let start = self
            .selected_index
            .saturating_sub(half)
            .min(total.saturating_sub(self.max_visible));
        let end = (start + self.max_visible).min(total);

        for (i, row) in self.flat[start..end].iter().enumerate() {
            let absolute = start + i;
            let line = self.render_row(absolute, row);
            out.push(pad_line(&line, width));
        }

        // Scroll counter when not the entire list is visible.
        if start > 0 || end < total {
            let item_count = self
                .flat
                .iter()
                .filter(|r| matches!(r, FlatRow::Item { .. }))
                .count();
            let item_pos = self.flat[..=self.selected_index]
                .iter()
                .filter(|r| matches!(r, FlatRow::Item { .. }))
                .count();
            out.push(pad_line(
                &format!("  {DIM}({item_pos}/{item_count}){RESET}"),
                width,
            ));
        }

        out.push(String::new());
        out.extend(self.border.render(width));
        out
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        let Some(data) = Self::raw_key(event) else {
            return HandleResult::Ignored;
        };
        let kb = get_keybindings();

        if kb.matches(data, Keybinding::SelectUp) {
            self.move_to_next_item(-1);
            return HandleResult::Handled;
        }
        if kb.matches(data, Keybinding::SelectDown) {
            self.move_to_next_item(1);
            return HandleResult::Handled;
        }
        if kb.matches(data, Keybinding::SelectPageUp) {
            self.jump_by_pages(-1);
            return HandleResult::Handled;
        }
        if kb.matches(data, Keybinding::SelectPageDown) {
            self.jump_by_pages(1);
            return HandleResult::Handled;
        }
        // Check ctrl+c first — `tui.select.cancel` is bound to escape
        // *and* ctrl+c by default in the TS reference, but we want a
        // distinct `Exit` outcome for the latter so the driver can
        // differentiate between "close dialog" and "tear down session".
        if data == "\x03" {
            let _ = self.events.send(ConfigSelectorEvent::Exit);
            return HandleResult::Handled;
        }
        if kb.matches(data, Keybinding::SelectCancel) {
            let _ = self.events.send(ConfigSelectorEvent::Cancelled);
            return HandleResult::Handled;
        }
        if data == " " || kb.matches(data, Keybinding::SelectConfirm) {
            self.toggle_selected();
            return HandleResult::Handled;
        }

        HandleResult::Ignored
    }
}

impl ConfigSelectorComponent {
    fn render_row(&self, absolute: usize, row: &FlatRow) -> String {
        match row {
            FlatRow::Group { label } => {
                format!("  {ACCENT}{BOLD}{label}{RESET}")
            }
            FlatRow::Subgroup { label } => {
                format!("    {MUTED}{label}{RESET}")
            }
            FlatRow::Item { item_index } => {
                let item = &self.items[*item_index];
                let selected = absolute == self.selected_index;
                let cursor = if selected { "> " } else { "  " };
                let checkbox = if item.enabled {
                    format!("{SUCCESS}[x]{RESET}")
                } else {
                    format!("{DIM}[ ]{RESET}")
                };
                let name = if selected {
                    format!("{BOLD}{}{RESET}", item.display_name)
                } else {
                    item.display_name.clone()
                };
                format!("{cursor}    {checkbox} {name}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Group / item assembly
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

/// Flatten `items` into a render-ready row list with `Group` / `Subgroup`
/// headers inserted. Sort order mirrors the TS:
/// 1. packages before top-level
/// 2. user before project (then temporary)
/// 3. by source string within scope
/// 4. by [`ResourceKind::order`] inside each group
/// 5. alphabetical by display name inside each subgroup
fn build_flat(items: &[ResourceItem]) -> Vec<FlatRow> {
    if items.is_empty() {
        return Vec::new();
    }

    // Bucket by group_key.
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

    // Sort group order.
    group_order.sort_by(|a, b| {
        let ai = items[groups[a][0]].clone();
        let bi = items[groups[b][0]].clone();
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

        // Within a group, sub-bucket by kind.
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

fn render_header_line(title: &str, hint: &str, width: u16) -> String {
    let target = width as usize;
    let title_w = visible_width(title);
    let hint_w = visible_width(hint);
    if title_w + hint_w + 1 >= target {
        return truncate_to_width(title, target);
    }
    let gap = target - title_w - hint_w;
    format!("{title}{}{hint}", " ".repeat(gap))
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
    use crate::core::extensions::source_registry::{
        InstallScope, PathMetadata, ResolvedResource, ResourceOrigin,
    };
    use std::path::PathBuf;

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

    fn key(s: &str) -> InputEvent {
        InputEvent::Raw(s.to_string())
    }

    #[test]
    fn empty_resolved_renders_placeholder() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let comp = ConfigSelectorComponent::new(&ResolvedPaths::default(), tx);
        let lines = comp.render(40);
        assert!(lines.iter().any(|l| l.contains("No resources configured")));
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
        let (tx, _rx) = mpsc::unbounded_channel();
        let comp = ConfigSelectorComponent::new(&resolved, tx);
        let lines = comp.render(80);
        assert!(lines.iter().any(|l| l.contains("Project (.hand/)")));
        assert!(lines.iter().any(|l| l.contains("Skills")));
        assert!(lines.iter().any(|l| l.contains("Prompts")));
        // Skill display uses the parent dir name when filename is SKILL.md.
        assert!(lines.iter().any(|l| l.contains("my-skill")));
        assert!(lines.iter().any(|l| l.contains("foo.md")));
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
        let (tx, _rx) = mpsc::unbounded_channel();
        let comp = ConfigSelectorComponent::new(&resolved, tx);
        let lines = comp.render(80);
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

    #[test]
    fn down_arrow_advances_to_next_item_skipping_headers() {
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
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut comp = ConfigSelectorComponent::new(&resolved, tx);
        let first = comp.selected_item().unwrap().path.clone();
        assert_eq!(first, PathBuf::from("/a.md"));
        comp.handle_input(&key("\x1b[B")); // ANSI down arrow
        assert_eq!(comp.selected_item().unwrap().path, PathBuf::from("/b.md"));
    }

    #[test]
    fn up_arrow_clamps_at_first_item() {
        let resolved = make_resolved(&[(
            "/a.md",
            ResourceKind::Skills,
            InstallScope::Project,
            ResourceOrigin::TopLevel,
            "auto",
            true,
        )]);
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut comp = ConfigSelectorComponent::new(&resolved, tx);
        comp.handle_input(&key("\x1b[A"));
        assert_eq!(comp.selected_item().unwrap().path, PathBuf::from("/a.md"));
    }

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
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut comp = ConfigSelectorComponent::new(&resolved, tx);
        comp.handle_input(&key(" "));
        let evt = rx.try_recv().unwrap();
        match evt {
            ConfigSelectorEvent::ToggleRequested {
                path,
                kind,
                enabled,
            } => {
                assert_eq!(path, PathBuf::from("/skills/alpha.md"));
                assert_eq!(kind, ResourceKind::Skills);
                assert!(!enabled, "toggling an enabled item disables it");
            }
            other => panic!("unexpected event: {other:?}"),
        }
        // In-memory state should reflect the toggle so subsequent renders
        // show the new check-state immediately.
        assert!(!comp.items()[0].enabled);
    }

    #[test]
    fn escape_emits_cancelled() {
        let resolved = make_resolved(&[(
            "/a.md",
            ResourceKind::Skills,
            InstallScope::Project,
            ResourceOrigin::TopLevel,
            "auto",
            true,
        )]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut comp = ConfigSelectorComponent::new(&resolved, tx);
        comp.handle_input(&key("\x1b"));
        assert_eq!(rx.try_recv().unwrap(), ConfigSelectorEvent::Cancelled);
    }

    #[test]
    fn ctrl_c_emits_exit_event() {
        let resolved = make_resolved(&[(
            "/a.md",
            ResourceKind::Skills,
            InstallScope::Project,
            ResourceOrigin::TopLevel,
            "auto",
            true,
        )]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut comp = ConfigSelectorComponent::new(&resolved, tx);
        comp.handle_input(&key("\x03"));
        assert_eq!(rx.try_recv().unwrap(), ConfigSelectorEvent::Exit);
    }

    #[test]
    fn extensions_subdir_display_uses_parent_prefix() {
        // For extensions whose parent directory name is *not*
        // "extensions", prefix the display with the parent name so the
        // user can tell sibling extensions apart.
        let resolved = make_resolved(&[(
            "/agent/npm/node_modules/foo/extensions/sub/ext.ts",
            ResourceKind::Extensions,
            InstallScope::User,
            ResourceOrigin::Package,
            "npm:foo",
            true,
        )]);
        let (tx, _rx) = mpsc::unbounded_channel();
        let comp = ConfigSelectorComponent::new(&resolved, tx);
        let lines = comp.render(80);
        assert!(
            lines.iter().any(|l| l.contains("sub/ext.ts")),
            "expected 'sub/ext.ts' in render, got {lines:?}",
        );
    }

    #[test]
    fn entries_within_subgroup_sorted_alphabetically() {
        let resolved = make_resolved(&[
            (
                "/zeta.md",
                ResourceKind::Skills,
                InstallScope::Project,
                ResourceOrigin::TopLevel,
                "auto",
                true,
            ),
            (
                "/alpha.md",
                ResourceKind::Skills,
                InstallScope::Project,
                ResourceOrigin::TopLevel,
                "auto",
                true,
            ),
        ]);
        let (tx, _rx) = mpsc::unbounded_channel();
        let comp = ConfigSelectorComponent::new(&resolved, tx);
        let lines = comp.render(40);
        let alpha_idx = lines
            .iter()
            .position(|l| l.contains("alpha.md"))
            .expect("alpha rendered");
        let zeta_idx = lines
            .iter()
            .position(|l| l.contains("zeta.md"))
            .expect("zeta rendered");
        assert!(alpha_idx < zeta_idx, "alpha must precede zeta");
    }

    #[test]
    fn items_accessor_returns_full_list() {
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
                InstallScope::User,
                ResourceOrigin::TopLevel,
                "auto",
                false,
            ),
        ]);
        let (tx, _rx) = mpsc::unbounded_channel();
        let comp = ConfigSelectorComponent::new(&resolved, tx);
        assert_eq!(comp.items().len(), 2);
    }
}
