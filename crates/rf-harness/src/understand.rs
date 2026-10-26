//! Running the understand pass on a real cartridge, and the inert-hook check
//! (ticket W36-09; `docs/design/REBUILD_LAB.md` section 5, step 5).
//!
//! [`run_nes`] / [`run_snes`] build a core, run the drive script under
//! `rf_rebuild::profile::observe` and propose names. [`name_hooks`] turns the
//! proposals into recipe name rows, resolves them against the atlas and
//! returns the ids to hook, so a lockstep run can prove that **applying every
//! proposed name as a hook changes nothing**: the rows are resolved like any
//! recipe's, the hooks are registered, and the sink they report to does
//! nothing.
//!
//! Only counts and names are reported; no ROM bytes leave a test (D-021).

use std::collections::BTreeMap;

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore as _, InputFrame, PpuPixel, ResetKind};
use rf_nes::core::NesCore;
use rf_rebuild::naming::{count_by_rule, merge_name_rows, propose_names, Proposal};
use rf_rebuild::profile::{observe, ObserveError, Understanding};
use rf_rebuild::{match_recipe, Atlas, Platform, Recipe};
use rf_snes::core::SnesCore;

use crate::lockstep::HookSinkFactory;

/// What an understand run produced.
pub struct NamingRun {
    pub understanding: Understanding,
    pub proposals: Vec<Proposal>,
}

/// Run an iNES image under `atlas` with `inputs` and name what was seen.
///
/// # Errors
/// The image does not load, or the atlas is for another cartridge.
pub fn run_nes(
    raw: &[u8],
    atlas: &Atlas,
    inputs: impl IntoIterator<Item = InputFrame>,
) -> Result<NamingRun, String> {
    let mut core = NesCore::from_ines_bytes(raw).map_err(|e| format!("{e:?}"))?;
    core.reset(ResetKind::Hard);
    finish(
        observe(&mut core, Platform::Nes, atlas, inputs, |_, _, _| {}),
        atlas,
    )
}

/// Run a SNES image under `atlas`. `run_frame` ignores `input` on the SNES,
/// so the pad words are latched into the bus here, as the app does.
///
/// # Errors
/// The image does not load, or the atlas is for another cartridge.
pub fn run_snes(
    raw: &[u8],
    atlas: &Atlas,
    inputs: impl IntoIterator<Item = InputFrame>,
) -> Result<NamingRun, String> {
    let mut core = SnesCore::load(raw).map_err(|e| e.to_string())?;
    core.reset(ResetKind::Hard);
    finish(
        observe(&mut core, Platform::Snes, atlas, inputs, |c, _, input| {
            let ports = &mut c.system_mut().bus.joypads.ports;
            for (slot, word) in ports.iter_mut().zip(input.ports) {
                *slot = word;
            }
        }),
        atlas,
    )
}

fn finish(r: Result<Understanding, ObserveError>, atlas: &Atlas) -> Result<NamingRun, String> {
    let understanding = r.map_err(|e| e.to_string())?;
    let proposals = propose_names(&understanding, atlas);
    Ok(NamingRun {
        understanding,
        proposals,
    })
}

/// A recipe holding every proposal as a name row (starting from `base`), and
/// the atlas function ids those rows resolve to. Every row must resolve:
/// `Err` names the ones that did not.
///
/// # Errors
/// A row that no atlas function matched.
pub fn name_hooks(
    atlas: &Atlas,
    proposals: &[Proposal],
    base: Recipe,
) -> Result<(Recipe, Vec<u32>), String> {
    let mut recipe = base;
    merge_name_rows(&mut recipe, atlas, proposals);
    let report = match_recipe(atlas, &recipe);
    if !report.unmatched.is_empty() {
        return Err(format!("unmatched rows: {:?}", report.unmatched));
    }
    let mut ids: Vec<u32> = report.matched.iter().map(|m| m.function).collect();
    ids.sort_unstable();
    ids.dedup();
    Ok((recipe, ids))
}

/// A sink that does nothing with hook events: the "inert" hook host.
pub struct InertSink;

impl CoreSink for InertSink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

/// `Options::hook_sink` for [`InertSink`].
#[must_use]
pub fn inert_sink<'a>() -> HookSinkFactory<'a> {
    Box::new(|| Box::new(InertSink))
}

/// Counts for the research note: functions named per rule, observed
/// functions, atlas functions.
#[must_use]
pub fn summary_lines(run: &NamingRun, atlas: &Atlas) -> Vec<String> {
    let u = &run.understanding;
    let by_rule: BTreeMap<String, usize> = count_by_rule(&run.proposals);
    let mut out = vec![format!(
        "{} frames observed; {} of {} atlas functions executed; {} named by rules",
        u.frames,
        u.observed_functions(),
        atlas.functions.len(),
        run.proposals.len()
    )];
    out.push(format!("by rule: {by_rule:?}"));
    out
}

/// One line per proposal, for a sample table: name, rule, confidence, evidence.
#[must_use]
pub fn sample_lines(run: &NamingRun) -> Vec<String> {
    run.proposals
        .iter()
        .map(|p| {
            format!(
                "| {} | {} | {} | {} |",
                p.name,
                p.source,
                p.confidence,
                p.evidence.join("; ")
            )
        })
        .collect()
}

/// FNV-1a over what a frame's machine state exposes: work RAM, VRAM, palette
/// RAM, OAM and the CPU registers.
fn state_print<C: rf_core_api::EmulatorCore>(c: &C) -> u64 {
    let v = c.state_view();
    let regs = format!("{:?}", v.cpu_regs);
    [v.wram, v.vram, v.cgram, v.oam, regs.as_bytes()]
        .iter()
        .flat_map(|s| s.iter())
        .fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
}

/// Run `inputs` through a plain core (`run_frame`, atlas installed) and through
/// a second one under the understand-pass observer, and compare the state
/// they end in. This is what makes "the observed run is the game" a measured
/// fact on a real cartridge rather than a claim: the lockstep checker proves
/// hooks change nothing, and this proves the observer's frame loop does not
/// either.
///
/// # Errors
/// The cartridge or atlas does not load; `Ok(false)` means the end states
/// differ.
pub fn observed_matches_plain_nes(
    raw: &[u8],
    atlas: &Atlas,
    inputs: &[InputFrame],
) -> Result<bool, String> {
    let mut plain = NesCore::from_ines_bytes(raw).map_err(|e| format!("{e:?}"))?;
    plain.reset(ResetKind::Hard);
    plain
        .set_rebuild(Some(atlas.clone()))
        .map_err(|e| format!("{e:?}"))?;
    let mut watched = NesCore::from_ines_bytes(raw).map_err(|e| format!("{e:?}"))?;
    watched.reset(ResetKind::Hard);
    for i in inputs {
        plain.run_frame(i, &mut InertSink);
    }
    observe(
        &mut watched,
        Platform::Nes,
        atlas,
        inputs.iter().copied(),
        |_, _, _| {},
    )
    .map_err(|e| e.to_string())?;
    Ok(state_print(&plain) == state_print(&watched))
}

/// [`observed_matches_plain_nes`] for a SNES image (the pad words are latched
/// into the bus before each frame, as the app does).
///
/// # Errors
/// As the NES one.
pub fn observed_matches_plain_snes(
    raw: &[u8],
    atlas: &Atlas,
    inputs: &[InputFrame],
) -> Result<bool, String> {
    let latch = |c: &mut SnesCore, input: &InputFrame| {
        let ports = &mut c.system_mut().bus.joypads.ports;
        for (slot, word) in ports.iter_mut().zip(input.ports) {
            *slot = word;
        }
    };
    let mut plain = SnesCore::load(raw).map_err(|e| e.to_string())?;
    plain.reset(ResetKind::Hard);
    plain
        .set_rebuild(Some(atlas.clone()))
        .map_err(|e| format!("{e:?}"))?;
    let mut watched = SnesCore::load(raw).map_err(|e| e.to_string())?;
    watched.reset(ResetKind::Hard);
    for i in inputs {
        latch(&mut plain, i);
        plain.run_frame(i, &mut InertSink);
    }
    observe(
        &mut watched,
        Platform::Snes,
        atlas,
        inputs.iter().copied(),
        |c, _, input| latch(c, input),
    )
    .map_err(|e| e.to_string())?;
    Ok(state_print(&plain) == state_print(&watched))
}
