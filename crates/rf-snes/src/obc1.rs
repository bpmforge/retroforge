//! OBC1 ("OBJ Controller"), fullsnes "SNES Cart OBC1 (OBJ Controller)"
//! (ticket W19-01). One retail title: Metal Combat: Falcon's Revenge
//! (1993), which also requires a Super Scope lightgun the chip has nothing
//! to do with.
//!
//! ## What the chip is
//!
//! Unlike SA-1/GSU/DSP-1, OBC1 is not a second processor and has no
//! firmware of its own — clean-room hardware emulation with nothing
//! copyrighted to avoid. Fullsnes: "a 80pin OBJ Controller chip... Port
//! 7FF0h-7FF3h/7FF5h are totally useless... putting a huge 80pin chip into
//! the cartridge for merging 2bit fragments is definetly overcomplicated."
//! It is a pure address remapper sitting in front of the cart's own 8 KiB
//! battery-backed SRAM (`crate::bus::SnesBus::sram`), which is why this
//! module owns no bulk buffer of its own — only the three real registers.
//!
//! ## Register model
//!
//! `$7FF0-$7FF7` (fullsnes "OBC1 I/O Ports"):
//! - `$7FF0`/`$7FF1`/`$7FF2`/`$7FF3` (OAM Xloc/Yloc/Tile/Attr, R/W):
//!   redirect to SRAM byte `[Base+Index*4+0..3]`. Not real registers —
//!   [`crate::mapping::obc1_target`] resolves them straight to
//!   [`crate::mapping::Target::Sram`].
//! - `$7FF4` (OAM Bits, R/W): redirects to SRAM byte
//!   `[Base+Index/4+200h]`, bits `(Index AND 3)*2 .. +1`. Fullsnes: reads
//!   "reportedly return the desired BYTE... WITHOUT isolating & shifting
//!   the desired BITS into place"; writes are documented as a
//!   read-modify-write of just those 2 bits. [`crate::bus::SnesBus`]
//!   implements both, using [`Self::index`] at access time to find the bit
//!   position ([`crate::mapping::Target::Obc1Bits`]).
//! - `$7FF5` (Base select, R/W): bit 0 selects the 220h-byte table's base
//!   address — "0=7C00h, 1=7800h" (fullsnes; note this is the *inverse* of
//!   the usual "bit clear = first option" convention). See
//!   [`Self::base_offset`].
//! - `$7FF6` (Index / OBJ Number, R/W): 0..127. Fullsnes: "the Index isn't
//!   automatically incremented" — software must rewrite it before every
//!   access.
//! - `$7FF7` ("Unknown (set to 00h or 0Ah, maybe SRAM vs I/O mode
//!   select)"): stored and read back verbatim; fullsnes hedges this with
//!   "maybe", so nothing in this build branches on it. Writes of a value
//!   other than the two the chapter names are counted in
//!   [`Self::unknown_reg_other_writes`] — diagnostic only, mirrors
//!   `crate::sa1::Sa1Regs::unknown_write_offsets`'s "does not gate or
//!   alter any write" contract.
//!
//! ## What is NOT modelled
//!
//! Every sentence fullsnes hedges with "reportedly" or "?" is left alone
//! rather than guessed at, per the same discipline SA-1/GSU's own modules
//! use for their ambiguous corners:
//! - "Setting Index bits7+5 does reportedly enable SRAM mapping at
//!   6000h..77FFh?" — not implemented; `$6000-$77FF` behaves as ordinary
//!   SRAM regardless of the index value.
//! - "ROM is reportedly mapped to bank 00h..3Fh, and also to bank
//!   70h..71h?" — the cartridge's plain LoROM ROM mapping (`crate::mapping::map`)
//!   is unaffected by anything in this module either way.
//! - The read/write timing restrictions `$7FF4` "may involve" are not
//!   modelled: this build's read-modify-write is instantaneous within one
//!   bus access, same as every other register in this crate.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};

/// `$7FF5` bit 0 clear: table base `$7C00`, i.e. SRAM byte offset
/// `$7C00-$6000`.
const BASE_LOW: usize = 0x7C00 - 0x6000;
/// `$7FF5` bit 0 set: table base `$7800`, i.e. SRAM byte offset
/// `$7800-$6000`.
const BASE_HIGH: usize = 0x7800 - 0x6000;
/// `$7FF6` Index is documented 0..127 — a 7-bit field.
const INDEX_MASK: u8 = 0x7F;

/// The OBC1's three true registers (`$7FF5-$7FF7`) — see the module doc
/// for the full register model and citations. The sprite/attribute table
/// they address lives in the cartridge's shared `crate::bus::SnesBus::sram`,
/// not here.
#[derive(Debug, Clone, Copy)]
pub struct Obc1Regs {
    /// `$7FF5` raw byte (only bit 0 is documented).
    base: u8,
    /// `$7FF6` raw byte (only bits 0-6 are documented as meaningful for
    /// addressing — see [`Self::index_masked`] — but the full byte is
    /// stored/read back verbatim, same as every other undocumented-bit
    /// port in this crate).
    index: u8,
    /// `$7FF7` raw byte.
    unknown: u8,
    /// Diagnostic count of `$7FF7` writes whose value was neither `$00`
    /// nor `$0A` (the two fullsnes names). Not part of save state — see
    /// the module doc.
    pub unknown_reg_other_writes: u32,
}

impl Default for Obc1Regs {
    fn default() -> Self {
        Self::new()
    }
}

impl Obc1Regs {
    /// Fullsnes documents no reset values for this chip (unlike SA-1's
    /// explicit "Reset" column) — every register starts zeroed, same
    /// default every other undocumented-reset port in this crate uses.
    #[must_use]
    pub fn new() -> Self {
        Self {
            base: 0,
            index: 0,
            unknown: 0,
            unknown_reg_other_writes: 0,
        }
    }

    /// `$7FF5` bit 0 -> the selected table base, as an SRAM byte offset
    /// (fullsnes: "Base for 220h-byte region (bit0: 0=7C00h, 1=7800h)").
    #[must_use]
    pub fn base_offset(&self) -> usize {
        if self.base & 1 == 0 {
            BASE_LOW
        } else {
            BASE_HIGH
        }
    }

    /// `$7FF6` masked to the documented 0..127 OBJ-number range.
    #[must_use]
    pub fn index_masked(&self) -> u8 {
        self.index & INDEX_MASK
    }

    /// Build the small pure-mapping view [`crate::mapping::obc1_target`]
    /// needs. Recomputed on every access rather than cached, the same
    /// pattern `Sa1State::banks`/`GsuState::board` use.
    #[must_use]
    pub fn board(&self, sram_len: usize) -> crate::mapping::Obc1Board {
        crate::mapping::Obc1Board {
            base_offset: self.base_offset(),
            index: self.index_masked(),
            sram_len,
        }
    }

    /// A read of `$7FF5`, `$7FF6` or `$7FF7`.
    ///
    /// # Panics
    /// If `offset` is outside `$7FF5-$7FF7` — callers only reach this
    /// through [`crate::mapping::Target::Obc1Register`], which
    /// [`crate::mapping::obc1_target`] only produces for those three
    /// offsets.
    #[must_use]
    pub fn read(&self, offset: u16) -> u8 {
        match offset {
            0x7FF5 => self.base,
            0x7FF6 => self.index,
            0x7FF7 => self.unknown,
            _ => unreachable!("Obc1Register offset outside $7FF5-$7FF7: {offset:#06x}"),
        }
    }

    /// A write to `$7FF5`, `$7FF6` or `$7FF7`. Same panic contract as
    /// [`Self::read`].
    pub fn write(&mut self, offset: u16, value: u8) {
        match offset {
            0x7FF5 => self.base = value,
            0x7FF6 => self.index = value,
            0x7FF7 => {
                if value != 0x00 && value != 0x0A {
                    self.unknown_reg_other_writes += 1;
                }
                self.unknown = value;
            }
            _ => unreachable!("Obc1Register offset outside $7FF5-$7FF7: {offset:#06x}"),
        }
    }

    pub(crate) fn save(&self, o: &mut StateOut) -> Result<(), StateError> {
        o.u8(self.base)?;
        o.u8(self.index)?;
        o.u8(self.unknown)
    }

    pub(crate) fn load(&mut self, i: &mut StateIn) -> Result<(), StateError> {
        self.base = i.u8()?;
        self.index = i.u8()?;
        self.unknown = i.u8()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_state_selects_base_7c00_and_index_zero() {
        let regs = Obc1Regs::new();
        assert_eq!(regs.base_offset(), BASE_LOW);
        assert_eq!(regs.index_masked(), 0);
    }

    #[test]
    fn base_select_bit0_clear_is_7c00_set_is_7800() {
        let mut regs = Obc1Regs::new();
        regs.write(0x7FF5, 0x00);
        assert_eq!(regs.base_offset(), BASE_LOW);
        regs.write(0x7FF5, 0x01);
        assert_eq!(regs.base_offset(), BASE_HIGH);
        // Only bit 0 is documented; an odd value with other bits set
        // still selects the same base.
        regs.write(0x7FF5, 0xFF);
        assert_eq!(regs.base_offset(), BASE_HIGH);
    }

    #[test]
    fn index_masks_to_seven_bits_for_addressing_but_reads_back_raw() {
        let mut regs = Obc1Regs::new();
        regs.write(0x7FF6, 0xFF);
        assert_eq!(regs.index_masked(), 0x7F);
        // The raw port mirrors exactly what was written.
        assert_eq!(regs.read(0x7FF6), 0xFF);
    }

    #[test]
    fn unknown_register_reads_mirror_writes() {
        let mut regs = Obc1Regs::new();
        regs.write(0x7FF7, 0x0A);
        assert_eq!(regs.read(0x7FF7), 0x0A);
        assert_eq!(regs.unknown_reg_other_writes, 0);
        regs.write(0x7FF7, 0x00);
        assert_eq!(regs.read(0x7FF7), 0x00);
        assert_eq!(regs.unknown_reg_other_writes, 0);
        regs.write(0x7FF7, 0x42);
        assert_eq!(regs.read(0x7FF7), 0x42);
        assert_eq!(regs.unknown_reg_other_writes, 1);
    }

    #[test]
    fn board_view_reflects_live_registers() {
        let mut regs = Obc1Regs::new();
        regs.write(0x7FF5, 0x01);
        regs.write(0x7FF6, 0x05);
        let board = regs.board(8192);
        assert_eq!(board.base_offset, BASE_HIGH);
        assert_eq!(board.index, 5);
        assert_eq!(board.sram_len, 8192);
    }

    #[test]
    fn save_load_round_trips_all_three_registers() {
        let mut regs = Obc1Regs::new();
        regs.write(0x7FF5, 0x01);
        regs.write(0x7FF6, 0x2A);
        regs.write(0x7FF7, 0x0A);

        struct MemStream {
            buf: Vec<u8>,
            at: usize,
        }
        impl rf_core_api::StateWriter for MemStream {
            fn write_all(&mut self, bytes: &[u8]) -> Result<(), StateError> {
                self.buf.extend_from_slice(bytes);
                Ok(())
            }
        }
        impl rf_core_api::StateReader for MemStream {
            fn read_exact(&mut self, out: &mut [u8]) -> Result<(), StateError> {
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
        regs.save(&mut StateOut::new(&mut stream)).expect("save");
        let mut restored = Obc1Regs::new();
        restored.load(&mut StateIn::new(&mut stream)).expect("load");

        assert_eq!(restored.read(0x7FF5), 0x01);
        assert_eq!(restored.read(0x7FF6), 0x2A);
        assert_eq!(restored.read(0x7FF7), 0x0A);
    }
}
