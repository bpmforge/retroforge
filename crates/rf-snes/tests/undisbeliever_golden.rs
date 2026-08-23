//! undisbeliever `snes-test-roms` golden frames (ticket W7-15;
//! FR-CORE-033).
//!
//! ## These were blocked by a frozen clock, not by the renderer
//!
//! W7-07 and W7-05 both recorded that this whole set "never leaves forced
//! blank", ran all eighteen ROMs to prove it, and concluded they were
//! per-dot timing tests that a scanline-composed PPU could not express.
//! The evidence was real and the diagnosis was wrong.
//!
//! `Cpu::step` returned immediately while `stopped` (WAI/STP) without
//! touching the bus, so the step cost **zero** master cycles. WAI waits
//! for an interrupt; every interrupt this machine raises comes from the
//! raster; the raster only moves when master cycles are spent. The CPU
//! waited for a vblank that could never arrive. Measured:
//! `hdmaen_latch_test` ran 3,000,000 instructions and advanced the clock
//! by 6,840 master cycles, still on frame 0.
//!
//! With a halted step charged one internal CPU cycle, 17 of the 20 ROMs
//! leave forced blank and render their test screens.
//!
//! ## Self-generated, and what that is worth
//!
//! Unlike the PeterLemon suite, this author ships no reference
//! screenshots, so these hashes cannot be checked against anyone else's
//! output. Each frame WAS rendered to a PNG and looked at — they draw
//! their own legible test screens (a brick field labelled "4 bpp" with
//! "OBJ", and similar) rather than noise or a blank — and the runner
//! below re-renders each ROM twice and requires the two hashes to agree,
//! so a frame captured mid-animation cannot be pinned as if it were
//! settled. That makes them regression detectors, which is what a golden
//! is for; it does not make them proof the picture is right.

use rf_snes::cpu::CpuBus;
use rf_snes::SnesSystem;
use sha2::{Digest, Sha256};

/// `(rom, sha256 of the frame's palette indices)`.
const GOLDENS: &[(&str, &str)] = &[
    // **These two hash IDENTICALLY, and that is a finding, not a
    // coincidence.** They differ only in the per-dot behaviour of a
    // mid-scanline `$2100`/`$21FF` write during HDMA, and this PPU
    // composes per scanline, so it cannot express the difference the two
    // ROMs exist to distinguish. Both render the same legible test screen
    // (a brick field labelled "4 bpp" with "OBJ") and both are stable
    // across two runs, so they are honest regression detectors for
    // everything EXCEPT the thing they were written to test.
    //
    // When W7-15's criterion 1 lands, these hashes must DIVERGE. Two
    // equal hashes here is the cheapest available measure of how much
    // per-dot work is left.
    (
        "hdma-2100-glitch.sfc",
        "eac00c983c3758b79963fca384e9202cb1326dc077a5086af08b4bddfb29ea76",
    ),
    (
        "hdma-21ff-glitch.sfc",
        "eac00c983c3758b79963fca384e9202cb1326dc077a5086af08b4bddfb29ea76",
    ),
];

/// Fetched and run, but not pinned, with the reason.
const EXCLUDED: &[(&str, &str)] = &[
    (
    "hdmaen_latch_test.sfc",
    "leaves forced blank now (it did not before this ticket) but renders a frame that is \
     ENTIRELY palette index 0. Its hash is therefore the same as every other all-backdrop \
     frame in this project and cannot tell a correct blank screen from a PPU that drew \
     nothing -- the same index-domain vacuity that excludes GreenSpace and the RedSpace \
     pair from the PeterLemon suite. What it checks (HDMAEN latched at init, not mid-frame) \
     is already covered without a ROM by tests::hdma::enabling_a_channel_mid_frame_does_not_\
     transfer_until_the_next_init.",
    ),
    // ---- same picture as the pinned pair (W7-13, 2026-08-23) ----
    (
        "hdma-2100-glitch-2ch-0a.sfc",
        "hashes IDENTICALLY to the two PINNED glitch ROMs (7 palette indices, same sha256). Pinning it would add a third copy of one picture and no discrimination at all -- what it actually tests is INIDISP/HDMA timing, which the indexed pixel stream does not carry (law 4). Measured 2026-08-23 across all 29 ROMs: TWO distinct pixel hashes total.",
    ),
    (
        "hdma-2100-glitch-2ch-81.sfc",
        "hashes IDENTICALLY to the two PINNED glitch ROMs (7 palette indices, same sha256). Pinning it would add a third copy of one picture and no discrimination at all -- what it actually tests is INIDISP/HDMA timing, which the indexed pixel stream does not carry (law 4). Measured 2026-08-23 across all 29 ROMs: TWO distinct pixel hashes total.",
    ),
    (
        "hdma-21ff-2100-0f-glitch.sfc",
        "hashes IDENTICALLY to the two PINNED glitch ROMs (7 palette indices, same sha256). Pinning it would add a third copy of one picture and no discrimination at all -- what it actually tests is INIDISP/HDMA timing, which the indexed pixel stream does not carry (law 4). Measured 2026-08-23 across all 29 ROMs: TWO distinct pixel hashes total.",
    ),
    (
        "hdma-21ff-2100-glitch.sfc",
        "hashes IDENTICALLY to the two PINNED glitch ROMs (7 palette indices, same sha256). Pinning it would add a third copy of one picture and no discrimination at all -- what it actually tests is INIDISP/HDMA timing, which the indexed pixel stream does not carry (law 4). Measured 2026-08-23 across all 29 ROMs: TWO distinct pixel hashes total.",
    ),
    (
        "inidisp_d7_glitch_test.sfc",
        "hashes IDENTICALLY to the two PINNED glitch ROMs (7 palette indices, same sha256). Pinning it would add a third copy of one picture and no discrimination at all -- what it actually tests is INIDISP/HDMA timing, which the indexed pixel stream does not carry (law 4). Measured 2026-08-23 across all 29 ROMs: TWO distinct pixel hashes total.",
    ),
    // ---- all-backdrop: index-domain vacuity ----
    (
        "hdmaen_latch_test_2.sfc",
        "renders an ENTIRELY BLANK frame -- 1 distinct palette index -- so its hash is the same as every other all-backdrop frame in this project and cannot tell a correct blank screen from a PPU that drew nothing. Same index-domain vacuity that excludes the RedSpace pair from the PeterLemon suite. Measured 2026-08-23.",
    ),
    (
        "inidisp_brightness_delay.sfc",
        "renders an ENTIRELY BLANK frame -- 1 distinct palette index -- so its hash is the same as every other all-backdrop frame in this project and cannot tell a correct blank screen from a PPU that drew nothing. Same index-domain vacuity that excludes the RedSpace pair from the PeterLemon suite. Measured 2026-08-23.",
    ),
    (
        "inidisp_forgot_to_force_blank.sfc",
        "renders an ENTIRELY BLANK frame -- 1 distinct palette index -- so its hash is the same as every other all-backdrop frame in this project and cannot tell a correct blank screen from a PPU that drew nothing. Same index-domain vacuity that excludes the RedSpace pair from the PeterLemon suite. Measured 2026-08-23.",
    ),
    (
        "scpu-a-dma-bug-1.sfc",
        "renders an ENTIRELY BLANK frame -- 1 distinct palette index -- so its hash is the same as every other all-backdrop frame in this project and cannot tell a correct blank screen from a PPU that drew nothing. Same index-domain vacuity that excludes the RedSpace pair from the PeterLemon suite. Measured 2026-08-23.",
    ),
    (
        "scpu-a-dma-bug-2.sfc",
        "renders an ENTIRELY BLANK frame -- 1 distinct palette index -- so its hash is the same as every other all-backdrop frame in this project and cannot tell a correct blank screen from a PPU that drew nothing. Same index-domain vacuity that excludes the RedSpace pair from the PeterLemon suite. Measured 2026-08-23.",
    ),
    (
        "scpu-a-dma-bug-3.sfc",
        "renders an ENTIRELY BLANK frame -- 1 distinct palette index -- so its hash is the same as every other all-backdrop frame in this project and cannot tell a correct blank screen from a PPU that drew nothing. Same index-domain vacuity that excludes the RedSpace pair from the PeterLemon suite. Measured 2026-08-23.",
    ),
    (
        "scpu-a-dma-bug-5.sfc",
        "renders an ENTIRELY BLANK frame -- 1 distinct palette index -- so its hash is the same as every other all-backdrop frame in this project and cannot tell a correct blank screen from a PPU that drew nothing. Same index-domain vacuity that excludes the RedSpace pair from the PeterLemon suite. Measured 2026-08-23.",
    ),
    (
        "scpu-a-dma-bug-ch0.sfc",
        "renders an ENTIRELY BLANK frame -- 1 distinct palette index -- so its hash is the same as every other all-backdrop frame in this project and cannot tell a correct blank screen from a PPU that drew nothing. Same index-domain vacuity that excludes the RedSpace pair from the PeterLemon suite. Measured 2026-08-23.",
    ),
    (
        "scpu-a-dma-bug-fix.sfc",
        "renders an ENTIRELY BLANK frame -- 1 distinct palette index -- so its hash is the same as every other all-backdrop frame in this project and cannot tell a correct blank screen from a PPU that drew nothing. Same index-domain vacuity that excludes the RedSpace pair from the PeterLemon suite. Measured 2026-08-23.",
    ),
    (
        "scpu-a-dma-bug-fix2.sfc",
        "renders an ENTIRELY BLANK frame -- 1 distinct palette index -- so its hash is the same as every other all-backdrop frame in this project and cannot tell a correct blank screen from a PPU that drew nothing. Same index-domain vacuity that excludes the RedSpace pair from the PeterLemon suite. Measured 2026-08-23.",
    ),
    (
        "scpu-a-dma-bug-r2.sfc",
        "renders an ENTIRELY BLANK frame -- 1 distinct palette index -- so its hash is the same as every other all-backdrop frame in this project and cannot tell a correct blank screen from a PPU that drew nothing. Same index-domain vacuity that excludes the RedSpace pair from the PeterLemon suite. Measured 2026-08-23.",
    ),
    (
        "scpu-a-dma-bug-strange.sfc",
        "renders an ENTIRELY BLANK frame -- 1 distinct palette index -- so its hash is the same as every other all-backdrop frame in this project and cannot tell a correct blank screen from a PPU that drew nothing. Same index-domain vacuity that excludes the RedSpace pair from the PeterLemon suite. Measured 2026-08-23.",
    ),
    (
        "scpu-a-dma-bug-two-regs.sfc",
        "renders an ENTIRELY BLANK frame -- 1 distinct palette index -- so its hash is the same as every other all-backdrop frame in this project and cannot tell a correct blank screen from a PPU that drew nothing. Same index-domain vacuity that excludes the RedSpace pair from the PeterLemon suite. Measured 2026-08-23.",
    ),
    // ---- discriminating, but the oracle needs a ruling ----
    (
        "inidisp_enable_display_mid_frame.sfc",
        "EXCLUDED PENDING A RULING, NOT BECAUSE IT IS VACUOUS -- this is the one family where a discriminating golden is now possible. Its picture is indistinguishable from the others, but W7-15's per-dot write recording gives it a mid-line $2100 profile that IS distinct: exactly ONE write, on one line. Hashing that record would discriminate, but it pins OUR INSTRUMENTATION rather than rendered output and couples the golden to Ppu::is_segmentable. See W7-13's notes for the three options; do not pin this until one is chosen.",
    ),
    (
        "inidisp_hammer_0f.sfc",
        "EXCLUDED PENDING A RULING, NOT BECAUSE IT IS VACUOUS -- this is the one family where a discriminating golden is now possible. Its picture is indistinguishable from the others, but W7-15's per-dot write recording gives it a mid-line $2100 profile that IS distinct: 1,988 writes across 70 lines. Hashing that record would discriminate, but it pins OUR INSTRUMENTATION rather than rendered output and couples the golden to Ppu::is_segmentable. See W7-13's notes for the three options; do not pin this until one is chosen.",
    ),
    (
        "inidisp_hammer_0f00.sfc",
        "EXCLUDED PENDING A RULING, NOT BECAUSE IT IS VACUOUS -- this is the one family where a discriminating golden is now possible. Its picture is indistinguishable from the others, but W7-15's per-dot write recording gives it a mid-line $2100 profile that IS distinct: 5,460 writes across all 224 lines. Hashing that record would discriminate, but it pins OUR INSTRUMENTATION rather than rendered output and couples the golden to Ppu::is_segmentable. See W7-13's notes for the three options; do not pin this until one is chosen.",
    ),
    (
        "inidisp_hammer_0f0f.sfc",
        "EXCLUDED PENDING A RULING, NOT BECAUSE IT IS VACUOUS -- this is the one family where a discriminating golden is now possible. Its picture is indistinguishable from the others, but W7-15's per-dot write recording gives it a mid-line $2100 profile that IS distinct: 5,460 writes across all 224 lines. Hashing that record would discriminate, but it pins OUR INSTRUMENTATION rather than rendered output and couples the golden to Ppu::is_segmentable. See W7-13's notes for the three options; do not pin this until one is chosen.",
    ),
    (
        "inidisp_hammer_0f8f.sfc",
        "EXCLUDED PENDING A RULING, NOT BECAUSE IT IS VACUOUS -- this is the one family where a discriminating golden is now possible. Its picture is indistinguishable from the others, but W7-15's per-dot write recording gives it a mid-line $2100 profile that IS distinct: 1,724 writes across 101 lines. Hashing that record would discriminate, but it pins OUR INSTRUMENTATION rather than rendered output and couples the golden to Ppu::is_segmentable. See W7-13's notes for the three options; do not pin this until one is chosen.",
    ),
    (
        "inidisp_hammer_0f8f_fast.sfc",
        "EXCLUDED PENDING A RULING, NOT BECAUSE IT IS VACUOUS -- this is the one family where a discriminating golden is now possible. Its picture is indistinguishable from the others, but W7-15's per-dot write recording gives it a mid-line $2100 profile that IS distinct: 5,460 writes across all 224 lines. Hashing that record would discriminate, but it pins OUR INSTRUMENTATION rather than rendered output and couples the golden to Ppu::is_segmentable. See W7-13's notes for the three options; do not pin this until one is chosen.",
    ),
    (
        "inidisp_hammer_0f_long.sfc",
        "EXCLUDED PENDING A RULING, NOT BECAUSE IT IS VACUOUS -- this is the one family where a discriminating golden is now possible. Its picture is indistinguishable from the others, but W7-15's per-dot write recording gives it a mid-line $2100 profile that IS distinct: 5,460 writes across all 224 lines. Hashing that record would discriminate, but it pins OUR INSTRUMENTATION rather than rendered output and couples the golden to Ppu::is_segmentable. See W7-13's notes for the three options; do not pin this until one is chosen.",
    ),
    (
        "inidisp_hammer_8f0f.sfc",
        "EXCLUDED PENDING A RULING, NOT BECAUSE IT IS VACUOUS -- this is the one family where a discriminating golden is now possible. Its picture is indistinguishable from the others, but W7-15's per-dot write recording gives it a mid-line $2100 profile that IS distinct: 1,844 writes across 108 lines, and it never leaves forced blank at all. Hashing that record would discriminate, but it pins OUR INSTRUMENTATION rather than rendered output and couples the golden to Ppu::is_segmentable. See W7-13's notes for the three options; do not pin this until one is chosen.",
    ),
];

const MAX_INSTRUCTIONS: u64 = 20_000_000;
const SETTLE: u64 = 200_000;

fn rom_dir() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("RF_UNDIS") {
        return Some(p.into());
    }
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../roms/snes/undisbeliever/snes-test-roms");
    p.exists().then_some(p)
}

/// Run to a settled, faded-in frame and hash its palette indices.
///
/// Returns `None` when the ROM never leaves forced blank — which, after
/// this ticket, means something specific rather than "as usual".
fn render_and_hash(path: &std::path::Path) -> Option<(String, u8)> {
    let rom = std::fs::read(path).ok()?;
    let mut s = SnesSystem::load(&rom).ok()?;
    let mut n = 0u64;
    while n < MAX_INSTRUCTIONS {
        if s.step().is_err() {
            break;
        }
        n += 1;
        if !s.bus.ppu.forced_blank && s.bus.ppu.brightness == 0x0F {
            break;
        }
    }
    if s.bus.ppu.forced_blank || s.bus.ppu.brightness != 0x0F {
        return None;
    }
    for _ in 0..SETTLE {
        let _ = s.step();
    }
    let mut hasher = Sha256::new();
    for y in 0..224u16 {
        let line = s.bus.ppu.render_scanline(y);
        let indices: Vec<u8> = line.pixels.iter().map(|p| p.palette_index).collect();
        hasher.update(&indices);
    }
    use std::fmt::Write;
    let hex = hasher.finalize().iter().fold(String::new(), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    });
    Some((hex, s.bus.ppu.bg_mode))
}

/// **The clock must advance while the CPU is halted.** This is the whole
/// ticket in one assertion, and it needs no ROM.
///
/// A `WAI` with NMI enabled must eventually be woken by vblank. Before the
/// fix this loop would spin forever with the clock frozen.
#[test]
fn a_halted_cpu_still_advances_the_master_clock() {
    // Minimal LoROM cart: CLI, then WAI, then a branch back to the WAI.
    let mut rom = vec![0u8; 64 * 1024];
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    rom[0x7FD5] = 0x20;
    let code: &[u8] = &[
        0x58, // CLI
        0xCB, // WAI
        0x80, 0xFD, // BRA -3
    ];
    rom[..code.len()].copy_from_slice(code);

    let mut s = SnesSystem::load(&rom).expect("loads");
    // Enable NMI so vblank has something to wake it with.
    s.bus.write(0x00_4200, 0x80);
    let before = s.master_cycles;
    // A frame is 341 dots x 4 master cycles x 262 lines = 357,368 master
    // cycles, so the loop has to be long enough to cross one at 6 cycles
    // per halted step. Sized from the hardware number rather than picked.
    for _ in 0..100_000 {
        let _ = s.step();
    }
    let spent = s.master_cycles - before;
    assert!(
        spent > 0,
        "100,000 steps of a halted CPU advanced the clock by {spent} master cycles - \
         WAI waits for an interrupt that only the raster can raise, so a free halt \
         is a deadlock"
    );
    assert!(
        s.bus.timing.frame > 0,
        "the raster never reached a new frame, so vblank could never fire"
    );
}

#[test]
#[ignore = "local: run scripts/fetch-test-roms.sh first"]
fn undisbeliever_goldens_match() {
    let Some(dir) = rom_dir() else {
        eprintln!("SKIP: undisbeliever ROMs not fetched");
        return;
    };
    let mut failures = Vec::new();
    for (name, expected) in GOLDENS {
        let path = dir.join(name);
        if !path.exists() {
            eprintln!("SKIP {name}: not fetched");
            continue;
        }
        let Some((actual, mode)) = render_and_hash(&path) else {
            failures.push(format!("{name}: never left forced blank"));
            continue;
        };
        // Stability: the same ROM run twice must agree, or the frame was
        // captured mid-animation and the pin is a coin flip.
        let (again, _) = render_and_hash(&path).expect("second run");
        if again != actual {
            failures.push(format!("{name}: not stable across two runs"));
            continue;
        }
        eprintln!("{name}: mode {mode}, sha256 {actual}");
        if actual != *expected {
            failures.push(format!(
                "{name}\n    expected {expected}\n    actual   {actual}"
            ));
        }
    }
    // Excluded ROMs are still RUN and reported, so an exclusion cannot
    // outlive its reason silently.
    for (name, why) in EXCLUDED {
        let path = dir.join(name);
        if !path.exists() {
            continue;
        }
        match render_and_hash(&path) {
            Some((hash, mode)) => {
                eprintln!("  excluded {name}: mode {mode}, sha256 {hash}\n    {why}")
            }
            None => eprintln!("  excluded {name}: still never leaves forced blank\n    {why}"),
        }
    }

    assert!(
        failures.is_empty(),
        "undisbeliever:\n{}",
        failures.join("\n")
    );
}

/// Survey every fetched ROM: what it renders, and what it drives.
///
/// **A tool, not an assertion** — it prints and asserts nothing, because
/// its job is to answer "can a golden here discriminate at all?" and the
/// answer turned out to be mostly no.
///
/// Measured 2026-08-21 across all 29 ROMs (ticket W7-13 criterion 2):
///
/// * **2 distinct pixel hashes.** 13 ROMs render one identical frame
///   (7 palette indices) and 16 render an entirely blank one (1 index).
///   Different tests, same picture — the same index-domain vacuity that
///   excludes the RedSpace pair from the PeterLemon suite, at scale.
/// * **7 distinct mid-line write hashes**, 21 of them empty. The
///   `inidisp_hammer` family DOES separate here — 1,988 writes over 70
///   lines against 5,460 over 224 against a single write — and that
///   signal did not exist before W7-15 recorded writes per line.
///
/// So "the undisbeliever set is green" cannot mean 29 pinned hashes: 21
/// of them would pin the same two pictures and prove nothing. What these
/// ROMs test — INIDISP brightness, forced-blank timing, DMA bugs — lives
/// in the brightness and colour domains that law 4 keeps out of the
/// indexed pixel stream.
#[test]
#[ignore = "tool: prints a survey, asserts nothing"]
fn survey_the_whole_set() {
    let Some(dir) = rom_dir() else {
        eprintln!("SKIP");
        return;
    };
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".sfc"))
        .collect();
    names.sort();
    eprintln!("{} ROMs", names.len());
    for name in names {
        let path = dir.join(&name);
        let rom = std::fs::read(&path).unwrap();
        let Ok(mut s) = SnesSystem::load(&rom) else {
            eprintln!("{name:<38} LOAD FAILED");
            continue;
        };
        let mut n = 0u64;
        while n < MAX_INSTRUCTIONS {
            if s.step().is_err() {
                break;
            }
            n += 1;
            if !s.bus.ppu.forced_blank && s.bus.ppu.brightness == 0x0F {
                break;
            }
        }
        let left = !s.bus.ppu.forced_blank && s.bus.ppu.brightness == 0x0F;
        for _ in 0..SETTLE {
            let _ = s.step();
        }
        let mut distinct = std::collections::BTreeSet::new();
        let mut hasher = Sha256::new();
        for y in 0..224u16 {
            let line = s.bus.ppu.render_scanline(y);
            let idx: Vec<u8> = line.pixels.iter().map(|p| p.palette_index).collect();
            distinct.extend(idx.iter().copied());
            hasher.update(&idx);
        }
        use std::fmt::Write;
        let hex = hasher.finalize().iter().fold(String::new(), |mut a, b| {
            let _ = write!(a, "{b:02x}");
            a
        });
        let mut wh = Sha256::new();
        for l in 0..240u16 {
            for (dot, addr, val) in s.bus.ppu.line_writes_for_test(l) {
                wh.update(l.to_le_bytes());
                wh.update(dot.to_le_bytes());
                wh.update(addr.to_le_bytes());
                wh.update([val]);
            }
        }
        let whex = wh.finalize().iter().fold(String::new(), |mut a, b| {
            use std::fmt::Write as _;
            let _ = write!(a, "{b:02x}");
            a
        });
        let mid: usize = (0..240u16)
            .map(|l| s.bus.ppu.line_writes_for_test(l).len())
            .sum();
        let lines_with: usize = (0..240u16)
            .filter(|&l| !s.bus.ppu.line_writes_for_test(l).is_empty())
            .count();
        let regs: std::collections::BTreeSet<u16> = (0..240u16)
            .flat_map(|l| s.bus.ppu.line_writes_for_test(l).into_iter().map(|w| w.1))
            .collect();
        eprintln!(
            "{name:<38} blank_left={left:<5} mode={} idx={:<3} mid={mid:<6} \
             lines={lines_with:<4} regs={:04X?} pix={} write={}",
            s.bus.ppu.bg_mode,
            distinct.len(),
            regs.iter().collect::<Vec<_>>(),
            &hex[..10],
            &whex[..14]
        );
    }
}
