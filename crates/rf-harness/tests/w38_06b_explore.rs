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
    // `$PRE_DUMP=from:every` also dumps screens of the pinned play itself.
    let pre: Option<(usize, usize)> = std::env::var("PRE_DUMP").ok().and_then(|v| {
        let (a, b) = v.split_once(':')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    });
    let out = std::env::var("EXPLORE_OUT").ok().map(std::path::PathBuf::from);
    let upto: usize = std::env::var("PIN_UPTO").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    let mut freeze = Freeze::new();
    for (f, m) in script().into_iter().enumerate().take(upto) {
        freeze.before(f, &mut core);
        core.system_mut().bus.joypads.ports[0] = m;
        let mut shot = Shot::new();
        core.run_frame(&rf_core_api::InputFrame::empty(), &mut shot);
        if let (Some((from, every)), Some(out)) = (pre, out.as_ref()) {
            if f >= from && (f - from) % every == 0 {
                let cg: Vec<u8> = core.system().bus.ppu.cgram.iter().flat_map(|c| c.to_le_bytes()).collect();
                std::fs::create_dir_all(out).expect("dir");
                write_bmp(&out.join(format!("p{f:06}.bmp")), &shot, &cg);
            }
        }
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
    let mut extra = Vec::new();
    // `$CONT=n`: first the pinned script's own next n frames after `$PIN_UPTO`.
    if let (Ok(c), Ok(u)) = (std::env::var("CONT"), std::env::var("PIN_UPTO")) {
        let (c, u): (usize, usize) = (c.parse().expect("n"), u.parse().expect("n"));
        extra.extend(script().into_iter().skip(u).take(c));
    }
    extra.extend(frames_of(&toks));
    let mut core = at_pinned_end(&raw);
    let w = |c: &SnesCore, a: usize| {
        let r = &c.system().bus.wram;
        u16::from(r[a]) | u16::from(r[a + 1]) << 8
    };
    let all: Vec<u16> = extra.clone();
    // `$SCAN=1`: words that only ever decrease while changing (a countdown).
    let scan = std::env::var("SCAN").is_ok();
    let mut hist: Vec<Vec<u8>> = Vec::new();
    for (f, m) in all.iter().enumerate() {
        core.system_mut().bus.joypads.ports[0] = *m;
        let mut shot = Shot::new();
        core.run_frame(&rf_core_api::InputFrame::empty(), &mut shot);
        if scan {
            hist.push(core.system().bus.wram[..0x2000].to_vec());
        }
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
    if scan {
        let word = |h: &Vec<u8>, a: usize| u16::from(h[a]) | u16::from(h[a + 1]) << 8;
        for a in 0..0x2000 - 1 {
            let (mut dec, mut inc) = (0usize, 0usize);
            let mut steps = std::collections::BTreeSet::new();
            for w in hist.windows(2) {
                let (x, y) = (word(&w[0], a), word(&w[1], a));
                if y < x {
                    dec += 1;
                    steps.insert(x - y);
                } else if y > x {
                    inc += 1;
                }
            }
            if inc == 0 && dec >= 8 {
                println!(
                    "SCAN {a:#x}: {dec} decreases of {:?}, {:#x} -> {:#x}",
                    steps.iter().take(4).collect::<Vec<_>>(),
                    word(&hist[0], a),
                    word(hist.last().unwrap(), a)
                );
            }
        }
    }
}

fn save_all(core: &SnesCore) -> Vec<u8> {
    let mut o = Out(Vec::new());
    for reg in StateRegion::ALL {
        core.system().save_region(reg, &mut o).expect("saves a region");
    }
    o.0
}

fn restore(raw: &[u8], bytes: &[u8]) -> SnesCore {
    let mut core = SnesCore::load(raw).expect("loads");
    let mut r = In(bytes, 0);
    for reg in StateRegion::ALL {
        core.system_mut().load_region(reg, &mut r).expect("loads a region");
    }
    core
}

/// `$SEARCH=goal:steps:tries` (goal: `up`, `left`, `right`, `down`): a bounded
/// random search from the restored machine; each step tries `tries` random
/// (buttons, duration) chunks on a copy and keeps the one that ends furthest
/// toward the goal (a room change ends the search at once). Prints the tokens.
#[test]
#[ignore = "needs Super Metroid (RF_SCREENS_ROMS_DIR); exploration aid"]
fn search() {
    let Some(path) = rom_path() else { return };
    let raw = load_rom(&path);
    let spec = std::env::var("SEARCH").expect("goal:steps:tries");
    let mut it = spec.split(':');
    let goal = it.next().unwrap().to_string();
    let steps: usize = it.next().unwrap().parse().unwrap();
    let tries: usize = it.next().unwrap().parse().unwrap();
    let mut frame0: usize = std::env::var("PIN_UPTO").expect("frame").parse().unwrap();
    let mut state = save_all(&at_pinned_end(&raw));
    let room0 = {
        let c = restore(&raw, &state);
        let r = &c.system().bus.wram;
        u16::from(r[0x7bb]) | u16::from(r[0x7bc]) << 8
    };
    let combos = ["L", "R", "L+A", "R+A", "A", "R+A+X", "L+A+X", "U", "D"];
    let durs = [8usize, 12, 16, 20, 25, 30, 40];
    let mut rng: u64 = std::env::var("SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let mut next = |n: usize| {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((rng >> 33) as usize) % n
    };
    let rd = |c: &SnesCore, a: usize| {
        let r = &c.system().bus.wram;
        u16::from(r[a]) | u16::from(r[a + 1]) << 8
    };
    let score = |c: &SnesCore| -> i64 {
        let (x, y) = (i64::from(rd(c, 0xaf6)), i64::from(rd(c, 0xafa)));
        match goal.as_str() {
            "up" => -y,
            "down" => y,
            "left" => -x,
            _ => x,
        }
    };
    let mut chosen: Vec<String> = Vec::new();
    let mut best_so_far = i64::MIN;
    for step in 0..steps {
        let mut best: Option<(i64, String, Vec<u8>, bool)> = None;
        for _ in 0..tries {
            let tok = format!("{}{}", combos[next(combos.len())], durs[next(durs.len())]);
            let mut core = restore(&raw, &state);
            let mut freeze = Freeze::running();
            for (i, m) in frames_of(&tok).into_iter().enumerate() {
                freeze.before(frame0 + i, &mut core);
                core.system_mut().bus.joypads.ports[0] = m;
                core.run_frame(&rf_core_api::InputFrame::empty(), &mut NullSink);
            }
            let changed = rd(&core, 0x7bb) != room0;
            let sc = score(&core) + if changed { 1_000_000 } else { 0 };
            if best.as_ref().is_none_or(|b| sc > b.0) {
                best = Some((sc, tok, save_all(&core), changed));
            }
        }
        let (sc, tok, st, changed) = best.expect("a try");
        frame0 += frames_of(&tok).len();
        state = st;
        chosen.push(tok);
        best_so_far = best_so_far.max(sc);
        if step % 10 == 0 {
            println!("step {step} score {sc} frame {frame0}");
        }
        if changed {
            println!("ROOM CHANGED after {} chunks", chosen.len());
            break;
        }
    }
    println!("TOKENS {}", chosen.join(" "));
    if let Ok(f) = std::env::var("SEARCH_OUT") {
        std::fs::write(f, chosen.join(" ")).expect("writes");
    }
    let c = restore(&raw, &state);
    println!("END x {} y {} room {:#x} best {best_so_far}", rd(&c, 0xaf6), rd(&c, 0xafa), rd(&c, 0x7bb));
    if let Ok(out) = std::env::var("EXPLORE_OUT") {
        std::fs::create_dir_all(&out).expect("dir");
        let mut core = restore(&raw, &state);
        let mut shot = Shot::new();
        core.run_frame(&rf_core_api::InputFrame::empty(), &mut shot);
        let cg: Vec<u8> = core.system().bus.ppu.cgram.iter().flat_map(|c| c.to_le_bytes()).collect();
        write_bmp(&std::path::Path::new(&out).join("end.bmp"), &shot, &cg);
    }
}
