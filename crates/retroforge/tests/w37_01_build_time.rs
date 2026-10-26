//! **W37-01: how long the background atlas build takes** (ROM-gated, law 5;
//! ignored, release). The core thread never waits for it (the build runs on its
//! own thread and the game keeps going on the interpreter), so this is the time
//! until the first atlas / the next swap, measured as wall time of
//! `retroforge::rebuild::build_for`, the function the background job calls.
//!
//! For each cartridge: the vectors-only atlas (the first atlas of a cold
//! launch), then the atlas rebuilt from a recording of the first frames of
//! the game (what a grow job does). Only sizes and times are printed (D-021).
//!
//! ```text
//! cargo test --release -p retroforge --test w37_01_build_time -- --ignored --nocapture
//! ```

use std::path::Path;
use std::time::Instant;

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_nes::core::NesCore;
use rf_snes::core::SnesCore;

struct Null;
impl CoreSink for Null {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn rom(console: &str, file: &str) -> Option<Vec<u8>> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = Path::new(&root).join(console).join(file);
    p.exists()
        .then(|| retroforge::rom_open::load_rom_bytes(&p).expect("rom loads"))
}

fn measure(label: &str, raw: &[u8], record: impl FnOnce(&[u8]) -> rf_core_api::CoverageLog) {
    let t = Instant::now();
    let first = retroforge::rebuild::build_for(raw, &rf_core_api::CoverageLog::new(0)).expect("ok");
    let first_t = t.elapsed();
    let t = Instant::now();
    let log = record(raw);
    let rec_t = t.elapsed();
    let t = Instant::now();
    let grown = retroforge::rebuild::build_for(raw, &log).expect("ok");
    let grown_t = t.elapsed();
    println!(
        "{label}: image {} KiB\n  vectors-only atlas: {} functions, {} insns, built in {first_t:?}\n  \
         recorded {} instructions ({} PRG bytes executed) in {rec_t:?}\n  \
         atlas from that recording: {} functions, {} observed + {} inferred insns, built in {grown_t:?}",
        raw.len() / 1024,
        first.functions.len(),
        first.stats.observed_insns + first.stats.inferred_insns,
        log.instructions_recorded,
        log.executed_prg_bytes(),
        grown.functions.len(),
        grown.stats.observed_insns,
        grown.stats.inferred_insns,
    );
}

fn record_snes(raw: &[u8], frames: u32) -> rf_core_api::CoverageLog {
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.set_coverage_recording(true);
    for f in 0..frames {
        // Start now and then: past title screens into whatever follows.
        core.system_mut().bus.joypads.ports[0] = if f % 90 < 6 { 1 << 12 } else { 0 };
        core.run_frame(&InputFrame::empty(), &mut Null);
    }
    core.take_coverage().expect("armed")
}

#[test]
#[ignore = "needs ROMs (RF_SCREENS_ROMS_DIR); a minute in release"]
fn background_atlas_build_times() {
    let mut any = false;
    if let Some(raw) = rom("nes", "Super Mario Bros. + Duck Hunt (USA).zip") {
        any = true;
        measure("SMB + Duck Hunt (NES, mapper 66)", &raw, |raw| {
            let mut core = NesCore::from_ines_bytes(raw).expect("loads");
            core.set_coverage_recording(true);
            core.reset(ResetKind::Hard);
            for f in 0..1200u32 {
                let mut i = InputFrame::empty();
                i.ports[0] = match f {
                    200..=204 | 400..=404 => 1 << 3,
                    f if f > 500 => 1 << 7,
                    _ => 0,
                };
                core.run_frame(&i, &mut Null);
            }
            core.take_coverage().expect("armed")
        });
    }
    for (file, label) in [
        ("Super Mario World (USA).zip", "SMW (SNES, 512 KiB)"),
        ("Killer Instinct (USA).zip", "Killer Instinct (SNES, 4 MiB)"),
        ("Mortal Kombat 3 (USA).zip", "Mortal Kombat 3 (SNES, 4 MiB)"),
    ] {
        if let Some(raw) = rom("snes", file) {
            any = true;
            measure(label, &raw, |raw| record_snes(raw, 3000));
        }
    }
    if !any {
        println!("SKIP: no ROMs found");
    }
}
