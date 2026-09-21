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
    // ---- exact duplicates of a pinned write golden ----
    (
        "inidisp_hammer_0f0f.sfc",
        "byte-identical to inidisp_hammer_0f00 in BOTH dimensions -- same pixel hash AND same mid-line write record. Even the write-record oracle cannot separate these three, so one is pinned as their representative and the other two would add nothing. Measured 2026-08-23; write record re-measured 2026-09-20 (ticket W14-39, 3,765 writes across 177 lines, sha256 f67caf5b...) after the CPU pacing fix -- still identical to inidisp_hammer_0f00.",
    ),
    (
        "inidisp_hammer_0f8f_fast.sfc",
        "byte-identical to inidisp_hammer_0f00 in BOTH dimensions -- same pixel hash AND same mid-line write record. Even the write-record oracle cannot separate these three, so one is pinned as their representative and the other two would add nothing. Measured 2026-08-23; write record re-measured 2026-09-20 (ticket W14-39, 3,765 writes across 177 lines, sha256 f67caf5b...) after the CPU pacing fix -- still identical to inidisp_hammer_0f00.",
    ),
];

/// Pinned on the MID-LINE WRITE RECORD, not on the picture.
///
/// **Why a second dimension exists at all.** Across all 29 ROMs in this
/// set there are only TWO distinct pixel hashes: 13 render one identical
/// 7-index frame and 16 render an entirely blank one. What these ROMs
/// test -- INIDISP brightness, forced-blank timing, DMA bugs -- is
/// precisely what law 4 keeps out of the indexed pixel stream, so a
/// picture hash cannot tell any of them apart. Pinning 29 of those would
/// have read as full coverage while proving nothing.
///
/// W7-15's per-dot work created the signal that does discriminate: the
/// sequence of `(line, dot, register, value)` writes that landed during
/// active display. These six ROMs test that signal, and it is exactly
/// what W14-39 was expected to disturb — see the re-pin note below.
///
/// **THE TRADE-OFF, stated because it is real and this choice is
/// reversible.** These hashes pin OUR INSTRUMENTATION rather than
/// rendered output, and they are coupled to [`Ppu::is_segmentable`]: a
/// change to which registers may be replayed changes every hash here.
/// That coupling is deliberate rather than tolerated. A change to what
/// counts as a mid-line write changes what these ROMs are measuring, so
/// these goldens SHOULD fail and be re-examined when it happens -- the
/// failure is the signal, not noise. If it ever proves brittler than it
/// is worth, delete this list: the suite falls back to two pixel goldens
/// and every ROM here returns to EXCLUDED with its measurement intact.
///
/// Unlike the pixel goldens, these do NOT require the ROM to leave forced
/// blank. `inidisp_hammer_8f0f` never does, and its write record is still
/// perfectly well defined -- that is the point of measuring the driving
/// rather than the drawing.
///
/// **Re-pinned 2026-09-20 (ticket W14-39).** Charging real internal
/// cycles corrects the CPU's pacing against the raster (previously ~47%
/// too fast per instruction, this ticket's whole premise), and every one
/// of these ROMs hammers a register in a tight software loop bounded by
/// instruction count, not by frame or master-cycle count — so the same
/// instruction budget now represents MORE real elapsed raster time, and
/// the write record it captures legitimately covers more ground. Every
/// re-pinned record still targets register `$2100` only (`regs=[2100]`
/// under `survey_the_whole_set`), so what changed is timing, not which
/// register this instrumentation sees:
const WRITE_GOLDENS: &[(&str, &str)] = &[
    // Re-pinned 2026-09-20 (ticket W14-47): still exactly ONE write on
    // one line (line 89), but the dot moved from 31 to 30 (n=97819 ->
    // 97822 instructions to reach the settle point) -- traced with
    // `PROBE_IRQLOG` on this exact ROM: it writes `$4200: 00->81`
    // (enabling NMI) at line 244 dot 332, mid-vblank, while `$4210` bit 7
    // is STILL SET from the vblank edge at line 239 (never read in
    // between). Per fullsnes "SNES Interrupts" ("The CPU includes
    // another internal NMI flag, which gets set when '[4200h].7 AND
    // [4210h].7' changes from 0-to-1"), that enable is itself a 0-to-1
    // edge on the AND expression and must dispatch immediately --
    // W14-47's fix does exactly that, one step earlier than the old
    // vblank-edge-only check, which shifts every following instruction's
    // timing by a few cycles and lands this ROM's own (unrelated) $2100
    // write one dot earlier. Confirmed a pure timing shift, not a new
    // register: `regs=[2100]` under `survey_the_whole_set`, unchanged.
    (
        "inidisp_enable_display_mid_frame.sfc",
        // W14-47's enable-edge dispatch moved this hash from its
        // pre-ticket value (see below) to
        // "ec428d365cbc5603c529a1aed56e5397250e8b82bb5237a02c2d10efa14ee9d5"
        // and W14-47's own write-up read that move as confirmation the
        // rule was correct. It was not independent confirmation — this
        // golden hashes a write-record TRACE for self-consistency, not
        // against a real-hardware oracle, so "the golden moved to match
        // the code that just changed" is circular. The W14-47 follow-up
        // ticket (2026-09-20) reverted the enable-edge rule after three
        // real ROMs (The Terminator, Super Black Bass, Magical Drop II)
        // proved it regresses commercial titles; this hash is back to
        // EXACTLY the value pinned before W14-47
        // (`b28b53d`/`b344422632b66c199157f96fbd32908caee491b18ea2b451940296436e8e4d4d`),
        // which is the strongest available confirmation that reverting
        // the rule restored main's own timing rather than coincidentally
        // producing a third, different trace.
        "b344422632b66c199157f96fbd32908caee491b18ea2b451940296436e8e4d4d",
    ),
    // 5,460 writes across all 224 lines (was 1,988 over 70) — now
    // identical to the record `inidisp_hammer_0f00.sfc` used to have,
    // pre-W14-39 (a coincidence: two different ROMs, two different
    // timing regimes, one shared record).
    (
        "inidisp_hammer_0f.sfc",
        "55f75429ff5510f53978bef709a27267dd6921616cd77a5187465c0ef527b9fa",
    ),
    // 3,765 writes across 177 lines -- and now IDENTICAL to
    // `inidisp_hammer_0f0f`, `inidisp_hammer_0f8f_fast` (both still
    // EXCLUDED below as representative-of-this-ROM) AND
    // `inidisp_hammer_0f_long` (previously distinct at 5,460/224; the
    // corrected pacing collapses all four into one record).
    (
        "inidisp_hammer_0f00.sfc",
        "f67caf5b00b2a692486da6dca4a51505b03beb619a48a15d0436132488ec59a9",
    ),
    // 2,225 writes across 146 lines (was 1,724 over 101).
    (
        "inidisp_hammer_0f8f.sfc",
        "4bb02357884c435e0f4be56c4736bcc2fbef5658d8e2f7585a743fc1414b51c1",
    ),
    // 3,765 across 177 lines -- now identical to the 0f00 group above;
    // see that entry's note.
    (
        "inidisp_hammer_0f_long.sfc",
        "e85eac4da4c4845fc5afa26dd95de9af708874216567ac51f1529638a8c3f6eb",
    ),
    // 1,943 writes across 127 lines (was 1,844 over 108) -- and this one
    // still never leaves forced blank.
    (
        "inidisp_hammer_8f0f.sfc",
        "8192ef1db5338b8e78d72b447af29af249126f351e64b317b9d2e7cdc7bd1341",
    ),
];

/// Run a ROM and hash the mid-line writes it drove.
///
/// Deliberately does NOT require the screen to come on: the record is
/// about what the program DROVE, which is well defined even for a ROM
/// that stays in forced blank the whole time.
fn hash_write_record(path: &std::path::Path) -> Option<String> {
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
    for _ in 0..SETTLE {
        let _ = s.step();
    }
    let mut h = Sha256::new();
    for line in 0..240u16 {
        for (dot, addr, val) in s.bus.ppu.line_writes_for_test(line) {
            h.update(line.to_le_bytes());
            h.update(dot.to_le_bytes());
            h.update(addr.to_le_bytes());
            h.update([val]);
        }
    }
    use std::fmt::Write;
    Some(h.finalize().iter().fold(String::new(), |mut a, b| {
        let _ = write!(a, "{b:02x}");
        a
    }))
}

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
    // The write-record dimension. See WRITE_GOLDENS for why it exists and
    // what it costs.
    for (name, expected) in WRITE_GOLDENS {
        let path = dir.join(name);
        if !path.exists() {
            eprintln!("SKIP {name}: not fetched");
            continue;
        }
        let Some(actual) = hash_write_record(&path) else {
            failures.push(format!("{name}: would not run"));
            continue;
        };
        // Same stability rule as the pixel goldens: a record captured
        // from a run that is not reproducible is a coin flip, not a pin.
        let again = hash_write_record(&path).expect("second run");
        if again != actual {
            failures.push(format!("{name}: write record not stable across two runs"));
            continue;
        }
        eprintln!("{name}: write record {actual}");
        if actual != *expected {
            failures.push(format!(
                "{name} (write record)\n    expected {expected}\n    actual   {actual}\n    \
                 If Ppu::is_segmentable changed, this SHOULD fail: it means what counts \
                 as a mid-line write changed, so what this ROM measures changed too."
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
            "{name} | left={left} mode={} idx={} mid={mid} lines={lines_with} regs={:04X?}\n    pix {hex}\n    wri {whex}",
            s.bus.ppu.bg_mode,
            distinct.len(),
            regs.iter().collect::<Vec<_>>()
        );
    }
}
