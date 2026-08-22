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
//! **Cause 1, FIXED.** The S-DSP was not connected to the SPC700's bus:
//! `Apu::write_register` had no `0xF3` arm and its read arm was a literal
//! `0xF3 => 0`. There is now a 128-byte register file, the `$F2`/`$F3`
//! plumbing, and a 32-cycle sample clock driving the mixer. It did not
//! make these ROMs pass.
//!
//! **Cause 2, FIXED — and it was never the DSP.** The HLE boot handshake
//! was **edge-triggered on the CPU's store**. blargg's uploader starts a
//! transfer with a 16-bit `STA $2140`, which lands as two byte writes,
//! port 0 first — traced, not assumed:
//!
//! ```text
//! idx=0 val=CC  ports_in before [00, 00, 00, 04]
//! idx=1 val=01  ports_in before [CC, 00, 00, 04]
//! ```
//!
//! So the handshake read the "kind" byte one instruction early, saw `0`,
//! and took the "transfer nothing, just run" branch — jumping to `$0400`
//! with nothing uploaded there. The SPC700 NOP-slid through empty ARAM
//! while the 65816 spun at `$00:815C` waiting for a counter echo that
//! could never come. `IplBoot::poll` is now level-triggered on the APU's
//! clock, which is what the real IPL is: a polling program that cannot
//! observe a half-finished write. The upload now runs to completion and
//! the SPC executes blargg's code.
//!
//! **Cause 3, OPEN.** With both fixed, the machine gets much further and
//! still does not finish. What is established:
//!
//! * the handshake completes and the SPC700 runs the uploaded program;
//! * there is heavy two-way port traffic — 9,009 port-state changes in
//!   8M instructions — so both processors are live and conversing;
//! * every ROM still prints only its FIRST test name, unchanged at 40M,
//!   80M, 120M, 160M and 200M instructions.
//!
//! What that rules out: a dead SPC, a failed upload, and a stalled
//! handshake. What it does not identify: why the first test never
//! completes. Nothing beyond the above has been traced, so **do not
//! assume cause 3 is one bug, and do not assume it is the same bug in
//! all four ROMs.** Being wrong about that twice is what this file's
//! history already records.
//!
//! **What unblocking needs now.** Find where the SPC700 is spinning
//! inside blargg's own test code — a PC histogram over the uploaded
//! program, the way the port trace found cause 2 — and what it is waiting
//! for. Trace first; every cause so far has been something other than the
//! plausible one.
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
#[ignore = "KNOWN-RED: cause 3 open — boot and DSP are fixed, tests still do not finish"]
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
