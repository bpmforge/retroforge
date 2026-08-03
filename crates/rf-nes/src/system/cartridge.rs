//! Turning an `rf-cart`-parsed NES header back into actual PRG/CHR bytes.
//!
//! `rf_cart::Cartridge::load` gives us cartridge *metadata* only —
//! `NesHeader` (mapper, PRG/CHR sizes, mirroring, trainer/battery flags) —
//! never the ROM bytes themselves (pre-flight note on ticket W1-02: fixing
//! that in `rf-cart` is out of this ticket's scope). This module does the
//! slicing `rf-cart`'s own header parser already validated the length for:
//! [nesdev.org/wiki/INES](https://www.nesdev.org/wiki/INES) lays out the
//! file as `16-byte header, optional 512-byte trainer, PRG ROM, CHR ROM`,
//! in that order, which is exactly what `parse_nes_header` used to compute
//! its own `needed` bounds check — so the slice indices here can never be
//! out of range for an image that already parsed successfully.
use rf_cart::{CartError, Cartridge, NesHeader};

const HEADER_LEN: usize = 16;
const TRAINER_LEN: usize = 512;
/// Fallback CHR RAM size when the header declares `chr_rom_size == 0`
/// (cartridge uses CHR RAM rather than CHR ROM). 8 KiB is the common case
/// for mapper 0 (NROM); nothing in this ticket's acceptance depends on the
/// exact size since no PPU exists yet to read it (W1-04a+ seam).
const DEFAULT_CHR_RAM_SIZE: usize = 8 * 1024;

/// Everything that can go wrong turning a raw ROM image into an
/// `rf-nes`-usable [`NesRom`]. Wraps [`CartError`] for the parsing/format
/// failures `rf-cart` already detects, plus the two failure modes that are
/// this crate's own responsibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NesLoadError {
    /// `rf-cart` itself rejected the image (bad magic, truncated, unknown
    /// mapper, ...).
    Cart(CartError),
    /// The image parsed, but as an SNES cartridge, not NES.
    NotNesImage,
    /// A mapper `rf-cart`'s header parser already accepts (it is in
    /// `nes::SUPPORTED_MAPPERS`) but that `rf-nes` does not implement yet.
    /// Ticket W1-02 only implements mapper 0 (NROM); MMC1/UxROM/CNROM/MMC3
    /// are `rf-cart`-parseable today but have no `rf-nes` behavior until a
    /// later ticket lands, so this crate must not silently treat their PRG
    /// data as if it were NROM-mapped.
    UnimplementedMapper(u16),
}

impl std::fmt::Display for NesLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NesLoadError::Cart(e) => write!(f, "{e}"),
            NesLoadError::NotNesImage => write!(f, "image is an SNES cartridge, not NES"),
            NesLoadError::UnimplementedMapper(id) => {
                write!(
                    f,
                    "mapper {id} is not implemented by rf-nes yet (NROM/mapper 0 only)"
                )
            }
        }
    }
}

impl std::error::Error for NesLoadError {}

/// A fully materialized NES cartridge: the parsed header plus the actual
/// PRG/CHR bytes sliced out of the raw image. `chr_rom` holds either real
/// CHR ROM data or a zeroed CHR RAM backing store — see `chr_is_ram`.
///
/// CHR data has no consumer yet (no PPU exists until W1-04a); it is
/// carried here — rather than discarded — so that ticket has an
/// unambiguous seam: `NesRom::chr_rom`/`chr_is_ram` are the intended
/// hand-off point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NesRom {
    header: NesHeader,
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_is_ram: bool,
}

impl NesRom {
    /// Parse `raw` via `rf_cart::Cartridge::load`, then slice PRG/CHR out
    /// of it ourselves (see module doc). Rejects mapper numbers `rf-nes`
    /// does not implement yet, even though `rf-cart`'s header parser
    /// accepts them.
    pub fn from_ines_bytes(raw: &[u8]) -> Result<Self, NesLoadError> {
        let header = match Cartridge::load(raw).map_err(NesLoadError::Cart)? {
            Cartridge::Nes { header, .. } => header,
            Cartridge::Snes { .. } => return Err(NesLoadError::NotNesImage),
        };
        if header.mapper != 0 {
            return Err(NesLoadError::UnimplementedMapper(header.mapper));
        }

        let mut offset = HEADER_LEN;
        if header.trainer {
            offset += TRAINER_LEN;
        }
        let prg_end = offset + header.prg_rom_size;
        // Safe: `Cartridge::load` -> `parse_nes_header` already verified
        // `raw.len() >= HEADER_LEN + trainer + prg_rom_size + chr_rom_size`
        // using this exact same layout, so these slices are in bounds.
        let prg_rom = raw[offset..prg_end].to_vec();

        let chr_is_ram = header.chr_rom_size == 0;
        let chr_rom = if chr_is_ram {
            vec![0u8; DEFAULT_CHR_RAM_SIZE]
        } else {
            raw[prg_end..prg_end + header.chr_rom_size].to_vec()
        };

        Ok(NesRom {
            header,
            prg_rom,
            chr_rom,
            chr_is_ram,
        })
    }

    /// The parsed iNES/NES 2.0 header (mapper, mirroring, battery, ...).
    pub fn header(&self) -> &NesHeader {
        &self.header
    }

    /// Raw PRG ROM bytes, 16 KiB or 32 KiB for mapper 0 (NROM).
    pub fn prg_rom(&self) -> &[u8] {
        &self.prg_rom
    }

    /// Raw CHR ROM/RAM bytes (see `chr_is_ram` for which). Unused by the
    /// CPU bus; carried for the future PPU ticket (W1-04a).
    pub fn chr_rom(&self) -> &[u8] {
        &self.chr_rom
    }

    /// Whether `chr_rom` is a zeroed CHR RAM backing store (header
    /// declared `chr_rom_size == 0`) rather than real ROM data.
    pub fn chr_is_ram(&self) -> bool {
        self.chr_is_ram
    }
}
