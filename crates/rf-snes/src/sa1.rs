//! SA-1 SNES-side register file and board memories (ticket W17-01,
//! D-013, SA-1 slice 1).
//!
//! This slice has no SA-1 CPU (that is W17-02): the SNES CPU runs the
//! cartridge's SNES-side code alone, with the SA-1 register window wired
//! up as data storage so later slices can read back what was written.
//! Cited to fullsnes "SNES Cart SA-1 I/O Map" throughout.
//!
//! ## Why the write-only block is one raw byte array
//!
//! The whole $2200-$22FF write-only block is stored verbatim, offset by
//! address, rather than decoded field-by-field. This slice only needs
//! four of those bytes (the $2220-$2223 ROM bank registers) and one more
//! ($2224 BMAPS) to build the SNES-side memory map; everything else
//! (control/timer $2200-$2215, DMA $2230-$2239/$223F, arithmetic
//! $2250-$2254, the variable-length bit reader $2258-$225B, and the
//! undocumented offsets fullsnes notes some titles write anyway) is
//! future slices' (W17-02..04) job to interpret. Storing every write now
//! means a title that pokes those registers before this slice's CPU-only
//! boot reaches them does not lose the value once a later slice starts
//! reading it.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};

/// Offset of the write-only register block's first byte ($2200).
const BASE: u16 = 0x2200;
/// The write-only block spans exactly one page ($2200-$22FF); the
/// read-only block ($2300-$230E) is handled separately by [`Sa1Regs::read`]
/// and is never written here.
const RAW_LEN: usize = 0x100;

/// Register offsets used by this slice's mapping (fullsnes "SNES Cart
/// SA-1 Memory Control").
const CXB: usize = 0x2220 - BASE as usize;
const DXB: usize = 0x2221 - BASE as usize;
const EXB: usize = 0x2222 - BASE as usize;
const FXB: usize = 0x2223 - BASE as usize;
const BMAPS: usize = 0x2224 - BASE as usize;

/// The SA-1 SNES-side register window's live state.
#[derive(Debug, Clone)]
pub struct Sa1Regs {
    /// Every write-only register, $2200-$22FF, indexed by `offset - $2200`.
    raw: [u8; RAW_LEN],
}

impl Default for Sa1Regs {
    fn default() -> Self {
        Self::new()
    }
}

impl Sa1Regs {
    /// Reset values fullsnes documents ("Reset", under "SNES Cart SA-1
    /// I/O Map"): `$2200`=$20, `$2220-$2223`=$00,$01,$02,$03, `$2228`=$FF;
    /// every other write-only register resets to $00 or is "N/A" (this
    /// slice zeroes those too — a read of them is open bus regardless,
    /// per [`Self::read`]).
    #[must_use]
    pub fn new() -> Self {
        let mut raw = [0u8; RAW_LEN];
        raw[(0x2200 - BASE) as usize] = 0x20;
        raw[CXB] = 0x00;
        raw[DXB] = 0x01;
        raw[EXB] = 0x02;
        raw[FXB] = 0x03;
        raw[(0x2228 - BASE) as usize] = 0xFF;
        Self { raw }
    }

    /// A write into `$2200-$22FF`. Panics if `offset` is outside that
    /// range — callers only reach this through [`crate::mapping::Target::Sa1Register`],
    /// which `bus.rs` restricts to `$2200-$23FF`, and `$2300-$23FF`
    /// writes are dropped by the caller before this is reached (see
    /// `SnesBus::write`).
    pub fn write(&mut self, offset: u16, value: u8) {
        self.raw[usize::from(offset - BASE)] = value;
    }

    /// A read of `$2200-$23FF`. `None` means open bus: the write-only
    /// block itself ($2200-$22FF, real hardware never reads it back
    /// either — every port in fullsnes's table is marked "(W)" only) and
    /// any offset this slice's read-only table doesn't name.
    ///
    /// **Slice 1 has no SA-1 CPU**, so every read-only register
    /// ($2300-$230E: SFR, CFR, HCR, VCR, MR, OF, VDP, VC) answers its
    /// documented reset value rather than a live-computed one — nothing
    /// exists yet to move a status flag or generate a message, timer
    /// count, arithmetic result or bit-reader byte. W17-02 onward gives
    /// these real behaviour as each subsystem lands.
    #[must_use]
    pub fn read(&self, offset: u16) -> Option<u8> {
        if (0x2300..=0x230E).contains(&offset) {
            Some(0)
        } else {
            None
        }
    }

    /// `$2220` CXB — HiROM `$C0-$CF` / LoROM `$00-$1F`.
    #[must_use]
    pub fn cxb(&self) -> u8 {
        self.raw[CXB]
    }
    /// `$2221` DXB — HiROM `$D0-$DF` / LoROM `$20-$3F`.
    #[must_use]
    pub fn dxb(&self) -> u8 {
        self.raw[DXB]
    }
    /// `$2222` EXB — HiROM `$E0-$EF` / LoROM `$80-$9F`.
    #[must_use]
    pub fn exb(&self) -> u8 {
        self.raw[EXB]
    }
    /// `$2223` FXB — HiROM `$F0-$FF` / LoROM `$A0-$BF`.
    #[must_use]
    pub fn fxb(&self) -> u8 {
        self.raw[FXB]
    }
    /// `$2224` BMAPS — the SNES-side `$6000-$7FFF` BW-RAM block select.
    #[must_use]
    pub fn bmaps(&self) -> u8 {
        self.raw[BMAPS]
    }

    pub(crate) fn save(&self, o: &mut StateOut) -> Result<(), StateError> {
        o.bytes(&self.raw)
    }

    pub(crate) fn load(&mut self, i: &mut StateIn) -> Result<(), StateError> {
        i.fill(&mut self.raw)
    }
}

/// The SA-1 board's live state: the register file plus its two on-board
/// memories (fullsnes "Misc": 2 KiB I-RAM, up to 2 MiB BW-RAM).
#[derive(Debug, Clone)]
pub struct Sa1State {
    pub regs: Sa1Regs,
    pub iram: Vec<u8>,
    pub bwram: Vec<u8>,
    /// The parsed cartridge's fixed sizes — kept alongside the live
    /// memories so [`crate::mapping::Sa1RomBanks`] can be rebuilt on every
    /// access without recomputing lengths from the `Vec`s (a zero-length
    /// `Vec` and "no BW-RAM" are the same fact stated two ways, but the
    /// board is the one `rf_cart` already computed).
    pub board: rf_cart::Sa1Board,
}

impl Sa1State {
    #[must_use]
    pub fn new(board: rf_cart::Sa1Board) -> Self {
        Self {
            regs: Sa1Regs::new(),
            iram: vec![0; board.iram_len],
            bwram: vec![0; board.bwram_len],
            board,
        }
    }

    /// The live [`crate::mapping::Sa1RomBanks`] view `target()` resolves
    /// addresses against.
    #[must_use]
    pub fn banks(&self) -> crate::mapping::Sa1RomBanks {
        crate::mapping::Sa1RomBanks {
            cxb: self.regs.cxb(),
            dxb: self.regs.dxb(),
            exb: self.regs.exb(),
            fxb: self.regs.fxb(),
            bmaps: self.regs.bmaps(),
            board: self.board,
        }
    }
}
