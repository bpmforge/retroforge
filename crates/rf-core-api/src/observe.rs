//! Per-instruction observation for the Rebuild lab's understand pass
//! (ticket W36-09; `docs/design/REBUILD_LAB.md` section 5, step 5).
//!
//! The understand pass needs to know, for every executed instruction, which
//! atlas function it belongs to and which CPU-bus accesses it made, so that a
//! function can be described by what it does to the hardware. Neither exists
//! on the normal frame path and neither is paid for there:
//!
//! - the bus accesses ride on [`crate::WatchTable::set_observe`]: the cores
//!   already test "is anything watching" before they look at an access, and
//!   turning the log on makes that test true;
//! - the executing function comes from the block executor's trace ring.
//!
//! [`crate::EmulatorCore::run_frame_observed`] runs one frame exactly as
//! `run_frame` does (same instructions, same order) and calls a
//! [`StepObserver`] after each instruction. It is observation only: it must
//! leave the machine bit-identical to `run_frame`, and each core has a test
//! that says so.
//!
//! What is logged is the **CPU's view of the bus**: opcode and operand
//! fetches, data reads and writes, stack traffic and DMA bytes the CPU bus
//! carries. Reads have no side effect beyond the one the hardware already
//! has (a status register read clears its flag whether anyone logs it or not).

/// One CPU-bus access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusAccess {
    /// The address the CPU put on the bus (24-bit on the 65816).
    pub addr: u32,
    /// The byte read, or the byte being written.
    pub value: u8,
    pub write: bool,
}

/// What the block executor knows about the instruction that just ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StepFacts {
    /// The step was an NMI/IRQ sequence rather than an instruction.
    pub interrupt: bool,
    /// Atlas function owning the instruction (the first one, when several
    /// share its block); `None` when the atlas has no node for it, or for an
    /// interrupt sequence.
    pub function: Option<u32>,
    /// The function whose first instruction this was, if it was one.
    pub entry_of: Option<u32>,
}

/// Called by [`crate::EmulatorCore::run_frame_observed`] after every
/// instruction (or interrupt sequence).
pub trait StepObserver {
    fn step(&mut self, facts: StepFacts, accesses: &[BusAccess]);
    /// The frame is over (called once per observed frame).
    fn frame_end(&mut self) {}
}
