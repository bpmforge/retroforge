//! Replaying an imported [`Movie`](crate::movie::Movie) headlessly with the
//! coverage recorder armed (ticket W37-00).
//!
//! This module only **drives**: it builds the frame list ([`plan`]), runs a
//! core over it and hands each finished frame to an observer. What counts as
//! "the run still matches the movie" is game knowledge (RAM addresses), so the
//! observer is supplied by the caller (the ROM-gated test), never decided
//! here.
//!
//! ## Lining a movie up with a cartridge it was not made on
//!
//! A movie starts at a console power-on. A multicart (SMB + Duck Hunt) boots
//! to a menu first, so [`plan`] puts a caller-supplied `lead` (the menu
//! drive), a `delay` of idle frames and then the movie, optionally skipping
//! its first `skip` frames (a negative offset, only sensible when those
//! frames are idle). The caller sweeps `delay`/`skip` and keeps the best.
//!
//! A movie's frame-0 reset is its way of saying "power on"; the core has
//! just been powered on (or the lead-in did it), so it is not applied again.
//! Resets in later frames are.

use rf_core_api::coverage::CoverageLog;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_nes::core::NesCore;
use rf_snes::core::SnesCore;

use crate::movie::{Movie, MovieFrame, Reset};

/// The frames a replay runs, and where the movie starts inside them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub frames: Vec<MovieFrame>,
    /// Index in `frames` of the first movie frame that was kept.
    pub movie_start: usize,
    /// Movie frames dropped from the front.
    pub skipped: usize,
}

impl Plan {
    /// The movie frame number that runs at plan frame `f`, if `f` is inside
    /// the movie part.
    #[must_use]
    pub fn movie_frame_at(&self, f: usize) -> Option<usize> {
        f.checked_sub(self.movie_start).map(|m| m + self.skipped)
    }
}

/// `lead` pad words on port 0, `delay` idle frames, then `movie` from frame
/// `skip` on (module doc).
#[must_use]
pub fn plan(lead: &[u16], delay: usize, skip: usize, movie: &Movie) -> Plan {
    let mut frames: Vec<MovieFrame> = lead
        .iter()
        .map(|&w| MovieFrame {
            pads: [w, 0],
            reset: None,
        })
        .collect();
    frames.extend(std::iter::repeat_n(MovieFrame::default(), delay));
    let movie_start = frames.len();
    for (i, f) in movie.frames.iter().enumerate().skip(skip) {
        let mut f = *f;
        if i == 0 {
            f.reset = None;
        }
        frames.push(f);
    }
    Plan {
        frames,
        movie_start,
        skipped: skip,
    }
}

/// What a replay did.
pub struct Replay {
    /// Frames run (the observer may have stopped it early).
    pub frames_run: u32,
    /// `true` when the observer ended the run before the plan did.
    pub stopped: bool,
    /// The recording, when it was armed.
    pub coverage: Option<CoverageLog>,
}

struct Null;

impl CoreSink for Null {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn reset_kind(r: Reset) -> ResetKind {
    match r {
        Reset::Soft => ResetKind::Soft,
        Reset::Hard => ResetKind::Hard,
    }
}

/// Run `plan` on `core`: reset, latch, run a frame, call `observe(frame,
/// core)`; stop when it returns `false`. `latch` puts the pads where the
/// console reads them. The loop is a `for` over the plan, so it ends.
fn drive<C: EmulatorCore>(
    core: &mut C,
    plan: &Plan,
    record: bool,
    latch: impl Fn(&mut C, &InputFrame),
    mut observe: impl FnMut(u32, &C) -> bool,
) -> Replay {
    core.set_coverage_recording(record);
    core.reset(ResetKind::Hard);
    let mut sink = Null;
    let mut frames_run = 0u32;
    let mut stopped = false;
    for (n, f) in (0u32..).zip(&plan.frames) {
        if let Some(r) = f.reset {
            core.reset(reset_kind(r));
        }
        let mut input = InputFrame::empty();
        input.ports[0] = f.pads[0];
        input.ports[1] = f.pads[1];
        latch(core, &input);
        core.run_frame(&input, &mut sink);
        frames_run = n + 1;
        if !observe(n, core) {
            stopped = true;
            break;
        }
    }
    Replay {
        frames_run,
        stopped,
        coverage: if record { core.take_coverage() } else { None },
    }
}

/// Replay on an iNES image. `record` arms the coverage recorder.
///
/// # Errors
/// The image does not load.
pub fn replay_nes(
    raw: &[u8],
    plan: &Plan,
    record: bool,
    observe: impl FnMut(u32, &NesCore) -> bool,
) -> Result<Replay, String> {
    let mut core = NesCore::from_ines_bytes(raw).map_err(|e| format!("{e:?}"))?;
    Ok(drive(&mut core, plan, record, |_, _| {}, observe))
}

/// Replay on a SNES image. The core ignores `InputFrame` here, so the pad
/// words are latched into the joypad ports, as the app does
/// (`understand::run_snes`).
///
/// # Errors
/// The image does not load.
pub fn replay_snes(
    raw: &[u8],
    plan: &Plan,
    record: bool,
    observe: impl FnMut(u32, &SnesCore) -> bool,
) -> Result<Replay, String> {
    let mut core = SnesCore::load(raw).map_err(|e| e.to_string())?;
    let latch = |c: &mut SnesCore, input: &InputFrame| {
        let ports = &mut c.system_mut().bus.joypads.ports;
        for (slot, word) in ports.iter_mut().zip(input.ports) {
            *slot = word;
        }
    };
    Ok(drive(&mut core, plan, record, latch, observe))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movie::{Console, Format};

    fn movie(pads: &[u16], resets: &[(usize, Reset)]) -> Movie {
        Movie {
            format: Format::Fm2,
            console: Console::Nes,
            frames: pads
                .iter()
                .enumerate()
                .map(|(i, &p)| MovieFrame {
                    pads: [p, 0],
                    reset: resets.iter().find(|r| r.0 == i).map(|r| r.1),
                })
                .collect(),
            rerecords: None,
            rom_hint: None,
            dropped_subframes: 0,
        }
    }

    #[test]
    fn plan_lays_out_lead_delay_and_movie_and_drops_the_power_on_reset() {
        let m = movie(&[1, 2, 3], &[(0, Reset::Soft), (2, Reset::Soft)]);
        let p = plan(&[8, 8], 3, 0, &m);
        assert_eq!(p.frames.len(), 2 + 3 + 3);
        assert_eq!(p.movie_start, 5);
        assert_eq!(p.frames[0].pads[0], 8);
        assert_eq!(p.frames[3], MovieFrame::default());
        assert_eq!(p.frames[5].pads[0], 1);
        assert_eq!(p.frames[5].reset, None, "frame 0 reset is the power-on");
        assert_eq!(p.frames[7].reset, Some(Reset::Soft), "later ones stay");
        assert_eq!(p.movie_frame_at(6), Some(1));
        assert_eq!(p.movie_frame_at(1), None);
    }

    #[test]
    fn skip_drops_the_front_of_the_movie() {
        let m = movie(&[1, 2, 3], &[]);
        let p = plan(&[], 0, 2, &m);
        assert_eq!(p.frames.len(), 1);
        assert_eq!(p.frames[0].pads[0], 3);
        assert_eq!(p.movie_frame_at(0), Some(2));
        assert_eq!(plan(&[], 0, 99, &m).frames.len(), 0);
    }
}
