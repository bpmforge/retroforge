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
//! The real system bus (mappers, PPU/APU registers, OAM DMA) is
//! [`crate::system::NesBus`] (ticket W1-02); this module only defines the
//! seam plus a recording mock for the vector-test harness (see
//! `cpu::tests::vectors`). `NesBus` is exactly the "future `CpuBus`
//! implementor" the module doc above describes: OAM DMA's stolen cycles
//! and the master cycle counter both live there, advanced inside its
//! `read`/`write`, never inside `Cpu::step`.
pub trait CpuBus {
    /// One CPU read cycle at `addr`. Called for every read, including
    /// "dummy" reads whose value is discarded (e.g. absolute,X page-cross
    /// probe, RMW's unmodified-value write-back precursor).
    fn read(&mut self, addr: u16) -> u8;

    /// One CPU write cycle at `addr`. Called for every write, including
    /// the RMW "write back the unmodified value" cycle.
    fn write(&mut self, addr: u16, value: u8);

    /// Whether the NMI line is currently **asserted** (ticket W1-01b).
    /// This is the logical/asserted state, not the raw electrical level —
    /// NMI is active-low on real hardware (nesdev.org/wiki/CPU_interrupts:
    /// "reacts to high-to-low transitions"), but callers here only ever
    /// need "is a non-maskable interrupt being requested right now", so
    /// this method returns `true` for that. [`Cpu`](super::Cpu) polls this
    /// once per bus cycle and edge-detects the false->true transition
    /// itself (see `cpu/exec.rs`'s `CountingBus`).
    ///
    /// Defaults to `false` (never asserted) so existing `CpuBus`
    /// implementors — `RecordingBus`/`SinkBus` in `cpu::tests`, and any
    /// bus that predates interrupt support — keep compiling unchanged; a
    /// real system bus (W1-02+) overrides this to reflect the PPU's NMI
    /// output and any other NMI sources.
    fn nmi_line(&self) -> bool {
        false
    }

    /// Whether an NMI **edge** has been latched and not yet serviced —
    /// the signal the `BRK`/`IRQ` hijack decision must use (ticket W2-20).
    ///
    /// This exists because [`CpuBus::nmi_line`] is the wrong question at
    /// that decision point. NMI is edge-triggered, but the NES holds the
    /// line asserted for the whole of vblank (until `$2002` is read or
    /// `PPUCTRL` bit 7 is cleared), so a *level* test stays true long
    /// after the edge it represents has been serviced — and a `BRK`
    /// executed anywhere in that window would be hijacked by an interrupt
    /// that already ran. blargg's `cpu_interrupts_v2` `2-nmi_and_brk`
    /// measures exactly this: its NMI handler never touches `$2002`, so
    /// the line is still asserted when the following `BRK` runs, and a
    /// level-based hijack turns its first three delay steps from
    /// "NMI, then BRK" into "BRK hijacked", widening the ROM's 5-clock
    /// hijack window to 8.
    ///
    /// Defaults to [`CpuBus::nmi_line`]: a bus with no edge detector of
    /// its own can only offer the level, and for the mock buses in
    /// `cpu::tests` (which assert the line for exactly the instruction
    /// under test) the two coincide. `cpu/exec.rs`'s `CountingBus`, the
    /// only implementor that runs a real machine, overrides this with its
    /// own sticky edge latch.
    fn nmi_edge_pending(&self) -> bool {
        self.nmi_line()
    }

    /// Whether the IRQ line is currently **asserted** (same
    /// asserted-not-electrical convention as [`CpuBus::nmi_line`]). IRQ is
    /// level-sensitive (nesdev.org/wiki/CPU_interrupts: "reacts to a low
    /// signal level") and shared by every IRQ source on the real bus (APU
    /// frame counter/DMC, mapper IRQs, ...) — this method reports whether
    /// *any* of them currently hold the line low; a real bus ORs its
    /// sources together here. Defaults to `false` for the same
    /// mock-bus-compatibility reason as `nmi_line`.
    fn irq_line(&self) -> bool {
        false
    }
}
