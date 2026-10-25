//! Ticket W36-09: `run_frame_observed` on the SNES core, over a small
//! synthetic LoROM. The observed frame must leave the machine bit-identical
//! to `run_frame`, name each instruction's atlas function, log the CPU-bus
//! accesses with their values (24-bit addresses, writes included) and leave
//! the access log off afterwards.

use rf_core_api::{
    BusAccess, CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind, StepFacts,
    StepObserver,
};
use rf_rebuild::Atlas;
use rf_snes::core::SnesCore;

fn put(rom: &mut [u8], addr: usize, code: &[u8]) {
    rom[addr - 0x8000..addr - 0x8000 + code.len()].copy_from_slice(code);
}

/// `JSR pal; INC $10; JMP loop` (emulation mode); `pal` writes CGADD
/// (`$2121`) then two CGDATA (`$2122`) bytes.
fn rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x1_0000];
    let base = 0x7FC0;
    rom[base + 0x15] = 0x20;
    rom[base + 0x17] = 6;
    let sum: u16 = 0xBEEF;
    rom[base + 0x1C..base + 0x1E].copy_from_slice(&(sum ^ 0xFFFF).to_le_bytes());
    rom[base + 0x1E..base + 0x20].copy_from_slice(&sum.to_le_bytes());
    rom[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    put(
        &mut rom,
        0x8000,
        &[0x20, 0x20, 0x80, 0xE6, 0x10, 0x4C, 0x00, 0x80],
    );
    put(
        &mut rom,
        0x8020,
        &[
            0xA9, 0x80, 0x8D, 0x21, 0x21, // LDA #$80 ; STA $2121
            0xA9, 0x1F, 0x8D, 0x22, 0x21, // LDA #$1F ; STA $2122
            0xA9, 0x03, 0x8D, 0x22, 0x21, // LDA #$03 ; STA $2122
            0x60,
        ],
    );
    rom
}

struct Quiet;
impl CoreSink for Quiet {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn atlas_of(raw: &[u8]) -> Atlas {
    let mut rec = SnesCore::load(raw).expect("loads");
    rec.reset(ResetKind::Hard);
    rec.set_coverage_recording(true);
    for _ in 0..2 {
        rec.run_frame(&InputFrame::empty(), &mut Quiet);
    }
    let log = rec.take_coverage().expect("armed");
    rf_snes::rebuild::build_atlas_from_image(raw, &log).expect("atlas")
}

#[derive(Default)]
struct Collect {
    frames: u32,
    steps: u64,
    reg_writes: Vec<(Option<u32>, BusAccess)>,
}
impl StepObserver for Collect {
    fn step(&mut self, facts: StepFacts, accesses: &[BusAccess]) {
        self.steps += 1;
        for a in accesses {
            if a.write && (a.addr & 0xFFFF) == 0x2122 {
                self.reg_writes.push((facts.function, *a));
            }
        }
    }
    fn frame_end(&mut self) {
        self.frames += 1;
    }
}

fn core(raw: &[u8], atlas: &Atlas) -> SnesCore {
    let mut c = SnesCore::load(raw).expect("loads");
    c.reset(ResetKind::Hard);
    c.set_rebuild(Some(atlas.clone())).expect("atlas fits");
    c
}

fn fingerprint(c: &SnesCore) -> u64 {
    let v = c.state_view();
    [v.wram, v.vram, v.cgram, v.oam]
        .iter()
        .flat_map(|s| s.iter())
        .fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
}

#[test]
fn observed_frame_is_run_frame() {
    let raw = rom();
    let atlas = atlas_of(&raw);
    let (mut plain, mut watched) = (core(&raw, &atlas), core(&raw, &atlas));
    let mut obs = Collect::default();
    for _ in 0..3 {
        plain.run_frame(&InputFrame::empty(), &mut Quiet);
        assert!(watched.run_frame_observed(&InputFrame::empty(), &mut Quiet, &mut obs));
    }
    assert_eq!(fingerprint(&plain), fingerprint(&watched));
    assert_eq!(plain.peek(0x10), watched.peek(0x10));
    assert_eq!(plain.rebuild_stats(), watched.rebuild_stats());
    assert_eq!(obs.frames, 3);
    assert!(obs.steps > 100, "the program ran: {}", obs.steps);
    watched.run_frame(&InputFrame::empty(), &mut Quiet);
    assert!(watched.system().bus.access_log().is_empty());
    assert!(!watched.config().watches.observes());
}

#[test]
fn register_writes_carry_their_values_and_the_executing_function() {
    let raw = rom();
    let atlas = atlas_of(&raw);
    let pal = atlas
        .functions
        .iter()
        .position(|f| f.entry_rom == 0x20)
        .expect("palette routine") as u32;
    let mut c = core(&raw, &atlas);
    let mut obs = Collect::default();
    assert!(c.run_frame_observed(&InputFrame::empty(), &mut Quiet, &mut obs));
    let values: Vec<u8> = obs
        .reg_writes
        .iter()
        .map(|(_, a)| a.value)
        .take(4)
        .collect();
    assert_eq!(values, [0x1F, 0x03, 0x1F, 0x03]);
    assert!(obs.reg_writes.iter().all(|(f, _)| *f == Some(pal)));
}
