//! The 5A22's memory-speed model (ticket W6-01b;
//! `docs/design/EMULATION_CORES.md` §3.1, FR-CORE-030).
//!
//! ## Why this exists before anything needs it
//!
//! The design doc is blunt about it: "CPU timing is expressed in master
//! cycles from day one — retrofitting is not viable." Every bus access on
//! this machine costs 6, 8 or 12 master cycles depending on *where* it
//! lands, so a core that counts instructions, or even CPU cycles, has
//! already thrown away the information that makes SNES timing work. The
//! difference is not academic: the same `LDA $2100` costs 6 in a fast
//! bank and 8 in a slow one, and a scanline is 1364 master cycles.
//!
//! ## The regions
//!
//! Master clock 21.477 MHz (`MASTER_CLOCK_HZ`). Costs, per fullsnes
//! "Memory Timing" and consistent with the bit-trick form bsnes/higan
//! uses (cross-checked exhaustively — see the tests):
//!
//! | Bank      | Offset        | Contents              | Cycles |
//! |-----------|---------------|-----------------------|--------|
//! | `$00-$3F` | `$0000-$1FFF` | WRAM mirror           | 8      |
//! | `$00-$3F` | `$2000-$3FFF` | PPU / APU registers   | 6      |
//! | `$00-$3F` | `$4000-$41FF` | joypad                | **12** |
//! | `$00-$3F` | `$4200-$5FFF` | CPU / DMA registers   | 6      |
//! | `$00-$3F` | `$6000-$7FFF` | expansion             | 8      |
//! | `$00-$3F` | `$8000-$FFFF` | cartridge ROM         | 8      |
//! | `$40-$7F` | any           | ROM / WRAM            | 8      |
//! | `$80-$BF` | `$0000-$7FFF` | mirrors of `$00-$3F`  | as above |
//! | `$80-$FF` | `$8000-$FFFF` | ROM                   | 6 fast / 8 slow |
//! | `$C0-$FF` | any           | ROM                   | 6 fast / 8 slow |
//!
//! The joypad row is the one worth staring at: `$4000-$41FF` is **12**,
//! twice the cost of the CPU registers 512 bytes above it, and it is the
//! only 12 on the machine.
//!
//! ## FastROM
//!
//! `$420D` bit 0 selects 6 instead of 8 for banks `$80+` above `$8000`.
//! It changes nothing anywhere else — a fact worth stating because
//! "FastROM makes the machine faster" invites applying it too widely.
//! The register itself belongs to W6-02a; this module only takes the
//! resulting flag as an argument, so the cost function stays pure.
//!
//! ## What this does NOT model
//!
//! Only the cost of a bus **access**. A 65816 instruction also spends
//! *internal* cycles that touch no address (the SingleStepTests traces
//! show them as entries with no value), and those carry their own cost.
//! Totalling a whole instruction therefore needs a cycle-accurate
//! executor, which this ticket does not build — see the W6-01b close
//! note. [`AccessCost`] deliberately counts what it can actually
//! account for, and names it accordingly, rather than reporting a
//! plausible instruction total that would be quietly short.

use super::CpuBus;

/// SNES master clock, in Hz.
pub const MASTER_CLOCK_HZ: u32 = 21_477_270;

/// A fast access: 6 master cycles.
pub const FAST: u8 = 6;
/// A normal access: 8 master cycles.
pub const SLOW: u8 = 8;
/// The joypad region: 12 master cycles.
pub const XSLOW: u8 = 12;

/// Master cycles for one bus access at `addr`.
///
/// `fast_rom` is `$420D` bit 0. See the module doc for the region table.
#[must_use]
pub fn access_cycles(addr: u32, fast_rom: bool) -> u8 {
    let bank = ((addr >> 16) & 0xFF) as u8;
    let offset = addr as u16;

    // Banks $40-$7F and $C0-$FF have no register window at all; the whole
    // bank is memory. $C0+ is the fast half.
    if (0x40..=0x7F).contains(&bank) {
        return SLOW;
    }
    if bank >= 0xC0 {
        return if fast_rom { FAST } else { SLOW };
    }

    // $00-$3F and $80-$BF share one layout; only the ROM half above
    // $8000 tells them apart.
    if offset >= 0x8000 {
        return if bank >= 0x80 && fast_rom { FAST } else { SLOW };
    }
    match offset {
        0x0000..=0x1FFF => SLOW,  // WRAM mirror
        0x2000..=0x3FFF => FAST,  // PPU / APU
        0x4000..=0x41FF => XSLOW, // joypad — the only 12
        0x4200..=0x5FFF => FAST,  // CPU / DMA
        _ => SLOW,                // $6000-$7FFF expansion
    }
}

/// A [`CpuBus`] wrapper that tallies the master-cycle cost of the
/// accesses passing through it.
///
/// Exists so the model is *used* rather than merely defined: a cost
/// function nothing calls is indistinguishable from a wrong one. Wrapping
/// the bus rather than threading a counter through the CPU keeps every
/// addressing mode and opcode untouched, which matters because those are
/// verified against 5,080,000 vectors and should not be disturbed to add
/// bookkeeping.
///
/// Counts **accesses only** — see the module doc's last section.
pub struct AccessCost<'a> {
    inner: &'a mut dyn CpuBus,
    fast_rom: bool,
    /// Master cycles accumulated across every access so far.
    pub master_cycles: u64,
    /// Number of accesses, so callers can tell "no accesses" from
    /// "accesses that happened to be free" — nothing is free here, so a
    /// zero cost with a non-zero count would be a bug.
    pub accesses: u64,
}

impl<'a> AccessCost<'a> {
    pub fn new(inner: &'a mut dyn CpuBus, fast_rom: bool) -> Self {
        Self {
            inner,
            fast_rom,
            master_cycles: 0,
            accesses: 0,
        }
    }

    fn charge(&mut self, addr: u32) {
        self.master_cycles += u64::from(access_cycles(addr, self.fast_rom));
        self.accesses += 1;
    }
}

impl CpuBus for AccessCost<'_> {
    fn read(&mut self, addr: u32) -> u8 {
        self.charge(addr);
        self.inner.read(addr)
    }

    fn write(&mut self, addr: u32, value: u8) {
        self.charge(addr);
        self.inner.write(addr, value);
    }

    /// Deliberately **not** charged.
    ///
    /// `peek` is the side-effect-free read a debugger or tracer uses; it
    /// is not a bus access the CPU performed. Charging it would let
    /// opening a memory viewer change the machine's timing — the same
    /// class of bug the trait's own doc warns about for `read`.
    fn peek(&self, addr: u32) -> u8 {
        self.inner.peek(addr)
    }
}
