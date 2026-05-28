//! Image component — composes [`crate::terminal_image`] output with a
//! placeholder fallback for non-graphics terminals.
//
// audit: M3.T5 — parity reviewed against upstream TUI/image.ts on 2026-05-07.
// non-goal: TS keeps a `dimensions` field (raw pixel dims sniffed from the
// image bytes) to size the render. Our `terminal_image::render_image`
// auto-derives dimensions, so we don't expose a separate dims setter.

use crate::terminal_image::{ImageRenderOptions, render_image};
use crate::tui::Component;

// Re-export the protocol enum so callers can keep using
// `components::image::ImageProtocol`.
pub use crate::terminal_image::ImageProtocol;

/// Optional rendering knobs for [`ImageComponent`], mirroring TS's `ImageOptions`.
#[derive(Debug, Clone, Default)]
pub struct ImageOptions {
    /// Maximum width in cells. `None` defers to the component's display width.
    pub max_width_cells: Option<usize>,
    /// Maximum height in cells. `None` defers to the component's display height.
    pub max_height_cells: Option<usize>,
    /// Optional filename surfaced in the fallback placeholder.
    pub filename: Option<String>,
    /// Kitty image ID to reuse across renders (for animations/updates).
    pub image_id: Option<u32>,
}

/// Theme for fallback rendering, mirroring TS's `ImageTheme`. The Rust port
/// uses an ANSI prefix instead of a closure for consistency with the rest of
/// the components.
#[derive(Debug, Clone, Default)]
pub struct ImageTheme {
    /// ANSI prefix applied to the placeholder text (e.g. `"\x1b[90m"`).
    pub fallback_color: Option<String>,
}

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
    /// Optional rendering knobs.
    options: ImageOptions,
    /// Optional theme for fallback rendering.
    theme: ImageTheme,
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
            options: ImageOptions::default(),
            theme: ImageTheme::default(),
        }
    }

    /// Builder: attach rendering options.
    pub fn with_options(mut self, options: ImageOptions) -> Self {
        self.options = options;
        self
    }

    /// Builder: attach a theme.
    pub fn with_theme(mut self, theme: ImageTheme) -> Self {
        self.theme = theme;
        self
    }

    /// Replace rendering options at runtime.
    pub fn set_options(&mut self, options: ImageOptions) {
        self.options = options;
    }

    /// Replace the theme at runtime.
    pub fn set_theme(&mut self, theme: ImageTheme) {
        self.theme = theme;
    }

    /// Current rendering options.
    pub fn options(&self) -> &ImageOptions {
        &self.options
    }

    /// Current theme.
    pub fn theme(&self) -> &ImageTheme {
        &self.theme
    }

    /// Kitty image ID currently associated with this component (if any).
    pub fn image_id(&self) -> Option<u32> {
        self.options.image_id
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
        let label = match (&self.options.filename, self.alt_text.as_str()) {
            (Some(name), _) if !name.is_empty() => name.clone(),
            (_, alt) => alt.to_string(),
        };

        let border_w = self.width.min(width).max(2);
        let inner_w = border_w.saturating_sub(2);
        let mut lines = Vec::new();

        let colorize = |s: String| -> String {
            match &self.theme.fallback_color {
                Some(prefix) if !prefix.is_empty() => format!("{prefix}{s}\x1b[0m"),
                _ => s,
            }
        };

        lines.push(colorize(format!("┌{}┐", "─".repeat(inner_w))));

        let rows = self.height.saturating_sub(2);
        let mid = rows / 2;
        for r in 0..rows {
            if r == mid {
                let text: String = label.chars().take(inner_w).collect();
                let text_w = text.chars().count();
                let pad = inner_w.saturating_sub(text_w);
                let left = pad / 2;
                let right = pad - left;
                lines.push(colorize(format!(
                    "│{}{text}{}│",
                    " ".repeat(left),
                    " ".repeat(right)
                )));
            } else {
                lines.push(colorize(format!("│{}│", " ".repeat(inner_w))));
            }
        }

        lines.push(colorize(format!("└{}┘", "─".repeat(inner_w))));
        lines
    }
}

impl Component for ImageComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let render_width = width as usize;
        if self.data.is_empty() || self.protocol == ImageProtocol::Fallback {
            return self.render_placeholder(render_width);
        }

        let max_cols = self
            .options
            .max_width_cells
            .map(|c| c.min(render_width).min(self.width))
            .unwrap_or_else(|| self.width.min(render_width));
        let max_rows = self
            .options
            .max_height_cells
            .map(|r| r.min(self.height))
            .unwrap_or(self.height);

        let opts = ImageRenderOptions {
            max_cols: Some(max_cols as u16),
            max_rows: Some(max_rows as u16),
            preserve_aspect: true,
            label: self
                .options
                .filename
                .clone()
                .or_else(|| Some(self.alt_text.clone())),
        };
        render_image(&self.data, &opts)
    }

    fn invalidate(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_image::{CellDimensions, TerminalImageCapabilities, set_capabilities};
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

    #[test]
    fn fallback_uses_filename_label_and_theme_color() {
        let comp = ImageComponent::new(ImageProtocol::Fallback)
            .with_options(ImageOptions {
                filename: Some("photo.png".into()),
                ..ImageOptions::default()
            })
            .with_theme(ImageTheme {
                fallback_color: Some("\x1b[35m".into()),
            });
        let lines = comp.render(80);
        assert!(lines.iter().any(|l| l.contains("photo.png")));
        assert!(lines.iter().all(|l| l.contains("\x1b[35m")));
        assert!(lines.iter().all(|l| l.ends_with("\x1b[0m")));
    }

    #[test]
    fn options_image_id_round_trips() {
        let comp = ImageComponent::new(ImageProtocol::Fallback).with_options(ImageOptions {
            image_id: Some(42),
            ..ImageOptions::default()
        });
        assert_eq!(comp.image_id(), Some(42));
    }
}
