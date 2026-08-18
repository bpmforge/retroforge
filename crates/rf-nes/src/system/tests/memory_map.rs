//! Acceptance criterion 1: "memory map per nesdev (mirrors, PPU/APU regs
//! stubbed as needed)". nesdev.org/wiki/CPU_memory_map: `$0000-$07FF` is
//! mirrored every `$0800` through `$1FFF`; `$2000-$2007` (PPU registers)
//! is mirrored every `$0008` through `$3FFF`.
use super::bus_with_pattern_rom;
use crate::cpu::CpuBus;

#[test]
fn ram_write_is_visible_at_all_three_mirrors() {
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.write(0x0000, 0x42);
    assert_eq!(bus.read(0x0800), 0x42);
    assert_eq!(bus.read(0x1000), 0x42);
    assert_eq!(bus.read(0x1800), 0x42);
}

#[test]
fn ram_mirror_write_is_visible_at_base_address() {
    // Mirroring is symmetric: a write through a mirror lands on the same
    // underlying byte the base address sees.
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.write(0x1801, 0x99);
    assert_eq!(bus.read(0x0001), 0x99);
    assert_eq!(bus.read(0x0801), 0x99);
    assert_eq!(bus.read(0x1001), 0x99);
}

#[test]
fn ram_mirroring_does_not_bleed_into_adjacent_bytes() {
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.write(0x0000, 0x11);
    bus.write(0x0001, 0x22);
    assert_eq!(bus.read(0x1800), 0x11);
    assert_eq!(bus.read(0x1801), 0x22);
}

/// PPU register mirroring, proven through `OAMADDR`/`OAMDATA` (index 3/4
/// of the 8-register window) since those are the only two registers this
/// ticket's stub gives real, observable side effects — a plain
/// open-bus-passthrough register wouldn't distinguish "mirrored to the
/// same register" from "mirrored to nothing in particular".
#[test]
fn ppu_register_mirroring_every_8_bytes_through_3fff() {
    let mut bus = bus_with_pattern_rom(1, 1);
    // $2003 (OAMADDR) sets the address; $200C is $2004 (OAMDATA) mirrored
    // once (+8). If mirroring is correct, this writes into OAM at 0x05 and
    // advances OAMADDR, exactly like writing OAMDATA at its base address.
    bus.write(0x2003, 0x05);
    bus.write(0x200C, 0xAB);
    assert_eq!(bus.oam()[0x05], 0xAB);

    // And the far end of the mirrored range ($3FFC = $2004 + 0x1FF8,
    // still index 4 mod 8) reaches the very same register.
    bus.write(0x3FFB, 0x10); // $3FFB & 7 == 3 -> OAMADDR = 0x10
    bus.write(0x3FFC, 0xCD); // $3FFC & 7 == 4 -> OAMDATA
    assert_eq!(bus.oam()[0x10], 0xCD);
}

/// Unimplemented PPU registers (everything except OAMADDR/OAMDATA) are an
/// obvious stub: writes are dropped, reads return open bus. This also
/// proves those registers *are* mirrored (both addresses observe the same
/// — unchanged — open-bus latch), without claiming any real PPUCTRL/
/// PPUSTATUS/... behavior.
/// Write-only PPU registers read back the **PPU's** decay register, and
/// mirrors observe the same one (ticket W2-19).
///
/// This test previously asserted they returned the CPU bus's open-bus
/// latch, driven here by a write to `$0000`. That was wrong, and blargg's
/// `ppu_open_bus` readme says why in its opening line: "Unlike other
/// open-bus addresses, the PPU ones are separate." The CPU-side latch is
/// deliberately left driven to a DIFFERENT value below, so an
/// implementation that went back to consulting it would fail rather than
/// coincide.
#[test]
fn unimplemented_ppu_registers_read_the_ppu_decay_register() {
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.write(0x0000, 0x77); // CPU open-bus latch := $77 — a decoy
    bus.write(0x2001, 0x5A); // any PPU write sets the decay register
    assert_eq!(bus.read(0x2000), 0x5A); // PPUCTRL: write-only, decay value
    assert_eq!(bus.read(0x2801), 0x5A); // mirror of $2001: the same latch
}

#[test]
fn cartridge_prg_ram_is_readable_and_writable() {
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.write(0x6000, 0xAB);
    bus.write(0x7FFF, 0xCD);
    assert_eq!(bus.read(0x6000), 0xAB);
    assert_eq!(bus.read(0x7FFF), 0xCD);
    // Distinct from PRG ROM ($8000+): writes there have no effect.
    assert_ne!(bus.read(0x8000), 0xAB);
}

#[test]
fn prg_rom_writes_are_ignored_nrom_has_no_mapper_registers() {
    let mut bus = bus_with_pattern_rom(1, 1);
    let before = bus.read(0x8000);
    bus.write(0x8000, !before);
    assert_eq!(bus.read(0x8000), before, "NROM PRG ROM is not writable");
}

#[test]
fn disabled_test_registers_and_unmapped_expansion_are_open_bus() {
    let mut bus = bus_with_pattern_rom(1, 1);
    // A write to a disabled/unmapped region has no storage effect, but
    // real hardware still drives the byte being written onto the shared
    // data bus for that cycle — so a read right afterward, anywhere
    // equally undriven, sees the write's value, not some earlier one.
    bus.write(0x401F, 0xFF); // disabled test register: storage dropped
    assert_eq!(
        bus.read(0x401F),
        0xFF,
        "the write itself drove the open-bus latch"
    );
    assert_eq!(
        bus.read(0x5000),
        0xFF,
        "unmapped $4020-$5FFF: still open bus"
    );

    bus.write(0x0000, 0x5A); // a real RAM write re-drives the latch
    assert_eq!(
        bus.read(0x401F),
        0x5A,
        "open-bus latch tracks whatever was driven most recently"
    );
}

/// Ticket W1-03: `NesBus::peek` (the trace logger's disassembly-only read)
/// returns a fixed `$FF` for the write-only/internal APU register stub
/// range, distinct from the real `open_bus`-tracking `read`/`write` path
/// above — verified against the real fetched `nestest.log`'s `$4004`,
/// `$4005`, `$4006`, `$4007`, and `$4015` occurrences (see `NesBus::peek`'s
/// doc comment); this test is the ROM/log-independent regression for that
/// finding, run on every machine regardless of whether the golden log is
/// fetched.
#[test]
fn peek_returns_fixed_ff_for_the_apu_stub_range_and_never_ticks_the_clock() {
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.write(0x0000, 0x37); // drive open_bus to a known, distinct byte
    let before = bus.master_cycle();

    for addr in [0x4004u16, 0x4005, 0x4006, 0x4007, 0x4014, 0x4015, 0x401F] {
        assert_eq!(
            bus.peek(addr),
            0xFF,
            "peek(${addr:04X}) must be the fixed $FF disassembly placeholder, not open_bus (0x37)"
        );
    }
    assert_eq!(
        bus.master_cycle(),
        before,
        "peek must never advance the master clock"
    );
    // The real emulation path is unaffected by `peek`'s placeholder, but it
    // is no longer pure open bus either: ticket W2-01a made `$4015` the
    // APU's one readable register. nesdev.org/wiki/APU ("Status ($4015)"):
    // the returned byte is `IF-D NT21` with "Bit 5 is open bus", and the
    // read "does not affect open bus". With every channel silent at
    // power-on, that leaves exactly bit 5 of the 0x37 latch.
    assert_eq!(
        bus.read(0x4015),
        0x37 & 0x20,
        "real $4015 read: silent APU status, with only bit 5 from the open-bus latch"
    );
    assert_eq!(
        bus.read(0x401F),
        0x37,
        "and the $4015 read left the open-bus latch itself untouched"
    );
}

/// Ticket W4-06d, acceptance criterion 2: the accessors return what a ROM
/// actually wrote, not a plausible-looking zero buffer.
///
/// Writes through the real `$2006`/`$2007` register path — the same path a
/// game uses — rather than poking the arrays, so this would catch an
/// accessor wired to the wrong buffer, and it exercises `$2007`'s address
/// auto-increment on the way.
#[test]
fn vram_and_palette_accessors_return_what_the_rom_actually_wrote() {
    let mut bus = bus_with_pattern_rom(1, 1);

    // Nametable 0 at $2000: three known bytes, relying on $2007's
    // post-write increment to advance.
    bus.write(0x2006, 0x20);
    bus.write(0x2006, 0x00);
    for byte in [0xAB, 0xCD, 0xEF] {
        bus.write(0x2007, byte);
    }
    assert_eq!(
        &bus.vram()[0..3],
        &[0xAB, 0xCD, 0xEF],
        "VRAM accessor must show bytes written through $2007"
    );

    // Palette RAM at $3F00.
    bus.write(0x2006, 0x3F);
    bus.write(0x2006, 0x00);
    for byte in [0x21, 0x0F, 0x30] {
        bus.write(0x2007, byte);
    }
    let palette = bus.palette();
    assert_eq!(
        &palette[0..3],
        &[0x21, 0x0F, 0x30],
        "palette accessor must show bytes written through $2007"
    );

    // Anti-vacuity: an address NOT written stays clear, so the assertions
    // above cannot be passing because everything reads back as the value
    // that happened to be written last.
    assert_eq!(bus.vram()[0x0100], 0x00);
}

/// The accessors are NON-OBSERVING (criterion 3): reading them must not
/// advance the master clock, touch open bus, or clock anything.
///
/// This is W3-05a's hazard class — a debug viewer repaints continuously,
/// so an observing accessor would perturb MMC3 A12 IRQ timing on every
/// frame a panel is open, invisibly to any pixel comparison.
#[test]
fn the_vram_and_palette_accessors_observe_nothing() {
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.write(0x2006, 0x20);
    bus.write(0x2006, 0x00);
    bus.write(0x2007, 0x5A);

    let cycle_before = bus.master_cycle();
    // `peek` is the side-effect-free read the trace logger uses; $4000 is
    // a write-only APU port, so it reports the open-bus latch.
    let open_bus_before = bus.peek(0x4000);

    // Read them the way a repainting panel would: repeatedly.
    for _ in 0..100 {
        let _ = bus.vram();
        let _ = bus.palette();
        let _ = bus.oam();
    }

    assert_eq!(
        bus.master_cycle(),
        cycle_before,
        "an accessor that advanced the clock would perturb every timing the core has"
    );
    assert_eq!(
        bus.peek(0x4000),
        open_bus_before,
        "an accessor that touched the data bus would change open-bus reads"
    );
}
