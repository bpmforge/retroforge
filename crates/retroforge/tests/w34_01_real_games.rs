//! Ticket W34-01 on real games: what the tile grids cost per frame, and that
//! the SNES grids are the tilemap read on a real level. ROM-gated and
//! `#[ignore]`d (timing, and the ROMs never live in git).
//!
//! ```text
//! cargo test --release -p retroforge --test w34_01_real_games -- --ignored --nocapture
//! RF_SCREENS_ROMS_DIR=/path/to/Roms overrides the ROM folder.
//! ```
//!
//! The cost is measured in process as capture OFF against capture ON,
//! interleaved frame by frame (the simulation is identical either way, the
//! other tests prove it) so machine load hits both alike:
//!
//! * `frame`: advance one frame and take the bundle; ON adds the tile
//!   capture and (SNES) the per-line registers the bundle then keeps;
//! * `grids`: the builder alone, `EmuStepper::tile_grids`, on the frame just
//!   run (what the core thread adds before it sends the frame).

use std::path::{Path, PathBuf};
use std::time::Instant;

use retroforge::stepper::EmuStepper;
use rf_core_api::{FrameBundleBuilder, InputFrame};
use rf_input::SnesButton;

fn rom(console: &str, file: &str) -> Option<Vec<u8>> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p: PathBuf = Path::new(&root).join(console).join(file);
    if !p.exists() {
        println!("SKIP: {} not found", p.display());
        return None;
    }
    Some(retroforge::rom_open::load_rom_bytes(&p).expect("rom"))
}

struct Game {
    st: EmuStepper,
    b: FrameBundleBuilder,
    h: usize,
    snes: bool,
}

impl Game {
    fn open(raw: &[u8]) -> Self {
        let st = EmuStepper::open(raw).expect("opens");
        let snes = st.bg_layer_views().is_some();
        let h = if snes { 224 } else { 240 };
        Game {
            st,
            b: FrameBundleBuilder::new(256, h as u16),
            h,
            snes,
        }
    }

    fn frame(&mut self, pad: u16) -> rf_core_api::FrameBundle {
        let mut f = InputFrame::empty();
        f.ports[0] = pad;
        self.st.latch_and_advance_frame(f, &mut self.b);
        self.b.take(self.st.frame_count())
    }

    fn run(&mut self, pad: u16, n: u32) {
        for _ in 0..n {
            drop(self.frame(pad));
        }
    }

    fn tap(&mut self, pad: u16, hold: u32) {
        self.run(pad, hold);
        self.run(0, 8);
    }
}

fn snes_pad(b: SnesButton) -> u16 {
    1 << b.bit()
}

/// Boot to the first level of Super Mario World (the script of
/// `w33_09_scenes_smw::reach_yi1`, which `w30_screens` runs through the GUI).
fn smw_yi1(g: &mut Game) {
    g.run(0, 200);
    for i in 0..10 {
        g.tap(
            snes_pad(if i % 2 == 0 {
                SnesButton::Start
            } else {
                SnesButton::A
            }),
            6,
        );
        g.run(0, 100);
    }
    g.run(0, 300);
    g.tap(snes_pad(SnesButton::A), 8);
    g.run(0, 400);
    g.tap(snes_pad(SnesButton::Left), 8);
    g.run(0, 150);
    g.tap(snes_pad(SnesButton::A), 8);
    g.run(0, 150);
}

/// Super Mario Bros. 3 to World 1-1 (`w30_screens::smb3_enter_level`).
fn smb3_11(g: &mut Game) {
    const A: u16 = 0x01;
    const START: u16 = 0x08;
    const UP: u16 = 0x10;
    const RIGHT: u16 = 0x80;
    g.run(0, 300);
    g.tap(START, 6);
    g.run(0, 120);
    g.tap(START, 6);
    g.run(0, 400);
    g.tap(RIGHT, 8);
    g.run(0, 60);
    g.tap(UP, 8);
    g.run(0, 60);
    g.tap(A, 8);
    g.run(0, 100);
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p) as usize]
}

/// Interleaved A/B over `rounds` pairs while Mario runs right (`pad`).
fn measure(g: &mut Game, pad: u16, rounds: u32, label: &str) {
    let (mut off, mut on, mut build, mut bytes, mut grids_n) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for round in 0..rounds {
        for capture in [round % 2 == 0, round % 2 != 0] {
            g.st.set_tile_capture(capture);
            g.b.set_capture_wide(capture);
            let t = Instant::now();
            let bundle = g.frame(pad);
            let us = t.elapsed().as_secs_f64() * 1e6;
            if capture {
                on.push(us);
                let t = Instant::now();
                let grids = g.st.tile_grids(&bundle.lines, [false; 4], g.h);
                build.push(t.elapsed().as_secs_f64() * 1e6);
                bytes.push(grids.iter().map(|x| x.cells.len() * 16).sum::<usize>() as f64);
                grids_n.push(grids.len() as f64);
                std::hint::black_box(&grids);
            } else {
                off.push(us);
            }
        }
    }
    let (m_off, m_on) = (pct(&mut off, 0.5), pct(&mut on, 0.5));
    println!(
        "COST {label}: frame OFF median {m_off:.0} us, ON {m_on:.0} us (delta {:.0} us); \
         grids() median {:.0} us p95 {:.0} us; payload median {:.0} B max {:.0} B; \
         grids/frame median {:.0} max {:.0}; n = {} each",
        m_on - m_off,
        pct(&mut build, 0.5),
        pct(&mut build, 0.95),
        pct(&mut bytes, 0.5),
        pct(&mut bytes, 1.0),
        pct(&mut grids_n, 0.5),
        pct(&mut grids_n, 1.0),
        off.len()
    );
}

#[test]
#[ignore = "real ROM; timing; run deliberately in release"]
fn smw_cost_and_oracle() {
    let Some(raw) = rom("snes", "Super Mario World (USA).zip") else {
        return;
    };
    let mut g = Game::open(&raw);
    smw_yi1(&mut g);
    let right = snes_pad(SnesButton::Right);

    // The oracle on the real level: cells compared with the tilemap read.
    g.st.set_tile_capture(true);
    g.b.set_capture_wide(true);
    let (mut compared, mut multi, mut frames) = (0usize, 0usize, 0usize);
    for n in 0..240 {
        let bundle = g.frame(if n % 60 < 50 { right } else { 0 });
        let grids = g.st.tile_grids(&bundle.lines, [false; 4], g.h);
        let snap = g.st.snes_debug_snapshot().expect("snes");
        frames += 1;
        for gr in &grids {
            if gr.rows_px != (0, 224) {
                multi += 1;
                continue;
            }
            if !gr.exact {
                continue;
            }
            let l = bundle.lines[0].expect("registers");
            let scroll = std::array::from_fn(|i| (l.hofs[i] & 0x3FF, l.vofs[i] & 0x3FF));
            let o = retroforge::live_heights::character_grid(
                gr.layer + 1,
                gr.cell_px,
                &snap.vram,
                &snap.ppu_regs,
                scroll,
                (256, 224),
            )
            .expect("layer");
            assert_eq!((u32::from(gr.cols), u32::from(gr.rows)), (o.cols, o.rows));
            assert_eq!((u32::from(gr.offset.0), u32::from(gr.offset.1)), o.offset);
            let mut by_char = std::collections::HashMap::new();
            for row in 0..o.rows {
                for col in 0..o.cols {
                    let c = o.char_at(col, row).unwrap();
                    let p = gr.cell(col as u16, row as u16).unwrap().pattern;
                    assert_eq!(
                        *by_char.entry(c).or_insert(p),
                        p,
                        "char {c} hashed two ways"
                    );
                    compared += 1;
                }
            }
        }
    }
    println!("ORACLE smw: {compared} cells over {frames} frames equal the tilemap read; {multi} banded grids seen");
    assert!(compared > 10_000);

    measure(&mut g, right, 600, "smw (yi1, running right)");
}

#[test]
#[ignore = "real ROM; timing; run deliberately in release"]
fn smb3_cost() {
    let Some(raw) = rom("nes", "Super Mario Bros. 3 (USA).zip") else {
        return;
    };
    let mut g = Game::open(&raw);
    assert!(!g.snes);
    smb3_11(&mut g);
    measure(&mut g, 0x80, 600, "smb3 (1-1, running right)");
}
