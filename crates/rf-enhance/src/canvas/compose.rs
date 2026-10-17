//! The margin side of the canvas model: what a margin pixel shows, composed
//! the way the picture is (ticket W33-03; the logic is W31-07 .. W33-02's).
//!
//! * Margin order, front to back (W32-08): per picture row, by the 4:3
//!   picture's own evidence ([`super::hud::row_orders`]); BG1 > BG2 > BG3 > BG4
//!   where a row has no evidence.
//! * A margin cell no layer knows shows the row's backdrop colour, bsnes-hd's
//!   "auto" fill, not black. Since W33-02 that colour is RECONSTRUCTED from
//!   the row's registers ([`backdrop_color`]), not voted from the picture.
//! * A remembered cell is resolved with the palette it was seen under
//!   ([`super::palette_table`]), not the palette of the row being drawn now.
//! * Sub-screen layers sit behind the main ones through the row's colour math
//!   ([`margin_color`]).
//!
//! Pure functions over a [`LayerCanvases`] snapshot; nothing here reads a core.
use std::collections::HashMap;

use rf_core_api::color::{bgr555_to_rgb, color_math_bgr555};
use rf_core_api::{ColorMathOp, FrameBundle, LineRegisters, PixelLayer, PpuPixel};

use super::palette_table::Cgram;
use super::placement::LayerFrames;
use super::{LayerCanvases, LAYERS};

/// The margin pixel at `(dx, dy)` from the composite picture's left edge /
/// top row: the front-most layer with a known cell at that layer's own world
/// position, else `None`. `origins` are this frame's
/// [`LayerFrames::origins`]; HUD layers give nothing; `order` is the row's
/// front-to-back order ([`super::hud::row_orders`]), BG1 first
/// when `None`.
#[must_use]
pub fn front_layer_pixel(
    layers: &LayerCanvases,
    origins: &[Option<(i64, i64)>; LAYERS],
    dx: i64,
    dy: i64,
    order: Option<&[u8; LAYERS]>,
) -> Option<PpuPixel> {
    front_layer_pixel_in(layers, |n| origins[n], dx, dy, order, 0x0F)
}

/// [`front_layer_pixel`] with each layer's origin from `origin_of` (the
/// per-line scroll makes it depend on the row, [`LayerFrames::origin_at`])
/// and only the layers whose bit is set in `screen` (a `$212C`/`$212D`
/// mask, BG1 = bit 0).
#[must_use]
pub fn front_layer_pixel_in(
    layers: &LayerCanvases,
    origin_of: impl Fn(usize) -> Option<(i64, i64)>,
    dx: i64,
    dy: i64,
    order: Option<&[u8; LAYERS]>,
    screen: u8,
) -> Option<PpuPixel> {
    const DEFAULT: [u8; LAYERS] = [0, 1, 2, 3];
    order.unwrap_or(&DEFAULT).iter().find_map(|&n| {
        let n = usize::from(n);
        if screen & (1 << n) == 0 || *layers.hud.get(n)? {
            return None;
        }
        let canvas = layers.canvases.get(n)?.as_ref()?;
        let (ox, oy) = origin_of(n)?;
        canvas.get(ox + dx, oy + dy)
    })
}

/// Ticket W33-01: one margin pixel composed the way the picture is.
///
/// For the dot `dx` columns from the picture's left edge on picture row
/// `row`: the MAIN source is the front-most known cell of a layer on the
/// main screen on that row (`regs.tm`), else the row's backdrop colour; the
/// SUB source is the front-most known cell of a layer on the sub screen
/// (`regs.ts`) when the row's `$2130` bit 1 selects the sub screen, else the
/// fixed colour (`$2132`). Then the row's colour math runs exactly as the
/// picture's does: fullsnes "SNES PPU Color-Math": per 5-bit channel add or
/// subtract (`$2131` bit 7), clamp to 0..31, halve when `$2131` bit 6 says so
/// EXCEPT where the sub screen is selected but transparent (the fixed colour
/// stands in, and hardware does not halve it), and only where `$2131` enables
/// math for the main source (BG1-4 bits 0-3, backdrop bit 5). `$2130` bits
/// 6-7 = 3 force the main screen black first; bits 4-5 = 3 turn math off.
/// The colour WINDOW cases (modes 1 and 2 of either) cannot be evaluated off
/// the 256-dot picture, so a margin treats them as "no window effect".
/// Sprites are never known in a margin. Brightness (`$2100`) last.
///
/// Why this is what the hills need: Super Mario World keeps its far hills
/// (BG2) on the sub screen only and shows them through colour math over the
/// backdrop. A compose that only looks at main-screen layers draws sky there.
#[must_use]
pub fn margin_color(
    layers: &LayerCanvases,
    frames: &LayerFrames,
    row: usize,
    dx: i64,
    order: Option<&[u8; LAYERS]>,
    regs: &LineRegisters,
    current: &Cgram,
) -> [u8; 3] {
    let dy = row as i64;
    let at = |screen: u8| {
        front_layer_pixel_in(layers, |n| frames.origin_at(n, row), dx, dy, order, screen)
    };
    // W33-02: a remembered cell shows under the palette it was seen with.
    let word = |p: &PpuPixel| layers.palettes.word(p, current);
    let main = at(regs.tm & 0x0F);
    let (main_word, main_bit) = match main {
        Some(p) => (
            word(&p),
            match p.layer {
                PixelLayer::Background(n) => u32::from(n),
                _ => 5,
            },
        ),
        None => (regs.backdrop_color, 5),
    };
    let sub = regs
        .use_subscreen
        .then(|| at(regs.ts & 0x0F))
        .flatten()
        .map(|p| word(&p));
    shade(regs, main_word, main_bit, sub)
}

/// One dot's final colour: the row's colour math over `main_word` (a
/// BGR555 word whose source is `main_bit` of `$2131`: BG1-4 = 0-3, OBJ = 4,
/// backdrop = 5) and the second operand (`sub`, the sub-screen's word, or
/// `None` for a transparent sub screen = the fixed colour `$2132`), then
/// brightness. The rules are fullsnes "SNES PPU Color-Math" (see
/// [`margin_color`]); shared so the backdrop and the cells agree.
fn shade(regs: &LineRegisters, main_word: u16, main_bit: u32, sub: Option<u16>) -> [u8; 3] {
    let main_black = regs.clip_mode == 3;
    let main_word = if main_black { 0 } else { main_word };
    let op = if regs.prevent_mode == 3 || regs.math_enable & (1 << main_bit) == 0 {
        ColorMathOp::None
    } else {
        let half = regs.half && !main_black && !(regs.use_subscreen && sub.is_none());
        match (regs.subtract, half) {
            (false, false) => ColorMathOp::Add,
            (false, true) => ColorMathOp::AddHalf,
            (true, false) => ColorMathOp::Subtract,
            (true, true) => ColorMathOp::SubtractHalf,
        }
    };
    let shown = color_math_bgr555(main_word, sub.unwrap_or(regs.fixed_color), op);
    bgr555_to_rgb(shown, regs.brightness)
}

/// Ticket W33-02: the colour the BACKDROP shows on a row, reconstructed from
/// the registers the row was drawn with (fullsnes "SNES PPU Color-Math" and
/// "2100h - INIDISP"): CGRAM entry 0 as the main source (black when `$2130`
/// bits 6-7 force the main screen black), combined with the fixed colour
/// `$2132` when `$2131` bit 5 enables math for the backdrop (add/subtract
/// per bit 7, halved per bit 6 EXCEPT that a half is not applied when the
/// main screen is forced black), unless `$2130` bits 4-5 = 3 turn math off,
/// then scaled by the row's brightness (0 = force-blank = black).
///
/// This is what Super Mario World's HDMA sky gradient is: the fixed colour
/// changes per row, the backdrop is black, math adds them. No voting over
/// the picture, so a row nothing was drawn on still has an exact colour.
/// The sub screen is taken as transparent (the fixed colour): a layer on the
/// sub screen is a CELL, not the backdrop (see [`margin_color`]).
#[must_use]
pub fn backdrop_color(regs: &LineRegisters) -> [u8; 3] {
    shade(regs, regs.backdrop_color, 5, None)
}

/// Ticket W33-02: per picture row, the colour the backdrop shows there:
/// [`backdrop_color`] of the row's registers when it has them (SNES);
/// otherwise the palette index the picture's own `Backdrop` pixels carry
/// (the NES emits the already-resolved palette-RAM value for every dot,
/// `rf_nes::ppu` module doc, so its backdrop is exact too), turned into a
/// colour by `resolve(row, index)` (the renderer's palette for that row, or
/// the fixed NES table; `rf-enhance` knows neither). `None` when no row has
/// any backdrop pixel and none has registers.
///
/// A row with fewer than `width / 8` backdrop pixels (a platform or a wall
/// covers it) is too thin to say anything and takes the nearest row above
/// with evidence, else the nearest below: the universal backdrop colour
/// (nesdev.org "PPU palettes", `$3F00`) changes on a split, not per row, and a
/// vote over a handful of dots streaked Mega Man 2's margins.
#[must_use]
pub fn backdrop_rows(
    bundle: &FrameBundle,
    resolve: &dyn Fn(usize, u8) -> [u8; 3],
) -> Vec<Option<[u8; 3]>> {
    let width = usize::from(bundle.width);
    if width == 0 {
        return Vec::new();
    }
    let enough = u32::try_from((width / 8).max(1)).unwrap_or(u32::MAX);
    // Per row: the registers' exact colour, or the winning backdrop index
    // when it has enough pixels to be believed.
    let votes: Vec<Result<[u8; 3], Option<u8>>> = bundle
        .video
        .chunks_exact(width)
        .enumerate()
        .map(|(y, row)| {
            if let Some(regs) = bundle.lines.get(y).and_then(Option::as_ref) {
                return Ok(backdrop_color(regs));
            }
            let mut counts: HashMap<u8, u32> = HashMap::new();
            for p in row.iter().filter(|p| p.layer == PixelLayer::Backdrop) {
                *counts.entry(p.palette_index).or_default() += 1;
            }
            // Ties break on the index, so the pick is deterministic.
            Err(counts
                .into_iter()
                .max_by_key(|&(i, n)| (n, i))
                .filter(|&(_, n)| n >= enough)
                .map(|(i, _)| i))
        })
        .collect();
    let rgb = |y: usize, v: &Result<[u8; 3], Option<u8>>| match v {
        Ok(c) => Some(*c),
        Err(idx) => idx.map(|i| resolve(y, i)),
    };
    let mut out: Vec<Option<[u8; 3]>> = votes.iter().enumerate().map(|(y, v)| rgb(y, v)).collect();
    // Thin rows: carry the previous row's colour down, then the next row's
    // up for any still unset at the top. Each pass is one `for` over the
    // rows (law 8).
    let mut carry = None;
    for slot in &mut out {
        match slot {
            Some(c) => carry = Some(*c),
            None => *slot = carry,
        }
    }
    let mut carry = None;
    for slot in out.iter_mut().rev() {
        match slot {
            Some(c) => carry = Some(*c),
            None => *slot = carry,
        }
    }
    out
}

/// The palette index a margin cell shows: the front-most known layer's, else
/// 0 (the row's backdrop colour once resolved with the row palette).
#[must_use]
pub fn margin_palette_index(
    layers: &LayerCanvases,
    origins: &[Option<(i64, i64)>; LAYERS],
    dx: i64,
    dy: i64,
) -> u8 {
    front_layer_pixel(layers, origins, dx, dy, None).map_or(0, |p| p.palette_index)
}

/// Everything the compose needs to know about one picture row.
#[derive(Debug, Clone, Copy, Default)]
pub struct RowFacts<'a> {
    /// The row's backdrop colour ([`backdrop_rows`]).
    pub backdrop: Option<[u8; 3]>,
    /// The row's front-to-back layer order (`None` = BG1 first).
    pub order: Option<&'a [u8; LAYERS]>,
    /// The registers the row was drawn with (SNES, widescreen on).
    pub regs: Option<&'a LineRegisters>,
    /// The row's written palette words and master brightness (SNES); `None`
    /// on the NES, whose cells are already master-colour values.
    pub palette: Option<(&'a Cgram, u8)>,
}

/// The one margin compose for both consoles: the colour of the margin dot
/// `dx` columns from the picture's left edge on picture row `row`, or `None`
/// when nothing is known and the row has no backdrop either (the caller keeps
/// what the PPU drew). `fixed_rgb` turns a NES master-colour index into RGB
/// (the renderer's fixed table; `rf-enhance` does not know it).
///
/// With the row's registers (SNES) the dot is composed through the row's
/// colour math ([`margin_color`]); without them it is the front-most known
/// cell, resolved with the palette it was seen under, else the row's backdrop.
#[must_use]
pub fn margin_pixel(
    layers: &LayerCanvases,
    frames: &LayerFrames,
    row: usize,
    dx: i64,
    facts: &RowFacts<'_>,
    fixed_rgb: &dyn Fn(u8) -> [u8; 3],
) -> Option<[u8; 3]> {
    if let (Some(regs), Some((words, _))) = (facts.regs, facts.palette) {
        return Some(margin_color(
            layers,
            frames,
            row,
            dx,
            facts.order,
            regs,
            words,
        ));
    }
    let cell = front_layer_pixel_in(
        layers,
        |n| frames.origin_at(n, row),
        dx,
        row as i64,
        facts.order,
        0x0F,
    );
    let Some(cell) = cell else {
        // Nothing known: the row's own backdrop colour, else CGRAM 0.
        return facts
            .backdrop
            .or_else(|| facts.palette.map(|(w, b)| bgr555_to_rgb(w[0], b)));
    };
    // With the palette the cell was seen under, not the row's current one.
    Some(match facts.palette {
        Some((words, brightness)) => bgr555_to_rgb(layers.palettes.word(&cell, words), brightness),
        None => fixed_rgb(cell.palette_index),
    })
}
