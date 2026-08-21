//! Ticket W7-11 criterion 3: a mapper-28 test ROM, via the `$6000`
//! protocol.
//!
//! ## Why this fixture exists rather than a fetched ROM
//!
//! Every other mapper in this crate is checked against a published
//! conformance ROM. Mapper 28 has none: Action 53 is a homebrew multicart
//! collection, not a mapper with a test suite, and there is no mapper-28
//! ROM in `tests/rom-manifest.toml`, in `docs/TESTING.md`, or in anything
//! fetched into `roms/`. That absence is what left this criterion blocked.
//!
//! Rather than drop it, the fixture is built from this repo's own cc65
//! toolchain — the same approach `fixtures/nes/rf-scroller` already takes,
//! and the one W7-11's own HANDOFF recommended. **No ROM bytes are
//! committed** (law 5); `fixtures/nes/action53/build.sh` produces it and
//! this test skips when it has not been run.
//!
//! ## What it actually proves
//!
//! The banking arithmetic, which is the part of mapper 28 that cannot be
//! guessed. Each of the eight 16 KiB banks is stamped with its own index
//! at its first byte; the ROM selects each in turn through the `$5000`
//! select latch plus the `$8000` data port and reads the stamp back. It
//! also asserts on every iteration that `$C000` still reads bank 1 — mode
//! 3's fixed half must not move while `$8000` does, which is exactly the
//! "fixed bank forces the outer size to 32 KiB" rule the mapper's doc
//! calls out.

use rf_nes::{Cpu, NesBus};

fn rom_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("RF_ACTION53_ROM") {
        return Some(p.into());
    }
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/nes/action53/build/action53.nes");
    p.exists().then_some(p)
}

/// blargg's `$6000` protocol: `$80` = running, `0` = pass, anything else
/// is a failure code, with `$DE $B0 $61` at `$6001`-`$6003` marking the
/// result as real rather than uninitialised RAM.
#[test]
#[ignore = "local: run fixtures/nes/action53/build.sh first"]
fn action53_fixture_passes_the_6000_protocol() {
    let Some(path) = rom_path() else {
        eprintln!("SKIP: run fixtures/nes/action53/build.sh");
        return;
    };
    let bytes = std::fs::read(&path).expect("rom readable");
    let mut bus = NesBus::from_ines_bytes(&bytes).expect("mapper 28 must load");
    let mut cpu = Cpu::power_on(&mut bus);

    for _ in 0..2_000_000u64 {
        cpu.step(&mut bus);
        if bus.peek(0x6000) != 0x80 && bus.peek(0x6003) == 0x61 {
            break;
        }
    }

    assert_eq!(
        [bus.peek(0x6001), bus.peek(0x6002), bus.peek(0x6003)],
        [0xDE, 0xB0, 0x61],
        "the $6000 signature is absent - the ROM never reached its result, \
         so the status byte below is uninitialised RAM rather than a verdict"
    );
    assert_eq!(
        bus.peek(0x6000),
        0,
        "mapper 28 banking failed; the ROM records the offending bank at $0203 \
         (got bank {})",
        bus.peek(0x0203)
    );
    // The RAM witness agrees, which rules out a PRG-RAM aliasing accident
    // making $6000 read 0 for the wrong reason.
    assert_eq!(bus.peek(0x0200), 0, "the RAM witness must agree with $6000");
}
