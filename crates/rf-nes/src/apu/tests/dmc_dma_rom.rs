//! blargg's `dmc_dma_during_read4` suite (ticket W2-01b, FR-CORE-024) — the
//! five ROMs that measure what a DMC DMA does to a CPU read with side
//! effects.
//!
//! ## These ROMs speak neither `$6000` nor a pass/fail protocol
//!
//! `tests/rom-manifest.toml` tags this suite `protocol = "six_thousand"`,
//! but its PRG-RAM stays all-zero for every one of the five: they are from
//! blargg's older, screen-only shell, so **their results exist only in the
//! PPU nametable**. This is the same class of protocol mistag W1-05b found
//! on `sprite_hit_tests`; the manifest row is left alone (out of this
//! ticket's write scope) and this test reads the rendered text instead, via
//! `NesBus::ppu_vram_for_test`. Two of the ROMs do print `Passed`/`Failed`
//! themselves; the other three print only hex plus a CRC, which their source
//! headers list accepted values for.
//!
//! **Why several outputs are accepted per ROM**: the headers say so —
//! `dma_2007_read`'s is "Number of extra reads depends in [sic] CPU-PPU
//! synchronization at reset", `double_2007_read`'s is "Output (depends on
//! CPU-PPU synchronization)". A real console powers up with a random
//! CPU/APU/PPU alignment; this crate is deterministic (ARCHITECTURE §3) and
//! picks one, so the assertion is "our output is one of the variants the
//! author documented as correct", never "the one variant we happen to
//! produce".
//!
//! ## Status: 5 of 5 (W2-01c, W2-01d)
//!
//! This suite is W2-01b's acceptance criterion and it is **not met**; the
//! ticket carries the block note, `crates/rf-harness/waivers.toml` the
//! waiver. What passes and what does not is asserted below rather than
//! described, so a future fix cannot quietly regress the three that work:
//!
//! - `dma_2007_read` — matches documented variant 2 (`44 55`, CRC
//!   `5E3DF9C4`) exactly.
//! - `dma_2007_write` — the ROM prints `Passed`. (Its header's expected hex
//!   is from an older build and no longer matches what the ROM itself
//!   accepts; the ROM's own verdict is the authority.)
//! - `read_write_2007` — prints `Passed`, and its hex matches the header.
//! - `dma_4016_read` — prints `Passed` (`08 08 07 08 08`) **as of ticket
//!   W2-01c**. The fix was not in the DMA model at all: the ROM's `end:`
//!   routine counts how many reads it takes the pad to return 1, so it is
//!   measuring shifted BITS, not bus reads. A standard controller's shift
//!   register is clocked by the EDGE of the read strobe, and the three
//!   back-to-back `$4016` reads a DMC halt puts on the bus hold one strobe
//!   asserted — one edge, one extra bit, which is exactly the `07` this
//!   ROM wants. See `NesBus::last_joy_read_cycle`.
//! - `double_2007_read` — CRC `85CFD627`, the first of the four accepted,
//!   **as of ticket W2-01d**. The ROM provokes it with `lda $20F7,x` where
//!   `x = $10`: `$20F7 + $10 = $2107` crosses a page, so the 6502 issues
//!   its dummy read at `$2007` and the real read at `$2107` (also `$2007`
//!   after mirroring) on back-to-back cycles. The second read reports the
//!   value the first already presented, while the buffer and `v` still
//!   advance twice — see `Ppu::last_2007_read_dot`.

use std::path::{Path, PathBuf};

use crate::cpu::Cpu;
use crate::system::NesBus;

/// Runs `rom` for `frames` frames and returns the text it printed to the
/// screen: the nametable's 30 rows of 32 tiles, decoded as ASCII (blargg's
/// console writes character codes straight into the nametable) and
/// whitespace-normalized.
fn screen_text(rom_path: &Path, frames: u32) -> String {
    let rom_bytes = std::fs::read(rom_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", rom_path.display()));
    let mut bus = NesBus::from_ines_bytes(&rom_bytes)
        .unwrap_or_else(|e| panic!("invalid rom image {}: {e}", rom_path.display()));
    let mut cpu = Cpu::power_on(&mut bus);

    for _ in 0..frames {
        let start_frame = bus.frame_count();
        while bus.frame_count() == start_frame {
            cpu.step(&mut bus);
        }
    }

    let vram = bus.ppu_vram_for_test();
    let text: String = (0..30 * 32)
        .map(|i| {
            let tile = vram[i];
            if (0x20..0x7F).contains(&tile) {
                tile as char
            } else {
                ' '
            }
        })
        .collect();
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn resolve(name: &str) -> Option<PathBuf> {
    let rel = format!("../../roms/nes/dmc_dma_during_read4/{name}.nes");
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    path.is_file().then_some(path)
}

/// What each ROM must print, transcribed from its own source header
/// (`nes-test-roms/dmc_dma_during_read4/source/*.s`) or, where the ROM
/// self-reports, from its own verdict. `expected_pass` records this
/// engine's measured status: see the module doc for the two that fail and
/// why they are separate problems.
struct Rom {
    name: &'static str,
    /// Any one of these substrings appearing in the screen text is a pass.
    accepted: &'static [&'static str],
    expected_pass: bool,
}

const ROMS: &[Rom] = &[
    // "DMC DMA during $2007 read causes 2-3 extra $2007 reads before real
    // read." Two documented variants; this engine lands on the second.
    Rom {
        name: "dma_2007_read",
        accepted: &["159A7A8F", "5E3DF9C4"],
        expected_pass: true,
    },
    // "DMC DMA during $2007 write has no effect." The ROM self-reports, and
    // its verdict is the authority over its header's older expected hex.
    Rom {
        name: "dma_2007_write",
        accepted: &["Passed"],
        expected_pass: true,
    },
    // "DMC DMA during $4016 read causes extra $4016 read." Needs exactly
    // one extra BIT (`07`) -- fixed in W2-01c by the edge-triggered
    // shift-clock model; see this module's doc.
    Rom {
        name: "dma_4016_read",
        accepted: &["Passed", "08 08 07 08 08"],
        expected_pass: true,
    },
    // "Double read of $2007 sometimes ignores extra read, and puts odd
    // things into buffer" -- a PPU behaviour, not a DMA one. Implemented
    // in ticket W2-01d; this engine now lands on the FIRST of the four
    // documented variants (`22 44 55 66 77`, CRC 85CFD627).
    Rom {
        name: "double_2007_read",
        accepted: &["85CFD627", "F018C287", "440EF923", "E52F41A5"],
        expected_pass: true,
    },
    // "Read of $2007 just before write behaves normally."
    Rom {
        name: "read_write_2007",
        accepted: &["Passed", "33 11 22 33 09 55 66 77"],
        expected_pass: true,
    },
];

/// Runs each ROM and asserts the measured status EXACTLY: the three that
/// pass must keep passing (a regression there is a real bug), and the two
/// that fail must keep failing for the reason recorded -- if one starts
/// passing, this test fails too, because that means the block note is stale
/// and the ticket can move.
#[test]
fn dmc_dma_during_read4_all_five_pass() {
    let mut missing = 0;
    let mut surprises = Vec::new();
    for rom in ROMS {
        let Some(path) = resolve(rom.name) else {
            missing += 1;
            continue;
        };
        let text = screen_text(&path, 600);
        let passed = rom.accepted.iter().any(|a| text.contains(a));
        eprintln!(
            "dmc_dma_during_read4/{}: {} -- {text:?}",
            rom.name,
            if passed { "PASS" } else { "FAIL" }
        );
        if passed != rom.expected_pass {
            surprises.push(format!(
                "{}: expected {} but measured {} -- {text:?}",
                rom.name,
                if rom.expected_pass { "PASS" } else { "FAIL" },
                if passed { "PASS" } else { "FAIL" }
            ));
        }
    }

    if missing == ROMS.len() {
        eprintln!(
            "SKIP dmc_dma_during_read4: roms/nes/dmc_dma_during_read4/*.nes not found. Fetch \
             them first: scripts/fetch-test-roms.sh"
        );
        return;
    }
    assert_eq!(missing, 0, "partial fetch: {missing} of 5 ROMs missing");
    assert!(
        surprises.is_empty(),
        "measured status differs from what W2-01b's block note records:
{}",
        surprises.join(
            "
"
        )
    );
}
