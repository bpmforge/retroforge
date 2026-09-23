//! Address-mapping tests (FR-CORE-035).

use rf_cart::{
    Sa1Board,
    SnesMapMode::{HiRom, LoRom},
};

use crate::mapping::{
    gsu_target, map, obc1_target, sa1_target, st010_target, GsuBoard, Obc1Board, Sa1RomBanks,
    Target, WRAM_LEN,
};

const ROM_32K: usize = 32 * 1024;
const ROM_1M: usize = 1024 * 1024;

fn rom_at(mode: rf_cart::SnesMapMode, bank: u8, offset: u16, rom_len: usize) -> usize {
    match map(mode, bank, offset, rom_len, 0) {
        Target::Rom(i) => i,
        other => panic!("expected ROM at {bank:02X}:{offset:04X}, got {other:?}"),
    }
}

/// The two aliases the mirror-map fixture names — asserted directly.
///
/// These are the exact pairs FORMAT.md documents, and the LoROM one is
/// the check that was silently vacuous in the ROM itself until W6-02a.
#[test]
fn the_documented_bank_aliases_resolve_to_the_same_rom_byte() {
    // LoROM: $00:8000 is $80:8000.
    assert_eq!(
        rom_at(LoRom, 0x00, 0x8000, ROM_32K),
        rom_at(LoRom, 0x80, 0x8000, ROM_32K),
        "LoROM must mirror ROM into the low banks"
    );
    // HiROM: $40:8000 is $C0:8000.
    assert_eq!(
        rom_at(HiRom, 0x40, 0x8000, ROM_1M),
        rom_at(HiRom, 0xC0, 0x8000, ROM_1M),
        "HiROM must mirror ROM into the low banks"
    );
}

/// The alias test above would pass if EVERY address mapped to ROM byte 0.
/// This makes the mapping prove it distinguishes addresses at all.
#[test]
fn different_banks_reach_different_rom_bytes() {
    assert_eq!(rom_at(LoRom, 0x00, 0x8000, ROM_1M), 0x0_0000);
    assert_eq!(rom_at(LoRom, 0x01, 0x8000, ROM_1M), 0x0_8000);
    assert_eq!(rom_at(LoRom, 0x02, 0x8000, ROM_1M), 0x1_0000);
    assert_eq!(rom_at(LoRom, 0x00, 0xFFFF, ROM_1M), 0x0_7FFF);

    assert_eq!(rom_at(HiRom, 0xC0, 0x0000, ROM_1M), 0x0_0000);
    assert_eq!(rom_at(HiRom, 0xC0, 0xFFFF, ROM_1M), 0x0_FFFF);
    assert_eq!(rom_at(HiRom, 0xC1, 0x0000, ROM_1M), 0x1_0000);
}

/// An undersized ROM repeats rather than reading nothing — and the reset
/// vector of the 32 KiB fixture depends on it.
#[test]
fn undersized_roms_mirror_and_the_reset_vector_resolves() {
    // Bank $01 is past the end of a 32 KiB LoROM; it wraps to the start.
    assert_eq!(rom_at(LoRom, 0x01, 0x8000, ROM_32K), 0);
    // The reset vector at $00:FFFC lands at file offset $7FFC, which is
    // where a 32 KiB LoROM keeps it.
    assert_eq!(rom_at(LoRom, 0x00, 0xFFFC, ROM_32K), 0x7FFC);
    // HiROM keeps its vectors at $FFFC of the first 64 KiB.
    assert_eq!(rom_at(HiRom, 0x00, 0xFFFC, 64 * 1024), 0xFFFC);
}

#[test]
fn wram_banks_cover_the_full_128k_and_mirror_into_system_banks() {
    assert_eq!(map(LoRom, 0x7E, 0x0000, ROM_1M, 0), Target::Wram(0));
    assert_eq!(map(LoRom, 0x7E, 0xFFFF, ROM_1M, 0), Target::Wram(0xFFFF));
    assert_eq!(map(LoRom, 0x7F, 0x0000, ROM_1M, 0), Target::Wram(0x1_0000));
    assert_eq!(
        map(LoRom, 0x7F, 0xFFFF, ROM_1M, 0),
        Target::Wram(WRAM_LEN - 1),
        "banks $7E-$7F must cover WRAM exactly, with no gap and no overrun"
    );

    // The low 8 KiB is visible from every system bank — the alias the
    // fixture's check 2 exercises.
    for bank in [0x00u8, 0x01, 0x3F, 0x80, 0xBF] {
        assert_eq!(
            map(LoRom, bank, 0x1234, ROM_1M, 0),
            Target::Wram(0x1234),
            "bank {bank:02X} must mirror WRAM's low 8 KiB"
        );
    }
    // ...but only the low 8 KiB.
    assert_ne!(
        map(LoRom, 0x00, 0x2000, ROM_1M, 0),
        Target::Wram(0x2000),
        "$2000 is registers, not WRAM"
    );
}

#[test]
fn the_register_window_is_2000_to_5fff_in_system_banks_only() {
    for offset in [0x2000u16, 0x2100, 0x4016, 0x4200, 0x420B, 0x5FFF] {
        assert_eq!(
            map(LoRom, 0x00, offset, ROM_1M, 0),
            Target::Register(offset)
        );
        assert_eq!(
            map(HiRom, 0x80, offset, ROM_1M, 0),
            Target::Register(offset)
        );
    }
    // Banks $40-$7D are cartridge, not registers — a mapping that put a
    // register window there would shadow ROM.
    assert!(matches!(
        map(HiRom, 0x40, 0x2100, ROM_1M, 0),
        Target::Rom(_)
    ));
}

#[test]
fn save_ram_lands_where_each_map_puts_it_and_is_open_when_absent() {
    // HiROM: $20-$3F:$6000-$7FFF.
    assert!(matches!(
        map(HiRom, 0x20, 0x6000, ROM_1M, 8192),
        Target::Sram(_)
    ));
    // LoROM: banks $70-$7D below $8000.
    assert!(matches!(
        map(LoRom, 0x70, 0x0000, ROM_1M, 8192),
        Target::Sram(_)
    ));
    // With no save RAM, those windows must not alias onto something else.
    assert_eq!(map(HiRom, 0x20, 0x6000, ROM_1M, 0), Target::Open);
    assert_eq!(map(LoRom, 0x00, 0x6000, ROM_1M, 0), Target::Open);
}

/// WRAM banks must win over the cartridge range they sit inside.
///
/// `$7E`/`$7F` fall within `$40-$7D`-adjacent territory, and an ordering
/// mistake would map work RAM to ROM — which boots, runs, and corrupts
/// everything quietly.
#[test]
fn wram_banks_are_not_captured_by_the_cartridge_range() {
    for bank in [0x7Eu8, 0x7F] {
        for offset in [0x0000u16, 0x7FFF, 0x8000, 0xFFFF] {
            assert!(
                matches!(map(LoRom, bank, offset, ROM_1M, 8192), Target::Wram(_)),
                "LoROM {bank:02X}:{offset:04X} must be WRAM"
            );
            assert!(
                matches!(map(HiRom, bank, offset, ROM_1M, 8192), Target::Wram(_)),
                "HiROM {bank:02X}:{offset:04X} must be WRAM"
            );
        }
    }
}

/// Every address in the whole space maps to something, in both modes,
/// without panicking and without an out-of-range index.
#[test]
fn every_address_maps_within_bounds() {
    for mode in [LoRom, HiRom] {
        for bank in 0..=0xFFu32 {
            // Sampling offsets rather than all 65536 keeps this a unit
            // test; the boundaries that matter are covered exactly above.
            for offset in (0..=0xFFFFu32).step_by(0x40) {
                let addr_bank = bank as u8;
                let off = offset as u16;
                match map(mode, addr_bank, off, ROM_32K, 8192) {
                    Target::Rom(i) => assert!(i < ROM_32K, "{addr_bank:02X}:{off:04X}"),
                    Target::Wram(i) => assert!(i < WRAM_LEN, "{addr_bank:02X}:{off:04X}"),
                    Target::Sram(i) => assert!(i < 8192, "{addr_bank:02X}:{off:04X}"),
                    // `map` itself never returns these (tickets W14-19,
                    // W17-01): only `dsp1_target`/`sa1_target`, checked
                    // separately by `bus.rs` before `map` runs, do.
                    Target::Register(_)
                    | Target::Dsp1Dr
                    | Target::Dsp1Sr
                    | Target::Sa1IRam(_)
                    | Target::Sa1BwRam(_)
                    | Target::Sa1Register(_)
                    | Target::Sa1Bitmap(_)
                    | Target::GsuRam(_)
                    | Target::GsuRegister(_)
                    | Target::Cx4Ram(_)
                    | Target::Cx4Register(_)
                    | Target::Obc1Register(_)
                    | Target::Obc1Bits(_)
                    | Target::St010Ram(_)
                    | Target::St010Register(_)
                    | Target::Open => {}
                }
            }
        }
    }
}

/// Ticket W17-01: `sa1_target` (fullsnes "SNES Cart SA-1", memory-map +
/// "Memory Control" sections).
mod sa1 {
    use super::*;

    fn regs(rom_len: usize, bwram_len: usize) -> Sa1RomBanks {
        Sa1RomBanks {
            cxb: 0x00,
            dxb: 0x01,
            exb: 0x02,
            fxb: 0x03,
            bmaps: 0,
            bmap: 0,
            board: Sa1Board {
                rom_len,
                bwram_len,
                iram_len: 2048,
            },
        }
    }

    #[test]
    fn register_window_is_2200_to_23ff() {
        let r = regs(ROM_1M, 8192);
        assert_eq!(
            sa1_target(&r, 0x00, 0x2200),
            Some(Target::Sa1Register(0x2200))
        );
        assert_eq!(
            sa1_target(&r, 0x80, 0x23FF),
            Some(Target::Sa1Register(0x23FF))
        );
        assert_eq!(sa1_target(&r, 0x00, 0x21FF), None, "PPU/APU regs, not SA-1");
    }

    #[test]
    fn iram_is_3000_to_37ff_direct() {
        let r = regs(ROM_1M, 8192);
        assert_eq!(sa1_target(&r, 0x00, 0x3000), Some(Target::Sa1IRam(0)));
        assert_eq!(sa1_target(&r, 0x3F, 0x37FF), Some(Target::Sa1IRam(0x7FF)));
        assert_eq!(sa1_target(&r, 0x00, 0x3800), None, "past I-RAM's 2 KiB");
    }

    #[test]
    fn bwram_window_is_6000_to_7fff_selected_by_bmaps() {
        let mut r = regs(ROM_1M, 256 * 1024);
        r.bmaps = 2; // block 2 -> byte offset 0x4000
        assert_eq!(sa1_target(&r, 0x00, 0x6000), Some(Target::Sa1BwRam(0x4000)));
        assert_eq!(sa1_target(&r, 0x00, 0x7FFF), Some(Target::Sa1BwRam(0x5FFF)));
        // With no BW-RAM at all, the window is not claimed here (falls
        // through to open bus via `map`).
        let none = regs(ROM_1M, 0);
        assert_eq!(sa1_target(&none, 0x00, 0x6000), None);
    }

    #[test]
    fn full_bwram_is_40_to_4f_and_mirrors_every_4_banks() {
        let r = regs(ROM_1M, 256 * 1024);
        assert_eq!(sa1_target(&r, 0x40, 0x0000), Some(Target::Sa1BwRam(0)));
        assert_eq!(
            sa1_target(&r, 0x43, 0xFFFF),
            Some(Target::Sa1BwRam(256 * 1024 - 1))
        );
        // $44 mirrors $40.
        assert_eq!(
            sa1_target(&r, 0x44, 0x1234),
            sa1_target(&r, 0x40, 0x1234),
            "fullsnes: BW-RAM mirrors in 44h-4Fh"
        );
    }

    #[test]
    fn hirom_style_c0_to_ff_uses_the_same_four_registers() {
        let r = regs(8 * 1024 * 1024, 0);
        // CXB=0 selects 1 MiB bank 0 at $C0-$CF.
        assert_eq!(sa1_target(&r, 0xC0, 0x0000), Some(Target::Rom(0)));
        assert_eq!(sa1_target(&r, 0xC1, 0x0000), Some(Target::Rom(0x1_0000)));
        // DXB=1 selects bank 1 (1 MiB in) at $D0-$DF.
        assert_eq!(sa1_target(&r, 0xD0, 0x0000), Some(Target::Rom(0x10_0000)));
        // FXB=3 selects bank 3 (3 MiB in) at $F0-$FF.
        assert_eq!(sa1_target(&r, 0xF0, 0x0000), Some(Target::Rom(0x30_0000)));
    }

    #[test]
    fn lorom_style_direct_mode_splits_first_and_second_2mib() {
        // bit7=0 on every register: direct mode everywhere.
        let mut r = regs(4 * 1024 * 1024, 0);
        r.cxb = 0x00;
        r.dxb = 0x00;
        r.exb = 0x00;
        r.fxb = 0x00;
        assert_eq!(sa1_target(&r, 0x00, 0x8000), Some(Target::Rom(0)));
        assert_eq!(sa1_target(&r, 0x3F, 0x8000), Some(Target::Rom(0x1F_8000)));
        // Second 2 MiB starts at bank $80.
        assert_eq!(sa1_target(&r, 0x80, 0x8000), Some(Target::Rom(0x20_0000)));
        assert_eq!(sa1_target(&r, 0xBF, 0x8000), Some(Target::Rom(0x3F_8000)));
    }

    #[test]
    fn lorom_style_banked_mode_shows_the_selected_1mib_bank() {
        // bit7=1: CXB selects 1 MiB bank 5 for the $00-$1F quarter.
        let mut r = regs(8 * 1024 * 1024, 0);
        r.cxb = 0x80 | 0x05;
        assert_eq!(sa1_target(&r, 0x00, 0x8000), Some(Target::Rom(0x50_0000)));
        assert_eq!(sa1_target(&r, 0x1F, 0x8000), Some(Target::Rom(0x5F_8000)));
    }

    #[test]
    fn non_sa1_addresses_are_untouched_when_bwram_and_rom_are_absent() {
        let r = regs(0, 0);
        assert_eq!(sa1_target(&r, 0x00, 0x8000), None);
        assert_eq!(sa1_target(&r, 0xC0, 0x0000), None);
    }
}

/// Ticket W18-01: `gsu_target` (fullsnes "SNES Cart GSU-n Memory Map" /
/// "...I/O Map", the GSU2 table this project uses uniformly).
mod gsu {
    use super::*;

    fn board(rom_len: usize, ram_len: usize) -> GsuBoard {
        GsuBoard {
            rom_len,
            ram_len,
            ron: false,
            ran: false,
        }
    }

    #[test]
    fn register_window_is_3000_to_3fff_in_low_and_mirror_banks() {
        let b = board(ROM_1M, 0);
        assert_eq!(
            gsu_target(&b, 0x00, 0x3000),
            Some(Target::GsuRegister(0x3000))
        );
        assert_eq!(
            gsu_target(&b, 0x80, 0x3FFF),
            Some(Target::GsuRegister(0x3FFF))
        );
        assert_eq!(gsu_target(&b, 0x00, 0x2FFF), None, "PPU/APU regs, not GSU");
        assert_eq!(
            gsu_target(&b, 0x00, 0x4000),
            None,
            "past the register window"
        );
    }

    #[test]
    fn ram_mirror_is_6000_to_7fff() {
        let b = board(ROM_1M, 32 * 1024);
        assert_eq!(gsu_target(&b, 0x00, 0x6000), Some(Target::GsuRam(0)));
        assert_eq!(gsu_target(&b, 0x3F, 0x7FFF), Some(Target::GsuRam(0x1FFF)));
        // With no RAM at all, the window is not claimed here.
        let none = board(ROM_1M, 0);
        assert_eq!(gsu_target(&none, 0x00, 0x6000), None);
    }

    #[test]
    fn rom_is_8000_to_ffff_lorom_style_in_banks_00_to_3f_only() {
        let b = board(2 * 1024 * 1024, 0);
        assert_eq!(gsu_target(&b, 0x00, 0x8000), Some(Target::Rom(0)));
        assert_eq!(gsu_target(&b, 0x3F, 0x8000), Some(Target::Rom(0x1F_8000)));
        // Banks $80-$BF are fullsnes's separate, unpopulated "Additional
        // CPU ROM" chip select — `gsu_target` reports `None` for it here
        // and leaves the ordinary LoROM mirror (via the generic `map`) to
        // supply the answer, see `gsu_target`'s doc.
        assert_eq!(gsu_target(&b, 0x80, 0x8000), None);
        assert_eq!(gsu_target(&b, 0xBF, 0x8000), None);
    }

    #[test]
    fn rom_mirrors_undersized_images() {
        // A 512 KiB ROM (GSU1-sized) still resolves every bank via modulo.
        let b = board(512 * 1024, 0);
        assert_eq!(gsu_target(&b, 0x00, 0x8000), Some(Target::Rom(0)));
        assert_eq!(gsu_target(&b, 0x10, 0x8000), Some(Target::Rom(0)));
    }

    #[test]
    fn hirom_style_40_to_5f_mirrors_the_same_rom_linearly() {
        let b = board(2 * 1024 * 1024, 0);
        assert_eq!(gsu_target(&b, 0x40, 0x0000), Some(Target::Rom(0)));
        assert_eq!(gsu_target(&b, 0x41, 0x0000), Some(Target::Rom(0x1_0000)));
        assert_eq!(
            gsu_target(&b, 0x5F, 0xFFFF),
            Some(Target::Rom(2 * 1024 * 1024 - 1))
        );
    }

    #[test]
    fn full_ram_is_70_to_71() {
        let b = board(ROM_1M, 128 * 1024);
        assert_eq!(gsu_target(&b, 0x70, 0x0000), Some(Target::GsuRam(0)));
        assert_eq!(
            gsu_target(&b, 0x71, 0xFFFF),
            Some(Target::GsuRam(128 * 1024 - 1))
        );
    }

    #[test]
    fn additional_backup_ram_and_cpu_rom_banks_are_untouched() {
        // Fullsnes's "not installed in existing cartridges" regions:
        // banks $78-$79, $80-$BF:$8000-FFFF (as additional CPU ROM, not
        // the LoROM mirror this test's other cases exercise), $C0-$FF.
        let b = board(ROM_1M, 0);
        assert_eq!(gsu_target(&b, 0x78, 0x0000), None);
        assert_eq!(gsu_target(&b, 0xC0, 0x0000), None);
    }

    #[test]
    fn non_gsu_addresses_are_untouched_when_rom_and_ram_are_absent() {
        let b = board(0, 0);
        assert_eq!(gsu_target(&b, 0x00, 0x8000), None);
        assert_eq!(gsu_target(&b, 0x70, 0x0000), None);
    }
}

/// Ticket W19-01: `obc1_target` (fullsnes "SNES Cart OBC1 (OBJ
/// Controller)" and its "OBC1 I/O Ports" table).
mod obc1 {
    use super::*;

    fn board(base_offset: usize, index: u8, sram_len: usize) -> Obc1Board {
        Obc1Board {
            base_offset,
            index,
            sram_len,
        }
    }

    #[test]
    fn base_7c00_is_sram_offset_0x1c00_base_7800_is_0x1800() {
        // fullsnes: "$7FF5h Base for 220h-byte region (bit0: 0=7C00h,
        // 1=7800h)" — the two named workspace ranges
        // (7800h-7A1Fh/7C00h-7E1Fh) are relative to bank $6000, so as an
        // SRAM byte offset that's $1800/$1C00.
        assert_eq!(0x7C00 - 0x6000, 0x1C00);
        assert_eq!(0x7800 - 0x6000, 0x1800);
    }

    #[test]
    fn registers_7ff5_7ff6_7ff7_are_obc1_register() {
        let b = board(0x1C00, 0, 8192);
        assert_eq!(
            obc1_target(&b, 0x00, 0x7FF5),
            Some(Target::Obc1Register(0x7FF5))
        );
        assert_eq!(
            obc1_target(&b, 0x00, 0x7FF6),
            Some(Target::Obc1Register(0x7FF6))
        );
        assert_eq!(
            obc1_target(&b, 0x00, 0x7FF7),
            Some(Target::Obc1Register(0x7FF7))
        );
    }

    #[test]
    fn ports_7ff0_7ff3_redirect_to_base_plus_index_times_4() {
        // fullsnes: "7FF0h OAM Xloc = [Base+Index*4+0]" through
        // "7FF3h OAM Attr = [Base+Index*4+3]".
        let b = board(0x1C00, 5, 8192);
        assert_eq!(
            obc1_target(&b, 0x00, 0x7FF0),
            Some(Target::Sram(0x1C00 + 5 * 4))
        );
        assert_eq!(
            obc1_target(&b, 0x00, 0x7FF1),
            Some(Target::Sram(0x1C00 + 5 * 4 + 1))
        );
        assert_eq!(
            obc1_target(&b, 0x00, 0x7FF2),
            Some(Target::Sram(0x1C00 + 5 * 4 + 2))
        );
        assert_eq!(
            obc1_target(&b, 0x00, 0x7FF3),
            Some(Target::Sram(0x1C00 + 5 * 4 + 3))
        );
    }

    #[test]
    fn port_7ff4_redirects_to_base_plus_index_div_4_plus_0x200_as_bits() {
        // fullsnes: "7FF4h OAM Bits = [Base+Index/4+200h]...".
        let b = board(0x1800, 127, 8192);
        assert_eq!(
            obc1_target(&b, 0x00, 0x7FF4),
            Some(Target::Obc1Bits(0x1800 + 127 / 4 + 0x200))
        );
    }

    #[test]
    fn the_220h_table_spans_exactly_the_documented_workspace() {
        // index 0..127: attributes span base+0..base+0x1FF (512 bytes),
        // bits span base+0x200..base+0x21F (32 bytes) — 0x220 total,
        // fullsnes's own "220h-byte region" naming.
        let b = board(0x1C00, 127, 8192);
        assert_eq!(
            obc1_target(&b, 0x00, 0x7FF3),
            Some(Target::Sram(0x1C00 + 127 * 4 + 3))
        );
        assert_eq!(0x1C00 + 127 * 4 + 3, 0x1C00 + 0x1FF);
        assert_eq!(
            obc1_target(&b, 0x00, 0x7FF4),
            Some(Target::Obc1Bits(0x1C00 + 127 / 4 + 0x200))
        );
        assert_eq!(0x1C00 + 127 / 4 + 0x200, 0x1C00 + 0x21F);
    }

    #[test]
    fn every_other_window_byte_is_plain_sram() {
        let b = board(0x1C00, 0, 8192);
        assert_eq!(obc1_target(&b, 0x00, 0x6000), Some(Target::Sram(0)));
        assert_eq!(obc1_target(&b, 0x00, 0x7FEF), Some(Target::Sram(0x1FEF)));
        assert_eq!(obc1_target(&b, 0x00, 0x7FFF), Some(Target::Sram(0x1FFF)));
    }

    #[test]
    fn window_is_visible_in_every_system_area_bank() {
        let b = board(0x1C00, 0, 8192);
        for bank in [0x00u8, 0x3F, 0x80, 0xBF] {
            assert_eq!(
                obc1_target(&b, bank, 0x7FF6),
                Some(Target::Obc1Register(0x7FF6)),
                "bank {bank:02X}"
            );
        }
    }

    #[test]
    fn outside_the_window_or_the_system_area_falls_through_to_map() {
        let b = board(0x1C00, 0, 8192);
        // Below $6000 in a system-area bank: not this chip's window.
        assert_eq!(obc1_target(&b, 0x00, 0x1000), None);
        // Above $7FFF: ROM territory.
        assert_eq!(obc1_target(&b, 0x00, 0x8000), None);
        // Banks $40-$7F/$C0-$FF are not the system area this chip claims.
        assert_eq!(obc1_target(&b, 0x40, 0x7FF6), None);
        assert_eq!(obc1_target(&b, 0x70, 0x7FF6), None);
        assert_eq!(obc1_target(&b, 0xC0, 0x7FF6), None);
    }

    #[test]
    fn zero_length_sram_reports_open_across_the_whole_window() {
        let b = board(0x1C00, 0, 0);
        assert_eq!(obc1_target(&b, 0x00, 0x7FF6), Some(Target::Open));
        assert_eq!(obc1_target(&b, 0x00, 0x6000), Some(Target::Open));
    }
}

/// Ticket W19-04: `st010_target` (fullsnes "SNES Cart DSP-n/ST010/ST011").
mod st010_window {
    use super::{st010_target, Target};

    #[test]
    fn ram_window_covers_the_whole_4kib_range_in_banks_68_through_6f() {
        for bank in 0x68u8..=0x6F {
            assert_eq!(
                st010_target(bank, 0x0000),
                Some(Target::St010Ram(0)),
                "bank {bank:02X}"
            );
            assert_eq!(
                st010_target(bank, 0x0FFF),
                Some(Target::St010Ram(0x0FFF)),
                "bank {bank:02X}"
            );
        }
    }

    #[test]
    fn ram_window_mirrors_the_same_offsets_across_every_claimed_bank() {
        // Every bank in $68-$6F is the SAME 4096-byte RAM, not eight
        // different slices — see the function's own doc.
        assert_eq!(st010_target(0x68, 0x0020), st010_target(0x6F, 0x0020));
    }

    #[test]
    fn ram_window_is_not_mapped_above_offset_0fff() {
        assert_eq!(st010_target(0x68, 0x1000), None);
        assert_eq!(st010_target(0x6F, 0xFFFF), None);
    }

    #[test]
    fn command_and_busy_bytes_resolve_inside_the_ram_window() {
        assert_eq!(st010_target(0x68, 0x0020), Some(Target::St010Ram(0x0020)));
        assert_eq!(st010_target(0x68, 0x0021), Some(Target::St010Ram(0x0021)));
    }

    #[test]
    fn inert_dr_sr_pair_is_mapped_in_banks_60_through_67() {
        for bank in 0x60u8..=0x67 {
            assert_eq!(
                st010_target(bank, 0x0000),
                Some(Target::St010Register(0)),
                "bank {bank:02X}"
            );
            assert_eq!(
                st010_target(bank, 0x0001),
                Some(Target::St010Register(1)),
                "bank {bank:02X}"
            );
            assert_eq!(st010_target(bank, 0x0002), None, "bank {bank:02X}");
        }
    }

    #[test]
    fn every_bank_in_00_to_7f_mirrors_to_80_to_ff() {
        // Fullsnes SNES I/O Ports section: "All banks in range 00-7F are
        // also mirrored to 80-FF."
        assert_eq!(st010_target(0x68, 0x0010), st010_target(0xE8, 0x0010));
        assert_eq!(st010_target(0x60, 0x0000), st010_target(0xE0, 0x0000));
    }

    #[test]
    fn banks_outside_60_through_6f_are_unclaimed() {
        assert_eq!(st010_target(0x00, 0x0000), None);
        assert_eq!(st010_target(0x40, 0x0020), None);
        assert_eq!(st010_target(0x70, 0x0000), None);
    }
}
