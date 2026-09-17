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

/// The mapper numbers `NesBus::new`'s dispatch can actually construct
/// (ticket W2-02, message derived from it by W2-16; `4`/MMC3 added by
/// W2-03, in the same commit as its `NesBus::new` arm — see that ticket's
/// own note: W2-02 shipped its mappers unreachable once before by letting
/// this list and the dispatch `match` drift apart across commits). This is
/// rf-nes's statement about what it can EMULATE — deliberately separate
/// from `rf_cart::nes`'s `SUPPORTED_MAPPERS`, which is rf-cart's statement
/// about which headers it can PARSE and identify. The two lists are
/// identical as of W2-03 (both `[0, 1, 2, 3, 4]`) — a coincidence of this
/// crate's current mapper roadmap, not a reason to collapse them: the next
/// mapper rf-cart learns to identify should not silently become emulable
/// the moment it's added there.
///
/// **28 was missing until W7-11's fixture caught it.** Action 53 was
/// implemented, given 12 unit tests, added to `rf_cart`'s
/// `SUPPORTED_MAPPERS`, and wired into `NesBus::new`'s factory — and this
/// gate rejected it before that factory was ever reached, so the `28 =>`
/// arm was dead code and no mapper-28 ROM could load. Every one of those
/// unit tests passed throughout, because each constructed `Action53`
/// directly. That is the whole argument for this ticket's third criterion
/// existing: only a real ROM entering through `from_ines_bytes` crosses
/// this line.
pub(crate) const EMULATED_MAPPERS: &[u16] =
    &[0, 1, 2, 3, 4, 7, 9, 11, 28, 64, 66, 69, 71, 79, 118, 206];

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
    /// `nes::SUPPORTED_MAPPERS`) but that `rf-nes` does not implement yet
    /// (i.e. it's absent from `EMULATED_MAPPERS`, above) — this crate must
    /// not silently treat an unemulated mapper's PRG data as if it were
    /// NROM-mapped. As of W2-03, `EMULATED_MAPPERS` and
    /// `nes::SUPPORTED_MAPPERS` happen to be identical (`[0, 1, 2, 3, 4]`),
    /// so this variant currently has no reachable example — see
    /// `system/tests/rom_loading.rs` for how that's tested (or rather, why
    /// it currently can't be, directly).
    UnimplementedMapper(u16),
}

impl std::fmt::Display for NesLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NesLoadError::Cart(e) => write!(f, "{e}"),
            NesLoadError::NotNesImage => write!(f, "image is an SNES cartridge, not NES"),
            NesLoadError::UnimplementedMapper(id) => {
                // Ticket W2-16: this used to say "(NROM/mapper 0 only)",
                // which went stale the moment W2-02 landed MMC1/UxROM/
                // CNROM the same day and told users something false about
                // the emulator's own capability. Derived from
                // `EMULATED_MAPPERS` rather than restated in prose, so it
                // cannot drift again: adding a mapper to that list updates
                // this message for free.
                let supported = EMULATED_MAPPERS
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    f,
                    "mapper {id} is not implemented by rf-nes yet \
                     (emulated so far: {supported})"
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
        // Ticket W2-02: this gate is the ONLY thing standing between a real
        // `.nes` file and `NesBus::new`'s mapper dispatch, so it must list
        // exactly what that `match` can construct — no more (or `new`
        // hits its `unreachable!`), no less (or a supported mapper is
        // rejected and the implementation is dead code from the
        // production path, which is precisely what happened when this
        // read `!= 0` while MMC1/UxROM/CNROM were already implemented).
        //
        // Deliberately NOT reusing `rf_cart::nes::SUPPORTED_MAPPERS`: that
        // list is rf-cart's statement about which headers it can *parse
        // and identify* (it includes 4/MMC3 for identification purposes),
        // whereas this one is rf-nes's statement about which mappers it
        // can actually *emulate*. They are different questions that happen
        // to overlap, and collapsing them would silently admit MMC3 the
        // moment rf-cart learned to name it. MMC3 is ticket W2-03; when it
        // lands, add `4` here in the same commit as its `NesBus::new` arm.
        if !EMULATED_MAPPERS.contains(&header.mapper) {
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
