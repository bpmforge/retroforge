//! Shadow-core lockstep during play (ticket W37-04; `docs/design/REBUILD_LAB.md`,
//! "W37-04: shadow-core lockstep during play").
//!
//! While Rebuild is on and a game is not yet **proven**, a second, plain core
//! (no atlas) is stepped beside the one the player uses, right after it, with
//! the same input, on the same core thread. At the end of each frame the two
//! are compared by the harness's own lockstep comparison
//! (`rf_harness::shadow`). One difference anywhere:
//!
//! 1. is recorded in the game's [`VerifyRecord`] (it can never be proven now);
//! 2. is re-run from the frame's start on private copies, an instruction at a
//!    time, to find the executing function; that function is **demoted** (the
//!    executor hands its instructions to the plain interpreter from then on)
//!    and remembered per ROM, by entry key, so it stays demoted next session;
//! 3. is saved as a repro bundle (same files as a harness run's);
//! 4. resyncs the shadow from the player's core, so one fault is one report.
//!
//! All of it happens in the frame the difference appears in.
//!
//! **What stops it.** The check is *off* once the stored record says the game
//! is proven (`Verification::of`), and switches off the moment a checkpoint
//! makes it so. It is *suspended* while a hook script that can write memory is
//! loaded (the shadow has no hooks and the hook's writes are the player's
//! choice, not a rebuilt-code fault); it resumes from a fresh copy of the
//! player's core when the hook goes. Anything else that changes the player's
//! core behind the frame loop (a state load, a rewind, a memory-editor write,
//! a stepped frame, a reset) is healed the same way: the shadow is rebuilt
//! from a snapshot of the player's core before the next compared frame.
//! Run-ahead and the margin redraw run on copies of the player's core after
//! its real frame, so they never reach the player's timeline and the check
//! sees exactly the committed frames.
//!
//! **A run** is one session with the check on that compared at least
//! [`RUN_MIN_FRAMES`] frames (or saw a difference). A session folds into the
//! record every [`FLUSH_FRAMES`] compared frames, at each difference and when
//! the check ends, and every fold *replaces* the session's earlier
//! contribution, so a crash loses at most one interval and a long session is
//! one run, not many.
//!
//! **Cost.** One extra core step per frame, one machine snapshot of the shadow
//! (the next frame's start state, which is also what a bundle needs), one
//! frame comparison. Measured numbers are in the REBUILD_LAB as-built section.
//!
//! Law 4: nothing here changes the player's core except demoting a function in
//! its executor (behaviour-neutral by construction: it only moves instructions
//! to the interpreter the executor already equals).

use std::collections::VecDeque;
use std::path::PathBuf;

use rf_core_api::{CoreEvent, CoreSink, InputFrame, PpuPixel};
use rf_harness::shadow as hs;
#[cfg(test)]
use rf_rebuild::VerifyRecord;
use rf_rebuild::{FunctionKey, TraceKind, Verification};

use crate::rebuild::{read_record, repro_root, write_demoted, write_record};
use crate::stepper::{EmuStepper, MachineSnapshot};

/// Compared frames between checkpoints (10 s at 60 fps).
pub const FLUSH_FRAMES: u64 = 600;
/// Compared frames a session needs before it counts as a run toward
/// [`rf_rebuild::PROVEN_RUNS`] (a session that saw a difference counts at once:
/// the record is already spoiled).
pub const RUN_MIN_FRAMES: u64 = 600;
/// Repro bundles one session writes; later differences are counted only.
pub const MAX_BUNDLES: u32 = 8;
/// Frames of input a bundle holds.
const INPUT_TAIL: usize = 120;
/// Steps the private rebuilt copy keeps while looking for the function.
const TRACE_LEN: usize = 256;

/// Why the check is not comparing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuspendReason {
    /// A hook script that can write memory is loaded: a mod is active.
    WriteHook,
}

/// What the check is doing, for the card.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ShadowStatus {
    /// Not started (no atlas yet, or Rebuild failed).
    #[default]
    NotStarted,
    /// The game is proven: no shadow core was started, or it was stopped.
    Proven,
    /// Comparing; counts are this session's.
    Checking { frames: u64, divergences: u64 },
    /// A mod is active, so nothing is checked until it goes.
    Suspended {
        reason: SuspendReason,
        frames: u64,
        divergences: u64,
    },
}

/// The latest difference, for the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowTrouble {
    /// The frame it appeared in (the session's frame counter at its start).
    pub frame: u64,
    /// The first field that differed.
    pub field: String,
    /// Entry (ROM offset) of the function that was demoted, if one was found.
    pub function_entry: Option<u32>,
    /// Where the repro bundle was saved, if it was.
    pub bundle: Option<PathBuf>,
}

/// What a frame's check changed that the controller must act on.
#[derive(Debug, Default)]
pub struct ShadowOutcome {
    /// The verification state after a checkpoint, when one ran.
    pub verification: Option<Verification>,
    /// Something the card shows changed.
    pub changed: bool,
}

/// Drops everything a frame produces; the check compares machine state.
struct NullSink;

impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _event: CoreEvent) {}
}

pub struct ShadowCheck {
    root: PathBuf,
    sha: String,
    /// The plain core; built on the first compared frame.
    shadow: Option<EmuStepper>,
    /// The shadow at the start of the frame about to run (equal to the
    /// player's core then).
    snap: Option<MachineSnapshot>,
    /// The player's frame counter at the start of the frame about to run.
    start_fc: u64,
    /// `before_frame` readied a comparison for the frame now running.
    armed: bool,
    /// The player's frame counter the shadow matches, if it is in step.
    synced_to: Option<u64>,
    /// Something changed the player's core behind the frame loop.
    invalid: bool,
    tail: VecDeque<(u64, InputFrame)>,
    frames: u64,
    divergences: u64,
    run_counted: bool,
    /// What this session already folded into the stored record.
    flushed: (u64, u64),
    next_flush: u64,
    demoted: Vec<FunctionKey>,
    bundles: u32,
    last: Option<ShadowTrouble>,
    suspended: Option<SuspendReason>,
    /// Test only: poison one byte of the player's core at a frame.
    #[cfg(test)]
    pub(crate) poison: Option<(u64, u32, u8)>,
}

impl ShadowCheck {
    /// A check for the ROM with this hash, starting with the functions earlier
    /// sessions demoted. Cheap: the shadow core is built on the first frame.
    #[must_use]
    pub fn start(root: PathBuf, sha: &str, demoted: Vec<FunctionKey>) -> Self {
        Self {
            root,
            sha: sha.to_string(),
            shadow: None,
            snap: None,
            start_fc: 0,
            armed: false,
            synced_to: None,
            invalid: false,
            tail: VecDeque::new(),
            frames: 0,
            divergences: 0,
            run_counted: false,
            flushed: (0, 0),
            next_flush: FLUSH_FRAMES,
            demoted,
            bundles: 0,
            last: None,
            suspended: None,
            #[cfg(test)]
            poison: None,
        }
    }

    /// The functions demoted so far (this and earlier sessions), by entry key.
    #[must_use]
    pub fn demoted(&self) -> &[FunctionKey] {
        &self.demoted
    }

    #[must_use]
    pub fn status(&self) -> ShadowStatus {
        match self.suspended {
            Some(reason) => ShadowStatus::Suspended {
                reason,
                frames: self.frames,
                divergences: self.divergences,
            },
            None => ShadowStatus::Checking {
                frames: self.frames,
                divergences: self.divergences,
            },
        }
    }

    #[must_use]
    pub fn last_trouble(&self) -> Option<&ShadowTrouble> {
        self.last.as_ref()
    }

    /// The player's core was changed behind the frame loop (state load,
    /// rewind, memory-editor write): rebuild the shadow before the next
    /// compared frame.
    pub fn invalidate(&mut self) {
        self.invalid = true;
    }

    /// Call just before the player's core runs a frame with `input`. `rom` is
    /// the image the player's core was opened from (patched when a mod is on).
    pub fn before_frame(&mut self, primary: &mut EmuStepper, rom: &[u8], input: InputFrame) {
        self.armed = false;
        if crate::margin_draw::blocked_by_hooks(primary) {
            self.suspended = Some(SuspendReason::WriteHook);
            self.synced_to = None;
            return;
        }
        self.suspended = None;
        let fc = primary.frame_count();
        if self.shadow.is_none() {
            match EmuStepper::open(rom) {
                Ok(s) => self.shadow = Some(s),
                Err(_) => return,
            }
        }
        if (self.invalid || self.synced_to != Some(fc) || self.snap.is_none())
            && !self.resync(primary)
        {
            return;
        }
        self.start_fc = fc;
        self.tail.push_back((fc, input));
        if self.tail.len() > INPUT_TAIL {
            self.tail.pop_front();
        }
        #[cfg(test)]
        if let Some((frame, addr, value)) = self.poison {
            if frame == fc {
                primary.poke_ram(addr, value);
            }
        }
        self.armed = true;
    }

    /// Put the shadow exactly where the player's core is (a snapshot of the
    /// player's core, read-only for it).
    fn resync(&mut self, primary: &EmuStepper) -> bool {
        let Some(shadow) = self.shadow.as_mut() else {
            return false;
        };
        let ok = match primary.snapshot() {
            Ok(snap) => shadow.restore(&snap).is_ok().then_some(snap),
            Err(_) => None,
        };
        self.tail.clear();
        self.invalid = false;
        match ok {
            Some(snap) => {
                self.snap = Some(snap);
                self.synced_to = Some(primary.frame_count());
                true
            }
            None => {
                self.snap = None;
                self.synced_to = None;
                false
            }
        }
    }

    /// Call right after the player's core ran the frame `before_frame` readied
    /// (`ran`: it completed one). Steps the shadow through the same frame,
    /// compares, and on a difference does everything the module doc lists.
    /// `may_flush` is false while a background cache job runs (two handles on
    /// one cache index can race); a due checkpoint then waits for a later frame.
    pub fn after_frame(
        &mut self,
        primary: &mut EmuStepper,
        rom: &[u8],
        input: InputFrame,
        ran: bool,
        may_flush: bool,
    ) -> ShadowOutcome {
        let mut out = ShadowOutcome::default();
        if !self.armed {
            return out;
        }
        self.armed = false;
        let Some(shadow) = self.shadow.as_mut() else {
            return out;
        };
        if !ran {
            self.synced_to = None;
            return out;
        }
        shadow.copy_sim_config_from(primary);
        if shadow.latch_and_advance_frame(input, &mut NullSink) == 0 {
            self.synced_to = None;
            return out;
        }
        let diffs = hs::compare_frame(shadow.core_ref(), primary.core_ref());
        self.frames += 1;
        self.synced_to = Some(primary.frame_count());
        if diffs.is_empty() {
            // The shadow after this frame is the start of the next one.
            self.snap = shadow.snapshot().ok();
            if self.snap.is_none() {
                self.synced_to = None;
            }
            if self.frames >= self.next_flush && may_flush {
                self.next_flush = self.frames + FLUSH_FRAMES;
                out.verification = self.flush();
            }
        } else {
            self.on_difference(primary, rom, input, &diffs);
            // One fault, one report: carry on from the player's core.
            self.resync(primary);
            out.verification = self.flush();
            out.changed = true;
        }
        out
    }

    /// The player's core and the shadow differ after a frame: find the
    /// function, demote it, save the bundle.
    fn on_difference(
        &mut self,
        primary: &mut EmuStepper,
        rom: &[u8],
        input: InputFrame,
        diffs: &[hs::FieldDiff],
    ) {
        self.divergences += 1;
        let mut note = ShadowTrouble {
            frame: self.start_fc,
            field: diffs
                .first()
                .map_or_else(String::new, |d| d.field.to_string()),
            function_entry: None,
            bundle: None,
        };
        if self.bundles < MAX_BUNDLES {
            self.bundles += 1;
            self.investigate(primary, rom, input, diffs, &mut note);
        }
        log::warn!(
            "shadow core: the rebuilt path differed at frame {} ({}){}",
            note.frame,
            note.field,
            note.function_entry
                .map_or_else(String::new, |e| format!(", demoted function at ROM {e:#x}")),
        );
        self.last = Some(note);
    }

    /// Re-run the frame from its start on private copies, an instruction at a
    /// time. The player's core is never restored (a state does not carry the
    /// audio decimator's phase, so a restore would change its sound).
    fn investigate(
        &mut self,
        primary: &mut EmuStepper,
        rom: &[u8],
        input: InputFrame,
        diffs: &[hs::FieldDiff],
        note: &mut ShadowTrouble,
    ) {
        let Some(snap) = self.snap.clone() else {
            return;
        };
        let (Ok(mut plain), Ok(mut rebuilt)) = (EmuStepper::open(rom), EmuStepper::open(rom))
        else {
            return;
        };
        let Some(atlas) = primary.rebuild_atlas_clone() else {
            return;
        };
        if rebuilt.set_rebuild_atlas(Some(Box::new(atlas))).is_err() {
            return;
        }
        rebuilt.set_rebuild_trace(TRACE_LEN);
        if plain.restore(&snap).is_err() || rebuilt.restore(&snap).is_err() {
            return;
        }
        plain.copy_sim_config_from(primary);
        rebuilt.copy_sim_config_from(primary);
        #[cfg(test)]
        if let Some((frame, addr, value)) = self.poison {
            if frame == self.start_fc {
                rebuilt.poke_ram(addr, value);
            }
        }
        let state = hs::state_bytes(plain.core_ref(), rom);
        let fine = hs::fine_pass(plain.core_mut(), rebuilt.core_mut(), &input);
        let trace = rebuilt.rebuild_trace();

        // The function: the step that made the cores differ is the newest
        // thing in the ring (an interrupt entry is not a function; look past
        // it). Anything else (no instruction-level difference, RAM code, no
        // function) demotes nothing, and the card says so.
        if fine.is_some() {
            let culprit = trace
                .iter()
                .rev()
                .find(|e| e.kind != TraceKind::Interrupt)
                .and_then(|e| match e.kind {
                    TraceKind::Atlas {
                        function: Some(f), ..
                    } => rebuilt.rebuild_function_key(f),
                    _ => None,
                });
            if let Some(key) = culprit {
                if !self.demoted.contains(&key) {
                    self.demoted.push(key);
                    write_demoted(&self.root, &self.sha, &self.demoted);
                }
                primary.set_rebuild_demoted(&self.demoted);
                note.function_entry = Some(key.0);
            }
        }

        let dir = self.bundle_dir();
        let inputs: Vec<InputFrame> = self.tail.iter().map(|&(_, i)| i).collect();
        let first_frame = self.tail.front().map_or(0, |&(f, _)| f);
        let written = hs::write_repro(
            &dir,
            &hs::Repro {
                raw: rom,
                console: if plain.is_snes() {
                    hs::ReproConsole::Snes
                } else {
                    hs::ReproConsole::Nes
                },
                inputs: &inputs,
                first_frame: u32::try_from(first_frame).unwrap_or(u32::MAX),
                frame: u32::try_from(self.start_fc).unwrap_or(u32::MAX),
                fields: diffs.to_vec(),
                fine,
                state,
                trace,
                wram_plain: hs::wram(plain.core_ref()),
                wram_rebuilt: hs::wram(rebuilt.core_ref()),
                trace_len: TRACE_LEN,
            },
        );
        for e in &written.errors {
            log::warn!("shadow core: repro bundle part not written: {e}");
        }
        if !written.bundle.files.is_empty() {
            note.bundle = Some(written.bundle.dir);
        }
    }

    /// A directory for this difference's bundle, unique within the ROM.
    fn bundle_dir(&self) -> PathBuf {
        let rom_dir = repro_root().join(self.sha.get(..16).unwrap_or(&self.sha));
        let base = format!("frame-{}", self.start_fc);
        let first = rom_dir.join(&base);
        if !first.exists() {
            return first;
        }
        // Terminates: a bounded `for`; the last candidate is returned as is.
        (2..1000u32)
            .map(|n| rom_dir.join(format!("{base}-{n}")))
            .find(|p| !p.exists())
            .unwrap_or(first)
    }

    /// Fold this session into the stored record, replacing what it folded in
    /// before (so a checkpoint is idempotent and `runs` moves once).
    pub fn flush(&mut self) -> Option<Verification> {
        let mut record = read_record(&self.root, &self.sha).unwrap_or_default();
        record.frames = record
            .frames
            .saturating_sub(self.flushed.0)
            .saturating_add(self.frames);
        record.divergences = record
            .divergences
            .saturating_sub(self.flushed.1)
            .saturating_add(self.divergences);
        if !self.run_counted && (self.frames >= RUN_MIN_FRAMES || self.divergences > 0) {
            record.runs = record.runs.saturating_add(1);
            self.run_counted = true;
        }
        if !write_record(&self.root, &self.sha, &record) {
            // Not written: keep what is unflushed unflushed (and the run
            // uncounted), so the next checkpoint tries again.
            if self.flushed == (0, 0) {
                self.run_counted = false;
            }
            return None;
        }
        self.flushed = (self.frames, self.divergences);
        Some(Verification::of(Some(&record)))
    }
}

impl Drop for ShadowCheck {
    /// The session ends (Rebuild off, the ROM closed, the thread stopped):
    /// fold in what the last checkpoint missed.
    fn drop(&mut self) {
        if self.frames > 0 && self.flushed != (self.frames, self.divergences) {
            let _ = self.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use rf_rebuild::{PROVEN_FRAMES, PROVEN_RUNS};

    use super::*;
    use crate::core_thread::{spawn, CoreCommand, CoreEvent, CoreHandle, FrameMsg};
    use crate::rebuild::{
        store_record_for_test, stored_demoted, stored_verification, RebuildInfo, RebuildStatus,
    };

    /// 16 KiB NROM that counts forever (`INX ; JMP $C000`), reset vector
    /// `$C000`. `tag` makes the image, and so its cache entries, unique to a
    /// test. (`synthetic_nrom()` in the core-thread tests runs no ROM code at
    /// all, so it has no function to demote.)
    fn counting_nrom(tag: u8) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&rf_cart::nes::INES_MAGIC);
        data.push(1);
        data.push(1);
        data.extend_from_slice(&[0u8; 10]);
        let mut prg = vec![0xEAu8; 16 * 1024];
        prg[0..4].copy_from_slice(&[0xE8, 0x4C, 0x00, 0xC0]);
        prg[0x100] = tag;
        prg[0x3FFC..0x3FFE].copy_from_slice(&0xC000u16.to_le_bytes());
        data.extend(prg);
        data.extend(vec![0u8; 8 * 1024]);
        data
    }

    fn sha_of(image: &[u8]) -> String {
        crate::hash::sha256_hex(image)
    }

    /// The test is the UI: it steps a frame, acknowledges it (or the core
    /// drops every frame after `MAX_PENDING_FRAMES`) and hands the message to
    /// `done`. `true` when `done` accepted one within `max` steps.
    fn step_until(core: &CoreHandle, max: usize, done: &mut dyn FnMut(&FrameMsg) -> bool) -> bool {
        for _ in 0..max {
            core.cmd_tx.send(CoreCommand::StepFrame).expect("accepted");
            let CoreEvent::Frame(msg) = core
                .evt_rx
                .recv_timeout(Duration::from_secs(30))
                .expect("frame")
            else {
                continue;
            };
            core.pending_frames.fetch_sub(1, Ordering::AcqRel);
            if done(&msg) {
                return true;
            }
        }
        false
    }

    fn on(msg: &FrameMsg) -> Option<RebuildInfo> {
        match &msg.rebuild {
            Some(RebuildStatus::On(i)) => Some(i.clone()),
            _ => None,
        }
    }

    /// Turn Rebuild on and step until the first atlas is installed and the
    /// status says so. Returns that status and the frame counter.
    fn rebuild_on(core: &CoreHandle) -> (RebuildInfo, u64) {
        core.cmd_tx
            .send(CoreCommand::SetRebuild(true))
            .expect("accepted");
        let mut seen = None;
        assert!(
            step_until(core, 600, &mut |m| {
                seen = on(m).map(|i| (i, m.frame_count));
                seen.is_some()
            }),
            "Rebuild never turned on"
        );
        seen.expect("set")
    }

    fn finish(core: CoreHandle) {
        let _ = core.cmd_tx.send(CoreCommand::Shutdown);
        let _ = core.join_handle.join();
    }

    /// Ticket W37-04, acceptance 1 and the persistence clause: a forced
    /// divergence demotes the function within the frame it appears in, saves
    /// a repro bundle, spoils the record, and the demotion survives a restart.
    #[test]
    fn a_forced_divergence_demotes_the_function_in_one_frame_and_saves_a_bundle() {
        let image = counting_nrom(0x41);
        let sha = sha_of(&image);
        let core = spawn(image.clone()).expect("spawns");
        let (first, _) = rebuild_on(&core);
        assert!(
            matches!(first.shadow, ShadowStatus::Checking { .. }),
            "{first:?}"
        );
        assert_eq!(first.demoted, 0);

        // A few clean frames, then one byte of the machine changes under the
        // rebuilt side only (RAM $0010; the program never touches it).
        let mut fc = 0;
        assert!(
            step_until(&core, 5, &mut |m| {
                fc = m.frame_count;
                false
            }) || fc > 0
        );
        core.cmd_tx
            .send(CoreCommand::PoisonForTest {
                frame: fc,
                addr: 0x10,
                value: 0xAB,
            })
            .expect("accepted");
        // The very next report after the poisoned frame carries the verdict.
        let mut hit = None;
        assert!(step_until(&core, 1, &mut |m| {
            hit = on(m);
            true
        }));
        let info = hit.expect("a divergence is reported in the frame it happens in");
        assert!(
            matches!(info.shadow, ShadowStatus::Checking { divergences: 1, .. }),
            "{info:?}"
        );
        assert_eq!(info.demoted, 1, "{info:?}");
        assert!(
            matches!(
                info.verification,
                Verification::Diverged { divergences: 1, .. }
            ),
            "one difference removes any proven claim at once: {info:?}"
        );
        let trouble = info.last_difference.expect("what was found");
        assert_eq!(trouble.frame, fc);
        // The poke costs the poisoned side a bus cycle, so the cycle counter
        // is the first field to differ; work RAM follows.
        assert!(
            ["cycles", "wram"].contains(&trouble.field.as_str()),
            "{trouble:?}"
        );
        assert_eq!(trouble.function_entry, Some(0), "the reset function");
        let dir = trouble.bundle.expect("a bundle was saved");
        assert!(dir.starts_with(crate::rebuild::repro_root()), "{dir:?}");
        for f in [
            "plain_state_before_frame.rfstate",
            "inputs.txt",
            "trace.txt",
            "diff.txt",
            "wram_plain.bin",
            "wram_rebuilt.bin",
            "README.txt",
        ] {
            assert!(dir.join(f).is_file(), "{f} missing from {dir:?}");
        }
        let diff = std::fs::read_to_string(dir.join("diff.txt")).expect("text");
        assert!(diff.contains("wram"), "{diff}");
        let readme = std::fs::read_to_string(dir.join("README.txt")).expect("text");
        assert!(readme.contains("live session"), "{readme}");

        // Executing the demoted function now counts as plain fallback.
        let mut demoted_hits = 0;
        assert!(step_until(&core, 400, &mut |m| {
            demoted_hits = on(m).map_or(0, |i| i.stats.fallback_demoted);
            demoted_hits > 0
        }));
        assert!(demoted_hits > 0);
        // One fault, one report: the shadow was resynced, not left to repeat it.
        let mut again = None;
        step_until(&core, 20, &mut |m| {
            again = on(m).or(again.clone());
            false
        });
        if let Some(i) = again {
            assert!(
                matches!(i.shadow, ShadowStatus::Checking { divergences: 1, .. }),
                "{i:?}"
            );
        }
        finish(core);

        // Stored for the next session: the record and the demotion.
        assert!(matches!(
            stored_verification(&sha),
            Verification::Diverged { divergences: 1, .. }
        ));
        assert_eq!(stored_demoted(&sha), vec![(0, 0)]);

        // Restart: the function is demoted from the first frame on.
        let core = spawn(image).expect("spawns");
        let (restarted, _) = rebuild_on(&core);
        assert_eq!(restarted.demoted, 1, "{restarted:?}");
        let mut hits = 0;
        assert!(step_until(&core, 400, &mut |m| {
            hits = on(m).map_or(0, |i| i.stats.fallback_demoted);
            hits > 0
        }));
        finish(core);
    }

    /// Acceptance 2: a game the record calls proven starts no shadow core.
    #[test]
    fn a_proven_game_starts_no_shadow() {
        let image = counting_nrom(0x42);
        let sha = sha_of(&image);
        store_record_for_test(
            &sha,
            &VerifyRecord {
                frames: PROVEN_FRAMES,
                divergences: 0,
                runs: PROVEN_RUNS,
            },
        );
        let core = spawn(image).expect("spawns");
        let (info, _) = rebuild_on(&core);
        assert_eq!(info.shadow, ShadowStatus::Proven, "{info:?}");
        assert!(matches!(info.verification, Verification::Proven { .. }));
        // And it stays off, with a poisoned byte nobody compares.
        core.cmd_tx
            .send(CoreCommand::PoisonForTest {
                frame: 0,
                addr: 0x10,
                value: 1,
            })
            .expect("accepted");
        step_until(&core, 30, &mut |_| false);
        finish(core);
        assert!(matches!(
            stored_verification(&sha),
            Verification::Proven { .. }
        ));
    }

    /// Acceptance 2, the other half: a game that crosses the threshold while
    /// the shadow is running switches it off at that checkpoint.
    #[test]
    fn the_shadow_switches_off_when_the_game_becomes_proven() {
        let image = counting_nrom(0x43);
        let sha = sha_of(&image);
        // One run short, and a few hundred frames short.
        store_record_for_test(
            &sha,
            &VerifyRecord {
                frames: PROVEN_FRAMES - FLUSH_FRAMES / 2,
                divergences: 0,
                runs: PROVEN_RUNS - 1,
            },
        );
        let core = spawn(image).expect("spawns");
        let (info, _) = rebuild_on(&core);
        assert!(
            matches!(info.shadow, ShadowStatus::Checking { .. }),
            "{info:?}"
        );
        let mut last = None;
        let off = step_until(&core, 2000, &mut |m| {
            last = on(m).or(last.take());
            last.as_ref()
                .is_some_and(|i| i.shadow == ShadowStatus::Proven)
        });
        assert!(off, "never switched off: {last:?}");
        let last = last.expect("seen");
        assert!(matches!(last.verification, Verification::Proven { .. }));
        finish(core);
        assert!(matches!(
            stored_verification(&sha),
            Verification::Proven { .. }
        ));
    }

    /// A mod that writes memory suspends the check, honestly worded, and the
    /// same poisoned byte is not reported as a fault while it is loaded.
    #[test]
    fn a_write_hook_suspends_the_check() {
        let image = counting_nrom(0x44);
        let sha = sha_of(&image);
        let core = spawn(image).expect("spawns");
        let setup = crate::rebuild::HookSetup {
            manifest: rf_plugin_sdk::Manifest {
                id: "shadow-suspend".into(),
                version: "0.1.0".into(),
                api: "0.1".into(),
                license: "MIT".into(),
                provenance: "test".into(),
                capabilities: rf_plugin_sdk::Capabilities {
                    function_hooks: true,
                    write_memory: true,
                    ..rf_plugin_sdk::Capabilities::default()
                },
            },
            lua_source: "-- no hooks".to_string(),
            recipe: rf_rebuild::Recipe {
                id: "t".into(),
                engine: "t".into(),
                functions: vec![],
                ram: vec![],
                patches: vec![],
                ..Default::default()
            },
        };
        core.cmd_tx
            .send(CoreCommand::SetHooks(Some(Box::new(setup))))
            .expect("accepted");
        let (_, fc) = rebuild_on(&core);
        core.cmd_tx
            .send(CoreCommand::PoisonForTest {
                frame: fc + 2,
                addr: 0x10,
                value: 0xAB,
            })
            .expect("accepted");
        let mut last = None;
        step_until(&core, 20, &mut |m| {
            last = on(m).or(last.take());
            false
        });
        let last = last.expect("a report");
        assert!(
            matches!(
                last.shadow,
                ShadowStatus::Suspended {
                    reason: SuspendReason::WriteHook,
                    divergences: 0,
                    ..
                }
            ),
            "{last:?}"
        );
        let card = crate::rebuild_card::card(true, &RebuildStatus::On(last)).expect("card");
        assert!(
            card.text().contains("not checked: mod active"),
            "{}",
            card.text()
        );
        finish(core);
        assert_eq!(stored_verification(&sha), Verification::NotChecked);
    }

    /// 32 KiB LoROM that loops `NOP ; BRA -3` from its reset vector.
    fn counting_lorom(tag: u8) -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0..3].copy_from_slice(&[0xEA, 0x80, 0xFD]);
        rom[0x100] = tag;
        rom[0x7FD5] = 0x20;
        rom[0x7FD7] = 0x08;
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        rom
    }

    /// The SNES core is compared the same way, and a clean run stays clean
    /// across the shadow's per-frame snapshot (so the snapshot round trip
    /// loses nothing the comparison reads).
    #[test]
    fn a_clean_snes_run_stays_clean_and_a_forced_difference_is_found() {
        let image = counting_lorom(0x45);
        let sha = sha_of(&image);
        let core = spawn(image).expect("spawns");
        let (info, _) = rebuild_on(&core);
        assert!(
            matches!(info.shadow, ShadowStatus::Checking { .. }),
            "{info:?}"
        );
        let mut fc = 0;
        let mut last = None;
        step_until(&core, 60, &mut |m| {
            fc = m.frame_count;
            last = on(m).or(last.take());
            false
        });
        let ShadowStatus::Checking {
            frames,
            divergences: 0,
        } = last.expect("a report").shadow
        else {
            panic!("expected a clean check");
        };
        assert!(frames > 0, "the shadow compared frames");
        core.cmd_tx
            .send(CoreCommand::PoisonForTest {
                frame: fc,
                addr: 0x10,
                value: 0xAB,
            })
            .expect("accepted");
        let mut hit = None;
        step_until(&core, 1, &mut |m| {
            hit = on(m);
            true
        });
        let info = hit.expect("reported in the frame");
        assert!(
            matches!(info.shadow, ShadowStatus::Checking { divergences: 1, .. }),
            "{info:?}"
        );
        assert_eq!(info.demoted, 1, "{info:?}");
        finish(core);
        assert_eq!(stored_demoted(&sha).len(), 1);
    }
}
