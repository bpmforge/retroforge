//! gilyon `snes-tests` **cputest** (ticket W6-02b; FR-CORE-030,
//! `docs/TESTING.md` §5's `gilyon_cputest` row).
//!
//! ## What this ROM is, and why it is a different oracle from the vectors
//!
//! 1610 tests covering every 65C816 opcode except `STP`/`WAI`, each in
//! every addressing mode it supports, with edge cases and wrapping
//! behaviour in **both** emulation and native mode. The SingleStepTests
//! vectors already check instruction semantics far more exhaustively per
//! opcode — 5,080,000 cases — so why run this too?
//!
//! Because it is a **program**, not a vector set. It boots on the real
//! bus, sets up the PPU, waits on `$4210`, reads controllers through
//! auto-joypad, uses `BRK`/`COP` handlers, and runs across four ROM banks
//! with `JSL`/`RTL` between them. A vector suite cannot fail the way a
//! wrong memory map, a stuck vblank flag or a mis-latched joypad port
//! fails. This is the first thing in the project that exercises the CPU,
//! the mapping, the registers and the frame clock *together*.
//!
//! ## The result protocol
//!
//! Two independent readings, because each covers the other's blind spot:
//!
//! * **`test_num`** — a zero-page word at `$00:0010`. It starts at
//!   `$FFFF` and increments only after a test passes, so its final value
//!   is a count and, on failure, names the exact test to look up in the
//!   shipped `tests-full.txt`. That is this suite's "RAM result block".
//! * **The verdict text** — the ROM writes `"Success"` or `"Failed"` as
//!   raw ASCII through `$2118` into VRAM. Reading it back is the ROM's
//!   own words rather than our interpretation of a counter.
//!
//! `test_num` alone could in principle be reached by a wild jump; the
//! text alone would not say which test failed. Together they are strong.
//!
//! ## Input is required, and that is not a workaround
//!
//! The ROM pauses between test banks with "Press A for next tests…",
//! polling `$4212` bit 0 (auto-joypad busy) and then `$4218` bit 7 (the
//! A button), waiting for a **press and then a release**. A harness that
//! held A permanently would hang on the release wait. So the button is
//! pulsed, which also means this test genuinely exercises the auto-joypad
//! latch — criterion 1 of this ticket — rather than merely asserting it
//! in isolation.

use rf_snes::cpu::CpuBus;
use rf_snes::SnesSystem;

/// Zero page `$0010`: `test_num`, per the ROM's `ZEROPAGE` segment
/// (`.res $10` then `test_num: .word 0`) and `lorom.cfg`'s
/// `ZEROPAGE: start = 0`.
const TEST_NUM_ADDR: u32 = 0x0000_0010;
/// The highest test label in `tests-full.map` is `test0649`, so a
/// complete run leaves `test_num` here.
const LAST_TEST: u16 = 0x0649;
/// Tilemap word the ROM writes its verdict to (`ldy #$32`).
const VERDICT_VRAM_WORD: u16 = 0x32;
/// The full run takes ~6.6M instructions; this is generous headroom and
/// bounded so a hang fails rather than spins forever.
const MAX_INSTRUCTIONS: u64 = 40_000_000;

fn rom_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("RF_GILYON_CPUTEST") {
        return Some(p.into());
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let p = root.join("roms/snes/gilyon-snes-tests/cputest/cputest-full.sfc");
    p.exists().then_some(p)
}

/// Pulse A so the "Press A" prompts advance: pressed for 8 frames,
/// released for 8.
fn drive_input(s: &mut SnesSystem) {
    let pressed = (s.bus.timing.frame / 8).is_multiple_of(2);
    s.bus.joypads.ports[0] = if pressed { 0x0080 } else { 0x0000 };
}

fn read_test_num(s: &SnesSystem) -> u16 {
    u16::from(s.bus.peek(TEST_NUM_ADDR)) | (u16::from(s.bus.peek(TEST_NUM_ADDR + 1)) << 8)
}

fn read_verdict(s: &SnesSystem) -> String {
    s.bus
        .vram_low_bytes(VERDICT_VRAM_WORD, 16)
        .iter()
        .map(|&b| {
            if (0x20..0x7F).contains(&b) {
                b as char
            } else {
                ' '
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

#[test]
#[ignore = "local: needs the gilyon archive extracted to roms/snes/gilyon-snes-tests/"]
fn cputest_full_reports_success_and_every_test_passes() {
    let Some(path) = rom_path() else {
        eprintln!(
            "SKIP: extract roms/snes/gilyon-snes-tests-v1.4.zip to \
             roms/snes/gilyon-snes-tests/ (or set RF_GILYON_CPUTEST)"
        );
        return;
    };
    let rom = std::fs::read(&path).expect("cputest-full.sfc readable");
    let mut s = SnesSystem::load(&rom).expect("cputest is a plain LoROM cart and must load");

    let mut ran = 0u64;
    while ran < MAX_INSTRUCTIONS {
        s.step()
            .expect("all 256 opcodes are implemented as of W6-01b");
        ran += 1;
        drive_input(&mut s);
        // Check periodically rather than every step: reading the verdict
        // costs more than the instruction does.
        if ran.is_multiple_of(100_000)
            && read_test_num(&s) == LAST_TEST
            && !read_verdict(&s).is_empty()
        {
            break;
        }
    }

    let test_num = read_test_num(&s);
    let verdict = read_verdict(&s);
    eprintln!("gilyon cputest: test_num={test_num:#06X}/{LAST_TEST:#06X}, ROM says {verdict:?}, {ran} instructions");

    assert_eq!(
        test_num,
        LAST_TEST,
        "stopped at test {test_num:#06X}; test {:#06X} is the one that failed — look it up in \
         the shipped cputest/tests-full.txt, which describes each test's inputs and expected \
         register/memory results",
        test_num.wrapping_add(1)
    );
    assert_eq!(
        verdict, "Success",
        "the ROM writes its own verdict to VRAM; it said {verdict:?}"
    );
}
