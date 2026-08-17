//! Accuracy-vs-Compatibility diff over the executed test-ROM suite
//! (ticket W3-07; FR-MODE-004, `docs/design/EMULATION_CORES.md` §5's
//! "CI diffs both").
//!
//! ## What this enforces, and why green-everywhere is a FAILURE
//!
//! §5, verbatim: "Both configs are deterministic. Compatibility never
//! changes *observable game-facing* behavior for the supported library —
//! **divergences must be test-suite-visible, or the switch doesn't
//! exist.**"
//!
//! That sentence cuts both ways, and this module enforces both edges:
//!
//! - An **undeclared** divergence fails. A compatibility path may only
//!   differ where someone wrote down that it differs, which is the same
//!   discipline `waivers.toml` applies to known-fails.
//! - A **declared** divergence that stops happening also fails. If the
//!   suite cannot see a switch, then by §5's own sentence the switch is
//!   not there — the two paths have converged and the declaration is a
//!   claim about a thing that no longer exists. This is the direct analog
//!   of `tier_b_suites`' stale-waiver rule (a waiver over a ROM that has
//!   started passing is itself an error).
//!
//! The second rule is the one that keeps this honest. Without it, a
//! compatibility switch could quietly become a no-op — someone deletes
//! the fast path, both configs run the same code, the diff goes green,
//! and the green is indistinguishable from "correct". That is precisely
//! the vacuous check W3-07's own ticket note warned about: "a diff today
//! would compare a path against itself and pass forever".
//!
//! ## Scope today
//!
//! One switch exists (EMULATION_CORES §5's "Open-bus modeling: full |
//! simplified" — see `rf_nes::Ppu`'s `accuracy_mode` field), so one
//! divergence is declared. §5's other headline row, PPU catch-up
//! stepping, is owed by ticket W3-07b; when it lands it must produce NO
//! divergence (§5 rates its risk "none if catch-up correct"), and this
//! runner is the tool that says whether that is true.

use std::path::{Path, PathBuf};

use crate::blargg_evidence::{self, ScreenOutcome};
use crate::{BlarggStatus, Manifest, Protocol, Suite};

/// A divergence this project has decided is expected, with the reason and
/// the switch responsible.
#[derive(Debug, Clone, Copy)]
pub struct DeclaredDivergence {
    /// Suite id, matching `[[suite]].id` in `tests/rom-manifest.toml`.
    pub suite: &'static str,
    /// ROM label, or `"*"` for every ROM in the suite.
    pub rom: &'static str,
    /// Which EMULATION_CORES §5 switch causes it.
    pub switch: &'static str,
    /// Why the divergence is acceptable — never empty.
    pub reason: &'static str,
}

/// Every divergence Compatibility mode is allowed to produce.
///
/// Deliberately a short, hand-written list rather than anything derived:
/// each entry is a decision that Compatibility may be observably worse
/// *here specifically*, and §5 requires those to be visible rather than
/// inferred.
pub const DECLARED: &[DeclaredDivergence] = &[DeclaredDivergence {
    suite: "ppu_open_bus",
    rom: "ppu_open_bus",
    switch: "Open-bus modeling: full | simplified",
    reason: "Compatibility does not age the PPU's decay register, so an unrefreshed bit holds \
             its value instead of falling to 0 after ~600ms. This ROM's tests 3, 5, 7 and 9 \
             each refresh part of the register and then require the UNrefreshed part to have \
             decayed within one second, so it passes under Accuracy and fails under \
             Compatibility. That is the switch being test-suite-visible, which EMULATION_CORES \
             section 5 requires of any compatibility setting.",
}];

/// One suite/ROM's result under both configs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    pub suite: String,
    pub rom: String,
    pub accuracy: String,
    pub compatibility: String,
}

impl Comparison {
    /// Did the two configs disagree?
    pub fn diverged(&self) -> bool {
        self.accuracy != self.compatibility
    }
}

/// What a full run concluded.
#[derive(Debug, Default)]
pub struct DiffReport {
    /// Every ROM actually executed under both configs.
    pub compared: Vec<Comparison>,
    /// ROMs whose files were not fetched — never counted as agreement.
    pub skipped: usize,
    /// Divergences with no entry in [`DECLARED`].
    pub undeclared: Vec<Comparison>,
    /// Entries of [`DECLARED`] that did NOT diverge on this run.
    pub stale: Vec<String>,
}

impl DiffReport {
    /// A run is clean only when nothing undeclared diverged AND every
    /// declaration still holds — see this module's doc for why the second
    /// half matters as much as the first.
    pub fn is_clean(&self) -> bool {
        self.undeclared.is_empty() && self.stale.is_empty()
    }
}

/// Run one ROM and reduce it to a comparable string.
///
/// The verdict AND its message are both included: two runs that both
/// "fail" but fail at different sub-tests have diverged, and folding them
/// to a bare pass/fail would hide exactly the kind of difference this diff
/// exists to surface.
fn outcome_string(path: &Path, protocol: Protocol, frames: u32, accuracy: bool) -> String {
    match protocol {
        Protocol::ScreenText => {
            match blargg_evidence::run_screen_text_with_mode(path, frames, accuracy) {
                Ok(ScreenOutcome::Passed(text)) => format!("PASSED {}", squash(&text)),
                Ok(ScreenOutcome::Failed(text)) => format!("FAILED {}", squash(&text)),
                Ok(ScreenOutcome::NoVerdict(text)) => format!("NO-VERDICT {}", squash(&text)),
                Err(e) => format!("ERROR {e}"),
            }
        }
        _ => match blargg_evidence::run_with_mode(path, frames, accuracy) {
            Ok(outcome) if outcome.status == BlarggStatus::Passed => {
                format!("PASSED {}", squash(&outcome.message))
            }
            Ok(outcome) => format!("{:?} {}", outcome.status, squash(&outcome.message)),
            Err(e) => format!("ERROR {e}"),
        },
    }
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Which protocols this runner can actually score. Anything else is
/// reported as skipped rather than silently treated as agreeing.
fn runnable(suite: &Suite) -> bool {
    matches!(suite.protocol, Protocol::SixThousand | Protocol::ScreenText)
}

/// Run every runnable ROM in `manifest` under both configs.
pub fn run(manifest: &Manifest, repo_root: &Path) -> DiffReport {
    let mut report = DiffReport::default();

    for suite in &manifest.suites {
        if !runnable(suite) {
            report.skipped += suite.roms.len();
            continue;
        }
        for rom in &suite.roms {
            let Some(artifact) = manifest.artifacts.iter().find(|a| a.id == rom.artifact) else {
                report.skipped += 1;
                continue;
            };
            let path: PathBuf = repo_root.join(&artifact.dest);
            if !path.is_file() {
                report.skipped += 1;
                continue;
            }
            let comparison = Comparison {
                suite: suite.id.clone(),
                rom: rom.rom.clone(),
                accuracy: outcome_string(&path, suite.protocol, rom.frame_budget, true),
                compatibility: outcome_string(&path, suite.protocol, rom.frame_budget, false),
            };
            report.compared.push(comparison);
        }
    }

    report.undeclared = undeclared_divergences(&report.compared, DECLARED);
    report.stale = stale_declarations(&report.compared, DECLARED);
    report
}

/// The §5 "or the switch doesn't exist" half, as a pure function so both
/// failure directions can be tested without any fetched ROM (CI has
/// none — NFR-006).
///
/// Only a ROM that actually RAN can falsify a declaration: an unfetched
/// one proves nothing either way, and treating absence as convergence
/// would turn a mirror outage into a spurious failure.
pub fn stale_declarations(compared: &[Comparison], declared: &[DeclaredDivergence]) -> Vec<String> {
    let mut stale = Vec::new();
    for decl in declared {
        let matches =
            |c: &&Comparison| c.suite == decl.suite && (decl.rom == "*" || c.rom == decl.rom);
        let ran = compared.iter().any(|c| matches(&c));
        let observed = compared.iter().filter(matches).any(|c| c.diverged());
        if ran && !observed {
            stale.push(format!(
                "{}/{} no longer diverges, but is declared to (switch: {}). Either the \
                 compatibility path stopped doing anything -- in which case the switch does not \
                 exist, per EMULATION_CORES section 5 -- or this declaration should be deleted.",
                decl.suite, decl.rom, decl.switch
            ));
        }
    }
    stale
}

/// Divergences with no matching entry in `declared`.
pub fn undeclared_divergences(
    compared: &[Comparison],
    declared: &[DeclaredDivergence],
) -> Vec<Comparison> {
    compared
        .iter()
        .filter(|c| {
            c.diverged()
                && !declared
                    .iter()
                    .any(|d| d.suite == c.suite && (d.rom == "*" || d.rom == c.rom))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmp(suite: &str, rom: &str, a: &str, c: &str) -> Comparison {
        Comparison {
            suite: suite.into(),
            rom: rom.into(),
            accuracy: a.into(),
            compatibility: c.into(),
        }
    }

    const DECL: &[DeclaredDivergence] = &[DeclaredDivergence {
        suite: "declared_suite",
        rom: "declared_rom",
        switch: "test switch",
        reason: "test",
    }];

    #[test]
    fn a_declared_divergence_that_still_happens_is_clean() {
        let compared = vec![cmp("declared_suite", "declared_rom", "PASSED", "FAILED")];
        assert!(undeclared_divergences(&compared, DECL).is_empty());
        assert!(stale_declarations(&compared, DECL).is_empty());
    }

    /// The obvious direction: compatibility broke something nobody
    /// authorised it to break.
    #[test]
    fn an_undeclared_divergence_is_reported() {
        let compared = vec![
            cmp("declared_suite", "declared_rom", "PASSED", "FAILED"),
            cmp("other_suite", "other_rom", "PASSED", "FAILED"),
        ];
        let undeclared = undeclared_divergences(&compared, DECL);
        assert_eq!(undeclared.len(), 1);
        assert_eq!(undeclared[0].suite, "other_suite");
    }

    /// The direction that inverts the usual instinct, and the one that
    /// keeps this from becoming decoration: the switch stopped doing
    /// anything, everything agrees, and that is a FAILURE because
    /// EMULATION_CORES §5 says a switch nothing can see is not a switch.
    #[test]
    fn a_declared_divergence_that_stopped_happening_is_reported() {
        let compared = vec![cmp("declared_suite", "declared_rom", "PASSED", "PASSED")];
        assert!(
            undeclared_divergences(&compared, DECL).is_empty(),
            "nothing diverged, so nothing is undeclared"
        );
        let stale = stale_declarations(&compared, DECL);
        assert_eq!(
            stale.len(),
            1,
            "a converged switch must be reported, not celebrated"
        );
        assert!(stale[0].contains("no longer diverges"));
    }

    /// A mirror outage is not evidence about a switch.
    #[test]
    fn an_unfetched_rom_neither_confirms_nor_falsifies_a_declaration() {
        assert!(stale_declarations(&[], DECL).is_empty());
    }

    /// Guards the real list, not a synthetic one: a declaration with no
    /// reason is a declaration nobody has to justify.
    #[test]
    fn every_real_declaration_carries_a_switch_and_a_reason() {
        for d in DECLARED {
            assert!(
                !d.switch.trim().is_empty(),
                "{}/{} has no switch",
                d.suite,
                d.rom
            );
            assert!(
                d.reason.trim().len() > 40,
                "{}/{} has no substantive reason",
                d.suite,
                d.rom
            );
        }
    }
}
