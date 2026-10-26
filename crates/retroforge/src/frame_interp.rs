//! Ticket W36-07: interpolated presentation (D-028).
//!
//! Two halves, on either side of the `FrameMsg` channel, so the accuracy-exact stream is never
//! touched (`docs/design/FRAME_INTERPOLATION.md` section 5):
//!
//! * [`capture`] (NES) and [`capture_snes`] (SNES) run on the core thread, after a real frame, and READ
//!   the machine's presentation captures (NES: the background colour under every dot, the OAM index that
//!   won each dot, the scroll of each line; SNES: the layer and priority of every dot, the OAM index, the
//!   scroll of every background on every line) into [`Captured`]. Nothing is written back.
//! * [`Presenter`] runs on the UI thread. It keeps the last two console frames' layers and shows the
//!   OLDER one when the newer arrives: the one frame of latency D-028 accepts. Between two arrivals,
//!   each repaint composes a frame `alpha` of the way from the older to the newer
//!   ([`rf_enhance::interpolate::compose`], [`rf_enhance::interpolate_snes::compose_snes`]); a pair the
//!   composer refuses is simply repeated.

use std::time::Instant;

use rf_enhance::interpolate::{self, Layers, OAM_ENTRIES};
use rf_enhance::interpolate_snes::{self, SnesLayers, SnesLine};
use rf_enhance::interpolation::Refusal;

use crate::margin_draw::Map;
use crate::stepper::EmuStepper;

/// Picture size the NES capture covers.
const WIDTH: usize = 256;
const HEIGHT: usize = 240;

/// FNV-1a over the 32 palette bytes: any change in the palette changes it.
fn palette_digest(palette: &[u8; 32]) -> u64 {
    palette.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The recipe's object table, as places for sprites to cluster around.
#[derive(Debug, Clone)]
pub struct Objects(Map);

impl Objects {
    /// `None` when the recipe describes no object table.
    #[must_use]
    pub fn from_recipe(recipe: &rf_rebuild::Recipe) -> Option<Self> {
        Map::from_recipe(recipe).map(Self)
    }
}

/// Read the layers of the frame the real machine just ran. `ids` is the sink's per-dot OAM index,
/// `oam` the OAM that drew this picture, `objects` the recipe's object table when there is one. `None`
/// when the captures are not all there (so the Presenter passes the frame through).
#[must_use]
pub fn capture(
    stepper: &EmuStepper,
    ids: Option<&[u8]>,
    width: usize,
    height: usize,
    oam: &[u8; 256],
    objects: Option<&Objects>,
) -> Option<Box<Captured>> {
    let (bg, scroll) = (stepper.bg_under(), stepper.line_scroll());
    let ids = ids?;
    if (width, height) != (WIDTH, HEIGHT)
        || bg.len() != WIDTH * HEIGHT
        || ids.len() != WIDTH * HEIGHT
        || scroll.len() != HEIGHT
    {
        return None;
    }
    let palette = stepper.palette();
    // The background colour under every dot, as RGB through the table the console frame was drawn with:
    // the layers carry colours, not indices, so a pair whose palette cycles still draws each dot in the
    // palette of the frame it came from, and a widescreen margin (no index) fits the same type.
    let table: [u32; 64] = std::array::from_fn(|i| interpolate::rgb_key(resolve(i as u8)));
    let mut behind = [false; OAM_ENTRIES];
    for (b, entry) in behind.iter_mut().zip(oam.chunks_exact(4)) {
        *b = entry[2] & 0x20 != 0;
    }
    Some(Box::new(Captured::Nes(Layers {
        width,
        height,
        rgba: Vec::new(), // the Presenter fills it from `FrameMsg::rgba`
        bg: bg.iter().map(|&i| table[usize::from(i & 0x3F)]).collect(),
        ids: ids.to_vec(),
        scroll: scroll.to_vec(),
        backdrop: table[usize::from(palette[0] & 0x3F)],
        behind,
        anchors: objects.map_or_else(Vec::new, |o| o.0.anchors(&|a| stepper.peek_bus(a))),
        index: stepper.frame_count(),
        palette_digest: palette_digest(palette),
        palette: palette
            .iter()
            .map(|&i| table[usize::from(i & 0x3F)])
            .collect(),
    })))
}

/// The layers of one console frame, whichever console it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Captured {
    Nes(Layers),
    Snes(SnesLayers),
}

impl Captured {
    /// Completed-frame count.
    #[must_use]
    pub fn index(&self) -> u64 {
        match self {
            Captured::Nes(l) => l.index,
            Captured::Snes(l) => l.index,
        }
    }

    /// `(width, height)` of the picture the layers describe.
    #[must_use]
    pub fn dims(&self) -> (usize, usize) {
        match self {
            Captured::Nes(l) => (l.width, l.height),
            Captured::Snes(l) => (l.width, l.height),
        }
    }

    /// The picture as the console showed it (or as widescreen shows it, after [`Self::widened`]).
    #[must_use]
    pub fn rgba(&self) -> &[u8] {
        match self {
            Captured::Nes(l) => &l.rgba,
            Captured::Snes(l) => &l.rgba,
        }
    }

    /// Fill in the pixels the capture left empty (the Presenter takes them from `FrameMsg::rgba`).
    pub fn set_rgba(&mut self, rgba: &[u8]) {
        match self {
            Captured::Nes(l) => l.rgba = rgba.to_vec(),
            // An SNES shows fewer rows than the buffer has: the rows the layers describe.
            Captured::Snes(l) => {
                l.rgba = rgba[..(l.width * l.height * 4).min(rgba.len())].to_vec();
            }
        }
    }

    /// The same frame as a wider picture (`rgba`, `width` by `rows`): the console's dots in the middle,
    /// the margins widescreen composed on either side. `None` when it does not fit.
    #[must_use]
    pub fn widened(&self, rgba: &[u8], width: usize, rows: usize) -> Option<Self> {
        match self {
            Captured::Nes(l) => interpolate::widen(l, rgba, width, rows).map(Captured::Nes),
            Captured::Snes(l) => l.widened(rgba, width, rows).map(Captured::Snes),
        }
    }

    /// Whether the layers describe their own picture (checked once per console frame).
    fn describes_its_picture(&self) -> bool {
        match self {
            Captured::Nes(l) => interpolate::describes_its_picture(l),
            Captured::Snes(l) => l.is_well_formed(),
        }
    }
}

/// Compose between two frames of the same console into `out`.
fn compose(prev: &Captured, cur: &Captured, alpha: u8, out: &mut Vec<u8>) -> Result<(), Refusal> {
    match (prev, cur) {
        (Captured::Nes(a), Captured::Nes(b)) => {
            interpolate::compose_verified(a, b, alpha, out).map(|_| ())
        }
        (Captured::Snes(a), Captured::Snes(b)) => {
            interpolate_snes::compose_snes(a, b, alpha, out).map(|_| ())
        }
        _ => Err(Refusal::Malformed),
    }
}

/// How far back each (layer, priority) is in a mode's order, for every `bg_mode | bg3_priority << 3`
/// (`rf_snes::ppu::priority_slots`).
fn snes_ranks() -> Vec<[[u8; 4]; 5]> {
    (0..16u8)
        .map(|key| {
            let mut table = [[interpolate_snes::RANK_ABSENT; 4]; 5];
            for (rank, (layer, pri)) in rf_snes::ppu::priority_slots(key & 7, key & 8 != 0)
                .into_iter()
                .enumerate()
            {
                if let Some(slot) = table
                    .get_mut(usize::from(layer))
                    .and_then(|l| l.get_mut(usize::from(pri)))
                {
                    *slot = u8::try_from(rank).unwrap_or(u8::MAX);
                }
            }
            table
        })
        .collect()
}

/// Read the layers of the SNES frame the real machine just ran (ticket W37-06): which layer and priority
/// drew each dot (`codes`, the frame buffer's layer capture), which sprite (`ids`), and each row's
/// registers (`regs`, the frame buffer's line-register capture) with how the PPU latched the row
/// ([`EmuStepper::snes_line_facts`]). `None` when any of it is missing (the Presenter then passes the
/// frame through). Nothing is written back.
#[must_use]
pub fn capture_snes(
    stepper: &EmuStepper,
    codes: Option<&[u8]>,
    ids: Option<&[u8]>,
    regs: Option<&[Option<rf_core_api::LineRegisters>]>,
    width: usize,
    height: usize,
) -> Option<Box<Captured>> {
    let (codes, ids, regs) = (codes?, ids?, regs?);
    if width < 256 || height == 0 || codes.len() != width * height || ids.len() != width * height {
        return None;
    }
    // The rows the core drew this frame (it sends registers for each): an SNES shows 224 or 239 of the
    // buffer's rows, and the rest are not part of the picture.
    let rows = regs.iter().take(height).take_while(|r| r.is_some()).count();
    if rows == 0 {
        return None;
    }
    let (height, codes, ids) = (rows, &codes[..rows * width], &ids[..rows * width]);
    let mut lines = Vec::with_capacity(height);
    for y in 0..height {
        let r = regs.get(y).copied().flatten()?;
        let facts = stepper.snes_line_facts(u16::try_from(y).ok()?)?;
        lines.push(SnesLine {
            hofs: r.hofs.map(|v| v & 0x3FF),
            vofs: r.vofs.map(|v| v & 0x3FF),
            tm: r.tm,
            brightness: r.brightness,
            backdrop: r.backdrop_color,
            mode_key: (facts.bg_mode & 7) | (u8::from(facts.bg3_priority) << 3),
            hazards: facts.hazards,
        });
    }
    Some(Box::new(Captured::Snes(SnesLayers {
        width,
        height,
        rgba: Vec::new(), // the Presenter fills it from `FrameMsg::rgba`
        codes: codes.to_vec(),
        ids: ids.to_vec(),
        lines,
        ranks: snes_ranks(),
        index: stepper.frame_count(),
    })))
}

/// Colour index to RGB, the table the console frame was drawn with.
fn resolve(i: u8) -> [u8; 3] {
    rf_renderer::resolve_index(i, None)
}

/// What became of each console frame's gap, for the readout and the test.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// Gaps between console frames in which an intermediate frame was composed.
    pub interpolated: u64,
    /// Gaps whose intermediate was refused, so the console frame was repeated.
    pub repeated: u64,
    /// Gaps no repaint fell into (a display no faster than the game).
    pub untried: u64,
}

/// What one presented picture was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// The console's own frame, number `n`.
    Console(u64),
    /// Composed between console frames `from` and `from + 1`, `alpha` of 256 of the way.
    Intermediate { from: u64, alpha: u8 },
}

/// The UI-thread half.
#[derive(Debug, Default)]
pub struct Presenter {
    /// The frame on screen (older) and the one that arrived last (newer), with their pixels.
    prev: Option<Captured>,
    cur: Option<Captured>,
    /// Whether each describes its own picture ([`Captured::describes_its_picture`]), checked once per
    /// console frame when it arrives.
    prev_ok: bool,
    cur_ok: bool,
    /// When `cur` arrived, and the running estimate of the gap between arrivals.
    arrived: Option<Instant>,
    period_us: u64,
    /// The gap that has just opened has been tried / its outcome.
    tried: Option<bool>,
    /// The last refusal, for the readout.
    pub last_refusal: Option<Refusal>,
    /// How many composed pictures each refusal turned down (a count per picture tried, for the
    /// readout and the measurements, not per gap).
    pub refusals: std::collections::BTreeMap<String, u64>,
    pub tally: Tally,
    /// Time spent composing (microseconds) and how many pictures that was, for the cost readout.
    pub compose_cost: (u64, u64),
    scratch: Vec<u8>,
    /// Test hook: use this alpha instead of the clock.
    pub alpha_override: Option<u8>,
    /// The last composed picture: `(from, alpha)` it was made for.
    cached: Option<(u64, u8)>,
}

/// The gap between console frames when nothing has been measured yet (NTSC, 60.0988 Hz).
const DEFAULT_PERIOD_US: u64 = 16_639;
/// A measured gap outside this range is a pause or a fast-forward, not the game's rate.
const PERIOD_RANGE_US: std::ops::RangeInclusive<u64> = 4_000..=50_000;

impl Presenter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            period_us: DEFAULT_PERIOD_US,
            ..Self::default()
        }
    }

    /// Forget both frames (interpolation went off, or the picture stopped being the plain one): the
    /// next frame is shown as it comes.
    pub fn reset(&mut self) {
        self.close_gap();
        self.prev = None;
        self.cur = None;
        self.arrived = None;
        self.cached = None;
    }

    /// Whether a frame is being held back (so the shown picture lags the machine).
    #[must_use]
    pub fn is_delaying(&self) -> bool {
        self.prev.is_some()
    }

    /// The console frame number on screen.
    #[must_use]
    pub fn shown_frame(&self) -> Option<u64> {
        self.prev.as_ref().map(Captured::index)
    }

    fn close_gap(&mut self) {
        match self.tried.take() {
            Some(true) => self.tally.interpolated += 1,
            Some(false) => self.tally.repeated += 1,
            None if self.prev.is_some() => self.tally.untried += 1,
            None => {}
        }
    }

    /// A console frame arrived with `layers` (from [`capture`], pixels filled in, widened to the shown
    /// picture when widescreen is on). Returns the picture to show now: the PREVIOUS console frame's (one
    /// frame late), or this one's if there is none yet.
    pub fn arrive(&mut self, layers: Captured, now: Instant) -> Vec<u8> {
        // A picture of another size or another console (widescreen toggled, a new core) is not the next
        // frame of the last.
        if self.cur.as_ref().is_some_and(|c| {
            c.dims() != layers.dims()
                || std::mem::discriminant(c) != std::mem::discriminant(&layers)
        }) {
            self.reset();
        }
        if let Some(t) = self.arrived {
            let dt = u64::try_from(now.duration_since(t).as_micros()).unwrap_or(u64::MAX);
            if PERIOD_RANGE_US.contains(&dt) {
                self.period_us = (self.period_us * 3 + dt) / 4;
            }
        }
        self.arrived = Some(now);
        self.close_gap();
        self.cached = None;
        // The first frame has nothing before it: shown as it comes, with no gap to fill yet.
        self.prev_ok = self.cur_ok;
        self.cur_ok = layers.describes_its_picture();
        let first = layers.rgba().to_vec();
        let Some(shown) = self.cur.replace(layers) else {
            self.prev = None;
            return first;
        };
        let out = shown.rgba().to_vec();
        self.prev = Some(shown);
        out
    }

    /// The alpha for a repaint at `now` (0 right at an arrival).
    fn alpha_at(&self, now: Instant) -> u8 {
        if let Some(a) = self.alpha_override {
            return a;
        }
        let Some(t) = self.arrived else { return 0 };
        let el = u64::try_from(now.duration_since(t).as_micros()).unwrap_or(u64::MAX);
        let period = self.period_us.max(1);
        u8::try_from((el.saturating_mul(256) / period).min(255)).unwrap_or(255)
    }

    /// The picture for a repaint that brought no new console frame, or `None` to leave the one on
    /// screen (alpha 0, nothing held, the pair already repeated, or a refusal). `shown` says which it
    /// was.
    pub fn intermediate(&mut self, now: Instant) -> Option<(&[u8], Shown)> {
        let (prev, cur) = (self.prev.as_ref()?, self.cur.as_ref()?);
        let alpha = self.alpha_at(now);
        if alpha == 0 {
            return None;
        }
        // Compose again only when the shift could have changed (about 16 steps across a gap).
        let step = alpha & 0xF0;
        let from = prev.index();
        if self.cached == Some((from, step)) {
            return None; // that picture (or the repeat of a refusal) is already on screen
        }
        let started = Instant::now();
        let composed = if self.prev_ok && self.cur_ok {
            let r = compose(prev, cur, alpha.max(1), &mut self.scratch);
            if r.is_ok() {
                let us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
                self.compose_cost = (
                    self.compose_cost.0.saturating_add(us),
                    self.compose_cost.1 + 1,
                );
            }
            r
        } else {
            Err(Refusal::Malformed)
        };
        match composed {
            Ok(_) => {
                self.tried = Some(true);
                self.cached = Some((from, step));
                Some((self.scratch.as_slice(), Shown::Intermediate { from, alpha }))
            }
            Err(r) => {
                // One success makes the gap count as interpolated; a refusal only if none came before.
                if self.tried.is_none() {
                    self.tried = Some(false);
                }
                self.last_refusal = Some(r);
                *self.refusals.entry(format!("{r:?}")).or_default() += 1;
                self.cached = Some((from, step));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A flat 256x240 frame at scroll `sx`, whose background is a function of the world column so a
    /// shift is a real scroll; `shade` tints the console frame so frames are told apart.
    fn layers(index: u64, sx: u16) -> (Captured, Vec<u8>) {
        let mut bg = vec![0u32; WIDTH * HEIGHT];
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                bg[y * WIDTH + x] =
                    interpolate::rgb_key(resolve(((usize::from(sx) + x) % 60) as u8));
            }
        }
        let mut rgba = vec![0u8; WIDTH * HEIGHT * 4];
        for (i, b) in bg.iter().enumerate() {
            rgba[i * 4..i * 4 + 3].copy_from_slice(&interpolate::rgb_of(*b));
            rgba[i * 4 + 3] = 255;
        }
        let l = Layers {
            width: WIDTH,
            height: HEIGHT,
            rgba: rgba.clone(),
            bg,
            ids: vec![interpolate::NO_SPRITE; WIDTH * HEIGHT],
            scroll: vec![(sx, 0); HEIGHT],
            backdrop: 0,
            behind: [false; OAM_ENTRIES],
            anchors: Vec::new(),
            index,
            palette_digest: 1,
            palette: Vec::new(),
        };
        (Captured::Nes(l), rgba)
    }

    /// The composer mirrors the core's hazard bits (it cannot depend on the core): they must agree, and the
    /// rank table built from the core's order puts sprites at level 3 in front in mode 1.
    #[test]
    fn the_snes_composer_agrees_with_the_core_about_hazards_and_priority_order() {
        use rf_enhance::interpolate_snes::hazard as h;
        use rf_snes::ppu::line_hazard as c;
        assert_eq!(
            [h::MODE7, h::HIRES, h::MOSAIC, h::WINDOW],
            [c::MODE7, c::HIRES, c::MOSAIC, c::WINDOW]
        );
        assert_eq!(
            [h::INTERLACE, h::MIDLINE, h::FORCED_BLANK, h::DIRECT_COLOR],
            [c::INTERLACE, c::MIDLINE, c::FORCED_BLANK, c::DIRECT_COLOR]
        );
        assert_eq!(interpolate_snes::CODE_SUB, rf_renderer::frame::LAYER_SUB);
        assert_eq!(
            interpolate_snes::CODE_SPRITE,
            rf_renderer::frame::LAYER_SPRITE
        );
        assert_eq!(
            interpolate_snes::CODE_BACKDROP,
            rf_renderer::frame::LAYER_BACKDROP
        );
        let ranks = snes_ranks();
        assert_eq!(ranks.len(), 16);
        let mode1 = &ranks[1];
        assert_eq!(mode1[4][3], 0, "OBJ level 3 is frontmost");
        assert!(
            mode1[0][1] < mode1[0][0],
            "BG1's high tiles are in front of its low ones"
        );
        assert_eq!(
            mode1[3][0],
            interpolate_snes::RANK_ABSENT,
            "mode 1 has no BG4"
        );
        assert_eq!(
            ranks[1 | 8][2][1],
            0,
            "BG3's high tiles lead with $2105 bit 3"
        );
    }

    /// The first frame is shown as it comes; from then on each arrival shows the PREVIOUS console
    /// frame (one frame of latency), and a gap with a repaint in it composes a different picture.
    #[test]
    fn it_holds_back_one_frame_and_fills_the_gap() {
        let mut p = Presenter::new();
        let t0 = Instant::now();
        let (l1, f1) = layers(1, 10);
        let (l2, f2) = layers(2, 13);
        let (l3, _f3) = layers(3, 16);
        assert_eq!(p.arrive(l1, t0), f1, "nothing earlier to show");
        assert_eq!(
            p.arrive(l2, t0 + Duration::from_millis(17)),
            f1,
            "one frame late"
        );
        assert_eq!(p.shown_frame(), Some(1));
        // A repaint 8 ms into the gap: an intermediate that is neither frame.
        let mid = p
            .intermediate(t0 + Duration::from_millis(17 + 8))
            .map(|(px, s)| (px.to_vec(), s));
        let (px, shown) = mid.expect("composed");
        assert!(matches!(shown, Shown::Intermediate { from: 1, .. }));
        assert_ne!(px, f1);
        assert_ne!(px, f2);
        // Nothing new to show until the alpha moves on.
        assert!(p.intermediate(t0 + Duration::from_millis(17 + 8)).is_none());
        assert_eq!(p.arrive(l3, t0 + Duration::from_millis(34)), f2);
        assert_eq!(p.tally.interpolated, 1);
        // A gap with no repaint in it is counted as untried, never as interpolated.
        let (l4, _f4) = layers(4, 19);
        p.arrive(l4, t0 + Duration::from_millis(51));
        assert_eq!(p.tally.untried, 1);
    }

    /// A pair the composer refuses is repeated, and counted so.
    #[test]
    fn a_refused_pair_is_repeated() {
        let mut p = Presenter::new();
        let t0 = Instant::now();
        let (l1, _f1) = layers(1, 10);
        let (l2, _f2) = layers(2, 200); // a jump
        p.arrive(l1, t0);
        p.arrive(l2, t0 + Duration::from_millis(17));
        assert!(p.intermediate(t0 + Duration::from_millis(25)).is_none());
        assert_eq!(p.last_refusal, Some(Refusal::LargeMotion));
        let (l3, _f3) = layers(3, 203);
        p.arrive(l3, t0 + Duration::from_millis(34));
        assert_eq!(p.tally.repeated, 1);
    }

    /// `reset` forgets everything: the next frame is shown as it comes.
    #[test]
    fn reset_shows_the_next_frame_as_it_comes() {
        let mut p = Presenter::new();
        let (l1, _f1) = layers(1, 10);
        p.arrive(l1, Instant::now());
        p.reset();
        assert!(!p.is_delaying());
        let (l2, f2) = layers(2, 13);
        assert_eq!(p.arrive(l2, Instant::now()), f2);
    }
}
