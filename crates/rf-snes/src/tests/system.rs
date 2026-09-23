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

/// Ticket W14-28: eight `NOP`s -- the ordinary, documented idiom for
/// waiting out the hardware divider's 16-cycle latency (fullsnes "SNES
/// Maths Multiply/Divide": the latency "is a CPU-cycle count, independent
/// of whether any given cycle is fast or slow on the bus, or internal")
/// -- must be enough real CPU time for a divide to finish, the same as on
/// hardware. `NOP` is a single-byte, implied-mode opcode: it makes exactly
/// one bus access (the opcode fetch) but no 65816 instruction executes in
/// fewer than 2 cycles (WDC 65C816 datasheet, instruction timing), so it
/// always has one internal cycle beyond that access. Before this ticket,
/// `SnesSystem::step` fed `MathUnit::tick` the access-charged master
/// cycles only, so `NOP`'s internal cycle was silently dropped — eight of
/// them left a divide one step short, and the game reading the result
/// (here, and in the real ROM this reproduces, The Flintstones' boot; see
/// `docs/TESTING.md`) saw a stale, still-shifting remainder instead of the
/// finished one.
#[test]
fn eight_nops_are_enough_to_finish_a_divide_the_way_hardware_would() {
    let mut rom = lorom_image(0x20, 0x00);
    let mut code: Vec<u8> = vec![
        0xA9, 0x3F, // LDA #$3F        (dividend 63)
        0x8D, 0x04, 0x42, // STA $4204 (WRDIVL)
        0xA9, 0x0B, // LDA #$0B        (divisor 11)
        0x8D, 0x06, 0x42, // STA $4206 (WRDIVB -- starts the divide)
    ];
    code.extend(std::iter::repeat(0xEA).take(8)); // eight NOPs
    rom[..code.len()].copy_from_slice(&code);
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;

    let mut system = SnesSystem::load(&rom).expect("loads");
    for _ in 0..(4 + 8) {
        system.step().expect("implemented");
    }
    assert!(
        !system.bus.math.busy(),
        "63 / 11's divide must be finished after the write plus 8 NOPs, \
         the same as on hardware"
    );
    assert_eq!(system.bus.math.rddiv, 5, "63 / 11 quotient");
    assert_eq!(system.bus.math.rdmpy, 8, "63 / 11 remainder");
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
/// $23) it now loads instead of refusing. Super FX ($13/$15/$1A) is
/// likewise absent as of ticket W18-01 (D-014) — moved to
/// `gsu_carts_load_with_the_board_wired_up` below. OBC1 ($25) is likewise
/// absent as of ticket W19-01 (D-013's sibling ruling for this chip) —
/// moved to `obc1_carts_load_with_the_registers_wired_up` below; chipset
/// $23/$24 (nibble 2, hw 3/4) under this same nibble stay refused, since
/// hw=5 is the only assigned OBC1 combination (see `rf_cart::snes`'s own
/// `unsupported_chip_obc1_reported_from_chipset_byte` test).
#[test]
fn every_named_coprocessor_family_is_refused_and_named() {
    for (chipset, expect) in [
        (0x23u8, "OBC1"),
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

/// Ticket W18-01 (D-014): chipset $13/$15/$1A now loads with the GSU
/// board wired up instead of the FR-CORE-013 refusal — a GSU cart keeps
/// the plain LoROM map mode (fullsnes: "the cartridge header declares the
/// cartridge as LoROM"), unlike SA-1's dedicated map mode $23.
#[test]
fn gsu_carts_load_with_the_board_wired_up() {
    for chipset in [0x13u8, 0x15, 0x1A] {
        let system = SnesSystem::load(&lorom_image(0x20, chipset))
            .unwrap_or_else(|e| panic!("chipset ${chipset:02X} (GSU) must load, got {e:?}"));
        assert!(
            system.bus.gsu.is_some(),
            "chipset ${chipset:02X}: GSU board must be installed"
        );
        assert_eq!(system.bus.mode, SnesMapMode::LoRom);
    }
    // A plain LoROM cart never gets one.
    let plain = SnesSystem::load(&lorom_image(0x20, 0x00)).expect("loads");
    assert!(plain.bus.gsu.is_none());
}

/// Ticket W19-01: chipset $25 now loads with the OBC1 registers wired up
/// instead of the FR-CORE-013 refusal — an OBC1 cart keeps the plain
/// LoROM map mode (fullsnes gives it no dedicated one, unlike SA-1's $23).
#[test]
fn obc1_carts_load_with_the_registers_wired_up() {
    let system = SnesSystem::load(&lorom_image(0x20, 0x25))
        .unwrap_or_else(|e| panic!("chipset $25 (OBC1) must load, got {e:?}"));
    assert!(
        system.bus.obc1.is_some(),
        "chipset $25: OBC1 registers must be installed"
    );
    assert_eq!(system.bus.mode, SnesMapMode::LoRom);
    assert_eq!(system.bus.sram.len(), 8 * 1024, "header RAM size is 8 KiB");
    // A plain LoROM cart never gets one.
    let plain = SnesSystem::load(&lorom_image(0x20, 0x00)).expect("loads");
    assert!(plain.bus.obc1.is_none());
}

/// Ticket W19-01 acceptance: reads/writes through the bus reach the real
/// chip behaviour, not just the register-presence check above.
///
/// Exercises every register per fullsnes "SNES Cart OBC1 I/O Ports":
/// - `$7FF0-$7FF3` redirect to `[Base+Index*4+0..3]`.
/// - `$7FF4` write is a 2-bit read-modify-write at `[Base+Index/4+200h]`;
///   read returns the whole undecoded byte.
/// - `$7FF5` base select flips which 220h-byte region `$7FF0-$7FF4`
///   address.
/// - Bytes outside `$7FF0-$7FF7` are ordinary SRAM, so a value written
///   through the redirect ($7FF0) is also visible at its own direct SRAM
///   address (base+index*4), proving the redirect and not a private copy.
#[test]
fn the_obc1_window_reaches_the_registers_and_table_through_the_bus() {
    let mut system = SnesSystem::load(&lorom_image(0x20, 0x25)).expect("OBC1 loads");

    // Base defaults to $7C00 (bit0=0); pick index 3.
    system.bus.write(0x00_7FF6, 3);
    system.bus.write(0x00_7FF0, 0x11); // Xloc
    system.bus.write(0x00_7FF1, 0x22); // Yloc
    system.bus.write(0x00_7FF2, 0x33); // Tile
    system.bus.write(0x00_7FF3, 0x44); // Attr
    assert_eq!(system.bus.read(0x00_7FF0), 0x11);
    assert_eq!(system.bus.read(0x00_7FF3), 0x44);
    // The same bytes are ordinary SRAM at their direct address:
    // base($7C00) + index(3)*4 = $7C0C, within the $6000-$7FFF window.
    assert_eq!(system.bus.read(0x00_7C0C), 0x11);
    assert_eq!(system.bus.read(0x00_7C0F), 0x44);

    // $7FF4 (Bits): index 3 -> byte at Base+0/4+200h = $7C00+$200 = $7E00,
    // bit position (3&3)*2 = 6. A write of 0b10 there must only touch bits
    // 6-7, leaving whatever else lives in that shared byte untouched.
    system.bus.write(0x00_7E00, 0b0011_1111); // seed the byte directly
    system.bus.write(0x00_7FF4, 0b10); // value's low 2 bits matter
    assert_eq!(
        system.bus.read(0x00_7E00),
        0b10_111111,
        "only bits 6-7 must change"
    );
    // Read of $7FF4 returns the whole raw byte, unshifted.
    assert_eq!(system.bus.read(0x00_7FF4), system.bus.read(0x00_7E00));

    // A full-byte write (nothing documents the other 6 bits as required
    // to be zero — a title need only care about its own field) must
    // still only touch bits 6-7, and must not panic (FR-CORE-013's
    // "never a crash" — an earlier draft shifted the raw byte before
    // masking, which happened to be equivalent here but was untested at
    // this exact boundary).
    system.bus.write(0x00_7E00, 0b0011_1111);
    system.bus.write(0x00_7FF4, 0xFF);
    assert_eq!(
        system.bus.read(0x00_7E00),
        0b11_111111,
        "a full-byte write must still only set bits 6-7"
    );

    // Base select ($7FF5): flipping to $7800 moves the SAME index's table
    // entry to a different SRAM address.
    system.bus.write(0x00_7FF5, 0x01);
    system.bus.write(0x00_7FF0, 0x99);
    assert_eq!(system.bus.read(0x00_7800 + 3 * 4), 0x99);
    // The old $7C00-region byte is untouched by the base flip.
    assert_eq!(system.bus.read(0x00_7C0C), 0x11);

    // Index isn't auto-incremented (fullsnes): writing $7FF0 again with
    // the same index overwrites the same cell rather than advancing.
    system.bus.write(0x00_7FF0, 0xAB);
    assert_eq!(system.bus.read(0x00_7800 + 3 * 4), 0xAB);
}

/// Ticket W18-01 acceptance: the SNES side can boot a GSU cart to its
/// reset vector and run, with GO reading back `false` until software
/// writes R15's MSB.
#[test]
fn gsu_cart_boots_and_go_reads_back_after_r15_write() {
    let mut system = SnesSystem::load(&lorom_image(0x20, 0x15)).expect("GSU cart loads");
    let gsu = system.bus.gsu.as_ref().expect("GSU board installed");
    assert!(!gsu.regs.go(), "GO must be clear at reset");

    // Write R15 (PC): LSB then MSB, mirroring fullsnes's documented
    // protocol. MSB write sets GO.
    system.bus.write(0x00_301E, 0x34);
    system.bus.write(0x00_301F, 0x12);
    let gsu = system.bus.gsu.as_ref().expect("GSU board installed");
    assert!(gsu.regs.go(), "writing R15's MSB must set GO");
    assert_eq!(gsu.regs.r15(), 0x1234);

    // SFR write can force GO back to 0 (fullsnes: "can be forcefully=0
    // via 3030h").
    system.bus.write(0x00_3030, 0x00);
    assert!(!system.bus.gsu.as_ref().unwrap().regs.go());
}

/// Ticket W18-01 acceptance: SCMR RON/RAN gate which side owns the
/// ROM/RAM bus — while the GSU owns it, the SNES side's own read sees
/// open bus rather than the cartridge.
///
/// Ticket W18-06 (D-016) tried and reverted a `GO && RON`/`GO && RAN`
/// reading of this rule for READS too — a traced Star Fox (USA) boot
/// sets RON=1 with GO=0 across 2,361 distinct ROM reads scattered
/// through ordinary code (not just the exception-vector region) and
/// expects open bus at every one of them; gating those on GO instead
/// showed the CPU real ROM, which led it to a bad jump into
/// zero-initialized WRAM and a stray STP, measured as `boot_census_child`
/// flipping Star Fox (USA)/(Rev 1)/(Rev 2) from `rendered` to `uniform`.
/// This test's ORIGINAL raw-bit assertion is therefore still correct and
/// deliberately unchanged: RON/RAN gate SNES READS unconditionally. Only
/// SNES WRITES to GSU RAM are gated by GO too (`Gsu::owns_ram_bus`,
/// `SnesBus::write`'s `Target::GsuRam` arm) — see
/// `snes_ram_setup_write_lands_even_with_ran_set_while_the_gsu_is_stopped`
/// below for the write-side fix this asymmetry supports.
#[test]
fn gsu_scmr_ron_ran_gate_the_snes_sides_own_reads() {
    let mut system = SnesSystem::load(&lorom_image(0x20, 0x15)).expect("GSU cart loads");
    system.bus.rom[0] = 0x77;
    // Before RON is set, the SNES reads its own ROM normally.
    assert_eq!(system.bus.read(0x00_8000), 0x77);

    // $303Ah SCMR: bit 4 = RON, bit 3 = RAN. The write itself drives
    // `open_bus` to the value written ($10), so a read right after would
    // trivially "see open bus" whether or not RON gating works — drive a
    // different, distinguishing sentinel first to prove the ROM byte
    // really is being replaced by open bus, not just coincidentally equal
    // to the SCMR write's own value.
    system.bus.write(0x00_303A, 0x10);
    system.bus.open_bus = 0xAB;
    assert_eq!(
        system.bus.read(0x00_8000),
        0xAB,
        "RON=1: the SNES side must see open bus, not the cartridge"
    );

    system.bus.write(0x00_303A, 0x00);
    assert_eq!(
        system.bus.read(0x00_8000),
        0x77,
        "RON=0: the SNES side owns the ROM bus again"
    );
}

/// Ticket W18-01 acceptance #2: the GSU's IRQ flag (SFR bit 15) ORs into
/// the same 65C816 IRQ input SA-1's `$2209` bit 7 already does, and
/// reading SFR's high byte clears it (fullsnes: "reset on read").
#[test]
fn gsu_irq_flag_ors_into_the_cpu_and_clears_on_read() {
    let mut system = SnesSystem::load(&lorom_image(0x20, 0x15)).expect("GSU cart loads");
    // NOP forever at the reset vector, with I clear so the ordinary IRQ
    // dispatch path (not the WAI one) is what fires.
    system.bus.rom[0] = 0xEA; // NOP, at $00:8000 (the reset vector `lorom_image` sets)
    system.cpu.set_flag(crate::cpu::flags::I, false);
    system.bus.gsu.as_mut().unwrap().regs.set_irq_for_test(true);
    system.step().expect("NOP steps");
    // An IRQ was taken: PC left the plain NOP's successor address for the
    // IRQ vector's target instead.
    assert_ne!(
        system.cpu.pc, 0x8001,
        "the GSU IRQ must have been dispatched"
    );
    // The flag is still set — dispatching the interrupt does not itself
    // acknowledge it, exactly as real hardware works: the handler must
    // read SFR. Simulate that handler read now.
    assert!(system.bus.gsu.as_ref().unwrap().regs.irq_pending());
    let hi = system.bus.read(0x00_3031);
    assert_eq!(hi & 0x80, 0x80, "SFR's high byte reports the IRQ bit set");
    assert!(
        !system.bus.gsu.as_ref().unwrap().regs.irq_pending(),
        "fullsnes: IRQ is \"reset on read\" — reading SFR's high byte must clear it"
    );
}

/// Ticket W18-04 follow-up (coordinator-directed, tracing the Star Fox
/// census regression): pins the credit-based interleave's actual
/// granularity — a real concern the coordinator raised was "does a single
/// `SnesSystem::step` call hand the GSU an unbounded credit, starving the
/// 65C816 for millions of GSU opcodes at a time". It does not: every
/// `SnesSystem::step` call executes exactly ONE 65C816 instruction, THEN
/// deposits only THAT instruction's own master-cycle cost as GSU credit
/// before returning — so the GSU can never run more opcodes in one call
/// than that one instruction's worth of real 10.7/21.4MHz time affords,
/// which for an ordinary (non-DMA-heavy) 65C816 instruction is a handful
/// of GSU opcodes, not millions. This test runs the 65C816 on a tight
/// `JMP $8000` loop (cheap, constant per-instruction cost) alongside a
/// GSU `NOP;BRA;NOP` loop (ticket W18-04's own
/// `clsr_1_runs_twice_the_opcodes_per_master_cycle_of_clsr_0` unit test
/// uses the identical GSU program) and asserts, over many
/// `SnesSystem::step` calls, that the GSU's `instructions_executed`
/// counter never jumps by more than a small bound in any ONE call.
#[test]
fn gsu_credit_per_step_is_bounded_by_that_steps_own_instruction_cost() {
    let mut rom = lorom_image(0x20, 0x15);
    // 65C816 program at $00:8000 (the reset vector `lorom_image` sets):
    // `JMP $8000` — an unconditional 3-byte jump back to itself, the
    // cheapest possible "keeps stepping forever" loop.
    rom[0] = 0x4C;
    rom[1] = 0x00;
    rom[2] = 0x80;
    // GSU program at GSU address $0100 (bank 0, PBR's reset value):
    // NOP; BRA -3 (back to the NOP); NOP (the branch's delay slot) — the
    // same infinite loop `clsr_1_runs_twice_the_opcodes_per_master_cycle_
    // of_clsr_0` already pins in isolation.
    rom[0x100] = 0x01;
    rom[0x101] = 0x05;
    rom[0x102] = 0xFD;
    rom[0x103] = 0x01;
    let mut system = SnesSystem::load(&rom).expect("GSU cart loads");
    // SNES-side: R15 = $0100, which also sets GO (fullsnes: R15.MSB write
    // "does also set GO=1").
    system.bus.write(0x00_301E, 0x00);
    system.bus.write(0x00_301F, 0x01);
    assert!(system.bus.gsu.as_ref().unwrap().regs.go());

    // A real GSU opcode's cheapest documented cost is 1 GSU cycle; a
    // `JMP $8000` instruction (an ordinary, uncontended 3-byte absolute
    // jump, no DMA) spends only a handful of master cycles per step — a
    // generous bound (`50`) catches an actual "one step hands out a
    // massive credit" regression by orders of magnitude while leaving
    // plenty of headroom over the ~2-6 opcodes/step this project's own
    // census tracing (docs/TESTING.md W18-04) measured for real titles.
    const MAX_GSU_OPS_PER_STEP: u64 = 50;
    let mut total_gsu_ops = 0u64;
    for _ in 0..5000 {
        let before = system
            .bus
            .gsu
            .as_ref()
            .unwrap()
            .regs
            .instructions_executed();
        system.step().expect("JMP is implemented");
        let after = system
            .bus
            .gsu
            .as_ref()
            .unwrap()
            .regs
            .instructions_executed();
        let this_step = after - before;
        assert!(
            this_step <= MAX_GSU_OPS_PER_STEP,
            "one SnesSystem::step call ran {this_step} GSU opcodes -- \
             the credit-based interleave must bound this to roughly one \
             65C816 instruction's worth of real GSU time, not an \
             unbounded burst"
        );
        total_gsu_ops += this_step;
    }
    // Sanity: the GSU is actually making progress (not starved to zero by
    // an over-correction), at a rate in the same order of magnitude as
    // the real hardware ratio (~3-4 GSU opcodes per 65C816 instruction
    // for this cheap a loop, per this project's own clock-conversion
    // constants).
    assert!(
        total_gsu_ops > 5000,
        "the GSU should average at least ~1 opcode per step for a loop \
         this cheap, got {total_gsu_ops} over 5000 steps"
    );
}

/// Ticket W18-04 follow-up 5: the actual Star Fox boot regression's root
/// cause was `rf_cart::snes::superfx_expansion_ram_kib` defaulting a real
/// dump's unpopulated extended header (a `$FF` fill byte, exactly Star
/// Fox (USA)'s own header shape) to `0` GSU RAM — with no RAM at all, the
/// game's own decompressor priming its ROM read pointer from a value the
/// SNES DMAs into cartridge RAM before GO reads back zero instead of the
/// real base address. This end-to-end test reproduces the exact real
/// header shape (chipset `$13`, extended-header byte `$FF`) and proves
/// the full path: the cart parses to 32 KiB of GSU RAM (fullsnes "RAM Size
/// for Starfox/Starwing is 32Kbytes"), a plain 65C816 byte write to
/// `$70:0000` lands in it (SCMR's reset-value `RAN=0` means the SNES owns
/// the bus, per fullsnes SCMR bit 3 "0=SNES, 1=GSU"), and a GSU `LDB (R1)`
/// with `R1=0` reads that exact byte back — the same RAM-pointer idiom
/// Star Fox's own code uses, just with a literal address instead of one
/// loaded from ROM.
#[test]
fn snes_write_to_gsu_ram_is_visible_to_a_gsu_ldb() {
    let mut rom = lorom_image(0x20, 0x13); // Star Fox's real chipset byte.
    rom[0x7FC0 - 3] = 0xFF; // Unpopulated extended header (Star Fox's own).
    let cart = rf_cart::Cartridge::load(&rom).expect("GSU cart accepted");
    let header = match &cart {
        rf_cart::Cartridge::Snes { header, .. } => header,
        other => panic!("expected an SNES cartridge, got {other:?}"),
    };
    assert_eq!(
        header.coprocessor,
        rf_cart::Coprocessor::SuperFx {
            version: rf_cart::SuperFxVersion::Gsu1,
            ram_kib: 32,
        },
        "an unpopulated extended header must default to fullsnes's stated \
         32 KiB for this board shape, not 0"
    );

    // 65C816 program at $00:8000 (the reset vector `lorom_image` sets):
    // `JMP $8000` — an unconditional 3-byte jump back to itself, the same
    // "keeps stepping forever" idle loop the credit-interleave test above
    // uses, so `system.step()` has something to run while the GSU works.
    rom[0] = 0x4C;
    rom[1] = 0x00;
    rom[2] = 0x80;
    // GSU program at $0100 (bank 0, PBR's reset value): IWT R1,#0000; TO
    // R2; ALT1; LDB (R1) -> R2 = ram[R1] (zero-extended byte); NOP forever.
    rom[0x100] = 0xF1; // IWT R1,#$0000
    rom[0x101] = 0x00;
    rom[0x102] = 0x00;
    rom[0x103] = 0x12; // TO R2
    rom[0x104] = 0x3D; // ALT1
    rom[0x105] = 0x41; // LDB (R1)
    rom[0x106] = 0x01; // NOP
    rom[0x107] = 0x05; // BRA -3 (spin on the NOP forever)
    rom[0x108] = 0xFD;
    rom[0x109] = 0x01; // NOP (BRA's delay slot)
    let mut system = SnesSystem::load(&rom).expect("GSU cart loads");

    // The plain 65C816 write path (no DMA needed to exercise the mapping):
    // $70:0000 is GSU RAM bank $70, offset $0000 (fullsnes "70-71:0000-
    // FFFF Game Pak RAM").
    assert!(
        !system.bus.gsu.as_ref().unwrap().regs.ran(),
        "SCMR's reset value must leave RAN=0 (SNES owns the RAM bus)"
    );
    system.bus.write(0x70_0000, 0x77);
    assert_eq!(
        system.bus.gsu.as_ref().unwrap().ram[0],
        0x77,
        "the SNES-side write must land in Gsu RAM, not be dropped or \
         misrouted to cart SRAM/open bus"
    );

    // Start the GSU: R15 = $0100 sets GO (fullsnes: R15.MSB write "does
    // also set GO=1").
    system.bus.write(0x00_301E, 0x00);
    system.bus.write(0x00_301F, 0x01);
    assert!(system.bus.gsu.as_ref().unwrap().regs.go());
    for _ in 0..10 {
        system.step().expect("GSU program is implemented");
    }
    let r2 = {
        let gsu = system.bus.gsu.as_ref().unwrap();
        let lo = gsu.regs.peek(0x3004).unwrap();
        let hi = gsu.regs.peek(0x3005).unwrap();
        u16::from_le_bytes([lo, hi])
    };
    assert_eq!(
        r2, 0x77,
        "GSU LDB (R1) must read back the exact byte the SNES wrote to \
         $70:0000, proving the write path is not dropped/misrouted"
    );
}

/// Ticket W18-04 follow-up 5, part 2: the coordinator's explicit second
/// scenario — a DMA transfer, not just a plain CPU store, landing in GSU
/// RAM through the `$6000-$7FFF` mirror (`mapping::gsu_target`'s
/// `mirrored_system` arm) rather than through bank `$70` directly. Proves
/// `SnesBus::run_channel`'s generic `self.write(a, v)` (the same path
/// every DMA channel uses, shared with plain WRAM-to-WRAM transfers —
/// `dma_moves_real_bytes_through_the_wram_port` pins that path in
/// isolation) reaches `Target::GsuRam` through the mirror exactly like it
/// does through `$70:xxxx`, and that both address forms share the SAME
/// underlying index (`gsu_target`'s `idx = offset - 0x6000` for the
/// mirror vs `index = (bank-0x70)<<16 | offset` for bank `$70` — both `0`
/// for offset `$6000`/bank `$70` offset `$0000`).
///
/// The transfer reads through `$2180` WMDATA (the WRAM data port, real
/// and already exercised in isolation by `dma_moves_real_bytes_through_
/// the_wram_port`) in reverse (B-bus source, A-bus destination) — the
/// SNES-side idiom for "copy WRAM into an arbitrary A-bus address" DMA
/// direction, landing the byte at `$00:6000` (bank `$00`'s `mirrored_
/// system` GSU-RAM window).
#[test]
fn dma_transfer_into_the_6000_mirror_reaches_gsu_ram() {
    let mut rom = lorom_image(0x20, 0x13); // Star Fox's real chipset byte.
    rom[0x7FC0 - 3] = 0xFF; // Unpopulated extended header (32 KiB GSU RAM).
    rom[0] = 0x4C; // 65C816: JMP $8000 (idle self-loop).
    rom[1] = 0x00;
    rom[2] = 0x80;
    let mut system = SnesSystem::load(&rom).expect("GSU cart loads");
    assert_eq!(
        system.bus.gsu.as_ref().unwrap().ram.len(),
        32 * 1024,
        "the fixed cart-header follow-up must give this board 32 KiB"
    );

    // Source: WRAM offset $0050, reachable through $2180-$2183.
    system.bus.wram[0x50] = 0x99;
    system.bus.write(0x00_2181, 0x50); // WMADDL
    system.bus.write(0x00_2182, 0x00); // WMADDM
    system.bus.write(0x00_2183, 0x00); // WMADDH

    // Channel 0: control=$80 (reverse: B->A, pattern 0, A-bus increment),
    // B-bus=$2180 (WMDATA), A-bus=$00:6000 (the GSU-RAM mirror), 1 byte.
    system.bus.write(0x00_4300, 0x80);
    system.bus.write(0x00_4301, 0x80);
    system.bus.write(0x00_4302, 0x00);
    system.bus.write(0x00_4303, 0x60);
    system.bus.write(0x00_4304, 0x00);
    system.bus.write(0x00_4305, 0x01);
    system.bus.write(0x00_4306, 0x00);
    system.bus.write(0x00_420B, 0x01);
    system.bus.service_dma();

    assert_eq!(
        system.bus.gsu.as_ref().unwrap().ram[0],
        0x99,
        "a DMA transfer through the $6000-$7FFF mirror must land in the \
         same GSU RAM buffer $70:0000 does, not cart SRAM or open bus"
    );
}

/// Ticket W18-06 (D-016, both titles' traces): pins the exact real shape
/// a traced Vortex boot uses and this project's original raw-RAN WRITE
/// gate broke. Two traces jointly decided this fix: Vortex's own boot
/// sets SCMR=$39 (RON=1, RAN=1) with GO=0 confirmed on every one of 5530
/// sampled SCMR writes (docs/TESTING.md's W18-06 section), then writes
/// its whole 8 KiB `$6000-$7FFF` GSU RAM mirror as plain SNES-side
/// stores BEFORE ever setting GO — real hardware evidence that a
/// stopped GSU (`GO=0`) is not actually contending for the RAM bus for
/// a write no matter what SCMR says. A traced Star Fox (USA) boot,
/// separately, showed the identical GO-gating applied to READS regresses
/// (see `gsu_scmr_ron_ran_gate_the_snes_sides_own_reads`'s updated doc),
/// so the two traces jointly support gating ONLY writes on GO, not reads
/// — `Gsu::owns_ram_bus` (GO&&RAN), used only in `SnesBus::write`'s
/// `Target::GsuRam` arm.
#[test]
fn snes_ram_setup_write_lands_even_with_ran_set_while_the_gsu_is_stopped() {
    let mut rom = lorom_image(0x20, 0x14); // Vortex's real chipset byte ($14: GSU+RAM, no battery).
    rom[0x7FC0 - 3] = 0x05; // Extended header: 32 KiB expansion RAM (fullsnes "$FFBDh=05h..06h").
    rom[0] = 0x4C; // 65C816: JMP $8000 (idle self-loop) — the reset vector target.
    rom[1] = 0x00;
    rom[2] = 0x80;
    // GSU program at $0100 (bank 0, PBR's reset value), same idiom as
    // `snes_write_to_gsu_ram_is_visible_to_a_gsu_ldb`: IWT R1,#$1FFF (the
    // LAST byte of the SNES's 8 KiB block, so this test proves the WHOLE
    // range landed, not just its first byte); TO R2; ALT1; LDB (R1) -> R2
    // = ram[R1] (zero-extended byte); NOP forever.
    rom[0x100] = 0xF1; // IWT R1,#$1FFF
    rom[0x101] = 0xFF;
    rom[0x102] = 0x1F;
    rom[0x103] = 0x12; // TO R2
    rom[0x104] = 0x3D; // ALT1
    rom[0x105] = 0x41; // LDB (R1)
    rom[0x106] = 0x01; // NOP
    rom[0x107] = 0x05; // BRA -3 (spin on the NOP forever)
    rom[0x108] = 0xFD;
    rom[0x109] = 0x01; // NOP (BRA's delay slot)
    let mut system = SnesSystem::load(&rom).expect("GSU cart loads");
    assert_eq!(
        system.bus.gsu.as_ref().unwrap().ram.len(),
        32 * 1024,
        "Vortex's real header must give this board 32 KiB (fullsnes: \
         $05h -> 32 KiByte)"
    );

    // SCMR = $39 (RON=1, RAN=1): the exact byte the traced boot leaves in
    // place before its RAM setup block, with GO still 0.
    system.bus.write(0x00_303A, 0x39);
    assert!(
        !system.bus.gsu.as_ref().unwrap().regs.go(),
        "GO must still be 0 -- this scenario is entirely pre-launch setup"
    );

    // The whole 8 KiB $6000-$7FFF mirror, one SNES-side store per byte —
    // Vortex's own descending sweep, reproduced as a plain ascending fill
    // (direction does not matter to the bus; only that every byte lands).
    for offset in 0u32..0x2000 {
        let value = (offset & 0xFF) as u8;
        system.bus.write(0x00_6000 + offset, value);
    }
    assert_eq!(
        system.bus.gsu.as_ref().unwrap().ram[0],
        0x00,
        "byte 0 of the block must have landed (offset 0 -> value 0)"
    );
    assert_eq!(
        system.bus.gsu.as_ref().unwrap().ram[0x1FFF],
        0xFF,
        "the LAST byte of the block must also have landed"
    );

    // Now actually start the GSU (sets GO=1 via R15's MSB write,
    // fullsnes) and let its own LDB read the last byte back.
    system.bus.write(0x00_301E, 0x00);
    system.bus.write(0x00_301F, 0x01);
    assert!(system.bus.gsu.as_ref().unwrap().regs.go());
    for _ in 0..10 {
        system.step().expect("GSU program is implemented");
    }
    let r2 = {
        let gsu = system.bus.gsu.as_ref().unwrap();
        let lo = gsu.regs.peek(0x3004).unwrap();
        let hi = gsu.regs.peek(0x3005).unwrap();
        u16::from_le_bytes([lo, hi])
    };
    assert_eq!(
        r2, 0xFF,
        "the GSU's own LDB (R1) must read back the exact byte the SNES \
         wrote at $00:7FFF -- proving the whole 8 KiB block the SNES \
         wrote BEFORE GO was set actually landed in GSU RAM, not just \
         RAM's zero-initialized default"
    );
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
    // Ticket W17-03: $2229/$2226 reset to $00 (every chunk/region
    // protected) — a real ROM enables writes before using either memory,
    // so this test does the same rather than exercising the protected
    // path here (that is `tests::sa1_protection`'s job).
    system.bus.write(0x00_2229, 0xFF); // SIWP: enable all 8 I-RAM chunks
    system.bus.write(0x00_2226, 0x80); // SBWE: enable BW-RAM writes
    system.bus.write(0x00_2228, 0x00); // BWPA: minimum protected floor (256 bytes)

    // I-RAM at $3000-$37FF, banks $00-$3F/$80-$BF.
    system.bus.write(0x00_3000, 0xAB);
    assert_eq!(system.bus.read(0x00_3000), 0xAB);
    assert_eq!(
        system.bus.read(0x80_3000),
        0xAB,
        "I-RAM mirrors across banks"
    );

    // BW-RAM window at $6000-$7FFF, block 0 by default ($2224 resets to
    // 0); offset $100 clears BWPA's 256-byte protected floor.
    system.bus.write(0x00_6100, 0xCD);
    assert_eq!(system.bus.read(0x00_6100), 0xCD);
    // The same byte is visible through the full BW-RAM window at $40:0100.
    assert_eq!(system.bus.read(0x40_0100), 0xCD);

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

/// Ticket W17-02 acceptance #1/#2: the SA-1 has its own CPU, its own bus,
/// and its own reset vector, and the interleave loop actually runs it.
#[test]
fn sa1_boots_from_its_own_reset_vector_once_reset_clears() {
    let mut rom = lorom_image(0x23, 0x35);
    // The SA-1's own program: LDA #$77 at $00:8100 (ROM offset $0100) —
    // deliberately distinct from the main CPU's own code at $00:8000
    // (ROM offset 0, all-zero/BRK here since this test never runs it).
    rom[0x0100] = 0xA9;
    rom[0x0101] = 0x77;
    let mut system = SnesSystem::load(&rom).expect("SA-1 cart loads");
    assert!(
        !system.bus.sa1.as_ref().unwrap().booted,
        "must not have run before Reset ever clears"
    );

    system.bus.write(0x00_2203, 0x00);
    system.bus.write(0x00_2204, 0x81); // CRV: reset vector $8100
    system.bus.write(0x00_2200, 0x00); // clears Reset (and Wait, and the message)

    // One SNES CPU instruction produces plenty of master-clock credit for
    // the SA-1's own two-byte LDA immediate (ticket W17-02's clocking
    // rule interleaves it inside the very same `SnesSystem::step` call).
    system.step().expect("main CPU step");

    let sa1 = system.bus.sa1.as_ref().unwrap();
    assert!(sa1.booted, "the SA-1 must have booted once Reset cleared");
    assert_eq!(
        sa1.cpu.a & 0xFF,
        0x77,
        "must have run its OWN program from its OWN vector, not the SNES CPU's"
    );
}

/// Ticket W17-02 acceptance #2: "the SA-1 is halted at reset until $2200
/// clears the reset/wait bits". The reset-vector fetch happens the instant
/// Reset clears (this module's chosen reading — see `Sa1Regs::sa1_wait_asserted`'s
/// doc), but Wait alone still holds it from executing anything.
#[test]
fn sa1_boots_but_does_not_execute_while_wait_is_asserted() {
    let mut rom = lorom_image(0x23, 0x35);
    rom[0x0100] = 0xA9;
    rom[0x0101] = 0x77;
    let mut system = SnesSystem::load(&rom).expect("SA-1 cart loads");
    system.bus.write(0x00_2203, 0x00);
    system.bus.write(0x00_2204, 0x81);
    // Clear Reset but assert Wait (bit 6) in the same write.
    system.bus.write(0x00_2200, 0x40);
    system.step().expect("main CPU step");
    {
        let sa1 = system.bus.sa1.as_ref().unwrap();
        assert!(
            sa1.booted,
            "the reset-vector fetch happens even while Wait holds execution"
        );
        assert_eq!(
            sa1.cpu.a & 0xFF,
            0x00,
            "must not have executed LDA while Wait is asserted"
        );
    }
    system.bus.write(0x00_2200, 0x00); // release Wait
    system.step().expect("main CPU step");
    assert_eq!(system.bus.sa1.as_ref().unwrap().cpu.a & 0xFF, 0x77);
}

/// Ticket W17-02 acceptance #3: the SA-1 takes an NMI raised from the SNES
/// side ($2200 bit 4) once CIE (`$220A` bit 4) enables it, and vectors
/// through its OWN NMI vector ($2205/$2206) rather than any ROM vector —
/// fullsnes's "these are ALWAYS replacing the normal vectors in ROM".
#[test]
fn sa1_takes_an_nmi_from_the_snes_once_enabled_and_uses_its_own_vector() {
    let mut rom = lorom_image(0x23, 0x35);
    // Main CPU: two NOPs, one per `system.step()` call below — keeps the
    // master-clock credit each call hands the SA-1 small and predictable,
    // rather than whatever a `BRK` (the all-zero default) would cost.
    rom[0x0000] = 0xEA;
    rom[0x0001] = 0xEA;
    // NOP then STP at the boot PC: STP halts the SA-1 right there
    // regardless of exactly how many SA-1 instructions this slice's
    // approximate clocking rule lets one SNES instruction's worth of
    // credit buy, so the test does not have to pin that count down.
    rom[0x0100] = 0xEA;
    rom[0x0101] = 0xDB;
    // Same pattern at the NMI vector target ($9000): NOP then STP.
    rom[0x1000] = 0xEA;
    rom[0x1001] = 0xDB;
    let mut system = SnesSystem::load(&rom).expect("SA-1 cart loads");
    system.bus.write(0x00_2203, 0x00);
    system.bus.write(0x00_2204, 0x81); // reset vector $8100
    system.bus.write(0x00_2205, 0x00);
    system.bus.write(0x00_2206, 0x90); // NMI vector $9000
    system.bus.write(0x00_2200, 0x00); // clear Reset: boots and runs the NOP
    system.step().expect("boot + NOP + STP");
    assert_eq!(
        system.bus.sa1.as_ref().unwrap().cpu.pc,
        0x8102,
        "NOP then STP: halted one byte past the STP itself"
    );

    system.bus.write(0x00_220A, 0x10); // CIE bit 4: enable NMI-from-SNES
    system.bus.write(0x00_2200, 0x10); // CCNT bit 4: raise it
    system
        .step()
        .expect("nmi delivered and its handler's first NOP (and now STP) run");

    let sa1 = system.bus.sa1.as_ref().unwrap();
    // Ticket W14-39: the main CPU's own two NOPs now each charge their
    // real internal cycle too (previously dropped — see `speed.rs`'s
    // doc), so this step hands the SA-1 more master-cycle credit than
    // before and it now has enough to run the NOP AND the STP at the
    // vector target, not just the NOP — same as the boot case above,
    // which already asserts "NOP then STP: halted one byte past the STP
    // itself". This is the ticket's own documented effect ("charging
    // more master cycles per instruction shifts every IRQ/HDMA/NMI
    // timing"), not a regression: the SA-1 is genuinely interleaved on
    // the same master clock, and that clock now moves at its real rate.
    assert_eq!(
        sa1.cpu.pc, 0x9002,
        "must have vectored through $2205/$2206, run the NOP there, run the STP, and halted \
         one byte past it"
    );
    assert_eq!(
        system.bus.read(0x00_2301) & 0x10,
        0x10,
        "CFR must report the NMI status until $220B acks it"
    );
    system.bus.write(0x00_220B, 0x10); // CIC ack
    assert_eq!(system.bus.read(0x00_2301) & 0x10, 0);
}

/// Ticket W17-02 acceptance #3: "the SNES CPU's IRQ line ORed with the
/// SA-1-raised IRQ", and the `$2209` bit 6 -> `$220E`/`$220F` vector
/// override for it.
#[test]
fn the_snes_cpu_takes_an_irq_raised_by_the_sa1_through_the_port_vector() {
    let mut rom = lorom_image(0x23, 0x35);
    // Main CPU: CLI (enable IRQs), then spin on NOPs.
    rom[0x0000] = 0x58; // CLI
    for byte in rom.iter_mut().take(0x20).skip(1) {
        *byte = 0xEA; // NOP
    }
    rom[0x1000] = 0xEA; // NOP at the IRQ port vector target ($9000)
    let mut system = SnesSystem::load(&rom).expect("SA-1 cart loads");
    system.bus.write(0x00_2201, 0x80); // SIE bit 7: enable IRQ-from-SA-1
    system.bus.write(0x00_220E, 0x00);
    system.bus.write(0x00_220F, 0x90); // SIV: port IRQ vector $9000
    system.bus.write(0x00_2209, 0x40 | 0x80); // SCNT: select port vector, raise IRQ

    system.step().expect("CLI"); // clears the I flag
    system
        .step()
        .expect("IRQ delivered and its handler's first NOP run");

    assert_eq!(
        system.cpu.pc, 0x9001,
        "must have vectored through $220E/$220F, run the NOP there, and advanced past it"
    );
    assert_eq!(
        system.bus.read(0x00_2300) & 0x80,
        0x80,
        "SFR must report the IRQ-from-SA-1 status until $2202 acks it"
    );
    system.bus.write(0x00_2202, 0x80); // SIC ack
    assert_eq!(system.bus.read(0x00_2300) & 0x80, 0);
}

/// Ticket W17-02 acceptance #4: "SA-1 state is in the save state with a
/// round-trip test" — the second CPU's own register file and its
/// booted/credit scheduling state, alongside the control/message
/// registers W17-01 already covered.
#[test]
fn sa1_cpu_state_survives_a_cart_region_round_trip() {
    let mut rom = lorom_image(0x23, 0x35);
    rom[0x0100] = 0xA9;
    rom[0x0101] = 0x77; // LDA #$77
    let mut system = SnesSystem::load(&rom).expect("SA-1 cart loads");
    system.bus.write(0x00_2203, 0x00);
    system.bus.write(0x00_2204, 0x81);
    system.bus.write(0x00_2200, 0x00);
    system.step().expect("boot + LDA");
    assert_eq!(system.bus.sa1.as_ref().unwrap().cpu.a & 0xFF, 0x77);

    struct MemStream {
        buf: Vec<u8>,
        at: usize,
    }
    impl rf_core_api::StateWriter for MemStream {
        fn write_all(&mut self, bytes: &[u8]) -> Result<(), rf_core_api::StateError> {
            self.buf.extend_from_slice(bytes);
            Ok(())
        }
    }
    impl rf_core_api::StateReader for MemStream {
        fn read_exact(&mut self, out: &mut [u8]) -> Result<(), rf_core_api::StateError> {
            let end = self.at + out.len();
            out.copy_from_slice(&self.buf[self.at..end]);
            self.at = end;
            Ok(())
        }
    }

    let mut stream = MemStream {
        buf: Vec::new(),
        at: 0,
    };
    system
        .save_region(crate::state::StateRegion::Cart, &mut stream)
        .expect("save cart region");

    let mut restored = SnesSystem::load(&rom).expect("SA-1 cart loads");
    restored
        .load_region(crate::state::StateRegion::Cart, &mut stream)
        .expect("load cart region");

    let sa1 = restored.bus.sa1.as_ref().unwrap();
    assert_eq!(
        sa1.cpu.a & 0xFF,
        0x77,
        "the SA-1's own register file must round-trip"
    );
    assert!(
        sa1.booted,
        "booted must round-trip, or the next step would re-fetch the reset vector"
    );
}

/// A `BRA *` cartridge with a full-line BG1 tile and a window mask on it —
/// enough graphics for a window-register write's effect to show up in
/// composed pixels, and a CPU that does nothing so the test controls
/// every register write itself (ticket W14-31).
///
/// Window registers, not `$2100`, are the chosen "is_segmentable register"
/// (the acceptance's own text allows either): `$2100`'s forced-blank flag
/// is deliberately NOT part of `LineState` (see `LineState`'s field list),
/// so a batch composer only ever reconstructs it correctly for the exact
/// line that captured a `line_regs` snapshot — every OTHER line composes
/// forced-blank from whatever is live at the end of the frame regardless
/// of this fix, which would make a cross-line assertion fail for reasons
/// this ticket's `write_scope` (buffer lifetime only, not what is latched)
/// does not cover. `$2126`/`$2127` (W1 left/right) are `Windows`, which
/// [`Ppu::latch_line`] captures into `LineState` for EVERY visible line in
/// real time — exactly the record this ticket's fix stops discarding
/// before a batch composer can read it.
fn window_test_system() -> SnesSystem {
    let mut rom = lorom_image(0x20, 0x00);
    rom[0x0000] = 0x80; // BRA -2: spin forever, so only the test's own
    rom[0x0001] = 0xFE; // `system.bus.write` calls change any register.
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    let mut system = SnesSystem::load(&rom).expect("loads");

    let ppu = &mut system.bus.ppu;
    ppu.forced_blank = false;
    ppu.bg_mode = 0;
    ppu.bgs[0].enabled = true;
    ppu.bgs[0].tilemap_base = 0;
    ppu.bgs[0].char_base = 0x1000;
    // Character 1, 2bpp: plane 0 all ones -> colour 1 everywhere.
    for row in 0..8u16 {
        let at = usize::from(0x1000 + 8 + row) * 2;
        ppu.vram[at] = 0xFF;
        ppu.vram[at + 1] = 0x00;
    }
    // Tilemap: every entry of the whole 32x32 map is tile 1, so every
    // visible row draws BG1 rather than transparent backdrop — one
    // tilemap ROW (32 entries) only covers the tile's own 8 pixels of
    // height, and this test checks rows well past line 8.
    for i in 0..(32u16 * 32) {
        let at = usize::from(i) * 2;
        ppu.vram[at] = 1;
        ppu.vram[at + 1] = 0;
    }
    ppu.write_register(0x212E, 0x01); // WOBJSEL/main-mask: window applies to BG1
    ppu.write_register(0x2123, 0x02); // W12SEL: window 1 masks BG1
    ppu.write_register(0x2126, 10); // W1 left
    ppu.write_register(0x2127, 20); // W1 right
    system
}

/// Is BG1 masked out (window-excluded backdrop) at column `x` of visible
/// row `y`?
fn masked_at(system: &mut SnesSystem, y: u16, x: usize) -> bool {
    system.bus.ppu.render_scanline(y).pixels[x].layer == rf_core_api::PixelLayer::Backdrop
}

/// Step until the beam is inside hardware line `line`'s active display —
/// i.e. [`crate::timing::Timing::mid_line_position`] would attribute a
/// register write here as a mid-line one. Bounded, per law 8: a `BRA *`
/// spin never halts on its own, so an unreachable `line` must fail the
/// test rather than loop forever.
fn step_to_mid_line(system: &mut SnesSystem, line: u16) {
    for _ in 0..2_000_000u64 {
        if system.bus.timing.line == line && system.bus.timing.mid_line_position().is_some() {
            return;
        }
        system.step().expect("BRA is implemented");
    }
    panic!("never reached hardware line {line}'s active display in 2,000,000 steps");
}

/// Ticket W14-31 acceptance #3 (first case): a mid-frame register write on
/// a late visible line must reach that SAME frame's own composition, not
/// fall back to whatever the live registers happen to be once
/// `Step::Frame`/`render_frame` regains control after the boundary.
///
/// Reproduces the shape `PROBE_LINEWRITES=216` traced for Final Fantasy
/// Mystic Quest in the W14-29 write-up (docs/TESTING.md): a write on
/// hardware line 216, composed only after the frame that contains it has
/// already fully elapsed.
#[test]
fn a_mid_frame_write_on_a_late_visible_line_survives_into_that_frames_own_composition() {
    let mut system = window_test_system();

    // Baseline: the whole frame masks columns 10-20, everywhere, before
    // any write moves the window — asserted on rows spanning the frame
    // so the "later lines changed, earlier ones didn't" comparison below
    // is against a real baseline rather than an assumed one.
    for y in [0u16, 100, 214, 217, 223] {
        assert!(
            masked_at(&mut system, y, 15),
            "row {y}: baseline masks x=15"
        );
        assert!(
            !masked_at(&mut system, y, 50),
            "row {y}: baseline does not mask x=50"
        );
    }

    // Hardware line 216 = visible row 215 (ticket W7-13's off-by-one).
    step_to_mid_line(&mut system, 216);
    system.bus.write(0x00_2126, 100); // W1 left
    system.bus.write(0x00_2127, 200); // W1 right

    // Run this same frame to completion and into the next one, so
    // `render_frame` composes the frame the write landed in — the exact
    // call shape `SnesCore::step(Step::Frame)`'s `emit_frame` also uses.
    let frame = system.render_frame(2_000_000).expect("BRA is implemented");

    // Re-derive from the SAME picture `render_frame` returned, not a
    // fresh `render_scanline` call, so the assertion is on what a real
    // caller (the frame sink) actually received.
    let masked = |y: usize, x: usize| frame[y][x] == 0;
    assert!(masked(0, 15), "row 0: kept the old span (10-20)");
    assert!(!masked(0, 50), "row 0: x=50 is outside the old span");
    assert!(
        masked(214, 15),
        "row 214 (line 215, just before the write): kept the old span"
    );
    assert!(
        masked(217, 100),
        "row 217 (line 218, after the write): reflects the new span (100-200)"
    );
    assert!(
        !masked(217, 15),
        "row 217: x=15 is outside the new span, no longer masked"
    );
    assert!(
        masked(223, 150),
        "the last visible row also reflects the new span"
    );
}

/// Ticket W14-31 acceptance #3 (second case): a register write during
/// vblank must NOT be applied to the frame that just finished — it can
/// only ever be seen by the frame that has not started its active
/// display yet.
#[test]
fn a_write_during_vblank_is_not_applied_to_the_just_completed_frame() {
    let mut system = window_test_system();

    // Run to vblank of the frame the test will compose. `in_vblank` is
    // coarse (any line past the boundary), which is exactly what is
    // wanted here — the write must land somewhere in the stretch between
    // this frame's last active line and the next frame's first one.
    let mut n = 0u64;
    while !system.bus.timing.in_vblank() && n < 2_000_000 {
        system.step().expect("BRA is implemented");
        n += 1;
    }
    assert!(system.bus.timing.in_vblank(), "must have reached vblank");

    // This write happens strictly AFTER the frame being composed latched
    // every one of its own visible lines — moving the span here must be
    // invisible to that frame's picture.
    system.bus.write(0x00_2126, 100);
    system.bus.write(0x00_2127, 200);

    let frame = system.render_frame(2_000_000).expect("BRA is implemented");
    let masked = |y: usize, x: usize| frame[y][x] == 0;
    assert!(
        masked(0, 15),
        "the just-completed frame must still show the OLD span at row 0"
    );
    assert!(
        masked(223, 15),
        "...and at the last visible row too — the vblank write reached \
         no line of this frame"
    );
    assert!(
        !masked(0, 150),
        "and must not show the NEW span anywhere in this frame"
    );
}

/// Ticket W14-28 (second, independent defect found investigating Full
/// Throttle - All-American Racing (USA) (Beta)'s regression): per the WDC
/// W65C816S datasheet, `WAI` resumes "upon the occurrence of a hardware
/// interrupt (NMI, IRQ if the interrupt disable flag is clear, or ABORT)"
/// -- an IRQ line assertion wakes `WAI` even with the interrupt disable
/// flag (`I`) SET; `I` only decides whether the interrupt is *dispatched*
/// (vector fetch, push PC/P, jump to handler) or whether `WAI` merely
/// "resumes with the next instruction". Before this fix, `SnesSystem::step`
/// only ever cleared `Cpu::stopped` by calling `Cpu::interrupt` -- which
/// this crate's IRQ branch gates on `!flag(I)` -- so a title that executed
/// `WAI` with `I` set (the ordinary idiom for waiting on the H/V IRQ alone,
/// since NMI needs no unmasking) parked forever the moment only a masked
/// IRQ fired: nothing ever cleared `stopped`. Traced to `$81:CB94`'s `WAI`
/// in the ROM above (`docs/TESTING.md`'s W14-28 write-up), parked at frame
/// 108 with `NmiTimen` bit 7 set but the H/V comparison's `I`-masked fire
/// never dispatched.
#[test]
fn wai_wakes_on_a_masked_irq_without_dispatching() {
    let mut rom = lorom_image(0x20, 0x00);
    rom[0x0000] = 0xCB; // WAI
    rom[0x0001] = 0xEA; // NOP -- where a masked wake must resume
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    let mut system = SnesSystem::load(&rom).expect("loads");
    // The 65816 resets with I set (interrupts disabled) -- the masked
    // case this test targets, with no SEI needed to reach it.
    assert!(system.cpu.flag(crate::cpu::flags::I));

    system.step().expect("WAI is implemented");
    assert!(system.cpu.stopped, "WAI must halt instruction execution");
    assert!(
        system.cpu.wai,
        "and must be recorded as a WAI halt, not STP"
    );
    let pc_after_wai = system.cpu.pc;

    // The H/V IRQ comparison latching a match while I is still set --
    // exactly what a per-frame "wait for the IRQ line" idiom produces.
    system.bus.irq.fired = true;
    system.step().expect("still implemented while stopped");

    assert!(
        !system.cpu.stopped,
        "a masked IRQ must still wake WAI (WDC 65C816S datasheet)"
    );
    assert!(!system.cpu.wai);
    assert_eq!(
        system.cpu.pc, pc_after_wai,
        "a masked IRQ resumes at the next instruction -- it must not \
         dispatch to the interrupt handler"
    );
    assert!(
        system.cpu.flag(crate::cpu::flags::I),
        "no dispatch occurred, so I must be untouched"
    );
}

/// The same masked-IRQ line assertion must NOT wake `STP`: per the
/// datasheet, `STP` "can only be restarted... by...a hardware RESET" --
/// unlike `WAI`, no interrupt of any kind wakes it. `Cpu::wai` is what
/// lets `SnesSystem::step` tell the two halts apart (ticket W14-28).
#[test]
fn stp_does_not_wake_on_a_masked_irq() {
    let mut rom = lorom_image(0x20, 0x00);
    rom[0x0000] = 0xDB; // STP
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    let mut system = SnesSystem::load(&rom).expect("loads");

    system.step().expect("STP is implemented");
    assert!(system.cpu.stopped);
    assert!(!system.cpu.wai, "STP is not a WAI halt");

    system.bus.irq.fired = true;
    system.step().expect("still implemented while stopped");
    assert!(
        system.cpu.stopped,
        "STP must stay halted -- it wakes only on reset, never an IRQ"
    );
}
/// Ticket W14-47 follow-up (2026-09-20 census regressions: Magical Drop
/// II, Super Black Bass, Tecmo Super Bowl III, The Terminator, War
/// 3010). W14-47 read fullsnes "SNES Interrupts" -- "The CPU includes
/// another internal NMI flag, which gets set when '[4200h].7 AND
/// [4210h].7' changes from 0-to-1" -- as licensing a dispatch from the
/// ENABLE operand's own rise (a `$4200` write turning bit 7 on while
/// `$4210` bit 7 is already stale), not only the flag's. Real ROMs
/// refute it: The Terminator and Super Black Bass both enable NMI for
/// the first time ever while `$4210` is stale from an earlier vblank
/// (NMI was off through it), and neither dispatches on hardware -- both
/// wait for the next genuine vblank edge. This test pins that: a `$4200`
/// write must never dispatch by itself, no matter what `$4210` holds:
/// only a real vblank edge (`events.vblank_started`, driven by
/// `Timing::advance`) may.
#[test]
fn nmi_enable_write_never_dispatches_by_itself_even_with_a_stale_flag() {
    let mut rom = lorom_image(0x20, 0x00);
    rom[0x0000] = 0xEA; // NOP, ×2 -- something harmless to execute
    rom[0x0001] = 0xEA;
    rom[0x7FFA] = 0x50; // emulation-mode NMI vector ($00:FFFA/FFFB)
    rom[0x7FFB] = 0x80; // -> $80:8050
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    let mut system = SnesSystem::load(&rom).expect("loads");
    assert!(system.cpu.e, "reset leaves the CPU in emulation mode");

    // The vblank flag is already set (a prior vblank whose $4210 read
    // never happened) and NMI is not yet enabled -- no dispatch from
    // this alone, same as always.
    system.bus.timing.nmi_flag = true;
    system.step().expect("NOP is implemented");
    assert_ne!(
        system.cpu.pc, 0x8050,
        "NMI is still disabled -- an already-set flag alone must not dispatch"
    );

    // Now enable NMI while that same flag is still set -- fullsnes's
    // AND-edge rises here too, but this is software's first-ever enable,
    // not a redispatch of an already-armed NMI, and The Terminator/Super
    // Black Bass both prove real hardware does not fire from this alone.
    system.bus.write(0x4200, 0x80);
    assert!(
        system.bus.timing.nmi_flag,
        "the write must not itself have touched the flag"
    );
    system.step().expect("NOP is implemented");
    assert_ne!(
        system.cpu.pc, 0x8050,
        "enabling NMI while $4210 bit 7 is already stale must NOT dispatch \
         immediately -- it must wait for the next real vblank edge, exactly \
         like main (no enable-edge rule at all)"
    );
}

/// The flag's own edge is untouched by any of the above: enabling NMI
/// while the flag is 0, then the flag genuinely rising later, must still
/// dispatch -- this is the one rule `SnesSystem::step` keeps.
#[test]
fn nmi_dispatches_on_the_next_real_vblank_edge_once_enabled() {
    let mut rom = lorom_image(0x20, 0x00);
    rom[0x0000] = 0xEA;
    rom[0x0001] = 0xEA;
    rom[0x7FFA] = 0x50;
    rom[0x7FFB] = 0x80;
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    let mut system = SnesSystem::load(&rom).expect("loads");
    assert!(
        !system.bus.timing.nmi_flag,
        "starts clear, well outside vblank"
    );

    system.bus.write(0x4200, 0x80); // enable NMI; flag is still 0
    system.step().expect("NOP is implemented");
    assert_ne!(
        system.cpu.pc, 0x8050,
        "enabling NMI while the flag is 0 must not dispatch by itself"
    );

    // A genuine flag rise (the vblank edge) with NMI already enabled
    // must still dispatch -- this is the ordinary, pre-W14-47,
    // hundreds-of-titles-verified path, unaffected by any of the above.
    // Driven through `Timing::advance` itself (one master cycle short of
    // the vblank line), not by poking `nmi_flag` directly, so
    // `events.vblank_started` -- what `SnesSystem::step` actually keys
    // its dispatch on -- is set the same way a real vblank sets it.
    system.bus.timing.line = system.bus.timing.vblank_start - 1;
    system.bus.timing.line_cycles = crate::timing::MASTER_PER_LINE - 1;
    system.step().expect("NOP is implemented");
    assert_eq!(
        system.cpu.pc, 0x8050,
        "a real flag edge while NMI is enabled must still dispatch"
    );
}

/// fullsnes "SNES Interrupts": "If one does disable and re-enable NMIs,
/// then an old NMI may be executed again; acknowledging avoids that
/// effect." A THIRD real ROM (Magical Drop II) shows this sentence is
/// also not safe to implement literally: it disables and re-enables bit
/// 7 EVERY single frame as routine practice, with `$4210` unread and set
/// the whole time, and does not want -- and main never produces -- a
/// redispatch on any of those toggles. Since no real title this ticket
/// or its follow-up traced ever needed the disable/re-enable redispatch
/// either, `SnesSystem::step` does not implement it at all; this test
/// pins that absence rather than the redispatch fullsnes's sentence
/// describes.
#[test]
fn disable_then_reenable_does_not_redispatch_while_the_flag_is_still_set() {
    let mut rom = lorom_image(0x20, 0x00);
    for i in 0..4 {
        rom[i] = 0xEA; // four NOPs: one per step below
    }
    rom[0x7FFA] = 0x50;
    rom[0x7FFB] = 0x80;
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    let mut system = SnesSystem::load(&rom).expect("loads");

    // Get NMI genuinely armed once via the ordinary flag edge (driven
    // through `Timing::advance`, exactly like a real boot would reach
    // it -- see the previous test's doc for why poking `nmi_flag`
    // directly is not equivalent), then acknowledge nothing so the flag
    // stays set (the precondition fullsnes's sentence needs).
    system.bus.write(0x4200, 0x80);
    system.bus.timing.line = system.bus.timing.vblank_start - 1;
    system.bus.timing.line_cycles = crate::timing::MASTER_PER_LINE - 1;
    system.step().expect("NOP is implemented");
    assert_eq!(system.cpu.pc, 0x8050, "the ordinary flag-edge dispatch");
    system.cpu.pc = 0x8001;
    assert!(
        system.bus.timing.nmi_flag,
        "the flag is only cleared by a $4210 read or vblank end -- neither happened"
    );

    system.bus.write(0x4200, 0x00); // disable
    system.step().expect("NOP is implemented");
    assert_ne!(system.cpu.pc, 0x8050, "disabling must not itself dispatch");

    system.cpu.pc = 0x8003;
    system.bus.write(0x4200, 0x80); // re-enable while the flag is STILL set
    system.step().expect("NOP is implemented");
    assert_ne!(
        system.cpu.pc, 0x8050,
        "re-enabling while the old flag is still unread must NOT redispatch \
         -- Magical Drop II does this every frame and main never redispatches \
         from it"
    );
}

/// Ticket W19-02: an end-to-end check that a Cx4 cartridge's header wires
/// up the real register window through `SnesSystem::load` — the SNES CPU
/// side can write the DMA ports, trigger a transfer, and read the result
/// back out of CX4RAM through the ordinary bus, exactly as `Coprocessor::
/// Cx4`'s doc says a real title's boot code would poke the chip.
#[test]
fn cx4_cart_dma_transfer_is_visible_through_the_bus() {
    let mut rom = lorom_image(0x20, 0xF3); // fullsnes "[FFD6]=F3h" Cx4.
    rom[0x7FC0 - 1] = 0x10; // fullsnes "[FFBF]=10h ;CustomChip=CX4".
                            // Four bytes of payload the DMA will copy, placed at LoROM $01:8000
                            // (file offset 0x8000, since bank 1 offset 0x8000 -> index 1*0x8000).
    rom.resize(0x1_0000, 0);
    rom[0x8000] = 0xCA;
    rom[0x8001] = 0xFE;
    rom[0x8002] = 0xBA;
    rom[0x8003] = 0xBE;

    let cart = rf_cart::Cartridge::load(&rom).expect("Cx4 cart accepted");
    let header = match &cart {
        rf_cart::Cartridge::Snes { header, .. } => header,
        other => panic!("expected an SNES cartridge, got {other:?}"),
    };
    assert_eq!(header.coprocessor, rf_cart::Coprocessor::Cx4);

    let mut system = SnesSystem::load(&rom).expect("Cx4 cart loads");
    assert!(system.bus.cx4.is_some());

    // Program the documented DMA ports (fullsnes "CX4 I/O Map") through
    // the ordinary SNES-side bus write path, bank $00.
    system.bus.write(0x00_7F40, 0x00); // source lsb
    system.bus.write(0x00_7F41, 0x80); // source mid
    system.bus.write(0x00_7F42, 0x01); // source msb -> $01:8000
    system.bus.write(0x00_7F43, 0x04); // length lsb = 4
    system.bus.write(0x00_7F44, 0x00); // length msb
    system.bus.write(0x00_7F45, 0x10); // dest lsb -> CX4RAM+$0010
    system.bus.write(0x00_7F46, 0x00); // dest msb
    system.bus.write(0x00_7F47, 0x00); // start, SNES-to-CX4 direction

    // The transfer landed in CX4RAM, visible at $6010 in bank $00.
    assert_eq!(system.bus.read(0x00_6010), 0xCA);
    assert_eq!(system.bus.read(0x00_6011), 0xFE);
    assert_eq!(system.bus.read(0x00_6012), 0xBA);
    assert_eq!(system.bus.read(0x00_6013), 0xBE);

    // A general register round-trips through the bus too (R2 at
    // $7F80+2*3=$7F86).
    system.bus.write(0x00_7F86, 0x11);
    system.bus.write(0x00_7F87, 0x22);
    system.bus.write(0x00_7F88, 0x33);
    assert_eq!(system.bus.cx4.as_ref().unwrap().regs[2], 0x0033_2211);
}
