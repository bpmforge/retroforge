//! Superset decode: every PRG offset taken as a candidate instruction start
//! (ticket W36-02).
//!
//! Where the recording never went, the bytes are unclassified: code, data
//! and the middle of other instructions all look the same. The superset
//! view decodes *every* offset independently, so no alignment choice is
//! baked in; the atlas builder then claims only the offsets that static
//! control flow actually reaches. This is a flat pass over the image
//! (`map` over offsets), so it terminates by construction regardless of
//! content: nothing in it follows a branch.

use rf_core_api::coverage::CoverageLog;

use crate::isa::{DecodeMode, Isa};

/// Per-offset decode lengths for never-executed bytes.
pub struct Superset {
    /// `len[i]` is the length of the instruction that decodes at `i`, or 0
    /// when `i` was executed (the recording is authoritative there) or too
    /// few bytes remain.
    pub len: Vec<u8>,
}

impl Superset {
    /// Offsets at which an instruction decodes and the recording did not
    /// execute the byte.
    #[must_use]
    pub fn candidates(&self) -> usize {
        self.len.iter().filter(|&&l| l != 0).count()
    }
}

/// Decode every never-executed offset of `prg`. `log` may be shorter or
/// longer than `prg`; missing flags read as unexecuted.
#[must_use]
pub fn superset_decode(isa: &dyn Isa, prg: &[u8], log: &CoverageLog) -> Superset {
    let max = isa.max_len().max(1);
    let len = (0..prg.len())
        .map(|i| {
            if log.prg_flags.get(i).copied().unwrap_or(0) != 0 {
                return 0;
            }
            // The CPU address only matters to relative-branch targets, which
            // this view does not look at.
            let end = (i + max).min(prg.len());
            isa.decode(&prg[i..end], 0, DecodeMode::default())
                .map_or(0, |d| d.len)
        })
        .collect();
    Superset { len }
}
