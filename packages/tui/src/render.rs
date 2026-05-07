//! Differential rendering engine.
//!
//! Compares previous and current frames to minimize terminal output.

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
    /// terminal commands to update the display.
    pub fn diff(&mut self, new_lines: &[String]) -> String {
        if self.first_render {
            self.first_render = false;
            self.prev_lines = new_lines.to_vec();
            return self.full_render(new_lines);
        }

        let mut commands = String::new();
        let prev_len = self.prev_lines.len();
        let new_len = new_lines.len();

        // Find first changed line
        let first_changed = self
            .prev_lines
            .iter()
            .zip(new_lines.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(std::cmp::min(prev_len, new_len));

        // Find last changed line
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

        // Use synchronized output markers to reduce flicker
        commands.push_str("\x1b[?2026h"); // Begin synchronized update

        // Move to first changed line
        if first_changed > 0 {
            commands.push_str(&format!("\x1b[{}B", first_changed));
        }

        // Render changed lines
        let end = if new_len > prev_len {
            new_len
        } else {
            std::cmp::max(
                new_len.saturating_sub(last_changed_from_end),
                first_changed + 1,
            )
        };

        let render_end = end.min(new_len);
        for (i, line) in new_lines
            .iter()
            .enumerate()
            .take(render_end)
            .skip(first_changed)
        {
            commands.push_str("\x1b[2K"); // Clear line
            commands.push('\r');
            commands.push_str(line);
            if i < render_end - 1 || new_len < prev_len {
                commands.push('\n');
            }
        }

        // Clear extra lines if content shrunk
        if new_len < prev_len {
            for _ in new_len..prev_len {
                commands.push_str("\x1b[2K\n");
            }
            // Move back up
            let extra = prev_len - new_len;
            if extra > 0 {
                commands.push_str(&format!("\x1b[{}A", extra));
            }
        }

        commands.push_str("\x1b[?2026l"); // End synchronized update

        self.prev_lines = new_lines.to_vec();
        commands
    }

    /// Generate commands for a full render (no diff).
    fn full_render(&self, lines: &[String]) -> String {
        let mut commands = String::new();
        commands.push_str("\x1b[?2026h"); // Begin synchronized update

        for (i, line) in lines.iter().enumerate() {
            commands.push_str(line);
            if i < lines.len() - 1 {
                commands.push('\n');
            }
        }

        commands.push_str("\x1b[?2026l"); // End synchronized update
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
        // After reset, should do full render even with same content
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
}
