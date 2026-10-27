//! The lockstep comparison as a library for a live session (ticket W37-04;
//! `docs/design/REBUILD_LAB.md`, "W37-04: shadow-core lockstep during play").
//!
//! [`crate::lockstep::run`] owns both cores and replays from power-on. The
//! app's shadow check owns neither: its primary core is the one the player is
//! using and its shadow is a plain core stepped beside it. So this module
//! hands out the same pieces on borrowed cores, with nothing new decided
//! here: the frame comparison ([`compare_frame`]), the instruction-level
//! re-run ([`fine_pass`]), and the repro bundle ([`write_repro`]).
//!
//! **What the live comparison does not hash.** The production sink has no
//! per-pixel tap, so the picture and sound hashes the harness compares are
//! not part of it (both sides are given the same hash). The machine state it
//! compares covers every register, memory and device region the harness
//! compares, and the picture and sound are functions of that state; only a
//! difference that never reaches state would be missed.
//!
//! Layering: rf-harness is allowed to name `rf-nes` and `rf-snes`
//! (`scripts/validate-arch.sh` rule 3); the app crate reaches them through
//! this module's two enums rather than adding the console-specific code
//! twice.

use std::path::{Path, PathBuf};

use rf_core_api::InputFrame;
use rf_nes::core::NesCore;
use rf_nes::rebuild::TraceEntry;
use rf_snes::core::SnesCore;

use crate::lockstep::{
    fine_pass as fine_pass_generic, scalar, write_bundle_shaped, BundleShape, Divergence, Hashes,
    Investigation, LockstepMachine,
};
pub use crate::lockstep::{BundleFiles, FieldDiff, FineDivergence};

/// Hashes that are equal on both sides, so the picture and sound compare as
/// "not measured" (see the module doc).
const NO_HASH: Hashes = Hashes { video: 0, audio: 0 };

/// A core to read.
#[derive(Clone, Copy)]
pub enum CoreRef<'a> {
    Nes(&'a NesCore),
    Snes(&'a SnesCore),
}

/// A core to step.
pub enum CoreMut<'a> {
    Nes(&'a mut NesCore),
    Snes(&'a mut SnesCore),
}

/// The fields that differ between a plain core and a rebuilt one at a frame
/// boundary; empty when they agree. Two cores of different consoles differ in
/// a `console` field.
#[must_use]
pub fn compare_frame(plain: CoreRef<'_>, rebuilt: CoreRef<'_>) -> Vec<FieldDiff> {
    match (plain, rebuilt) {
        (CoreRef::Nes(p), CoreRef::Nes(r)) => NesCore::compare_frame(p, r, NO_HASH, NO_HASH),
        (CoreRef::Snes(p), CoreRef::Snes(r)) => SnesCore::compare_frame(p, r, NO_HASH, NO_HASH),
        _ => {
            let mut out = Vec::new();
            scalar(&mut out, "console", 0u8, 1u8);
            out
        }
    }
}

/// Step both cores one instruction at a time through one frame (both are at
/// the same frame boundary and `input` is the frame's input); the first
/// instruction after which registers, cycle count or work RAM differ is the
/// answer. `None`: no such instruction in the frame (a difference in another
/// memory, or none), or the cores are of different consoles.
pub fn fine_pass(
    plain: CoreMut<'_>,
    rebuilt: CoreMut<'_>,
    input: &InputFrame,
) -> Option<FineDivergence> {
    match (plain, rebuilt) {
        (CoreMut::Nes(p), CoreMut::Nes(r)) => fine_pass_generic(p, r, input, None),
        (CoreMut::Snes(p), CoreMut::Snes(r)) => fine_pass_generic(p, r, input, None),
        _ => None,
    }
}

/// The shipped `.rfstate` bytes of `core` at a frame boundary (the bundle's
/// state file). `raw` is the cartridge image.
///
/// # Errors
/// The core refused to serialise (not at a frame boundary), or the image's
/// hash was unreadable.
pub fn state_bytes(core: CoreRef<'_>, raw: &[u8]) -> Result<Vec<u8>, String> {
    match core {
        CoreRef::Nes(c) => c.snapshot(raw),
        CoreRef::Snes(c) => c.snapshot(raw),
    }
}

/// Work RAM of `core`.
#[must_use]
pub fn wram(core: CoreRef<'_>) -> Vec<u8> {
    match core {
        CoreRef::Nes(c) => LockstepMachine::wram(c),
        CoreRef::Snes(c) => LockstepMachine::wram(c),
    }
}

/// The last executed steps of a rebuilt core (needs the trace ring on).
#[must_use]
pub fn trace(core: CoreRef<'_>) -> Vec<TraceEntry> {
    match core {
        CoreRef::Nes(c) => c.rebuild_trace(),
        CoreRef::Snes(c) => c.rebuild_trace(),
    }
}

/// Everything a live repro bundle holds.
pub struct Repro<'a> {
    /// The cartridge image (its hash goes in the README; no bytes are written).
    pub raw: &'a [u8],
    pub console: ReproConsole,
    /// Recent input, oldest first; the last entry is the diverging frame's.
    pub inputs: &'a [InputFrame],
    /// The session frame number of `inputs[0]`.
    pub first_frame: u32,
    /// The session frame number of the diverging frame.
    pub frame: u32,
    pub fields: Vec<FieldDiff>,
    pub fine: Option<FineDivergence>,
    /// The plain core at the start of the diverging frame ([`state_bytes`]).
    pub state: Result<Vec<u8>, String>,
    pub trace: Vec<TraceEntry>,
    pub wram_plain: Vec<u8>,
    pub wram_rebuilt: Vec<u8>,
    /// Steps the rebuilt core's trace ring keeps (README text only).
    pub trace_len: usize,
}

/// Which console a bundle is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReproConsole {
    Nes,
    Snes,
}

/// What [`write_repro`] wrote.
#[derive(Debug)]
pub struct ReproWritten {
    pub bundle: BundleFiles,
    /// Why a part could not be written, if any.
    pub errors: Vec<String>,
}

/// Write the repro bundle for a live divergence into `dir`: the same files
/// and format as a harness run's (see [`crate::lockstep`]), marked as a live
/// session in its README.
pub fn write_repro(dir: &Path, r: &Repro<'_>) -> ReproWritten {
    let d = Divergence {
        frame: r.frame,
        fields: r.fields.clone(),
        fine: r.fine.clone(),
    };
    let inv = Investigation {
        state: r.state.clone(),
        fine: r.fine.clone(),
        trace: r.trace.clone(),
        wram_plain: r.wram_plain.clone(),
        wram_rebuilt: r.wram_rebuilt.clone(),
    };
    let shape = BundleShape {
        first_frame: r.first_frame,
        input_tail: r.inputs.len(),
        trace_len: r.trace_len,
        fine: true,
        live: true,
    };
    let (files, errors) = match r.console {
        ReproConsole::Nes => write_bundle_shaped::<NesCore>(dir, r.raw, r.inputs, &d, &inv, &shape),
        ReproConsole::Snes => {
            write_bundle_shaped::<SnesCore>(dir, r.raw, r.inputs, &d, &inv, &shape)
        }
    };
    ReproWritten {
        bundle: BundleFiles {
            dir: PathBuf::from(dir),
            files,
        },
        errors,
    }
}
