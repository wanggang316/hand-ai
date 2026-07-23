//! Box primitive: a full-width background panel with padding around inner content.
//!
//! The rt counterpart to the legacy `BoxComponent`. It fills its entire render
//! area with a background style (so the panel reads as a solid block, including
//! the padding rows/columns), then paints a single inner [`RtComponent`] inset by
//! the padding. Unlike the legacy box — which composed a `Container` of children
//! into `Vec<String>` — this holds one boxed child and lets the child paint into
//! the padded inner rect; nesting a container child covers the multi-child case
//! without this primitive owning layout.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Widget};

use crate::rt::events::RtKey;
use crate::rt::view::{HandleOutcome, RtComponent};

/// A background panel with padding wrapping an optional inner component.
///
/// The background style fills the *whole* area — every row including the padding
/// band — so on a resize the panel background always spans the full width. The
/// inner component, if present, renders into the area inset by `padding_x`
/// columns on each side and `padding_y` rows top and bottom.
pub struct WidgetBox {
    padding_x: u16,
    padding_y: u16,
    background: Style,
    child: Option<Box<dyn RtComponent>>,
}

impl WidgetBox {
    /// An empty box with no padding and the default (unstyled) background.
    pub fn new() -> Self {
        Self {
            padding_x: 0,
            padding_y: 0,
            background: Style::default(),
            child: None,
        }
    }

    /// Set horizontal and vertical padding.
    #[must_use]
    pub fn padding(mut self, x: u16, y: u16) -> Self {
        self.padding_x = x;
        self.padding_y = y;
        self
    }

    /// Set the background style that fills the whole panel (padding included).
    #[must_use]
    pub fn background(mut self, style: Style) -> Self {
        self.background = style;
        self
    }

    /// Place a single inner component, painted inset by the padding.
    #[must_use]
    pub fn child(mut self, child: Box<dyn RtComponent>) -> Self {
        self.child = Some(child);
        self
    }

    /// Whether the box currently holds a child.
    pub fn has_child(&self) -> bool {
        self.child.is_some()
    }
}

impl Default for WidgetBox {
    fn default() -> Self {
        Self::new()
    }
}

impl RtComponent for WidgetBox {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        // Fill the entire area (including padding) with the background style, so
        // the panel background covers the full width at any size. `Block` with a
        // style paints every cell in the rect.
        Block::new().style(self.background).render(area, buf);

        let Some(child) = &self.child else { return };
        let inner = pad_rect(area, self.padding_x, self.padding_y);
        if inner.is_empty() {
            return;
        }
        child.render(inner, buf);
    }

    fn handle_key(&mut self, _key: &RtKey) -> HandleOutcome {
        HandleOutcome::Ignored
    }
}

/// Shrink `area` by `pad_x` columns per side and `pad_y` rows top and bottom,
/// collapsing to a zero-sized rect when padding exceeds the area.
fn pad_rect(area: Rect, pad_x: u16, pad_y: u16) -> Rect {
    let dx = pad_x.saturating_mul(2);
    let dy = pad_y.saturating_mul(2);
    if area.width <= dx || area.height <= dy {
        return Rect::new(area.x, area.y, 0, 0);
    }
    Rect::new(
        area.x + pad_x,
        area.y + pad_y,
        area.width - dx,
        area.height - dy,
    )
}
