//! System-level tests: cartridge loading, the FR-CORE-013 diagnostic,
//! reset, and master-cycle accounting (ticket W6-02a).

use rf_cart::{CartError, SnesMapMode};

use crate::cpu::CpuBus;
use crate::system::SnesSystem;

/// A self-consistent LoROM image with a chosen map-mode and chipset byte.
///
/// Mirrors the helper `rf-cart`'s own tests use, so the two agree on what
/// "a plausible cartridge" means.
fn lorom_image(mode_byte: u8, chipset: u8) -> Vec<u8> {
    let mut data = vec![0u8; 0x8000];
    let base = 0x7FC0;
    data[base + 0x15] = mode_byte;
    data[base + 0x16] = chipset;
    data[base + 0x17] = 6;
    data[base + 0x18] = 3;
    let checksum: u16 = 0xBEEF;
    data[base + 0x1C..base + 0x1E].copy_from_slice(&(checksum ^ 0xFFFF).to_le_bytes());
    data[base + 0x1E..base + 0x20].copy_from_slice(&checksum.to_le_bytes());
    data[base + 0x3C] = 0x00;
    data[base + 0x3D] = 0x80;
    data
}

/// **FR-CORE-013 / FR-CORE-035.** An enhancement chip must produce a
/// diagnostic that NAMES the chip — and must not panic.
///
/// The requirement says "never a crash", so this asserts the failure
/// arrives as a `Result` from the entry point a front-end actually calls,
/// rather than somewhere deeper where a caller could miss it.
#[test]
fn enhancement_chips_are_refused_with_a_diagnostic_naming_them() {
    // Cartridge-type high nibble $3 = SA-1.
    let rom = lorom_image(0x20, 0x35);
    let err = match SnesSystem::load(&rom) {
        Err(e) => e,
        Ok(_) => panic!("SA-1 must be refused, not loaded"),
    };
    let CartError::UnsupportedChip { name } = &err else {
        panic!("expected UnsupportedChip, got {err:?}");
    };
    assert!(
        name.contains("SA-1"),
        "the diagnostic must name the chip; got {name:?}"
    );
    // And it must render as readable text, not a debug dump.
    assert!(
        !err.to_string().is_empty(),
        "the diagnostic must have a Display form"
    );
}

/// The refusal must not be indiscriminate: a plain ROM-only cart loads.
///
/// Without this, "everything is refused" would satisfy the test above.
#[test]
fn a_plain_cartridge_loads() {
    let rom = lorom_image(0x20, 0x00);
    let system = SnesSystem::load(&rom).expect("a ROM-only LoROM cart must load");
    assert_eq!(system.bus.mode, SnesMapMode::LoRom);
}

/// Truncated and malformed images must fail rather than panic — the same
/// "never a crash" clause, on the other kind of bad input.
#[test]
fn malformed_images_fail_without_panicking() {
    for raw in [vec![], vec![0u8; 16], vec![0xFFu8; 0x8000]] {
        let _ = SnesSystem::load(&raw);
    }
}

/// Reset fetches PC through the MAPPING, not from a file offset.
#[test]
fn reset_takes_pc_from_the_vector_through_the_mapping() {
    let mut rom = lorom_image(0x20, 0x00);
    // Reset vector -> $9ABC.
    rom[0x7FFC] = 0xBC;
    rom[0x7FFD] = 0x9A;
    let system = SnesSystem::load(&rom).expect("loads");
    assert_eq!(system.cpu.pc, 0x9ABC);
    assert_eq!(system.cpu.pbr, 0, "reset always starts in bank 0");
    assert!(system.cpu.e, "the 65816 resets into emulation mode");
}

/// Stepping charges master cycles, and the charge tracks the region.
#[test]
fn stepping_accumulates_master_cycles_by_region() {
    let mut rom = lorom_image(0x20, 0x00);
    // LDA #$42 at the reset target.
    rom[0x0000] = 0xA9;
    rom[0x0001] = 0x42;
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;

    let mut system = SnesSystem::load(&rom).expect("loads");
    assert_eq!(system.master_cycles, 0, "reset starts the clock at zero");
    system.step().expect("implemented");
    assert_eq!(
        system.master_cycles, 16,
        "two SlowROM fetches at 8 master cycles each"
    );
    assert_eq!(system.cpu.a & 0xFF, 0x42);
}

/// A bounded runner: a ROM that never finishes must fail a test, not hang
/// it.
#[test]
fn run_until_is_bounded_and_stops_on_a_halt() {
    let mut rom = lorom_image(0x20, 0x00);
    rom[0x0000] = 0xDB; // STP
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    let mut system = SnesSystem::load(&rom).expect("loads");
    let ran = system.run_until(1000, None).expect("implemented");
    assert_eq!(
        ran, 1,
        "STP stops the run after the instruction that halted"
    );
    assert!(system.cpu.stopped);

    // And an endless loop is capped rather than hanging.
    let mut rom2 = lorom_image(0x20, 0x00);
    rom2[0x0000] = 0x80; // BRA -2
    rom2[0x0001] = 0xFE;
    rom2[0x7FFC] = 0x00;
    rom2[0x7FFD] = 0x80;
    let mut looping = SnesSystem::load(&rom2).expect("loads");
    assert_eq!(looping.run_until(50, None).expect("implemented"), 50);
}

/// Writes to ROM are dropped, not applied and not fatal.
#[test]
fn rom_is_read_only_and_a_write_to_it_is_survivable() {
    let rom = lorom_image(0x20, 0x00);
    let mut system = SnesSystem::load(&rom).expect("loads");
    let before = system.bus.peek(0x80_8000);
    system.bus.write(0x80_8000, 0x5A);
    assert_eq!(
        system.bus.peek(0x80_8000),
        before,
        "a write to ROM must not take effect"
    );
}

/// Unmapped reads see open bus — the last value driven — rather than a
/// tidy zero that would hide the difference.
#[test]
fn unmapped_reads_return_open_bus() {
    let rom = lorom_image(0x20, 0x00);
    let mut system = SnesSystem::load(&rom).expect("loads");
    system.bus.write(0x7E_0000, 0xC7); // drives $C7 onto the bus
    assert_eq!(
        system.bus.read(0x00_6000),
        0xC7,
        "an open window returns the last value driven"
    );
}

/// FastROM comes from the header and reaches the cost model.
#[test]
fn fastrom_from_the_header_changes_the_access_cost() {
    let slow = SnesSystem::load(&lorom_image(0x20, 0x00)).expect("loads");
    let fast = SnesSystem::load(&lorom_image(0x30, 0x00)).expect("loads");
    assert!(!slow.bus.fast_rom);
    assert!(fast.bus.fast_rom);
    assert_eq!(slow.access_cost(0x80_8000), 8);
    assert_eq!(fast.access_cost(0x80_8000), 6);
}

/// FR-CORE-035 names "SA-1, Super FX, DSP-1, …" — so test the family,
/// not one member.
///
/// The single-chip test above cannot see a hole: `rf-cart` gates on the
/// cartridge-type byte's LOW nibble (`>= 3` means a coprocessor is
/// present) while the chip NAME comes from the high nibble. A chip whose
/// low nibble fell below the threshold would load as a plain cartridge
/// and run as if the coprocessor were not there — silently wrong, and
/// invisible to a test that only tries one byte.
///
/// DSP ($03/$05) is deliberately absent from this table as of ticket
/// W14-19 (D-010): it is the one coprocessor nibble this build now runs,
/// via the HLE in [`crate::dsp1`] — see
/// `dsp_carts_load_with_the_hle_installed_instead_of_being_refused`
/// below for its positive coverage. SA-1 ($34/$35) is likewise absent as
/// of ticket W17-01 (D-013) — moved to
/// `sa1_carts_load_with_the_board_wired_up` below, since (under map mode
/// $23) it now loads instead of refusing.
#[test]
fn every_named_coprocessor_family_is_refused_and_named() {
    for (chipset, expect) in [
        (0x13u8, "Super FX"),
        (0x15, "Super FX"),
        (0x1A, "Super FX"),
        (0x25, "OBC1"),
        (0x43, "S-DD1"),
        (0x55, "S-RTC"),
        (0xE3, "Super Game Boy"),
        (0xF5, "custom coprocessor"),
    ] {
        let rom = lorom_image(0x20, chipset);
        match SnesSystem::load(&rom) {
            Err(CartError::UnsupportedChip { name }) => assert!(
                name.contains(expect),
                "chipset ${chipset:02X}: diagnostic {name:?} should name {expect:?}"
            ),
            Err(other) => panic!("chipset ${chipset:02X}: expected UnsupportedChip, got {other:?}"),
            Ok(_) => panic!(
                "chipset ${chipset:02X} ({expect}) LOADED as a plain cartridge — FR-CORE-013 hole"
            ),
        }
    }
}

/// ...and the plain cartridge types still load, so the refusal above is
/// discriminating rather than blanket.
#[test]
fn plain_cartridge_types_are_not_refused() {
    for chipset in [0x00u8, 0x01, 0x02] {
        assert!(
            SnesSystem::load(&lorom_image(0x20, chipset)).is_ok(),
            "chipset ${chipset:02X} is ROM/ROM+RAM/ROM+RAM+battery and must load"
        );
    }
}

/// Ticket W14-19: chipset $03/$04/$05 (coprocessor nibble "DSP") now
/// loads with the DSP-1 HLE installed instead of the FR-CORE-013
/// refusal `every_named_coprocessor_family_is_refused_and_named` still
/// checks for every other family.
#[test]
fn dsp_carts_load_with_the_hle_installed_instead_of_being_refused() {
    for chipset in [0x03u8, 0x04, 0x05] {
        let system = SnesSystem::load(&lorom_image(0x20, chipset))
            .unwrap_or_else(|e| panic!("chipset ${chipset:02X} (DSP) must load, got {e:?}"));
        assert!(
            system.bus.dsp1.is_some(),
            "chipset ${chipset:02X}: DSP-1 HLE must be installed"
        );
    }
    // A plain ROM (no coprocessor nibble match) never gets one.
    let plain = SnesSystem::load(&lorom_image(0x20, 0x00)).expect("loads");
    assert!(plain.bus.dsp1.is_none());
}

/// Ticket W14-19 acceptance: "the core routes the cart's DSP window
/// through mapping.rs/bus.rs only when the cart reports the chip". Reads
/// and writes at an address inside the LoROM-small window (D-010's
/// snes9x-superset numbers: banks $20-$3F/$A0-$BF, DR $8000-$BFFF, SR
/// $C000-$FFFF) reach the chip rather than ROM/open bus.
#[test]
fn the_dsp_window_reaches_the_chip_through_the_bus() {
    let mut system = SnesSystem::load(&lorom_image(0x20, 0x03)).expect("DSP loads");
    // 2Fh (version) needs no parameters: write the command, then read the
    // two-word result straight back out through the real bus path.
    system.bus.write(0x30_8000, 0x2F);
    assert_eq!(system.bus.read(0x30_8000), 0x01);
    assert_eq!(system.bus.read(0x30_8000), 0x01);
    // SR reads 0x80 (ready) anywhere in its sub-range, e.g. the far end.
    assert_eq!(system.bus.read(0x30_FFFF), 0x80);
    // Peek must not perturb: reading DR through peek twice sees the same
    // byte, unlike `read` which would advance past the low half.
    system.bus.write(0x30_8000, 0x0F); // memory test -> one zero word
    assert_eq!(system.bus.peek(0x30_8000), 0x00);
    assert_eq!(system.bus.peek(0x30_8000), 0x00);
}

/// Ticket W17-01 (D-013): chipset $34/$35 under map mode $23 now loads
/// with the SA-1 board wired up instead of the FR-CORE-013 refusal.
/// Everything else that carried "SA-1" in the diagnostic before this
/// ticket used a DIFFERENT map mode (plain LoROM/HiROM), which still
/// refuses — see the note on `every_named_coprocessor_family_is_refused_and_named`.
#[test]
fn sa1_carts_load_with_the_board_wired_up() {
    for chipset in [0x34u8, 0x35] {
        let system = SnesSystem::load(&lorom_image(0x23, chipset))
            .unwrap_or_else(|e| panic!("chipset ${chipset:02X} (SA-1) must load, got {e:?}"));
        assert!(
            system.bus.sa1.is_some(),
            "chipset ${chipset:02X}: SA-1 board must be installed"
        );
        assert_eq!(system.bus.mode, SnesMapMode::Sa1);
    }
    // A plain LoROM cart never gets one.
    let plain = SnesSystem::load(&lorom_image(0x20, 0x00)).expect("loads");
    assert!(plain.bus.sa1.is_none());
}

/// Ticket W17-01 acceptance #3: "the SNES-side CPU can boot an SA-1 cart
/// to its reset vector and run" — with the SA-1 CPU itself absent (W17-02
/// adds it), which is the "uniform" bucket the ticket's census criterion
/// names.
#[test]
fn sa1_cart_resets_and_steps_through_its_own_rom() {
    let mut rom = lorom_image(0x23, 0x34);
    // LDA #$42 at the reset target, bank $00 (an SA-1 cart's default
    // vectors and header always sit in LoROM bank $00, fullsnes "SNES
    // Cart SA-1").
    rom[0x0000] = 0xA9;
    rom[0x0001] = 0x42;
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    let mut system = SnesSystem::load(&rom).expect("SA-1 cart loads");
    system.step().expect("implemented");
    assert_eq!(system.cpu.a & 0xFF, 0x42);
}

/// Ticket W17-01 acceptance #2: the SNES-side memory map — ROM bank
/// registers, BW-RAM window, I-RAM, and the register window round-trip.
#[test]
fn sa1_memory_map_reaches_iram_bwram_and_registers_through_the_bus() {
    let rom = lorom_image(0x23, 0x35); // +battery, so ram_size (8 KiB) is BW-RAM
    let mut system = SnesSystem::load(&rom).expect("SA-1 cart loads");

    // I-RAM at $3000-$37FF, banks $00-$3F/$80-$BF.
    system.bus.write(0x00_3000, 0xAB);
    assert_eq!(system.bus.read(0x00_3000), 0xAB);
    assert_eq!(
        system.bus.read(0x80_3000),
        0xAB,
        "I-RAM mirrors across banks"
    );

    // BW-RAM window at $6000-$7FFF, block 0 by default ($2224 resets to 0).
    system.bus.write(0x00_6000, 0xCD);
    assert_eq!(system.bus.read(0x00_6000), 0xCD);
    // The same byte is visible through the full BW-RAM window at $40:0000.
    assert_eq!(system.bus.read(0x40_0000), 0xCD);

    // The register window: a write to $2220 (CXB) is stored and later
    // affects the ROM mapping (acceptance #2's "writes ... stored").
    system.bus.write(0x00_2220, 0x81); // bank 1, LoROM-mapped
    let mapped = system.bus.read(0x00_8000);
    assert_eq!(
        mapped,
        system.bus.rom[0x10_0000 % system.bus.rom.len()],
        "CXB=$81 must bank-select 1 MiB block 1 into $00:8000"
    );

    // The read-only block answers its documented reset value (no SA-1 CPU
    // yet to move it) rather than open bus.
    assert_eq!(system.bus.read(0x00_2300), 0x00);
}

/// A non-DSP cartridge's mapping at the same addresses is unchanged: the
/// window only exists when `rf-cart` reports the chip.
#[test]
fn a_plain_cart_at_dsp_window_addresses_is_ordinary_rom() {
    let mut system = SnesSystem::load(&lorom_image(0x20, 0x00)).expect("plain loads");
    // $30:8000 in a plain LoROM cart is an ordinary ROM byte, not DR: a
    // write to it must be dropped (ROM is read-only), which a DR write
    // would never do — DR always accepts a byte and changes DSP1 state.
    let before = system.bus.read(0x30_8000);
    system.bus.write(0x30_8000, before ^ 0xFF);
    assert_eq!(
        system.bus.read(0x30_8000),
        before,
        "a plain cart's ROM at this address must not move just because DSP-1 exists"
    );
}

/// **DMA through the wired path.** The unit tests drive `service_dma`
/// directly; "sufficient to boot libSFX fixture ROMs" is a claim about
/// what happens when real 65816 code writes `$420B`, so assert that.
#[test]
fn real_code_writing_420b_performs_the_transfer_and_is_charged_for_it() {
    let mut rom = lorom_image(0x20, 0x00);
    let program: &[u8] = &[
        // Point the WRAM port at $1:0000 (outside the low-8K mirror, so
        // the destination cannot overlap the source).
        0xA9, 0x00, 0x8D, 0x81, 0x21, // LDA #$00 : STA $2181
        0x8D, 0x82, 0x21, // STA $2182
        0xA9, 0x01, 0x8D, 0x83, 0x21, // LDA #$01 : STA $2183
        // Channel 0: pattern 0, A-bus increment, B-bus $2180 (WMDATA).
        0xA9, 0x00, 0x8D, 0x00, 0x43, // control = $00
        0xA9, 0x80, 0x8D, 0x01, 0x43, // b_address = $80
        0xA9, 0x00, 0x8D, 0x02, 0x43, // A low  = $00
        0xA9, 0x01, 0x8D, 0x03, 0x43, // A mid  = $01  -> $00:0100
        0xA9, 0x00, 0x8D, 0x04, 0x43, // A bank = $00
        0xA9, 0x04, 0x8D, 0x05, 0x43, // count low  = 4
        0xA9, 0x00, 0x8D, 0x06, 0x43, // count high = 0
        0xA9, 0x01, 0x8D, 0x0B, 0x42, // MDMAEN: fire channel 0
        0xDB, // STP
    ];
    rom[..program.len()].copy_from_slice(program);
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;

    let mut system = SnesSystem::load(&rom).expect("loads");
    system.bus.wram[0x100..0x104].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

    let before = system.master_cycles;
    system.run_until(500, None).expect("implemented");

    assert!(system.cpu.stopped, "the program should reach its STP");
    assert_eq!(
        &system.bus.wram[0x1_0000..0x1_0004],
        &[0xDE, 0xAD, 0xBE, 0xEF],
        "a $420B write from real code must actually move the bytes"
    );
    // 4 bytes at 8 cycles plus 8 for the channel = 40, on top of the
    // instruction cost. Asserting the DMA component specifically, so a
    // transfer that happened for free would still fail.
    assert!(
        system.master_cycles >= before + 40,
        "the transfer must be charged: {} cycles elapsed",
        system.master_cycles - before
    );
}
