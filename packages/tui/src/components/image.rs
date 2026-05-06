//! Image component — terminal image rendering with SIXEL support.

use crate::tui::Component;

/// Image rendering protocol support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    /// SIXEL graphics protocol.
    Sixel,
    /// Kitty graphics protocol.
    Kitty,
    /// No graphics protocol available — show placeholder.
    None,
}

/// Component that displays an image in the terminal.
#[derive(Debug)]
pub struct ImageComponent {
    /// Raw image data (e.g., PNG bytes).
    data: Vec<u8>,
    /// Display width in columns.
    width: usize,
    /// Display height in rows.
    height: usize,
    /// Detected or configured protocol.
    protocol: ImageProtocol,
    /// Alt text shown when image can't be rendered.
    alt_text: String,
}

impl ImageComponent {
    /// Create a new image component.
    pub fn new(protocol: ImageProtocol) -> Self {
        Self {
            data: Vec::new(),
            width: 40,
            height: 10,
            protocol,
            alt_text: "[image]".to_string(),
        }
    }

    /// Set the image data.
    pub fn set_image_data(&mut self, data: Vec<u8>, width: usize, height: usize) {
        self.data = data;
        self.width = width;
        self.height = height;
    }

    /// Set alt text shown when image cannot be rendered.
    pub fn set_alt_text(&mut self, text: impl Into<String>) {
        self.alt_text = text.into();
    }

    /// Set the display protocol.
    pub fn set_protocol(&mut self, protocol: ImageProtocol) {
        self.protocol = protocol;
    }

    /// Check if image data is loaded.
    pub fn has_data(&self) -> bool {
        !self.data.is_empty()
    }

    /// Detect terminal image protocol support.
    pub fn detect_protocol() -> ImageProtocol {
        // Check TERM_PROGRAM for known SIXEL-capable terminals
        if let Ok(term) = std::env::var("TERM_PROGRAM") {
            let term_lower = term.to_lowercase();
            if term_lower.contains("wezterm")
                || term_lower.contains("mintty")
                || term_lower.contains("foot")
            {
                return ImageProtocol::Sixel;
            }
            if term_lower.contains("kitty") {
                return ImageProtocol::Kitty;
            }
        }
        // Check TERM for xterm with SIXEL
        if let Ok(term) = std::env::var("TERM")
            && term.contains("sixel")
        {
            return ImageProtocol::Sixel;
        }
        ImageProtocol::None
    }

    fn render_placeholder(&self, width: usize) -> Vec<String> {
        let border_w = self.width.min(width);
        let mut lines = Vec::new();

        // Top border
        lines.push(format!("┌{}┐", "─".repeat(border_w.saturating_sub(2))));

        // Content area
        let inner_w = border_w.saturating_sub(2);
        let rows = self.height.saturating_sub(2);
        let mid = rows / 2;

        for r in 0..rows {
            if r == mid {
                let text = &self.alt_text;
                let text_w = text.len().min(inner_w);
                let pad = inner_w.saturating_sub(text_w);
                let left = pad / 2;
                let right = pad - left;
                lines.push(format!(
                    "│{}{}{}│",
                    " ".repeat(left),
                    &text[..text_w],
                    " ".repeat(right)
                ));
            } else {
                lines.push(format!("│{}│", " ".repeat(inner_w)));
            }
        }

        // Bottom border
        lines.push(format!("└{}┘", "─".repeat(border_w.saturating_sub(2))));
        lines
    }
}

impl Component for ImageComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let width = width as usize;
        if self.data.is_empty() || self.protocol == ImageProtocol::None {
            return self.render_placeholder(width);
        }

        match self.protocol {
            ImageProtocol::Sixel => {
                // In a real implementation, we'd convert the image to SIXEL.
                // For now, show placeholder with [SIXEL] indicator.
                let mut lines = self.render_placeholder(width);
                if let Some(first) = lines.first_mut() {
                    *first = format!("┌─[SIXEL]{}┐", "─".repeat(width.saturating_sub(12).max(0)));
                }
                lines
            }
            ImageProtocol::Kitty => {
                // Kitty protocol would use ESC sequences.
                let mut lines = self.render_placeholder(width);
                if let Some(first) = lines.first_mut() {
                    *first = format!("┌─[KITTY]{}┐", "─".repeat(width.saturating_sub(12).max(0)));
                }
                lines
            }
            ImageProtocol::None => self.render_placeholder(width),
        }
    }

    fn invalidate(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_image_shows_placeholder() {
        let comp = ImageComponent::new(ImageProtocol::None);
        let lines = comp.render(80);
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|l| l.contains("[image]")));
    }

    #[test]
    fn custom_alt_text() {
        let mut comp = ImageComponent::new(ImageProtocol::None);
        comp.set_alt_text("photo.png");
        let lines = comp.render(80);
        assert!(lines.iter().any(|l| l.contains("photo.png")));
    }

    #[test]
    fn has_data_false_when_empty() {
        let comp = ImageComponent::new(ImageProtocol::None);
        assert!(!comp.has_data());
    }

    #[test]
    fn has_data_true_after_set() {
        let mut comp = ImageComponent::new(ImageProtocol::None);
        comp.set_image_data(vec![0u8; 100], 20, 10);
        assert!(comp.has_data());
    }

    #[test]
    fn sixel_indicator_shown() {
        let mut comp = ImageComponent::new(ImageProtocol::Sixel);
        comp.set_image_data(vec![0u8; 100], 20, 10);
        let lines = comp.render(80);
        assert!(lines[0].contains("SIXEL"));
    }
}
