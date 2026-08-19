//! Effective-address computation for the 65C816 (ticket W6-01a).
//!
//! ## The three rules that decide every address on this chip
//!
//! 1. **Which bank?** Absolute data accesses use `DBR`. Long accesses
//!    carry their own bank. Direct-page and stack accesses are *always*
//!    bank 0. Code fetches use `PBR`. A mode that used the wrong one
//!    reads a plausible byte from the wrong 64 KiB.
//! 2. **Does it wrap, and where?** Direct-page addressing wraps within
//!    bank 0 — `D = $FFFF` plus an offset does not spill into bank 1. But
//!    an *indexed absolute* access that crosses `$FFFF` **does** carry
//!    into the next bank, because it is a 24-bit add. These two are
//!    opposite and are the classic 65816 addressing bug.
//! 3. **In emulation mode, direct-page indexing wraps in the page.**
//!    With `DL = 0`, `$FF` indexed by 1 gives `$00`, not `$0100` — the
//!    6502 behaviour, preserved.
//!
//! Each function below states which of these it is applying, because the
//! answer is not guessable from the mode's name.

use super::{Cpu, CpuBus};

/// A resolved 24-bit effective address.
pub type Addr = u32;

/// Combine a bank and a 16-bit offset.
#[must_use]
pub fn bank(bank: u8, offset: u16) -> Addr {
    (u32::from(bank) << 16) | u32::from(offset)
}

/// Direct page: `D + offset`, **always bank 0**, wrapping within the
/// bank.
///
/// The wrap is why this returns `u16` arithmetic widened rather than a
/// 24-bit add: `D = $FFF0` with an offset of `$20` addresses `$00:0010`,
/// not `$01:0010`.
#[must_use]
pub fn direct(cpu: &Cpu, offset: u8) -> Addr {
    Addr::from(cpu.d.wrapping_add(u16::from(offset)))
}

/// Direct page indexed by X or Y.
///
/// **Emulation mode wraps within the page** when `DL == 0`: this is the
/// 6502's zero-page indexing, and software written for it relies on the
/// wrap. Native mode (or a non-zero `DL`) is a plain 16-bit add.
#[must_use]
pub fn direct_indexed(cpu: &Cpu, offset: u8, index: u16) -> Addr {
    if cpu.e && cpu.d & 0x00FF == 0 {
        let lo = offset.wrapping_add(index as u8);
        Addr::from(cpu.d | u16::from(lo))
    } else {
        Addr::from(cpu.d.wrapping_add(u16::from(offset)).wrapping_add(index))
    }
}

/// Absolute: 16-bit operand in the **data** bank.
#[must_use]
pub fn absolute(cpu: &Cpu, offset: u16) -> Addr {
    bank(cpu.dbr, offset)
}

/// Absolute indexed: `DBR:offset + index`, as a **24-bit** add.
///
/// This is the one that carries into the next bank, and it is the exact
/// opposite of direct-page's wrap. `DBR = $7E`, offset `$FFFF`, index 1
/// addresses `$7F:0000`.
#[must_use]
pub fn absolute_indexed(cpu: &Cpu, offset: u16, index: u16) -> Addr {
    (bank(cpu.dbr, offset) + u32::from(index)) & 0x00FF_FFFF
}

/// Absolute long: the operand carries its own bank, so `DBR` is ignored.
#[must_use]
pub fn long(addr: u32) -> Addr {
    addr & 0x00FF_FFFF
}

/// Absolute long indexed: a 24-bit add that wraps at the top of memory.
#[must_use]
pub fn long_indexed(addr: u32, index: u16) -> Addr {
    (addr.wrapping_add(u32::from(index))) & 0x00FF_FFFF
}

/// Stack relative: `SP + offset`, **always bank 0**.
#[must_use]
pub fn stack_relative(cpu: &Cpu, offset: u8) -> Addr {
    Addr::from(cpu.sp.wrapping_add(u16::from(offset)))
}

/// Read a 16-bit pointer from bank 0, wrapping within it.
///
/// Bank 0 and the wrap are both deliberate: every direct-page indirect
/// mode fetches its pointer from bank 0 regardless of `DBR`, and a
/// pointer straddling `$FFFF` wraps to `$0000` rather than reading
/// bank 1.
pub fn read_pointer16(bus: &mut dyn CpuBus, at: Addr) -> u16 {
    let lo = bus.read(at);
    let hi = bus.read(Addr::from((at as u16).wrapping_add(1)));
    u16::from(lo) | (u16::from(hi) << 8)
}

/// Read a 16-bit pointer **within whatever bank `at` names**.
///
/// Distinct from [`read_pointer16`], which forces bank 0 — correct for
/// every direct-page indirect mode and wrong for exactly two opcodes.
/// `JMP ($nnnn,X)` and `JSR ($nnnn,X)` take their pointer from the
/// *program* bank, so forcing the high byte to bank 0 fetched a plausible
/// low byte and a zero high byte: `pc: got 0x0011, want 0xA711`.
///
/// Deliberately a sibling rather than a change to `read_pointer16`: over
/// a hundred opcodes route through that function and rely on its bank-0
/// behaviour, so "fixing" it in place would have traded two failures for
/// a hundred.
pub fn read_pointer16_in_bank(bus: &mut dyn CpuBus, at: Addr) -> u16 {
    let b = (at >> 16) as u8;
    let lo = bus.read(at);
    let hi = bus.read(bank(b, (at as u16).wrapping_add(1)));
    u16::from(lo) | (u16::from(hi) << 8)
}

/// Read a 24-bit pointer from bank 0, wrapping within it.
pub fn read_pointer24(bus: &mut dyn CpuBus, at: Addr) -> u32 {
    let lo = bus.read(at);
    let mid = bus.read(Addr::from((at as u16).wrapping_add(1)));
    let hi = bus.read(Addr::from((at as u16).wrapping_add(2)));
    u32::from(lo) | (u32::from(mid) << 8) | (u32::from(hi) << 16)
}

/// `(dp)` — direct page indirect. The pointer lives in bank 0; the
/// resulting address is in the **data** bank.
pub fn direct_indirect(cpu: &Cpu, bus: &mut dyn CpuBus, offset: u8) -> Addr {
    let ptr = read_pointer16(bus, direct(cpu, offset));
    bank(cpu.dbr, ptr)
}

/// `(dp,X)` — indexed *before* the indirection: X selects which pointer.
pub fn direct_indexed_indirect(cpu: &Cpu, bus: &mut dyn CpuBus, offset: u8) -> Addr {
    let at = direct_indexed(cpu, offset, cpu.x);
    let ptr = read_pointer16(bus, at);
    bank(cpu.dbr, ptr)
}

/// `(dp),Y` — indexed *after* the indirection, and the add is 24-bit, so
/// it can carry into the next bank.
pub fn direct_indirect_indexed(cpu: &Cpu, bus: &mut dyn CpuBus, offset: u8) -> Addr {
    let ptr = read_pointer16(bus, direct(cpu, offset));
    (bank(cpu.dbr, ptr) + u32::from(cpu.y)) & 0x00FF_FFFF
}

/// `[dp]` — direct page indirect long. The pointer carries its own bank,
/// so `DBR` is ignored.
pub fn direct_indirect_long(cpu: &Cpu, bus: &mut dyn CpuBus, offset: u8) -> Addr {
    long(read_pointer24(bus, direct(cpu, offset)))
}

/// `[dp],Y` — indirect long, then indexed.
pub fn direct_indirect_long_indexed(cpu: &Cpu, bus: &mut dyn CpuBus, offset: u8) -> Addr {
    long_indexed(read_pointer24(bus, direct(cpu, offset)), cpu.y)
}

/// `(sr,S),Y` — stack relative indirect indexed.
pub fn stack_relative_indirect_indexed(cpu: &Cpu, bus: &mut dyn CpuBus, offset: u8) -> Addr {
    let ptr = read_pointer16(bus, stack_relative(cpu, offset));
    (bank(cpu.dbr, ptr) + u32::from(cpu.y)) & 0x00FF_FFFF
}

/// Read an 8- or 16-bit value, low byte first.
///
/// The high byte's address is a **24-bit** increment: a 16-bit read at
/// `$7E:FFFF` takes its high byte from `$7F:0000`. That matches the
/// indexed-absolute carry rule above, and differs from the direct-page
/// pointer wrap, which is why the two have separate helpers.
pub fn read_value(bus: &mut dyn CpuBus, at: Addr, eight: bool, wrap_bank0: bool) -> u16 {
    let lo = bus.read(at);
    if eight {
        return u16::from(lo);
    }
    u16::from(lo) | (u16::from(bus.read(next_byte(at, wrap_bank0))) << 8)
}

/// The address of the second byte of a 16-bit access — rule 2 of the
/// module doc, applied.
///
/// `wrap_bank0` is set for direct-page and stack-relative modes, whose
/// accesses live in bank 0 and stay there: with `D = $FFF8`, a 16-bit
/// read at offset `$07` takes its low byte from `$00:FFFF` and its high
/// byte from `$00:0000`. Every other mode does a true 24-bit increment
/// and carries into the next bank.
///
/// One case in 4,980,000 distinguished these (`c4 n 1616`, `CPY $07` with
/// `D = $FFF8`), which is a fair measure of how easy it is to write the
/// carrying version everywhere and never notice.
fn next_byte(at: Addr, wrap_bank0: bool) -> Addr {
    if wrap_bank0 {
        Addr::from((at as u16).wrapping_add(1))
    } else {
        (at + 1) & 0x00FF_FFFF
    }
}

/// Write an 8- or 16-bit value, low byte first.
pub fn write_value(bus: &mut dyn CpuBus, at: Addr, value: u16, eight: bool, wrap_bank0: bool) {
    bus.write(at, value as u8);
    if !eight {
        bus.write(next_byte(at, wrap_bank0), (value >> 8) as u8);
    }
}
