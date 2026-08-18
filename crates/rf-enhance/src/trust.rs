//! The heuristic trust ladder (ticket W3-05c; D-004, FR-ENH-011/012).
//!
//! ## Built as a general mechanism on purpose
//!
//! D-004 is a project-wide law, not an anti-flicker detail: *"every
//! enhancement heuristic ships shadow-first"*. The stitcher's scene-cut
//! detection and the profile decoders' heuristics are subject to exactly
//! the same rule, so this module knows nothing about anti-flicker — a
//! heuristic is a string id, and everything here works the same for any
//! of them. W3-05c's own brief says so explicitly ("design it as a
//! general mechanism rather than an anti-flicker special case").
//!
//! ## The ladder, and the one rule that carries the weight
//!
//! [`TrustState`] is `Shadow → Advisory → Active`, fresh install
//! all-shadow. The load-bearing rule is that **only `Active` may change
//! what the emulator does**:
//!
//! - `Shadow` — "detect + record, never act" (FR-ENH-011, verbatim).
//! - `Advisory` — "badge suggests". A suggestion is a UI event, not an
//!   action, so advisory **still does not act**. This is the distinction
//!   most likely to be got wrong later, which is why
//!   [`TrustLadder::should_act`] is one function every call site must go
//!   through rather than a `match` each of them writes for itself.
//! - `Active` — acts.
//!
//! ## Suppression is deliberately hard to do quietly
//!
//! FR-ENH-012: suppressing a **safety** heuristic per-game "shall require
//! a stored justification that auto-reopens when the trigger recurs in a
//! new scene context". Both halves are enforced here rather than left to
//! a caller's good manners: [`TrustLadder::suppress`] refuses an empty
//! justification, and a suppression records the scene context it was
//! granted in, so [`TrustLadder::is_suppressed`] stops applying the
//! moment the same trigger fires somewhere else.
//!
//! That "somewhere else" is the whole point. A user silencing a warning
//! on one screen has said something about that screen; treating it as a
//! statement about the entire game is how a safety heuristic becomes
//! decorative.
//!
//! ## No telemetry, and nothing here can add any
//!
//! NFR-005. The report card is an in-memory `Vec` this module never
//! flushes anywhere — it has no I/O, no clock, and no network dependency
//! of any kind. Persistence is the *caller's* job, through whatever
//! per-game store it already owns (`retroforge::game_settings`), which is
//! also why [`TrustLadder::to_settings_value`]/[`TrustLadder::from_settings_value`]
//! are plain string conversions rather than file operations.

use std::collections::BTreeMap;

/// Where a heuristic sits on the ladder (D-004).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum TrustState {
    /// Detect and record, never act. The default, everywhere, always —
    /// "fresh install all-shadow" is expressed by this being `Default`
    /// rather than by an initialiser somebody could forget to call.
    #[default]
    Shadow,
    /// A badge may suggest the heuristic's verdict. Still does not act.
    Advisory,
    /// May change what the emulator does.
    Active,
}

impl TrustState {
    /// Parse a persisted name. Unknown text degrades to [`TrustState::Shadow`]
    /// rather than erroring: a settings file written by a newer build (or
    /// hand-edited) must not stop a game loading, and the safe direction
    /// for an unrecognised trust level is unambiguously "trust it least".
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name.trim() {
            "advisory" => TrustState::Advisory,
            "active" => TrustState::Active,
            _ => TrustState::Shadow,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            TrustState::Shadow => "shadow",
            TrustState::Advisory => "advisory",
            TrustState::Active => "active",
        }
    }
}

/// Why a heuristic contradicted itself or its context — the events
/// FR-ENH-012 requires be recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contradiction {
    /// Which heuristic. A plain string so this module never needs to know
    /// the set of heuristics that exist.
    pub heuristic: String,
    /// What happened, in the user's terms — this is report-card text.
    pub detail: String,
    /// The scene context it happened in (see [`TrustLadder::suppress`]).
    pub scene: String,
}

/// A per-game suppression of a safety heuristic (FR-ENH-012).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suppression {
    /// Required, non-empty — enforced by [`TrustLadder::suppress`].
    pub justification: String,
    /// The scene context the user granted it in. A trigger in any other
    /// context reopens it.
    pub granted_in_scene: String,
}

/// Refusals from [`TrustLadder::suppress`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuppressError {
    /// FR-ENH-012's "shall require a stored justification".
    JustificationRequired,
}

impl std::fmt::Display for SuppressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SuppressError::JustificationRequired => write!(
                f,
                "suppressing a safety heuristic requires a written justification (FR-ENH-012)"
            ),
        }
    }
}

impl std::error::Error for SuppressError {}

/// Per-game trust state for every heuristic, plus the report card.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustLadder {
    /// Only non-default states are stored, so a fresh game serialises to
    /// nothing at all and "all-shadow" cannot drift as heuristics are
    /// added.
    states: BTreeMap<String, TrustState>,
    /// Profile-pinned states (FR-ENH-011: "profiles may pin states").
    /// Separate from `states` because a pin is not the user's setting and
    /// must not be written back into their per-game file.
    pinned: BTreeMap<String, TrustState>,
    suppressions: BTreeMap<String, Suppression>,
    report_card: Vec<Contradiction>,
}

impl TrustLadder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The effective state: a profile pin wins over the user's setting,
    /// and absent both it is [`TrustState::Shadow`].
    #[must_use]
    pub fn state(&self, heuristic: &str) -> TrustState {
        self.pinned
            .get(heuristic)
            .or_else(|| self.states.get(heuristic))
            .copied()
            .unwrap_or_default()
    }

    /// Promote or demote a heuristic. Any state is reachable in either
    /// direction — the ladder describes trust earned, not a one-way door,
    /// and a user who regrets promoting something must be able to undo it.
    pub fn set_state(&mut self, heuristic: &str, state: TrustState) {
        if state == TrustState::Shadow {
            self.states.remove(heuristic);
        } else {
            self.states.insert(heuristic.to_string(), state);
        }
    }

    /// Pin a state from a profile (FR-ENH-011).
    pub fn pin(&mut self, heuristic: &str, state: TrustState) {
        self.pinned.insert(heuristic.to_string(), state);
    }

    /// **The one question every call site must ask.** May this heuristic
    /// change what the emulator does, right now, in `scene`?
    ///
    /// `Advisory` returns `false` — a badge is not an action. Keeping
    /// that judgement in one function is the point: a call site writing
    /// its own `match` is exactly how "advisory" quietly starts acting.
    #[must_use]
    pub fn should_act(&self, heuristic: &str, scene: &str) -> bool {
        self.state(heuristic) == TrustState::Active && !self.is_suppressed(heuristic, scene)
    }

    /// Record a contradiction to the local report card (FR-ENH-012). No
    /// telemetry, no I/O — see the module doc.
    pub fn record_contradiction(&mut self, heuristic: &str, detail: &str, scene: &str) {
        self.report_card.push(Contradiction {
            heuristic: heuristic.to_string(),
            detail: detail.to_string(),
            scene: scene.to_string(),
        });
    }

    #[must_use]
    pub fn report_card(&self) -> &[Contradiction] {
        &self.report_card
    }

    /// Suppress a safety heuristic for this game, in this scene context.
    ///
    /// # Errors
    /// [`SuppressError::JustificationRequired`] if `justification` is
    /// empty or whitespace — FR-ENH-012 says "shall require a stored
    /// justification", and a blank string is not one.
    pub fn suppress(
        &mut self,
        heuristic: &str,
        justification: &str,
        scene: &str,
    ) -> Result<(), SuppressError> {
        if justification.trim().is_empty() {
            return Err(SuppressError::JustificationRequired);
        }
        self.suppressions.insert(
            heuristic.to_string(),
            Suppression {
                justification: justification.trim().to_string(),
                granted_in_scene: scene.to_string(),
            },
        );
        Ok(())
    }

    /// Is this heuristic suppressed **in this scene**?
    ///
    /// A suppression granted in one scene does not apply in another —
    /// FR-ENH-012's "auto-reopens when the trigger recurs in a new scene
    /// context", expressed as a query rather than as a background job
    /// that has to remember to run.
    #[must_use]
    pub fn is_suppressed(&self, heuristic: &str, scene: &str) -> bool {
        self.suppressions
            .get(heuristic)
            .is_some_and(|s| s.granted_in_scene == scene)
    }

    #[must_use]
    pub fn suppression(&self, heuristic: &str) -> Option<&Suppression> {
        self.suppressions.get(heuristic)
    }

    /// Serialise the *user's* state for a per-game settings store.
    ///
    /// Pins are excluded (they belong to a profile, not the user) and so
    /// is the report card (it is a session observation, not a setting).
    /// Empty when everything is at its default, so a game nobody has
    /// touched writes no key at all.
    #[must_use]
    pub fn to_settings_value(&self) -> String {
        let mut parts: Vec<String> = self
            .states
            .iter()
            .map(|(k, v)| format!("{k}={}", v.name()))
            .collect();
        for (k, s) in &self.suppressions {
            // `|` separates records, `=` separates fields, so a
            // justification containing either would corrupt the line —
            // strip them rather than escape, since a justification is
            // prose a human wrote and losing a pipe from it costs nothing.
            let justification = s.justification.replace(['|', '=', '\n'], " ");
            let scene = s.granted_in_scene.replace(['|', '=', '\n'], " ");
            parts.push(format!("suppress:{k}={scene}:{justification}"));
        }
        parts.join("|")
    }

    /// Inverse of [`TrustLadder::to_settings_value`]. Unparseable records
    /// are skipped rather than failing the load — same reasoning as
    /// [`TrustState::from_name`]: a settings file must never stop a game
    /// from starting, and the failure direction is toward less trust.
    #[must_use]
    pub fn from_settings_value(value: &str) -> Self {
        let mut ladder = TrustLadder::new();
        for record in value.split('|').filter(|r| !r.trim().is_empty()) {
            let Some((key, rest)) = record.split_once('=') else {
                continue;
            };
            if let Some(heuristic) = key.strip_prefix("suppress:") {
                let (scene, justification) = rest.split_once(':').unwrap_or((rest, ""));
                if justification.trim().is_empty() {
                    // A suppression with no justification is not a
                    // suppression (FR-ENH-012) — drop it, which reopens
                    // the heuristic. Failing safe here matters more than
                    // honouring a malformed file.
                    continue;
                }
                ladder.suppressions.insert(
                    heuristic.to_string(),
                    Suppression {
                        justification: justification.trim().to_string(),
                        granted_in_scene: scene.to_string(),
                    },
                );
            } else {
                ladder.set_state(key, TrustState::from_name(rest));
            }
        }
        ladder
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAFETY: &str = "anti-flicker";
    const SCENE_A: &str = "scene-a";
    const SCENE_B: &str = "scene-b";

    /// D-004's "fresh install all-shadow", expressed where it cannot be
    /// forgotten: the DEFAULT, for a heuristic nobody has ever mentioned.
    #[test]
    fn a_fresh_ladder_is_all_shadow_for_any_heuristic() {
        let ladder = TrustLadder::new();
        for name in ["anti-flicker", "scene-cut", "never-heard-of-it"] {
            assert_eq!(ladder.state(name), TrustState::Shadow, "{name}");
            assert!(!ladder.should_act(name, SCENE_A), "{name} must not act");
        }
    }

    /// The rule that carries the weight, asserted for every rung rather
    /// than only the two obvious ones: **advisory does not act.**
    #[test]
    fn only_active_may_act_advisory_is_still_a_suggestion() {
        let mut ladder = TrustLadder::new();
        assert!(!ladder.should_act(SAFETY, SCENE_A), "shadow must not act");

        ladder.set_state(SAFETY, TrustState::Advisory);
        assert_eq!(ladder.state(SAFETY), TrustState::Advisory);
        assert!(
            !ladder.should_act(SAFETY, SCENE_A),
            "ADVISORY MUST NOT ACT — a badge is a suggestion, not a change to the emulation"
        );

        ladder.set_state(SAFETY, TrustState::Active);
        assert!(ladder.should_act(SAFETY, SCENE_A), "active acts");
    }

    /// FR-ENH-011: "profiles may pin states", and a pin must beat the
    /// user's own setting or it is not a pin.
    #[test]
    fn a_profile_pin_overrides_the_users_state_without_overwriting_it() {
        let mut ladder = TrustLadder::new();
        ladder.set_state(SAFETY, TrustState::Active);
        ladder.pin(SAFETY, TrustState::Shadow);
        assert_eq!(ladder.state(SAFETY), TrustState::Shadow, "pin wins");
        assert!(!ladder.should_act(SAFETY, SCENE_A));
        assert!(
            ladder.to_settings_value().contains("active"),
            "the user's own setting must survive the pin, not be overwritten by it"
        );
    }

    /// FR-ENH-012, first half: a blank justification is not a
    /// justification.
    #[test]
    fn suppression_requires_a_real_justification() {
        let mut ladder = TrustLadder::new();
        assert_eq!(
            ladder.suppress(SAFETY, "   ", SCENE_A),
            Err(SuppressError::JustificationRequired)
        );
        assert_eq!(
            ladder.suppress(SAFETY, "", SCENE_A),
            Err(SuppressError::JustificationRequired)
        );
        assert!(
            ladder.suppression(SAFETY).is_none(),
            "nothing may be stored"
        );
        assert!(ladder
            .suppress(SAFETY, "flickers on purpose here", SCENE_A)
            .is_ok());
        assert_eq!(
            ladder.suppression(SAFETY).unwrap().justification,
            "flickers on purpose here"
        );
    }

    /// FR-ENH-012, second half and the one with teeth: the suppression
    /// **auto-reopens** in a new scene context.
    #[test]
    fn a_suppression_does_not_follow_the_user_into_a_new_scene() {
        let mut ladder = TrustLadder::new();
        ladder.set_state(SAFETY, TrustState::Active);
        ladder
            .suppress(SAFETY, "intentional blink in this cutscene", SCENE_A)
            .unwrap();

        assert!(ladder.is_suppressed(SAFETY, SCENE_A));
        assert!(
            !ladder.should_act(SAFETY, SCENE_A),
            "suppressed where granted"
        );

        assert!(
            !ladder.is_suppressed(SAFETY, SCENE_B),
            "AUTO-REOPEN: silencing a warning on one screen says nothing about another"
        );
        assert!(
            ladder.should_act(SAFETY, SCENE_B),
            "the heuristic must be live again in a new scene context"
        );
    }

    #[test]
    fn contradictions_land_on_a_local_report_card() {
        let mut ladder = TrustLadder::new();
        assert!(ladder.report_card().is_empty());
        ladder.record_contradiction(SAFETY, "blink period violated", SCENE_A);
        ladder.record_contradiction("scene-cut", "reset mid-scroll", SCENE_B);
        assert_eq!(ladder.report_card().len(), 2);
        assert_eq!(ladder.report_card()[0].heuristic, SAFETY);
        assert_eq!(ladder.report_card()[1].scene, SCENE_B);
    }

    /// Per-game persistence (FR-ENH-011) — round trip, including the
    /// suppression and its justification.
    #[test]
    fn state_and_suppressions_round_trip_through_a_settings_value() {
        let mut ladder = TrustLadder::new();
        ladder.set_state(SAFETY, TrustState::Active);
        ladder.set_state("scene-cut", TrustState::Advisory);
        ladder
            .suppress(SAFETY, "deliberate strobe", SCENE_A)
            .unwrap();
        ladder.record_contradiction(SAFETY, "not persisted", SCENE_A);

        let restored = TrustLadder::from_settings_value(&ladder.to_settings_value());
        assert_eq!(restored.state(SAFETY), TrustState::Active);
        assert_eq!(restored.state("scene-cut"), TrustState::Advisory);
        assert_eq!(
            restored.suppression(SAFETY).unwrap().justification,
            "deliberate strobe"
        );
        assert!(restored.is_suppressed(SAFETY, SCENE_A));
        assert!(
            !restored.is_suppressed(SAFETY, SCENE_B),
            "scope survives the round trip"
        );
        assert!(
            restored.report_card().is_empty(),
            "the report card is a session observation, not a setting"
        );
    }

    /// A game nobody has configured writes nothing, so "all-shadow"
    /// cannot rot as heuristics are added later.
    #[test]
    fn an_untouched_game_serialises_to_nothing() {
        assert_eq!(TrustLadder::new().to_settings_value(), "");
        assert_eq!(TrustLadder::from_settings_value(""), TrustLadder::new());
    }

    /// A corrupt or newer-format file must not stop a game loading, and
    /// must fail toward LESS trust rather than more.
    #[test]
    fn unparseable_records_degrade_toward_shadow_rather_than_failing() {
        let ladder = TrustLadder::from_settings_value(
            "anti-flicker=teleport|garbage-with-no-equals|scene-cut=active",
        );
        assert_eq!(
            ladder.state("anti-flicker"),
            TrustState::Shadow,
            "an unknown trust level must read as the least trusting one"
        );
        assert_eq!(
            ladder.state("scene-cut"),
            TrustState::Active,
            "valid records still load"
        );

        // A stored suppression with no justification is not a
        // suppression — dropping it REOPENS the heuristic, which is the
        // safe direction.
        let no_justification = TrustLadder::from_settings_value("suppress:anti-flicker=scene-a:");
        assert!(!no_justification.is_suppressed(SAFETY, SCENE_A));
    }
}
