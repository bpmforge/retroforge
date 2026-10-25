//! Ticket W36-05: a lockstep repro bundle's savestate loads through the app's
//! own loader (`retroforge::save_state`), not only through the harness's
//! copy of the region-tag table.
//!
//! The tag table cannot live in a crate both sides see: `rf-state` may not
//! depend on `rf-nes` and `rf-nes` may not depend on `rf-state`
//! (`scripts/validate-arch.sh` rules 1 and 3). So the duplicate stays in
//! `rf-harness` and this test is what keeps it honest: it forces a
//! divergence, takes the bundle's `.rfstate`, applies it with
//! `apply_container` to a fresh core and requires the result to be the plain
//! core's machine at the start of the diverging frame.

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::atlas::build_nes_atlas;
use rf_harness::lockstep::{run, Options, Side};
use rf_nes::core::NesCore;
use rf_nes::CpuBus as _;
use rf_state::Container;

/// NROM-128: `LDX #0; loop: INC $10; INX; STX $11; JMP loop`, reset -> $8000.
fn tiny_ines() -> Vec<u8> {
    let mut d = vec![0u8; 16];
    d[0..4].copy_from_slice(b"NES\x1a");
    d[4] = 1;
    d[5] = 1;
    let mut prg = vec![0xEAu8; 0x4000];
    prg[..9].copy_from_slice(&[0xA2, 0x00, 0xE6, 0x10, 0xE8, 0x86, 0x11, 0x4C, 0x02]);
    prg[9] = 0x80;
    prg[0x3FFC] = 0x00;
    prg[0x3FFD] = 0x80;
    d.extend(prg);
    d.extend(vec![0u8; 0x2000]);
    d
}

struct Sink;
impl CoreSink for Sink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn inputs(n: usize) -> Vec<InputFrame> {
    (0..n)
        .map(|i| {
            let mut f = InputFrame::empty();
            f.ports[0] = (i % 7) as u16;
            f
        })
        .collect()
}

#[test]
fn a_lockstep_bundle_state_loads_through_the_apps_own_loader() {
    const DIVERGE: u32 = 9;
    let ines = tiny_ines();
    let mut rec = NesCore::from_ines_bytes(&ines).expect("loads");
    rec.set_coverage_recording(true);
    rec.reset(ResetKind::Hard);
    for _ in 0..4 {
        rec.run_frame(&InputFrame::empty(), &mut Sink);
    }
    let atlas = build_nes_atlas(&ines, &rec.take_coverage().expect("armed")).expect("atlas");

    let dir = std::env::temp_dir().join(format!("rf-w36-05-bundle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut poke = |frame: u32, side: Side, core: &mut NesCore| {
        if frame == DIVERGE && side == Side::Rebuilt {
            core.bus_mut().write(0x0300, 0x5A);
        }
    };
    let opts = Options {
        bundle_dir: Some(dir.clone()),
        before_frame: Some(&mut poke),
        ..Options::default()
    };
    let report = run(&ines, &atlas, inputs(30), opts).expect("runs");
    assert_eq!(report.divergence.expect("diverges").frame, DIVERGE);

    let bytes = std::fs::read(dir.join("plain_state_before_frame.rfstate")).expect("bundle state");
    let (container, _) =
        Container::decode(&bytes, &retroforge::save_state::nes_migrations()).expect("decodes");

    // The plain machine at the start of the diverging frame, replayed.
    let mut plain = NesCore::from_ines_bytes(&ines).expect("loads");
    plain.reset(ResetKind::Hard);
    for input in inputs(30).iter().take(DIVERGE as usize) {
        plain.run_frame(input, &mut Sink);
    }

    let mut fresh = NesCore::from_ines_bytes(&ines).expect("loads");
    fresh.reset(ResetKind::Hard);
    let (cpu, bus) = fresh.parts_mut();
    retroforge::save_state::apply_container(cpu, bus, &container)
        .expect("the app's loader accepts the bundle state");
    assert_eq!(fresh.bus().ram(), plain.bus().ram(), "work RAM");
    assert_eq!(fresh.bus().frame_count(), plain.bus().frame_count());
    assert_eq!(fresh.bus().master_cycle(), plain.bus().master_cycle());
    assert_eq!(fresh.state_view().cpu_regs, plain.state_view().cpu_regs);
    let _ = std::fs::remove_dir_all(&dir);
}
