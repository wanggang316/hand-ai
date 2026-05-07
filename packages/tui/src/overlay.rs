//! Overlay system — render content on top of base components.
//!
//! Two compositors live here for backward compat:
//!
//! - The legacy [`Overlay`] / [`render_with_overlay`] pair (simple
//!   centered-or-corner overlay with optional border + dim).
//! - The richer [`OverlayOptions`] / [`compose_overlays`] used by [`crate::Tui`]
//!   for stacked, anchor-positioned overlays.
//!
//! ## Style-leak prevention
//!
//! Overlay content frequently sets background or bold attributes. If the
//! composed line ends without a full `\x1b[0m` reset, terminals will smear
//! that styling onto the cells past the overlay (and, on dismiss, the diff
//! renderer's cached lines will keep it). [`compose_overlays`] therefore:
//!
//! 1. Appends `\x1b[0m` at the end of every line touched by an overlay.
//! 2. Prepends `\x1b[0m` at the start of the line immediately below the
//!    overlay region (when one exists), to clear residual SGR before the
//!    underlying content emits its own escapes.
//!
//! When an overlay is hidden, [`crate::Tui::hide_overlay`] forces a full
//! re-render so the cached lines do not contain any leftover overlay styling.

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
        // Top border. Use display width (not byte length) for the title so
        // CJK / emoji titles don't underflow the saturating_sub and produce
        // a too-short border.
        let title_str = overlay
            .title
            .as_ref()
            .map(|t| format!(" {t} "))
            .unwrap_or_default();
        let title_w = visible_width(&title_str);
        let remaining = inner_w.saturating_sub(title_w);
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

// ---------------------------------------------------------------------------
// Rich overlay options (used by `Tui`)
// ---------------------------------------------------------------------------

/// Anchor positions for an overlay relative to the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAnchor {
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

/// Per-side margin in cells. Negative inputs are clamped to zero (the field
/// type is `u16`, so callers using `as u16` on signed values get the natural
/// saturation).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OverlayMargin {
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
    pub left: u16,
}

impl OverlayMargin {
    /// Uniform margin on all sides.
    pub fn uniform(n: u16) -> Self {
        Self {
            top: n,
            right: n,
            bottom: n,
            left: n,
        }
    }
}

/// Options controlling how an overlay is composited into the frame.
#[derive(Debug, Clone)]
pub struct OverlayOptions {
    /// Where the overlay sits relative to the viewport.
    pub anchor: OverlayAnchor,
    /// Margin between the overlay and the viewport edge.
    pub margin: OverlayMargin,
    /// When true, input is delivered to the overlay component before falling
    /// through to listeners and the focused child.
    pub capture_input: bool,
    /// When true, the entire base frame is dimmed (each non-empty row wrapped
    /// with `\x1b[2m` / `\x1b[22m`) before the overlay is stamped on top. The
    /// overlay's own cells are stamped after dimming, so they remain at full
    /// brightness. This matches typical modal-dialog UX.
    pub dim_background: bool,
    /// When true, draw a single-cell border around the overlay's content.
    pub border: bool,
}

impl Default for OverlayOptions {
    fn default() -> Self {
        Self {
            anchor: OverlayAnchor::Center,
            margin: OverlayMargin::default(),
            capture_input: true,
            dim_background: true,
            border: true,
        }
    }
}

/// Stable handle returned by [`crate::Tui::show_overlay`] for later dismissal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OverlayHandle(pub(crate) u64);

impl OverlayHandle {
    /// The numeric id (debug / test inspection).
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// Compose `base_lines` with `overlays` (back-to-front) into a frame of the
/// given viewport size.
///
/// The result is exactly `height` lines tall (truncating or padding as needed)
/// and each line is at most `width` columns wide. Style-leak prevention is
/// applied as described in the module docs.
pub fn compose_overlays(
    base_lines: &[String],
    overlays: &[(&dyn crate::Component, &OverlayOptions)],
    width: u16,
    height: u16,
) -> Vec<String> {
    let w = width as usize;
    let h = height as usize;

    // Start from `height` rows: pad the base with blank lines and trim if it
    // overshoots so anchor math stays meaningful.
    let mut result: Vec<String> = Vec::with_capacity(h);
    for line in base_lines.iter().take(h) {
        result.push(line.clone());
    }
    while result.len() < h {
        result.push(String::new());
    }

    // Track which rows ended up touched by any overlay, so we can append the
    // line-end reset and prepend a fresh-line reset on the row immediately
    // below the overlay region.
    let mut touched_rows: Vec<bool> = vec![false; h];
    let mut max_touched_row: Option<usize> = None;

    for (component, options) in overlays {
        let lines = component.render(width);
        if lines.is_empty() {
            continue;
        }

        let content_width = lines.iter().map(|l| visible_width(l)).max().unwrap_or(0);
        let raw_w = if options.border {
            content_width + 2
        } else {
            content_width
        };
        let raw_h = if options.border {
            lines.len() + 2
        } else {
            lines.len()
        };

        if raw_w == 0 || raw_h == 0 {
            continue;
        }

        let m = &options.margin;
        // Clamp overlay to the viewport's interior (after margins).
        let avail_w = w.saturating_sub(m.left as usize + m.right as usize);
        let avail_h = h.saturating_sub(m.top as usize + m.bottom as usize);
        let ov_w = raw_w.min(avail_w).max(1);
        let ov_h = raw_h.min(avail_h).max(1);

        let (start_row, start_col) = anchor_position(options.anchor, w, h, ov_w, ov_h, m);

        // Dim the entire base frame; the overlay is stamped on top afterwards
        // so its own cells stay at full brightness.
        if options.dim_background {
            for row in result.iter_mut().take(h) {
                if !row.is_empty() {
                    *row = format!("\x1b[2m{row}\x1b[22m");
                }
            }
        }

        // Build the visual overlay lines, with optional border.
        let overlay_visual = build_visual_lines(&lines, options.border, ov_w, ov_h);

        // Stamp each visual line onto the result.
        for (i, ov_line) in overlay_visual.iter().enumerate() {
            let row = start_row + i;
            if row >= h {
                break;
            }
            result[row] = stamp_styled_line(&result[row], ov_line, start_col, w);
            touched_rows[row] = true;
            max_touched_row = Some(match max_touched_row {
                Some(prev) => prev.max(row),
                None => row,
            });
        }
    }

    // Style-leak prevention pass.
    for (row, touched) in touched_rows.iter().enumerate() {
        if *touched {
            // Always append a hard reset to the line. Idempotent if one is
            // already present.
            result[row].push_str("\x1b[0m");
        }
    }
    // Also scrub the line immediately below the overlay region: prepend a
    // reset so any residual SGR state at the terminal cursor (between frames)
    // does not bleed forward.
    if let Some(top) = max_touched_row
        && top + 1 < h
    {
        let next = &mut result[top + 1];
        let prefixed = format!("\x1b[0m{next}");
        *next = prefixed;
    }

    result
}

fn anchor_position(
    anchor: OverlayAnchor,
    w: usize,
    h: usize,
    ov_w: usize,
    ov_h: usize,
    m: &OverlayMargin,
) -> (usize, usize) {
    let mt = m.top as usize;
    let mr = m.right as usize;
    let mb = m.bottom as usize;
    let ml = m.left as usize;

    let row_top = mt;
    let row_center = mt + (h.saturating_sub(mt + mb).saturating_sub(ov_h)) / 2;
    let row_bottom = h.saturating_sub(mb).saturating_sub(ov_h);

    let col_left = ml;
    let col_center = ml + (w.saturating_sub(ml + mr).saturating_sub(ov_w)) / 2;
    let col_right = w.saturating_sub(mr).saturating_sub(ov_w);

    match anchor {
        OverlayAnchor::TopLeft => (row_top, col_left),
        OverlayAnchor::TopCenter => (row_top, col_center),
        OverlayAnchor::TopRight => (row_top, col_right),
        OverlayAnchor::CenterLeft => (row_center, col_left),
        OverlayAnchor::Center => (row_center, col_center),
        OverlayAnchor::CenterRight => (row_center, col_right),
        OverlayAnchor::BottomLeft => (row_bottom, col_left),
        OverlayAnchor::BottomCenter => (row_bottom, col_center),
        OverlayAnchor::BottomRight => (row_bottom, col_right),
    }
}

/// Build the final visual overlay lines, optionally framing them in a border.
fn build_visual_lines(content: &[String], border: bool, ov_w: usize, _ov_h: usize) -> Vec<String> {
    if !border {
        return content.to_vec();
    }
    let inner_w = ov_w.saturating_sub(2);
    let mut out: Vec<String> = Vec::with_capacity(content.len() + 2);
    out.push(format!("┌{}┐", "─".repeat(inner_w)));
    for line in content {
        let vis = visible_width(line);
        let pad = inner_w.saturating_sub(vis);
        out.push(format!("│{line}{}│", " ".repeat(pad)));
    }
    out.push(format!("└{}┘", "─".repeat(inner_w)));
    out
}

/// Stamp `overlay_text` onto `base` starting at column `col`. Unlike
/// [`stamp_overlay_on_line`], this preserves ANSI codes from `base` outside of
/// the overlay region (the simple stamper used by [`render_with_overlay`]
/// strips dim markers wholesale, which is acceptable for that legacy path but
/// not for richer compositing).
fn stamp_styled_line(base: &str, overlay_text: &str, col: usize, viewport_w: usize) -> String {
    let ov_visible = visible_width(overlay_text);
    let segs = crate::utils::extract_segments(base, col, col + ov_visible, viewport_w, false);

    let mut out = String::new();
    out.push_str(&segs.before);
    // Pad the gap between `before` and the overlay column with spaces.
    if segs.before_width < col {
        out.push_str(&" ".repeat(col - segs.before_width));
    }
    out.push_str(overlay_text);
    // After the overlay, append the segment that was on the right of it.
    out.push_str(&segs.after);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Component, HandleResult, InputEvent};

    // ---------- legacy `Overlay` / `render_with_overlay` ----------

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
        assert!(!result.iter().any(|l| l.contains("┌")));
    }

    #[test]
    fn empty_overlay_content() {
        let base = vec!["Hello".to_string()];
        let overlay = Overlay::centered(vec![]);
        let result = render_with_overlay(&base, &overlay, 40);
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
        assert!(!result.is_empty());
    }

    // ---------- compose_overlays ----------

    struct StaticOverlay {
        lines: Vec<String>,
    }

    impl StaticOverlay {
        fn new(lines: Vec<&str>) -> Self {
            Self {
                lines: lines.into_iter().map(String::from).collect(),
            }
        }
    }

    impl Component for StaticOverlay {
        fn render(&self, _width: u16) -> Vec<String> {
            self.lines.clone()
        }
        fn handle_input(&mut self, _e: &InputEvent) -> HandleResult {
            HandleResult::Ignored
        }
    }

    fn opts_no_border_no_dim(anchor: OverlayAnchor) -> OverlayOptions {
        OverlayOptions {
            anchor,
            margin: OverlayMargin::default(),
            capture_input: false,
            dim_background: false,
            border: false,
        }
    }

    #[test]
    fn test_compose_overlays_centered() {
        let base: Vec<String> = (0..10).map(|_| " ".repeat(40)).collect();
        let comp = StaticOverlay::new(vec!["XYZ"]);
        let opts = opts_no_border_no_dim(OverlayAnchor::Center);
        let overlays: Vec<(&dyn Component, &OverlayOptions)> = vec![(&comp, &opts)];
        let result = compose_overlays(&base, &overlays, 40, 10);
        // Center of a 10-row, 40-col viewport with a 1x3 overlay: row 4, col ~18.
        let row = result
            .iter()
            .position(|l| l.contains("XYZ"))
            .expect("XYZ visible");
        assert!(
            (3..=5).contains(&row),
            "expected centered row 3..=5, got {row}"
        );
    }

    #[test]
    fn test_overlay_renders_at_anchor() {
        let base: Vec<String> = (0..10).map(|_| " ".repeat(40)).collect();
        let comp = StaticOverlay::new(vec!["TR"]);
        let opts = opts_no_border_no_dim(OverlayAnchor::TopRight);
        let overlays: Vec<(&dyn Component, &OverlayOptions)> = vec![(&comp, &opts)];
        let result = compose_overlays(&base, &overlays, 40, 10);
        // Top row should contain "TR" at the right edge.
        let r0 = &result[0];
        assert!(r0.contains("TR"), "row 0 should contain TR: {r0:?}");
        let stripped = crate::utils::strip_ansi(r0);
        assert!(
            stripped.trim_end().ends_with("TR"),
            "TR should be at right edge: {stripped:?}"
        );
    }

    #[test]
    fn test_overlay_margin_applied() {
        let base: Vec<String> = (0..10).map(|_| " ".repeat(40)).collect();
        let comp = StaticOverlay::new(vec!["MARGIN"]);
        let opts = OverlayOptions {
            anchor: OverlayAnchor::TopLeft,
            margin: OverlayMargin {
                top: 2,
                left: 3,
                right: 0,
                bottom: 0,
            },
            capture_input: false,
            dim_background: false,
            border: false,
        };
        let overlays: Vec<(&dyn Component, &OverlayOptions)> = vec![(&comp, &opts)];
        let result = compose_overlays(&base, &overlays, 40, 10);
        let stripped: Vec<String> = result.iter().map(|l| crate::utils::strip_ansi(l)).collect();
        assert!(
            !stripped[0].contains("MARGIN"),
            "row 0 must not have MARGIN: {:?}",
            stripped[0]
        );
        assert!(
            stripped[2].contains("MARGIN"),
            "row 2 must have MARGIN: {:?}",
            stripped[2]
        );
        // Column offset: leading three spaces from the left margin.
        let col = stripped[2].find("MARGIN").unwrap();
        assert_eq!(col, 3, "expected col 3, got {col}");
    }

    /// Regression: border title with a CJK / wide character must not
    /// underflow `inner_w - title_byte_len`. Prior to the fix, a 2-display
    /// width title `中文` (6 bytes) on a width-10 overlay would compute
    /// `inner_w (8) - 6 (bytes) = 2 dashes` and produce a misaligned border.
    #[test]
    fn build_overlay_title_uses_display_width_for_border() {
        let overlay = Overlay {
            content: vec!["body".to_string()],
            position: OverlayPosition::TopLeft,
            border: true,
            dim_background: false,
            title: Some("中文".to_string()),
        };
        let lines = build_overlay_lines(&overlay, 10);
        let top = &lines[0];
        // Top border must be exactly 10 display columns wide regardless of
        // how many bytes the title used.
        assert_eq!(
            crate::utils::visible_width(top),
            10,
            "top border display width: {top:?}"
        );
    }

    #[test]
    fn test_compose_overlays_appends_reset() {
        // Overlay touches some rows; each touched row must end in \x1b[0m.
        let base: Vec<String> = (0..6).map(|_| " ".repeat(20)).collect();
        let comp = StaticOverlay::new(vec!["\x1b[31mfoo\x1b[0m"]);
        let opts = opts_no_border_no_dim(OverlayAnchor::TopLeft);
        let overlays: Vec<(&dyn Component, &OverlayOptions)> = vec![(&comp, &opts)];
        let result = compose_overlays(&base, &overlays, 20, 6);
        assert!(
            result[0].ends_with("\x1b[0m"),
            "row touched by overlay must end with reset: {:?}",
            result[0]
        );
        // Row immediately below should start with a reset prefix.
        assert!(
            result[1].starts_with("\x1b[0m"),
            "row below overlay must start with reset: {:?}",
            result[1]
        );
    }
}
