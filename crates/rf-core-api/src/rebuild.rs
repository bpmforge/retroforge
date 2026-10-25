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
