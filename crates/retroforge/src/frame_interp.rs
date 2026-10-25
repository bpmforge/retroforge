//! Ticket W36-07: interpolated presentation (D-028).
//!
//! Two halves, on either side of the `FrameMsg` channel, so the accuracy-exact stream is never
//! touched (`docs/design/FRAME_INTERPOLATION.md` section 5):
//!
//! * [`capture`] runs on the core thread, after a real frame, and READS the machine's presentation
//!   captures (the background colour under every dot, the OAM index that won each dot, the scroll of
//!   each line) into [`Layers`]. Nothing is written back.
//! * [`Presenter`] runs on the UI thread. It keeps the last two console frames' layers and shows the
//!   OLDER one when the newer arrives: the one frame of latency D-028 accepts. Between two arrivals,
//!   each repaint composes a frame `alpha` of the way from the older to the newer
//!   ([`rf_enhance::interpolate::compose`]); a pair the composer refuses is simply repeated.

use std::time::Instant;

use rf_enhance::interpolate::{self, Layers, OAM_ENTRIES};
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
) -> Option<Box<Layers>> {
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
    let mut behind = [false; OAM_ENTRIES];
    for (b, entry) in behind.iter_mut().zip(oam.chunks_exact(4)) {
        *b = entry[2] & 0x20 != 0;
    }
    Some(Box::new(Layers {
        width,
        height,
        rgba: Vec::new(), // the Presenter fills it from `FrameMsg::rgba`
        bg: bg.to_vec(),
        ids: ids.to_vec(),
        scroll: scroll.to_vec(),
        backdrop: palette[0] & 0x3F,
        behind,
        anchors: objects.map_or_else(Vec::new, |o| o.0.anchors(&|a| stepper.peek_bus(a))),
        index: stepper.frame_count(),
        palette_digest: palette_digest(palette),
    }))
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
    prev: Option<Layers>,
    cur: Option<Layers>,
    /// When `cur` arrived, and the running estimate of the gap between arrivals.
    arrived: Option<Instant>,
    period_us: u64,
    /// The gap that has just opened has been tried / its outcome.
    tried: Option<bool>,
    /// The last refusal, for the readout.
    pub last_refusal: Option<Refusal>,
    pub tally: Tally,
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
        self.prev.as_ref().map(|l| l.index)
    }

    fn close_gap(&mut self) {
        match self.tried.take() {
            Some(true) => self.tally.interpolated += 1,
            Some(false) => self.tally.repeated += 1,
            None if self.prev.is_some() => self.tally.untried += 1,
            None => {}
        }
    }

    /// A console frame arrived with `layers` (from [`capture`]) and its `rgba`. Returns the picture to
    /// show now: the PREVIOUS console frame's (one frame late), or this one's if there is none yet.
    pub fn arrive(&mut self, layers: Layers, rgba: &[u8], now: Instant) -> Vec<u8> {
        if let Some(t) = self.arrived {
            let dt = u64::try_from(now.duration_since(t).as_micros()).unwrap_or(u64::MAX);
            if PERIOD_RANGE_US.contains(&dt) {
                self.period_us = (self.period_us * 3 + dt) / 4;
            }
        }
        self.arrived = Some(now);
        self.close_gap();
        let mut layers = layers;
        layers.rgba = rgba.to_vec();
        self.cached = None;
        // The first frame has nothing before it: shown as it comes, with no gap to fill yet.
        let Some(shown) = self.cur.replace(layers) else {
            self.prev = None;
            return rgba.to_vec();
        };
        let out = shown.rgba.clone();
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
        if self.cached == Some((prev.index, step)) {
            return None; // that picture (or the repeat of a refusal) is already on screen
        }
        match interpolate::compose(prev, cur, alpha.max(1), &resolve, &mut self.scratch) {
            Ok(_) => {
                self.tried = Some(true);
                self.cached = Some((prev.index, step));
                Some((
                    self.scratch.as_slice(),
                    Shown::Intermediate {
                        from: prev.index,
                        alpha,
                    },
                ))
            }
            Err(r) => {
                // One success makes the gap count as interpolated; a refusal only if none came before.
                if self.tried.is_none() {
                    self.tried = Some(false);
                }
                self.last_refusal = Some(r);
                self.cached = Some((prev.index, step));
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
    fn layers(index: u64, sx: u16) -> (Layers, Vec<u8>) {
        let mut bg = vec![0u8; WIDTH * HEIGHT];
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                bg[y * WIDTH + x] = ((usize::from(sx) + x) % 60) as u8;
            }
        }
        let mut rgba = vec![0u8; WIDTH * HEIGHT * 4];
        for (i, b) in bg.iter().enumerate() {
            rgba[i * 4..i * 4 + 3].copy_from_slice(&resolve(*b));
            rgba[i * 4 + 3] = 255;
        }
        let l = Layers {
            width: WIDTH,
            height: HEIGHT,
            rgba: Vec::new(),
            bg,
            ids: vec![interpolate::NO_SPRITE; WIDTH * HEIGHT],
            scroll: vec![(sx, 0); HEIGHT],
            backdrop: 0,
            behind: [false; OAM_ENTRIES],
            anchors: Vec::new(),
            index,
            palette_digest: 1,
        };
        (l, rgba)
    }

    /// The first frame is shown as it comes; from then on each arrival shows the PREVIOUS console
    /// frame (one frame of latency), and a gap with a repaint in it composes a different picture.
    #[test]
    fn it_holds_back_one_frame_and_fills_the_gap() {
        let mut p = Presenter::new();
        let t0 = Instant::now();
        let (l1, f1) = layers(1, 10);
        let (l2, f2) = layers(2, 13);
        let (l3, f3) = layers(3, 16);
        assert_eq!(p.arrive(l1, &f1, t0), f1, "nothing earlier to show");
        assert_eq!(
            p.arrive(l2, &f2, t0 + Duration::from_millis(17)),
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
        assert_eq!(p.arrive(l3, &f3, t0 + Duration::from_millis(34)), f2);
        assert_eq!(p.tally.interpolated, 1);
        // A gap with no repaint in it is counted as untried, never as interpolated.
        let (l4, f4) = layers(4, 19);
        p.arrive(l4, &f4, t0 + Duration::from_millis(51));
        assert_eq!(p.tally.untried, 1);
    }

    /// A pair the composer refuses is repeated, and counted so.
    #[test]
    fn a_refused_pair_is_repeated() {
        let mut p = Presenter::new();
        let t0 = Instant::now();
        let (l1, f1) = layers(1, 10);
        let (l2, f2) = layers(2, 200); // a jump
        p.arrive(l1, &f1, t0);
        p.arrive(l2, &f2, t0 + Duration::from_millis(17));
        assert!(p.intermediate(t0 + Duration::from_millis(25)).is_none());
        assert_eq!(p.last_refusal, Some(Refusal::LargeMotion));
        let (l3, f3) = layers(3, 203);
        p.arrive(l3, &f3, t0 + Duration::from_millis(34));
        assert_eq!(p.tally.repeated, 1);
    }

    /// `reset` forgets everything: the next frame is shown as it comes.
    #[test]
    fn reset_shows_the_next_frame_as_it_comes() {
        let mut p = Presenter::new();
        let (l1, f1) = layers(1, 10);
        p.arrive(l1, &f1, Instant::now());
        p.reset();
        assert!(!p.is_delaying());
        let (l2, f2) = layers(2, 13);
        assert_eq!(p.arrive(l2, &f2, Instant::now()), f2);
    }
}
