//! Shared drive for the W38-06 Super Metroid tests (ROM-gated, law 5): the ROM
//! loader, the token script and a frame dump for looking at the screen. Only
//! offsets, counts and verdicts are printed (D-021).
//!
//! The play was found by watching the screen (menu flow, rooms, doors) and RAM
//! that changed; nothing is taken from a community RAM map or a disassembly.
#![allow(dead_code)]

use std::io::Read as _;
use std::path::PathBuf;

use rf_core_api::{CoreEvent, CoreSink, InputFrame, PpuPixel};
use rf_input::SnesButton;

pub const FILE: &str = "Super Metroid (Japan, USA) (En,Ja).zip";
pub const RAM_BASE: u32 = 0x7E_0000;

/// Boot, the option screen, a new game and the story pages (A advances each),
/// to the first room with the player in control: about 11,800 frames.
pub fn to_free_play() -> String {
    let mut s = String::from("w300 w300 S6 w120 S6 w120 A6 w120 A6 w120 S6 w120 A6 w600");
    for _ in 0..55 {
        s += " A6 w150";
    }
    s + " w500 w600 w300"
}

/// Free play: down the shaft in the first room (the screen scrolls down), the
/// corridor and the following rooms to the one with the glass capsule, where a
/// boss enters the picture and a countdown starts. Found by watching screen
/// dumps and the room-id word change at each door.
pub const FIRST_ROOMS: &str = "R40 w60 R40 w60 L40 w60 R40 w60 D10 w60 L40 w60 L40 w60 R40 w60 D10 w60 R40 R+A25 R30 R+A25 R30 w250 R+A30 R90 R+A25 R120 R120 R120 R90 R90 R90 L60 L60 L60 L60 R+A20 R80 R80 R80 R80 R120 R120 R120 R120 R120 R120 R120 R120 R60 w300 R60 w300 w300 w600 w300";

/// The way back during the countdown: left through every room (steps are
/// climbed with a jump), up the zig-zag room, and into the tall room where the
/// first screen was, one screen below the start.
pub fn way_back() -> String {
    let rep = |t: &str, n: usize| vec![t; n].join(" ");
    format!(
        "L100 L+A30 L40 L+A30 L40 L60 L60 {} {} w30 R+A25 R30 w30 {} L6 A45 L+A5 L20 L40 L60 L60 L60 w30 {} w60 U10 w10",
        rep("L+A20 L40", 8),
        rep("L+A20 L40", 10),
        rep("R+A20 R40", 5),
        rep("L+A15 L45", 8)
    )
}

/// Up the tall room (spin jumps from side to side, found by a bounded random
/// search over jump tokens scored on the height reached, one trial at a time on
/// a copy of the machine), to the top with under a second of the countdown left.
pub const CLIMB: &str = "w10 L+A20 R+A20 L+A20 R+A35 R12 R+A35 L+A12 L12 R+A12 R+A12 L+A20 R25 R+A45 R25 R+A20 R+A35 R12 R+A20 L+A35 R25 R25 R+A35 L+A12 R+A45 A40 L+A45 L25 L25 R+A35 w10 L+A35 R+A45 R+A20 L+A20 L+A12 R+A35 L+A12 R+A45 R+A20 R+A35 L+A35 w10 L+A45 A40 L+A20 L+A20 R+A35 R+A12 w60";

/// The scene after the countdown, then the first outdoor rooms: right to the
/// wall, left across the long room to its door (one shot opens it), through it
/// and left along the next room (hopping), then back right.
pub fn outdoors() -> String {
    let rep = |t: &str, n: usize| vec![t; n].join(" ");
    format!(
        "w1800 w800 R60 R60 R60 R60 R60 R60 L100 L100 L100 L100 L100 L100 L100 L100 X5 w30 L60 L60 L60 L60 L60 L60 {} {}",
        rep("L+A20 L40", 14),
        rep("R+A20 R40", 14)
    )
}

/// The whole token script of W38-06.
pub fn script_tokens() -> String {
    format!(
        "{} {FIRST_ROOMS} {} {CLIMB} {}",
        to_free_play(),
        way_back(),
        outdoors()
    )
}

/// The pad words of the whole script.
pub fn script() -> Vec<u16> {
    frames_of(&script_tokens())
}

/// Frame at which the first room is reached (the observed passes start a little
/// before it).
pub fn play_from() -> usize {
    frames_of(&to_free_play()).len() - 120
}

pub fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = std::path::Path::new(&root).join("snes").join(FILE);
    p.exists().then_some(p)
}

/// The single SNES image inside the zip (content-sniffed, like the app).
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
            Ok(rf_cart::Cartridge::Snes { .. })
        ) {
            return buf;
        }
    }
    panic!("no SNES image in the zip");
}

/// `wN` idles N frames, `<buttons>N` holds them (`A B X Y S E U D L R`, `+`).
pub fn frames_of(text: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for tok in text.split_whitespace() {
        let (mask, n) = if let Some(n) = tok.strip_prefix('w') {
            (0u16, n.parse::<usize>().expect("wN"))
        } else {
            let split = tok.find(|c: char| c.is_ascii_digit()).expect("button+N");
            let (b, n) = tok.split_at(split);
            let mask = b.split('+').fold(0u16, |m, c| {
                let bit = match c {
                    "A" => SnesButton::A,
                    "B" => SnesButton::B,
                    "X" => SnesButton::X,
                    "Y" => SnesButton::Y,
                    "S" => SnesButton::Start,
                    "E" => SnesButton::Select,
                    "U" => SnesButton::Up,
                    "D" => SnesButton::Down,
                    "L" => SnesButton::Left,
                    "R" => SnesButton::Right,
                    other => panic!("button {other}"),
                }
                .bit();
                m | (1 << bit)
            });
            (mask, n.parse().expect("N"))
        };
        out.extend(std::iter::repeat_n(mask, n));
    }
    out
}

pub fn inputs_of(masks: &[u16]) -> Vec<InputFrame> {
    masks
        .iter()
        .map(|&m| {
            let mut f = InputFrame::empty();
            f.ports[0] = m;
            f
        })
        .collect()
}

/// A sink that keeps the last frame's palette indices (for a screen dump).
pub struct Shot {
    pub lines: Vec<Vec<u8>>,
}

impl Shot {
    pub fn new() -> Self {
        Self { lines: Vec::new() }
    }
}

impl CoreSink for Shot {
    fn video_scanline(&mut self, y: u16, p: &[PpuPixel]) {
        let y = usize::from(y);
        if self.lines.len() <= y {
            self.lines.resize(y + 1, Vec::new());
        }
        self.lines[y] = p.iter().map(|p| p.palette_index).collect();
    }
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

pub struct NullSink;

impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

/// Writes the dump as a 24-bit BMP (`sips -s format png` makes a PNG of it);
/// `cgram` is the 512-byte palette (15-bit BGR).
pub fn write_bmp(path: &std::path::Path, shot: &Shot, cgram: &[u8]) {
    let h = shot.lines.len();
    let w = shot.lines.iter().map(Vec::len).max().unwrap_or(0);
    let row = (w * 3).div_ceil(4) * 4;
    let mut out = Vec::with_capacity(54 + row * h);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((54 + row * h) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as u32).to_le_bytes());
    out.extend_from_slice(&(h as u32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for line in shot.lines.iter().rev() {
        let start = out.len();
        for x in 0..w {
            let i = usize::from(line.get(x).copied().unwrap_or(0));
            let c = u16::from(cgram[i * 2]) | u16::from(cgram[i * 2 + 1]) << 8;
            let e = |v: u16| ((v & 31) * 255 / 31) as u8;
            out.extend_from_slice(&[e(c >> 10), e(c >> 5), e(c)]);
        }
        out.resize(start + row, 0);
    }
    std::fs::write(path, out).expect("writes the dump");
}
