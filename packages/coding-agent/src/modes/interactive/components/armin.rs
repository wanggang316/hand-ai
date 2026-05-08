//! Easter-egg [`ArminComponent`]: a 31×36 XBM image of "Armin says hi"
//! revealed with one of seven animation effects.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/armin.ts`.
//!
//! The TS component drives itself with `setInterval(_, 1000/fps)` and calls
//! `tui.requestRender()` on every tick. The Rust port mirrors the
//! ownership-inversion pattern used by [`super::countdown_timer::CountdownTimer`]
//! and [`super::daxnuts::DaxnutsComponent`]: callers invoke
//! [`ArminComponent::tick`] from whatever cadence their driver provides
//! (frame loop, `tokio::time::interval`, manual test stepping). The tick rate
//! is per-effect (30 fps for most, 60 fps for `glitch`) and reported via
//! [`ArminComponent::tick_interval`] so drivers can schedule appropriately.
//!
//! Parity scope: the data table and six effects (`Fade`, `Scanline`,
//! `Typewriter`, `Rain`, `Crt`, `Glitch`) are ported. The remaining one
//! (`Dissolve`) is tracked as `// TODO(parity): port additional reveal
//! effects` and currently falls back to instant-reveal so callers never get a
//! frozen splash.

use std::time::Duration;

use hand_tui::Component;
use rand::{Rng, SeedableRng, rngs::StdRng};

/// Image width in pixels.
pub const WIDTH: usize = 31;
/// Image height in pixels.
pub const HEIGHT: usize = 36;

const BYTES_PER_ROW: usize = WIDTH.div_ceil(8);
/// Half-block render rows (each terminal row stacks two pixel rows).
pub const DISPLAY_HEIGHT: usize = HEIGHT.div_ceil(2);

/// Raw XBM bitmap: `1` = background, `0` = foreground, LSB-first per byte.
const BITS: [u8; 144] = [
    0xff, 0xff, 0xff, 0x7f, 0xff, 0xf0, 0xff, 0x7f, 0xff, 0xed, 0xff, 0x7f, 0xff, 0xdb, 0xff, 0x7f,
    0xff, 0xb7, 0xff, 0x7f, 0xff, 0x77, 0xfe, 0x7f, 0x3f, 0xf8, 0xfe, 0x7f, 0xdf, 0xff, 0xfe, 0x7f,
    0xdf, 0x3f, 0xfc, 0x7f, 0x9f, 0xc3, 0xfb, 0x7f, 0x6f, 0xfc, 0xf4, 0x7f, 0xf7, 0x0f, 0xf7, 0x7f,
    0xf7, 0xff, 0xf7, 0x7f, 0xf7, 0xff, 0xe3, 0x7f, 0xf7, 0x07, 0xe8, 0x7f, 0xef, 0xf8, 0x67, 0x70,
    0x0f, 0xff, 0xbb, 0x6f, 0xf1, 0x00, 0xd0, 0x5b, 0xfd, 0x3f, 0xec, 0x53, 0xc1, 0xff, 0xef, 0x57,
    0x9f, 0xfd, 0xee, 0x5f, 0x9f, 0xfc, 0xae, 0x5f, 0x1f, 0x78, 0xac, 0x5f, 0x3f, 0x00, 0x50, 0x6c,
    0x7f, 0x00, 0xdc, 0x77, 0xff, 0xc0, 0x3f, 0x78, 0xff, 0x01, 0xf8, 0x7f, 0xff, 0x03, 0x9c, 0x78,
    0xff, 0x07, 0x8c, 0x7c, 0xff, 0x0f, 0xce, 0x78, 0xff, 0xff, 0xcf, 0x7f, 0xff, 0xff, 0xcf, 0x78,
    0xff, 0xff, 0xdf, 0x78, 0xff, 0xff, 0xdf, 0x7d, 0xff, 0xff, 0x3f, 0x7e, 0xff, 0xff, 0xff, 0x7f,
];

/// Reveal effect. The TS source picks one uniformly at random per
/// construction; the Rust port lets callers pick deterministically (good for
/// tests) and exposes [`Effect::random`] for the shipped behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Reveal pixels in row-major order, three at a time per tick.
    Typewriter,
    /// Reveal one full row per tick top-down.
    Scanline,
    /// Falling-rain "Matrix" effect.
    Rain,
    /// Reveal random pixels at a constant rate.
    Fade,
    /// CRT style: open from the middle row outward.
    Crt,
    /// Briefly glitch then snap to the final image.
    Glitch,
    /// Resolve from random noise to the final image.
    Dissolve,
}

impl Effect {
    /// All seven effects in TS source order.
    pub const ALL: [Effect; 7] = [
        Effect::Typewriter,
        Effect::Scanline,
        Effect::Rain,
        Effect::Fade,
        Effect::Crt,
        Effect::Glitch,
        Effect::Dissolve,
    ];

    /// Pick one effect uniformly at random. Mirrors the TS constructor.
    pub fn random() -> Self {
        // SystemTime nanos as a cheap entropy source; we don't need cryptographic
        // randomness for an easter-egg picker.
        let idx = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize)
            .unwrap_or(0))
            % Self::ALL.len();
        Self::ALL[idx]
    }

    /// Tick cadence the TS source uses (`60` fps for `glitch`, `30` fps
    /// otherwise).
    pub fn tick_interval(&self) -> Duration {
        match self {
            Effect::Glitch => Duration::from_millis(1000 / 60),
            _ => Duration::from_millis(1000 / 30),
        }
    }
}

/// Whether pixel `(x, y)` is foreground (`true`) or background (`false`).
fn get_pixel(x: usize, y: usize) -> bool {
    if y >= HEIGHT {
        return false;
    }
    let byte_index = y * BYTES_PER_ROW + x / 8;
    let bit_index = x % 8;
    ((BITS[byte_index] >> bit_index) & 1) == 0
}

/// Render the half-block character for cell `(x, row)`.
fn get_char(x: usize, row: usize) -> char {
    let upper = get_pixel(x, row * 2);
    let lower = get_pixel(x, row * 2 + 1);
    match (upper, lower) {
        (true, true) => '\u{2588}',  // full block
        (true, false) => '\u{2580}', // upper half block
        (false, true) => '\u{2584}', // lower half block
        _ => ' ',
    }
}

/// Build the fully-revealed image grid.
fn build_final_grid() -> Vec<Vec<char>> {
    (0..DISPLAY_HEIGHT)
        .map(|row| (0..WIDTH).map(|x| get_char(x, row)).collect())
        .collect()
}

fn empty_grid() -> Vec<Vec<char>> {
    vec![vec![' '; WIDTH]; DISPLAY_HEIGHT]
}

/// Per-column state for the [`Effect::Rain`] effect.
#[derive(Debug, Clone, Copy)]
struct RainDrop {
    /// Current vertical position of the falling pixel. Negative values mean
    /// the drop is still above the visible area.
    y: i32,
    /// Number of rows already revealed at the bottom of this column.
    settled: usize,
}

/// Per-effect mutable state. Only the ported effects carry meaningful state;
/// the rest fall back to instant-reveal so the component never freezes
/// mid-animation.
enum EffectState {
    Scanline {
        row: usize,
    },
    Fade {
        positions: Vec<(usize, usize)>,
        idx: usize,
    },
    Typewriter {
        pos: usize,
    },
    Rain {
        drops: Vec<RainDrop>,
    },
    Crt {
        expansion: usize,
    },
    Glitch {
        phase: usize,
    },
    /// Placeholder for the effects not yet ported. Always reveals on the
    /// first tick — see TS source for the actual animation.
    InstantReveal {
        revealed: bool,
    },
}

/// Easter-egg "Armin says hi" splash component.
pub struct ArminComponent {
    effect: Effect,
    final_grid: Vec<Vec<char>>,
    current_grid: Vec<Vec<char>>,
    state: EffectState,
    rng: StdRng,
    done: bool,
}

impl ArminComponent {
    /// Construct with the `Effect` chosen by [`Effect::random`].
    pub fn new() -> Self {
        Self::with_effect(Effect::random())
    }

    /// Construct with a specific effect, seeded from OS entropy.
    pub fn with_effect(effect: Effect) -> Self {
        Self::build(effect, StdRng::from_entropy())
    }

    /// Construct with a specific effect and a deterministic seed (useful
    /// for tests and reproducible builds).
    pub fn with_effect_seeded(effect: Effect, seed: u64) -> Self {
        Self::build(effect, StdRng::seed_from_u64(seed))
    }

    fn build(effect: Effect, mut rng: StdRng) -> Self {
        let final_grid = build_final_grid();
        let current_grid = empty_grid();
        let state = init_state(effect, &mut rng);
        Self {
            effect,
            final_grid,
            current_grid,
            state,
            rng,
            done: false,
        }
    }

    /// Effect this instance is animating with.
    pub fn effect(&self) -> Effect {
        self.effect
    }

    /// Tick cadence the driver should use for this effect.
    pub fn tick_interval(&self) -> Duration {
        self.effect.tick_interval()
    }

    /// Advance one frame. Returns `true` while the animation is still
    /// running and `false` once it has reached its final frame. Subsequent
    /// calls after completion are no-ops and continue to return `false`.
    pub fn tick(&mut self) -> bool {
        if self.done {
            return false;
        }
        let finished = match &mut self.state {
            EffectState::Scanline { row } => {
                tick_scanline(row, &self.final_grid, &mut self.current_grid)
            }
            EffectState::Fade { positions, idx } => {
                tick_fade(positions, idx, &self.final_grid, &mut self.current_grid)
            }
            EffectState::Typewriter { pos } => {
                tick_typewriter(pos, &self.final_grid, &mut self.current_grid)
            }
            EffectState::Rain { drops } => tick_rain(
                drops,
                &self.final_grid,
                &mut self.current_grid,
                &mut self.rng,
            ),
            EffectState::Crt { expansion } => {
                tick_crt(expansion, &self.final_grid, &mut self.current_grid)
            }
            EffectState::Glitch { phase } => tick_glitch(
                phase,
                &self.final_grid,
                &mut self.current_grid,
                &mut self.rng,
            ),
            EffectState::InstantReveal { revealed } => {
                if !*revealed {
                    self.current_grid = self.final_grid.clone();
                    *revealed = true;
                }
                true
            }
        };
        if finished {
            self.done = true;
            return false;
        }
        true
    }

    /// Whether the animation has completed.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Snapshot of the current grid (mostly for tests).
    pub fn current_grid(&self) -> &[Vec<char>] {
        &self.current_grid
    }
}

impl Default for ArminComponent {
    fn default() -> Self {
        Self::new()
    }
}

fn init_state(effect: Effect, rng: &mut StdRng) -> EffectState {
    match effect {
        Effect::Scanline => EffectState::Scanline { row: 0 },
        Effect::Typewriter => EffectState::Typewriter { pos: 0 },
        Effect::Crt => EffectState::Crt { expansion: 0 },
        Effect::Glitch => EffectState::Glitch { phase: 0 },
        Effect::Rain => {
            // Each column starts with a drop somewhere above the visible
            // area. Mirrors `-Math.floor(Math.random() * DISPLAY_HEIGHT * 2)`
            // in the TS source (range `(-DISPLAY_HEIGHT * 2, 0]`).
            let drops = (0..WIDTH)
                .map(|_| RainDrop {
                    y: -(rng.gen_range(0..(DISPLAY_HEIGHT as i32 * 2))),
                    settled: 0,
                })
                .collect();
            EffectState::Rain { drops }
        }
        Effect::Fade => {
            let mut positions = Vec::with_capacity(DISPLAY_HEIGHT * WIDTH);
            for row in 0..DISPLAY_HEIGHT {
                for x in 0..WIDTH {
                    positions.push((row, x));
                }
            }
            // Fisher-Yates shuffle using the same nano-time entropy as
            // `Effect::random`. Shuffle quality is not load-bearing for an
            // easter egg.
            let mut seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64)
                .unwrap_or(0)
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            for i in (1..positions.len()).rev() {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let j = (seed >> 33) as usize % (i + 1);
                positions.swap(i, j);
            }
            EffectState::Fade { positions, idx: 0 }
        }
        // TODO(parity): port the dissolve reveal effect. For now it
        // instantly reveals on the first tick so the component never freezes
        // mid-animation.
        Effect::Dissolve => EffectState::InstantReveal { revealed: false },
    }
}

fn tick_scanline(row: &mut usize, final_grid: &[Vec<char>], current: &mut [Vec<char>]) -> bool {
    if *row >= DISPLAY_HEIGHT {
        return true;
    }
    for x in 0..WIDTH {
        current[*row][x] = final_grid[*row][x];
    }
    *row += 1;
    *row >= DISPLAY_HEIGHT
}

fn tick_rain(
    drops: &mut [RainDrop],
    final_grid: &[Vec<char>],
    current: &mut Vec<Vec<char>>,
    rng: &mut StdRng,
) -> bool {
    let mut all_settled = true;
    *current = empty_grid();
    let display_height_i = DISPLAY_HEIGHT as i32;

    for (x, drop) in drops.iter_mut().enumerate() {
        // Draw the already-settled tail of this column.
        if drop.settled > 0 {
            let start = DISPLAY_HEIGHT - drop.settled;
            for row in start..DISPLAY_HEIGHT {
                current[row][x] = final_grid[row][x];
            }
        }

        // Whole column already revealed; nothing to animate.
        if drop.settled >= DISPLAY_HEIGHT {
            continue;
        }

        // Find the lowest still-unrevealed foreground row in this column.
        // Scans bottom-up over rows `[0, DISPLAY_HEIGHT - 1 - drop.settled]`.
        let mut target_row: Option<usize> = None;
        let upper_excl = DISPLAY_HEIGHT - drop.settled;
        for row in (0..upper_excl).rev() {
            if final_grid[row][x] != ' ' {
                target_row = Some(row);
                break;
            }
        }

        // No foreground left in this column: mark it complete to guarantee
        // termination. (Diverges from the TS source which would loop
        // forever — see module docs.)
        let target_row = match target_row {
            Some(r) => r,
            None => {
                drop.settled = DISPLAY_HEIGHT;
                continue;
            }
        };

        all_settled = false;

        // Advance the drop one row.
        drop.y += 1;

        if drop.y >= 0 && drop.y < display_height_i {
            if drop.y as usize >= target_row {
                // Settle on the target row and re-spawn above the screen.
                drop.settled = DISPLAY_HEIGHT - target_row;
                drop.y = -(rng.gen_range(0..5) + 1);
            } else {
                // Still falling: render a streak character.
                current[drop.y as usize][x] = '\u{2593}';
            }
        }
    }

    all_settled
}

/// Number of glitch frames before the final clean image is shown.
const GLITCH_FRAMES: usize = 8;

fn tick_glitch(
    phase: &mut usize,
    final_grid: &[Vec<char>],
    current: &mut Vec<Vec<char>>,
    rng: &mut StdRng,
) -> bool {
    if *phase < GLITCH_FRAMES {
        // Build a corrupted frame: each row is either kept, swapped with a
        // random row, or cyclically shifted.
        let mut next: Vec<Vec<char>> = Vec::with_capacity(DISPLAY_HEIGHT);
        for row in final_grid {
            // Range mirrors `Math.floor(Math.random() * 7) - 3` → `[-3, 3]`.
            let offset: i32 = rng.gen_range(0..7) - 3;
            let shift_roll: f32 = rng.r#gen();
            let swap_roll: f32 = rng.r#gen();

            if shift_roll < 0.30 {
                // Cyclic shift; positive offset rotates left, negative
                // rotates right. Offset of 0 leaves the row unchanged.
                let mut row_buf = row.clone();
                if offset > 0 {
                    row_buf.rotate_left(offset as usize);
                } else if offset < 0 {
                    row_buf.rotate_right((-offset) as usize);
                }
                next.push(row_buf);
            } else if swap_roll < 0.20 {
                // Vertical swap: replace this row with a random row.
                let swap_row = rng.gen_range(0..DISPLAY_HEIGHT);
                next.push(final_grid[swap_row].clone());
            } else {
                next.push(row.clone());
            }
        }
        *current = next;
        *phase += 1;
        return false;
    }

    // Final frame: snap to the clean image.
    *current = final_grid.to_vec();
    true
}

fn tick_crt(expansion: &mut usize, final_grid: &[Vec<char>], current: &mut Vec<Vec<char>>) -> bool {
    let mid_row = DISPLAY_HEIGHT / 2;
    *current = empty_grid();

    // Symmetric vertical sweep starting from the middle row.
    let top = mid_row.saturating_sub(*expansion);
    // `mid_row + expansion` may overflow `DISPLAY_HEIGHT`; clamp to the last
    // valid index.
    let bottom = (mid_row + *expansion).min(DISPLAY_HEIGHT - 1);
    for row in top..=bottom {
        for x in 0..WIDTH {
            current[row][x] = final_grid[row][x];
        }
    }

    *expansion += 1;
    *expansion > DISPLAY_HEIGHT
}

fn tick_typewriter(pos: &mut usize, final_grid: &[Vec<char>], current: &mut [Vec<char>]) -> bool {
    let pixels_per_frame = 3;
    for _ in 0..pixels_per_frame {
        let row = *pos / WIDTH;
        let x = *pos % WIDTH;
        if row >= DISPLAY_HEIGHT {
            return true;
        }
        current[row][x] = final_grid[row][x];
        *pos += 1;
    }
    *pos / WIDTH >= DISPLAY_HEIGHT
}

fn tick_fade(
    positions: &[(usize, usize)],
    idx: &mut usize,
    final_grid: &[Vec<char>],
    current: &mut [Vec<char>],
) -> bool {
    let pixels_per_frame = 15;
    for _ in 0..pixels_per_frame {
        if *idx >= positions.len() {
            return true;
        }
        let (row, x) = positions[*idx];
        current[row][x] = final_grid[row][x];
        *idx += 1;
    }
    *idx >= positions.len()
}

/// Hardcoded accent colour while the theme integration is deferred (matches
/// the dark-theme `accent` slot used by `oauth_selector` and `daxnuts`).
const ACCENT: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

impl Component for ArminComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let width = width as usize;
        let padding = 1;
        let available_width = width.saturating_sub(padding);

        let mut lines: Vec<String> = self
            .current_grid
            .iter()
            .map(|row| {
                let clipped: String = row.iter().take(available_width).collect();
                let visible_len = clipped.chars().count();
                let pad_right = width.saturating_sub(padding + visible_len);
                format!(" {ACCENT}{clipped}{RESET}{}", " ".repeat(pad_right))
            })
            .collect();

        let message = "ARMIN SAYS HI";
        let pad_right = width.saturating_sub(padding + message.len());
        lines.push(format!(
            " {ACCENT}{message}{RESET}{}",
            " ".repeat(pad_right)
        ));

        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanline_reveals_one_row_per_tick() {
        let mut c = ArminComponent::with_effect(Effect::Scanline);
        // Initially everything is blanks.
        assert!(
            c.current_grid()
                .iter()
                .all(|row| row.iter().all(|&ch| ch == ' '))
        );

        // First tick reveals row 0.
        c.tick();
        assert!(c.current_grid()[0].iter().any(|&ch| ch != ' '));
        // Row 1 still blank.
        assert!(c.current_grid()[1].iter().all(|&ch| ch == ' '));

        // Tick to completion.
        for _ in 0..DISPLAY_HEIGHT {
            c.tick();
        }
        assert!(c.is_done());
        // After completion further ticks are no-ops.
        assert!(!c.tick());
    }

    #[test]
    fn fade_reveals_pixels_per_frame() {
        let mut c = ArminComponent::with_effect(Effect::Fade);
        let total_pixels = DISPLAY_HEIGHT * WIDTH;
        // Conservative upper bound: ceil(total / 15) ticks.
        let max_ticks = total_pixels.div_ceil(15) + 2;
        for _ in 0..max_ticks {
            c.tick();
            if c.is_done() {
                break;
            }
        }
        assert!(c.is_done(), "fade should complete in <= {max_ticks} ticks");
        // Final grid should match the fully-revealed image.
        let final_grid = build_final_grid();
        assert_eq!(c.current_grid(), final_grid.as_slice());
    }

    #[test]
    fn typewriter_reveals_three_pixels_per_tick_in_row_major_order() {
        let mut c = ArminComponent::with_effect(Effect::Typewriter);
        // Initially blank.
        assert!(
            c.current_grid()
                .iter()
                .all(|row| row.iter().all(|&ch| ch == ' '))
        );

        // First tick: positions 0..3 in row 0 should match the final grid.
        c.tick();
        let final_grid = build_final_grid();
        for x in 0..3 {
            assert_eq!(c.current_grid()[0][x], final_grid[0][x]);
        }
        // Position 3 still blank.
        assert_eq!(c.current_grid()[0][3], ' ');

        // Tick to completion. Total pixels = DISPLAY_HEIGHT * WIDTH; 3/tick.
        let total_pixels = DISPLAY_HEIGHT * WIDTH;
        let max_ticks = total_pixels.div_ceil(3) + 2;
        for _ in 0..max_ticks {
            c.tick();
            if c.is_done() {
                break;
            }
        }
        assert!(
            c.is_done(),
            "typewriter should complete in <= {max_ticks} ticks"
        );
        assert_eq!(c.current_grid(), final_grid.as_slice());
    }

    #[test]
    fn rain_terminates_and_reveals_final_image_with_seeded_rng() {
        let mut c = ArminComponent::with_effect_seeded(Effect::Rain, 0);
        // Worst case bound: each column needs at most O(DISPLAY_HEIGHT *
        // initial_offset) ticks to settle every row, with re-spawn delays of
        // up to 5. 10x DISPLAY_HEIGHT^2 is a generous ceiling.
        let max_ticks = DISPLAY_HEIGHT * DISPLAY_HEIGHT * 10;
        let mut ticks = 0;
        loop {
            ticks += 1;
            c.tick();
            if c.is_done() {
                break;
            }
            assert!(
                ticks < max_ticks,
                "rain should settle within {max_ticks} ticks (took {ticks})"
            );
        }
        assert_eq!(c.current_grid(), build_final_grid().as_slice());
    }

    #[test]
    fn rain_is_deterministic_for_same_seed() {
        let mut a = ArminComponent::with_effect_seeded(Effect::Rain, 42);
        let mut b = ArminComponent::with_effect_seeded(Effect::Rain, 42);
        for _ in 0..50 {
            a.tick();
            b.tick();
            assert_eq!(a.current_grid(), b.current_grid());
        }
    }

    #[test]
    fn crt_expands_symmetrically_from_middle_row() {
        let mut c = ArminComponent::with_effect(Effect::Crt);
        let final_grid = build_final_grid();
        let mid_row = DISPLAY_HEIGHT / 2;

        // Tick 1: only the middle row should be revealed.
        c.tick();
        for (row, current) in c.current_grid().iter().enumerate() {
            if row == mid_row {
                assert_eq!(current, &final_grid[row]);
            } else {
                assert!(
                    current.iter().all(|&ch| ch == ' '),
                    "row {row} should still be blank after first tick",
                );
            }
        }

        // Tick 2: rows mid-1, mid, mid+1 revealed (when in range).
        c.tick();
        let lo = mid_row.saturating_sub(1);
        let hi = (mid_row + 1).min(DISPLAY_HEIGHT - 1);
        assert_eq!(&c.current_grid()[lo..=hi], &final_grid[lo..=hi]);

        // Tick to completion.
        for _ in 0..(DISPLAY_HEIGHT + 4) {
            c.tick();
            if c.is_done() {
                break;
            }
        }
        assert!(c.is_done());
        assert_eq!(c.current_grid(), final_grid.as_slice());
    }

    #[test]
    fn glitch_runs_eight_corrupt_frames_then_settles() {
        let mut c = ArminComponent::with_effect_seeded(Effect::Glitch, 0);
        // The TS source advances 8 corrupt frames before snapping to the
        // clean image. Each corrupt tick keeps `is_done` false.
        for i in 0..GLITCH_FRAMES {
            assert!(c.tick(), "tick {i} should still be running");
            assert!(!c.is_done(), "should not be done before final frame");
        }
        // Next tick produces the clean final frame.
        c.tick();
        assert!(
            c.is_done(),
            "glitch should be done after {GLITCH_FRAMES} + 1 ticks"
        );
        assert_eq!(c.current_grid(), build_final_grid().as_slice());
    }

    #[test]
    fn glitch_is_deterministic_for_same_seed() {
        let mut a = ArminComponent::with_effect_seeded(Effect::Glitch, 7);
        let mut b = ArminComponent::with_effect_seeded(Effect::Glitch, 7);
        for _ in 0..GLITCH_FRAMES + 2 {
            a.tick();
            b.tick();
            assert_eq!(a.current_grid(), b.current_grid());
        }
    }

    #[test]
    fn dissolve_instant_reveal_completes_in_one_tick() {
        // TODO(parity): replace with a real animation test once the
        // dissolve effect is ported.
        let mut c = ArminComponent::with_effect(Effect::Dissolve);
        c.tick();
        assert_eq!(c.current_grid(), build_final_grid().as_slice());
    }

    #[test]
    fn tick_interval_matches_effect_fps() {
        assert_eq!(Effect::Glitch.tick_interval(), Duration::from_millis(16));
        assert_eq!(Effect::Scanline.tick_interval(), Duration::from_millis(33));
        assert_eq!(Effect::Fade.tick_interval(), Duration::from_millis(33));
    }

    #[test]
    fn render_produces_display_height_plus_caption() {
        let c = ArminComponent::with_effect(Effect::Scanline);
        let lines = c.render(80);
        assert_eq!(lines.len(), DISPLAY_HEIGHT + 1);
        // Last line is the caption.
        assert!(lines.last().unwrap().contains("ARMIN SAYS HI"));
    }

    #[test]
    fn final_grid_is_31_by_18_with_foreground_pixels() {
        let g = build_final_grid();
        assert_eq!(g.len(), DISPLAY_HEIGHT);
        for row in &g {
            assert_eq!(row.len(), WIDTH);
        }
        // Sanity-check: at least one pixel rendered as a block.
        let has_block = g.iter().any(|row| {
            row.iter()
                .any(|&c| c == '\u{2588}' || c == '\u{2580}' || c == '\u{2584}')
        });
        assert!(has_block, "expected at least one rendered pixel");
    }
}
