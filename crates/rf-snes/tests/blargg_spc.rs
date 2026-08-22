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
//! **Cause 3, IDENTIFIED — and it is not a bug.** With 1 and 2 fixed, the
//! SPC700 sits at exactly ONE PC forever: `$0495`, in all four ROMs, for
//! all 10,000,000 sampled instructions (`distinct SPC PCs: 1`). The
//! uploaded code there is:
//!
//! ```text
//! $0490: E5 C0 FF    MOV A, !$FFC0    ; read IPL ROM byte 0
//! $0493: 68 CD       CMP A, #$CD      ; the real IPL's first byte
//! $0495: D0 FE       BNE $0495        ; spin forever otherwise
//! $0497: 5F C0 FF    JMP !$FFC0       ; ...and then EXECUTE the IPL
//! ```
//!
//! It reads `$EF` — [`rf_snes::apu::IPL_STUB`] is `[0xEF; 64]`, ours, not
//! Nintendo's, per law 5 ("no ROM bytes in git") and Brad's 2026-08-20
//! ruling that extended it to "not in a fetch list either".
//!
//! **`apu/mod.rs`'s own doc predicted this and got one word wrong.** It
//! says the cost is "a program that reads IPL bytes *as data* sees our
//! stub, not the real ROM. No commercial title is known to do that." The
//! commercial part is still true. The population was never commercial
//! titles: **test ROMs do it, and test ROMs are the things that verify
//! this emulator.** These four read it as data AND jump to it.
//!
//! So these ROMs cannot pass without a real SPC700 boot ROM, and this
//! project will never contain one. The 2026-08-20 ruling recorded
//! "requiring the user to supply it" as still open, and
//! [`rf_snes::apu::Apu::set_ipl_rom`] is that door: point
//! `RF_SPC_IPL_ROM` at a 64-byte dump of your own console's IPL and this
//! suite runs. Without it the suite SKIPS rather than fails, because a
//! missing artifact is not a defect — the same posture every other
//! artifact-gated suite here takes.
//!
//! ```text
//! cargo run -p rf-harness --bin fetch-test-roms -- \
//!     blargg-spc-dsp6 blargg-spc-smp blargg-spc-timer blargg-spc-mem-access-times
//! RF_SPC_IPL_ROM=/path/to/your/spc700.rom \
//!     cargo test -p rf-snes --release --test blargg_spc -- --ignored
//! ```
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

/// A 64-byte SPC700 boot ROM the USER supplied, if they did.
///
/// Never shipped, never fetched — see the module doc. Anything other than
/// exactly 64 readable bytes is treated as absent rather than padded,
/// because a half-right IPL fails in ways that look like emulator bugs.
fn user_ipl() -> Option<[u8; rf_snes::apu::IPL_LEN]> {
    let path = std::env::var("RF_SPC_IPL_ROM").ok()?;
    let bytes = std::fs::read(path).ok()?;
    <[u8; rf_snes::apu::IPL_LEN]>::try_from(bytes.as_slice()).ok()
}

/// Run one ROM to the instruction budget and return what it printed.
fn run(path: &std::path::Path, ipl: [u8; rf_snes::apu::IPL_LEN]) -> String {
    let bytes = std::fs::read(path).expect("rom readable");
    let mut s = SnesSystem::load(&bytes).expect("blargg's SPC ROMs are plain LoROM carts");
    s.bus.apu.set_ipl_rom(ipl);
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
#[ignore = "local: needs the fetched ROMs and a user-supplied RF_SPC_IPL_ROM"]
fn blargg_spc_tests_report_success() {
    let Some(dir) = rom_dir() else {
        eprintln!(
            "SKIP: cargo run -p rf-harness --bin fetch-test-roms -- \
             blargg-spc-dsp6 blargg-spc-smp blargg-spc-timer blargg-spc-mem-access-times"
        );
        return;
    };

    let Some(ipl) = user_ipl() else {
        eprintln!(
            "SKIP: these ROMs read $FFC0 as data, compare it against $CD and \n\
             jump to it, so they need a REAL SPC700 boot ROM. This project \n\
             never ships or downloads one (law 5). Point RF_SPC_IPL_ROM at a \n\
             64-byte dump of your own console's IPL to run them."
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
        let text = run(&path, ipl);
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
        "blargg's SPC ROMs did not report a pass.\n  {}",
        failures.join("\n  ")
    );
}
