//! CPU half of the `CPU_` chunk (ticket W2-04). See `crate::state`'s module
//! doc for the encoding rules and for why completeness is enforced by an
//! exhaustive destructure rather than by review.

use rf_core_api::StateError;

use crate::cpu::{Cpu, UnstableOp};
use crate::state::{StateIn, StateOut};

impl Cpu {
    /// Writes the register file and every interrupt latch, in declaration
    /// order.
    pub(crate) fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        // Exhaustive, no `..`: a new CPU field must be handled here or the
        // build breaks (crate::state module doc).
        let Cpu {
            a,
            x,
            y,
            s,
            pc,
            p,
            unstable_op,
            jammed,
            nmi_prev_asserted,
            nmi_edge_latched,
            pending_nmi_after,
            pending_irq_after,
            i_flag_poll_snapshot,
            nmi_hijack_consumed,
        } = self;

        out.u8(*a)?;
        out.u8(*x)?;
        out.u8(*y)?;
        out.u8(*s)?;
        out.u16(*pc)?;
        out.u8(*p)?;
        out.u8(encode_unstable_op(*unstable_op))?;
        out.bool(*jammed)?;
        out.bool(*nmi_prev_asserted)?;
        out.bool(*nmi_edge_latched)?;
        out.bool(*pending_nmi_after)?;
        out.bool(*pending_irq_after)?;
        out.bool(*i_flag_poll_snapshot)?;
        out.bool(*nmi_hijack_consumed)
    }

    /// Restores what [`Cpu::save_state`] wrote, in the same order.
    pub(crate) fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.a = inp.u8()?;
        self.x = inp.u8()?;
        self.y = inp.u8()?;
        self.s = inp.u8()?;
        self.pc = inp.u16()?;
        self.p = inp.u8()?;
        self.unstable_op = decode_unstable_op(inp.u8()?)?;
        self.jammed = inp.bool()?;
        self.nmi_prev_asserted = inp.bool()?;
        self.nmi_edge_latched = inp.bool()?;
        self.pending_nmi_after = inp.bool()?;
        self.pending_irq_after = inp.bool()?;
        self.i_flag_poll_snapshot = inp.bool()?;
        self.nmi_hijack_consumed = inp.bool()?;
        Ok(())
    }
}

/// `None` is 0 so the common case encodes as a zero byte; the variants are
/// numbered from 1 in declaration order and that numbering is part of the
/// wire format (`docs/design/CONTRACTS.md` §3 — the container is a public
/// commitment from first release), so entries may be appended but never
/// renumbered.
fn encode_unstable_op(op: Option<UnstableOp>) -> u8 {
    match op {
        None => 0,
        Some(UnstableOp::Ane) => 1,
        Some(UnstableOp::Lxa) => 2,
        Some(UnstableOp::Sha) => 3,
        Some(UnstableOp::Shx) => 4,
        Some(UnstableOp::Shy) => 5,
        Some(UnstableOp::Tas) => 6,
        Some(UnstableOp::Las) => 7,
    }
}

fn decode_unstable_op(v: u8) -> Result<Option<UnstableOp>, StateError> {
    Ok(match v {
        0 => None,
        1 => Some(UnstableOp::Ane),
        2 => Some(UnstableOp::Lxa),
        3 => Some(UnstableOp::Sha),
        4 => Some(UnstableOp::Shx),
        5 => Some(UnstableOp::Shy),
        6 => Some(UnstableOp::Tas),
        7 => Some(UnstableOp::Las),
        other => {
            return Err(StateError::Corrupt(format!(
                "unknown UnstableOp discriminant {other} in CPU_ chunk"
            )))
        }
    })
}
