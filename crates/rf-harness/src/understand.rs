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
