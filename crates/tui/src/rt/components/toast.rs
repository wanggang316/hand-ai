//! Toast/notification stack: level-tagged transient messages, newest first.
//!
//! The rt-native counterpart to the legacy `ToastComponent`. Where the legacy
//! widget renders to `Vec<String>` of ANSI-coded lines, this implements
//! [`RtComponent`] and paints level-styled rows into a ratatui [`Buffer`].
//!
//! # Pinned behaviour
//!
//! - **Newest first, capped.** Toasts stack with the most recent at the top; at
//!   most [`max_visible`](Toast::set_max_visible) rows paint.
//! - **Overflow is hidden, not dropped.** When more than the cap are live, the
//!   *older* overflow toasts are hidden — but kept. Removing a newer toast
//!   ([`dismiss_newest`](Toast::dismiss_newest)) or letting one expire brings the
//!   next hidden toast back into view, rather than losing it. This is the whole
//!   point of the retain-don't-discard model (VAL-WIDGET-012).
//! - **No ghost rows.** A dismissed or expired toast leaves no residue: the row
//!   count is derived from the live-and-visible set each frame, so a shrunk stack
//!   repaints the vacated rows blank.
//! - **Per-toast TTL seam.** [`push_with_ttl`](Toast::push_with_ttl) attaches a
//!   tick budget; [`tick_ttl`](Toast::tick_ttl) decrements every budget and
//!   removes those that reach zero. This gives a host (the gallery) a
//!   deterministic way to drive expiry so the overflow-re-appears behaviour is
//!   observable without wall-clock timing.
//! - **CJK/emoji safety.** Each message is truncated to the row width on a
//!   *grapheme* boundary via [`truncate_graphemes_with_ellipsis`], measured in
//!   display columns — a two-column glyph is kept or dropped whole, and a narrow
//!   terminal never byte-slices a multibyte grapheme (the legacy `&msg[..n]`
//!   panic this avoids).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use super::truncate_graphemes_with_ellipsis;
use crate::rt::events::RtKey;
use crate::rt::view::{HandleOutcome, RtComponent};

/// Default maximum number of visible toasts.
pub const DEFAULT_MAX_VISIBLE: usize = 3;

/// Toast severity level, driving the icon and color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    /// Informational.
    Info,
    /// Success / completion.
    Success,
    /// A warning.
    Warning,
    /// An error.
    Error,
}

impl ToastLevel {
    /// The bracketed level icon shown before the message (`[i] [*] [!] [x]`).
    pub fn icon(self) -> &'static str {
        match self {
            ToastLevel::Info => "[i]",
            ToastLevel::Success => "[*]",
            ToastLevel::Warning => "[!]",
            ToastLevel::Error => "[x]",
        }
    }

    /// The color the icon is painted with.
    fn color(self) -> Color {
        match self {
            ToastLevel::Info => Color::Cyan,
            ToastLevel::Success => Color::Green,
            ToastLevel::Warning => Color::Yellow,
            ToastLevel::Error => Color::Red,
        }
    }
}

/// One live toast: its level, message, and optional TTL (ticks remaining).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ToastEntry {
    level: ToastLevel,
    message: String,
    /// Ticks remaining before expiry; `None` means it never expires on its own.
    ttl: Option<u32>,
}

/// A stack of level-tagged toast notifications, newest first, with a visible cap
/// that hides — rather than discards — the overflow.
pub struct Toast {
    /// Live toasts in push order (oldest first, newest last). Rendering reverses
    /// this so the newest paints on top.
    entries: Vec<ToastEntry>,
    /// Maximum number of toasts painted at once. Older overflow is hidden but
    /// retained.
    max_visible: usize,
}

impl Toast {
    /// A new, empty toast stack with the default visible cap.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            max_visible: DEFAULT_MAX_VISIBLE,
        }
    }

    /// Push a toast that never expires on its own (dismissed explicitly).
    pub fn push(&mut self, level: ToastLevel, message: impl Into<String>) {
        self.entries.push(ToastEntry {
            level,
            message: message.into(),
            ttl: None,
        });
    }

    /// Push a toast with a tick budget: after `ttl` calls to
    /// [`tick_ttl`](Toast::tick_ttl) it expires and is removed. A `ttl` of `0`
    /// expires on the very next tick.
    pub fn push_with_ttl(&mut self, level: ToastLevel, message: impl Into<String>, ttl: u32) {
        self.entries.push(ToastEntry {
            level,
            message: message.into(),
            ttl: Some(ttl),
        });
    }

    /// Push an info toast.
    pub fn info(&mut self, message: impl Into<String>) {
        self.push(ToastLevel::Info, message);
    }

    /// Push a success toast.
    pub fn success(&mut self, message: impl Into<String>) {
        self.push(ToastLevel::Success, message);
    }

    /// Push a warning toast.
    pub fn warning(&mut self, message: impl Into<String>) {
        self.push(ToastLevel::Warning, message);
    }

    /// Push an error toast.
    pub fn error(&mut self, message: impl Into<String>) {
        self.push(ToastLevel::Error, message);
    }

    /// Remove the newest (most recently pushed) toast, if any. Because the cap
    /// hides the *oldest* overflow, removing the newest reveals the next hidden
    /// toast — the observable half of the retain-don't-discard contract.
    pub fn dismiss_newest(&mut self) {
        self.entries.pop();
    }

    /// Remove the oldest (earliest pushed) toast, if any.
    pub fn dismiss_oldest(&mut self) {
        if !self.entries.is_empty() {
            self.entries.remove(0);
        }
    }

    /// Remove every toast.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Advance every TTL by one tick, removing toasts whose budget reaches zero.
    ///
    /// Non-expiring toasts (`ttl == None`) are untouched. When an expiring toast
    /// is removed and it was newer than a hidden overflow toast, the hidden one
    /// re-appears next frame — expiry drives the same re-appearance
    /// [`dismiss_newest`](Toast::dismiss_newest) does.
    pub fn tick_ttl(&mut self) {
        self.entries.retain_mut(|entry| match entry.ttl {
            Some(0) => false,
            Some(remaining) => {
                entry.ttl = Some(remaining - 1);
                true
            }
            None => true,
        });
    }

    /// The total number of live toasts (including hidden overflow).
    pub fn count(&self) -> usize {
        self.entries.len()
    }

    /// The number of toasts that would paint given the current cap (the live count
    /// clamped to `max_visible`).
    pub fn visible_count(&self) -> usize {
        self.entries.len().min(self.max_visible)
    }

    /// Set the maximum number of visible toasts; the extra older toasts are hidden
    /// but retained.
    pub fn set_max_visible(&mut self, max: usize) {
        self.max_visible = max;
    }

    /// The maximum number of visible toasts.
    pub fn max_visible(&self) -> usize {
        self.max_visible
    }

    /// The levels of the currently *visible* toasts, newest first — a
    /// test/inspection seam that mirrors exactly what [`render`] paints.
    ///
    /// [`render`]: RtComponent::render
    pub fn visible_levels(&self) -> Vec<ToastLevel> {
        self.entries
            .iter()
            .rev()
            .take(self.max_visible)
            .map(|e| e.level)
            .collect()
    }

    /// The messages of the currently *visible* toasts, newest first (untruncated).
    pub fn visible_messages(&self) -> Vec<String> {
        self.entries
            .iter()
            .rev()
            .take(self.max_visible)
            .map(|e| e.message.clone())
            .collect()
    }
}

impl Default for Toast {
    fn default() -> Self {
        Self::new()
    }
}

impl RtComponent for Toast {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let width = area.width as usize;
        let bottom = area.y.saturating_add(area.height);

        // Newest first, capped at `max_visible`; also bounded by the available
        // rows so a short area never paints past its bottom edge (no ghost row).
        let visible = self.entries.iter().rev().take(self.max_visible);

        for (row, entry) in visible.enumerate() {
            let y = area.y.saturating_add(row as u16);
            if y >= bottom {
                break;
            }
            let icon = entry.level.icon();
            // Reserve the icon and its trailing space; the remaining columns hold
            // the (grapheme-truncated) message. `saturating_sub` keeps the budget
            // at zero on a narrow pane rather than underflowing.
            let icon_cols = super::display_width(icon) + 1; // icon + space
            buf.set_stringn(
                area.x,
                y,
                format!("{icon} "),
                width,
                Style::default().fg(entry.level.color()),
            );
            let msg_budget = width.saturating_sub(icon_cols);
            if msg_budget > 0 {
                let shown = truncate_graphemes_with_ellipsis(&entry.message, msg_budget);
                buf.set_stringn(
                    area.x.saturating_add(icon_cols as u16),
                    y,
                    &shown,
                    msg_budget,
                    Style::default(),
                );
            }
        }
    }

    fn handle_key(&mut self, _key: &RtKey) -> HandleOutcome {
        HandleOutcome::Ignored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt::components::display_width;
    use ratatui::layout::Rect;

    fn render(comp: &Toast, width: u16, height: u16) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        comp.render(area, &mut buf);
        buf
    }

    fn row(buf: &Buffer, y: u16) -> String {
        let area = buf.area;
        let mut s = String::new();
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell((x, y)) {
                s.push_str(cell.symbol());
            }
        }
        s.trim_end().to_string()
    }

    fn all_rows(buf: &Buffer) -> Vec<String> {
        let area = buf.area;
        (area.y..area.y + area.height)
            .map(|y| row(buf, y))
            .collect()
    }

    #[test]
    fn empty_stack_paints_nothing() {
        let toast = Toast::new();
        let buf = render(&toast, 40, 3);
        assert!(all_rows(&buf).iter().all(|r| r.is_empty()));
    }

    #[test]
    fn newest_toast_paints_on_top() {
        let mut toast = Toast::new();
        toast.info("first");
        toast.error("second");
        let buf = render(&toast, 40, 3);
        // Newest ("second") is on the top row, with the error icon.
        assert!(row(&buf, 0).contains("[x]"));
        assert!(row(&buf, 0).contains("second"));
        assert!(row(&buf, 1).contains("[i]"));
        assert!(row(&buf, 1).contains("first"));
    }

    #[test]
    fn level_icons_are_bracketed() {
        assert_eq!(ToastLevel::Info.icon(), "[i]");
        assert_eq!(ToastLevel::Success.icon(), "[*]");
        assert_eq!(ToastLevel::Warning.icon(), "[!]");
        assert_eq!(ToastLevel::Error.icon(), "[x]");
    }

    #[test]
    fn cap_limits_visible_but_retains_overflow() {
        let mut toast = Toast::new();
        toast.set_max_visible(2);
        toast.info("one");
        toast.info("two");
        toast.info("three");
        // Three live, but only two visible.
        assert_eq!(toast.count(), 3);
        assert_eq!(toast.visible_count(), 2);
        assert_eq!(toast.visible_messages(), vec!["three", "two"]);
    }

    #[test]
    fn dismiss_newest_reveals_hidden_overflow() {
        let mut toast = Toast::new();
        toast.set_max_visible(2);
        toast.info("one"); // oldest — hidden by the cap
        toast.info("two");
        toast.info("three"); // newest
        // "one" is hidden behind the two-visible cap.
        assert_eq!(toast.visible_messages(), vec!["three", "two"]);
        // Dismiss the newest → the hidden "one" re-appears (retained, not lost).
        toast.dismiss_newest();
        assert_eq!(toast.count(), 2);
        assert_eq!(toast.visible_messages(), vec!["two", "one"]);
    }

    #[test]
    fn ttl_expiry_reveals_hidden_overflow() {
        let mut toast = Toast::new();
        toast.set_max_visible(2);
        toast.info("one"); // oldest, no TTL, hidden behind the cap
        toast.info("two"); // no TTL
        toast.push_with_ttl(ToastLevel::Warning, "three", 1); // newest, expires soon
        assert_eq!(toast.visible_messages(), vec!["three", "two"]);
        // One tick: "three" (ttl 1) survives (decrements to 0); nothing removed yet.
        toast.tick_ttl();
        assert_eq!(toast.count(), 3);
        // Second tick: "three" reaches zero and is removed → hidden "one" re-shows.
        toast.tick_ttl();
        assert_eq!(toast.count(), 2);
        assert_eq!(toast.visible_messages(), vec!["two", "one"]);
    }

    #[test]
    fn dismiss_leaves_no_ghost_row() {
        let mut toast = Toast::new();
        toast.info("alpha");
        toast.error("beta");
        // Render two, then dismiss to one and re-render into the same-size area:
        // the vacated row must be blank.
        let buf = render(&toast, 40, 3);
        assert!(!row(&buf, 1).is_empty());
        toast.dismiss_newest();
        let buf = render(&toast, 40, 3);
        assert!(row(&buf, 0).contains("alpha"));
        assert!(row(&buf, 1).is_empty(), "vacated row leaves no ghost");
        assert!(row(&buf, 2).is_empty());
    }

    #[test]
    fn clear_removes_all() {
        let mut toast = Toast::new();
        toast.info("a");
        toast.warning("b");
        toast.error("c");
        toast.clear();
        assert_eq!(toast.count(), 0);
    }

    #[test]
    fn non_expiring_toasts_untouched_by_tick() {
        let mut toast = Toast::new();
        toast.info("persistent");
        for _ in 0..100 {
            toast.tick_ttl();
        }
        assert_eq!(toast.count(), 1);
    }

    // --- CJK / narrow-terminal safety --------------------------------------

    /// The display width the icon-and-message *helper* produced at `width`,
    /// measured from the source strings rather than the painted grid (a wide
    /// glyph's reserved continuation cell reads back as a blank, which would
    /// double-count). Only meaningful when the icon itself fits the width; below
    /// that the render path clips the icon via `set_stringn` and this over-counts.
    fn painted_width(icon: &str, message: &str, width: u16) -> usize {
        let icon_cols = display_width(icon) + 1; // icon + space
        let msg_budget = (width as usize).saturating_sub(icon_cols);
        let msg = truncate_graphemes_with_ellipsis(message, msg_budget);
        icon_cols + display_width(&msg)
    }

    #[test]
    fn narrow_cjk_toast_truncates_without_panic() {
        let mut toast = Toast::new();
        let msg = "你好世界你好世界这是一个很长的消息";
        toast.info(msg);
        let icon_cols = display_width(ToastLevel::Info.icon()) + 1;
        for w in 1u16..=8 {
            // Rendering must not panic and must stay on the single toast row at any
            // narrow width (the render path clips the icon via `set_stringn` when
            // even the icon does not fit, so the buffer never overflows).
            let buf = render(&toast, w, 2);
            assert!(
                row(&buf, 1).is_empty(),
                "width {w}: spilled to a second row"
            );
            // Once the icon fits, icon + truncated message fit the pane in display
            // columns — no overflow, no byte-sliced grapheme.
            if (w as usize) >= icon_cols {
                assert!(
                    painted_width(ToastLevel::Info.icon(), msg, w) <= w as usize,
                    "width {w}: content wider than pane"
                );
            }
        }
    }

    #[test]
    fn narrow_emoji_toast_truncates_without_panic() {
        let mut toast = Toast::new();
        let msg = "🎉🎉🎉🎉🎉 celebration overflow";
        toast.error(msg);
        let icon_cols = display_width(ToastLevel::Error.icon()) + 1;
        for w in 1u16..=8 {
            let buf = render(&toast, w, 2);
            assert!(
                row(&buf, 1).is_empty(),
                "width {w}: spilled to a second row"
            );
            if (w as usize) >= icon_cols {
                assert!(
                    painted_width(ToastLevel::Error.icon(), msg, w) <= w as usize,
                    "width {w}: content wider than pane"
                );
            }
        }
    }
}
