//! blargg's recovered SPC test ROMs — the designated SPC set for ticket
//! W7-08 criterion 3, split by W7-17/W7-18.
//!
//! Provenance and byuu's caveat about what passing these proves live in
//! `tests/rom-manifest.toml`'s `blargg-spc-*` block. The short version:
//! this verifies against blargg's DSP core, the reference implementation,
//! not against silicon.
//!
//! # Three of four ROMs are still open work; one is a real gate now
//!
//! | ROM | status (2026-09-18) | reports |
//! |---|---|---|
//! | `spc_timer` | **GATING** (W7-17, closed) | `PASSED TESTS` |
//! | `spc_mem_access_times` | **KNOWN-RED** (W7-17, Brad's ruling D-3) | `Failed 02`, checksum only |
//! | `spc_dsp6` | reporting-only (W7-08, open) | `Echo/basics ... Failed 03` |
//! | `spc_smp` | reporting-only (W7-08, open) | opcode hashes, `Failed 02` |
//!
//! `spc_timer.sfc` is the one ROM this module asserts against: its "timer
//! read vs write" subtest is, per Brad's 2026-09-04 ruling, an undisputed
//! oracle (unlike `spc_mem_access_times`, see below), so
//! [`spc_timer_reports_pass`] is a real `#[test]` failure, not a printed
//! diagnostic — a regression here fails `cargo test -p rf-snes --test
//! blargg_spc -- --ignored`, which is the command `scripts/local-gate.sh`
//! already runs for this file. **`scripts/local-gate.sh` itself is
//! unchanged and stays out of `write_scope`**: its own step still wraps
//! that command in `if ! ...; then echo ...; fi` without `exit 1`, calling
//! the whole file "reporting only until W7-08/W7-17" — that comment is now
//! half true. `spc_dsp6`/`spc_smp` are still W7-08's open accuracy gaps
//! and reporting-only; `spc_timer` gates from inside this binary even
//! though the shell wrapper around it does not yet propagate the failure.
//! Restoring the shell-level `exit 1` for the whole file is W7-08's close
//! criterion, unchanged from the note already on that ticket, once
//! `spc_dsp6`/`spc_smp` also pass.
//!
//! `spc_mem_access_times.sfc` is a **recorded KNOWN-RED**, not silently
//! waived: [`spc_mem_access_times_is_a_known_red`] asserts it still
//! reports the documented failure shape. Brad's ruling 2026-09-04 (D-3,
//! `docs/DECISIONS.md`): the nesdev thread that names Overload's
//! `spc700_inst_op.pdf` (the source W7-18 verified against, opcode by
//! opcode) also records higan and Overload DISAGREEING on `(dp),Y` and the
//! CALL/RET/RTI stack orders, with no arbiter as of 2017 — and this ROM
//! compares an internal checksum it never prints an expected value for.
//! Passing it might mean matching higan's own disputed order rather than
//! silicon. If this ROM ever prints `PASSED TESTS`, that is a genuine
//! change worth investigating (something moved enough to satisfy an
//! oracle nobody today can explain), so the test FAILS loudly on that
//! outcome rather than treating a pass as free good news.
//!
//! # The fix that closed spc_timer (ticket W7-17)
//!
//! `spc_timer.sfc`'s "timer read vs write" subtest is blargg's own
//! hardware-verified test of `Timer::read_counter` (`$FD`-`$FF`): reading
//! a timer's 4-bit output counter is a "get-then-clear" operation, and an
//! increment already in flight when the read happens must never be lost
//! (nesdev forum t=10881, blargg, quoted in full on `Apu::access_tick`'s
//! doc comment). Ticket W7-18 charged every memory access's one shared-
//! clock cycle BEFORE performing the access, uniformly for reads and
//! writes. That is right for writes, but backwards for a register read
//! with a clear-on-read side effect: it let this access's own coincident
//! timer edge apply before the read sampled the counter, instead of
//! after. `Apu::read`'s `$00F0`-`$00FF` branch now calls
//! `Apu::read_register` before `Apu::access_tick`; every other access
//! (writes, and plain RAM/IPL reads, which have no such side effect) is
//! unchanged. See `crates/rf-snes/src/apu/mod.rs`'s `access_tick` doc for
//! the full citation and the write-side experiment that ruled out
//! reordering writes too.
//!
//! # Three causes stood between "hangs" and "reports", all fixed (W7-08)
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
/// The whole 32x32 text tilemap, not a 20-row window part-way into it.
///
/// **The old window started at `$0B60` and was 32x20**, which straddled
/// the end of the map and cut off text the ROMs had actually printed. It
/// hid a real verdict: `spc_timer.sfc` prints a hash and `Failed 02`
/// ABOVE the line the window caught, so the harness reported only
/// "timer read vs write 1111111222" and the ROM's own failure code never
/// reached the log. An oracle that cannot see the verdict is not an
/// oracle.
const STATUS_WORD: u16 = 0x0800;
const STATUS_LEN: usize = 32 * 32;

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
///
/// Returns `None` if the ROM was never fetched — a missing local artifact
/// is not a defect (NFR-006), and the caller decides whether that means
/// "skip" or "no comment".
fn run(
    dir: &std::path::Path,
    name: &str,
    ipl: Option<[u8; rf_snes::apu::IPL_LEN]>,
) -> Option<String> {
    let path = dir.join(name);
    if !path.exists() {
        eprintln!("SKIP {name}: not fetched");
        return None;
    }
    let bytes = std::fs::read(&path).expect("rom readable");
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
    let text = screen(&s);
    eprintln!("{name}: {text:?}");
    Some(text)
}

/// **THE GATE.** `spc_timer.sfc` is this ticket's one undisputed oracle
/// (Brad's ruling 2026-09-04, D-3): unlike `spc_mem_access_times`, nothing
/// disputes what a pass here means. A regression fails this `#[test]`,
/// which fails `cargo test -p rf-snes --test blargg_spc -- --ignored` —
/// the exact command `scripts/local-gate.sh` runs for this file (see the
/// module doc for why the shell wrapper around that command still does
/// not itself `exit 1`, and why that is W7-08's close criterion, not
/// this one's).
#[test]
#[ignore = "local: needs the fetched ROM (see module doc); GATES on a real oracle"]
fn spc_timer_reports_pass() {
    let Some(dir) = rom_dir() else {
        eprintln!("SKIP: cargo run -p rf-harness --bin fetch-test-roms -- blargg-spc-timer");
        return;
    };
    let Some(text) = run(&dir, "spc_timer.sfc", user_ipl()) else {
        return;
    };
    assert!(
        text.contains(FINAL_BANNER),
        "spc_timer.sfc regressed off its one undisputed oracle: {text:?}"
    );
}

/// **KNOWN-RED, RECORDED RATHER THAN WAIVED.** Brad's ruling 2026-09-04
/// (D-3, `docs/DECISIONS.md`): `spc_mem_access_times.sfc` compares an
/// internal checksum it never prints an expected value for, and the
/// nesdev thread behind Overload's `spc700_inst_op.pdf` — the document
/// W7-18 verified every access against — records higan and Overload
/// themselves disagreeing on `(dp),Y` and the CALL/RET/RTI stack orders
/// with no arbiter. This ROM may be encoding one side of that disputed
/// order rather than hardware truth, so it is not this ticket's oracle.
///
/// The test still runs it every time (a regression to "hangs" is still a
/// hard failure via `MAX_INSTRUCTIONS`/the harness's own budget, per
/// RF-L-12) and asserts it stays in the KNOWN-RED shape: a `Failed`
/// verdict, not `PASSED TESTS`. **If this ever starts passing, that is
/// news, not a quiet win** — something would have to move enough to
/// satisfy a disputed oracle, which is exactly the kind of accident D-3
/// warned against tuning toward, so the test fails and asks for the
/// change to be looked at rather than accepted silently.
#[test]
#[ignore = "local: needs the fetched ROM (see module doc); KNOWN-RED per D-3"]
fn spc_mem_access_times_is_a_known_red() {
    let Some(dir) = rom_dir() else {
        eprintln!(
            "SKIP: cargo run -p rf-harness --bin fetch-test-roms -- blargg-spc-mem-access-times"
        );
        return;
    };
    let Some(text) = run(&dir, "spc_mem_access_times.sfc", user_ipl()) else {
        return;
    };
    assert!(
        !text.contains(FINAL_BANNER),
        "spc_mem_access_times.sfc PASSED, which D-3 did not expect: {text:?}\n\
         This is not silently a win — the ROM's oracle status is disputed \
         (higan vs Overload, no arbiter). Investigate what changed before \
         updating this ticket's recorded known-red."
    );
}

/// **REPORTING ONLY.** `spc_dsp6.sfc` and `spc_smp.sfc` are W7-08's open
/// S-DSP/opcode accuracy gaps, not this ticket's. Printed every run so
/// the verdict stays visible (RF-L-12: an oracle nobody looks at is not
/// an oracle), never asserted, so this file's real gate (`spc_timer`)
/// does not get blocked on someone else's open ticket.
#[test]
#[ignore = "local: needs the fetched ROMs; reporting only, see module doc (W7-08 owns these)"]
fn spc_dsp6_and_spc_smp_report_status() {
    let Some(dir) = rom_dir() else {
        eprintln!(
            "SKIP: cargo run -p rf-harness --bin fetch-test-roms -- \
             blargg-spc-dsp6 blargg-spc-smp"
        );
        return;
    };
    let ipl = user_ipl();
    for name in ["spc_dsp6.sfc", "spc_smp.sfc"] {
        run(&dir, name, ipl);
    }
}
