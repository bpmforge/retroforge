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

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::{CoreSink, EmulatorCore, ResetKind, Step};

    struct Quiet;
    impl CoreSink for Quiet {
        fn video_scanline(&mut self, _y: u16, _p: &[rf_core_api::PpuPixel]) {}
        fn audio(&mut self, _s: &[i16]) {}
        fn event(&mut self, _e: rf_core_api::CoreEvent) {}
    }

    /// 16 KiB NROM that counts forever (`INX ; JMP $C000`).
    fn counting_nrom() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"NES\x1a");
        data.extend_from_slice(&[1, 1]);
        data.extend_from_slice(&[0u8; 10]);
        let mut prg = vec![0xEAu8; 16 * 1024];
        prg[0..4].copy_from_slice(&[0xE8, 0x4C, 0x00, 0xC0]);
        prg[0x3FFC..0x3FFE].copy_from_slice(&0xC000u16.to_le_bytes());
        data.extend(prg);
        data.extend(vec![0u8; 8 * 1024]);
        data
    }

    fn core() -> NesCore {
        let mut c = NesCore::from_ines_bytes(&counting_nrom()).expect("loads");
        c.reset(ResetKind::Hard);
        c
    }

    /// Two identical cores agree; a changed byte is named; the instruction
    /// pass finds the first instruction after which the cores differ; a bundle
    /// written for it holds the harness's files and says it is a live session.
    #[test]
    fn the_borrowed_comparison_finds_and_names_a_difference() {
        let (mut a, mut b) = (core(), core());
        let input = InputFrame::empty();
        for _ in 0..3 {
            a.run_frame(&input, &mut Quiet);
            b.run_frame(&input, &mut Quiet);
        }
        assert!(compare_frame(CoreRef::Nes(&a), CoreRef::Nes(&b)).is_empty());
        let before = (a.bus().master_cycle(), b.bus().master_cycle());
        assert_eq!(before.0, before.1);
        // The fine pass on a frame where one side has a poisoned byte.
        use rf_nes::CpuBus as _;
        b.bus_mut().write(0x0010, 0xAB);
        let fine = fine_pass(CoreMut::Nes(&mut a), CoreMut::Nes(&mut b), &input)
            .expect("the first instruction already differs");
        assert_eq!(fine.instruction, 0);
        assert!(fine
            .fields
            .iter()
            .any(|f| f.field == "wram" || f.field == "cycles"));
        // A frame boundary later the difference is in the frame comparison.
        while !a.step(Step::Frame, &mut Quiet).frame_complete {}
        while !b.step(Step::Frame, &mut Quiet).frame_complete {}
        let diffs = compare_frame(CoreRef::Nes(&a), CoreRef::Nes(&b));
        assert!(diffs.iter().any(|d| d.field == "wram"), "{diffs:?}");
        // Mixed consoles never compare equal.
        let s = rf_snes::core::SnesCore::load(&[0u8; 0x8000]).ok();
        if let Some(s) = s {
            assert_eq!(
                compare_frame(CoreRef::Nes(&a), CoreRef::Snes(&s))[0].field,
                "console"
            );
        }

        let dir = std::env::temp_dir().join(format!("rf-shadow-repro-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let raw = counting_nrom();
        let written = write_repro(
            &dir,
            &Repro {
                raw: &raw,
                console: ReproConsole::Nes,
                inputs: &[input, input],
                first_frame: 40,
                frame: 41,
                fields: diffs,
                fine: Some(fine),
                state: state_bytes(CoreRef::Nes(&a), &raw),
                trace: Vec::new(),
                wram_plain: wram(CoreRef::Nes(&a)),
                wram_rebuilt: wram(CoreRef::Nes(&b)),
                trace_len: 256,
            },
        );
        assert!(written.errors.is_empty(), "{:?}", written.errors);
        for f in [
            "README.txt",
            "inputs.txt",
            "diff.txt",
            "trace.txt",
            "wram_plain.bin",
        ] {
            assert!(dir.join(f).is_file(), "{f}");
        }
        let inputs = std::fs::read_to_string(dir.join("inputs.txt")).expect("text");
        assert!(inputs.contains("41 0000 0000  <-- diverging"), "{inputs}");
        assert!(inputs.contains("40 0000 0000\n"), "{inputs}");
        let readme = std::fs::read_to_string(dir.join("README.txt")).expect("text");
        assert!(
            readme.contains("W37-04") && readme.contains("live session"),
            "{readme}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
