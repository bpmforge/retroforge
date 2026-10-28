//! Driving the widescreen finders on a real cartridge (ticket W38-04).
//!
//! Three jobs, all over a caller's input script and a per-frame `before` hook
//! (test-side state setup such as a level select or an invincibility poke,
//! applied identically to every run):
//!
//! * [`observe_nes`] / [`observe_snes`]: one observed run under
//!   [`rf_rebuild::widescreen::WideReader`], giving the candidate sites.
//! * [`nes_snaps`] / [`snes_snaps`]: a plain run recorded as per-frame
//!   [`Snap`]s, for [`rf_rebuild::widescreen::compare_trial`]. A trial is a
//!   plain run of the unmodified image against a plain run of a copy with one
//!   operand changed ([`nes_patch`], [`snes_patch`]); the unmodified run is
//!   recorded once and reused.
//!
//! Only offsets, counts and verdicts leave a caller (law 5, D-021).

use rf_core_api::{
    CoreEvent, CoreSink, EmulatorCore as _, InputFrame, PpuPixel, ResetKind, StepObserver,
};
use rf_nes::core::NesCore;
use rf_rebuild::widescreen::{Snap, Watch, WideFacts, WideReader};
use rf_rebuild::{Atlas, Platform};
use rf_snes::core::SnesCore;

use crate::data_regions::{NesDriver, SnesDriver};
use crate::object_tables::{nes_cam, snes_cam, SNES_RAM_WINDOW};

struct Null;

impl CoreSink for Null {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

/// Observe an NES run from frame `from` on (earlier frames run plain).
///
/// # Errors
/// The image or atlas does not fit, or a frame cannot be observed.
pub fn observe_nes(
    raw: &[u8],
    atlas: &Atlas,
    inputs: &[InputFrame],
    from: usize,
    watch: Watch,
    mut before: impl FnMut(usize, &mut NesCore),
) -> Result<WideFacts, String> {
    let mut d = NesDriver::new(raw, atlas)?;
    let mut reader = WideReader::new(Platform::Nes, watch);
    for (f, input) in inputs.iter().enumerate() {
        before(f, &mut d.core);
        if f < from {
            d.frame(input, false)?;
            continue;
        }
        reader.set_resolver(d.resolver());
        d.frame_with(input, &mut reader as &mut dyn StepObserver)?;
    }
    Ok(reader.take_facts())
}

/// Observe a SNES run from frame `from` on.
///
/// # Errors
/// As [`observe_nes`].
pub fn observe_snes(
    raw: &[u8],
    atlas: &Atlas,
    inputs: &[InputFrame],
    from: usize,
    watch: Watch,
) -> Result<WideFacts, String> {
    let mut d = SnesDriver::new(raw, atlas)?;
    let mut reader = WideReader::new(Platform::Snes, watch);
    reader.set_resolver(d.resolver());
    for (f, input) in inputs.iter().enumerate() {
        if f < from {
            d.frame(input, false)?;
            continue;
        }
        d.frame_with(input, &mut reader as &mut dyn StepObserver)?;
    }
    Ok(reader.take_facts())
}

/// A plain NES run recorded after every frame.
///
/// # Errors
/// The image does not load.
pub fn nes_snaps(
    raw: &[u8],
    inputs: &[InputFrame],
    mut before: impl FnMut(usize, &mut NesCore),
) -> Result<Vec<Snap>, String> {
    let mut core = NesCore::from_ines_bytes(raw).map_err(|e| format!("{e:?}"))?;
    core.reset(ResetKind::Hard);
    core.bus_mut().set_bg_under_capture(true);
    let mut out = Vec::with_capacity(inputs.len());
    for (f, input) in inputs.iter().enumerate() {
        before(f, &mut core);
        core.run_frame(input, &mut Null);
        out.push(Snap {
            ram: core.bus().ram().to_vec(),
            oam: core.bus().oam().to_vec(),
            cam_x: nes_cam(&core).0,
        });
    }
    Ok(out)
}

/// A plain SNES run recorded after every frame (low work RAM, as the object
/// finder searches it).
///
/// # Errors
/// The image does not load.
pub fn snes_snaps(
    raw: &[u8],
    inputs: &[InputFrame],
    mut before: impl FnMut(usize, &mut SnesCore),
) -> Result<Vec<Snap>, String> {
    let mut core = SnesCore::load(raw).map_err(|e| e.to_string())?;
    core.reset(ResetKind::Hard);
    let mut out = Vec::with_capacity(inputs.len());
    for (f, input) in inputs.iter().enumerate() {
        before(f, &mut core);
        let ports = &mut core.system_mut().bus.joypads.ports;
        for (slot, word) in ports.iter_mut().zip(input.ports) {
            *slot = word;
        }
        core.run_frame(input, &mut Null);
        out.push(Snap {
            ram: core.system().bus.wram[..SNES_RAM_WINDOW].to_vec(),
            oam: core.system().bus.ppu.oam.to_vec(),
            cam_x: snes_cam(&core).0,
        });
    }
    Ok(out)
}

/// A copy of an iNES image with `bytes` written at PRG offset `at`.
///
/// # Errors
/// The offset is outside the PRG ROM.
pub fn nes_patch(raw: &[u8], at: u32, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let edits: Vec<(u32, u8)> = (at..).zip(bytes.iter().copied()).collect();
    rf_enhance::patch::prg::apply_prg_bytes(raw, &edits).map_err(|e| format!("{e:?}"))
}

/// A copy of a SNES image with `bytes` written at ROM offset `at` (a copier
/// header, if any, is skipped).
///
/// # Errors
/// Not a SNES image, or the offset is past the end.
pub fn snes_patch(raw: &[u8], at: u32, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let rf_cart::Cartridge::Snes { header, .. } =
        rf_cart::Cartridge::load(raw).map_err(|e| e.to_string())?
    else {
        return Err("not a SNES cartridge".to_string());
    };
    let skip = if header.had_copier_header { 512 } else { 0 };
    let mut out = raw.to_vec();
    for (i, b) in bytes.iter().enumerate() {
        let o = skip + at as usize + i;
        *out.get_mut(o).ok_or("offset past the end")? = *b;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Trials over a candidate list.
// ---------------------------------------------------------------------------

use rf_rebuild::widescreen::{
    compare_trial, locate, operand_bytes, widen_values, Candidate, Kind, Trial, View, MIN_GAIN,
};
use rf_rebuild::{Grade, Recipe};

/// One candidate and what its trials said.
#[derive(Debug, Clone)]
pub struct Tried {
    pub cand: Candidate,
    /// Every (operand value, trial) run alone.
    pub alone: Vec<(u16, Trial)>,
    /// The kept value and its trial: alone, or with `partner`.
    pub best: Option<(u16, Trial)>,
    /// Keepers this candidate only matters next to (their ROM operand offsets).
    pub partners: Vec<u32>,
}

impl Tried {
    #[must_use]
    pub fn grade(&self) -> Grade {
        rf_rebuild::widescreen::grade_of(self.best.as_ref().map(|b| &b.1))
    }

    /// The kind the trial shows: what widening did beats what the reads
    /// suggested.
    #[must_use]
    pub fn kind(&self) -> Kind {
        self.best
            .as_ref()
            .and_then(|b| b.1.reach_kind())
            .unwrap_or(self.cand.kind)
    }
}

fn reach_sum(t: &Trial) -> i32 {
    t.gain_right.max(0) + t.gain_left.max(0)
}

/// Writes bytes into a copy of an image: `(image, rom offset, bytes)`.
pub type PatchFn<'a> = &'a dyn Fn(&[u8], u32, &[u8]) -> Result<Vec<u8>, String>;
/// Plays an image from power-on and returns its per-frame snaps.
pub type RunFn<'a> = &'a dyn Fn(&[u8]) -> Result<Vec<Snap>, String>;

/// What [`try_candidates`] trials against.
#[derive(Clone, Copy)]
pub struct TrialSetup<'a> {
    pub raw: &'a [u8],
    pub view: &'a View,
    /// The unmodified image's run.
    pub plain: &'a [Snap],
    pub patch: PatchFn<'a>,
    pub run: RunFn<'a>,
    /// Wider constants tried per candidate (the widest ones).
    pub values: usize,
}

/// Try each candidate with each wider constant on a copy of `raw`, then try the
/// harmless ones together (see the group stage below). /// bytes into an image copy. `log` receives one line per trial.
///
/// # Errors
/// A patch or a run fails.
pub fn try_candidates(
    setup: &TrialSetup<'_>,
    cands: &[&Candidate],
    log: &mut dyn FnMut(&str),
) -> Result<Vec<Tried>, String> {
    let TrialSetup {
        raw,
        view,
        plain,
        patch,
        run,
        values,
    } = *setup;
    let mut out: Vec<Tried> = Vec::new();
    for &c in cands {
        let mut t = Tried {
            cand: c.clone(),
            alone: Vec::new(),
            best: None,
            partners: Vec::new(),
        };
        let all = widen_values(c);
        for &v in &all[all.len().saturating_sub(values)..] {
            let image = patch(raw, c.operand, &operand_bytes(c.width, v))?;
            let tr = compare_trial(view, plain, &run(&image)?);
            log(&trial_line("alone", c, v, &tr));
            if tr.keeper()
                && t.best
                    .as_ref()
                    .is_none_or(|b| reach_sum(&tr) > reach_sum(&b.1))
            {
                t.best = Some((v, tr.clone()));
            }
            t.alone.push((v, tr));
        }
        out.push(t);
    }
    // Groups. Some constants only act together: an activation window wider
    // than the cull distance is removed again at once. So the candidates that
    // did no visible harm alone (and the keepers) go in together at their wider
    // values; if the group is a keeper, each member is left out in turn, and
    // the ones whose absence loses the widening are the essential partners.
    let members: Vec<(usize, u16)> = out
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            t.best.is_some()
                || t.alone.iter().all(|(_, tr)| {
                    tr.first
                        .as_ref()
                        .is_none_or(|d| d.on_screen.is_empty() && !d.oam && !d.camera)
                })
        })
        .filter_map(|(i, t)| {
            let v = t
                .best
                .as_ref()
                .map(|b| b.0)
                .or_else(|| widen_values(&t.cand).last().copied())?;
            Some((i, v))
        })
        .collect();
    if members.len() >= 2 {
        let play = |skip: Option<usize>| -> Result<Trial, String> {
            let mut image = raw.to_vec();
            for &(i, v) in members.iter().filter(|(i, _)| Some(*i) != skip) {
                let c = &out[i].cand;
                image = patch(&image, c.operand, &operand_bytes(c.width, v))?;
            }
            Ok(compare_trial(view, plain, &run(&image)?))
        };
        let full = play(None)?;
        log(&format!(
            "TRIAL group        {} members first {:?} reach R{} L{} keeper {} {}",
            members.len(),
            full.first.as_ref().map(|d| d.frame),
            full.gain_right,
            full.gain_left,
            full.keeper(),
            full.why_not()
        ));
        if full.keeper() {
            let mut essential: Vec<usize> = Vec::new();
            for &(i, _) in &members {
                let without = play(Some(i))?;
                let need = !without.keeper() || reach_sum(&without) + MIN_GAIN <= reach_sum(&full);
                log(&format!(
                    "TRIAL leave-out    rom {:#07x} reach R{} L{} keeper {} essential {need}",
                    out[i].cand.operand,
                    without.gain_right,
                    without.gain_left,
                    without.keeper()
                ));
                if need {
                    essential.push(i);
                }
            }
            for &i in &essential {
                let partners: Vec<u32> = essential
                    .iter()
                    .filter(|&&j| j != i)
                    .map(|&j| out[j].cand.operand)
                    .collect();
                let v = members.iter().find(|m| m.0 == i).map_or(0, |m| m.1);
                if out[i].best.is_none() {
                    out[i].best = Some((v, full.clone()));
                }
                out[i].partners = partners;
            }
        }
    }
    Ok(out)
}

fn trial_line(how: &str, c: &Candidate, v: u16, tr: &Trial) -> String {
    let d = tr.first.as_ref();
    format!(
        "TRIAL {how:<12} {:<16} rom {:#07x} {:#06x}->{:#06x} slots {} first {:?} off {} on {} other {} oam {} cam {} visible {:?} reach R{} L{} keeper {} {}",
        c.kind.name(),
        c.operand,
        c.imm,
        v,
        tr.slots,
        d.map(|d| d.frame),
        d.map_or(0, |d| d.off_screen.len()),
        d.map_or(0, |d| d.on_screen.len()),
        d.map_or(0, |d| d.bookkeeping.len()),
        d.is_some_and(|d| d.oam),
        d.is_some_and(|d| d.camera),
        tr.visible_at,
        tr.gain_right,
        tr.gain_left,
        tr.keeper(),
        tr.why_not()
    )
}

/// `[[function]]` / `[[patch]]` TOML for the finished candidates: offsets and
/// constants only (law 5). Functions the recipe already names are reused (same
/// shape and exact hash); a new one is named by its entry address. A keeper is
/// `confirmed` in mod `keep_mod`; everything else is a `candidate` in
/// `cand_mod`, which nothing applies. `skip` lists operand ROM offsets that the
/// recipe already carries.
#[must_use]
pub fn rows_toml(
    atlas: &Atlas,
    recipe: &Recipe,
    tried: &[Tried],
    keep_mod: &str,
    cand_mod: &str,
    skip: &[u32],
    what: &str,
) -> String {
    let mut fns = String::new();
    let mut rows = String::new();
    let mut named: Vec<(u32, String)> = Vec::new();
    for t in tried.iter().filter(|t| !skip.contains(&t.cand.operand)) {
        let Some((fi, idx)) = locate(atlas, t.cand.operand) else {
            continue;
        };
        let Some(f) = atlas.functions.get(fi as usize) else {
            continue;
        };
        // Operand values to write: the kept one, else the widest tried.
        let Some(v) = t
            .best
            .as_ref()
            .map(|b| b.0)
            .or_else(|| widen_values(&t.cand).last().copied())
        else {
            continue;
        };
        let name = recipe
            .functions
            .iter()
            .find(|r| r.shape_hash == f.shape_hash && r.exact_hash == Some(f.byte_hash))
            .map(|r| r.name.clone())
            .or_else(|| named.iter().find(|n| n.0 == fi).map(|n| n.1.clone()))
            .unwrap_or_else(|| {
                let n = format!("wide_fn_{:x}", f.entry_cpu);
                fns.push_str(&format!(
                    "[[function]]\nname = \"{n}\"\nshape_hash = \"{:#x}\"\nexact_hash = \"{:#x}\"\naddress_hint = {:#x}\nhooks = []\ngrade = \"observed\"\nsource = \"observed: the atlas function holding a widescreen finder site ({what}); W38-04\"\n\n",
                    f.shape_hash, f.byte_hash, f.entry_cpu
                ));
                named.push((fi, n.clone()));
                n
            });
        let grade = t.grade();
        let (mod_name, how) = if grade == Grade::Confirmed {
            (
                keep_mod,
                "lockstep keeper: only off-screen state changes first",
            )
        } else {
            (cand_mod, "finder candidate, not kept by a lockstep trial")
        };
        let partner = if t.partners.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = t.partners.iter().map(|p| format!("{p:#x}")).collect();
            format!(
                "; acts only together with the patches at ROM offsets {}",
                list.join(", ")
            )
        };
        for (k, byte) in operand_bytes(t.cand.width, v).into_iter().enumerate() {
            rows.push_str(&format!(
                "[[patch]]\nname = \"wide-{}-{:x}\"\nmod = \"{mod_name}\"\nfunction = \"{name}\"\noffset = {}\nvalue = {byte:#04x}\ngrade = \"{}\"\nsource = \"observed: {} operand of an immediate {} (was {:#x}), confidence {} {}; {how}{partner}\"\n\n",
                t.kind().name(),
                t.cand.operand + k as u32,
                idx + k as u32,
                grade.as_str(),
                t.kind().name(),
                t.cand.op.map_or("?", |o| o.name()),
                t.cand.imm,
                t.cand.band.name(),
                t.cand.score,
            ));
        }
    }
    format!("{fns}{rows}")
}
