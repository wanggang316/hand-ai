//! The rt-native `/tree` selector — the filesystem directory picker built on the
//! [overlay runtime](super::overlay).
//!
//! It is a [`SelectorController`]: it produces rt-native styled [`Line`]s and
//! consumes keys while it is the mounted modal overlay. Its behaviour is the
//! directory-tree picker's:
//!
//! - the list is the directory contents scanned from a root (`cwd`, or a `<subdir>`
//!   argument), **directories first in alphabetical order**, then files
//!   alphabetically (VAL-OVERLAY-024);
//! - a directory row carries a trailing `/`, and its children are shown **indented**
//!   one depth below it;
//! - **noise directories** (`.git`, `target`, `node_modules`) never appear and are
//!   not descended into;
//! - **↑/↓ navigate with clamp** (a bounded filesystem list reads more naturally
//!   with clamped ends — the per-selector nav nail VAL-OVERLAY-002 pins `/tree` as
//!   *clamp*);
//! - **Enter** confirms the highlighted row's relative path, **Esc** cancels — each
//!   emits exactly one [`TreeOutcome`] on the outcome channel and raises the
//!   [`DoneSignal`](super::overlay::DoneSignal) so the runtime unmounts it.
//!
//! The driver owns the filesystem scan (so a non-directory `<subdir>` argument lands
//! the no-data status line, VAL-OVERLAY-019) and the `[/tree picked: <path>]` status
//! line; this component is pure UI + pick logic over its constructor inputs — the
//! reusable construct-in / channel-out selector shape.

use std::path::Path;
use std::sync::atomic::Ordering;

use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::HandleOutcome;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::mpsc;

use super::keys::NavKeys;
use super::overlay::{DoneSignal, SelectorController};

/// The most rows shown at once; the window scrolls to keep the selection visible.
const MAX_VISIBLE: usize = 16;

/// Directory names that are never listed and never descended into — build output,
/// VCS metadata, and dependency trees that would drown the picker in noise.
pub const NOISE_DIRS: &[&str] = &[".git", "target", "node_modules"];

/// One flattened row of the directory tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    /// The path relative to the scan root (POSIX-style, `/`-separated). This is the
    /// value emitted on Enter and shown in the `[/tree picked: <path>]` status line.
    pub rel_path: String,
    /// The final path component, rendered after the indent gutter. A directory
    /// carries a trailing `/`.
    pub label: String,
    /// Indent depth — `0` for the scan root's direct children.
    pub depth: usize,
    /// Whether the entry is a directory (drives the trailing `/` and the accent).
    pub is_dir: bool,
}

/// Whether `name` is a noise directory that must be skipped (case-sensitive, matching
/// the on-disk names).
#[must_use]
pub fn is_noise_dir(name: &str) -> bool {
    NOISE_DIRS.contains(&name)
}

/// Scan `root` into a flattened, depth-first tree of [`TreeRow`]s.
///
/// At each level the entries are ordered **directories first (alphabetically), then
/// files (alphabetically)** (VAL-OVERLAY-024). Directories are descended into
/// (their children indented one depth below), except the [noise
/// directories](NOISE_DIRS), which are skipped entirely — they neither appear nor are
/// recursed into. Hidden entries other than the noise set are kept (a `.hidden` file
/// is a legitimate pick). An unreadable directory contributes no rows (it is skipped,
/// not an error), so a permission-denied subtree degrades gracefully.
///
/// Kept as a pure function over a path so the ordering / skip / indent rules are
/// unit-testable against a temp dir without a running overlay.
#[must_use]
pub fn scan_tree(root: &Path) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    scan_into(root, "", 0, &mut rows);
    rows
}

/// Recursive worker for [`scan_tree`]: read `dir`, order its entries dirs-first, and
/// push each as a row (recursing into non-noise directories). `prefix` is the
/// relative path of `dir` from the scan root (empty at the root).
fn scan_into(dir: &Path, prefix: &str, depth: usize, rows: &mut Vec<TreeRow>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };

    // Collect (name, is_dir), dropping noise directories up front so they neither
    // list nor recurse.
    let mut entries: Vec<(String, bool)> = read
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir && is_noise_dir(&name) {
                return None;
            }
            Some((name, is_dir))
        })
        .collect();

    // Directories first, then files; each group alphabetical (case-insensitive so
    // the order reads naturally regardless of casing).
    entries.sort_by(|(a_name, a_dir), (b_name, b_dir)| {
        b_dir
            .cmp(a_dir)
            .then_with(|| a_name.to_lowercase().cmp(&b_name.to_lowercase()))
    });

    for (name, is_dir) in entries {
        let rel_path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let label = if is_dir {
            format!("{name}/")
        } else {
            name.clone()
        };
        rows.push(TreeRow {
            rel_path: rel_path.clone(),
            label,
            depth,
            is_dir,
        });
        if is_dir {
            let child_dir = dir.join(&name);
            scan_into(&child_dir, &rel_path, depth + 1, rows);
        }
    }
}

/// The outcome the selector emits on its channel — exactly one per open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeOutcome {
    /// The user confirmed this relative path (Enter).
    Selected(String),
    /// The user cancelled (Esc) — nothing is picked.
    Cancelled,
}

/// The rt-native `/tree` directory picker component.
pub struct TreeSelector {
    /// The flattened, dirs-first tree rows, in render order.
    rows: Vec<TreeRow>,
    /// The dialog title (shows the scanned root, relative to cwd).
    title: String,
    /// The highlighted row (index into `rows`).
    selected: usize,
    /// The outcome channel; exactly one [`TreeOutcome`] is sent on confirm/cancel.
    tx: mpsc::UnboundedSender<TreeOutcome>,
    /// Raised on the terminal key (Enter/Esc) so the overlay runtime unmounts this.
    done: DoneSignal,
    /// The resolved navigation keys, snapshotted from the live app-layer table
    /// when the selector mounted, so a user remap drives navigation + the hint
    /// (VAL-OVERLAY-021).
    nav: NavKeys,
}

impl TreeSelector {
    /// Build a selector over `rows` under `title` with the default navigation keys.
    #[must_use]
    pub fn new(
        rows: Vec<TreeRow>,
        title: impl Into<String>,
        tx: mpsc::UnboundedSender<TreeOutcome>,
        done: DoneSignal,
    ) -> Self {
        Self::with_nav(rows, title, tx, done, NavKeys::default())
    }

    /// Build a selector with the given resolved navigation keys.
    #[must_use]
    pub fn with_nav(
        rows: Vec<TreeRow>,
        title: impl Into<String>,
        tx: mpsc::UnboundedSender<TreeOutcome>,
        done: DoneSignal,
        nav: NavKeys,
    ) -> Self {
        Self {
            rows,
            title: title.into(),
            selected: 0,
            tx,
            done,
            nav,
        }
    }

    /// The highlighted row, if the tree is non-empty (test/introspection aid).
    #[must_use]
    pub fn highlighted(&self) -> Option<&TreeRow> {
        self.rows.get(self.selected)
    }

    /// The highlighted row index (test aid).
    #[must_use]
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Whether the tree has no rows (the `(empty)` state).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Move the cursor up one row (clamped at the top — `/tree` is a clamp selector,
    /// VAL-OVERLAY-002).
    fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move the cursor down one row (clamped at the bottom).
    fn move_down(&mut self) {
        if self.selected + 1 < self.rows.len() {
            self.selected += 1;
        }
    }

    /// Emit the highlighted row's relative path (Enter). A no-op when the tree is
    /// empty — there is nothing to pick, so the picker stays open.
    fn confirm(&self) -> bool {
        if let Some(row) = self.rows.get(self.selected) {
            let _ = self.tx.send(TreeOutcome::Selected(row.rel_path.clone()));
            true
        } else {
            false
        }
    }

    /// Emit the cancel outcome (Esc) — nothing is picked.
    fn cancel(&self) {
        let _ = self.tx.send(TreeOutcome::Cancelled);
    }

    /// The visible slice `[start, end)`, windowed so the selection stays on screen.
    fn visible_window(&self) -> (usize, usize) {
        let count = self.rows.len();
        if count <= MAX_VISIBLE {
            return (0, count);
        }
        let half = MAX_VISIBLE / 2;
        let start = self
            .selected
            .saturating_sub(half)
            .min(count.saturating_sub(MAX_VISIBLE));
        (start, (start + MAX_VISIBLE).min(count))
    }

    /// The tree body rendered as styled lines (title, windowed rows or the empty
    /// placeholder, and the key hint), wrapped to `width`.
    fn body_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let muted = Style::default().fg(Color::DarkGray);
        let accent = Style::default().fg(Color::Cyan);
        let dir_style = Style::default().fg(Color::Blue);

        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(Span::styled(
            self.title.clone(),
            accent.add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(String::new()));

        if self.rows.is_empty() {
            lines.push(Line::from(Span::styled(
                "(empty directory)".to_string(),
                muted,
            )));
        } else {
            let (start, end) = self.visible_window();
            for i in start..end {
                let row = &self.rows[i];
                let is_selected = i == self.selected;
                let indent = "  ".repeat(row.depth);

                let mut spans: Vec<Span<'static>> = Vec::new();
                if is_selected {
                    spans.push(Span::styled(format!("→ {indent}"), accent));
                    spans.push(Span::styled(
                        row.label.clone(),
                        accent.add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::raw(format!("  {indent}")));
                    let style = if row.is_dir {
                        dir_style
                    } else {
                        Style::default()
                    };
                    spans.push(Span::styled(row.label.clone(), style));
                }
                lines.push(Line::from(spans));
            }

            let count = self.rows.len();
            if end - start < count {
                lines.push(Line::from(Span::styled(
                    format!("  ({}/{})", self.selected + 1, count),
                    muted,
                )));
            }
        }

        lines.push(Line::from(String::new()));
        lines.push(Line::from(Span::styled(
            self.nav.hint_line("pick", "cancel"),
            muted,
        )));
        lines
    }
}

impl SelectorController for TreeSelector {
    fn render_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.body_lines(width)
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        // Navigation is resolved against the snapshotted app-layer keys so a user
        // remap (e.g. `select-down: j`) drives the picker (VAL-OVERLAY-021).
        let Some(id) = key.key_id.as_deref() else {
            // A modal selector owns every key so none reaches the editor beneath
            // (VAL-OVERLAY-005), even a bare-modifier key it does not act on.
            return HandleOutcome::Consumed;
        };
        if self.nav.is_up(id) {
            self.move_up();
        } else if self.nav.is_down(id) {
            self.move_down();
        } else if self.nav.is_confirm(id) {
            // Enter on an empty tree is inert: nothing to pick, so the picker
            // stays open and the done flag is not raised.
            if self.confirm() {
                self.done.store(true, Ordering::SeqCst);
            }
        } else if self.nav.is_cancel(id) {
            self.cancel();
            self.done.store(true, Ordering::SeqCst);
        }
        // Every key is consumed regardless (VAL-OVERLAY-005).
        HandleOutcome::Consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key_id(id: &str) -> RtKey {
        RtKey {
            key_id: Some(id.to_string()),
            raw: KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        }
    }

    fn selector(
        rows: Vec<TreeRow>,
    ) -> (
        TreeSelector,
        mpsc::UnboundedReceiver<TreeOutcome>,
        DoneSignal,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let done = super::super::overlay::new_done_signal();
        (TreeSelector::new(rows, "tree", tx, done.clone()), rx, done)
    }

    fn body_text(sel: &TreeSelector) -> String {
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

    fn drain(rx: &mut mpsc::UnboundedReceiver<TreeOutcome>) -> Option<TreeOutcome> {
        rx.try_recv().ok()
    }

    // --- scan: dirs-first alphabetical, trailing slash, indent, noise skip -----

    #[test]
    fn scan_orders_dirs_first_alphabetically_then_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        // Create files and dirs in a deliberately unsorted order.
        std::fs::write(root.join("zeta.txt"), "").unwrap();
        std::fs::write(root.join("alpha.txt"), "").unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::create_dir(root.join("assets")).unwrap();

        let rows = scan_tree(root);
        // Top-level order: assets/ (dir), src/ (dir), then the (recursively empty)
        // dirs contribute no children, then alpha.txt, zeta.txt (files).
        let top: Vec<&str> = rows
            .iter()
            .filter(|r| r.depth == 0)
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(top, vec!["assets/", "src/", "alpha.txt", "zeta.txt"]);
    }

    #[test]
    fn directories_carry_a_trailing_slash_and_files_do_not() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("lib")).unwrap();
        std::fs::write(dir.path().join("main.rs"), "").unwrap();

        let rows = scan_tree(dir.path());
        let lib = rows.iter().find(|r| r.rel_path == "lib").unwrap();
        assert_eq!(lib.label, "lib/", "a directory carries a trailing slash");
        assert!(lib.is_dir);
        let main = rows.iter().find(|r| r.rel_path == "main.rs").unwrap();
        assert_eq!(main.label, "main.rs", "a file has no trailing slash");
        assert!(!main.is_dir);
    }

    #[test]
    fn children_are_indented_one_depth_below_their_parent() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("lib.rs"), "").unwrap();

        let rows = scan_tree(dir.path());
        let src = rows.iter().find(|r| r.rel_path == "src").unwrap();
        let child = rows.iter().find(|r| r.rel_path == "src/lib.rs").unwrap();
        assert_eq!(src.depth, 0, "top-level dir is depth 0");
        assert_eq!(child.depth, 1, "its child is depth 1 (indented)");
    }

    #[test]
    fn noise_directories_never_appear_and_are_not_descended() {
        let dir = tempfile::TempDir::new().unwrap();
        for noise in [".git", "target", "node_modules"] {
            std::fs::create_dir(dir.path().join(noise)).unwrap();
            // Put a decoy file inside so a descent would surface it.
            std::fs::write(dir.path().join(noise).join("decoy"), "").unwrap();
        }
        std::fs::write(dir.path().join("keep.txt"), "").unwrap();

        let rows = scan_tree(dir.path());
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert!(!labels.iter().any(|l| l.contains(".git")), "{labels:?}");
        assert!(!labels.iter().any(|l| l.contains("target")), "{labels:?}");
        assert!(
            !labels.iter().any(|l| l.contains("node_modules")),
            "{labels:?}"
        );
        // The decoy inside a noise dir never surfaces (not descended).
        assert!(!rows.iter().any(|r| r.rel_path.contains("decoy")));
        // The legitimate file is present.
        assert!(rows.iter().any(|r| r.label == "keep.txt"));
    }

    // --- pick (Enter → relative path) + clamp navigation -----------------------

    #[test]
    fn enter_emits_the_highlighted_rows_relative_path() {
        let rows = vec![
            TreeRow {
                rel_path: "src".into(),
                label: "src/".into(),
                depth: 0,
                is_dir: true,
            },
            TreeRow {
                rel_path: "src/lib.rs".into(),
                label: "lib.rs".into(),
                depth: 1,
                is_dir: false,
            },
        ];
        let (mut sel, mut rx, done) = selector(rows);
        sel.handle_key(&key_id("down")); // → src/lib.rs
        sel.handle_key(&key_id("enter"));
        assert!(done.load(Ordering::SeqCst), "enter raises the done flag");
        assert_eq!(
            drain(&mut rx),
            Some(TreeOutcome::Selected("src/lib.rs".into()))
        );
    }

    #[test]
    fn navigation_clamps_at_both_ends_no_wrap() {
        // VAL-OVERLAY-002: /tree is a *clamp* selector — Up at the top and Down at
        // the bottom are no-ops, not wraps.
        let rows = vec![
            TreeRow {
                rel_path: "a".into(),
                label: "a".into(),
                depth: 0,
                is_dir: false,
            },
            TreeRow {
                rel_path: "b".into(),
                label: "b".into(),
                depth: 0,
                is_dir: false,
            },
        ];
        let (mut sel, _rx, _done) = selector(rows);
        assert_eq!(sel.selected_index(), 0);
        sel.handle_key(&key_id("up"));
        assert_eq!(sel.selected_index(), 0, "up at the top clamps (no wrap)");
        sel.handle_key(&key_id("down"));
        sel.handle_key(&key_id("down"));
        assert_eq!(
            sel.selected_index(),
            1,
            "down at the bottom clamps (no wrap)"
        );
    }

    #[test]
    fn escape_emits_cancelled_and_raises_done() {
        let rows = vec![TreeRow {
            rel_path: "a".into(),
            label: "a".into(),
            depth: 0,
            is_dir: false,
        }];
        let (mut sel, mut rx, done) = selector(rows);
        sel.handle_key(&key_id("escape"));
        assert!(done.load(Ordering::SeqCst));
        assert_eq!(drain(&mut rx), Some(TreeOutcome::Cancelled));
    }

    // --- subtree scan (/tree <subdir> only shows that subtree) -----------------

    #[test]
    fn scanning_a_subdir_shows_only_that_subtree() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("lib.rs"), "").unwrap();
        std::fs::create_dir(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs").join("guide.md"), "").unwrap();

        // Scanning src/ directly: only src's contents, rel to that root.
        let rows = scan_tree(&dir.path().join("src"));
        assert_eq!(rows.len(), 1, "only the subtree's contents");
        assert_eq!(rows[0].rel_path, "lib.rs");
        assert!(!rows.iter().any(|r| r.rel_path.contains("docs")));
        assert!(!rows.iter().any(|r| r.rel_path.contains("guide")));
    }

    // --- rendering: title + trailing slash surface -----------------------------

    #[test]
    fn renders_rows_with_directory_slashes() {
        let rows = vec![
            TreeRow {
                rel_path: "src".into(),
                label: "src/".into(),
                depth: 0,
                is_dir: true,
            },
            TreeRow {
                rel_path: "src/lib.rs".into(),
                label: "lib.rs".into(),
                depth: 1,
                is_dir: false,
            },
        ];
        let (sel, _rx, _done) = selector(rows);
        let body = body_text(&sel);
        assert!(body.contains("src/"), "dir slash shown: {body}");
        assert!(body.contains("lib.rs"), "child shown: {body}");
    }

    #[test]
    fn empty_tree_renders_the_placeholder() {
        let (sel, _rx, _done) = selector(vec![]);
        assert!(sel.is_empty());
        assert!(body_text(&sel).contains("(empty directory)"));
    }

    #[test]
    fn enter_on_an_empty_tree_is_inert() {
        let (mut sel, mut rx, done) = selector(vec![]);
        sel.handle_key(&key_id("enter"));
        assert!(!done.load(Ordering::SeqCst), "empty enter does not close");
        assert!(drain(&mut rx).is_none(), "no outcome on empty enter");
    }

    #[test]
    fn is_noise_dir_matches_the_skip_set() {
        assert!(is_noise_dir(".git"));
        assert!(is_noise_dir("target"));
        assert!(is_noise_dir("node_modules"));
        assert!(!is_noise_dir("src"));
        assert!(!is_noise_dir(".github"));
    }

    // --- custom navigation keys drive the registry-backed selector -------------

    #[test]
    fn custom_nav_keys_drive_navigation_and_the_hint() {
        // VAL-OVERLAY-021: a user rebinds select-down to `j`; the picker navigates
        // on `j` (not `down`) and the hint reflects it.
        let rows = vec![
            TreeRow {
                rel_path: "a".into(),
                label: "a".into(),
                depth: 0,
                is_dir: false,
            },
            TreeRow {
                rel_path: "b".into(),
                label: "b".into(),
                depth: 0,
                is_dir: false,
            },
        ];
        let (tx, mut rx) = mpsc::unbounded_channel();
        let done = super::super::overlay::new_done_signal();
        let nav = NavKeys {
            down: "j".to_string(),
            ..NavKeys::default()
        };
        let mut sel = TreeSelector::with_nav(rows, "tree", tx, done.clone(), nav);

        // The default `down` no longer moves; `j` does.
        sel.handle_key(&key_id("down"));
        assert_eq!(sel.selected_index(), 0, "default down is inert under remap");
        sel.handle_key(&key_id("j"));
        assert_eq!(sel.selected_index(), 1, "custom down key navigates");
        sel.handle_key(&key_id("enter"));
        assert!(done.load(Ordering::SeqCst));
        assert_eq!(drain(&mut rx), Some(TreeOutcome::Selected("b".into())));

        // The hint tells the truth about the custom key.
        assert!(
            body_text(&TreeSelector::with_nav(
                vec![],
                "t",
                mpsc::unbounded_channel().0,
                super::super::overlay::new_done_signal(),
                NavKeys {
                    down: "j".to_string(),
                    ..NavKeys::default()
                },
            ))
            .contains("↑/j"),
        );
    }
}
