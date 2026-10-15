//! Ticket W30-08: the extra vblank lines setting (`NesBus::
//! set_extra_vblank_lines`), on a synthetic NROM whose main loop spins a
//! counter and whose NMI handler counts frames.

use rf_core_api::{CoreSink, EmulatorCore, PpuPixel, StateError, StateWriter, Step};

use super::build_nrom_ines;
use crate::core::NesCore;
use crate::state::StateRegion;

/// `$C000` reset: NMI on, then `main: INC $00 ; BNE main ; INC $01 ; JMP
/// main`. `$C010` NMI: `INC $02 ; RTI`. A 16-bit counter at `$00/$01` shows
/// how much main-loop work fits in a frame; `$02` counts NMIs.
fn rom() -> Vec<u8> {
    build_nrom_ines(1, 1, |_| 0)
        .into_iter()
        .enumerate()
        .map(|(i, b)| match i.checked_sub(16) {
            // PRG starts after the 16-byte header; the 16 KiB bank is
            // mirrored at $8000 and $C000, so offset 0 is $C000.
            Some(o) => match o {
                0x0000 => 0xA9, // LDA #$80
                0x0001 => 0x80,
                0x0002 => 0x8D, // STA $2000
                0x0003 => 0x00,
                0x0004 => 0x20,
                0x0005 => 0xE6, // main: INC $00
                0x0006 => 0x00,
                0x0007 => 0xD0, // BNE main
                0x0008 => 0xFC,
                0x0009 => 0xE6, // INC $01
                0x000A => 0x01,
                0x000B => 0x4C, // JMP main
                0x000C => 0x05,
                0x000D => 0xC0,
                0x0010 => 0xE6, // nmi: INC $02
                0x0011 => 0x02,
                0x0012 => 0x40, // RTI
                0x3FFA => 0x10, // NMI vector -> $C010
                0x3FFB => 0xC0,
                0x3FFC => 0x00, // reset vector -> $C000
                0x3FFD => 0xC0,
                _ => b,
            },
            None => b,
        })
        .collect()
}

#[derive(Default)]
struct Sink {
    video: u64,
    audio: u64,
    audio_samples: usize,
}

const BASIS: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

impl CoreSink for Sink {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        let mut h = self.video ^ u64::from(y);
        for p in pixels {
            h = (h ^ u64::from(p.palette_index)).wrapping_mul(0x100_0000_01b3);
        }
        self.video = h;
    }
    fn audio(&mut self, samples: &[i16]) {
        self.audio_samples += samples.len();
        for s in samples {
            self.audio = fnv(self.audio, &s.to_le_bytes());
        }
    }
    fn event(&mut self, _ev: rf_core_api::CoreEvent) {}
}

struct Bytes(Vec<u8>);
impl StateWriter for Bytes {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), StateError> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Run {
    state: u64,
    video: u64,
    audio: u64,
    audio_samples: usize,
    nmis: u8,
    counter: u32,
    frames: u64,
    master_cycles: u64,
}

fn run(extra: Option<u16>, frames: u32) -> Run {
    let mut core = NesCore::from_ines_bytes(&rom()).expect("synthetic NROM loads");
    if let Some(n) = extra {
        core.set_extra_vblank_lines(n);
    }
    let mut sink = Sink {
        video: BASIS,
        audio: BASIS,
        ..Sink::default()
    };
    // The 16-bit counter wraps within a few frames; total the per-frame deltas.
    let (mut counter, mut last) = (0u32, 0u16);
    for _ in 0..frames {
        let _ = core.step(Step::Frame, &mut sink);
        let ram = core.bus().ram();
        let now = u16::from_le_bytes([ram[0], ram[1]]);
        counter += u32::from(now.wrapping_sub(last));
        last = now;
    }
    let mut state = BASIS;
    for region in StateRegion::ALL {
        let mut w = Bytes(Vec::new());
        core.bus()
            .save_region(core.cpu(), region, &mut w)
            .expect("save region");
        state = fnv(state, &w.0);
    }
    let ram = core.bus().ram();
    Run {
        state,
        video: sink.video,
        audio: sink.audio,
        audio_samples: sink.audio_samples,
        nmis: ram[2],
        counter,
        frames: core.bus().frame_count(),
        master_cycles: core.bus().master_cycle(),
    }
}

const FRAMES: u32 = 60;

/// Default off is the machine as it was before the setting existed — the
/// values below were captured on the tree before W30-08 touched `rf-nes`.
#[test]
fn default_is_byte_identical_to_the_pre_setting_build() {
    let got = run(None, FRAMES);
    assert_eq!(
        (
            got.state,
            got.video,
            got.audio,
            got.audio_samples,
            got.nmis,
            got.counter,
            got.frames,
            got.master_cycles
        ),
        PINNED_DEFAULT
    );
    assert_eq!(run(Some(0), FRAMES), got, "explicit 0 is the same machine");
}

const PINNED_DEFAULT: (u64, u64, u64, usize, u8, u32, u64, u64) = (
    1319139821921061609,
    16503727943581147941,
    1364737523038005955,
    47125,
    59,
    218765,
    60,
    1757173,
);

#[test]
fn setting_clamps_to_zero_through_240() {
    let mut core = NesCore::from_ines_bytes(&rom()).expect("loads");
    assert_eq!(core.extra_vblank_lines(), 0);
    assert_eq!(core.set_extra_vblank_lines(40), 40);
    assert_eq!(core.set_extra_vblank_lines(9999), 240);
}

/// The point of the feature: more main-loop work per frame, and nothing
/// else that a game or the speaker can tell: one NMI per frame, the same
/// number of frames, the same audio sample count (the APU is frozen during
/// the extra lines, so pitch and tempo cannot change).
#[test]
fn extra_lines_give_more_cpu_time_but_the_same_frames_nmis_and_audio() {
    let base = run(None, FRAMES);
    let extra = run(Some(40), FRAMES);
    assert_eq!(extra.frames, base.frames);
    assert_eq!(extra.nmis, base.nmis, "NMIs per 60 frames");
    // The first frame after power-on is the pre-render line's wrap, before
    // vblank: 60 frames carry 59 NMIs, with or without the setting.
    assert_eq!(base.nmis, 59);
    assert!(
        extra.audio_samples.abs_diff(base.audio_samples) <= 1,
        "audio samples {} vs {}",
        base.audio_samples,
        extra.audio_samples
    );
    // 40 lines = 40*341/3 = 4547 of ~29780 cycles: +15% CPU time.
    let ratio = f64::from(extra.counter) / f64::from(base.counter);
    assert!(
        ratio > 1.10,
        "counter {} -> {} is only {ratio:.3}x",
        base.counter,
        extra.counter
    );
    // The CPU clock really ran extra: more master cycles for the same frames.
    assert!(extra.master_cycles > base.master_cycles + 59 * 4000);
}

/// More lines, more time, monotonically.
#[test]
fn more_lines_means_more_work() {
    let counters: Vec<u32> = [0u16, 20, 80, 240]
        .into_iter()
        .map(|n| run(Some(n), 30).counter)
        .collect();
    assert!(counters.windows(2).all(|w| w[0] < w[1]), "{counters:?}");
}
