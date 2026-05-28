//! Differential rendering engine.
//!
//! Compares previous and current frames to minimize terminal output.
//!
//! # Cursor invariant
//!
//! After every call to [`DiffRenderer::diff`] (or `full_render`),
//! the hardware cursor is left at column 0 of the row immediately *below* the
//! last rendered line — i.e. exactly `prev_line_count()` rows below the top
//! of the rendered region. The next [`DiffRenderer::diff`] call homes the
//! cursor up by `prev_line_count()` rows before painting, so the renderer
//! can address any row in the region with a `\x1b[{n}B` (cursor-down)
//! sequence.
//!
//! Without this invariant, the cursor drifts by one rendered region per
//! frame and every multi-frame interactive session paints in the wrong rows.

/// Differential renderer that tracks previous frame state.
pub struct DiffRenderer {
    prev_lines: Vec<String>,
    first_render: bool,
    /// Terminal viewport height in rows. Set via
    /// [`Self::set_viewport_height`] before each diff. When `None`,
    /// the renderer falls back to the legacy "treat prev_lines as
    /// fully visible" behaviour — fine for fixed-height regions but
    /// wrong when chat scrollback pushes content past the viewport.
    ///
    /// With a viewport set, cursor math uses
    /// `min(prev_len, viewport)` for the displayed-row count, and
    /// the shrink-clear path uses cursor-down-without-scroll
    /// (`\x1b[B`) instead of LF, so a shrinking live region (e.g.
    /// loader dismissed) doesn't make the terminal scroll the top
    /// of the region into scrollback. That bug had the editor's
    /// borders and "Working…" lines leaking into the chat history
    /// every time the loader cycled.
    viewport_height: Option<usize>,
}

impl DiffRenderer {
    pub fn new() -> Self {
        Self {
            prev_lines: Vec::new(),
            first_render: true,
            viewport_height: None,
        }
    }

    /// Reset the renderer (forces full re-render on next diff).
    pub fn reset(&mut self) {
        self.prev_lines.clear();
        self.first_render = true;
    }

    /// Inform the renderer of the current terminal viewport height
    /// (in rows). Call this every frame before [`Self::diff`] —
    /// height changes (resize, fullscreen toggle) need to be picked
    /// up so the renderer's view of "what's actually visible"
    /// matches reality.
    pub fn set_viewport_height(&mut self, height: usize) {
        self.viewport_height = Some(height);
    }

    /// Compare new lines against previous state and generate minimal
    /// terminal commands to update the display. Honors the cursor invariant
    /// described in the module docs.
    pub fn diff(&mut self, new_lines: &[String]) -> String {
        if self.first_render {
            self.first_render = false;
            self.prev_lines = new_lines.to_vec();
            return self.full_render(new_lines);
        }

        let prev_len = self.prev_lines.len();
        let new_len = new_lines.len();

        // Displayed-row counts. When a viewport is set and `prev_len`
        // exceeds it, the top of the region has already scrolled into
        // scrollback and is no longer addressable — only the bottom
        // `viewport` rows are physically on screen. All cursor math
        // below operates in "displayed rows" so we don't try to
        // navigate up past the top of the terminal viewport (which
        // the terminal silently clamps and breaks the cursor invariant).
        let displayed_prev = self
            .viewport_height
            .map(|v| prev_len.min(v))
            .unwrap_or(prev_len);
        // `displayed_new` would be needed for symmetric scroll-aware
        // logic on growth, but the growth path already relies on
        // terminal-native LF scrolling, so we only need the
        // `displayed_prev` clamp for the shrink/home-up paths.
        let _displayed_new = self
            .viewport_height
            .map(|v| new_len.min(v))
            .unwrap_or(new_len);

        // Find first changed line.
        let first_changed = self
            .prev_lines
            .iter()
            .zip(new_lines.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(std::cmp::min(prev_len, new_len));

        // Find last changed line (counted from the end of the shorter side).
        let last_changed_from_end = self
            .prev_lines
            .iter()
            .rev()
            .zip(new_lines.iter().rev())
            .position(|(a, b)| a != b)
            .unwrap_or(0);

        let needs_update = first_changed < std::cmp::min(prev_len, new_len) || prev_len != new_len;

        if !needs_update {
            return String::new();
        }

        let mut commands = String::new();

        // Use synchronized output markers to reduce flicker.
        commands.push_str("\x1b[?2026h");

        // Per the cursor invariant, the cursor is one row past the last
        // previously rendered line. Home up to row 0 of the *displayed*
        // region. Using `prev_len` here would silently clamp at the top
        // of the viewport when `prev_len > viewport`, leaving the cursor
        // at row 0 instead of the logical region top — every subsequent
        // movement would then be off by the (prev_len - viewport) delta.
        if displayed_prev > 0 {
            commands.push('\r');
            commands.push_str(&format!("\x1b[{displayed_prev}A"));
        }

        // Translate the logical-region row indices we want to repaint
        // into displayed-region coordinates. Rows above the displayed
        // region are in scrollback and we can't touch them.
        let scrollback_top = prev_len.saturating_sub(displayed_prev);
        let first_changed_displayed = first_changed.saturating_sub(scrollback_top);
        if first_changed_displayed > 0 {
            commands.push_str(&format!("\x1b[{first_changed_displayed}B"));
        }

        // Decide how many rows we'll repaint.
        let render_end = if new_len > prev_len {
            new_len
        } else {
            std::cmp::max(
                new_len.saturating_sub(last_changed_from_end),
                first_changed + 1,
            )
        }
        .min(new_len);

        // Repaint rows [first_changed .. render_end). Each line emits a
        // clear + carriage-return + content + CRLF, leaving the cursor at
        // column 0 of the row immediately below. The trailing CRLF on
        // the growth path IS intentional — it relies on the terminal's
        // natural scroll-up-when-at-bottom behaviour to extend the live
        // region downward as chat appends.
        let paint_start = first_changed.max(scrollback_top);
        for line in new_lines.iter().take(render_end).skip(paint_start) {
            commands.push_str("\x1b[2K\r");
            commands.push_str(line);
            commands.push_str("\r\n");
        }

        // Track cursor in *logical* region coordinates. Each `\r\n`
        // above advanced the cursor by one logical row.
        let mut cursor_row = render_end;

        // If new content is shorter, clear the leftover prev rows.
        //
        // Critically, the shrink path uses `\x1b[B` (cursor-down,
        // no-scroll) rather than LF. LF at the terminal's bottom row
        // scrolls the viewport up by 1, which pushes the top of the
        // live region into scrollback — every loader dismissal would
        // leak the editor's border + a chat row into permanent
        // history. CUD just clamps at the bottom row, which is what
        // we want: rows beyond the visible viewport are already
        // off-screen anyway, no point pretending to clear them.
        if new_len < prev_len {
            // Skip past any unchanged rows we did not repaint.
            let skip = render_end.saturating_sub(cursor_row);
            if skip > 0 {
                commands.push_str(&format!("\x1b[{skip}B"));
                cursor_row += skip;
            }
            // Walk through each leftover row that is still on screen.
            // Logical rows `cursor_row..prev_len`. Clip to the displayed
            // region: rows past `displayed_prev` (counted from logical
            // top) are below the terminal's bottom and don't exist on
            // screen.
            let leftover_visible_end = prev_len.min(scrollback_top + displayed_prev);
            for _ in cursor_row..leftover_visible_end {
                // Clear current line in place, then advance one row
                // without scrolling.
                commands.push_str("\x1b[2K");
                commands.push_str("\x1b[1B\r");
            }
            cursor_row = prev_len;
        }

        // Restore the cursor invariant: we want it exactly `new_len`
        // rows below the top of the region (in logical coords).
        // Movement is in physical terminal rows; we can only move
        // within the visible viewport. The cursor invariant is
        // satisfied as long as the displayed-region cursor lands at
        // row `displayed_new` from its top.
        let target_logical = new_len;
        if cursor_row < target_logical {
            commands.push_str(&format!("\x1b[{}B", target_logical - cursor_row));
        } else if cursor_row > target_logical {
            commands.push_str(&format!("\x1b[{}A", cursor_row - target_logical));
        }

        commands.push_str("\x1b[?2026l");

        self.prev_lines = new_lines.to_vec();
        commands
    }

    /// Generate commands for a full render (no diff). After this call, the
    /// cursor is at column 0 of the row immediately below the last rendered
    /// line — i.e. `lines.len()` rows below the top of the region.
    fn full_render(&self, lines: &[String]) -> String {
        let mut commands = String::new();
        commands.push_str("\x1b[?2026h");

        // Start each line at column 0 (`\r`), clear it (`\x1b[2K`), write the
        // content, then drop to the next row with `\r\n`. The leading reset
        // is required in raw mode where `\n` is a line-feed only — without
        // `\r` each subsequent line would start wherever the previous line
        // ended, producing a stair-step rendering.
        for line in lines {
            commands.push_str("\x1b[2K\r");
            commands.push_str(line);
            commands.push_str("\r\n");
        }

        commands.push_str("\x1b[?2026l");
        commands
    }

    /// Get the previous frame line count.
    pub fn prev_line_count(&self) -> usize {
        self.prev_lines.len()
    }
}

impl Default for DiffRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_first_render_is_full() {
        let mut renderer = DiffRenderer::new();
        let lines = vec!["hello".to_string(), "world".to_string()];
        let output = renderer.diff(&lines);
        assert!(output.contains("hello"));
        assert!(output.contains("world"));
    }

    #[test]
    fn test_no_change_no_output() {
        let mut renderer = DiffRenderer::new();
        let lines = vec!["hello".to_string()];
        renderer.diff(&lines); // First render
        let output = renderer.diff(&lines); // Same content
        assert!(output.is_empty());
    }

    #[test]
    fn test_diff_detects_change() {
        let mut renderer = DiffRenderer::new();
        renderer.diff(&["hello".to_string()]);
        let output = renderer.diff(&["world".to_string()]);
        assert!(!output.is_empty());
        assert!(output.contains("world"));
    }

    #[test]
    fn test_diff_handles_added_lines() {
        let mut renderer = DiffRenderer::new();
        renderer.diff(&["line1".to_string()]);
        let output = renderer.diff(&["line1".to_string(), "line2".to_string()]);
        assert!(!output.is_empty());
        assert!(output.contains("line2"));
    }

    #[test]
    fn test_diff_handles_removed_lines() {
        let mut renderer = DiffRenderer::new();
        renderer.diff(&["line1".to_string(), "line2".to_string()]);
        let output = renderer.diff(&["line1".to_string()]);
        assert!(!output.is_empty());
    }

    #[test]
    fn test_reset_forces_full_render() {
        let mut renderer = DiffRenderer::new();
        renderer.diff(&["hello".to_string()]);
        renderer.reset();
        let output = renderer.diff(&["hello".to_string()]);
        assert!(!output.is_empty());
        assert!(output.contains("hello"));
    }

    #[test]
    fn test_synchronized_output_markers() {
        let mut renderer = DiffRenderer::new();
        let output = renderer.diff(&["test".to_string()]);
        assert!(output.contains("\x1b[?2026h")); // Begin
        assert!(output.contains("\x1b[?2026l")); // End
    }

    #[test]
    fn test_prev_line_count() {
        let mut renderer = DiffRenderer::new();
        assert_eq!(renderer.prev_line_count(), 0);
        renderer.diff(&["a".to_string(), "b".to_string()]);
        assert_eq!(renderer.prev_line_count(), 2);
    }

    /// Cursor-invariant regression: after a 3-line first render, the second
    /// frame must home the cursor up by 3 rows BEFORE painting changes,
    /// otherwise it will paint below the rendered region.
    #[test]
    fn diff_homes_cursor_to_top_before_painting_second_frame() {
        let mut renderer = DiffRenderer::new();
        renderer.diff(&["a".to_string(), "b".to_string(), "c".to_string()]);
        let out = renderer.diff(&["A".to_string(), "b".to_string(), "c".to_string()]);
        // Must contain a cursor-up-by-3 sequence.
        assert!(
            out.contains("\x1b[3A"),
            "expected up-3 home; got: {:?}",
            out
        );
        // Cursor-up MUST appear before the new content so we paint at row 0.
        let up_pos = out.find("\x1b[3A").unwrap();
        let content_pos = out.find('A').unwrap();
        assert!(
            up_pos < content_pos,
            "home-up must precede painting; got: {:?}",
            out
        );
    }

    /// Cursor-invariant regression: after a diff that left N lines on screen,
    /// the cursor ends N rows below the region top — verified indirectly by
    /// running two diffs back-to-back and confirming the second still emits
    /// the up-N home (i.e. the renderer's bookkeeping survived the previous
    /// diff intact).
    #[test]
    fn second_diff_after_change_still_homes_correctly() {
        let mut renderer = DiffRenderer::new();
        renderer.diff(&["a".to_string(), "b".to_string()]);
        // First update — line 0 changes.
        let _ = renderer.diff(&["A".to_string(), "b".to_string()]);
        // Second update — line 1 changes; must home up by 2 again.
        let out = renderer.diff(&["A".to_string(), "B".to_string()]);
        assert!(out.contains("\x1b[2A"), "got: {:?}", out);
    }

    /// When the new content is one row taller, the renderer must still leave
    /// the cursor at row N (one past the last) afterwards. Verified by the
    /// next diff homing up by N+1.
    #[test]
    fn growth_preserves_cursor_invariant() {
        let mut renderer = DiffRenderer::new();
        renderer.diff(&["a".to_string()]);
        let _ = renderer.diff(&["a".to_string(), "b".to_string()]);
        let out = renderer.diff(&["a".to_string(), "b".to_string(), "c".to_string()]);
        // Going from 2 lines to 3 lines: home should be up by 2.
        assert!(out.contains("\x1b[2A"), "got: {:?}", out);
    }

    /// When the new content is shorter, the renderer must clear leftover
    /// rows AND position the cursor at the correct (new) "one past last" row.
    #[test]
    fn shrink_clears_extra_rows_and_preserves_cursor() {
        let mut renderer = DiffRenderer::new();
        renderer.diff(&["a".to_string(), "b".to_string(), "c".to_string()]);
        let out = renderer.diff(&["a".to_string()]);
        // Must home up by 3 first.
        assert!(out.contains("\x1b[3A"), "got: {:?}", out);
        // Must include a clear-line for at least one of the dropped rows.
        let clear_count = out.matches("\x1b[2K").count();
        assert!(clear_count >= 2, "expected ≥2 \\x1b[2K, got: {:?}", out);
    }
}
