//! Superset decode: every PRG offset taken as a candidate instruction start
//! in every decode mode (tickets W36-02, W36-08).
//!
//! Where the recording never went, the bytes are unclassified: code, data
//! and the middle of other instructions all look the same. The superset
//! view decodes *every* offset independently, so no alignment choice is
//! baked in; the atlas builder then claims only the offsets that static
//! control flow actually reaches. This is a flat pass over the image
//! (`map` over offsets and modes), so it terminates by construction
//! regardless of content: nothing in it follows a branch. On the 65816 the
//! pass runs once per width mode, because the same bytes are a different
//! instruction stream in each.

use rf_core_api::coverage::CoverageLog;

use crate::isa::{DecodeMode, Isa};

/// Per-offset, per-mode decode lengths for never-executed bytes.
pub struct Superset {
    /// `len[i * modes + m]` is the length of the instruction that decodes at
    /// `i` in mode `m`, or 0 when `i` was executed (the recording is
    /// authoritative there) or too few bytes remain.
    pub len: Vec<u8>,
    /// Modes per offset ([`Isa::mode_count`]).
    pub modes: usize,
}

impl Superset {
    /// `(offset, mode)` pairs at which an instruction decodes and the
    /// recording did not execute the byte.
    #[must_use]
    pub fn candidates(&self) -> usize {
        self.len.iter().filter(|&&l| l != 0).count()
    }

    /// Decode length at `offset` in `mode`, 0 for none.
    #[must_use]
    pub fn at(&self, offset: usize, mode: usize) -> u8 {
        self.len
            .get(offset * self.modes + mode)
            .copied()
            .unwrap_or(0)
    }
}

/// Decode every never-executed offset of `prg` in every mode. `log` may be
/// shorter or longer than `prg`; missing flags read as unexecuted.
#[must_use]
pub fn superset_decode(isa: &dyn Isa, prg: &[u8], log: &CoverageLog) -> Superset {
    let max = isa.max_len().max(1);
    let modes = isa.mode_count().max(1);
    let mut len = Vec::with_capacity(prg.len() * modes);
    for i in 0..prg.len() {
        let executed = log.prg_flags.get(i).copied().unwrap_or(0) != 0;
        let end = (i + max).min(prg.len());
        // The CPU address only matters to branch targets, which this view
        // does not look at.
        let cpu = if isa.fixed_mapping() {
            isa.cpu_for_rom(i as u32)
        } else {
            0
        };
        for m in 0..modes {
            len.push(if executed {
                0
            } else {
                isa.decode(&prg[i..end], cpu, DecodeMode(m as u8))
                    .map_or(0, |d| d.len)
            });
        }
    }
    Superset { len, modes }
}
