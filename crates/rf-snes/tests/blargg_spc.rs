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
//! **Why.** The S-DSP is not connected to the SPC700's bus. In
//! `apu/mod.rs`'s `Apu::write_register` there is no `0xF3` arm at all, and
//! the read arm is a literal `0xF3 => 0`. `$F2`/`$F3` are the ONLY way an
//! SPC700 program reaches a DSP register, so no game and no test ROM can
//! program one, and every read-back is zero. blargg's echo test writes
//! its setup registers into the void and waits for a value that cannot
//! arrive.
//!
//! This was first found statically during W7-09 and recorded on W7-08's
//! ticket; it is repeated here because **an outside ROM has now confirmed
//! it independently**, and named the exact test that dies. That is a
//! materially stronger claim than a `grep`.
//!
//! **What is NOT yet established.** All four ROMs stall, each on its own
//! first test:
//!
//! | ROM | last line printed |
//! |---|---|
//! | `spc_dsp6` | `Echo/basics` |
//! | `spc_smp` | `CPU Instructions/Edge arith` |
//! | `spc_timer` | `timer read vs write` |
//! | `spc_mem_access_times` | `mem access times` |
//!
//! The `$F2`/`$F3` gap explains `spc_dsp6` directly. It is a plausible
//! common cause for the rest — a shared harness that programs the DSP
//! before running anything would stall them all identically — but that
//! has NOT been traced, and `spc_smp` stalling inside a CPU-instruction
//! test is the kind of detail that turns a tidy single-cause story into a
//! wrong one. Isolate each before believing one fix covers four.
//!
//! **What unblocking it needs.** A 128-byte S-DSP register file and the
//! `$F2`/`$F3` plumbing that reads and writes it. `Dsp` today holds
//! structured state (`voices`, `noise`, `echo`, plus a few named
//! registers) with no register-map layer, so this is real work, not a
//! wiring one-liner. Until it exists, criterion 1's five DSP features are
//! unreachable from a running machine however well their unit tests pass
//! — which is exactly the "implemented and tested is not verified against
//! the machine" distinction W7-08's own notes call its remaining content.
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
#[ignore = "KNOWN-RED: the S-DSP is not wired to $F2/$F3 — see this module's doc"]
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
