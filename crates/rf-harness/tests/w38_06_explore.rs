//! **W38-06 exploration** (ROM-gated, law 5): runs a token script on Super
//! Metroid and prints a few RAM words and a dump every N frames, so the play
//! can be found by watching. `RF_SCRIPT` tokens, `RF_SHOTS` output dir,
//! `RF_EVERY` frames between dumps. Only numbers are printed (D-021).
//!
//! ```text
//! RF_SCRIPT="w300 S6" cargo test --release -p rf-harness --test w38_06_explore -- --ignored --nocapture
//! ```

mod w38_06_common;

use rf_core_api::{EmulatorCore, ResetKind, StateError, StateReader, StateWriter};
use rf_snes::core::SnesCore;
use rf_snes::state::StateRegion;

const REGIONS: [StateRegion; 9] = [
    StateRegion::Cpu,
    StateRegion::Ppu,
    StateRegion::Apu,
    StateRegion::Wram,
    StateRegion::Vram,
    StateRegion::Oam,
    StateRegion::Cgram,
    StateRegion::Mapper,
    StateRegion::Cart,
];

use w38_06_common::*;

/// A byte buffer as a state stream (the scratch state files of the exploration).
#[derive(Default)]
struct Mem {
    data: Vec<u8>,
    at: usize,
}

impl StateWriter for Mem {
    fn write_all(&mut self, buf: &[u8]) -> Result<(), StateError> {
        self.data.extend_from_slice(buf);
        Ok(())
    }
}

impl StateReader for Mem {
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), StateError> {
        let end = self.at + buf.len();
        buf.copy_from_slice(self.data.get(self.at..end).ok_or_else(|| StateError::Io("short state".into()))?);
        self.at = end;
        Ok(())
    }
}

#[test]
#[ignore = "exploration; needs Super Metroid (RF_SCREENS_ROMS_DIR)"]
fn explore() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let script = std::env::var("RF_SCRIPT").unwrap_or_else(|_| "w300".into());
    let dir = std::env::var("RF_SHOTS").unwrap_or_else(|_| "/tmp/sm".into());
    let every: usize = std::env::var("RF_EVERY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);
    std::fs::create_dir_all(&dir).expect("dir");
    let masks = frames_of(&script);
    let mut core = SnesCore::load(&raw).expect("loads");
    core.reset(ResetKind::Hard);
    if let Ok(p) = std::env::var("RF_LOAD") {
        let mut m = Mem {
            data: std::fs::read(p).expect("state file"),
            at: 0,
        };
        for r in REGIONS {
            let mut len = [0u8; 4];
            m.read_exact(&mut len).expect("len");
            let mut part = Mem {
                data: vec![0; u32::from_le_bytes(len) as usize],
                at: 0,
            };
            m.read_exact(&mut part.data).expect("part");
            core.system_mut().load_region(r, &mut part).expect("region");
        }
    }
    let mut shot = Shot::new();
    let t = std::time::Instant::now();
    for (f, input) in inputs_of(&masks).iter().enumerate() {
        let ports = &mut core.system_mut().bus.joypads.ports;
        ports[0] = input.ports[0];
        core.run_frame(input, &mut shot);
        if (f + 1) % every == 0 || f + 1 == masks.len() {
            let w = |a: usize| {
                let r = &core.system().bus.wram;
                u16::from(r[a]) | u16::from(r[a + 1]) << 8
            };
            println!(
                "f{} {:?} ram 0998={:04x} 079b={:04x} 0911={:04x} 0915={:04x} 0af6={:04x} 0afa={:04x}",
                f + 1,
                t.elapsed(),
                w(0x998),
                w(0x79b),
                w(0x911),
                w(0x915),
                w(0xaf6),
                w(0xafa)
            );
            let cg: Vec<u8> = core
                .system()
                .bus
                .ppu
                .cgram
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .collect();
            write_bmp(
                &std::path::Path::new(&dir).join(format!("f{:06}.bmp", f + 1)),
                &shot,
                &cg,
            );
        }
    }
    if let Ok(p) = std::env::var("RF_SAVE") {
        let mut m = Mem::default();
        for r in REGIONS {
            let mut part = Mem::default();
            core.system().save_region(r, &mut part).expect("saves");
            m.data.extend_from_slice(&(part.data.len() as u32).to_le_bytes());
            m.data.extend_from_slice(&part.data);
        }
        std::fs::write(p, m.data).expect("writes");
    }
}

/// Whole-core copy as region payloads (the scratch search below restores it).
fn grab(core: &SnesCore) -> Vec<Vec<u8>> {
    REGIONS
        .iter()
        .map(|&r| {
            let mut m = Mem::default();
            core.system().save_region(r, &mut m).expect("saves");
            m.data
        })
        .collect()
}

fn put(core: &mut SnesCore, snap: &[Vec<u8>]) {
    for (&r, d) in REGIONS.iter().zip(snap) {
        let mut m = Mem {
            data: d.clone(),
            at: 0,
        };
        core.system_mut().load_region(r, &mut m).expect("region");
    }
}

/// A tiny xorshift (the search needs no quality, only a repeatable stream).
struct Rng(u64);

impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

/// Exploration helper: from `RF_LOAD`, random token strings are tried for
/// `RF_STEPS` rounds of `RF_TRIALS` trials; the string whose end has the
/// smallest value of the word at `RF_Y` (default Samus's y) wins each round
/// and is printed. A bounded hill climb; it only finds button scripts.
#[test]
#[ignore = "exploration; needs Super Metroid (RF_SCREENS_ROMS_DIR)"]
fn search() {
    let Some(path) = rom_path() else {
        return;
    };
    let raw = load_rom(&path);
    let load = std::env::var("RF_LOAD").expect("RF_LOAD");
    let steps: usize = std::env::var("RF_STEPS").ok().and_then(|v| v.parse().ok()).unwrap_or(6);
    let trials: usize = std::env::var("RF_TRIALS").ok().and_then(|v| v.parse().ok()).unwrap_or(24);
    let want_room: u16 = u16::from_str_radix(&std::env::var("RF_ROOM").unwrap_or("df45".into()), 16).expect("hex");
    let mut core = SnesCore::load(&raw).expect("loads");
    core.reset(ResetKind::Hard);
    let mut m = Mem {
        data: std::fs::read(load).expect("state file"),
        at: 0,
    };
    for r in REGIONS {
        let mut len = [0u8; 4];
        m.read_exact(&mut len).expect("len");
        let mut part = Mem {
            data: vec![0; u32::from_le_bytes(len) as usize],
            at: 0,
        };
        m.read_exact(&mut part.data).expect("part");
        core.system_mut().load_region(r, &mut part).expect("region");
    }
    let toks = ["L+A20", "R+A20", "A40", "L25", "R25", "L+A35", "R+A35", "L+A45", "R+A45", "L12", "R12", "L+A12", "R+A12", "w10"];
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut best_script = String::new();
    for step in 0..steps {
        let base = grab(&core);
        let mut best: Option<(u32, String)> = None;
        for _ in 0..trials {
            put(&mut core, &base);
            let mut script = String::new();
            for _ in 0..(3 + rng.next(4)) {
                script.push_str(toks[rng.next(toks.len() as u64) as usize]);
                script.push(' ');
            }
            let mut ok = true;
            let mut min_y = u32::MAX;
            for mask in frames_of(&script) {
                let mut input = rf_core_api::InputFrame::empty();
                input.ports[0] = mask;
                core.system_mut().bus.joypads.ports[0] = mask;
                core.run_frame(&input, &mut NullSink);
                let r = &core.system().bus.wram;
                let room = u16::from(r[0x79b]) | u16::from(r[0x79c]) << 8;
                let y = u32::from(r[0xafa]) | u32::from(r[0xafb]) << 8;
                if room != want_room || y > 0x8000 {
                    ok = false;
                    break;
                }
                min_y = min_y.min(y);
            }
            let r = &core.system().bus.wram;
            let y = u32::from(r[0xafa]) | u32::from(r[0xafb]) << 8;
            let score = y.min(min_y + 40);
            if ok && best.as_ref().is_none_or(|b| score < b.0) {
                best = Some((score, script));
            }
        }
        let Some((score, script)) = best else {
            println!("step {step}: no trial stayed in the room");
            break;
        };
        put(&mut core, &base);
        for mask in frames_of(&script) {
            core.system_mut().bus.joypads.ports[0] = mask;
            core.run_frame(&rf_core_api::InputFrame::empty(), &mut NullSink);
        }
        best_script.push_str(&script);
        println!("step {step}: score {score:#x} script {script}");
    }
    println!("TOTAL {best_script}");
}

/// Prints the pinned script's length and room-id word every 1000 frames.
#[test]
#[ignore = "exploration; needs Super Metroid (RF_SCREENS_ROMS_DIR)"]
fn pinned_script() {
    let Some(path) = rom_path() else {
        return;
    };
    let raw = load_rom(&path);
    let masks = script();
    let mut core = SnesCore::load(&raw).expect("loads");
    core.reset(ResetKind::Hard);
    for (f, input) in inputs_of(&masks).iter().enumerate() {
        core.system_mut().bus.joypads.ports[0] = input.ports[0];
        core.run_frame(input, &mut NullSink);
        if (f + 1) % 1000 == 0 || f + 1 == masks.len() {
            let r = &core.system().bus.wram;
            println!("f{} room {:02x}{:02x}", f + 1, r[0x79c], r[0x79b]);
        }
    }
}
