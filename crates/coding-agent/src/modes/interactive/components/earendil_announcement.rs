//! "pi has joined Earendil" announcement banner.
//!
//! Composes a [`Container`] of dynamic borders, two `Text` lines, an
//! optional `Image`, and `Spacer` separators. The bundled
//! `clankolas.png` asset can't yet be loaded automatically, so the
//! component takes the image bytes (or `None`) from the caller —
//! hosts that have wired an asset bundle pass the bytes; tests pass
//! `None` and verify the text-only layout.
//!
//! Theming caveat: the component reads `accent`, `muted`, `md_link`
//! slots from the interactive theme when a `Theme` is passed, and
//! falls back to hardcoded ANSI escapes that match the dark-theme
//! visual style otherwise.
//!
//! TODO: ship a bundled-asset loader so the image can be supplied
//! automatically.

use hand_tui::{
    Component, Container, ImageComponent, ImageOptions, ImageProtocol, ImageTheme, InputEvent,
    SpacerComponent, TextComponent,
};

use crate::modes::interactive::components::dynamic_border::DynamicBorderComponent;
use crate::modes::interactive::theme::{Theme, ThemeColor};

/// Blog post URL referenced in the announcement.
pub const BLOG_URL: &str = "https://mariozechner.at/posts/2026-04-08-ive-sold-out/";

/// Filename surfaced in the image fallback placeholder.
pub const IMAGE_FILENAME: &str = "clankolas.png";

/// Maximum width (in cells) the embedded image is allowed to occupy.
pub const IMAGE_MAX_WIDTH_CELLS: usize = 56;

/// Bright cyan — fallback ANSI accent when the theme is missing the slot.
const FALLBACK_ACCENT: &str = "\x1b[96m";
/// Bright black — fallback for muted slot.
const FALLBACK_MUTED: &str = "\x1b[90m";
/// Bright blue — fallback for `mdLink` slot.
const FALLBACK_MD_LINK: &str = "\x1b[94m";
/// SGR reset.
const RESET: &str = "\x1b[0m";

/// Container component rendering the Earendil announcement banner.
pub struct EarendilAnnouncementComponent {
    container: Container,
}

impl EarendilAnnouncementComponent {
    /// Construct the announcement.
    ///
    /// `theme` is consulted for the `accent`, `muted`, and `mdLink` colour
    /// slots; if `None`, hardcoded fallbacks (cyan / bright-black / bright-blue)
    /// are used. `image_bytes` is the raw PNG payload of `clankolas.png`; pass
    /// `None` to suppress the image (mirrors the TS branch when the asset can't
    /// be located on disk).
    pub fn new(theme: Option<&Theme>, image_bytes: Option<Vec<u8>>) -> Self {
        let accent = theme
            .and_then(|t| t.fg_ansi(ThemeColor::Accent).ok())
            .unwrap_or(FALLBACK_ACCENT)
            .to_string();
        let muted = theme
            .and_then(|t| t.fg_ansi(ThemeColor::Muted).ok())
            .unwrap_or(FALLBACK_MUTED)
            .to_string();
        let md_link = theme
            .and_then(|t| t.fg_ansi(ThemeColor::MdLink).ok())
            .unwrap_or(FALLBACK_MD_LINK)
            .to_string();

        let mut container = Container::new();

        // Top border, accent-coloured.
        container.add_child(Box::new(DynamicBorderComponent::with_color(accent.clone())));

        // Title: bold + accent.
        let title = format!("\x1b[1m{}pi has joined Earendil{}{}", accent, RESET, RESET);
        container.add_child(Box::new(TextComponent::new(title).with_padding(1, 0)));

        container.add_child(Box::new(SpacerComponent::new(1)));

        // Read the blog post:
        let prompt = format!("{}{}{}", muted, "Read the blog post:", RESET);
        container.add_child(Box::new(TextComponent::new(prompt).with_padding(1, 0)));

        // Blog URL, mdLink-coloured.
        let link = format!("{}{}{}", md_link, BLOG_URL, RESET);
        container.add_child(Box::new(TextComponent::new(link).with_padding(1, 0)));

        container.add_child(Box::new(SpacerComponent::new(1)));

        // Optional image.
        if let Some(bytes) = image_bytes {
            let mut image = ImageComponent::new(ImageProtocol::Fallback)
                .with_options(ImageOptions {
                    max_width_cells: Some(IMAGE_MAX_WIDTH_CELLS),
                    max_height_cells: None,
                    filename: Some(IMAGE_FILENAME.to_string()),
                    image_id: None,
                })
                .with_theme(ImageTheme {
                    fallback_color: Some(muted.clone()),
                });
            image.set_image_data(bytes, IMAGE_MAX_WIDTH_CELLS, 24);
            container.add_child(Box::new(image));
            container.add_child(Box::new(SpacerComponent::new(1)));
        }

        // Bottom border.
        container.add_child(Box::new(DynamicBorderComponent::with_color(accent)));

        Self { container }
    }
}

impl Component for EarendilAnnouncementComponent {
    fn render(&self, width: u16) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_input(&mut self, event: &InputEvent) -> hand_tui::HandleResult {
        self.container.handle_input(event)
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_borders_title_and_link_without_image() {
        let comp = EarendilAnnouncementComponent::new(None, None);
        let lines = comp.render(80);
        let body = lines.join("\n");

        // First and last lines are border rules.
        assert!(lines[0].contains("─"), "missing top border: {:?}", lines[0]);
        assert!(
            lines.last().unwrap().contains("─"),
            "missing bottom border: {:?}",
            lines.last().unwrap()
        );

        // Title, prompt, and URL all show up.
        assert!(body.contains("pi has joined Earendil"), "missing title");
        assert!(body.contains("Read the blog post:"), "missing prompt");
        assert!(body.contains(BLOG_URL), "missing blog URL");
    }

    #[test]
    fn fallback_palette_emits_default_ansi_codes() {
        let comp = EarendilAnnouncementComponent::new(None, None);
        let body = comp.render(80).join("\n");
        assert!(
            body.contains(FALLBACK_ACCENT),
            "fallback accent not applied"
        );
        assert!(body.contains(FALLBACK_MUTED), "fallback muted not applied");
        assert!(
            body.contains(FALLBACK_MD_LINK),
            "fallback md_link not applied"
        );
    }

    #[test]
    fn image_branch_renders_extra_lines() {
        // Tiny PNG-like blob; the placeholder fallback path doesn't decode the
        // bytes, it just shows a labelled box. We only need the layout to grow.
        let bytes_some = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

        let with_img = EarendilAnnouncementComponent::new(None, Some(bytes_some)).render(80);
        let without_img = EarendilAnnouncementComponent::new(None, None).render(80);

        assert!(
            with_img.len() > without_img.len(),
            "image branch should produce more lines (with={} without={})",
            with_img.len(),
            without_img.len()
        );

        // Filename surfaces in the placeholder.
        let body = with_img.join("\n");
        assert!(
            body.contains(IMAGE_FILENAME),
            "filename not surfaced in placeholder: {body}"
        );
    }
}
