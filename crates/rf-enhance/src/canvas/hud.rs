//! What is HUD and what is level, and which layer is in front (ticket W33-03).
//!
//! The HUD classifier. HUD is content that must never be stitched into the
//! world or widened into a margin: a status bar painted into the level would
//! sit at whatever position happened to be under it the first time it was
//! captured. Three tiers exist today, all judging by what the content DOES:
//!
//! 1. **NES, per band.** A band whose scroll did not change against the same
//!    rows of the previous frame is HUD ([`crate::scroll_tracker::ScrollTracker`],
//!    the "zero-delta" rule), and [`super::placement::band_rows`] gives its rows
//!    no position, so layer 0 never holds it.
//! 2. **SNES, per layer.** A layer whose scroll never moved while another
//!    layer's did, and that only draws a band of rows ([`hud_layers`]), keeps
//!    its canvas but gives the margins nothing.
//! 3. **Profile.** `declared_hud` rows (the profile says so) stay native in the
//!    presentation (`rf_enhance::widescreen`, applied where the margins are
//!    filled).
//!
//! The core states per line which layers are on the main screen
//! (`LineRegisters::tm`), which would let tier 2 start from a fact instead of
//! an observation; that is deliberately not wired here, because it would
//! change which layers the existing Super Mario World shots treat as HUD.
//!
//! The same module holds the front-to-back evidence ([`row_orders`]).
use rf_core_api::{FrameBundle, PixelLayer};

use super::placement::LayerFrames;
use super::LAYERS;
use crate::stitcher::Canvas;

/// Per scene, what a layer's scroll and picture rows have done so far (the
/// evidence for [`hud_layers`]).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Motion {
    seen: bool,
    x: (i64, i64),
    y: (i64, i64),
    /// First and last picture row the layer drew a pixel on.
    rows: (usize, usize),
    height: usize,
}

impl Motion {
    pub(crate) fn observe(
        &mut self,
        wx: i64,
        wy: i64,
        rows: Option<(usize, usize)>,
        height: usize,
    ) {
        if self.seen {
            self.x = (self.x.0.min(wx), self.x.1.max(wx));
            self.y = (self.y.0.min(wy), self.y.1.max(wy));
        } else {
            self.seen = true;
            self.x = (wx, wx);
            self.y = (wy, wy);
            self.rows = (usize::MAX, 0);
        }
        self.height = height;
        if let Some((lo, hi)) = rows {
            self.rows = (self.rows.0.min(lo), self.rows.1.max(hi));
        }
    }

    fn moved(&self) -> bool {
        self.seen && (self.x.1 - self.x.0 >= MOVED_PX || self.y.1 - self.y.0 >= MOVED_PX)
    }

    /// Draws only a band of the picture (less than three quarters of the
    /// rows) and has drawn something at all.
    fn fixed_band(&self) -> bool {
        self.seen
            && self.rows.0 <= self.rows.1
            && (self.rows.1 - self.rows.0 + 1) * 4 < self.height * 3
    }
}

/// A layer's world position must have spanned this many pixels to count as
/// having scrolled (a one pixel wobble is not a camera).
const MOVED_PX: i64 = 8;

/// Ticket W32-08: which layers are HUD: their scroll never moved while
/// another layer's did, and they only draw a band of rows. Judged on
/// everything seen so far, so a layer stitched before the camera first moved
/// is left out of the margins as soon as it is known.
pub(crate) fn hud_layers(motion: &[Motion; LAYERS]) -> [bool; LAYERS] {
    std::array::from_fn(|n| {
        let other_moved = motion.iter().enumerate().any(|(i, o)| i != n && o.moved());
        other_moved && !motion[n].moved() && motion[n].fixed_band()
    })
}

/// Ticket W32-08: for each picture row of the frame just observed, the layers
/// front to back, by the picture's own evidence. Where the picture shows layer
/// A at a spot layer B also has a known cell for, A is in front of B; layers
/// the picture says nothing about stay in layer order (BG1 first).
pub(crate) fn row_orders(
    bundle: &FrameBundle,
    frames: &LayerFrames,
    set: &[Canvas; LAYERS],
    hud: [bool; LAYERS],
) -> Vec<[u8; LAYERS]> {
    let width = usize::from(bundle.width);
    let pad = i64::from(bundle.width.saturating_sub(256) / 2);
    let known = |n: usize, c: usize, y: usize| -> bool {
        frames
            .origin_at(n, y)
            .is_some_and(|(ox, oy)| set[n].get(ox - pad + c as i64, oy + y as i64).is_some())
    };
    let default: [u8; LAYERS] = std::array::from_fn(|i| i as u8);
    if width == 0 {
        return Vec::new();
    }
    // Bounded `for` over rows and columns (law 8).
    bundle
        .video
        .chunks_exact(width)
        .enumerate()
        .map(|(y, row)| {
            let mut wins = [[0i32; LAYERS]; LAYERS];
            for (c, p) in row.iter().enumerate() {
                let PixelLayer::Background(a) = p.layer else {
                    continue;
                };
                let a = usize::from(a);
                if a >= LAYERS || hud[a] {
                    continue;
                }
                for b in (0..LAYERS).filter(|&b| b != a && !hud[b]) {
                    if known(b, c, y) {
                        wins[a][b] += 1;
                    }
                }
            }
            // Insertion sort by pairwise evidence: a layer moves in front
            // of the one before it only when the picture showed it
            // winning against that one. `j` drops on every iteration, so
            // the inner walk ends (law 8).
            let mut order = default;
            for i in 1..LAYERS {
                let mut j = i;
                while j > 0 {
                    let (front, back) = (usize::from(order[j - 1]), usize::from(order[j]));
                    if wins[back][front] <= wins[front][back] {
                        break;
                    }
                    order.swap(j - 1, j);
                    j -= 1;
                }
            }
            order
        })
        .collect()
}
