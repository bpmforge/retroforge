//! PAL vs NTSC golden frames (ticket W7-10 criterion 3).
//!
//! # What "differs in the expected way rather than by accident" means here
//!
//! It means **the picture must NOT differ at all**, and that is the whole
//! point of the test.
//!
//! PAL changes two things — the master clock (21.281370 MHz against
//! 21.477270 MHz) and the frame length (312 scanlines against 262). It
//! changes a third thing not at all: the PPU still renders 224 visible
//! lines, 239 with overscan, and a scanline is 1364 master cycles in both
//! regions. Every one of PAL's extra 50 lines is vblank.
//!
//! So the tempting bug — "PAL has more lines, so it must show more
//! picture" — would letterbox every PAL game differently from hardware,
//! and it would sail past a test that only asserted "the two frames
//! differ". Asserting they are IDENTICAL is the assertion with teeth.
//!
//! The difference that IS expected lives in the frame COUNT, and
//! `src/tests/timing.rs` pins that against the 312/262 ratio without
//! needing any artifact. This file covers the other half: a real ROM,
//! really rendered, hashed in both regions.
//!
//! ```text
//! scripts/fetch-peterlemon-ppu.sh
//! cargo test -p rf-snes --release --test region_golden -- --ignored
//! ```

use rf_snes::system::SnesSystem;
use rf_snes::timing::Region;
use sha2::{Digest, Sha256};

const HEIGHT: u16 = 224;
/// Compare at the same FRAME, not after the same instruction count.
///
/// This is the control that makes the comparison mean anything, and
/// getting it wrong is instructive: a fixed instruction budget leaves the
/// two regions at DIFFERENT frame counts (197 vs 166 on one of these
/// ROMs, because a PAL frame is longer), so any ROM whose state advances
/// per frame is legitimately at a different point in its own logic. The
/// first version of this test did that and reported a "region leak" that
/// was really just two different moments being compared.
///
/// These ROMs are driven by vblank, which happens once per frame in both
/// regions, so the same frame index IS the same moment in the program.
const TARGET_FRAME: u64 = 150;
/// Safety bound; PAL needs more instructions to reach a given frame.
const MAX_INSTRUCTIONS: u64 = 20_000_000;

fn rom_dir() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("RF_PETERLEMON_PPU") {
        return Some(p.into());
    }
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../roms/snes/peterlemon-ppu");
    p.exists().then_some(p)
}

/// Render one ROM in `region` and hash the palette indices of every
/// visible line.
///
/// Indices, not colours — same rule the PeterLemon suite follows, and for
/// the same reason: resolving to RGB is the renderer's job (law 4).
/// The ROM-controlled inputs the PPU renders FROM.
///
/// Captured so the comparison can tell "the renderer leaked the region"
/// apart from "the ROM is at a different point in its own animation".
/// Without it this test reports the second as the first — which is
/// exactly what it did on `8x8BGMap8BPP32x32`, whose scroll read 130 in
/// NTSC and 131 in PAL at the same frame because the ROM scrolls in its
/// MAIN LOOP and a longer PAL frame hands it more CPU time.
#[derive(Debug, PartialEq, Eq)]
struct PpuInputs {
    mode: u8,
    forced_blank: bool,
    brightness: u8,
    hofs: Vec<u16>,
    vofs: Vec<u16>,
}

/// Returns the frame hash, the ROM-controlled PPU inputs, and the
/// INSTRUCTIONS it took to reach the target frame.
fn hash_frame(path: &std::path::Path, region: Region) -> (String, PpuInputs, u64) {
    let bytes = std::fs::read(path).expect("rom readable");
    let mut s = SnesSystem::load(&bytes).expect("a plain LoROM cart");
    s.set_region(region);
    let mut ran = 0u64;
    // `ran` advances on every path through the body (law 8).
    while ran < MAX_INSTRUCTIONS && s.bus.timing.frame < TARGET_FRAME {
        s.step().expect("all 256 opcodes are implemented");
        ran += 1;
    }
    assert!(
        s.bus.timing.frame >= TARGET_FRAME,
        "{} never reached frame {TARGET_FRAME} in {region:?}",
        path.display()
    );
    let mut hasher = Sha256::new();
    for y in 0..HEIGHT {
        for px in &s.bus.ppu.render_scanline(y).pixels {
            hasher.update([px.palette_index]);
        }
    }
    let inputs = PpuInputs {
        mode: s.bus.ppu.bg_mode,
        forced_blank: s.bus.ppu.forced_blank,
        brightness: s.bus.ppu.brightness,
        hofs: s.bus.ppu.bgs.iter().map(|b| b.hofs).collect(),
        vofs: s.bus.ppu.bgs.iter().map(|b| b.vofs).collect(),
    };
    (
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        inputs,
        ran,
    )
}

#[test]
#[ignore = "local: run scripts/fetch-peterlemon-ppu.sh first"]
fn a_pal_frame_is_pixel_identical_to_its_ntsc_counterpart() {
    let Some(dir) = rom_dir() else {
        eprintln!("SKIP: run scripts/fetch-peterlemon-ppu.sh");
        return;
    };
    // Four ROMs across different BG modes, so this is not one lucky
    // picture. Each is a settled static frame, which is what makes the
    // comparison meaningful — an animating demo would differ between
    // regions purely because the two are at different frame counts, and
    // that would tell us nothing about the RENDERER being region-blind.
    let roms = [
        "8x8BG1Map2BPP32x328PAL.sfc",
        "8x8BGMap4BPP32x328PAL.sfc",
        "8x8BGMap8BPP32x32.sfc",
        "8x8BGMapTileFlip.sfc",
    ];

    let mut checked = 0;
    for name in roms {
        let path = dir.join(name);
        if !path.exists() {
            eprintln!("SKIP {name}: not fetched");
            continue;
        }
        let (ntsc_hash, ntsc_inputs, ntsc_instrs) = hash_frame(&path, Region::Ntsc);
        let (pal_hash, pal_inputs, pal_instrs) = hash_frame(&path, Region::Pal);
        eprintln!(
            "{name}: frame {TARGET_FRAME} after {ntsc_instrs} instrs (NTSC) \
             vs {pal_instrs} (PAL)"
        );

        // ALWAYS assert the timing really changed, even for a ROM whose
        // picture is not comparable — otherwise an inert `set_region`
        // would sail through on the skip path below.
        assert!(
            pal_instrs > ntsc_instrs,
            "{name}: a PAL frame is 312 lines against 262, so reaching frame \
             {TARGET_FRAME} must take MORE instructions ({pal_instrs} vs \
             {ntsc_instrs}); equal counts mean the region never took effect"
        );

        // Compare like with like. A ROM that advances its own state
        // outside vblank is at a different point in PAL, and its frames
        // SHOULD differ — that is the ROM's timing sensitivity, not a
        // renderer bug, and asserting on it would pin the wrong thing.
        if ntsc_inputs != pal_inputs {
            eprintln!(
                "   not comparable: the ROM drives the PPU from its main \
                 loop, so PAL is further along.\n     ntsc {ntsc_inputs:?}\n     \
                 pal  {pal_inputs:?}"
            );
            continue;
        }

        assert_eq!(
            ntsc_hash, pal_hash,
            "{name}: identical PPU inputs must render an identical picture in \
             both regions. PAL's extra 50 scanlines are vblank, not picture — \
             a difference here is the region leaking into rendering, which \
             would letterbox every PAL game differently from hardware.\n  \
             inputs: {ntsc_inputs:?}"
        );
        checked += 1;
    }
    assert!(
        checked >= 2,
        "only {checked} ROM(s) were actually comparable; a green run that \
         compared nothing is worse than a red one"
    );
}
