//! W37-00c: the Super Mario World menu-to-level drive, waiting on observed
//! game state instead of fixed frame counts. Not a test target itself (cargo
//! only builds `tests/*.rs` and `tests/*/main.rs`); included by the SMW
//! ROM-gated tests with `mod smw_drive;` (or a `#[path]` from another crate).
//!
//! The drive is closed-loop: the caller supplies `step(mask) -> game_mode`,
//! which runs one frame with that pad and returns the game-mode byte
//! (`$7E0100`) after it. Waits end when the byte reaches a value, with a frame
//! cap and a panic that names the stage, so a timing change in the core can
//! shift WHEN the game reaches a state but cannot silently strand a script.
//! The drive returns the pad it used, frame by frame, so a test that replays
//! the pad on a second machine (lockstep, trace, atlas) gets the same inputs.
//!
//! Only two mode values are used, both already observed facts of the merged
//! tests: `0x0E` the overworld map and `0x14` a level running (D-021).
#![allow(dead_code)]

use rf_input::SnesButton;

/// The game-mode byte's address.
pub const MODE: u32 = 0x7E_0100;
/// `$7E0100` while the overworld map is up.
pub const MAP: u8 = 0x0E;
/// `$7E0100` while a level runs.
pub const IN_LEVEL: u8 = 0x14;

/// Frames the game may take to reach the map or a level after the input that
/// leads there (the slowest observed is about 300; the cap is for a stuck game).
pub const STATE_CAP: usize = 1_500;

pub fn mask(buttons: &[SnesButton]) -> u16 {
    buttons.iter().fold(0, |m, b| m | (1 << b.bit()))
}

/// A recording pad driver over a `step` closure.
pub struct Drive<'a> {
    step: &'a mut dyn FnMut(u16) -> u8,
    /// Every mask fed so far, one per frame.
    pub pad: Vec<u16>,
    /// The mode after the last frame.
    pub mode: u8,
}

impl<'a> Drive<'a> {
    pub fn new(step: &'a mut dyn FnMut(u16) -> u8) -> Self {
        Self {
            step,
            pad: Vec::new(),
            mode: 0,
        }
    }

    /// One frame with `m` held.
    pub fn frame(&mut self, m: u16) -> u8 {
        self.pad.push(m);
        self.mode = (self.step)(m);
        self.mode
    }

    pub fn idle(&mut self, n: usize) {
        for _ in 0..n {
            self.frame(0);
        }
    }

    pub fn hold(&mut self, buttons: &[SnesButton], n: usize) {
        let m = mask(buttons);
        for _ in 0..n {
            self.frame(m);
        }
    }

    /// Hold `buttons` until the mode byte is `want` (the frame that shows it
    /// is the last one held); panics after `cap` frames.
    pub fn hold_until(&mut self, buttons: &[SnesButton], want: u8, cap: usize, stage: &str) {
        let m = mask(buttons);
        for _ in 0..cap {
            if self.frame(m) == want {
                return;
            }
        }
        self.stuck(stage, want, cap);
    }

    /// Idle until the mode byte is `want`; panics after `cap` frames.
    pub fn idle_until(&mut self, want: u8, cap: usize, stage: &str) {
        self.hold_until(&[], want, cap, stage);
    }

    fn stuck(&self, stage: &str, want: u8, cap: usize) -> ! {
        panic!(
            "SMW drive stuck at '{stage}': game mode never reached {want:#04x} within {cap} frames \
             (frame {}, mode {:#04x}); the menu script no longer reaches the game",
            self.pad.len(),
            self.mode
        )
    }
}

/// Boot, the title and file-select taps, the first (introduction) level, the
/// map at Yoshi's House, into the house and out of its right end, left to
/// Yoshi's Island 1, in; returns once that level's first frames have run. The
/// fixed offsets after a state are the margins the frame-timed script
/// `w200 S6 w108 A6 ... w158` had (the game needs them to settle: the
/// fade-in, the map's walk-on), now counted from the observed state.
pub fn to_level(step: &mut dyn FnMut(u16) -> u8) -> Vec<u16> {
    use SnesButton::{Left, Right, Start, A};
    let mut d = Drive::new(step);
    d.idle(200);
    for i in 0..10 {
        d.hold(&[if i % 2 == 0 { Start } else { A }], 6);
        d.idle(108);
    }
    // The introduction level ends on this press; the map follows.
    d.idle(300);
    d.hold(&[A], 8);
    d.idle_until(MAP, STATE_CAP, "map after the introduction level");
    // Yoshi's House, entered; the house is walked out of at its right end.
    d.idle(286);
    d.hold(&[A], 8);
    d.idle_until(IN_LEVEL, STATE_CAP, "Yoshi's House");
    d.idle(56);
    for _ in 0..3 {
        d.hold(&[Right], 40);
        d.idle(20);
        d.hold(&[Left], 40);
        d.idle(20);
    }
    d.hold(&[Right], 60);
    d.idle(20);
    d.hold_until(&[Right], MAP, STATE_CAP, "map after Yoshi's House");
    d.hold(&[Right], 26);
    // Left to Yoshi's Island 1, and in.
    d.idle(300);
    d.hold(&[Left], 8);
    d.idle(158);
    d.hold(&[A], 8);
    d.idle_until(IN_LEVEL, STATE_CAP, "Yoshi's Island 1");
    d.idle(4);
    d.pad
}

/// The short route used by the `reach_yi1` copies of the scout and interpolation
/// tests: the same boot and menu taps, one A on the introduction level, then straight from the
/// map at Yoshi's House left to Yoshi's Island 1 (no walk through the house); returns
/// once that level's first frames have run. Margins as in [`to_level`], counted from the
/// observed state.
pub fn to_yi1_direct(step: &mut dyn FnMut(u16) -> u8) -> Vec<u16> {
    use SnesButton::{Left, Start, A};
    let mut d = Drive::new(step);
    d.idle(200);
    for i in 0..10 {
        d.hold(&[if i % 2 == 0 { Start } else { A }], 6);
        d.idle(108);
    }
    d.idle(300);
    d.hold(&[A], 8);
    d.idle_until(MAP, STATE_CAP, "map after the introduction level");
    d.idle(286);
    d.hold(&[Left], 8);
    d.idle(158);
    d.hold(&[A], 8);
    d.idle_until(IN_LEVEL, STATE_CAP, "Yoshi's Island 1");
    d.idle(4);
    d.pad
}

struct NullSink;
impl rf_core_api::CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _p: &[rf_core_api::PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: rf_core_api::CoreEvent) {}
}

/// [`to_level`] on a scratch machine from a hard reset: the pad that takes
/// SMW from power-on to the first frames of Yoshi's Island 1. The core is
/// deterministic, so any machine fed this pad from a hard reset follows the
/// same states (plain, rebuilt, coverage-recording).
pub fn level_pad(raw: &[u8]) -> Vec<u16> {
    use rf_core_api::{EmulatorCore as _, InputFrame, ResetKind};
    let mut core = rf_snes::core::SnesCore::load(raw).expect("SMW loads");
    core.reset(ResetKind::Hard);
    to_level(&mut |m| {
        core.system_mut().bus.joypads.ports[0] = m;
        core.run_frame(&InputFrame::empty(), &mut NullSink);
        core.peek(MODE)
    })
}
