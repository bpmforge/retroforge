//! RetroAchievements flat address spaces mapped onto CPU-bus addresses.
//!
//! RA does not use CPU addresses: each console has its own flat map, defined
//! by `rc_console_memory_regions` in rcheevos (MIT, readable under D-033).
//! Sources, from `vendor/rcheevos/src/rcheevos/consoleinfo.c`:
//!
//! * NES, lines 711-726 (`_rc_memory_regions_nes`): the map IS the CPU map,
//!   `$0000-$FFFF`, with `$0800-$1FFF` declared as mirrors of `$0000-$07FF`
//!   and `$2000-$3FFF` as PPU-register mirrors. nesdev "CPU memory map".
//! * SNES, lines 901-915 (`_rc_memory_regions_snes`): RA `$000000-$01FFFF`
//!   is WRAM = CPU `$7E0000-$7FFFFF`; RA `$020000-$09FFFF` is cartridge RAM
//!   and `$0A0000-$0A07FF` is SA-1 I-RAM, both placed by RA "outside of the
//!   possible native addressing space" (real addresses `$1000000` and
//!   `$1080000`).
//!
//! rcheevos validates every address a set uses by reading it once after load
//! and disables achievements whose address reads back 0 bytes
//! (`rc_client_validate_addresses`), so [`translate`] returning `None` is a
//! real signal ("this core cannot serve that address"), not a default.

/// Consoles RetroForge ships; the discriminant is RA's `RC_CONSOLE_*` id
/// (`include/rc_consoles.h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Console {
    /// `RC_CONSOLE_NINTENDO` = 7.
    Nes,
    /// `RC_CONSOLE_SUPER_NINTENDO` = 3.
    Snes,
}

impl Console {
    /// RA's numeric console id.
    pub fn ra_id(self) -> u32 {
        match self {
            Console::Nes => crate::ffi::RC_CONSOLE_NINTENDO,
            Console::Snes => crate::ffi::RC_CONSOLE_SUPER_NINTENDO,
        }
    }
}

/// Where a SNES cartridge's own RAM sits on the CPU bus, which RA's flat
/// cartridge-RAM region (`$020000-$09FFFF`) cannot say. The app builds it
/// from the parsed cartridge header (`ram` is the header's RAM size in
/// bytes); this crate stays free of cartridge types.
///
/// RA's cartridge RAM is the cartridge's save RAM as one linear array
/// (libretro `RETRO_MEMORY_SAVE_RAM`, consoleinfo.c lines 901-915), so RA
/// offset `o = ra - $020000` is byte `o` of that array, and the map below
/// is the CPU address the cartridge decodes byte `o` at:
///
/// * LoROM (and S-DD1, whose base map is plain LoROM): SRAM is banks
///   `$70-$7D`, `$0000-$7FFF` each, 32 KiB per bank. fullsnes "SNES Memory
///   Map" / "SNES Cart LoROM Mapping": `70h-7Dh:0000h-7FFFh`.
/// * HiROM: SRAM is `$6000-$7FFF` of banks `$20-$3F` (and `$A0-$BF`), 8 KiB
///   per bank. fullsnes "SNES Memory Map" / "SNES Cart HiROM Mapping":
///   `20h-3Fh:6000h-7FFFh`.
/// * SA-1: BW-RAM (the RA cartridge RAM) is the `$40-$43` full-bank window,
///   64 KiB per bank (fullsnes "SNES Cart SA-1" memory map: "Entire 256K
///   BW-RAM at 40h-4Fh"); I-RAM (RA `$0A0000-$0A07FF`) is `$00:3000-$37FF`.
///
/// Each arm is the inverse of `rf_snes::mapping::map` / `sa1_target`.
/// Coprocessor carts whose RAM is not the save RAM (Super FX, CX4, OBC1)
/// stay [`CartMap::None`]: their RAM windows are not these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CartMap {
    /// Cartridge RAM cannot be served (no RAM, or a layout not modelled).
    None,
    LoRom {
        ram: u32,
    },
    HiRom {
        ram: u32,
    },
    Sa1 {
        bwram: u32,
    },
}

/// RA's SNES cartridge-RAM window start and the SA-1 I-RAM window.
const SNES_CART_RAM: u32 = 0x02_0000;
const SNES_CART_RAM_END: u32 = 0x09_FFFF;
const SNES_IRAM: u32 = 0x0A_0000;
const SNES_IRAM_LEN: u32 = 0x800;

/// Translate an RA address with no cartridge-RAM layout known: cartridge RAM
/// reads as unmapped. See [`translate_with`].
pub fn translate(console: Console, ra_addr: u32) -> Option<u32> {
    translate_with(console, ra_addr, CartMap::None)
}

/// Translate an RA address to the CPU-bus address to `peek`, or `None` when
/// the address is outside the map or this crate cannot serve it.
///
/// SNES cartridge RAM (`$020000+`) needs the cartridge's [`CartMap`]; with
/// [`CartMap::None`], or past the RAM's real size, it is `None` and rcheevos
/// disables the achievements that use it instead of reading garbage.
pub fn translate_with(console: Console, ra_addr: u32, cart: CartMap) -> Option<u32> {
    match console {
        Console::Nes => match ra_addr {
            // Work RAM and its mirrors fold onto the 2 KiB the CPU decodes.
            0x0000..=0x1FFF => Some(ra_addr & 0x07FF),
            0x2000..=0xFFFF => Some(ra_addr),
            _ => None,
        },
        Console::Snes => match ra_addr {
            0x0000..=0x01_FFFF => Some(0x7E_0000 + ra_addr),
            SNES_CART_RAM..=SNES_CART_RAM_END => {
                let o = ra_addr - SNES_CART_RAM;
                match cart {
                    CartMap::LoRom { ram } if o < ram && (o >> 15) < 14 => {
                        Some(((0x70 + (o >> 15)) << 16) | (o & 0x7FFF))
                    }
                    CartMap::HiRom { ram } if o < ram && (o >> 13) < 0x20 => {
                        Some(((0x20 + (o >> 13)) << 16) | (0x6000 + (o & 0x1FFF)))
                    }
                    CartMap::Sa1 { bwram } if o < bwram && (o >> 16) < 4 => {
                        Some(((0x40 + (o >> 16)) << 16) | (o & 0xFFFF))
                    }
                    _ => None,
                }
            }
            SNES_IRAM.. if ra_addr - SNES_IRAM < SNES_IRAM_LEN => match cart {
                CartMap::Sa1 { .. } => Some(0x3000 + (ra_addr - SNES_IRAM)),
                _ => None,
            },
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nes_map() {
        assert_eq!(translate(Console::Nes, 0x10), Some(0x10));
        assert_eq!(translate(Console::Nes, 0x0810), Some(0x10));
        assert_eq!(translate(Console::Nes, 0x6000), Some(0x6000));
        assert_eq!(translate(Console::Nes, 0xFFFF), Some(0xFFFF));
        assert_eq!(translate(Console::Nes, 0x1_0000), None);
    }

    #[test]
    fn snes_map() {
        assert_eq!(translate(Console::Snes, 0), Some(0x7E_0000));
        assert_eq!(translate(Console::Snes, 0x1_FFFF), Some(0x7F_FFFF));
        assert_eq!(translate(Console::Snes, 0x2_0000), None);
    }

    #[test]
    fn snes_lorom_sram() {
        let c = CartMap::LoRom { ram: 0x8000 };
        assert_eq!(translate_with(Console::Snes, 0x2_0000, c), Some(0x70_0000));
        assert_eq!(translate_with(Console::Snes, 0x2_7FFF, c), Some(0x70_7FFF));
        assert_eq!(translate_with(Console::Snes, 0x2_8000, c), None, "past RAM");
        let big = CartMap::LoRom { ram: 0x2_0000 };
        assert_eq!(
            translate_with(Console::Snes, 0x2_8000, big),
            Some(0x71_0000)
        );
        assert_eq!(translate_with(Console::Snes, 0x2_0010, CartMap::None), None);
    }

    #[test]
    fn snes_hirom_sram() {
        let c = CartMap::HiRom { ram: 0x4000 };
        assert_eq!(translate_with(Console::Snes, 0x2_0000, c), Some(0x20_6000));
        assert_eq!(translate_with(Console::Snes, 0x2_1FFF, c), Some(0x20_7FFF));
        assert_eq!(translate_with(Console::Snes, 0x2_2000, c), Some(0x21_6000));
        assert_eq!(translate_with(Console::Snes, 0x2_4000, c), None);
    }

    #[test]
    fn snes_sa1_bwram_and_iram() {
        let c = CartMap::Sa1 { bwram: 0x2_0000 };
        assert_eq!(translate_with(Console::Snes, 0x2_0000, c), Some(0x40_0000));
        assert_eq!(translate_with(Console::Snes, 0x3_0001, c), Some(0x41_0001));
        assert_eq!(translate_with(Console::Snes, 0x4_0000, c), None);
        assert_eq!(translate_with(Console::Snes, 0xA_0000, c), Some(0x3000));
        assert_eq!(translate_with(Console::Snes, 0xA_07FF, c), Some(0x37FF));
        assert_eq!(translate_with(Console::Snes, 0xA_0800, c), None);
        assert_eq!(translate_with(Console::Snes, 0xA_0000, CartMap::None), None);
    }
}
