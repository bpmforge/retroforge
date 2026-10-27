//! Ticket W41-02: ROM overrides through the `EmulatorCore` trait on the SNES
//! core, with a synthetic LoROM (no commercial bytes).

use rf_core_api::{
    CartImage, CoreEvent, CoreSink, EmulatorCore, PpuPixel, ResetKind, RomOverride, Step,
    MAX_ROM_OVERRIDES,
};
use rf_snes::core::SnesCore;

/// Program at `$00:8000`: `LDA $8100 ; STA $7E0010 ; BRA` back. `$8100` holds
/// 0x42.
fn rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0..9].copy_from_slice(&[0xAD, 0x00, 0x81, 0x8F, 0x10, 0x00, 0x7E, 0x80, 0xF7]);
    rom[0x100] = 0x42;
    rom[0x7FD5] = 0x20;
    rom[0x7FD7] = 0x08;
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    rom
}

fn ov(addr: u32, value: u8) -> RomOverride {
    RomOverride {
        addr,
        value,
        compare: None,
    }
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

fn run(overrides: &[RomOverride]) -> (u64, u8) {
    let mut c = SnesCore::load(&rom()).expect("loads");
    assert!(c.set_rom_overrides(overrides));
    let mut h = Hasher::default();
    for _ in 0..3 {
        c.step(Step::Frame, &mut h);
    }
    (h.0, c.peek(0x7E_0010))
}

#[test]
fn the_cpu_executes_the_override_deterministically() {
    let o = [ov(0x00_8100, 0x7C)];
    let (a, b) = (run(&o), run(&o));
    assert_eq!(a, b, "same input + overrides: same frames and state");
    assert_eq!(a.1, 0x7C);
    assert_eq!(run(&[]).1, 0x42);
}

#[test]
fn core_peek_shows_the_override_and_limit_is_enforced() {
    let mut c = SnesCore::load(&rom()).expect("loads");
    assert!(c.set_rom_overrides(&[ov(0x00_8100, 0x7C)]));
    assert_eq!(c.peek(0x00_8100), 0x7C);
    assert_eq!(c.peek(0x80_8100), 0x42, "mirror address not overridden");
    let many: Vec<_> = (0..=MAX_ROM_OVERRIDES as u32)
        .map(|i| ov(0x00_9000 + i, 1))
        .collect();
    assert!(!c.set_rom_overrides(&many));
    assert_eq!(c.peek(0x00_8100), 0x7C);
}

#[test]
fn overrides_survive_reset_and_clear_on_load() {
    let mut c = SnesCore::load(&rom()).expect("loads");
    assert!(c.set_rom_overrides(&[ov(0x00_8100, 0x7C)]));
    c.reset(ResetKind::Soft);
    c.reset(ResetKind::Hard);
    assert_eq!(c.peek(0x00_8100), 0x7C);
    let r = rom();
    c.load(CartImage::from_rom(&r)).expect("loads");
    assert_eq!(c.peek(0x00_8100), 0x42);
}
