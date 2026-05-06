//! Image component — composes [`crate::terminal_image`] output with a
//! placeholder fallback for non-graphics terminals.

use crate::terminal_image::{ImageRenderOptions, render_image};
use crate::tui::Component;

// Re-export the protocol enum so callers can keep using
// `components::image::ImageProtocol`.
pub use crate::terminal_image::ImageProtocol;

/// Component that displays an image in the terminal.
#[derive(Debug)]
pub struct ImageComponent {
    /// Raw image bytes (PNG / JPEG / GIF / WebP).
    data: Vec<u8>,
    /// Display width in columns.
    width: usize,
    /// Display height in rows.
    height: usize,
    /// Currently selected protocol (defaults to whatever the terminal supports).
    protocol: ImageProtocol,
    /// Placeholder text shown when the image can't be rendered inline.
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

    /// Replace the image bytes and intended display size.
    pub fn set_image_data(&mut self, data: Vec<u8>, width: usize, height: usize) {
        self.data = data;
        self.width = width;
        self.height = height;
    }

    /// Set the placeholder text used when the image cannot be rendered.
    pub fn set_alt_text(&mut self, text: impl Into<String>) {
        self.alt_text = text.into();
    }

    /// Override the protocol selection.
    pub fn set_protocol(&mut self, protocol: ImageProtocol) {
        self.protocol = protocol;
    }

    /// Whether image bytes have been loaded.
    pub fn has_data(&self) -> bool {
        !self.data.is_empty()
    }

    fn render_placeholder(&self, width: usize) -> Vec<String> {
        let border_w = self.width.min(width).max(2);
        let inner_w = border_w.saturating_sub(2);
        let mut lines = Vec::new();

        lines.push(format!("┌{}┐", "─".repeat(inner_w)));

        let rows = self.height.saturating_sub(2);
        let mid = rows / 2;
        for r in 0..rows {
            if r == mid {
                let text: String = self.alt_text.chars().take(inner_w).collect();
                let text_w = text.chars().count();
                let pad = inner_w.saturating_sub(text_w);
                let left = pad / 2;
                let right = pad - left;
                lines.push(format!(
                    "│{}{text}{}│",
                    " ".repeat(left),
                    " ".repeat(right)
                ));
            } else {
                lines.push(format!("│{}│", " ".repeat(inner_w)));
            }
        }

        lines.push(format!("└{}┘", "─".repeat(inner_w)));
        lines
    }
}

impl Component for ImageComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let render_width = width as usize;
        if self.data.is_empty() || self.protocol == ImageProtocol::Fallback {
            return self.render_placeholder(render_width);
        }

        let opts = ImageRenderOptions {
            max_cols: Some(self.width.min(render_width) as u16),
            max_rows: Some(self.height as u16),
            preserve_aspect: true,
            label: Some(self.alt_text.clone()),
        };
        render_image(&self.data, &opts)
    }

    fn invalidate(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_image::{
        CellDimensions, TerminalImageCapabilities, set_capabilities,
    };
    use std::sync::Mutex;

    /// Serializes tests that mutate the global capability cache.
    static CAPS_LOCK: Mutex<()> = Mutex::new(());

    fn force_fallback() {
        set_capabilities(TerminalImageCapabilities {
            kitty: false,
            iterm2: false,
            cell_dimensions: CellDimensions::default(),
        });
    }

    #[test]
    fn empty_image_shows_placeholder() {
        let comp = ImageComponent::new(ImageProtocol::Fallback);
        let lines = comp.render(80);
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|l| l.contains("[image]")));
    }

    #[test]
    fn custom_alt_text() {
        let mut comp = ImageComponent::new(ImageProtocol::Fallback);
        comp.set_alt_text("photo.png");
        let lines = comp.render(80);
        assert!(lines.iter().any(|l| l.contains("photo.png")));
    }

    #[test]
    fn has_data_false_when_empty() {
        let comp = ImageComponent::new(ImageProtocol::Fallback);
        assert!(!comp.has_data());
    }

    #[test]
    fn has_data_true_after_set() {
        let mut comp = ImageComponent::new(ImageProtocol::Fallback);
        comp.set_image_data(vec![0u8; 100], 20, 10);
        assert!(comp.has_data());
    }

    #[test]
    fn kitty_protocol_emits_apc_sequence() {
        let _g = CAPS_LOCK.lock().unwrap();
        set_capabilities(TerminalImageCapabilities {
            kitty: true,
            iterm2: false,
            cell_dimensions: CellDimensions::default(),
        });
        let mut comp = ImageComponent::new(ImageProtocol::Kitty);
        comp.set_image_data(vec![0u8; 16], 20, 10);
        let lines = comp.render(80);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("\x1b_G"));
    }

    #[test]
    fn fallback_protocol_renders_placeholder_lines() {
        let _g = CAPS_LOCK.lock().unwrap();
        force_fallback();
        let mut comp = ImageComponent::new(ImageProtocol::Fallback);
        comp.set_image_data(vec![0u8; 16], 20, 10);
        let lines = comp.render(80);
        assert!(lines.len() > 1);
        assert!(lines[0].starts_with('┌'));
    }
}
