//! Counters of the block executor (ticket W36-03; `docs/design/REBUILD_LAB.md`
//! section 4). Plain data so the shell and debugger can show them without
//! seeing a console core or `rf-rebuild`.

/// What the executor has seen since the atlas was installed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RebuildStats {
    /// Instructions found in the atlas that the recording had executed.
    pub observed: u64,
    /// Instructions found in the atlas by static decode only.
    pub inferred: u64,
    /// Instructions with no atlas node (RAM code, unmapped, never decoded):
    /// plain interpreter.
    pub fallback: u64,
    /// Of those: the offset is in the atlas, but only in other decode modes
    /// (65816 widths; ticket W36-08b). Always 0 on the 6502.
    pub fallback_width: u64,
    /// Of those: ROM code the atlas has no node for in any mode.
    pub fallback_no_node: u64,
    /// Of those: code fetched from RAM (NES: RAM and registers; SNES: work
    /// RAM). The atlas never holds volatile code.
    pub fallback_ram: u64,
    /// Of those: code fetched from memory a coprocessor owns, or any other
    /// non-ROM, non-RAM space.
    pub fallback_other: u64,
    /// Of those: the node's function was demoted by the shadow-core check
    /// (ticket W37-04), so the plain interpreter ran it.
    pub fallback_demoted: u64,
    /// Of those: the first block run after a savestate load (ticket W37-07).
    /// A load always resumes on the plain interpreter for one block, then
    /// dispatch re-enters the atlas at the next block leader.
    pub fallback_after_load: u64,
    /// Hardware interrupt entries (NMI/IRQ sequences, not instructions).
    pub interrupt_entries: u64,
    /// Indirect transfers (`JMP (a)`, `RTS`, `RTI`, `BRK`) compared against
    /// the atlas's recorded targets.
    pub indirect_checked: u64,
    /// Of those, targets the atlas had not recorded. Behaviour is never
    /// changed by a miss.
    pub dispatch_misses: u64,
    /// Distinct (site, target) misses held in the session delta list.
    pub delta_len: u32,
    /// Distinct misses dropped because the delta list was full.
    pub delta_dropped: u64,
    /// Functions in the installed atlas.
    pub atlas_functions: u32,
}

impl RebuildStats {
    /// Instructions run through the executor (atlas hits plus fallback).
    #[must_use]
    pub fn instructions(&self) -> u64 {
        self.observed + self.inferred + self.fallback
    }

    /// Share of instructions that had no atlas node, `0.0..=1.0`.
    #[must_use]
    pub fn fallback_rate(&self) -> f64 {
        match self.instructions() {
            0 => 0.0,
            n => self.fallback as f64 / n as f64,
        }
    }
}
