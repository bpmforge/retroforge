//! SNES cartridge address mapping — LoROM and HiROM (ticket W6-02a;
//! FR-CORE-035, `docs/design/EMULATION_CORES.md` §3.1).
//!
//! ## Why this is a pure function
//!
//! Mapping is the one part of the bus with no state at all: a bank, an
//! offset and the cartridge's shape fully determine where an access
//! lands. Keeping it separate from [`crate::bus`] means the mapping can
//! be tested exhaustively — every one of the 16,777,216 addresses, in
//! both modes — without constructing a machine, and it means a mapping
//! bug reports itself as a mapping bug rather than as a mislocated byte
//! three layers up.
//!
//! ## The two maps
//!
//! **LoROM** puts a 32 KiB ROM slice in the upper half of every bank:
//! bank `$00:8000` and bank `$80:8000` are the same byte, and the ROM
//! offset is `(bank & $7F) * $8000 + (offset - $8000)`.
//!
//! **HiROM** maps ROM linearly: `$C0:0000` onward is the ROM from byte
//! zero, and the upper half of each low bank shows the corresponding
//! slice — so `$40:8000` and `$C0:8000` are the same byte.
//!
//! That pair of aliases is exactly what `fixtures/snes/mirror-map` checks
//! from inside the machine, and the reason its FORMAT.md says "same
//! question, asked at the address each mapping actually uses".
//!
//! ## Mirroring undersized ROMs
//!
//! A 32 KiB ROM occupies one LoROM bank, but the address space offers
//! 128. Hardware repeats the ROM rather than reading nothing, so the
//! computed offset is taken modulo the ROM length. This is not a
//! convenience: the mirror-map fixture is 32 KiB and its own reset vector
//! is fetched through bank `$00`, which only resolves because of it.

use rf_cart::SnesMapMode;

/// Where an access lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Cartridge ROM, at an offset already reduced modulo the ROM size.
    Rom(usize),
    /// Work RAM, at an offset into the full 128 KiB.
    Wram(usize),
    /// Cartridge save RAM, at an offset already reduced modulo its size.
    Sram(usize),
    /// A hardware register; carries the 16-bit offset, since every
    /// register block is identified by its offset alone.
    Register(u16),
    /// Nothing is mapped here. Reads see open bus; writes are dropped.
    Open,
}

/// Total work RAM: 128 KiB, at banks `$7E`-`$7F`.
pub const WRAM_LEN: usize = 128 * 1024;

/// Resolve a 24-bit address.
///
/// `rom_len` and `sram_len` shape the mirroring; a zero `sram_len` means
/// the cartridge has no save RAM, and those windows read as open bus
/// rather than aliasing onto something else.
#[must_use]
pub fn map(mode: SnesMapMode, bank: u8, offset: u16, rom_len: usize, sram_len: usize) -> Target {
    // Banks $7E-$7F are the full 128 KiB of work RAM, in both maps. This
    // is checked FIRST because those bank numbers fall inside the
    // $40-$7D "cartridge" range that the map-specific arms below would
    // otherwise claim.
    if bank == 0x7E || bank == 0x7F {
        return Target::Wram(((usize::from(bank) - 0x7E) << 16) | usize::from(offset));
    }

    let system_area = bank < 0x40 || (0x80..0xC0).contains(&bank);
    if system_area && offset < 0x8000 {
        return match offset {
            // The low 8 KiB of WRAM, mirrored into every system bank.
            // This is the alias the fixture's check 2 exercises.
            0x0000..=0x1FFF => Target::Wram(usize::from(offset)),
            // PPU, APU, joypad, CPU and DMA registers all live here. The
            // register file sorts out which is which; mapping only has to
            // know it is not memory.
            0x2000..=0x5FFF => Target::Register(offset),
            // $6000-$7FFF: HiROM puts save RAM here; LoROM leaves it open.
            _ => match mode {
                SnesMapMode::HiRom if sram_len > 0 => {
                    let index = ((usize::from(bank) & 0x1F) << 13) | usize::from(offset - 0x6000);
                    Target::Sram(index % sram_len)
                }
                _ => Target::Open,
            },
        };
    }

    match mode {
        SnesMapMode::LoRom => {
            // LoROM save RAM lives in banks $70-$7D (and $F0-$FF) below
            // $8000. Checked before the ROM arm, which would otherwise
            // claim the whole bank.
            if (0x70..0x7E).contains(&bank) && offset < 0x8000 && sram_len > 0 {
                let index = ((usize::from(bank) - 0x70) << 15) | usize::from(offset);
                return Target::Sram(index % sram_len);
            }
            if rom_len == 0 {
                return Target::Open;
            }
            // 32 KiB per bank, taken from the upper half. The `& 0x7FFF`
            // makes the lower half of banks $40-$7D mirror the upper,
            // which is what hardware does and what keeps this a single
            // expression rather than two cases.
            let index = ((usize::from(bank) & 0x7F) << 15) | usize::from(offset & 0x7FFF);
            Target::Rom(index % rom_len)
        }
        SnesMapMode::HiRom => {
            if rom_len == 0 {
                return Target::Open;
            }
            // Linear: the bank's low six bits select a full 64 KiB slice,
            // so $C0:0000 is ROM byte 0 and $40:8000 aliases $C0:8000.
            let index = ((usize::from(bank) & 0x3F) << 16) | usize::from(offset);
            Target::Rom(index % rom_len)
        }
    }
}
