//! blargg's recovered SPC test ROMs — the designated SPC set for ticket
//! W7-08 criterion 3.
//!
//! Provenance and byuu's caveat about what passing these proves live in
//! `tests/rom-manifest.toml`'s `blargg-spc-*` block. The short version:
//! this verifies against blargg's DSP core, the reference implementation,
//! not against silicon.
//!
//! # THIS TEST IS KNOWN-RED, AND THAT IS THE POINT
//!
//! It does not pass today and it is not flaky. It is the acceptance test
//! criterion 3 designates, written before the thing it tests works, so
//! that the ticket has an oracle instead of an opinion.
//!
//! **What it found.** `spc_dsp6.sfc` boots, prints `"Running tests:"`,
//! prints `"Echo/basics"`, and then never advances — the screen is
//! byte-identical at 20M, 40M and 60M instructions while the frame
//! counter keeps climbing, so the 65816 is running and the test is not.
//!
//! **First cause, FIXED 2026-08-21.** The S-DSP was not connected to the
//! SPC700's bus at all: `Apu::write_register` had no `0xF3` arm and its
//! read arm was a literal `0xF3 => 0`. `$F2`/`$F3` are the ONLY path from
//! an SPC700 program to a DSP register, so nothing could program one and
//! every read-back was zero. There is now a 128-byte register file, the
//! `$F2`/`$F3` plumbing, and a 32-cycle sample clock driving the mixer.
//! **It did not make these ROMs pass** — which is exactly why the ticket
//! note said not to assume one cause covered four.
//!
//! **Second cause, OPEN — and it is the IPL boot HLE, not the DSP.**
//! Profiling `spc_dsp6` at 20M instructions shows both processors alive
//! and waiting on each other:
//!
//! * the 65816 spins in a two-instruction loop at `$00:815C`/`$815F`
//!   (9.5M hits each) — a port-wait;
//! * the SPC700's PC is up at `$C11F`, where **ARAM is all zeros**. It is
//!   NOP-sliding through empty memory, because `$00` is a NOP;
//! * `boot.is_running()` is true and `ipl_enabled` is false;
//! * ports read `apu->cpu = [CC, BB, 00, 00]` with the CPU having written
//!   counter `$00` and data `$20`.
//!
//! So `IplBoot::cpu_wrote` took its `BootState::Ready` "just run" branch —
//! the one that fires when `ports_in[1] == 0` at the moment `$CC` lands —
//! and jumped to an address nothing had been uploaded to. blargg's
//! uploader then waits forever for a byte counter that will never be
//! echoed. All four ROMs share that uploader, which fits them all
//! stalling; it has NOT been confirmed as the cause for all four.
//!
//! **What unblocking needs now.** Trace what blargg's uploader actually
//! writes to `$2140`-`$2143` and in what order, then fix
//! `IplBoot::cpu_wrote` to match. Do NOT guess at the protocol — the
//! previous pass of this ticket already burned a cycle on a plausible
//! port-protocol theory that was true but not actionable. Log the real
//! write sequence first.
//!
//! ```text
//! cargo run -p rf-harness --bin fetch-test-roms -- \
//!     blargg-spc-dsp6 blargg-spc-smp blargg-spc-timer blargg-spc-mem-access-times
//! cargo test -p rf-snes --release --test blargg_spc -- --ignored
//! ```

use rf_snes::SnesSystem;

/// Generous on purpose: the ROM is stuck, not slow, and this is what
/// proves it.
const MAX_INSTRUCTIONS: u64 = 60_000_000;

/// Tilemap word the ROMs write their status line to. **Found by dumping
/// VRAM as ASCII, not assumed** — the first kilowords hold 1bpp font
/// glyph data that reads as convincing garbage, and the text tilemap
/// starts well after them.
const STATUS_WORD: u16 = 0x0B60;
const STATUS_LEN: usize = 32 * 8;

const ROMS: &[&str] = &[
    "spc_dsp6.sfc",
    "spc_smp.sfc",
    "spc_timer.sfc",
    "spc_mem_access_times.sfc",
];

fn rom_dir() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("RF_BLARGG_SPC") {
        return Some(p.into());
    }
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../roms/snes/blargg-spc-6");
    p.exists().then_some(p)
}

/// The status area as ASCII, unprintables blanked and blank runs
/// collapsed so a screen dump reads as one line.
fn screen(s: &SnesSystem) -> String {
    let raw: String = s
        .bus
        .vram_low_bytes(STATUS_WORD, STATUS_LEN)
        .iter()
        .map(|&b| {
            if (0x20..0x7F).contains(&b) {
                b as char
            } else {
                ' '
            }
        })
        .collect();
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Run one ROM to the instruction budget and return what it printed.
fn run(path: &std::path::Path) -> String {
    let bytes = std::fs::read(path).expect("rom readable");
    let mut s = SnesSystem::load(&bytes).expect("blargg's SPC ROMs are plain LoROM carts");
    let mut ran = 0u64;
    // `ran` advances unconditionally on every path through the body
    // (law 8); the early exit is a `break`, never a `continue` that could
    // skip it.
    while ran < MAX_INSTRUCTIONS {
        if s.step().is_err() {
            break;
        }
        ran += 1;
    }
    screen(&s)
}

#[test]
#[ignore = "KNOWN-RED: the IPL boot HLE mis-takes its just-run branch — see this module's doc"]
fn blargg_spc_tests_report_success() {
    let Some(dir) = rom_dir() else {
        eprintln!(
            "SKIP: cargo run -p rf-harness --bin fetch-test-roms -- \
             blargg-spc-dsp6 blargg-spc-smp blargg-spc-timer blargg-spc-mem-access-times"
        );
        return;
    };

    let mut failures = Vec::new();
    for name in ROMS {
        let path = dir.join(name);
        if !path.exists() {
            eprintln!("SKIP {name}: not fetched");
            continue;
        }
        let text = run(&path);
        eprintln!("{name}: {text:?}");
        // blargg's convention, and the manifest's `screen_text` protocol:
        // the ROM's own printed output is the verdict. A run that never
        // reaches a verdict is a FAILURE, not an inconclusive — that
        // distinction is the whole reason this test is red rather than
        // quietly skipping.
        if !text.to_ascii_lowercase().contains("passed") {
            failures.push(format!("{name}: {text:?}"));
        }
    }

    assert!(
        failures.is_empty(),
        "blargg's SPC ROMs did not report a pass.\n  {}\n\nIf the first is \
         spc_dsp6 stuck on \"Echo/basics\", that is the documented cause: \
         Apu::write_register has no $F3 arm and its read arm returns a \
         literal 0, so the S-DSP is unreachable from the SPC700. See this \
         module's doc.",
        failures.join("\n  ")
    );
}
