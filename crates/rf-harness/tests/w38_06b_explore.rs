//! **W38-06b: exploration drive** (ROM-gated, law 5, D-021). Plays the pinned
//! W38-06 script, then the tokens in `$EXPLORE_TOKENS` (a file), dumping the
//! screen every `$DUMP_EVERY` frames and printing the player's position words
//! and the room-state word (all found by the W38-06 finders). Offsets and
//! counts only; screens go to `$EXPLORE_OUT`, outside the repo.

mod w38_06_common;

use rf_core_api::{EmulatorCore, ResetKind, StateError, StateReader, StateWriter};
use rf_snes::state::StateRegion;
use rf_snes::core::SnesCore;
use w38_06_common::*;

struct Out(Vec<u8>);
impl StateWriter for Out {
    fn write_all(&mut self, b: &[u8]) -> Result<(), StateError> {
        self.0.extend_from_slice(b);
        Ok(())
    }
}
struct In<'a>(&'a [u8], usize);
impl StateReader for In<'_> {
    fn read_exact(&mut self, b: &mut [u8]) -> Result<(), StateError> {
        let end = self.1 + b.len();
        b.copy_from_slice(self.0.get(self.1..end).ok_or(StateError::Io("short".into()))?);
        self.1 = end;
        Ok(())
    }
}

/// The machine at the end of the pinned script: restored from `$EXPLORE_STATE`
/// when that file exists, else played and saved (the SNES core's save regions,
/// which is exploration only; every trial still replays from power-on).
fn at_pinned_end(raw: &[u8]) -> SnesCore {
    let mut core = SnesCore::load(raw).expect("loads");
    let file = std::env::var("EXPLORE_STATE").ok();
    if let Some(f) = file.as_ref().filter(|f| std::path::Path::new(f).exists()) {
        let bytes = std::fs::read(f).expect("state");
        let mut r = In(&bytes, 0);
        for reg in StateRegion::ALL {
            core.system_mut().load_region(reg, &mut r).expect("loads a region");
        }
        return core;
    }
    core.reset(ResetKind::Hard);
    for m in script() {
        core.system_mut().bus.joypads.ports[0] = m;
        core.run_frame(&rf_core_api::InputFrame::empty(), &mut NullSink);
    }
    if let Some(f) = file {
        let mut o = Out(Vec::new());
        for reg in StateRegion::ALL {
            core.system().save_region(reg, &mut o).expect("saves a region");
        }
        std::fs::write(f, o.0).expect("writes");
    }
    core
}

#[test]
#[ignore = "needs Super Metroid (RF_SCREENS_ROMS_DIR); exploration aid"]
fn explore() {
    let Some(path) = rom_path() else { return };
    let raw = load_rom(&path);
    let toks = std::fs::read_to_string(std::env::var("EXPLORE_TOKENS").expect("tokens file"))
        .expect("reads");
    let out = std::path::PathBuf::from(std::env::var("EXPLORE_OUT").expect("out dir"));
    std::fs::create_dir_all(&out).expect("dir");
    let every: usize = std::env::var("DUMP_EVERY").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let extra = frames_of(&toks);
    let mut core = at_pinned_end(&raw);
    let w = |c: &SnesCore, a: usize| {
        let r = &c.system().bus.wram;
        u16::from(r[a]) | u16::from(r[a + 1]) << 8
    };
    let all: Vec<u16> = extra.clone();
    for (f, m) in all.iter().enumerate() {
        core.system_mut().bus.joypads.ports[0] = *m;
        let mut shot = Shot::new();
        core.run_frame(&rf_core_api::InputFrame::empty(), &mut shot);
        let k = f;
        let tail = f + 1 == all.len();
        if tail || (every > 0 && k % every == 0) {
            println!(
                "f {k} x {} y {} room {:#x} cam {:#x}",
                w(&core, 0xaf6), w(&core, 0xafa), w(&core, 0x7bb), w(&core, 0xb1)
            );
            let cg: Vec<u8> = core.system().bus.ppu.cgram.iter().flat_map(|c| c.to_le_bytes()).collect();
            write_bmp(&out.join(format!("f{k:06}.bmp")), &shot, &cg);
        }
    }
}
