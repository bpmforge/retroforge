//! blargg's recovered SPC test ROMs — the designated SPC set for ticket
//! W7-08 criterion 3.
//!
//! Provenance and byuu's caveat about what passing these proves live in
//! `tests/rom-manifest.toml`'s `blargg-spc-*` block. The short version:
//! this verifies against blargg's DSP core, the reference implementation,
//! not against silicon.
//!
//! # THIS TEST IS RED, AND WHAT IT NOW REPORTS IS THE VALUE
//!
//! It does not pass. It is also no longer a hang: these ROMs execute
//! their real test content and report their own verdicts, which is the
//! first external oracle this project's S-DSP and SPC timing have ever
//! had. What they say (2026-08-23):
//!
//! | ROM | reports |
//! |---|---|
//! | `spc_dsp6` | `Running tests: Echo/basics  Failed 03` |
//! | `spc_smp` | `E0 AB34EA3A  E4 05599DAA  ED 442B1C4D  Passed 01` |
//! | `spc_timer` | `timer read vs write 1111111222` |
//! | `spc_mem_access_times` | `E6 -- -- R1  E7 -- -- R0 ... EB -- -- R0` |
//!
//! Those are accuracy results, not infrastructure failures. `spc_dsp6`
//! ran the echo test and judged it wrong; `spc_smp` passed one group and
//! printed hashes for three more. Making them green is DSP and timing
//! accuracy work, and the ticket stays open for it.
//!
//! # Three causes stood between "hangs" and "reports", all fixed
//!
//! **Cause 1 — the S-DSP was not connected to the bus.**
//! `Apu::write_register` had no `0xF3` arm and its read arm was a literal
//! `0xF3 => 0`, so no SPC700 program could reach a DSP register. There is
//! now a 128-byte register file, the `$F2`/`$F3` plumbing, and a 32-cycle
//! sample clock. It did not make these ROMs pass, which is why the ticket
//! warned against assuming one cause covered four.
//!
//! **Cause 2 — the boot handshake was edge-triggered.** blargg's uploader
//! starts a transfer with a 16-bit `STA $2140`, which lands as two byte
//! writes, port 0 first — traced, not assumed:
//!
//! ```text
//! idx=0 val=CC  ports_in before [00, 00, 00, 04]
//! idx=1 val=01  ports_in before [CC, 00, 00, 04]
//! ```
//!
//! So the handshake read the "kind" byte one instruction early, saw `0`,
//! and took the "just run" branch — jumping to `$0400` with nothing
//! uploaded there. `IplBoot::poll` is now level-triggered on the APU's
//! clock, which is what the real IPL is: a polling program that cannot
//! observe a half-finished write.
//!
//! **Cause 3 — the ROMs need a boot ROM, and this project has none.**
//! They read `$FFC0` as DATA, compare it against `$CD`, spin forever
//! otherwise, and then `JMP !$FFC0` to execute it. Solved WITHOUT one:
//! `IPL_STUB` byte 0 is `$CD` (a compatibility constant programs read,
//! not borrowed code), and `Apu::reenter_ipl` HLEs the jump-back — a real
//! boot ROM re-runs its handshake on re-entry, and so does this. No
//! Nintendo bytes, no 64 bytes of clean-room assembly whose faithful
//! reimplementation would plausibly converge on the original anyway.
//!
//! A real dump still overrides the stub via `RF_SPC_IPL_ROM` if you have
//! one, but nothing requires it.
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
const STATUS_LEN: usize = 32 * 20;

const ROMS: &[&str] = &[
    "spc_dsp6.sfc",
    "spc_smp.sfc",
    "spc_timer.sfc",
    "spc_mem_access_times.sfc",
];

/// The banner blargg's ROMs print once, after ALL their subtests pass.
///
/// Per-subtest lines read `Passed NN` / `Failed NN` with a running count,
/// so matching on "passed" alone reports a whole ROM green when only its
/// first subtest ran.
const FINAL_BANNER: &str = "PASSED TESTS";

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
fn run(path: &std::path::Path, ipl: Option<[u8; rf_snes::apu::IPL_LEN]>) -> String {
    let bytes = std::fs::read(path).expect("rom readable");
    let mut s = SnesSystem::load(&bytes).expect("blargg's SPC ROMs are plain LoROM carts");
    if let Some(rom) = ipl {
        s.bus.apu.set_ipl_rom(rom);
    }
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
#[ignore = "local: needs the fetched ROMs; RED on real accuracy gaps, see module doc"]
fn blargg_spc_tests_report_success() {
    let Some(dir) = rom_dir() else {
        eprintln!(
            "SKIP: cargo run -p rf-harness --bin fetch-test-roms -- \
             blargg-spc-dsp6 blargg-spc-smp blargg-spc-timer blargg-spc-mem-access-times"
        );
        return;
    };

    // No boot ROM required any more. The built-in stub carries the one
    // byte these ROMs read as data ($CD), and `Apu::reenter_ipl` HLEs the
    // jump-back-to-$FFC0 that they use to request another upload. A real
    // dump can still be supplied to override it.
    let ipl = user_ipl();

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
        //
        // **THE VERDICT IS THE FINAL BANNER, NOT ANY "Passed".** This
        // check used to be `contains("passed")` case-insensitively, and
        // that was wrong in a way that reported success: these ROMs run
        // MANY subtests and print `Passed NN` after each one, where NN is
        // a running count. `spc_dsp6.sfc` alone carries Echo/basics,
        // Echo/esa_changes, Echo/edl_changes, Echo/wrap_around,
        // Echo/zero_length, Echo/echo calc, Echo/edl 0 quirk,
        // Echo/edl lengths and Envelope/envelope rates. A run that
        // finished the FIRST of those and then stalled printed
        // "Echo/basics Passed 01" — and the old check called that a pass
        // for the whole ROM.
        //
        // The ROM prints `PASSED TESTS` once, at the end, when every
        // subtest has passed. That is the only string that means what
        // this test claims.
        if !text.contains(FINAL_BANNER) {
            failures.push(format!("{name}: {text:?}"));
        }
    }

    assert!(
        failures.is_empty(),
        "blargg's SPC ROMs did not report a pass.\n  {}",
        failures.join("\n  ")
    );
}
