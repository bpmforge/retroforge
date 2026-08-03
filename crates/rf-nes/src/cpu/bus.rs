//! The CPU-side bus seam ([`CpuBus`]).
//!
//! [`Cpu`](super::Cpu) never touches memory directly — every access, dummy
//! reads and dummy writes included, goes through one of these two methods,
//! in the exact order and at the exact address hardware would use
//! (EMULATION_CORES.md §2.1: "cycle-stepped... not 'execute then add N
//! cycles'"). This is what makes the CPU cycle-accurate rather than merely
//! instruction-accurate: a wrong dummy read is a wrong bus trace even if
//! every register ends up correct.
//!
//! A real system bus (mappers, PPU/APU registers, OAM DMA) is a later
//! ticket (W1-02); this crate only defines the seam plus a recording mock
//! for the vector-test harness (see `cpu::tests::vectors`).
pub trait CpuBus {
    /// One CPU read cycle at `addr`. Called for every read, including
    /// "dummy" reads whose value is discarded (e.g. absolute,X page-cross
    /// probe, RMW's unmodified-value write-back precursor).
    fn read(&mut self, addr: u16) -> u8;

    /// One CPU write cycle at `addr`. Called for every write, including
    /// the RMW "write back the unmodified value" cycle.
    fn write(&mut self, addr: u16, value: u8);
}
