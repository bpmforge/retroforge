//! Addressing-mode bus sequences.
//!
//! Every method here issues exactly the reads/writes nesdev.org/6502_cpu.txt
//! specifies for that addressing mode, in order, including the dummy
//! accesses (zero-page-indexed's unindexed probe, indexed-absolute's
//! possibly-wrong-page probe, (zp),Y's same). Two variants exist for the
//! indexed absolute/indirect-indexed modes:
//!
//! - `*_read` — used by read-only instructions (LDA, ADC, CMP, ...): takes
//!   the extra re-read cycle **only** when the index crosses a page
//!   boundary (6502_cpu.txt "Absolute indexed addressing / Read
//!   instructions").
//! - `*_addr_slow` — used by write and read-modify-write instructions
//!   (STA, ASL, ...): **always** takes the extra cycle, because the
//!   processor cannot undo a write to a not-yet-page-corrected address, so
//!   it unconditionally reads from the (possibly wrong) address first
//!   before committing to the real one (6502_cpu.txt: "the processor
//!   cannot undo a write to an invalid address, it always reads from the
//!   address first").
use super::bus::CpuBus;
use super::Cpu;

impl Cpu {
    /// Zero-page operand address: one byte, no index.
    pub(super) fn am_zp_addr(&mut self, bus: &mut dyn CpuBus) -> u16 {
        self.fetch(bus) as u16
    }

    pub(super) fn am_zp_read(&mut self, bus: &mut dyn CpuBus) -> u8 {
        let addr = self.am_zp_addr(bus);
        bus.read(addr)
    }

    /// Zero-page indexed address (`zp,X` or `zp,Y`): the index never
    /// carries out of the zero page (6502_cpu.txt: "never fix the page
    /// address on crossing the zero page boundary").
    pub(super) fn am_zp_indexed_addr(&mut self, bus: &mut dyn CpuBus, index: u8) -> u16 {
        let base = self.fetch(bus);
        bus.read(base as u16); // dummy: "read from address, add index register to it"
        base.wrapping_add(index) as u16
    }

    /// Absolute operand address: two bytes, little-endian, no index.
    pub(super) fn am_abs_addr(&mut self, bus: &mut dyn CpuBus) -> u16 {
        let lo = self.fetch(bus);
        let hi = self.fetch(bus);
        u16::from_le_bytes([lo, hi])
    }

    pub(super) fn am_abs_read(&mut self, bus: &mut dyn CpuBus) -> u8 {
        let addr = self.am_abs_addr(bus);
        bus.read(addr)
    }

    /// Absolute indexed, read variant: extra cycle only on page cross.
    pub(super) fn am_absi_read(&mut self, bus: &mut dyn CpuBus, index: u8) -> u8 {
        let lo = self.fetch(bus);
        let hi = self.fetch(bus);
        let base = u16::from_le_bytes([lo, hi]);
        let (partial_lo, crossed) = lo.overflowing_add(index);
        let partial = u16::from_le_bytes([partial_lo, hi]);
        let value = bus.read(partial);
        if crossed {
            bus.read(base.wrapping_add(index as u16))
        } else {
            value
        }
    }

    /// Absolute indexed, unconditional-extra-cycle variant (write/RMW).
    pub(super) fn am_absi_addr_slow(&mut self, bus: &mut dyn CpuBus, index: u8) -> u16 {
        let lo = self.fetch(bus);
        let hi = self.fetch(bus);
        let base = u16::from_le_bytes([lo, hi]);
        let partial_lo = lo.wrapping_add(index);
        let partial = u16::from_le_bytes([partial_lo, hi]);
        bus.read(partial); // dummy, unconditional
        base.wrapping_add(index as u16)
    }

    /// `(zp,X)` indexed-indirect effective address. The pointer fetch never
    /// leaves the zero page (6502_cpu.txt: "the zero page boundary crossing
    /// is not handled").
    pub(super) fn am_indx_addr(&mut self, bus: &mut dyn CpuBus) -> u16 {
        let ptr = self.fetch(bus);
        bus.read(ptr as u16); // dummy: "read from the address, add X to it"
        let ptr_x = ptr.wrapping_add(self.x);
        let lo = bus.read(ptr_x as u16);
        let hi = bus.read(ptr_x.wrapping_add(1) as u16);
        u16::from_le_bytes([lo, hi])
    }

    pub(super) fn am_indx_read(&mut self, bus: &mut dyn CpuBus) -> u8 {
        let addr = self.am_indx_addr(bus);
        bus.read(addr)
    }

    /// `(zp),Y` indirect-indexed, read variant: extra cycle only on page
    /// cross.
    pub(super) fn am_indy_read(&mut self, bus: &mut dyn CpuBus) -> u8 {
        let ptr = self.fetch(bus);
        let lo = bus.read(ptr as u16);
        let hi = bus.read(ptr.wrapping_add(1) as u16);
        let base = u16::from_le_bytes([lo, hi]);
        let (partial_lo, crossed) = lo.overflowing_add(self.y);
        let partial = u16::from_le_bytes([partial_lo, hi]);
        let value = bus.read(partial);
        if crossed {
            bus.read(base.wrapping_add(self.y as u16))
        } else {
            value
        }
    }

    /// `(zp),Y`, unconditional-extra-cycle variant (write).
    pub(super) fn am_indy_addr_slow(&mut self, bus: &mut dyn CpuBus) -> u16 {
        let ptr = self.fetch(bus);
        let lo = bus.read(ptr as u16);
        let hi = bus.read(ptr.wrapping_add(1) as u16);
        let base = u16::from_le_bytes([lo, hi]);
        let partial_lo = lo.wrapping_add(self.y);
        let partial = u16::from_le_bytes([partial_lo, hi]);
        bus.read(partial); // dummy, unconditional
        base.wrapping_add(self.y as u16)
    }

    /// Read-modify-write: read the old value, write it back unmodified
    /// (the real hardware side effect this exists to model — e.g. LSR
    /// $D019 acknowledging a C64 CIA interrupt on the write-back), then
    /// write the new value computed by `op`.
    pub(super) fn rmw(&mut self, bus: &mut dyn CpuBus, addr: u16, op: fn(&mut Cpu, u8) -> u8) {
        let old = bus.read(addr);
        bus.write(addr, old);
        let new = op(self, old);
        bus.write(addr, new);
    }
}
