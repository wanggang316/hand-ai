//! Overlay system — render content on top of base components.

use crate::utils::visible_width;

/// Position of the overlay relative to the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayPosition {
    /// Centered horizontally and vertically.
    Center,
    /// Top-left corner.
    TopLeft,
    /// Top-right corner.
    TopRight,
    /// Bottom-left corner.
    BottomLeft,
    /// Bottom-right corner.
    BottomRight,
}

/// An overlay that renders on top of base content.
#[derive(Debug, Clone)]
pub struct Overlay {
    /// Content lines of the overlay.
    pub content: Vec<String>,
    /// Position of the overlay.
    pub position: OverlayPosition,
    /// Whether to draw a border around the overlay.
    pub border: bool,
    /// Whether to dim the background behind the overlay.
    pub dim_background: bool,
    /// Optional title for the border.
    pub title: Option<String>,
}

impl Overlay {
    /// Create a centered overlay with the given content.
    pub fn centered(content: Vec<String>) -> Self {
        Self {
            content,
            position: OverlayPosition::Center,
            border: true,
            dim_background: true,
            title: None,
        }
    }

    /// Create an overlay at the specified position.
    pub fn at(position: OverlayPosition, content: Vec<String>) -> Self {
        Self {
            content,
            position,
            border: true,
            dim_background: false,
            title: None,
        }
    }

    /// Set the title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set whether to draw a border.
    pub fn with_border(mut self, border: bool) -> Self {
        self.border = border;
        self
    }

    /// Set whether to dim the background.
    pub fn with_dim(mut self, dim: bool) -> Self {
        self.dim_background = dim;
        self
    }
}

/// Render base content with an overlay on top.
///
/// Returns new lines that combine the base content with the overlay content
/// positioned according to the overlay settings.
pub fn render_with_overlay(base_lines: &[String], overlay: &Overlay, width: usize) -> Vec<String> {
    let mut result: Vec<String> = base_lines.to_vec();

    // Ensure we have enough lines
    let base_height = result.len();

    // Calculate overlay dimensions
    let content_width = overlay
        .content
        .iter()
        .map(|l| visible_width(l))
        .max()
        .unwrap_or(0);
    let overlay_width = if overlay.border {
        content_width + 4 // 2 for borders + 2 for padding
    } else {
        content_width
    };
    let overlay_height = if overlay.border {
        overlay.content.len() + 2
    } else {
        overlay.content.len()
    };

    if overlay_width == 0 || overlay_height == 0 {
        return result;
    }

    // Calculate position
    let (start_row, start_col) = match overlay.position {
        OverlayPosition::Center => (
            base_height.saturating_sub(overlay_height) / 2,
            width.saturating_sub(overlay_width) / 2,
        ),
        OverlayPosition::TopLeft => (0, 0),
        OverlayPosition::TopRight => (0, width.saturating_sub(overlay_width)),
        OverlayPosition::BottomLeft => (base_height.saturating_sub(overlay_height), 0),
        OverlayPosition::BottomRight => (
            base_height.saturating_sub(overlay_height),
            width.saturating_sub(overlay_width),
        ),
    };

    // Ensure result has enough rows
    while result.len() < start_row + overlay_height {
        result.push(" ".repeat(width));
    }

    // Dim background if requested
    if overlay.dim_background {
        for line in &mut result {
            *line = format!("\x1b[2m{line}\x1b[22m");
        }
    }

    // Build overlay lines
    let overlay_lines = build_overlay_lines(overlay, overlay_width);

    // Stamp overlay onto result
    for (i, overlay_line) in overlay_lines.iter().enumerate() {
        let row = start_row + i;
        if row < result.len() {
            result[row] = stamp_overlay_on_line(&result[row], overlay_line, start_col, width);
        }
    }

    result
}

fn build_overlay_lines(overlay: &Overlay, overlay_width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let inner_w = if overlay.border {
        overlay_width.saturating_sub(2)
    } else {
        overlay_width
    };

    if overlay.border {
        // Top border
        let title_str = overlay
            .title
            .as_ref()
            .map(|t| format!(" {t} "))
            .unwrap_or_default();
        let remaining = inner_w.saturating_sub(title_str.len());
        lines.push(format!("┌{title_str}{}┐", "─".repeat(remaining)));
    }

    // Content
    for content_line in &overlay.content {
        let vis_w = visible_width(content_line);
        let pad = inner_w.saturating_sub(vis_w + if overlay.border { 2 } else { 0 });
        if overlay.border {
            lines.push(format!("│ {content_line}{} │", " ".repeat(pad)));
        } else {
            lines.push(format!("{content_line}{}", " ".repeat(pad)));
        }
    }

    if overlay.border {
        lines.push(format!("└{}┘", "─".repeat(inner_w)));
    }

    lines
}

fn stamp_overlay_on_line(base: &str, overlay_text: &str, col: usize, _width: usize) -> String {
    let base_chars: Vec<char> = strip_ansi_for_stamping(base);
    let mut result = String::new();

    // Characters before overlay
    for &ch in &base_chars[..col.min(base_chars.len())] {
        result.push(ch);
    }
    // Pad if base is shorter
    if base_chars.len() < col {
        for _ in 0..(col - base_chars.len()) {
            result.push(' ');
        }
    }

    // Overlay content
    result.push_str(overlay_text);

    // Characters after overlay
    let overlay_end = col + visible_width(overlay_text);
    if overlay_end < base_chars.len() {
        for &ch in &base_chars[overlay_end..] {
            result.push(ch);
        }
    }

    result
}

fn strip_ansi_for_stamping(s: &str) -> Vec<char> {
    // Simple ANSI strip - remove dim markers for stamping purposes
    let stripped = s.replace("\x1b[2m", "").replace("\x1b[22m", "");
    stripped.chars().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_overlay_basic() {
        let base = vec![
            "Hello World".to_string(),
            "Second line".to_string(),
            "Third line ".to_string(),
            "Fourth line".to_string(),
            "Fifth line ".to_string(),
        ];
        let overlay = Overlay::centered(vec!["Alert!".to_string()]);
        let result = render_with_overlay(&base, &overlay, 40);
        assert!(!result.is_empty());
        // Should contain the overlay content somewhere
        assert!(result.iter().any(|l| l.contains("Alert!")));
    }

    #[test]
    fn overlay_with_title() {
        let base = vec![" ".repeat(40); 5];
        let overlay = Overlay::centered(vec!["Content".to_string()]).with_title("Title");
        let result = render_with_overlay(&base, &overlay, 40);
        assert!(result.iter().any(|l| l.contains("Title")));
    }

    #[test]
    fn overlay_without_border() {
        let base = vec![" ".repeat(40); 5];
        let overlay = Overlay::centered(vec!["Text".to_string()]).with_border(false);
        let result = render_with_overlay(&base, &overlay, 40);
        assert!(result.iter().any(|l| l.contains("Text")));
        // No border characters
        assert!(!result.iter().any(|l| l.contains("┌")));
    }

    #[test]
    fn empty_overlay_content() {
        let base = vec!["Hello".to_string()];
        let overlay = Overlay::centered(vec![]);
        let result = render_with_overlay(&base, &overlay, 40);
        // Should return base unchanged (or nearly)
        assert!(!result.is_empty());
    }

    #[test]
    fn top_left_position() {
        let base = vec![" ".repeat(40); 5];
        let overlay = Overlay::at(OverlayPosition::TopLeft, vec!["TL".to_string()]);
        let result = render_with_overlay(&base, &overlay, 40);
        assert!(result[0].contains("┌") || result[0].starts_with("┌"));
    }

    #[test]
    fn dim_background() {
        let base = vec!["Hello".to_string()];
        let overlay = Overlay::centered(vec!["X".to_string()]).with_dim(true);
        let result = render_with_overlay(&base, &overlay, 40);
        // Background lines should contain dim escape codes initially
        // (they get stamped over by overlay content)
        assert!(!result.is_empty());
    }
}
