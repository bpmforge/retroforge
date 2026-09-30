//! Unit tests for the SNES bus, mapping, 5A22 registers and DMA
//! (ticket W6-02a).
//!
//! ## Why these exist alongside the fixture test
//!
//! `tests/mirror_map_fixtures.rs` runs real 65816 code that asks the
//! mapping questions from inside the machine, which is the strongest
//! evidence available. It is not sufficient on its own, and this ticket
//! learned that the hard way: the LoROM fixture's alias check was
//! **vacuous** — `ca65` had shortened `lda $008000` to absolute
//! addressing, which resolves through `DBR`, so the check compared WRAM
//! against ROM and could never have proved the alias it named. It took
//! being able to execute the ROM to notice.
//!
//! So the aliases are asserted here too, directly, where no assembler sits
//! between the intent and the assertion.

mod apu;
mod apu_ports;
mod dma;
mod dsp;
mod dsp1_dma;
mod hdma;
mod mapping;
mod ppu;
mod regs;
mod sdd1_dma;
mod system;
mod timing;
mod window;
mod window_ram;

#[test]
fn crate_is_wired() {
    assert_eq!(crate::CRATE_NAME, "rf-snes");
}
