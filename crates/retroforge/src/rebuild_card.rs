//! The Rebuild card's words (ticket W37-03): what the Enhancements panel says
//! about a game running through its rebuilt code map.
//!
//! Pure, like `enhance_panel`: the status goes in, plain lines come out, and the
//! egui layer only paints them. The rule the layout keeps is the ticket's: the
//! card **never says "rebuilt" without the tier mix and the verification state
//! next to it**. Both are lines of the same [`RebuildCard`], produced together,
//! and a test sweeps every status to prove that no card holds the word without
//! them. The detailed view (counters, dispatch misses, naming, hooks) stays in
//! the debugger's Coverage tab.

use rf_rebuild::Verification;

use crate::rebuild::{AtlasSource, RebuildInfo, RebuildStatus, ShadowStatus, SuspendReason};

/// What the card shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildCard {
    /// The bold first line.
    pub headline: String,
    /// The lines under it, in order.
    pub lines: Vec<String>,
    /// The game passed the proven threshold (`rf_rebuild::PROVEN_FRAMES` over
    /// `PROVEN_RUNS` runs, zero differences): show the badge.
    pub proven: bool,
    /// The checker found a difference: show the warning colour.
    pub diverged: bool,
}

impl RebuildCard {
    /// Everything the card says, for tests.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = self.headline.clone();
        for l in &self.lines {
            out.push('\n');
            out.push_str(l);
        }
        out
    }
}

/// The card for a switch that is `enabled` and a core reporting `status`.
/// `None` when the switch is off and nothing is running: the row's own sentence
/// is all there is to say.
#[must_use]
pub fn card(enabled: bool, status: &RebuildStatus) -> Option<RebuildCard> {
    match status {
        RebuildStatus::Off if !enabled => None,
        RebuildStatus::Off | RebuildStatus::Building => Some(RebuildCard {
            headline: "Building the code map\u{2026}".to_string(),
            lines: vec![
                "The map is built and installed while the game plays; until then it runs on the plain emulator."
                    .to_string(),
                format!(
                    "Verification: {}",
                    verification_words(Verification::NotChecked)
                ),
            ],
            proven: false,
            diverged: false,
        }),
        RebuildStatus::Failed(why) => Some(RebuildCard {
            headline: "Rebuild did not start".to_string(),
            lines: vec![
                format!("{why}."),
                "The game runs on the plain emulator.".to_string(),
            ],
            proven: false,
            diverged: false,
        }),
        RebuildStatus::On(info) => Some(on_card(info)),
    }
}

fn on_card(info: &RebuildInfo) -> RebuildCard {
    let mut lines = vec![coverage_line(info), atlas_line(info)];
    lines.extend(tier_lines(info));
    lines.push(format!(
        "Verification: {}",
        verification_words(info.verification)
    ));
    lines.extend(shadow_lines(info));
    if info.rebuilding {
        lines.push("Growing the map from this session\u{2026}".to_string());
    }
    lines.push(
        match info.source {
            AtlasSource::Vectors => "Map source: the game's start-up vectors only.",
            AtlasSource::Cached => "Map source: saved from earlier play.",
            AtlasSource::Coverage => "Map source: recorded play.",
        }
        .to_string(),
    );
    RebuildCard {
        headline: "Running through the rebuilt map".to_string(),
        lines,
        proven: matches!(info.verification, Verification::Proven { .. }),
        diverged: matches!(info.verification, Verification::Diverged { .. }),
    }
}

/// What the shadow check (a plain copy of the game stepped beside the real one,
/// compared every frame) is doing, and what it found.
fn shadow_lines(info: &RebuildInfo) -> Vec<String> {
    let mut out = Vec::new();
    out.push(match &info.shadow {
        ShadowStatus::NotStarted => "Shadow check: not started.".to_string(),
        ShadowStatus::Proven => {
            "Shadow check: off. This game is proven, so no second copy runs.".to_string()
        }
        ShadowStatus::Checking {
            frames,
            divergences: 0,
        } => format!(
            "Shadow check: on. A plain copy runs beside the game and is compared every frame \
             ({frames} frames this session, no difference)."
        ),
        ShadowStatus::Checking {
            frames,
            divergences,
        } => format!(
            "Shadow check: on. {divergences} of {frames} frames this session differed from the \
             plain copy."
        ),
        ShadowStatus::Suspended { reason, .. } => match reason {
            SuspendReason::WriteHook => "Shadow check: not checked: mod active. A mod that \
                writes to the game's memory is loaded, so the plain copy would not match by \
                design. It resumes when the mod is removed."
                .to_string(),
        },
    });
    if info.demoted > 0 {
        out.push(format!(
            "{} function{} moved to the plain emulator after a difference was found.",
            info.demoted,
            if info.demoted == 1 { "" } else { "s" }
        ));
    }
    if let Some(d) = &info.last_difference {
        let what = match d.function_entry {
            Some(rom) => {
                format!("the function at ROM offset {rom:#x} was moved to the plain emulator")
            }
            None => "no single function could be named, so nothing was moved".to_string(),
        };
        let saved = d.bundle.as_ref().map_or_else(String::new, |p| {
            format!(" Saved for a bug report: {}.", p.display())
        });
        out.push(format!(
            "Last difference: frame {} ({}); {what}.{saved}",
            d.frame, d.field
        ));
    }
    out
}

fn kib(bytes: u64) -> String {
    format!("{} KiB", bytes.div_ceil(1024))
}

fn pct(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "0%".to_string();
    }
    format!("{:.1}%", part as f64 * 100.0 / whole as f64)
}

fn coverage_line(info: &RebuildInfo) -> String {
    let f = &info.facts;
    format!(
        "Coverage: {} of the game's code has run ({} of {}).",
        pct(f.observed_bytes, f.prg_len),
        kib(f.observed_bytes),
        kib(f.prg_len)
    )
}

fn atlas_line(info: &RebuildInfo) -> String {
    let f = &info.facts;
    format!(
        "Map: {} functions, {} instructions, {}.",
        f.functions,
        f.instructions,
        kib(f.stored_bytes)
    )
}

/// The tier mix: of the instructions run since the map was installed, the share
/// each tier ran, and why the emulator-only share was.
fn tier_lines(info: &RebuildInfo) -> Vec<String> {
    let s = &info.stats;
    let total = s.instructions();
    if total == 0 {
        return vec!["Tier mix: no instructions counted yet.".to_string()];
    }
    let mut out = vec![format!(
        "Tier mix: {} seen in play, {} worked out from the code, {} on the plain emulator.",
        pct(s.observed, total),
        pct(s.inferred, total),
        pct(s.fallback, total)
    )];
    let reasons: Vec<String> = [
        (s.fallback_ram, "code running from RAM"),
        (s.fallback_no_node, "code not in the map yet"),
        (
            s.fallback_width,
            "code the map holds in another register width",
        ),
        (s.fallback_other, "code in coprocessor or other memory"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, why)| format!("{} {why}", pct(n, total)))
    .collect();
    if !reasons.is_empty() {
        out.push(format!(
            "On the plain emulator because of: {}.",
            reasons.join(", ")
        ));
    }
    out
}

/// The verification state in a sentence.
#[must_use]
pub fn verification_words(v: Verification) -> String {
    match v {
        Verification::NotChecked => "not checked".to_string(),
        Verification::Checked { frames } => format!("checked: {frames} frames in lockstep"),
        Verification::Diverged {
            frames,
            divergences,
        } => format!(
            "checked: {frames} frames in lockstep, and {divergences} of them differed. \
             Rebuild is not trusted for this game"
        ),
        Verification::Proven { frames } => {
            format!("proven: {frames} frames in lockstep, no difference")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rebuild::AtlasFacts;
    use rf_core_api::RebuildStats;

    fn info(verification: Verification, stats: RebuildStats) -> RebuildInfo {
        RebuildInfo {
            facts: AtlasFacts {
                functions: 412,
                instructions: 9_310,
                observed_bytes: 45 * 1024,
                prg_len: 192 * 1024,
                stored_bytes: 61 * 1024,
            },
            verification,
            stats,
            ..RebuildInfo::default()
        }
    }

    fn busy() -> RebuildStats {
        RebuildStats {
            observed: 800,
            inferred: 100,
            fallback: 100,
            fallback_ram: 60,
            fallback_no_node: 40,
            ..RebuildStats::default()
        }
    }

    /// The ticket's rule, over every status: whenever the card says "rebuilt"
    /// it also carries a tier-mix line and a verification line.
    #[test]
    fn the_card_never_says_rebuilt_without_tier_mix_and_verification() {
        let verifications = [
            Verification::NotChecked,
            Verification::Checked { frames: 600 },
            Verification::Diverged {
                frames: 600,
                divergences: 2,
            },
            Verification::Proven { frames: 40_000 },
        ];
        let mut statuses = vec![
            RebuildStatus::Off,
            RebuildStatus::Building,
            RebuildStatus::Failed("not an NES image".to_string()),
        ];
        for v in verifications {
            for stats in [RebuildStats::default(), busy()] {
                statuses.push(RebuildStatus::On(info(v, stats)));
            }
        }
        for enabled in [false, true] {
            for status in &statuses {
                let Some(card) = card(enabled, status) else {
                    continue;
                };
                let text = card.text();
                if text.to_lowercase().contains("rebuilt") {
                    assert!(text.contains("Tier mix:"), "{status:?}: {text}");
                    assert!(text.contains("Verification:"), "{status:?}: {text}");
                }
            }
        }
    }

    #[test]
    fn off_and_unswitched_has_no_card_and_a_switched_one_starts_building() {
        assert!(card(false, &RebuildStatus::Off).is_none());
        let c = card(true, &RebuildStatus::Off).expect("starting");
        assert!(c.headline.contains("Building"));
        assert!(!c.text().to_lowercase().contains("rebuilt"));
        assert!(c.text().contains("not checked"));
    }

    #[test]
    fn a_running_card_gives_coverage_size_tier_mix_reasons_and_verification() {
        let c = card(
            true,
            &RebuildStatus::On(info(Verification::Checked { frames: 600 }, busy())),
        )
        .expect("card");
        let t = c.text();
        assert!(t.contains("Coverage: 23.4% of the game's code has run (45 KiB of 192 KiB)"));
        assert!(t.contains("Map: 412 functions, 9310 instructions, 61 KiB"));
        assert!(t.contains(
            "80.0% seen in play, 10.0% worked out from the code, 10.0% on the plain emulator"
        ));
        assert!(t.contains("6.0% code running from RAM, 4.0% code not in the map yet"));
        assert!(t.contains("Verification: checked: 600 frames in lockstep"));
        assert!(!c.proven && !c.diverged);
    }

    #[test]
    fn only_a_proven_record_earns_the_badge_and_a_difference_is_loud() {
        let status = |v| RebuildStatus::On(info(v, busy()));
        let proven = card(true, &status(Verification::Proven { frames: 40_000 })).expect("card");
        assert!(proven.proven && proven.text().contains("proven: 40000 frames"));
        for v in [
            Verification::NotChecked,
            Verification::Checked { frames: 1_000_000 },
        ] {
            assert!(!card(true, &status(v)).expect("card").proven);
        }
        let bad = card(
            true,
            &status(Verification::Diverged {
                frames: 40_000,
                divergences: 1,
            }),
        )
        .expect("card");
        assert!(!bad.proven && bad.diverged);
        assert!(bad.text().contains("1 of them differed"));
    }

    /// Ticket W37-04: the shadow check's three honest states and what it found
    /// are in plain words, and the suspended one says it is not checked.
    #[test]
    fn the_card_says_what_the_shadow_check_is_doing_and_found() {
        let with = |shadow, demoted, last| {
            let mut i = info(Verification::Checked { frames: 600 }, busy());
            i.shadow = shadow;
            i.demoted = demoted;
            i.last_difference = last;
            card(true, &RebuildStatus::On(i)).expect("card").text()
        };
        let t = with(
            ShadowStatus::Checking {
                frames: 90,
                divergences: 0,
            },
            0,
            None,
        );
        assert!(t.contains("Shadow check: on."), "{t}");
        assert!(t.contains("90 frames this session, no difference"), "{t}");
        let t = with(ShadowStatus::Proven, 0, None);
        assert!(t.contains("Shadow check: off. This game is proven"), "{t}");
        let t = with(
            ShadowStatus::Suspended {
                reason: SuspendReason::WriteHook,
                frames: 5,
                divergences: 0,
            },
            0,
            None,
        );
        assert!(t.contains("not checked: mod active"), "{t}");
        let t = with(
            ShadowStatus::Checking {
                frames: 9,
                divergences: 1,
            },
            1,
            Some(crate::rebuild::ShadowTrouble {
                frame: 7,
                field: "wram".to_string(),
                function_entry: Some(0x1234),
                bundle: Some(std::path::PathBuf::from("/tmp/bundle")),
            }),
        );
        assert!(t.contains("1 of 9 frames"), "{t}");
        assert!(t.contains("1 function moved to the plain emulator"), "{t}");
        assert!(t.contains("Last difference: frame 7 (wram)"), "{t}");
        assert!(t.contains("ROM offset 0x1234"), "{t}");
        assert!(t.contains("/tmp/bundle"), "{t}");
    }
}
