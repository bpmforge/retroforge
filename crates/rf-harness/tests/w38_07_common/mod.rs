//! Shared drive for the W38-07 Castlevania tests (ROM-gated, law 5): the ROM
//! loader, a tiny input-script language and a frame-to-PNG helper. Only offsets,
//! counts and verdicts are printed (D-021).
//!
//! The scripts were found by watching the screen (menu flow, where the first
//! stage's enemies come from) and the RAM that changed at the frames the screen
//! scrolled; nothing is taken from a community RAM map or a disassembly.
#![allow(dead_code)]

use std::io::Read as _;
use std::path::PathBuf;

use rf_core_api::{CoreEvent, CoreSink, InputFrame, PpuPixel};

pub const FILE: &str = "Castlevania (USA).zip";
pub const A: u8 = 1 << 0;
pub const B: u8 = 1 << 1;
pub const SELECT: u8 = 1 << 2;
pub const START: u8 = 1 << 3;
pub const UP: u8 = 1 << 4;
pub const DOWN: u8 = 1 << 5;
pub const LEFT: u8 = 1 << 6;
pub const RIGHT: u8 = 1 << 7;

pub fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = std::path::Path::new(&root).join("nes").join(FILE);
    p.exists().then_some(p)
}

/// The single NES image inside the zip (content-sniffed, like the app).
pub fn load_rom(path: &std::path::Path) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("reads the zip");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("zip");
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).expect("entry");
        if !entry.is_file() {
            continue;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).expect("reads");
        if matches!(
            rf_cart::Cartridge::load(&buf),
            Ok(rf_cart::Cartridge::Nes { .. })
        ) {
            return buf;
        }
    }
    panic!("no NES image in the zip");
}

/// A script: whitespace-separated tokens. `wN` = N idle frames; otherwise
/// `+`-joined letters then a frame count, e.g. `R+B14` (A B E=select T=start
/// U D L R).
pub fn parse(script: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for tok in script.split_whitespace() {
        let split = tok
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or_else(|| panic!("token {tok} has no count"));
        let (keys, count) = tok.split_at(split);
        let n: usize = count.parse().expect("count");
        let mut mask = 0u8;
        if keys != "w" {
            for k in keys.split('+') {
                mask |= match k {
                    "A" => A,
                    "B" => B,
                    "E" => SELECT,
                    "T" => START,
                    "U" => UP,
                    "D" => DOWN,
                    "L" => LEFT,
                    "R" => RIGHT,
                    other => panic!("key {other}"),
                };
            }
        }
        out.extend(std::iter::repeat_n(mask, n));
    }
    out
}

pub fn to_inputs(pad: &[u8]) -> Vec<InputFrame> {
    pad.iter()
        .map(|&b| {
            let mut i = InputFrame::empty();
            i.ports[0] = u16::from(b);
            i
        })
        .collect()
}

pub struct NullSink;

impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

/// Collects one 256x240 frame of palette indices.
#[derive(Default)]
pub struct Shot {
    pub rows: Vec<Vec<u8>>,
}

impl CoreSink for Shot {
    fn video_scanline(&mut self, y: u16, p: &[PpuPixel]) {
        let y = usize::from(y);
        if self.rows.len() <= y {
            self.rows.resize(y + 1, Vec::new());
        }
        self.rows[y] = p.iter().map(|p| p.palette_index).collect();
    }
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

impl Shot {
    pub fn png(&self, scale: usize) -> Vec<u8> {
        let h = self.rows.len();
        let w = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut rgba = Vec::with_capacity(w * h * scale * scale * 4);
        for row in &self.rows {
            let mut line = Vec::with_capacity(w * scale * 4);
            for x in 0..w {
                let c =
                    rf_renderer::palette::palette_index_to_rgb(row.get(x).copied().unwrap_or(0));
                for _ in 0..scale {
                    line.extend_from_slice(&[c[0], c[1], c[2], 255]);
                }
            }
            for _ in 0..scale {
                rgba.extend_from_slice(&line);
            }
        }
        rf_renderer::png::encode_rgba(&rgba, (w * scale) as u32, (h * scale) as u32)
    }
}

/// First frame of the traced play: the title is left at frame 200 and the
/// stage's clock starts a little before this.
pub const PLAY_FROM: u32 = 400;
/// Pattern frames: 12 pad rows of the blind patrol, see [`play_script`].
const PATROL: &str = "R+B3 R17 R+A8 R+B3 R12 R+B3 R12 w6";

/// The play: Start on the title, then segments of a blind patrol (walk right,
/// whip, hop), each ended by a pair of Start presses (a no-op pause and resume
/// in play; the "continue" choice on a game-over screen). The blind patrol dies
/// often, which replays the stage's early part with its enemies each time.
pub fn play_script(segments: usize) -> Vec<u8> {
    let mut s = String::from("w200 T4 w196");
    for _ in 0..segments {
        for _ in 0..40 {
            s.push(' ');
            s.push_str(PATROL);
        }
        s.push_str(" T2 w20 T2 w30");
    }
    parse(&s)
}

/// Number of segments (env `CV_SEGS` shortens the play for development).
pub fn segments() -> usize {
    std::env::var("CV_SEGS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8)
}

pub fn frames() -> u32 {
    play_script(segments()).len() as u32
}

pub fn pad() -> Vec<u8> {
    play_script(segments())
}

pub fn inputs() -> Vec<InputFrame> {
    to_inputs(&pad())
}
