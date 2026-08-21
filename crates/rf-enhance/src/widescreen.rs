//! Per-BG-layer widescreen policies (ticket W8-05, FR-ENH-006's family).
//!
//! ## Widescreen shows what the hardware never drew
//!
//! That sentence is the whole design. A 4:3 SNES frame is 256 dots wide;
//! a widescreen frame asks the PPU for columns outside that, and whatever
//! is there was never part of the picture. For a scrolling background with
//! a tilemap wider than the screen, those columns hold real level geometry
//! the player was always going to see a moment later — showing them early
//! is the feature. For a HUD, a status bar, or a static full-screen image,
//! they hold whatever happens to sit next in VRAM. Same operation, same
//! registers, opposite answers. **So the decision is per layer, and it
//! cannot be a global switch.**
//!
//! ## Modelled on bsnes-hd, read rather than recalled
//!
//! This ticket's own note guessed the policy set as "extend, repeat,
//! letterbox". Its criterion says "on the bsnes-hd model", so bsnes-hd's
//! README was read (law 2), and the real option set is different and
//! better: per-BG `off` / `on` / scanline-conditional / `autoHor` /
//! `autoHor&Ver`, with sprites carrying their own `clip` / `safe` /
//! `unsafe` / `disable`.
//!
//! **The `auto` policies are this ticket's third criterion**, already
//! solved by the emulator it names: bsnes-hd disables widescreen for a
//! background that "spans full width with horizontal position 0", because
//! such a layer is a HUD or a full-screen image and widening it reveals
//! nothing real. That is a *detection*, not a declaration — which is what
//! makes it safe as a default for profiles nobody has hand-tuned.
//!
//! ## Law 6
//!
//! [`WidescreenPolicies::default`] is **disabled**, so a fresh install
//! renders 4:3 and Accuracy Mode is untouched. Enabling it is an explicit
//! act, and even then every layer starts on [`BgPolicy::AutoBoth`] —
//! bsnes-hd's own default — rather than on unconditional widening.

use rf_profiles::schema::Profile;

/// Visible width of a 4:3 SNES frame, in dots.
pub const NARROW_WIDTH: u16 = 256;

/// What to do with one background layer in the widescreen columns.
///
/// Names follow bsnes-hd's, so that a profile author reading its
/// documentation finds the same words here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BgPolicy {
    /// `off` — the layer stays 4:3. The right answer for a HUD.
    Off,
    /// `on` — widened unconditionally. Correct only when the author knows
    /// the tilemap carries real content out there.
    On,
    /// `autoHor` — widened unless the layer spans the full width at
    /// horizontal position 0.
    AutoHorizontal,
    /// `autoHor&Ver` — the same test on both axes, and bsnes-hd's default,
    /// so it is this crate's too.
    #[default]
    AutoBoth,
}

/// What to do with sprites in the widescreen columns.
///
/// bsnes-hd states plainly that "Objects/Sprites will not be visible
/// correctly in the widescreen areas" — OAM gives a sprite one X in a
/// 256-wide space, so there is no such thing as a sprite that was meant to
/// be out there. Hence a *conservative* default rather than `On`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ObjPolicy {
    /// `clip` — cut sprites at the 4:3 edge.
    Clip,
    /// `safe` — draw a sprite only if part of it falls inside the classic
    /// area. bsnes-hd's default, and this crate's.
    #[default]
    Safe,
    /// `unsafe` — draw sprites wholly in the widescreen columns too.
    /// bsnes-hd notes this "causes artifacts in many games".
    Unsafe,
    /// `disable` — draw no sprites at all (screenshot use).
    Disable,
}

impl BgPolicy {
    /// Parse a profile's spelling. Unknown values are refused rather than
    /// defaulted, because silently treating a typo as "off" gives a
    /// profile author a 4:3 layer and no reason why.
    pub fn from_name(name: &str) -> Result<Self, PolicyError> {
        Ok(match name {
            "off" => BgPolicy::Off,
            "on" => BgPolicy::On,
            "auto_hor" | "autoHor" => BgPolicy::AutoHorizontal,
            "auto" | "auto_hor_ver" | "autoHor&Ver" => BgPolicy::AutoBoth,
            other => return Err(PolicyError::UnknownBg(other.to_string())),
        })
    }
}

impl ObjPolicy {
    /// Parse a profile's spelling.
    pub fn from_name(name: &str) -> Result<Self, PolicyError> {
        Ok(match name {
            "clip" => ObjPolicy::Clip,
            "safe" => ObjPolicy::Safe,
            "unsafe" => ObjPolicy::Unsafe,
            "disable" => ObjPolicy::Disable,
            other => return Err(PolicyError::UnknownObj(other.to_string())),
        })
    }
}

/// A policy name a profile used that this build does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    UnknownBg(String),
    UnknownObj(String),
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PolicyError::UnknownBg(n) => write!(
                f,
                "`{n}` is not a background widescreen policy (off, on, auto_hor, auto)"
            ),
            PolicyError::UnknownObj(n) => write!(
                f,
                "`{n}` is not a sprite widescreen policy (clip, safe, unsafe, disable)"
            ),
        }
    }
}

impl std::error::Error for PolicyError {}

/// What a layer looks like on the frame being decided, as far as the
/// widescreen question is concerned.
///
/// Deliberately tiny: the auto policies need only whether the layer covers
/// the whole screen and where it is scrolled to. Passing the PPU in would
/// make this untestable without a machine, and would invite the decision
/// to start depending on things that are not part of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerView {
    /// The layer's tilemap width in dots (256 for a 32-tile map, 512 for
    /// 64, and so on).
    pub tilemap_width: u16,
    /// The layer's tilemap height in dots.
    pub tilemap_height: u16,
    pub hofs: u16,
    pub vofs: u16,
    /// Whether the layer is on the main screen at all.
    pub enabled: bool,
}

/// Why a layer is or is not widened, kept together so a UI can explain
/// itself instead of just showing a narrower picture than expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Widen this layer into the extra columns.
    Widen,
    /// Keep the layer at 4:3, for this reason.
    KeepNarrow(&'static str),
}

impl Decision {
    #[must_use]
    pub fn widens(&self) -> bool {
        matches!(self, Decision::Widen)
    }

    /// The reason, for a UI or a log. `None` when the layer is widened.
    #[must_use]
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Decision::Widen => None,
            Decision::KeepNarrow(why) => Some(why),
        }
    }
}

/// **Criterion 3.** Decide one layer, naming the reason when the answer is
/// "keep it narrow".
///
/// The `auto` test is bsnes-hd's: a background that spans the full width
/// at horizontal position 0 is not scrolling content, it is a HUD or a
/// static image, and the columns beyond 256 hold nothing it ever meant to
/// show. `AutoBoth` applies the same reasoning vertically, which catches a
/// full-screen image that happens to sit at a non-zero `hofs`.
///
/// A disabled layer is reported as narrow rather than widened, so a caller
/// counting widened layers cannot be fooled by one that draws nothing.
#[must_use]
pub fn decide(policy: BgPolicy, view: LayerView) -> Decision {
    if !view.enabled {
        return Decision::KeepNarrow("layer is not on the main screen");
    }
    let full_width = view.tilemap_width <= NARROW_WIDTH;
    let full_height = view.tilemap_height <= NARROW_WIDTH;
    match policy {
        BgPolicy::Off => Decision::KeepNarrow("policy is off"),
        BgPolicy::On => Decision::Widen,
        BgPolicy::AutoHorizontal => {
            if full_width && view.hofs == 0 {
                Decision::KeepNarrow(
                    "tilemap is only as wide as the screen and is not scrolled: \
                     the extra columns would show VRAM the hardware never drew",
                )
            } else {
                Decision::Widen
            }
        }
        BgPolicy::AutoBoth => {
            if full_width && view.hofs == 0 && full_height && view.vofs == 0 {
                Decision::KeepNarrow(
                    "tilemap is screen-sized and unscrolled on both axes - a HUD or a \
                     static image, whose widescreen columns are undrawn VRAM",
                )
            } else {
                Decision::Widen
            }
        }
    }
}

/// The whole per-frame policy set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WidescreenPolicies {
    /// **Off by default (law 6)**: a fresh install renders 4:3, and
    /// Accuracy Mode never consults this at all.
    pub enabled: bool,
    pub bg: [BgPolicy; 4],
    pub obj: ObjPolicy,
}

impl WidescreenPolicies {
    /// Read a profile's `[widescreen]` table.
    ///
    /// **A profile may declare policies but cannot enable widescreen** —
    /// `enabled` stays false here regardless. That is the same rule
    /// `[mods]` follows: the profile offers, the user chooses, and law 6's
    /// "a fresh install boots in Accuracy Mode" survives a profile that
    /// would rather it did not.
    pub fn from_profile(profile: &Profile) -> Result<Self, PolicyError> {
        let mut out = Self::default();
        let Some(ws) = profile.widescreen.as_ref() else {
            return Ok(out);
        };
        for (i, name) in [&ws.bg1, &ws.bg2, &ws.bg3, &ws.bg4].into_iter().enumerate() {
            if let Some(n) = name {
                out.bg[i] = BgPolicy::from_name(n)?;
            }
        }
        if let Some(n) = ws.obj.as_ref() {
            out.obj = ObjPolicy::from_name(n)?;
        }
        Ok(out)
    }

    /// Decide every layer at once.
    ///
    /// Returns narrow decisions for all four when disabled, so a caller
    /// never has to remember to check `enabled` first — forgetting that is
    /// exactly how an enhancement leaks into Accuracy Mode.
    #[must_use]
    pub fn decide_all(&self, views: [LayerView; 4]) -> [Decision; 4] {
        if !self.enabled {
            return std::array::from_fn(|_| Decision::KeepNarrow("widescreen is off"));
        }
        std::array::from_fn(|i| decide(self.bg[i], views[i]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrolling_level() -> LayerView {
        LayerView {
            tilemap_width: 512,
            tilemap_height: 512,
            hofs: 64,
            vofs: 0,
            enabled: true,
        }
    }

    fn hud() -> LayerView {
        LayerView {
            tilemap_width: 256,
            tilemap_height: 256,
            hofs: 0,
            vofs: 0,
            enabled: true,
        }
    }

    /// **Criterion 2, and law 6.** A fresh install renders 4:3.
    #[test]
    fn the_default_policies_render_four_by_three() {
        let p = WidescreenPolicies::default();
        assert!(!p.enabled, "widescreen is off until someone turns it on");
        assert_eq!(p.bg, [BgPolicy::AutoBoth; 4], "bsnes-hd's own default");
        assert_eq!(p.obj, ObjPolicy::Safe);

        // And with it off, no layer widens no matter how good a candidate.
        let decisions = p.decide_all([scrolling_level(); 4]);
        assert!(
            decisions.iter().all(|d| !d.widens()),
            "a disabled policy set must not widen anything - forgetting to check \
             `enabled` first is exactly how an enhancement leaks into Accuracy Mode"
        );
    }

    /// **Criterion 3.** The layer that would reveal undrawn VRAM is
    /// identified, and the reason is available rather than implied.
    #[test]
    fn a_hud_layer_is_kept_narrow_and_says_why() {
        let d = decide(BgPolicy::AutoBoth, hud());
        assert!(
            !d.widens(),
            "a screen-sized unscrolled layer must not widen"
        );
        let why = d.reason().expect("a narrow decision carries its reason");
        assert!(
            why.contains("VRAM") || why.contains("static"),
            "the reason should say what would have been shown: {why}"
        );

        // The same policy widens a real scrolling level, or it would be
        // useless rather than merely safe.
        assert!(decide(BgPolicy::AutoBoth, scrolling_level()).widens());
    }

    /// `autoHor` and `autoHor&Ver` differ on exactly one case, and it is
    /// the case that distinguishes them: a full-screen image parked at a
    /// non-zero vertical scroll.
    #[test]
    fn auto_horizontal_and_auto_both_differ_only_on_the_vertical_test() {
        let scrolled_vertically = LayerView { vofs: 32, ..hud() };
        assert!(
            !decide(BgPolicy::AutoHorizontal, scrolled_vertically).widens(),
            "autoHor looks only at width and hofs, and both still say HUD"
        );
        assert!(
            decide(BgPolicy::AutoBoth, scrolled_vertically).widens(),
            "autoHor&Ver sees the vertical scroll and treats it as content"
        );
    }

    /// `off` and `on` are unconditional, which is the point of having them
    /// alongside the automatic ones.
    #[test]
    fn the_explicit_policies_override_the_detection() {
        assert!(!decide(BgPolicy::Off, scrolling_level()).widens());
        assert!(
            decide(BgPolicy::On, hud()).widens(),
            "`on` is how an author overrules the detection when they know better"
        );
    }

    /// A layer that draws nothing is reported narrow, so a caller counting
    /// widened layers cannot be fooled by a disabled one.
    #[test]
    fn a_layer_that_is_not_on_the_main_screen_is_never_widened() {
        let off_screen = LayerView {
            enabled: false,
            ..scrolling_level()
        };
        let d = decide(BgPolicy::On, off_screen);
        assert!(!d.widens());
        assert_eq!(d.reason(), Some("layer is not on the main screen"));
    }

    /// An unknown policy name is refused, not defaulted: silently treating
    /// a typo as "off" hands the author a 4:3 layer and no reason why.
    #[test]
    fn an_unknown_policy_name_is_refused() {
        assert_eq!(
            BgPolicy::from_name("letterbox"),
            Err(PolicyError::UnknownBg("letterbox".to_string())),
            "even a plausible-sounding name from this ticket's own note is not \
             one of bsnes-hd's, and guessing would be worse than refusing"
        );
        assert!(ObjPolicy::from_name("on").is_err());
        assert_eq!(BgPolicy::from_name("auto").unwrap(), BgPolicy::AutoBoth);
        assert_eq!(ObjPolicy::from_name("safe").unwrap(), ObjPolicy::Safe);
    }

    /// A profile may DECLARE policies; it may not switch widescreen on.
    #[test]
    fn a_profile_cannot_enable_widescreen_by_itself() {
        let toml = r#"
[meta]
profile_version = "0.1"
title = "T"
console = "snes"
region = "ntsc"

[widescreen]
bg1 = "on"
bg2 = "off"
obj = "unsafe"
"#;
        let outcome = rf_profiles::load_str(toml).expect("loads");
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        let p = WidescreenPolicies::from_profile(&outcome.profile).expect("valid policies");
        assert_eq!(p.bg[0], BgPolicy::On);
        assert_eq!(p.bg[1], BgPolicy::Off);
        assert_eq!(
            p.bg[2],
            BgPolicy::AutoBoth,
            "an absent layer keeps the default"
        );
        assert_eq!(p.obj, ObjPolicy::Unsafe);
        assert!(
            !p.enabled,
            "law 6: a profile describes what widescreen should look like, it does \
             not get to turn it on"
        );
    }

    /// A profile naming a policy this build does not know is refused with
    /// the name in the message.
    #[test]
    fn a_profile_with_a_bad_policy_name_is_refused() {
        let toml = r#"
[meta]
profile_version = "0.1"
title = "T"
console = "snes"
region = "ntsc"

[widescreen]
bg1 = "sideways"
"#;
        let outcome = rf_profiles::load_str(toml).expect("the TOML itself is fine");
        let err = WidescreenPolicies::from_profile(&outcome.profile).unwrap_err();
        assert!(err.to_string().contains("sideways"), "{err}");
    }
}
