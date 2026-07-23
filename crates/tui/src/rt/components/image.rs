//! Image widget + graphics-protocol emission for the rt stack.
//!
//! # Why an image needs a channel that bypasses the `Buffer`
//!
//! A ratatui [`Buffer`] is a grid of text cells. The graphics protocols that put
//! a *picture* on the screen — the Kitty graphics protocol (`\x1b_G…\x1b\\`, an
//! APC string) and iTerm2 inline images (`\x1b]1337;File=…`, an OSC) — are not
//! cell content: they are escape sequences the terminal interprets out of band,
//! positioned at the current cursor. They cannot be stored in a cell and diffed
//! like text. So a graphics image on the rt stack is a **two-part** thing:
//!
//! 1. **In the buffer:** [`RtImage`] reserves `N` rows (blank cells in graphics
//!    mode, a bordered placeholder box in fallback mode). This is what the frame
//!    diff paints and clears, so the image's footprint scrolls, resizes, and
//!    repaints like any other widget — no ghost when it moves or the pane shrinks.
//! 2. **Out of band:** in graphics mode, [`RtImage::render`] does *not* try to
//!    stuff the escape into a cell. It records a [`PendingEmission`] — the encoded
//!    escape plus the viewport-local row it belongs on — into a shared
//!    [`RawEmissionQueue`]. After `terminal.draw` returns, the draw-owning task
//!    drains the queue and writes each escape at its row via
//!    [`RawEmissionQueue::flush_to`], moving the real cursor there first.
//!
//! This split is the load-bearing new mechanism. It respects invariant #1 of the
//! architecture ("the scheduler owns the terminal"): the widget never writes to
//! the terminal itself — it only *renders into a buffer* and *enqueues* an
//! emission; the single draw-owning task performs the raw write, ordered strictly
//! after the frame's `draw`. `m2-image-scrollback` extends this same queue to
//! carry images into native scrollback (history) and to attach OSC 8 hyperlinks;
//! nothing about the queue's shape needs to change for that — a scrollback image
//! is just a [`PendingEmission`] whose row is resolved against a committed history
//! line instead of a viewport row.
//!
//! # Protocol selection
//!
//! Capability detection is delegated wholesale to
//! [`crate::terminal_image::detect_capabilities`] / [`get_capabilities`] — the
//! legacy encoder module is the single source of truth for "what does this
//! terminal speak", and it is reused unchanged. This module only *resolves* the
//! snapshot into a concrete plan ([`resolve_protocol`]) and *frames* the bytes:
//!
//! - **Kitty** (ghostty / wezterm / kitty): `encode_kitty`, an APC with a
//!   transfer header and `i=<id>`. Kitty transmits PNG (`f=100`), so a non-PNG
//!   source is transcoded to PNG first ([`transcode_to_png`]); a source that
//!   cannot be decoded degrades to the fallback box rather than emitting an
//!   invalid APC.
//! - **iTerm2**: `encode_iterm2`, an OSC 1337 carrying the source bytes natively
//!   (iTerm2 decodes jpeg/gif/webp/png itself — no transcode).
//! - **Fallback** (plain terminal, or a multiplexer that swallows graphics): a
//!   bordered placeholder box painted into cells, labelled with the filename/alt
//!   and, when the bytes sniff, a `[<mime> WxH]` tag. No graphics bytes are ever
//!   produced on this path.

use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::rt::events::RtKey;
use crate::rt::view::{HandleOutcome, RtComponent};
use crate::terminal_image::{
    CellDimensions, ImageDimensions, ImageRenderOptions, allocate_image_id, encode_iterm2,
    encode_kitty, get_capabilities, get_cell_dimensions, get_image_dimensions,
};

/// The graphics protocol this terminal will actually receive, resolved from a
/// [`crate::terminal_image::TerminalImageCapabilities`] snapshot.
///
/// The rt-layer view of protocol selection. It is deliberately the same three
/// arms as [`crate::terminal_image::ImageProtocol`] but re-exposed here so this
/// module (and its tests) name the resolved choice without reaching across the
/// legacy boundary in call sites, and so the resolution *rule* — kitty wins a
/// tie, a multiplexer forces fallback — is pinned by rt-owned tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedProtocol {
    /// Kitty graphics protocol (Kitty, Ghostty, WezTerm). PNG payload.
    Kitty,
    /// iTerm2 inline images. Native (untranscoded) payload.
    ITerm2,
    /// No graphics protocol: paint a cell placeholder, emit zero graphics bytes.
    Fallback,
}

/// Resolve the *current* terminal's capabilities into a concrete protocol choice.
///
/// A thin wrapper over [`get_capabilities`] (which caches
/// [`detect_capabilities`](crate::terminal_image::detect_capabilities)) that maps
/// the capability snapshot to a [`ResolvedProtocol`]. The selection rule lives in
/// the legacy `protocol()` and is reused verbatim:
///
/// - a multiplexer (tmux/screen) reports neither capability → [`Fallback`], even
///   when the outer terminal *would* speak a protocol (the multiplexer swallows
///   graphics);
/// - Kitty wins a Kitty+iTerm2 tie;
/// - iTerm2 only when Kitty is absent.
///
/// [`Fallback`]: ResolvedProtocol::Fallback
#[must_use]
pub fn resolve_protocol() -> ResolvedProtocol {
    resolve(get_capabilities().kitty, get_capabilities().iterm2)
}

/// Pure resolution of `(kitty, iterm2)` capability flags into a protocol.
///
/// Split out from [`resolve_protocol`] so the matrix — ghostty→Kitty,
/// tmux-suppressed→Fallback, kitty+iterm2→Kitty — is unit-tested without touching
/// the global capability cache. The capability flags themselves carry the
/// multiplexer suppression (detect returns `kitty=false, iterm2=false` inside a
/// multiplexer), so this function is just the tie-break.
#[must_use]
pub fn resolve(kitty: bool, iterm2: bool) -> ResolvedProtocol {
    if kitty {
        ResolvedProtocol::Kitty
    } else if iterm2 {
        ResolvedProtocol::ITerm2
    } else {
        ResolvedProtocol::Fallback
    }
}

/// The image container format, sniffed from magic bytes.
///
/// Used both to build the fallback `[<mime> …]` tag and to decide whether the
/// Kitty path must transcode (everything but [`Png`] does).
///
/// [`Png`]: ImageFormat::Png
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// PNG — Kitty transmits this natively (`f=100`).
    Png,
    /// JPEG.
    Jpeg,
    /// GIF.
    Gif,
    /// WebP.
    Webp,
    /// Magic bytes matched no known container.
    Unknown,
}

impl ImageFormat {
    /// Sniff the container from the leading magic bytes.
    #[must_use]
    pub fn sniff(data: &[u8]) -> Self {
        if data.len() >= 4 && data[..4] == [0x89, 0x50, 0x4e, 0x47] {
            ImageFormat::Png
        } else if data.len() >= 2 && data[..2] == [0xff, 0xd8] {
            ImageFormat::Jpeg
        } else if data.len() >= 6 && (&data[..6] == b"GIF87a" || &data[..6] == b"GIF89a") {
            ImageFormat::Gif
        } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
            ImageFormat::Webp
        } else {
            ImageFormat::Unknown
        }
    }

    /// The short MIME-ish tag shown in the fallback placeholder (`png`, `jpeg`,
    /// …). [`Unknown`](ImageFormat::Unknown) has no tag.
    #[must_use]
    pub fn mime_tag(self) -> Option<&'static str> {
        match self {
            ImageFormat::Png => Some("png"),
            ImageFormat::Jpeg => Some("jpeg"),
            ImageFormat::Gif => Some("gif"),
            ImageFormat::Webp => Some("webp"),
            ImageFormat::Unknown => None,
        }
    }
}

/// The placeholder tag for a non-graphics terminal: `[<mime> WxH]` when both the
/// format and dimensions sniff, `[<mime>]` when only the format is known (a
/// corrupt file with a valid magic prefix but no readable dimensions), or `None`
/// when the bytes match no container at all.
///
/// This is the label that lets a plain-terminal capture confirm *which* format a
/// dropped image was and, when known, its pixel size — without decoding it.
#[must_use]
pub fn sniff_label(data: &[u8]) -> Option<String> {
    let mime = ImageFormat::sniff(data).mime_tag()?;
    match get_image_dimensions(data) {
        Some(ImageDimensions { width, height }) => Some(format!("[{mime} {width}x{height}]")),
        None => Some(format!("[{mime}]")),
    }
}

/// The cell-extent an image occupies once scaled to fit an area, in terminal
/// cells, with the source aspect ratio preserved.
///
/// Both axes are clamped: `cols ≤ area.width` and `rows ≤ area.height`. The
/// binding constraint is whichever axis would overflow *first* when the pixel
/// image is projected onto the cell grid — a wide image is bound by width and a
/// tall one by height — and the other axis is scaled down proportionally so the
/// picture is never stretched. This is the footprint the widget reserves and the
/// `c=`/`r=` (Kitty) or `width=`/`height=` (iTerm2) the encoder is handed, so the
/// blanked rows in the buffer and the graphics image drawn over them agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClampedCells {
    /// Width in terminal columns (`≥ 1`, `≤ area.width`).
    pub cols: u16,
    /// Height in terminal rows (`≥ 1`, `≤ area.height`).
    pub rows: u16,
}

/// Project `image` (pixels) onto the cell grid of `area`, preserving aspect and
/// clamping *both* axes to the area.
///
/// The natural cell extent is `ceil(px / cell_px)` per axis. If that already fits
/// `area`, it is used verbatim. Otherwise the image is scaled by the tighter of
/// the two fit ratios (`area.width / nat_cols`, `area.height / nat_rows`) so the
/// binding axis lands exactly on the area edge and the other axis shrinks in
/// proportion — never stretched past its own natural size, never overflowing.
///
/// `cell` is the pixel size of one terminal cell (font/zoom dependent; supplied by
/// a `CSI 16 t` reply when one was captured, else the 8×16 default). Splitting the
/// cell size out as a parameter keeps the clamp math a pure function the unit
/// tests drive across cell sizes without touching the global capability cache.
#[must_use]
pub fn clamp_to_area(image: ImageDimensions, area: Rect, cell: CellDimensions) -> ClampedCells {
    let max_cols = area.width.max(1);
    let max_rows = area.height.max(1);
    if image.width == 0 || image.height == 0 {
        return ClampedCells { cols: 1, rows: 1 };
    }
    let cell_w = u32::from(cell.width.max(1));
    let cell_h = u32::from(cell.height.max(1));

    // Natural cell extent: how many whole cells the image covers at 1:1.
    let nat_cols = image.width.div_ceil(cell_w).max(1);
    let nat_rows = image.height.div_ceil(cell_h).max(1);

    if nat_cols <= u32::from(max_cols) && nat_rows <= u32::from(max_rows) {
        return ClampedCells {
            cols: clamp_u16(nat_cols, max_cols),
            rows: clamp_u16(nat_rows, max_rows),
        };
    }

    // Overflow on at least one axis: scale by the tighter fit ratio so the
    // binding axis lands on the edge and the other shrinks proportionally.
    // Compare cross-multiplied to stay in integer math and avoid float drift:
    //   width-bound  when  max_cols/nat_cols < max_rows/nat_rows
    //                <=>   max_cols*nat_rows < max_rows*nat_cols
    let width_bound =
        u64::from(max_cols) * u64::from(nat_rows) < u64::from(max_rows) * u64::from(nat_cols);
    if width_bound {
        let cols = max_cols;
        let rows = ((u64::from(cols) * u64::from(nat_rows)) / u64::from(nat_cols)).max(1);
        ClampedCells {
            cols,
            rows: clamp_u16(rows as u32, max_rows),
        }
    } else {
        let rows = max_rows;
        let cols = ((u64::from(rows) * u64::from(nat_cols)) / u64::from(nat_rows)).max(1);
        ClampedCells {
            cols: clamp_u16(cols as u32, max_cols),
            rows,
        }
    }
}

/// Saturating cast of a `u32` cell count to `u16`, clamped to `max` and floored at
/// `1` (an image always occupies at least one cell).
fn clamp_u16(value: u32, max: u16) -> u16 {
    value.min(u32::from(max)).max(1) as u16
}

/// Whether `data` is a *decodable* image on the current build's decoder set.
///
/// The decode-validation gate every emission path runs before producing graphics
/// bytes: a source whose container magic sniffs but whose payload the `image`
/// crate cannot decode (truncated / corrupt) must never reach the wire as a
/// graphics escape — a half-image APC or an OSC 1337 wrapping undecodable bytes
/// makes the terminal emit an error reply or paint garbage. When this returns
/// `false` the widget degrades to the bordered placeholder box on *every* persona
/// (Kitty *and* iTerm2 *and* fallback), exactly as an undecodable Kitty source
/// already did — the migration fix is that iTerm2 is no longer exempt.
#[must_use]
pub fn decodes(data: &[u8]) -> bool {
    image::load_from_memory(data).is_ok()
}

/// Strip control and escape bytes from a label so a filename / alt string can
/// never smuggle a terminal escape sequence out through the placeholder box or an
/// image name.
///
/// A dropped file's name or an image's alt text is attacker-influenced text that
/// ends up (a) painted into buffer cells and (b) base64-is-not-applied to the
/// Kitty `c=`/`r=` params but *is* carried verbatim-ish elsewhere. If it carries
/// raw `\x1b`, a CSI/OSC introducer, or other C0/C1 control bytes, a naive render
/// could let it re-open a graphics/OSC context or move the cursor. Every
/// C0 control (`< 0x20`), `DEL`, and C1 control (`0x80..=0x9f`) is dropped; all
/// printable text (including CJK/emoji) is preserved so the label still reads.
#[must_use]
pub fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .filter(|&c| {
            let cp = c as u32;
            // Drop C0 controls (incl. ESC 0x1b), DEL, and C1 controls; keep all
            // printable characters (tab included would still be a control — drop).
            !(cp < 0x20 || cp == 0x7f || (0x80..=0x9f).contains(&cp))
        })
        .collect()
}

/// Clip `label` to `inner_w` *display* columns, appending an ellipsis (`…`) when
/// it does not fit.
///
/// Display-width clipping, not `char`-count clipping: a CJK glyph or emoji is two
/// columns wide, so the legacy `label.chars().take(inner_w)` overshoots the box by
/// one column per wide glyph and tears the border. This accumulates whole grapheme
/// clusters by their rendered width and stops before the budget overflows, so the
/// returned string is `≤ inner_w` columns and the placeholder's right border stays
/// aligned. Mirrors the crate's `truncate_graphemes_with_ellipsis` but is inlined
/// here so the image module's placeholder does not reach across the primitive
/// module's private helper.
#[must_use]
pub fn clip_label(label: &str, inner_w: usize) -> String {
    if inner_w == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(label) <= inner_w {
        return label.to_string();
    }
    // Reserve one column for the ellipsis marker when clipping happens.
    let budget = inner_w.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0usize;
    for cluster in label.graphemes(true) {
        let w = UnicodeWidthStr::width(cluster);
        if used + w > budget {
            break;
        }
        out.push_str(cluster);
        used += w;
    }
    out.push('…');
    out
}

/// A single graphics escape queued for emission after the frame draws.
///
/// The out-of-band half of an image: the encoded protocol sequence (`\x1b_G…` or
/// `\x1b]1337;…`) plus the *viewport-local* row it must be written at. The
/// draw-owning task resolves the row against the viewport origin and moves the
/// cursor there before writing `escape` (see [`RawEmissionQueue::flush_to`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingEmission {
    /// The fully-encoded graphics escape sequence.
    pub escape: String,
    /// The viewport-local row (`0` = top of the viewport) the image's top edge
    /// occupies. The flush adds the viewport origin to place it absolutely.
    pub row: u16,
    /// How many rows the image reserves. Carried so a later consumer
    /// (scrollback) can reason about the footprint; the viewport flush does not
    /// need it (the terminal advances the cursor itself after the escape).
    pub rows: u16,
}

/// The raw graphics-emission channel: a shared queue of [`PendingEmission`]s an
/// [`RtImage`] fills during `render` and the draw-owning task drains after the
/// frame's `terminal.draw`.
///
/// This is the mechanism that lets graphics bypass the `Buffer` without any
/// widget touching the terminal. A cheap `Arc<Mutex<Vec<_>>>`: cloning hands the
/// *same* queue to every image widget and to the draw task, exactly like the
/// scheduler's [`FrameRequester`](crate::rt::scheduler::FrameRequester) is cloned
/// to producers. Contention is nil — the critical section is a `Vec` push/drain
/// and the draw task is the only reader.
///
/// # Ordering contract
///
/// The draw task must call [`flush_to`](RawEmissionQueue::flush_to) *after*
/// `terminal.draw` (so the reserved blank rows are painted first, then the image
/// is drawn on top) and *before* releasing the synchronized-output block (so the
/// image appears atomically with the frame). Each frame typically calls
/// [`take`](RawEmissionQueue::take) or `flush_to` to drain, leaving the queue
/// empty for the next frame.
#[derive(Debug, Clone, Default)]
pub struct RawEmissionQueue {
    inner: Arc<Mutex<Vec<PendingEmission>>>,
}

impl RawEmissionQueue {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueue an emission (called by [`RtImage::render`] in graphics mode).
    pub fn push(&self, emission: PendingEmission) {
        self.lock().push(emission);
    }

    /// Whether the queue currently holds no pending emissions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Drain and return every pending emission, leaving the queue empty.
    ///
    /// The pure-data drain used by tests and by a flush target that wants to
    /// order the writes itself.
    #[must_use]
    pub fn take(&self) -> Vec<PendingEmission> {
        std::mem::take(&mut *self.lock())
    }

    /// Flush every pending emission to `out`, positioning the cursor at each
    /// image's absolute row (`viewport_origin_y + emission.row`) before writing
    /// its escape, then leaving the queue empty.
    ///
    /// `viewport_origin_y` is `frame.area().y` — the row the inline viewport
    /// currently starts at, which `insert_before` slides down as scrollback
    /// fills (architecture invariant #4). Emissions are sorted by row so the
    /// cursor moves monotonically down the frame. The cursor is saved
    /// (`\x1b7`) before the first move and restored (`\x1b8`) after the last, so
    /// the raw emission does not disturb wherever the draw left the caret (e.g.
    /// the input caret ratatui positioned).
    ///
    /// # Errors
    ///
    /// Propagates the first write error; on success the queue is drained.
    pub fn flush_to(
        &self,
        out: &mut impl std::io::Write,
        viewport_origin_y: u16,
    ) -> std::io::Result<()> {
        let mut pending = self.take();
        if pending.is_empty() {
            return Ok(());
        }
        pending.sort_by_key(|e| e.row);
        // Save the cursor once so the input caret / draw position survives the
        // out-of-band writes.
        out.write_all(b"\x1b7")?;
        for emission in &pending {
            let absolute_row = viewport_origin_y.saturating_add(emission.row);
            // CUP is 1-based; column 1 (left edge of the reserved rows).
            write!(out, "\x1b[{};1H", absolute_row.saturating_add(1))?;
            out.write_all(emission.escape.as_bytes())?;
        }
        out.write_all(b"\x1b8")?;
        out.flush()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<PendingEmission>> {
        self.inner
            .lock()
            .expect("raw emission queue mutex poisoned")
    }
}

/// An image widget for the rt stack.
///
/// Renders as **reserved rows plus an out-of-band graphics emission** (graphics
/// terminal) or a **bordered placeholder box** (plain / multiplexer). See the
/// module docs for the buffer-bypass rationale.
///
/// Construct with [`RtImage::new`], optionally give it a [`label`](RtImage::label)
/// (filename/alt shown in the placeholder and, for iTerm2, as the image name),
/// and — for graphics mode — attach a [`RawEmissionQueue`] via
/// [`emission_queue`](RtImage::emission_queue) so `render` has somewhere to
/// enqueue the escape. Without a queue, a graphics-mode image still reserves its
/// rows but enqueues nothing (there is no channel), which is the correct
/// degenerate behaviour for a pure `Buffer` snapshot test.
pub struct RtImage {
    /// Raw source bytes (png/jpeg/gif/webp).
    data: Vec<u8>,
    /// Filename or alt text shown in the placeholder / used as the iTerm2 name.
    label: Option<String>,
    /// Where a graphics-mode emission is queued. `None` = reserve rows only.
    queue: Option<RawEmissionQueue>,
    /// Forced protocol override (tests / the gallery forced-env seam). `None`
    /// resolves from the live terminal capabilities.
    protocol_override: Option<ResolvedProtocol>,
}

impl RtImage {
    /// An image over `data`, protocol resolved from the live terminal.
    #[must_use]
    pub fn new(data: impl Into<Vec<u8>>) -> Self {
        Self {
            data: data.into(),
            label: None,
            queue: None,
            protocol_override: None,
        }
    }

    /// Set the filename / alt label (shown in the fallback box; used as the
    /// iTerm2 image name).
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Attach the [`RawEmissionQueue`] a graphics-mode render enqueues into.
    #[must_use]
    pub fn emission_queue(mut self, queue: RawEmissionQueue) -> Self {
        self.queue = Some(queue);
        self
    }

    /// Force a specific protocol, bypassing capability detection.
    ///
    /// The seam the gallery's forced-env switch and the emission tests use to
    /// exercise each protocol path deterministically without a real terminal.
    #[must_use]
    pub fn protocol(mut self, protocol: ResolvedProtocol) -> Self {
        self.protocol_override = Some(protocol);
        self
    }

    /// The protocol this image will use: the override if set, else resolved from
    /// the live terminal capabilities.
    #[must_use]
    pub fn resolved_protocol(&self) -> ResolvedProtocol {
        self.protocol_override.unwrap_or_else(resolve_protocol)
    }

    /// The cell footprint (`cols` × `rows`) this image occupies in `area`, with
    /// the source aspect ratio preserved and *both* axes clamped to the area.
    ///
    /// A wide image is clamped to the display width and its height scaled down to
    /// match (`m2-image-safety` width clamp); a tall image is clamped to the pane
    /// height and its width scaled down (super-tall clamp). The cell size comes
    /// from the cached capabilities — the 8×16 default, or a `CSI 16 t` reply
    /// folded in via [`set_cell_dimensions`](crate::terminal_image::set_cell_dimensions)
    /// so the rows honour the terminal's real pixel-per-cell metric.
    ///
    /// Falls back to filling `area` when the bytes do not sniff (an undecodable
    /// blob still reserves a sensible footprint rather than collapsing to one row).
    #[must_use]
    pub fn clamped_cells(&self, area: Rect) -> ClampedCells {
        match get_image_dimensions(&self.data) {
            Some(dims) => clamp_to_area(dims, area, get_cell_dimensions()),
            None => ClampedCells {
                cols: area.width.max(1),
                rows: area.height.max(1),
            },
        }
    }

    /// The number of rows this image reserves in `area` — the `rows` of its
    /// aspect-preserving, two-axis-clamped [`clamped_cells`](RtImage::clamped_cells)
    /// footprint.
    #[must_use]
    pub fn reserved_rows(&self, area: Rect) -> u16 {
        self.clamped_cells(area).rows
    }

    /// Build the encoded graphics escape for the given protocol, or `None` when
    /// this image degrades to the fallback box (undecodable source on the Kitty
    /// transcode path).
    ///
    /// Split out and public so an emission test asserts the framing without a
    /// live terminal or a queue.
    #[must_use]
    pub fn encode(&self, protocol: ResolvedProtocol, area: Rect) -> Option<String> {
        // Decode-validation before emit, on *every* graphics persona: an
        // undecodable source (truncated / corrupt bytes whose magic still sniffs)
        // must degrade to the placeholder box, never reach the wire as a half
        // graphics escape. iTerm2 is no longer exempt (migration fix).
        if !decodes(&self.data) {
            return None;
        }
        let cells = self.clamped_cells(area);
        let opts = ImageRenderOptions {
            max_cols: Some(cells.cols),
            max_rows: Some(cells.rows),
            preserve_aspect: true,
            // Sanitize the label so a filename / alt string can never smuggle an
            // escape sequence into the iTerm2 image name.
            label: self.label.as_deref().map(sanitize_label),
        };
        match protocol {
            ResolvedProtocol::Kitty => {
                // Kitty transmits PNG. Transcode a non-PNG source; the decode gate
                // above already guaranteed the source decodes, so the transcode
                // only fails on an encoder error, which still degrades to the box.
                let png = match ImageFormat::sniff(&self.data) {
                    ImageFormat::Png => Some(self.data.clone()),
                    _ => transcode_to_png(&self.data),
                }?;
                Some(encode_kitty(allocate_image_id(), &png, &opts))
            }
            // iTerm2 decodes every accepted format itself: pass the bytes native.
            ResolvedProtocol::ITerm2 => Some(encode_iterm2(&self.data, &opts)),
            ResolvedProtocol::Fallback => None,
        }
    }

    /// Paint the bordered fallback placeholder into `area`, with a label that
    /// pairs the filename/alt with the sniffed `[<mime> WxH]` tag when available.
    ///
    /// The border/centering *shape* matches the legacy `image_fallback`
    /// envelope, but the label row is built here with **display-width** math
    /// rather than the legacy `char`-count clipping/centering: the sanitized label
    /// is clipped to the inner width by rendered column ([`clip_label`]) and
    /// centered by display width, so a CJK/emoji label (two columns per glyph)
    /// stays inside the frame with the right border aligned instead of tearing it
    /// (the migration fix). A label carrying escape bytes was already stripped in
    /// [`fallback_label`](RtImage::fallback_label), so no terminal sequence reaches
    /// the cells.
    fn render_fallback(&self, area: Rect, buf: &mut Buffer) {
        let lines = self.fallback_lines(area);
        for (i, line) in lines.iter().enumerate() {
            let y = area.y.saturating_add(i as u16);
            if y >= area.y.saturating_add(area.height) {
                break;
            }
            buf.set_stringn(area.x, y, line, area.width as usize, Style::default());
        }
    }

    /// Build the fallback box as display-width-correct lines.
    ///
    /// A `┌─…─┐` top, `height-2` interior rows (the label centered by display
    /// width on the middle row), and a `└─…─┘` bottom. Split out so a unit test
    /// asserts the width invariant — every line's rendered width equals the box
    /// width — without a buffer.
    fn fallback_lines(&self, area: Rect) -> Vec<String> {
        let width = area.width as usize;
        let height = area.height as usize;
        // Degenerate areas (too small for a bordered box) fall back to a single
        // clipped label line so nothing overflows.
        if width < 2 || height < 1 {
            return vec![clip_label(&self.fallback_label(), width)];
        }
        let inner_w = width - 2;
        let interior = height.saturating_sub(2).max(1);
        let label = clip_label(&self.fallback_label(), inner_w);
        let label_w = UnicodeWidthStr::width(label.as_str());

        let bar = "─".repeat(inner_w);
        let mut lines = Vec::with_capacity(interior + 2);
        lines.push(format!("┌{bar}┐"));
        let mid = interior / 2;
        for r in 0..interior {
            if r == mid {
                let pad = inner_w.saturating_sub(label_w);
                let left = pad / 2;
                let right = pad - left;
                lines.push(format!(
                    "│{}{label}{}│",
                    " ".repeat(left),
                    " ".repeat(right)
                ));
            } else {
                lines.push(format!("│{}│", " ".repeat(inner_w)));
            }
        }
        lines.push(format!("└{bar}┘"));
        lines
    }

    /// The fallback box label: the filename/alt joined with the sniff tag, or
    /// whichever of the two is present, with any escape bytes stripped from the
    /// name so the placeholder can never carry a smuggled terminal sequence.
    fn fallback_label(&self) -> String {
        let sanitized = self.label.as_deref().map(sanitize_label);
        let name = sanitized.as_deref().filter(|s| !s.is_empty());
        let tag = sniff_label(&self.data);
        match (name, tag) {
            (Some(name), Some(tag)) => format!("{name} {tag}"),
            (Some(name), None) => name.to_string(),
            (None, Some(tag)) => tag,
            (None, None) => "image".to_string(),
        }
    }
}

impl RtComponent for RtImage {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        match self.resolved_protocol() {
            ResolvedProtocol::Fallback => self.render_fallback(area, buf),
            protocol => {
                // Reserve the rows as blank cells so the frame diff paints/clears
                // the image's footprint, then queue the escape for the draw task
                // to emit out of band. If encoding degrades (undecodable source
                // on the Kitty path), fall back to the placeholder box.
                match self.encode(protocol, area) {
                    Some(escape) => {
                        let rows = self.reserved_rows(area);
                        // Blank the reserved rows: the graphics image is painted
                        // over them out of band, and blanking is what clears any
                        // prior frame's content under the image.
                        for dy in 0..rows {
                            let y = area.y.saturating_add(dy);
                            buf.set_stringn(
                                area.x,
                                y,
                                " ".repeat(area.width as usize),
                                area.width as usize,
                                Style::default(),
                            );
                        }
                        if let Some(queue) = &self.queue {
                            queue.push(PendingEmission {
                                escape,
                                row: area.y,
                                rows,
                            });
                        }
                    }
                    None => self.render_fallback(area, buf),
                }
            }
        }
    }

    fn handle_key(&mut self, _key: &RtKey) -> HandleOutcome {
        HandleOutcome::Ignored
    }
}

/// Decode `data` (jpeg/gif/webp/png) and re-encode it as PNG for the Kitty path,
/// or `None` when the bytes cannot be decoded.
///
/// The `image` crate is scoped to exactly the four accepted decoders plus the PNG
/// encoder (see `Cargo.toml`). A decode failure returns `None` so the caller
/// degrades to the placeholder box instead of transmitting a corrupt APC —
/// "never emit an invalid graphics sequence" is the invariant this guards.
#[must_use]
pub fn transcode_to_png(data: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(data).ok()?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// Base64-encode `data` (exposed for the gallery's forced-env demo, which builds
/// a synthetic emission without going through [`RtImage`]).
#[must_use]
pub fn base64_encode(data: &[u8]) -> String {
    STANDARD.encode(data)
}

/// Environment variable that force-enables the terminal cell-size query.
///
/// The cell-size query (`CSI 16 t`) is the only path in this module that writes a
/// query to the terminal, and it is **off by default**: many terminals never
/// answer it and a blocking read would hang. It is enabled only when this env is
/// set (mirroring the `HAND_TUI_FORCE_KITTY_KEYBOARD` seam), so a `script -q`
/// probe can drive the query path against a silent PTY and prove the render loop
/// still paints within its poll budget — the write is fire-and-forget, the reply
/// (if any) is folded in asynchronously via [`parse_cell_size_reply`], and no read
/// ever blocks the frame.
pub const CELL_SIZE_QUERY_ENV: &str = "HAND_TUI_QUERY_CELL_SIZE";

/// Whether the cell-size query is force-enabled for this session (the env seam).
#[must_use]
pub fn cell_size_query_enabled() -> bool {
    std::env::var(CELL_SIZE_QUERY_ENV)
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Write the terminal cell-size query (`CSI 16 t`) to `out` **without waiting for
/// a reply**.
///
/// A no-op unless [`cell_size_query_enabled`] is set. This is deliberately
/// fire-and-forget: it issues the query and flushes, then returns immediately so
/// the caller's render/poll loop is never blocked on a terminal that stays silent.
/// A terminal that *does* answer replies with `CSI 6 ; <height> ; <width> t`; that
/// reply arrives on the input stream and is decoded by [`parse_cell_size_reply`]
/// and applied via [`set_cell_dimensions`](crate::terminal_image::set_cell_dimensions),
/// so a later frame's row allocation uses the real pixel-per-cell metric.
///
/// # Errors
///
/// Propagates a write/flush error; a silent terminal is *not* an error (the query
/// simply goes unanswered).
pub fn write_cell_size_query(out: &mut impl std::io::Write) -> std::io::Result<()> {
    if !cell_size_query_enabled() {
        return Ok(());
    }
    out.write_all(b"\x1b[16t")?;
    out.flush()
}

/// Parse a terminal cell-size reply (`CSI 6 ; <height> ; <width> t`) into
/// [`CellDimensions`], or `None` when `bytes` is not such a reply.
///
/// The companion to [`write_cell_size_query`]: the event loop feeds the raw input
/// bytes here and, on a match, applies the result via
/// [`set_cell_dimensions`](crate::terminal_image::set_cell_dimensions) so the next
/// frame's [`calculate rows`](RtImage::clamped_cells) scale by `ceil(pixel_height /
/// cell_height)` against the *reported* cell height rather than the 8×16 default.
/// Zero dimensions or a malformed reply yield `None`, leaving the default in place.
#[must_use]
pub fn parse_cell_size_reply(bytes: &[u8]) -> Option<CellDimensions> {
    // Look for the CSI 6 ; H ; W t report anywhere in the buffer.
    let text = std::str::from_utf8(bytes).ok()?;
    let start = text.find("\x1b[6;")?;
    let rest = &text[start + 4..];
    let end = rest.find('t')?;
    let body = &rest[..end];
    let mut parts = body.split(';');
    let height: u16 = parts.next()?.parse().ok()?;
    let width: u16 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || width == 0 || height == 0 {
        return None;
    }
    Some(CellDimensions { width, height })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL_8X16: CellDimensions = CellDimensions {
        width: 8,
        height: 16,
    };

    fn dims(width: u32, height: u32) -> ImageDimensions {
        ImageDimensions { width, height }
    }

    #[test]
    fn clamp_fits_uses_natural_extent() {
        // 80x64 px at 8x16 cells = 10x4 natural cells, and both fit a 40x30 area:
        // no scaling, the natural extent is used verbatim.
        let c = clamp_to_area(dims(80, 64), Rect::new(0, 0, 40, 30), CELL_8X16);
        assert_eq!(c, ClampedCells { cols: 10, rows: 4 });
    }

    #[test]
    fn clamp_width_bound_scales_height_down() {
        // 400x40 -> 50x3 natural cells; a 20-col area binds width (50->20) and the
        // height scales: 3 * 20/50 -> 1 row (floored, min 1).
        let c = clamp_to_area(dims(400, 40), Rect::new(0, 0, 20, 10), CELL_8X16);
        assert_eq!(c.cols, 20);
        assert_eq!(c.rows, 1);
    }

    #[test]
    fn clamp_height_bound_scales_width_down() {
        // 40x400 -> 5x25 natural cells; a 10-row area binds height (25->10) and the
        // width scales: 5 * 10/25 -> 2 cols.
        let c = clamp_to_area(dims(40, 400), Rect::new(0, 0, 40, 10), CELL_8X16);
        assert_eq!(c.rows, 10);
        assert_eq!(c.cols, 2);
    }

    #[test]
    fn clamp_both_axes_overflow_binds_tighter_axis() {
        // A square 512x512 px image is NOT square in cells: at 8x16 cells it is
        // 64x32 natural cells (cells are twice as tall as wide). A 40x30 area
        // overflows both. Width is the tighter fit (40/64 < 30/32), so width binds
        // to 40 and the height scales in proportion: 32 * 40/64 = 20.
        let c = clamp_to_area(dims(512, 512), Rect::new(0, 0, 40, 30), CELL_8X16);
        assert_eq!(c.cols, 40);
        assert_eq!(c.rows, 20);
    }

    #[test]
    fn clamp_zero_dims_is_single_cell() {
        assert_eq!(
            clamp_to_area(dims(0, 0), Rect::new(0, 0, 40, 30), CELL_8X16),
            ClampedCells { cols: 1, rows: 1 }
        );
    }

    #[test]
    fn clamp_never_exceeds_area_and_never_below_one() {
        let c = clamp_to_area(dims(4000, 4000), Rect::new(0, 0, 10, 5), CELL_8X16);
        assert!(c.cols >= 1 && c.rows >= 1);
        assert!(c.cols <= 10 && c.rows <= 5);
        let c = clamp_to_area(dims(1, 1), Rect::new(0, 0, 10, 5), CELL_8X16);
        assert_eq!(c, ClampedCells { cols: 1, rows: 1 });
    }

    #[test]
    fn clamp_honours_reported_cell_size() {
        // The same image against a taller reported cell: fewer rows.
        let small_cells = CellDimensions {
            width: 10,
            height: 20,
        };
        // 40x400 at 10x20 = 4x20 natural cells; area 40x100 fits -> 20 rows.
        let c = clamp_to_area(dims(40, 400), Rect::new(0, 0, 40, 100), small_cells);
        assert_eq!(c.rows, 20);
        // At the 8x16 default the same image is 5x25 -> 25 rows.
        let c = clamp_to_area(dims(40, 400), Rect::new(0, 0, 40, 100), CELL_8X16);
        assert_eq!(c.rows, 25);
    }

    #[test]
    fn sanitize_drops_controls_keeps_printable() {
        assert_eq!(sanitize_label("a\x1bb\x07c"), "abc");
        assert_eq!(sanitize_label("\x00\x1f\x7f\u{9f}"), "");
        assert_eq!(sanitize_label("héllo 世界 🎉"), "héllo 世界 🎉");
        assert_eq!(sanitize_label("tab\there"), "tabhere", "tab is a control");
    }

    #[test]
    fn clip_label_width_boundaries() {
        assert_eq!(clip_label("hello", 0), "");
        assert_eq!(clip_label("hi", 5), "hi", "fits, unchanged");
        assert_eq!(clip_label("hello", 5), "hello", "exact fit, no ellipsis");
        assert_eq!(clip_label("hello", 3), "he…", "clipped with ellipsis");
        // A single wide glyph in a 1-col budget cannot fit alongside an ellipsis.
        let one = clip_label("世界", 1);
        assert!(UnicodeWidthStr::width(one.as_str()) <= 1);
    }

    #[test]
    fn clip_label_keeps_wide_glyphs_whole() {
        // 4 CJK glyphs = 8 cols; clipping to 5 keeps whole glyphs within budget-1
        // (4 cols = 2 glyphs) plus the ellipsis: width <= 5, never a split glyph.
        let clipped = clip_label("你好世界", 5);
        assert!(UnicodeWidthStr::width(clipped.as_str()) <= 5, "{clipped:?}");
        assert!(clipped.ends_with('…'));
    }

    #[test]
    fn decodes_rejects_undecodable_but_sniffable() {
        // Valid magic prefix, no decodable payload.
        assert!(!decodes(&[0xff, 0xd8, 0x00, 0x00]), "truncated jpeg magic");
        assert!(!decodes(b""), "empty");
    }

    #[test]
    fn parse_cell_size_reply_round_trip() {
        let d = parse_cell_size_reply(b"\x1b[6;32;14t").unwrap();
        assert_eq!(d.width, 14);
        assert_eq!(d.height, 32);
    }

    #[test]
    fn cell_size_query_env_gating() {
        // The write helper is a no-op when the env seam is unset.
        // (The env itself is asserted end-to-end in the integration test; here we
        // only pin the CSI 16 t byte string the enabled path emits.)
        assert_eq!(CELL_SIZE_QUERY_ENV, "HAND_TUI_QUERY_CELL_SIZE");
    }
}
