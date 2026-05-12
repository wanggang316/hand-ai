//! Differential rendering engine.
//!
//! Compares previous and current frames to minimize terminal output.
//!
//! # Cursor invariant
//!
//! After every call to [`DiffRenderer::diff`] (or [`DiffRenderer::full_render`]),
//! the hardware cursor is left at column 0 of the row immediately *below* the
//! last rendered line — i.e. exactly `prev_line_count()` rows below the top
//! of the rendered region. The next [`diff`] call homes the cursor up by
//! `prev_line_count()` rows before painting, so the renderer can address any
//! row in the region with a `\x1b[{n}B` (cursor-down) sequence.
//!
//! Without this invariant, the cursor drifts by one rendered region per
//! frame and every multi-frame interactive session paints in the wrong rows.

/// Differential renderer that tracks previous frame state.
pub struct DiffRenderer {
    prev_lines: Vec<String>,
    first_render: bool,
}

impl DiffRenderer {
    pub fn new() -> Self {
        Self {
            prev_lines: Vec::new(),
            first_render: true,
        }
    }

    /// Reset the renderer (forces full re-render on next diff).
    pub fn reset(&mut self) {
        self.prev_lines.clear();
        self.first_render = true;
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
        // previously rendered line. Home up to row 0 of the region.
        if prev_len > 0 {
            commands.push('\r');
            commands.push_str(&format!("\x1b[{prev_len}A"));
        }

        // Move down to the first row that needs a change.
        if first_changed > 0 {
            commands.push_str(&format!("\x1b[{first_changed}B"));
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
        // column 0 of the row immediately below. The trailing CRLF (vs a
        // bare LF) keeps the next line correctly anchored in raw mode.
        for line in new_lines.iter().take(render_end).skip(first_changed) {
            commands.push_str("\x1b[2K\r");
            commands.push_str(line);
            commands.push_str("\r\n");
        }

        // Cursor is now at column 0 of row `render_end`.
        let mut cursor_row = render_end;

        // If new content is shorter, clear the leftover prev rows.
        if new_len < prev_len {
            // Skip past any unchanged rows we did not repaint.
            let skip = render_end.saturating_sub(cursor_row);
            if skip > 0 {
                commands.push_str(&format!("\x1b[{skip}B"));
                cursor_row += skip;
            }
            // Clear each leftover row.
            for _ in cursor_row..prev_len {
                commands.push_str("\x1b[2K\r\n");
            }
            cursor_row = prev_len;
        }

        // Restore the cursor invariant: we want it exactly `new_len` rows
        // below the top of the region. Cursor is currently at `cursor_row`.
        if cursor_row < new_len {
            commands.push_str(&format!("\x1b[{}B", new_len - cursor_row));
        } else if cursor_row > new_len {
            commands.push_str(&format!("\x1b[{}A", cursor_row - new_len));
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
