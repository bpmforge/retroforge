//! **W38-06b: exploration drive** (ROM-gated, law 5, D-021). Plays the pinned
//! W38-06 script, then the tokens in `$EXPLORE_TOKENS` (a file), dumping the
//! screen every `$DUMP_EVERY` frames and printing the player's position words
//! and the room-state word (all found by the W38-06 finders). Offsets and
//! counts only; screens go to `$EXPLORE_OUT`, outside the repo.

mod w38_06_common;

use rf_core_api::{EmulatorCore, ResetKind};
use rf_snes::core::SnesCore;
use w38_06_common::*;

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
    let base = script();
    let extra = frames_of(&toks);
    let mut core = SnesCore::load(&raw).expect("loads");
    core.reset(ResetKind::Hard);
    let w = |c: &SnesCore, a: usize| {
        let r = &c.system().bus.wram;
        u16::from(r[a]) | u16::from(r[a + 1]) << 8
    };
    let all: Vec<u16> = base.iter().chain(&extra).copied().collect();
    for (f, m) in all.iter().enumerate() {
        core.system_mut().bus.joypads.ports[0] = *m;
        let mut shot = Shot::new();
        core.run_frame(&rf_core_api::InputFrame::empty(), &mut shot);
        let k = f.wrapping_sub(base.len());
        let tail = f + 1 == all.len();
        if f >= base.len() && (tail || (every > 0 && k % every == 0)) {
            println!(
                "f {k} x {} y {} room {:#x} cam {:#x}",
                w(&core, 0xaf6), w(&core, 0xafa), w(&core, 0x7bb), w(&core, 0xb1)
            );
            let cg: Vec<u8> = core.system().bus.ppu.cgram.iter().flat_map(|c| c.to_le_bytes()).collect();
            write_bmp(&out.join(format!("f{k:06}.bmp")), &shot, &cg);
        } else if f + 1 == base.len() {
            println!("end of pinned script: x {} y {} room {:#x}", w(&core, 0xaf6), w(&core, 0xafa), w(&core, 0x7bb));
        }
    }
}
