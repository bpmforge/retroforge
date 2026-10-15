//! **Re-find a game's RAM rows by observation only** (W30, decision D-021).
//!
//! The profile rows that came from game disassemblies were stripped
//! (D-021); this harness gets them back the clean-room way. It plays a
//! real dump headlessly with scripted input and, WITHOUT reading anything
//! about the game, finds work-RAM bytes by what they DO:
//!
//! - **tick counter** — increments by exactly 1 on most frames of play and
//!   stands still while the game is paused (feeds `[overclock].tick_counter`);
//! - **player x / y** — a value that rises with held Right and falls with
//!   held Left and is still while idle (x); returns to its start after a
//!   jump (platformer y) or follows Up/Down holds (top-down y);
//! - **timer** — a value stepping down by one at a steady interval while
//!   the player idles;
//! - **lives** — a small value that steps down by exactly 1, rarely, within
//!   seconds of the screen being reset (a death).
//!
//! Every address in a profile row comes from the evidence this prints; the
//! row's `notes` carries that evidence. Nothing is looked up anywhere.
//!
//! Detectors are pure functions of per-address counters accumulated while
//! the script runs (nothing is kept per frame beyond a few tiny traces), so
//! a SNES run over all 128 KiB of work RAM stays bounded in memory.
//!
//! ```text
//! RF_REFIND_ROMS=a.zip:b.zip cargo test --release -p rf-harness \
//!   --test profile_refind -- --nocapture
//! ```
//!
//! Unset `RF_REFIND_ROMS` skips cleanly. Optional: `RF_REFIND_WRITE=1`
//! writes high-confidence rows into the matching profile;
//! `RF_REFIND_SHOTS=dir` drops a PNG every `RF_REFIND_SHOT_EVERY` frames
//! (default 120); `RF_REFIND_OPEN=<recipe>` overrides the opening and
//! `RF_REFIND_STOP=<frames>` stops the run early (opening exploration).

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, EventMask, PpuPixel, Step};

const ROMS_VAR: &str = "RF_REFIND_ROMS";
const SOURCE: &str = "observed: RetroForge re-find pass (W30, 2026-10-15)";

// ---- buttons (logical, the NES's own bit order plus the SNES extras) ----
const BTN_A: u16 = 1;
const BTN_B: u16 = 1 << 1;
const BTN_SELECT: u16 = 1 << 2;
const BTN_START: u16 = 1 << 3;
const BTN_UP: u16 = 1 << 4;
const BTN_DOWN: u16 = 1 << 5;
const BTN_LEFT: u16 = 1 << 6;
const BTN_RIGHT: u16 = 1 << 7;
const BTN_X: u16 = 1 << 8;
const BTN_Y: u16 = 1 << 9;
const BTN_L: u16 = 1 << 10;
const BTN_R: u16 = 1 << 11;

fn buttons(keys: &str) -> u16 {
    keys.chars()
        .map(|k| match k {
            'A' => BTN_A,
            'B' => BTN_B,
            's' => BTN_SELECT,
            'S' => BTN_START,
            'U' => BTN_UP,
            'D' => BTN_DOWN,
            'L' => BTN_LEFT,
            'R' => BTN_RIGHT,
            'X' => BTN_X,
            'Y' => BTN_Y,
            'l' => BTN_L,
            'r' => BTN_R,
            _ => 0,
        })
        .fold(0, |a, b| a | b)
}

/// Logical buttons -> the `$4218` word (fullsnes "Joypad").
fn snes_pad(pad: u16) -> u16 {
    [
        (BTN_B, 15),
        (BTN_Y, 14),
        (BTN_SELECT, 13),
        (BTN_START, 12),
        (BTN_UP, 11),
        (BTN_DOWN, 10),
        (BTN_LEFT, 9),
        (BTN_RIGHT, 8),
        (BTN_A, 7),
        (BTN_X, 6),
        (BTN_L, 5),
        (BTN_R, 4),
    ]
    .iter()
    .filter(|(b, _)| pad & b != 0)
    .fold(0, |w, (_, bit)| w | 1 << bit)
}

/// `frames:buttons,...` -> a per-frame pad list (`-` or empty = none).
fn parse_recipe(text: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for step in text.split(',') {
        let Some((n, keys)) = step.trim().split_once(':') else {
            continue;
        };
        let Ok(n) = n.trim().parse::<usize>() else {
            continue;
        };
        out.extend(std::iter::repeat_n(buttons(keys), n));
    }
    out
}

// ---------------------------------------------------------------- script

/// What a stretch of the script is for. The first six are the repeated
/// PROBES the position detectors score; the rest are single long stretches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Idle = 0,
    Right = 1,
    Left = 2,
    Up = 3,
    Down = 4,
    Jump = 5,
    Pause,
    LongRight,
    LongIdle,
    Free,
}
const PROBES: usize = 6;

#[derive(Clone, Copy)]
struct Seg {
    kind: Kind,
    frames: u32,
    /// Buttons for the first `hold` frames, none for the rest.
    pad: u16,
    hold: u32,
    /// Frames at the start that are not scored (a skid to a stop).
    settle: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    Platformer,
    TopDown,
}

struct Game {
    /// Profile directory under `profiles/`.
    slug: &'static str,
    /// Frames of opening input (`frames:buttons,...`).
    opening: &'static str,
    style: Style,
    /// The jump button.
    jump: u16,
    /// Button held while "running" in the free-play stretch.
    run: u16,
    /// The pause button's keys.
    pause: u16,
    /// Reps of the probe round.
    reps: u32,
    /// Input that puts the player back into play after a death
    /// (`frames:buttons,...`); harmless when already playing.
    recover: &'static str,
    /// Frames of the long idle stretch (timers, deaths).
    long_idle: u32,
    /// Free-play (hold right + hop) frames.
    free_play: u32,
}

fn games() -> Vec<Game> {
    vec![
        Game {
            slug: "snes/super-mario-world",
            opening: "4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,400:-,300:R,40:A,300:R,60:B,600:R,4:A,200:-,300:R,200:-,120:R,100:-,4:A,200:-,150:-",
            style: Style::Platformer,
            jump: BTN_B,
            run: BTN_Y,
            pause: BTN_START,
            reps: 10,
            recover: "400:-,4:A,300:-,4:A,300:-",
            long_idle: 4000,
            free_play: 6000,
        },
        Game {
            slug: "snes/the-legend-of-zelda-a-link-to-the-past",
            opening: "4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:A,12:-,4:D,12:-,4:D,12:-,4:D,12:-,4:R,12:-,4:R,12:-,4:R,12:-,4:A,200:-,4:A,200:-,4:A,300:-,300:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,100:D,60:R,200:D,200:-,100:D,200:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,200:D,300:-,15:R,200:D,300:-",
            style: Style::TopDown,
            jump: BTN_B,
            run: BTN_Y,
            pause: BTN_START,
            reps: 10,
            recover: "",
            long_idle: 2400,
            free_play: 4000,
        },
        Game {
            slug: "nes/super-mario-bros-2",
            opening: "4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:A,116:-,4:A,200:-,300:-",
            style: Style::Platformer,
            jump: BTN_A | BTN_B,
            run: BTN_B,
            pause: BTN_START,
            reps: 10,
            recover: "",
            long_idle: 4000,
            free_play: 9000,
        },
        Game {
            slug: "nes/contra",
            opening: "4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,400:-",
            style: Style::Platformer,
            jump: BTN_A,
            run: BTN_B,
            pause: BTN_START,
            reps: 10,
            recover: "",
            long_idle: 2400,
            free_play: 9000,
        },
        Game {
            slug: "nes/super-c",
            opening: "4:S,116:-,4:S,116:-,4:S,116:-,1300:-",
            style: Style::Platformer,
            jump: BTN_A,
            run: BTN_B,
            pause: BTN_START,
            reps: 10,
            recover: "",
            long_idle: 2400,
            free_play: 9000,
        },
        Game {
            slug: "nes/castlevania-ii-simons-quest",
            opening: "4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,400:-",
            style: Style::Platformer,
            jump: BTN_A | BTN_B,
            run: BTN_A,
            pause: BTN_START,
            reps: 10,
            recover: "",
            long_idle: 2400,
            free_play: 9000,
        },
        Game {
            slug: "snes/mega-man-7",
            opening: "4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:S,116:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,4:A,56:-,200:-",
            style: Style::Platformer,
            jump: BTN_B,
            run: BTN_Y,
            pause: BTN_START,
            reps: 10,
            recover: "",
            long_idle: 2400,
            free_play: 4000,
        },
        Game {
            slug: "snes/donkey-kong-country",
            opening: "",
            style: Style::Platformer,
            jump: BTN_A,
            run: BTN_Y,
            pause: BTN_START,
            reps: 10,
            recover: "4:A,300:-",
            long_idle: 2400,
            free_play: 4000,
        },
    ]
}

/// The play script that follows the opening.
fn play_script(g: &Game) -> Vec<Seg> {
    let seg = |kind, frames, pad, hold| Seg {
        kind,
        frames,
        pad,
        hold,
        settle: if kind == Kind::Idle { 40 } else { 0 },
    };
    let recover = |s: &mut Vec<Seg>| {
        for (n, keys) in g
            .recover
            .split(',')
            .filter_map(|st| st.trim().split_once(':'))
        {
            let n: u32 = n.parse().unwrap_or(0);
            let pad = buttons(keys);
            // Not 4 frames: that length marks a pause test's Start press.
            s.push(seg(Kind::Free, if n == 4 { 5 } else { n }, pad, n));
        }
    };
    let mut s = vec![seg(Kind::Idle, 120, 0, 0)];
    // Top-down rooms are small: shorter holds, so a wall is not hit.
    let hold = if g.style == Style::TopDown { 20 } else { 45 };
    for _ in 0..g.reps {
        s.push(seg(Kind::Idle, 70, 0, 0));
        s.push(seg(Kind::Right, hold, BTN_RIGHT, hold));
        s.push(seg(Kind::Idle, 70, 0, 0));
        s.push(seg(Kind::Left, hold, BTN_LEFT, hold));
        s.push(seg(Kind::Idle, 70, 0, 0));
        if g.style == Style::Platformer {
            s.push(seg(Kind::Jump, 110, g.jump, 8));
            s.push(seg(Kind::Idle, 70, 0, 0));
        }
        s.push(seg(Kind::Up, hold, BTN_UP, hold));
        s.push(seg(Kind::Idle, 70, 0, 0));
        s.push(seg(Kind::Down, hold, BTN_DOWN, hold));
    }
    // Pause tests: Start, stand still, Start again.
    for _ in 0..4 {
        s.push(seg(Kind::Idle, 60, 0, 0));
        s.push(seg(Kind::Free, 4, g.pause, 4));
        s.push(seg(Kind::Pause, 120, 0, 0));
        s.push(seg(Kind::Free, 4, g.pause, 4));
        s.push(seg(Kind::Idle, 60, 0, 0));
    }
    s.push(seg(Kind::LongRight, 600, BTN_RIGHT | g.run, 600));
    recover(&mut s);
    s.push(seg(Kind::LongIdle, g.long_idle, 0, 0));
    recover(&mut s);
    s.push(seg(Kind::LongRight, 600, BTN_RIGHT | g.run, 600));
    // Free play: run right hopping, then stand until something happens.
    for _ in 0..(g.free_play / 48) {
        s.push(seg(Kind::Free, 48, BTN_RIGHT | g.run | g.jump, 18));
    }
    recover(&mut s);
    s.push(seg(Kind::LongIdle, g.long_idle, 0, 0));
    s
}

// ------------------------------------------------------------- machines

#[derive(Default)]
struct Sink {
    rows: Vec<Vec<[u8; 3]>>,
    line_palette: Option<rf_renderer::palette::LinePalette>,
    xs: Vec<u16>,
    ys: Vec<u16>,
}

impl CoreSink for Sink {
    fn video_scanline(&mut self, _y: u16, pixels: &[PpuPixel]) {
        let pal = self.line_palette.as_ref();
        self.rows.push(
            pixels
                .iter()
                .map(|p| rf_renderer::palette::resolve_index(p.palette_index, pal))
                .collect(),
        );
    }
    fn palette_scanline(&mut self, _y: u16, palette: &[u16], brightness: u8) {
        self.line_palette = Some(rf_renderer::palette::LinePalette::from_words(
            palette, brightness,
        ));
    }
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, ev: CoreEvent) {
        if let CoreEvent::ScrollWrite { x, y, .. } = ev {
            self.xs.push(x);
            self.ys.push(y);
        }
    }
}

impl Sink {
    /// Mean of a coarse pixel sample, 0..=255 (a black screen is ~0).
    fn mean_luma(&self) -> u32 {
        let (mut sum, mut n) = (0u32, 0u32);
        for row in self.rows.iter().step_by(8) {
            for px in row.iter().step_by(8) {
                sum += u32::from(px[0]) + u32::from(px[1]) + u32::from(px[2]);
                n += 3;
            }
        }
        if n == 0 {
            0
        } else {
            sum / n
        }
    }

    fn png(&self) -> Vec<u8> {
        let h = self.rows.len();
        let w = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        if w == 0 || h == 0 {
            return Vec::new();
        }
        let mut rgba = Vec::with_capacity(w * h * 4);
        for row in &self.rows {
            for x in 0..w {
                let [r, g, b] = row.get(x).copied().unwrap_or([0; 3]);
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        rf_renderer::png::encode_rgba(&rgba, w as u32, h as u32)
    }
}

enum Machine {
    Nes(Box<rf_nes::core::NesCore>),
    Snes(Box<rf_snes::core::SnesCore>),
}

impl Machine {
    fn load(bytes: &[u8]) -> Option<(Self, String)> {
        match rf_cart::Cartridge::load(bytes).ok()? {
            rf_cart::Cartridge::Nes { .. } => {
                let c = rf_nes::core::NesCore::from_ines_bytes(bytes).ok()?;
                let h = rf_cart::hash::identity_nes(bytes).normalized.sha256;
                let mut m = Machine::Nes(Box::new(c));
                if let Machine::Nes(c) = &mut m {
                    c.bus_mut().set_event_mask(EventMask::SCROLL_WRITE);
                }
                Some((m, h))
            }
            rf_cart::Cartridge::Snes { .. } => {
                let c = rf_snes::core::SnesCore::load(bytes).ok()?;
                let h = rf_cart::hash::identity_snes(bytes).normalized.sha256;
                Some((Machine::Snes(Box::new(c)), h))
            }
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    const fn snes(&self) -> bool {
        matches!(self, Machine::Snes(_))
    }

    /// One frame with `pad` held; returns the frame's (x, y) scroll words
    /// (SNES: BG1 registers; NES: the last scroll write, else the last
    /// frame's).
    fn frame(&mut self, pad: u16, sink: &mut Sink, last: (i32, i32)) -> (i32, i32) {
        sink.rows.clear();
        sink.xs.clear();
        sink.ys.clear();
        match self {
            Machine::Nes(c) => {
                let input = rf_core_api::InputFrame {
                    ports: [pad & 0xFF, 0, 0, 0],
                };
                c.run_frame(&input, sink);
                match (sink.xs.last(), sink.ys.last()) {
                    (Some(&x), Some(&y)) => (i32::from(x), i32::from(y)),
                    _ => last,
                }
            }
            Machine::Snes(c) => {
                c.system_mut().bus.joypads.ports[0] = snes_pad(pad);
                let _ = c.step(Step::Frame, sink);
                let bg = &c.system().bus.ppu.bgs[0];
                (i32::from(bg.hofs), i32::from(bg.vofs))
            }
        }
    }

    /// Work RAM into `buf` (NES: 2 KiB; SNES: all 128 KiB).
    fn ram(&self, buf: &mut Vec<u8>) {
        buf.clear();
        match self {
            Machine::Nes(c) => buf.extend((0..0x800u32).map(|a| c.peek(a))),
            Machine::Snes(c) => buf.extend_from_slice(&c.system().bus.wram),
        }
    }

    /// The bus address of RAM offset `off`.
    fn bus_addr(&self, off: usize) -> u32 {
        if self.snes() {
            0x7E_0000 + off as u32
        } else {
            off as u32
        }
    }
}

// ------------------------------------------------------------ trackers

/// Signed difference `a - b` at a width (1 = u8, 2 = u16 little-endian).
fn sd(w: usize, a: u16, b: u16) -> i32 {
    if w == 1 {
        i32::from(a.wrapping_sub(b) as u8 as i8)
    } else {
        i32::from(a.wrapping_sub(b) as i16)
    }
}

/// Per-probe outcome counters for one address:
/// `[pos, neg, zero, return_up, return_down]`.
type Agg = [u8; 5];

/// Position evidence for every RAM address at one width.
struct Tracker {
    w: usize,
    n: usize,
    cur: Vec<u16>,
    prev: Vec<u16>,
    start: Vec<u16>,
    lo: Vec<i16>,
    hi: Vec<i16>,
    chg: Vec<u16>,
    agg: Vec<Vec<Agg>>,
    segs: [u32; PROBES],
    gmin: Vec<u16>,
    gmax: Vec<u16>,
}

impl Tracker {
    fn new(w: usize, ram_len: usize) -> Self {
        let n = ram_len + 1 - w;
        Self {
            w,
            n,
            cur: vec![0; n],
            prev: vec![0; n],
            start: vec![0; n],
            lo: vec![0; n],
            hi: vec![0; n],
            chg: vec![0; n],
            agg: vec![vec![[0; 5]; n]; PROBES],
            segs: [0; PROBES],
            gmin: vec![u16::MAX; n],
            gmax: vec![0; n],
        }
    }

    fn fill(&mut self, ram: &[u8]) {
        if self.w == 1 {
            for (c, &b) in self.cur.iter_mut().zip(ram) {
                *c = u16::from(b);
            }
        } else {
            for (i, c) in self.cur.iter_mut().enumerate() {
                *c = u16::from(ram[i]) | u16::from(ram[i + 1]) << 8;
            }
        }
    }

    /// Whole-run range of every value (called for every play frame).
    fn range(&mut self) {
        for ((lo, hi), &c) in self.gmin.iter_mut().zip(&mut self.gmax).zip(&self.cur) {
            *lo = (*lo).min(c);
            *hi = (*hi).max(c);
        }
    }

    fn spread(&self, a: usize) -> u32 {
        if self.w == 2 && self.gmax[a] >= self.gmin[a] {
            u32::from(self.gmax[a] - self.gmin[a])
        } else {
            0
        }
    }

    fn begin(&mut self) {
        self.start.copy_from_slice(&self.prev);
        self.lo.fill(0);
        self.hi.fill(0);
        self.chg.fill(0);
    }

    /// Called after `fill` for every frame of a probe segment.
    fn step(&mut self) {
        let w = self.w;
        for a in 0..self.n {
            let c = self.cur[a];
            if c != self.prev[a] {
                self.chg[a] = self.chg[a].saturating_add(1);
                let r = sd(w, c, self.start[a]).clamp(-30000, 30000) as i16;
                if r < self.lo[a] {
                    self.lo[a] = r;
                }
                if r > self.hi[a] {
                    self.hi[a] = r;
                }
            }
        }
    }

    fn end(&mut self, kind: usize) {
        self.segs[kind] += 1;
        let w = self.w;
        for a in 0..self.n {
            let end = sd(w, self.cur[a], self.start[a]);
            let (lo, hi) = (i32::from(self.lo[a]), i32::from(self.hi[a]));
            let g = &mut self.agg[kind][a];
            if self.chg[a] == 0 {
                g[2] = g[2].saturating_add(1);
            }
            if end >= 2 && lo >= -1 {
                g[0] = g[0].saturating_add(1);
            }
            if end <= -2 && hi <= 1 {
                g[1] = g[1].saturating_add(1);
            }
            if end.abs() <= 1 && hi >= 4 && lo >= -1 {
                g[3] = g[3].saturating_add(1);
            }
            if end.abs() <= 1 && lo <= -4 && hi <= 1 {
                g[4] = g[4].saturating_add(1);
            }
        }
    }

    fn rotate(&mut self) {
        std::mem::swap(&mut self.cur, &mut self.prev);
    }
}

/// Whole-run counters on single bytes.
struct Counters {
    n: usize,
    play_frames: u32,
    pause_frames: u32,
    /// Frames of the probe segments (the player in the level).
    probe_frames: u32,
    inc_probe: Vec<u32>,
    inc_play: Vec<u32>,
    chg_play: Vec<u32>,
    inc_pause: Vec<u32>,
    chg_pause: Vec<u32>,
    wraps: Vec<u16>,
    carried: Vec<u16>,
    /// Changes seen in play (saturating) — lives need rare changers.
    nchg: Vec<u16>,
    /// Timer evidence in the long idle stretches.
    t_events: Vec<u16>,
    t_last: Vec<u32>,
    t_int: Vec<u16>,
    t_bad: Vec<u16>,
    t_steps: Vec<u16>,
    /// `(frame, offset, old, new)` of ±1 steps on rarely-changing bytes.
    events: Vec<(u32, u32, u8, u8)>,
}

impl Counters {
    fn new(n: usize) -> Self {
        Self {
            n,
            play_frames: 0,
            pause_frames: 0,
            probe_frames: 0,
            inc_probe: vec![0; n],
            inc_play: vec![0; n],
            chg_play: vec![0; n],
            inc_pause: vec![0; n],
            chg_pause: vec![0; n],
            wraps: vec![0; n],
            carried: vec![0; n],
            nchg: vec![0; n],
            t_events: vec![0; n],
            t_last: vec![0; n],
            t_int: vec![0; n],
            t_bad: vec![0; n],
            t_steps: vec![0; n],
            events: Vec::new(),
        }
    }

    /// One frame of play (`pause` = the game is stopped by the script).
    fn frame(
        &mut self,
        f: u32,
        ram: &[u8],
        prev: &[u8],
        pause: bool,
        long_idle: bool,
        probe: bool,
    ) {
        if probe {
            self.probe_frames += 1;
        }
        if pause {
            self.pause_frames += 1;
        } else {
            self.play_frames += 1;
        }
        for a in 0..self.n {
            let (c, p) = (ram[a], prev[a]);
            if c == p {
                continue;
            }
            if pause {
                self.chg_pause[a] += 1;
                self.inc_pause[a] += u32::from(c == p.wrapping_add(1));
                continue;
            }
            self.chg_play[a] += 1;
            if probe {
                self.inc_probe[a] += u32::from(c == p.wrapping_add(1));
            }
            self.inc_play[a] += u32::from(c == p.wrapping_add(1));
            self.nchg[a] = self.nchg[a].saturating_add(1);
            if self.nchg[a] <= 8 && c.abs_diff(p) == 1 && c <= 99 && p <= 99 {
                self.events.push((f, a as u32, p, c));
            }
            // A low byte crossing its wrap, and the next byte following.
            if a + 1 < self.n {
                let up = p >= 0xC0 && c <= 0x40;
                let down = p <= 0x40 && c >= 0xC0;
                if up || down {
                    self.wraps[a] = self.wraps[a].saturating_add(1);
                    let (hp, hc) = (prev[a + 1], ram[a + 1]);
                    if (up && hc == hp.wrapping_add(1)) || (down && hc == hp.wrapping_sub(1)) {
                        self.carried[a] = self.carried[a].saturating_add(1);
                    }
                }
            }
            if long_idle {
                // Steps of exactly one down (a digit's 0 -> 9 counts).
                let stepped = c == p.wrapping_sub(1) || (p == 0 && c == 9);
                if stepped {
                    self.t_steps[a] = self.t_steps[a].saturating_add(1);
                    if self.t_events[a] > 0 {
                        let iv = (f - self.t_last[a]).min(60000) as u16;
                        if self.t_events[a] == 1 {
                            self.t_int[a] = iv;
                        } else if iv.abs_diff(self.t_int[a]) > 1 {
                            self.t_bad[a] = self.t_bad[a].saturating_add(1);
                        }
                    }
                    self.t_events[a] = self.t_events[a].saturating_add(1);
                    self.t_last[a] = f;
                } else {
                    // Any other change breaks a clean countdown.
                    self.t_bad[a] = self.t_bad[a].saturating_add(1);
                }
            }
        }
    }
}

// ------------------------------------------------------------ candidates

#[derive(Debug, Clone)]
struct Cand {
    off: usize,
    w: usize,
    score: f32,
    ev: String,
    /// Whole-run spread of the value (16-bit only; 0 otherwise).
    range: u32,
}

fn frac(n: u8, d: u32) -> f32 {
    if d == 0 {
        0.0
    } else {
        f32::from(n) / d as f32
    }
}

struct Evidence {
    xs: Vec<Cand>,
    ys: Vec<Cand>,
}

/// u16 only counts when the high byte demonstrably carries.
fn width_ok(cn: &Counters, off: usize, w: usize) -> bool {
    if w == 1 {
        return true;
    }
    let (wr, ca) = (cn.wraps[off], cn.carried[off]);
    wr >= 2 && u32::from(ca) * 10 >= u32::from(wr) * 9
}

fn position_candidates(t: &Tracker, cn: &Counters, style: Style, skip: &[usize]) -> Evidence {
    let k = &t.segs;
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for a in 0..t.n {
        if skip.contains(&a) || !width_ok(cn, a, t.w) {
            continue;
        }
        let g = |kind: usize| t.agg[kind][a];
        let (idle, right, left, up, down, jump) = (g(0), g(1), g(2), g(3), g(4), g(5));
        let z_idle = frac(idle[2], k[0]);
        // ---- x: right pos, left neg, idle still.
        let x = frac(right[0], k[1]).min(frac(left[1], k[2])).min(z_idle);
        if x >= 0.5 {
            let vert = (frac(up[2], k[3]) + frac(down[2], k[4])) / 2.0;
            xs.push(Cand {
                off: a,
                w: t.w,
                range: t.spread(a),
                score: x,
                ev: format!(
                    "right+ {}/{} left- {}/{} idle-still {}/{} vert-still {:.0}%",
                    right[0],
                    k[1],
                    left[1],
                    k[2],
                    idle[2],
                    k[0],
                    vert * 100.0
                ),
            });
        }
        // ---- y.
        match style {
            Style::Platformer => {
                let r_up = jump[3];
                let r_dn = jump[4];
                let (r, sign) = if r_up >= r_dn {
                    (r_up, "up")
                } else {
                    (r_dn, "down")
                };
                let pure = if r_up.max(r_dn) == 0 {
                    0.0
                } else {
                    f32::from(r_up.max(r_dn)) / f32::from(r_up + r_dn)
                };
                let walk = (f32::from(right[2]) + f32::from(left[2])) / (k[1] + k[2]).max(1) as f32;
                let y = frac(r, k[5]).min(z_idle).min(pure).min(walk * 2.0);
                if y >= 0.5 {
                    ys.push(Cand {
                        off: a,
                        w: t.w,
                        range: t.spread(a),
                        score: y,
                        ev: format!(
                            "jump returns {}/{} (value goes {} then back) idle-still {}/{} walk-still {}/{}",
                            r, k[5], sign, idle[2], k[0], right[2], k[1]
                        ),
                    });
                }
            }
            Style::TopDown => {
                // Up and Down in opposite directions, still otherwise.
                let a1 = frac(down[0], k[4]).min(frac(up[1], k[3]));
                let a2 = frac(down[1], k[4]).min(frac(up[0], k[3]));
                let y = a1.max(a2).min(z_idle);
                if y >= 0.5 {
                    let horiz = (frac(left[2], k[2]) + frac(right[2], k[1])) / 2.0;
                    ys.push(Cand {
                        off: a,
                        w: t.w,
                        range: t.spread(a),
                        score: y,
                        ev: format!(
                            "down {} {}/{} up {} {}/{} idle-still {}/{} horiz-still {:.0}%",
                            if a1 >= a2 { "+" } else { "-" },
                            if a1 >= a2 { down[0] } else { down[1] },
                            k[4],
                            if a1 >= a2 { "-" } else { "+" },
                            if a1 >= a2 { up[1] } else { up[0] },
                            k[3],
                            idle[2],
                            k[0],
                            horiz * 100.0
                        ),
                    });
                }
            }
        }
    }
    Evidence { xs, ys }
}

/// Drop sprite-table-looking clusters (several candidates a stride of four
/// apart inside 256 bytes) and bytes that merely follow a 16-bit hit.
fn declutter(mut v: Vec<Cand>) -> Vec<Cand> {
    // A sprite table: five or more candidates a multiple of four apart
    // (same 256-byte page, same offset within each four).
    let lanes: BTreeMap<(usize, usize, usize), usize> =
        v.iter().fold(BTreeMap::new(), |mut m, c| {
            *m.entry((c.off >> 8, c.off & 3, c.w)).or_insert(0) += 1;
            m
        });
    v.retain(|c| lanes[&(c.off >> 8, c.off & 3, c.w)] < 5);
    let wide: Vec<usize> = v.iter().filter(|c| c.w == 2).map(|c| c.off).collect();
    v.retain(|c| c.w == 2 || !wide.iter().any(|&o| c.off == o || c.off == o + 1));
    v.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap()
            .then(b.w.cmp(&a.w))
            .then(a.off.cmp(&b.off))
    });
    v
}

struct Tick {
    off: usize,
    rate: f32,
    play_ratio: f32,
    pause_ratio: f32,
    /// Share of in-level (probe) frames it stepped on.
    probe_ratio: f32,
    wraps: u16,
}

fn tick_candidates(cn: &Counters) -> Vec<Tick> {
    let mut v = Vec::new();
    if cn.play_frames < 600 {
        return v;
    }
    for a in 0..cn.n {
        let play = cn.inc_play[a] as f32 / cn.play_frames as f32;
        if play < 0.2 {
            continue;
        }
        // Nearly every change is a +1 step.
        let pure = cn.inc_play[a] as f32 / cn.chg_play[a].max(1) as f32;
        if pure < 0.9 {
            continue;
        }
        let pause = if cn.pause_frames == 0 {
            0.0
        } else {
            cn.chg_pause[a] as f32 / cn.pause_frames as f32
        };
        v.push(Tick {
            off: a,
            rate: cn.inc_probe[a] as f32 / cn.probe_frames.max(1) as f32 * 60.0,
            play_ratio: play,
            pause_ratio: pause,
            probe_ratio: cn.inc_probe[a] as f32 / cn.probe_frames.max(1) as f32,
            wraps: cn.wraps[a],
        });
    }
    v.sort_by(|a, b| {
        b.probe_ratio
            .partial_cmp(&a.probe_ratio)
            .unwrap()
            .then(a.off.cmp(&b.off))
    });
    v
}

struct Timer {
    off: usize,
    interval: u16,
    steps: u16,
}

fn timer_candidates(cn: &Counters) -> Vec<Timer> {
    let mut v = Vec::new();
    for a in 0..cn.n {
        let steps = cn.t_steps[a];
        if steps >= 8 && cn.t_int[a] >= 8 && cn.t_bad[a] * 5 <= steps {
            v.push(Timer {
                off: a,
                interval: cn.t_int[a],
                steps,
            });
        }
    }
    v.sort_by_key(|t| (t.interval, t.off));
    v
}

/// Frames at which the screen was reset: it jumped a long way, or it went
/// black for a stretch (a fade between a level and the map, a death).
fn markers(trace: &[(i32, i32)], dark: &[bool]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut last_mark = 0u32;
    for (i, w) in trace.windows(2).enumerate() {
        let d = (w[1].0 - w[0].0).abs().max((w[1].1 - w[0].1).abs());
        // A wrapped 9/10-bit register is not a jump.
        let d = d.min(1024 - d.min(1024));
        if d >= 48 && i as u32 >= last_mark + 120 {
            out.push(i as u32 + 1);
            last_mark = i as u32 + 1;
        }
    }
    let mut run = 0u32;
    for (i, &d) in dark.iter().enumerate() {
        if d {
            run += 1;
            if run == 15 {
                out.push(i as u32 - 14);
            }
        } else {
            run = 0;
        }
    }
    out.sort_unstable();
    out
}

struct Life {
    off: usize,
    start: u8,
    /// Decrements that fell within 300 frames of a screen reset.
    hits: usize,
    steps: Vec<(u32, u8, u8)>,
}

/// Small bytes whose only moves are ±1 steps (at most four other changes),
/// that start by stepping DOWN from 2..=9 and whose every decrement lands
/// within 300 frames of a screen reset (a death's fade or respawn).
fn life_candidates(cn: &Counters, marks: &[u32]) -> Vec<Life> {
    let mut by: BTreeMap<u32, Vec<(u32, u8, u8)>> = BTreeMap::new();
    for &(f, a, p, c) in &cn.events {
        by.entry(a).or_default().push((f, p, c));
    }
    let near = |f: u32| marks.iter().any(|&m| m.abs_diff(f) <= 300);
    let mut out = Vec::new();
    for (a, steps) in by {
        let a = a as usize;
        if usize::from(cn.nchg[a]) > steps.len() + 4 {
            continue;
        }
        let (_, p0, c0) = steps[0];
        if c0 + 1 != p0 || !(2..=9).contains(&p0) {
            continue;
        }
        // Deaths are seconds apart, and lives rarely go back up.
        if steps.windows(2).any(|w| w[1].0 - w[0].0 < 100) {
            continue;
        }
        let downs: Vec<&(u32, u8, u8)> = steps.iter().filter(|s| s.2 < s.1).collect();
        if (steps.len() - downs.len()) * 2 > downs.len() {
            continue;
        }
        let hits = downs.iter().filter(|s| near(s.0)).count();
        if hits == 0 || hits != downs.len() {
            continue;
        }
        out.push(Life {
            off: a,
            start: p0,
            hits,
            steps,
        });
    }
    out.sort_by(|a, b| b.hits.cmp(&a.hits).then(a.off.cmp(&b.off)));
    out
}

// ------------------------------------------------------------------ I/O

fn rom_bytes(path: &Path) -> Option<Vec<u8>> {
    let raw = std::fs::read(path).ok()?;
    if !raw.starts_with(b"PK\x03\x04") {
        return Some(raw);
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(raw)).ok()?;
    for index in 0..archive.len() {
        let Ok(mut file) = archive.by_index(index) else {
            break;
        };
        if !file.is_file() || file.name().to_ascii_lowercase().ends_with(".zip") {
            continue;
        }
        let mut buf = Vec::new();
        if (&mut file).take(64 << 20).read_to_end(&mut buf).is_err() {
            continue;
        }
        if rf_cart::Cartridge::load(&buf).is_ok() {
            return Some(buf);
        }
    }
    None
}

fn profiles_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles")
}

fn profile_path(slug: &str) -> PathBuf {
    profiles_root().join(slug).join("profile.toml")
}

/// The slug whose profile carries this normalised hash.
fn game_for_hash(hash: &str) -> Option<Game> {
    games()
        .into_iter()
        .find(|g| std::fs::read_to_string(profile_path(g.slug)).is_ok_and(|t| t.contains(hash)))
}

fn camera_addrs(slug: &str) -> Vec<u32> {
    let Ok(text) = std::fs::read_to_string(profile_path(slug)) else {
        return Vec::new();
    };
    let Ok(v) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(cam) = v.get("camera").and_then(|c| c.as_table()) {
        for axis in ["x", "y"] {
            if let Some(addr) = cam
                .get(axis)
                .and_then(|a| a.get("addr"))
                .and_then(toml::Value::as_integer)
            {
                out.push(addr as u32);
            }
        }
    }
    out
}

fn has_camera(slug: &str) -> bool {
    !camera_addrs(slug).is_empty()
}

// ------------------------------------------------------------------ run

fn changes_of(log: &[(usize, u32, u8, u8)], o: usize) -> Vec<(u32, u8, u8)> {
    log.iter()
        .filter(|e| e.0 == o)
        .map(|e| (e.1, e.2, e.3))
        .collect()
}

/// Debugging aid: `RF_REFIND_FIND="03 08 08"` (bytes) or `w388` (a
/// little-endian u16) prints every work-RAM offset holding that value at
/// the frame a run was stopped on — for matching what the screen shows
/// (a HUD number) to the bytes that hold it.
fn find_dump(m: &Machine) {
    let Ok(spec) = std::env::var("RF_REFIND_FIND") else {
        return;
    };
    let mut ram = Vec::new();
    m.ram(&mut ram);
    let needle: Vec<u8> = if let Some(w) = spec.strip_prefix('w') {
        w.trim()
            .parse::<u16>()
            .map(|v| v.to_le_bytes().to_vec())
            .unwrap_or_default()
    } else {
        spec.split_whitespace()
            .filter_map(|b| u8::from_str_radix(b, 16).ok())
            .collect()
    };
    if needle.is_empty() {
        return;
    }
    let hits: Vec<String> = ram
        .windows(needle.len())
        .enumerate()
        .filter(|(_, w)| *w == needle.as_slice())
        .take(64)
        .map(|(i, _)| format!("{:#X}", m.bus_addr(i)))
        .collect();
    println!("FIND {spec}: {}", hits.join(" "));
}

struct Report {
    rows: Vec<String>,
    overclock: Option<u32>,
}

fn run_game(g: &Game, bytes: &[u8]) -> Option<Report> {
    let started = Instant::now();
    let (mut m, _hash) = Machine::load(bytes)?;
    let snes = m.snes();
    let ram_len = if snes { 0x20000 } else { 0x800 };
    let opening = std::env::var("RF_REFIND_OPEN")
        .map(|t| parse_recipe(&t))
        .unwrap_or_else(|_| parse_recipe(g.opening));
    let script = play_script(g);
    let stop: Option<u32> = std::env::var("RF_REFIND_STOP")
        .ok()
        .and_then(|v| v.parse().ok());
    let shots = std::env::var("RF_REFIND_SHOTS").ok();
    let shot_every: u32 = std::env::var("RF_REFIND_SHOT_EVERY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);

    let mut sink = Sink::default();
    let mut ram = Vec::new();
    let mut prev: Vec<u8> = vec![0; ram_len];
    let mut t1 = Tracker::new(1, ram_len);
    let mut t2 = Tracker::new(2, ram_len);
    let mut cn = Counters::new(ram_len);
    let mut finder = (!has_camera(g.slug)).then(|| {
        rf_enhance::camera_finder::CameraFinder::new(if snes { 0x2000 } else { 0x800 }, 2)
    });
    let mut trace: Vec<(i32, i32)> = Vec::new();
    // `RF_REFIND_TRACE=off,off` : (offset, frame, old, new) of every change.
    let traced: Vec<usize> = std::env::var("RF_REFIND_TRACE")
        .map(|l| {
            l.split(',')
                .filter_map(|w| usize::from_str_radix(w.trim(), 16).ok())
                .collect()
        })
        .unwrap_or_default();
    let mut tracelog: Vec<(usize, u32, u8, u8)> = Vec::new();
    // Per frame: is the picture (nearly) black?
    let mut dark: Vec<bool> = Vec::new();
    let mut scroll = (0, 0);
    let mut f: u32 = 0;
    let shot = |sink: &Sink, f: u32| {
        if let Some(dir) = &shots {
            if f.is_multiple_of(shot_every) {
                let _ = std::fs::create_dir_all(dir);
                let png = sink.png();
                if !png.is_empty() {
                    let _ = std::fs::write(Path::new(dir).join(format!("{f:06}.png")), png);
                }
            }
        }
    };

    // ---- opening
    for &pad in &opening {
        scroll = m.frame(pad, &mut sink, scroll);
        trace.push(scroll);
        dark.push(sink.mean_luma() < 4);
        shot(&sink, f);
        f += 1;
        if stop.is_some_and(|s| f >= s) {
            println!("[{}] stopped at frame {f}", g.slug);
            find_dump(&m);
            return None;
        }
    }
    m.ram(&mut ram);
    prev.copy_from_slice(&ram[..ram_len]);
    t1.fill(&ram);
    t2.fill(&ram);
    t1.rotate();
    t2.rotate();

    // ---- play
    let mut pause_for_real = false;
    for sg in &script {
        let probe = (sg.kind as usize) < PROBES;
        let is_pause = sg.kind == Kind::Pause;
        // The Start presses that bracket a pause test are "Free" segments
        // of four frames; the stretch between them is the paused one.
        if sg.kind == Kind::Free && sg.frames == 4 {
            pause_for_real = !pause_for_real;
        }
        for i in 0..sg.frames {
            let pad = if i < sg.hold { sg.pad } else { 0 };
            scroll = m.frame(pad, &mut sink, scroll);
            trace.push(scroll);
            dark.push(sink.mean_luma() < 4);
            shot(&sink, f);
            m.ram(&mut ram);
            let ramv = &ram[..ram_len];
            let transient = sg.kind == Kind::Free && sg.frames == 4;
            // Frames that count as "play" for tick statistics.
            if !transient {
                cn.frame(f, ramv, &prev, is_pause, sg.kind == Kind::LongIdle, probe);
            }
            if probe {
                t1.fill(ramv);
                t2.fill(ramv);
                t2.range();
                if sg.settle == 0 && i == 0 {
                    t1.begin();
                    t2.begin();
                }
                if i >= sg.settle {
                    t1.step();
                    t2.step();
                }
                t1.rotate();
                t2.rotate();
                if sg.settle > 0 && i + 1 == sg.settle {
                    // The scored window opens after the settle frames.
                    t1.begin();
                    t2.begin();
                }
            } else {
                t1.fill(ramv);
                t2.fill(ramv);
                t2.range();
                t1.rotate();
                t2.rotate();
            }
            if let Some(fd) = &mut finder {
                let n = if snes { 0x2000 } else { 0x800 };
                let (hx, vy) = scroll;
                let xs = [(hx & 0xFF) as u8];
                let ys = [(vy & 0xFF) as u8];
                let layers = [(&xs[..], &ys[..]), (&[][..], &[][..])];
                let _ = fd.observe(&layers, &ramv[..n], true);
            }
            for &o in &traced {
                if o < ramv.len() && ramv[o] != prev[o] {
                    tracelog.push((o, f, prev[o], ramv[o]));
                }
            }
            prev.copy_from_slice(ramv);
            f += 1;
            if stop.is_some_and(|s| f >= s) {
                println!("[{}] stopped at frame {f}", g.slug);
                find_dump(&m);
                return None;
            }
        }
        if probe {
            // `cur` was rotated into `prev`; the end value is in `prev`.
            std::mem::swap(&mut t1.cur, &mut t1.prev);
            std::mem::swap(&mut t2.cur, &mut t2.prev);
            t1.end(sg.kind as usize);
            t2.end(sg.kind as usize);
            std::mem::swap(&mut t1.cur, &mut t1.prev);
            std::mem::swap(&mut t2.cur, &mut t2.prev);
        }
    }
    let _ = pause_for_real;
    let sim_secs = started.elapsed().as_secs_f32();

    // ---- detectors
    let mut rows: Vec<String> = Vec::new();
    let mut overclock = None;
    println!(
        "\n=== {} ({} frames, {:.0}s) ===",
        g.slug,
        f,
        started.elapsed().as_secs_f32()
    );
    println!("probe segments run: {:?}", t1.segs);

    let marks = markers(&trace, &dark);
    println!("screen-jump markers (frames): {marks:?}");

    // camera
    let cams = camera_addrs(g.slug);
    let mut skip: Vec<usize> = cams
        .iter()
        .flat_map(|&a| {
            let o = (a & 0x1FFFF) as usize;
            [o, o + 1]
        })
        .collect();
    if let Some(fd) = &finder {
        match fd.verdict() {
            Some(found) => {
                println!(
                    "camera: x @{:#X} {} ratio {:.2} moving {}; y {:?}",
                    m.bus_addr(found.x.offset),
                    found.x.width.as_str(),
                    found.x.ratio,
                    found.x.moving,
                    found
                        .y
                        .map(|y| (m.bus_addr(y.offset), y.width.as_str(), y.ratio))
                );
                skip.push(found.x.offset);
                skip.push(found.x.offset + 1);
                if let Some(y) = found.y {
                    skip.push(y.offset);
                    skip.push(y.offset + 1);
                }
                let mut row = format!(
                    "[camera]\nmode = \"{}\"\nx = {{ addr = {:#06X}, type = \"{}\" }}\n",
                    if g.style == Style::TopDown {
                        "top_down"
                    } else {
                        "side_scroller"
                    },
                    m.bus_addr(found.x.offset),
                    found.x.width.as_str()
                );
                if let Some(y) = found.y {
                    row.push_str(&format!(
                        "y = {{ addr = {:#06X}, type = \"{}\" }}\n",
                        m.bus_addr(y.offset),
                        y.width.as_str()
                    ));
                }
                rows.push(format!("#CAMERA\n{row}"));
            }
            None => {
                for (i, (ax, ay)) in fd.layers.iter().enumerate() {
                    println!(
                        "camera near-miss layer {i}: x moving {} best {:X?}; y moving {} best {:X?}",
                        ax.moving(),
                        ax.best(2),
                        ay.moving(),
                        ay.best(2)
                    );
                }
            }
        }
    } else {
        println!("camera: profile already declares {cams:X?}");
    }

    // tick counter
    let ticks = tick_candidates(&cn);
    println!(
        "tick counter candidates ({} play / {} pause frames):",
        cn.play_frames, cn.pause_frames
    );
    for t in ticks.iter().take(8) {
        println!(
            "  @{:#X} {:.1} inc/s  in-level {:.0}%  all-play {:.0}%  changes-while-paused {:.0}%  wraps {}",
            m.bus_addr(t.off),
            t.rate,
            t.probe_ratio * 100.0,
            t.play_ratio * 100.0,
            t.pause_ratio * 100.0,
            t.wraps
        );
    }
    // Prefer a counter that steps on (nearly) every in-level frame AND
    // stands still while paused (a per-logic-tick counter); else one that
    // steps on nearly every in-level frame regardless.
    // (Only the SNES governor needs a full mod-256 counter.)
    let strong = |t: &&Tick| t.wraps >= 4 && t.probe_ratio >= if snes { 0.9 } else { 0.8 };
    if let Some(t) = ticks
        .iter()
        .filter(strong)
        .find(|t| t.pause_ratio < 0.05)
        .or_else(|| ticks.iter().find(strong))
    {
        let addr = m.bus_addr(t.off);
        let frozen = t.pause_ratio < 0.05;
        println!(
            "  -> {} {addr:#X} ({:.1} Hz, {})",
            if frozen {
                "tick_counter"
            } else {
                "frame_counter"
            },
            t.rate,
            if t.pause_ratio < 0.05 {
                "frozen while paused"
            } else {
                "keeps counting while paused"
            }
        );
        // A counter that runs on while the game is paused is the frame
        // (NMI) counter: it cannot show slowdown, so it is no logic tick.
        if snes && frozen {
            overclock = Some(addr);
        }
        rows.push(format!(
            "#ROW\nlabel = \"{}\"\naddr = {addr:#08X}\nlen = 1\ntype = \"u8\"\nnotes = \"Increments by exactly 1 (mod 256) on {:.0}% of {} in-level frames = {:.1}/s, {} wraps; changed on {:.0}% of {} paused frames{}.\"\n",
            if frozen { "tick_counter" } else { "frame_counter" },
            t.probe_ratio * 100.0,
            cn.probe_frames,
            t.rate,
            t.wraps,
            t.pause_ratio * 100.0,
            cn.pause_frames,
            if frozen {
                " (a per-logic-tick counter)"
            } else {
                "; it runs on while paused, so it is the frame counter, not a logic tick"
            }
        ));
    }

    if std::env::var("RF_REFIND_VERBOSE").is_ok() {
        for t in [&t1, &t2] {
            println!("verbose u{} right-movers (R>=8, L>=8):", t.w * 8);
            let mut shown = 0;
            for a in 0..t.n {
                let (r, l, i) = (t.agg[1][a], t.agg[2][a], t.agg[0][a]);
                if (r[0] >= 8 && l[1] >= 8) || (r[1] >= 8 && l[0] >= 8) {
                    let (u, d) = (t.agg[3][a], t.agg[4][a]);
                    println!(
                        "  @{:#X} R(+{} -{} 0:{}) L(+{} -{} 0:{}) idle0 {}/{} U0 {} D0 {} wraps {}/{}",
                        m.bus_addr(a), r[0], r[1], r[2], l[0], l[1], l[2], i[2], t.segs[0], u[2], d[2], cn.carried[a], cn.wraps[a]
                    );
                    shown += 1;
                    if shown >= 60 {
                        break;
                    }
                }
            }
        }
    }
    if std::env::var("RF_REFIND_VERBOSE").is_ok() {
        println!("verbose jump-returners:");
        let mut shown = 0;
        for t in [&t1, &t2] {
            for a in 0..t.n {
                let j = t.agg[5][a];
                if j[3] + j[4] >= 8
                    && t.agg[0][a][2] >= 50
                    && t.agg[1][a][2] + t.agg[2][a][2] >= 10
                    && shown < 40
                {
                    println!(
                        "  u{} @{:#X} jump up {} down {} idle0 {}/{} walk0 R{} L{} range {}..{} wraps {}/{}",
                        t.w * 8, m.bus_addr(a), j[3], j[4], t.agg[0][a][2], t.segs[0],
                        t.agg[1][a][2], t.agg[2][a][2], t.gmin[a], t.gmax[a], cn.carried[a], cn.wraps[a]
                    );
                    shown += 1;
                }
            }
        }
    }
    // positions
    let e1 = position_candidates(&t1, &cn, g.style, &skip);
    let e2 = position_candidates(&t2, &cn, g.style, &skip);
    let xs = declutter(e1.xs.into_iter().chain(e2.xs).collect());
    let ys = declutter(e1.ys.into_iter().chain(e2.ys).collect());
    println!("player x candidates:");
    for c in xs.iter().take(8) {
        println!(
            "  @{:#X} u{} score {:.2} range {}  {}",
            m.bus_addr(c.off),
            c.w * 8,
            c.score,
            c.range,
            c.ev
        );
    }
    println!("player y candidates:");
    for c in ys.iter().take(8) {
        println!(
            "  @{:#X} u{} score {:.2} range {}  {}",
            m.bus_addr(c.off),
            c.w * 8,
            c.score,
            c.range,
            c.ev
        );
    }
    for (label, list) in [("player_x", &xs), ("player_y", &ys)] {
        let Some(c) = list.first() else { continue };
        // Twins behave identically in every probe (a mirrored copy).
        let twins: Vec<&Cand> = list
            .iter()
            .skip(1)
            .filter(|d| d.w == c.w && d.ev == c.ev)
            .collect();
        let tie = list.iter().skip(1).any(|d| {
            // A carried 16-bit pair outranks single bytes; ties are
            // only between candidates of the same kind.
            (c.w == 2 && d.w == 2 || c.w == 1)
                && d.score >= c.score - 0.05
                && !(d.w == c.w && d.ev == c.ev)
        });
        if c.score >= if c.w == 2 { 0.9 } else { 0.95 } && !tie {
            let copies = if twins.is_empty() {
                String::new()
            } else {
                format!(
                    " Identical probe outcomes at {} (a mirrored copy).",
                    twins
                        .iter()
                        .map(|d| format!("{:#X}", m.bus_addr(d.off)))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            rows.push(format!(
                "#ROW\nlabel = \"{label}\"\naddr = {:#08X}\nlen = {}\ntype = \"u{}\"\nnotes = \"Observed: {}; {}{}\"\n",
                m.bus_addr(c.off),
                c.w,
                c.w * 8,
                c.ev,
                if c.w == 2 {
                    format!(
                        "high byte carries on low-byte wraps ({}/{}); value spans {} over the run.",
                        cn.carried[c.off], cn.wraps[c.off], c.range
                    )
                } else {
                    "single byte (a 16-bit pair was not evidenced).".to_string()
                },
                copies
            ));
        }
    }

    if let Ok(list) = std::env::var("RF_REFIND_TRACE") {
        for w in list.split(',') {
            if let Ok(o) = usize::from_str_radix(w.trim(), 16) {
                println!(
                    "TRACE {o:#X}: {:?}",
                    &changes_of(&tracelog, o)[..changes_of(&tracelog, o).len().min(70)]
                );
            }
        }
    }
    if let Ok(list) = std::env::var("RF_REFIND_DUMP") {
        for w in list.split(',') {
            if let Ok(o) = usize::from_str_radix(w.trim(), 16) {
                if o < cn.n {
                    println!(
                        "DUMP {o:#X}: steps {} bad {} int {} events {} nchg {} chg_play {} inc_play {}",
                        cn.t_steps[o], cn.t_bad[o], cn.t_int[o], cn.t_events[o], cn.nchg[o], cn.chg_play[o], cn.inc_play[o]
                    );
                    let ev: Vec<_> = cn.events.iter().filter(|e| e.1 as usize == o).collect();
                    println!("   events {ev:?}");
                }
            }
        }
    }
    // timer
    let timers = timer_candidates(&cn);
    println!("timer candidates (steady -1 steps while idle):");
    for t in timers.iter().take(8) {
        println!(
            "  @{:#X} every {} frames ({:.2}/s), {} steps",
            m.bus_addr(t.off),
            t.interval,
            60.0 / f32::from(t.interval.max(1)),
            t.steps
        );
    }

    // The timer's lowest-interval digit with a tens digit stepping a tenth
    // as often just below it.
    if let Some(ones) = timers.first() {
        let tens = timers
            .iter()
            .find(|t| t.off + 1 == ones.off && t.interval.abs_diff(ones.interval * 10) <= 12);
        if let Some(tens) = tens {
            // A hundreds digit just before the tens: it steps in the very
            // frame the tens digit wraps 0 -> 9, and at no other time.
            let h = tens.off - 1;
            let hundreds = tens.off > 0
                && cn.t_steps[h] >= 1
                && cn.t_bad[h] == 0
                && cn.t_last[h] == cn.t_last[tens.off];
            let (first, len, extra) = if hundreds {
                (h, 3, "; the hundreds digit (at -1 from the tens) stepped in the same frame the tens wrapped 0 to 9".to_string())
            } else {
                (tens.off, 2, "; no hundreds digit was evidenced".to_string())
            };
            let mirrors: Vec<String> = timers
                .iter()
                .filter(|t| t.off != ones.off && t.interval == ones.interval)
                .map(|t| format!("{:#X}", m.bus_addr(t.off)))
                .collect();
            rows.push(format!(
                "#ROW\nlabel = \"timer_digits\"\naddr = {:#08X}\nlen = {}\ntype = \"bytes\"\nshow = \"digits\"\nnotes = \"Observed while idling (no input): the last digit steps down by 1 (9 after 0) every {} frames ({:.2}/s), {} steps in a row with no other change; the digit before it steps every {} frames{}. Mirrors with identical steps: {}.\"\n",
                m.bus_addr(first),
                len,
                ones.interval,
                60.0 / f32::from(ones.interval.max(1)),
                ones.steps,
                tens.interval,
                extra,
                if mirrors.is_empty() { "none seen".to_string() } else { mirrors.join(", ") }
            ));
        }
    }

    // lives
    let lives = life_candidates(&cn, &marks);
    println!("lives candidates (rare -1 step near a screen reset):");
    for l in lives.iter().take(8) {
        println!(
            "  @{:#X} start {} hits {} steps {:?}",
            m.bus_addr(l.off),
            l.start,
            l.hits,
            l.steps
        );
    }
    // Written only on repeated evidence: at least two deaths coincided
    // with its steps, and no other candidate matched as often.
    let unique = lives
        .first()
        .filter(|l| l.hits >= 2 && lives.get(1).is_none_or(|n| n.hits < l.hits));
    if let Some(l) = unique {
        rows.push(format!(
            "#ROW\nlabel = \"lives\"\naddr = {:#08X}\nlen = 1\ntype = \"u8\"\nnotes = \"Observed: the only small byte (value {}) that stepped down by exactly 1 ({}) within 300 frames of a screen reset (a black fade or a scroll jump) {} time(s), and every one of its decrements did; its ±1 steps (frame, from, to): {:?}; it changed {} time(s) over {} play frames.\"\n",
            m.bus_addr(l.off),
            l.start,
            l.start - 1,
            l.hits,
            l.steps,
            cn.nchg[l.off],
            cn.play_frames
        ));
    }
    println!("(simulation + detection {:.0}s)", sim_secs);
    Some(Report { rows, overclock })
}

#[test]
fn profile_refind() {
    let Ok(list) = std::env::var(ROMS_VAR) else {
        eprintln!("SKIP: set {ROMS_VAR} to a `:`-separated list of ROM paths");
        return;
    };
    let write = std::env::var("RF_REFIND_WRITE").is_ok();
    for path in list.split(':').filter(|p| !p.is_empty()) {
        let Some(bytes) = rom_bytes(Path::new(path)) else {
            println!("{path}: not a readable cartridge");
            continue;
        };
        let hash = match rf_cart::Cartridge::load(&bytes) {
            Ok(rf_cart::Cartridge::Nes { .. }) => {
                rf_cart::hash::identity_nes(&bytes).normalized.sha256
            }
            Ok(_) => rf_cart::hash::identity_snes(&bytes).normalized.sha256,
            Err(_) => continue,
        };
        let Some(game) = game_for_hash(&hash) else {
            println!("{path}: no profile with sha256 {hash}");
            continue;
        };
        let Some(report) = run_game(&game, &bytes) else {
            continue;
        };
        if write {
            write_rows(&game, &report);
        }
    }
}

/// Append the high-confidence rows to the profile (labels not yet there).
fn write_rows(g: &Game, r: &Report) {
    let path = profile_path(g.slug);
    let Ok(mut text) = std::fs::read_to_string(&path) else {
        return;
    };
    let mut out = String::new();
    for row in &r.rows {
        if let Some(body) = row.strip_prefix("#ROW\n") {
            let label = body
                .lines()
                .find_map(|l| l.strip_prefix("label = \""))
                .and_then(|l| l.strip_suffix('"'))
                .unwrap_or("");
            if text.contains(&format!("label = \"{label}\"")) {
                continue;
            }
            out.push_str("\n[[memory_map]]\n");
            out.push_str(body);
            out.push_str(&format!("source = \"{SOURCE}\"\n"));
        } else if let Some(body) = row.strip_prefix("#CAMERA\n") {
            if !text.contains("\n[camera]") {
                out.push('\n');
                out.push_str(body);
            }
        }
    }
    if let Some(addr) = r.overclock {
        if !text.contains("tick_counter") {
            if text.contains("\n[overclock]") {
                text = text.replace(
                    "\n[overclock]\n",
                    &format!("\n[overclock]\ntick_counter = {addr:#08X}\n"),
                );
            } else {
                out.push_str(&format!("\n[overclock]\ntick_counter = {addr:#08X}\n"));
            }
        }
    }
    text.push_str(&out);
    let _ = std::fs::write(path, text);
}
