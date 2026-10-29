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

/// The inverse of [`translate_with`]: the RA address that reads the byte at
/// CPU-bus address `bus_addr`, or `None` when RA has no address for it.
///
/// Mirrors fold onto the one RA address, so `translate_with(ra_address(a))`
/// is `a` only for canonical `a` (NES `$0000-$07FF`; SNES `$7E0000-$7FFFFF`
/// and cartridge RAM at its [`CartMap`] home). Folded: NES `$0800-$1FFF` to
/// `$0000-$07FF`; SNES `$00-$3F`/`$80-$BF:0000-1FFF` to WRAM `$7E:0000-1FFF`.
pub fn ra_address(console: Console, bus_addr: u32, cart: CartMap) -> Option<u32> {
    match console {
        Console::Nes => match bus_addr {
            0x0000..=0x1FFF => Some(bus_addr & 0x07FF),
            0x2000..=0xFFFF => Some(bus_addr),
            _ => None,
        },
        Console::Snes => {
            let (bank, off) = (bus_addr >> 16, bus_addr & 0xFFFF);
            if bank > 0xFF {
                return None;
            }
            match bank {
                0x7E | 0x7F => return Some(bus_addr - 0x7E_0000),
                0x00..=0x3F | 0x80..=0xBF if off < 0x2000 => return Some(off),
                _ => {}
            }
            let lo = bank & 0x7F;
            let cart_off = match cart {
                CartMap::LoRom { ram } if (0x70..=0x7D).contains(&bank) && off < 0x8000 => {
                    Some(((bank - 0x70) << 15 | off, ram))
                }
                CartMap::HiRom { ram }
                    if (0x20..=0x3F).contains(&lo) && (0x6000..0x8000).contains(&off) =>
                {
                    Some((((lo - 0x20) << 13) | (off - 0x6000), ram))
                }
                CartMap::Sa1 { bwram } if (0x40..=0x43).contains(&bank) => {
                    Some((((bank - 0x40) << 16) | off, bwram))
                }
                _ => None,
            };
            if let Some((o, size)) = cart_off {
                return (o < size && o <= SNES_CART_RAM_END - SNES_CART_RAM)
                    .then_some(SNES_CART_RAM + o);
            }
            match cart {
                CartMap::Sa1 { .. } if bank < 0x40 && (0x3000..0x3000 + 0x800).contains(&off) => {
                    Some(SNES_IRAM + (off - 0x3000))
                }
                _ => None,
            }
        }
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

    #[test]
    fn ra_address_inverts_translate() {
        let carts = [
            CartMap::None,
            CartMap::LoRom { ram: 0x8000 * 3 },
            CartMap::HiRom { ram: 0x2000 * 5 },
            CartMap::Sa1 { bwram: 0x1_8000 },
        ];
        for console in [Console::Nes, Console::Snes] {
            for cart in carts {
                for ra in (0..0xB_0000u32)
                    .step_by(7)
                    .chain([0, 0x7FF, 0x1_FFFF, 0x2_0000, 0x9_FFFF, 0xA_0000, 0xA_07FF])
                {
                    if let Some(bus) = translate_with(console, ra, cart) {
                        let back = ra_address(console, bus, cart);
                        // NES mirrors fold to the canonical RA address.
                        let want = if console == Console::Nes && ra < 0x2000 {
                            ra & 0x7FF
                        } else {
                            ra
                        };
                        assert_eq!(
                            back,
                            Some(want),
                            "{console:?} {cart:?} ra {ra:#X} bus {bus:#X}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn ra_address_folds_mirrors_and_refuses_unmapped() {
        assert_eq!(ra_address(Console::Nes, 0x0810, CartMap::None), Some(0x10));
        assert_eq!(ra_address(Console::Nes, 0x1_0000, CartMap::None), None);
        assert_eq!(
            ra_address(Console::Snes, 0x00_0123, CartMap::None),
            Some(0x123)
        );
        assert_eq!(
            ra_address(Console::Snes, 0xBF_1FFF, CartMap::None),
            Some(0x1FFF)
        );
        assert_eq!(
            ra_address(Console::Snes, 0x7E_0010, CartMap::None),
            Some(0x10)
        );
        assert_eq!(
            ra_address(Console::Snes, 0x7F_FFFF, CartMap::None),
            Some(0x1_FFFF)
        );
        // Cartridge RAM without a layout, or past its size, has no RA address.
        assert_eq!(ra_address(Console::Snes, 0x70_0000, CartMap::None), None);
        assert_eq!(
            ra_address(Console::Snes, 0x70_0000, CartMap::LoRom { ram: 0 }),
            None
        );
        assert_eq!(ra_address(Console::Snes, 0x3000, CartMap::None), None);
    }
}
