//! Shared by the W36-10 tests: the SMW Yoshi's Island 1 drive script (the
//! W36-08b/09 session) as pad words, and a few small helpers. Not a test.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use rf_core_api::{CoreEvent, CoreSink, InputFrame, PpuPixel};
use rf_input::SnesButton;

/// Only for the shared script constants.
#[allow(unused_imports)]
#[path = "../w33_07_common/mod.rs"]
pub mod discovery;

pub const SMW_FILE: &str = "Super Mario World (USA).zip";
pub const GAME_MODE: u32 = 0x7E_0100;
pub const IN_LEVEL: u8 = 0x14;
pub const PLAY_FRAMES: usize = 1500;

pub fn roms_root() -> String {
    std::env::var("RF_SCREENS_ROMS_DIR").unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".into())
}

pub fn smw_path() -> Option<PathBuf> {
    let p = Path::new(&roms_root()).join("snes").join(SMW_FILE);
    p.exists().then_some(p)
}

/// `wN` idle N frames, `<buttons>N` hold N frames (`A B X Y S E U D L R`
/// joined by `+`).
pub fn frames_of(text: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for tok in text.split_whitespace() {
        let (mask, n) = if let Some(n) = tok.strip_prefix('w') {
            (0u16, n.parse::<usize>().expect("wN"))
        } else {
            let split = tok.find(|c: char| c.is_ascii_digit()).expect("button+N");
            let (b, n) = tok.split_at(split);
            (button_mask(b), n.parse().expect("N"))
        };
        out.extend(std::iter::repeat_n(mask, n));
    }
    out
}

pub fn button_mask(buttons: &str) -> u16 {
    buttons.split('+').fold(0u16, |m, c| {
        let bit = match c {
            "A" => SnesButton::A.bit(),
            "B" => SnesButton::B.bit(),
            "X" => SnesButton::X.bit(),
            "Y" => SnesButton::Y.bit(),
            "S" => SnesButton::Start.bit(),
            "E" => SnesButton::Select.bit(),
            "U" => SnesButton::Up.bit(),
            "D" => SnesButton::Down.bit(),
            "L" => SnesButton::Left.bit(),
            "R" => SnesButton::Right.bit(),
            other => panic!("button {other}"),
        };
        m | (1 << bit)
    })
}

/// The W36-08/09 session: menus to the level, the play script, then
/// jump-and-run to `PLAY_FRAMES` level frames.
pub fn smw_session() -> Vec<u16> {
    let mut pad = discovery::smw_level_pad();
    let level_start = pad.len();
    pad.extend(frames_of(discovery::SMW_PLAY));
    let more = frames_of("R+B14 R22");
    while pad.len() < level_start + PLAY_FRAMES {
        pad.extend(&more);
    }
    pad
}

pub fn inputs(pad: &[u16]) -> Vec<InputFrame> {
    pad.iter()
        .map(|&mask| {
            let mut f = InputFrame::empty();
            f.ports[0] = mask;
            f
        })
        .collect()
}

pub struct Null;
impl CoreSink for Null {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

/// RAM meaning rows for the SMW recipe: (addr, width, name, where from). Every
/// address is one the SMW profile already records as observed.
pub const SMW_RAM: &[(u32, u8, &str, &str)] = &[
    (0x7E_0100, 1, "game_mode", "profile.toml [[scout.addr]] start_when: stepped 0x00..0x13 through boot, menus and the level load, held 0x14 in a level, 0x0e on the map"),
    (0x7E_0014, 1, "tick_counter", "profile.toml [[memory_map]] tick_counter: +1 on every in-level frame"),
    (0x7E_001A, 2, "camera_x", "profile.toml [camera] x; [[scout.addr]] camera_x: equals BG1HOFS"),
    (0x7E_001C, 2, "camera_y", "profile.toml [camera] y"),
    (0x7E_0094, 2, "player_x", "profile.toml [[scout.addr]] player_x: poking it moves Mario"),
    (0x7E_0096, 2, "player_y", "profile.toml [[scout.addr]] player_y"),
    (0x7E_0071, 1, "player_state", "profile.toml [[scout.addr]] start_when: 0 walking, 9 hit"),
    (0x7E_0057, 1, "level_id", "profile.toml [[scene.level_id]]: 0 on the title and map, non-zero in a level"),
    (0x7E_0DBE, 1, "lives", "profile.toml [[memory_map]] lives (minus one)"),
    (0x7E_0DBF, 1, "coins", "profile.toml [[memory_map]] coins"),
    (0x7E_0F25, 3, "timer_digits", "profile.toml [[memory_map]] timer_digits"),
];
