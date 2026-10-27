//! Ticket W41-02: ROM-read overrides (Game Genie style) on the NES core.
//! The cartridge is built in-test: a UxROM board (mapper 2, two 16 KiB PRG
//! banks, CHR RAM) so the bank-switched compare case is real. No commercial
//! ROM bytes.

use rf_core_api::{
    CartImage, CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind, RomOverride,
    MAX_ROM_OVERRIDES,
};
use rf_nes::core::NesCore;
use rf_nes::CpuBus;

/// Bank 0 byte at `$9000`, bank 1 byte at `$9000` (bank 1 is the switchable
/// window's second choice; it is also fixed at `$C000`).
const B0: u8 = 0x11;
const B1: u8 = 0x22;

/// Two-bank UxROM. Reset vector `$C000`; there the program is
/// `LDA $9000 ; STA $10 ; JMP $C000`. `$C100` holds `$FF` so a write there
/// is a bus-conflict-safe bank select.
fn uxrom() -> Vec<u8> {
    let mut d = vec![0u8; 16];
    d[0..4].copy_from_slice(b"NES\x1a");
    d[4] = 2; // 2 x 16 KiB PRG
    d[5] = 0; // CHR RAM
    d[6] = 0x20; // mapper 2
    let mut prg = vec![0u8; 0x8000];
    prg[0x1000] = B0; // bank 0, $9000
    prg[0x4000 + 0x1000] = B1; // bank 1, $9000 when selected
    let code = [0xAD, 0x00, 0x90, 0x85, 0x10, 0x4C, 0x00, 0xC0];
    prg[0x4000..0x4000 + code.len()].copy_from_slice(&code);
    prg[0x4000 + 0x100] = 0xFF;
    prg[0x7FFC..0x7FFE].copy_from_slice(&0xC000u16.to_le_bytes());
    d.extend(prg);
    d
}

fn core() -> NesCore {
    NesCore::from_ines_bytes(&uxrom()).expect("fixture loads")
}

fn ov(addr: u32, value: u8, compare: Option<u8>) -> RomOverride {
    RomOverride {
        addr,
        value,
        compare,
    }
}

fn read(c: &mut NesCore, addr: u16) -> u8 {
    c.bus_mut().read(addr)
}

fn select_bank(c: &mut NesCore, bank: u8) {
    c.bus_mut().write(0xC100, bank);
}

#[derive(Default)]
struct Hasher(u64);
impl CoreSink for Hasher {
    fn video_scanline(&mut self, _y: u16, p: &[PpuPixel]) {
        for px in p {
            self.0 = (self.0 ^ u64::from(px.palette_index)).wrapping_mul(0x0100_0000_01B3);
        }
    }
    fn audio(&mut self, s: &[i16]) {
        for v in s {
            self.0 = (self.0 ^ *v as u16 as u64).wrapping_mul(0x0100_0000_01B3);
        }
    }
    fn event(&mut self, _e: CoreEvent) {}
}

#[test]
fn an_override_replaces_the_rom_byte_and_empty_clears_it() {
    let mut c = core();
    assert_eq!(read(&mut c, 0x9000), B0);
    assert!(c.set_rom_overrides(&[ov(0x9000, 0x99, None)]));
    assert_eq!(read(&mut c, 0x9000), 0x99);
    assert_eq!(read(&mut c, 0x9001), 0, "neighbouring address untouched");
    assert!(c.set_rom_overrides(&[]));
    assert_eq!(read(&mut c, 0x9000), B0);
}

#[test]
fn compare_gates_the_override_on_the_cartridge_byte() {
    let mut c = core();
    assert!(c.set_rom_overrides(&[ov(0x9000, 0x99, Some(B0))]));
    assert_eq!(read(&mut c, 0x9000), 0x99, "compare matches");
    assert!(c.set_rom_overrides(&[ov(0x9000, 0x99, Some(B0 ^ 1))]));
    assert_eq!(read(&mut c, 0x9000), B0, "compare mismatch passes through");
}

#[test]
fn the_first_matching_override_wins() {
    let mut c = core();
    assert!(c.set_rom_overrides(&[ov(0x9000, 0x01, Some(0xEE)), ov(0x9000, 0x02, None)]));
    assert_eq!(
        read(&mut c, 0x9000),
        0x02,
        "first entry mismatches, second applies"
    );
}

#[test]
fn peek_shows_what_the_cpu_sees() {
    let mut c = core();
    assert!(c.set_rom_overrides(&[ov(0x9000, 0x99, None)]));
    let cycles = c.bus().master_cycle();
    assert_eq!(c.bus().peek(0x9000), 0x99);
    assert_eq!(c.bus().peek(0x9001), 0);
    assert_eq!(c.bus().master_cycle(), cycles, "peek is cycle-free");
}

#[test]
fn more_than_the_limit_is_refused_and_changes_nothing() {
    let mut c = core();
    assert!(c.set_rom_overrides(&[ov(0x9000, 0x99, None)]));
    let too_many: Vec<_> = (0..=MAX_ROM_OVERRIDES as u32)
        .map(|i| ov(0x9100 + i, 1, None))
        .collect();
    assert!(!c.set_rom_overrides(&too_many));
    assert_eq!(read(&mut c, 0x9000), 0x99, "previous list still in force");
    let at_limit: Vec<_> = too_many[..MAX_ROM_OVERRIDES].to_vec();
    assert!(c.set_rom_overrides(&at_limit));
    assert_eq!(read(&mut c, 0x9000), B0);
    assert_eq!(read(&mut c, 0x9100), 1);
}

#[test]
fn overrides_survive_reset_and_clear_on_load() {
    let mut c = core();
    assert!(c.set_rom_overrides(&[ov(0x9000, 0x99, None)]));
    c.reset(ResetKind::Soft);
    c.reset(ResetKind::Hard);
    assert_eq!(read(&mut c, 0x9000), 0x99);
    let rom = uxrom();
    c.load(CartImage::from_rom(&rom)).expect("loads");
    assert_eq!(read(&mut c, 0x9000), B0);
}

#[test]
fn a_compare_code_only_fires_while_its_bank_is_mapped() {
    let mut c = core();
    assert!(c.set_rom_overrides(&[ov(0x9000, 0x99, Some(B0))]));
    select_bank(&mut c, 0);
    assert_eq!(read(&mut c, 0x9000), 0x99, "bank 0 mapped: fires");
    select_bank(&mut c, 1);
    assert_eq!(read(&mut c, 0x9000), B1, "bank 1 mapped: compare fails");
    select_bank(&mut c, 0);
    assert_eq!(c.bus().peek(0x9000), 0x99);
}

#[test]
fn the_cpu_executes_the_override_and_the_frame_hash_is_deterministic() {
    let run = |overrides: &[RomOverride]| {
        let mut c = core();
        assert!(c.set_rom_overrides(overrides));
        let mut h = Hasher::default();
        for _ in 0..3 {
            c.run_frame(&InputFrame::empty(), &mut h);
        }
        (h.0, c.bus().peek(0x0010))
    };
    let o = [ov(0x9000, 0x5A, None)];
    let a = run(&o);
    let b = run(&o);
    assert_eq!(a, b, "same input + overrides: same frames and state");
    assert_eq!(a.1, 0x5A, "the program's LDA saw the override");
    assert_eq!(run(&[]).1, B0, "no overrides: the real byte");
}

/// Perf probe (run by hand): `cargo test -p rf-nes --release --test
/// rom_overrides -- --ignored --nocapture`. Times N frames with an empty
/// list (one length test per ROM read) and with one non-matching override.
#[test]
#[ignore = "timing probe, not an assertion"]
fn perf_probe_empty_vs_one_override() {
    let time = |overrides: &[RomOverride]| {
        let mut c = core();
        assert!(c.set_rom_overrides(overrides));
        let mut h = Hasher::default();
        let start = std::time::Instant::now();
        for _ in 0..600 {
            c.run_frame(&InputFrame::empty(), &mut h);
        }
        start.elapsed()
    };
    for _ in 0..3 {
        let empty = time(&[]);
        let one = time(&[ov(0xFFF0, 1, None)]);
        println!("600 frames: empty {empty:?}  one-nonmatching {one:?}");
    }
}
