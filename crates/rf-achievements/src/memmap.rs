//! RetroAchievements flat address spaces mapped onto CPU-bus addresses.
//!
//! RA does not use CPU addresses: each console has its own flat map, defined
//! by `rc_console_memory_regions` in rcheevos (MIT, readable under D-031).
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

/// Translate an RA address to the CPU-bus address to `peek`, or `None` when
/// the address is outside the map or this crate cannot serve it.
///
/// SNES cartridge RAM (`$020000+`) is `None`: the bank it lives in depends on
/// the cartridge's mapping, and `EmulatorCore::peek` takes CPU addresses
/// only. Achievement sets that use SRAM are therefore disabled by rcheevos
/// with a warning instead of reading garbage (documented gap, W41-05 may
/// extend `peek` or pass a second source).
pub fn translate(console: Console, ra_addr: u32) -> Option<u32> {
    match console {
        Console::Nes => match ra_addr {
            // Work RAM and its mirrors fold onto the 2 KiB the CPU decodes.
            0x0000..=0x1FFF => Some(ra_addr & 0x07FF),
            0x2000..=0xFFFF => Some(ra_addr),
            _ => None,
        },
        Console::Snes => match ra_addr {
            0x0000..=0x01_FFFF => Some(0x7E_0000 + ra_addr),
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
}
