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

// ---------------------------------------------------------------------
// Ticket W30-03: how wide, and where the margins come from.
// ---------------------------------------------------------------------

/// The widest picture the SNES PPU path will draw for itself, in dots
/// (mirrors `rf_snes::ppu::MAX_WIDTH`, which this crate cannot import).
/// An aspect wider than this is finished by [`fill_margins`], not the PPU.
pub const CORE_MAX_WIDTH: usize = 512;

/// A target screen shape for the widened picture (ticket W30-03). 16:9 is
/// the default and the only one a fresh install ever uses; the wider two
/// are opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Aspect {
    /// 16:9 - the ordinary widescreen TV.
    #[default]
    Wide16x9,
    /// 21:9 - ultrawide monitors.
    Wide21x9,
    /// 32:9 - super-ultrawide monitors.
    Wide32x9,
}

impl Aspect {
    /// Every choice, narrowest first (the order the picker lists them).
    pub const ALL: [Aspect; 3] = [Aspect::Wide16x9, Aspect::Wide21x9, Aspect::Wide32x9];

    /// Width : height as integers.
    #[must_use]
    pub const fn ratio(self) -> (u32, u32) {
        match self {
            Aspect::Wide16x9 => (16, 9),
            Aspect::Wide21x9 => (21, 9),
            Aspect::Wide32x9 => (32, 9),
        }
    }

    /// The label a picker shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Aspect::Wide16x9 => "16:9",
            Aspect::Wide21x9 => "21:9",
            Aspect::Wide32x9 => "32:9",
        }
    }

    /// The key a settings file stores. 16:9 is the default and is never
    /// written.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Aspect::Wide16x9 => "16:9",
            Aspect::Wide21x9 => "21:9",
            Aspect::Wide32x9 => "32:9",
        }
    }

    /// Parse a stored key; an unknown one is `None` (the caller keeps the
    /// default rather than guessing).
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Aspect::ALL.into_iter().find(|a| a.key() == key)
    }

    /// Dots across for a picture `height` lines tall.
    ///
    /// The arithmetic: the screen is `ratio.0 : ratio.1`, one dot is
    /// `pixel_aspect` times as wide as it is tall on screen (`1.0` for
    /// square dots, `8/7` for the TV setting), so
    ///
    /// ```text
    /// width = height * (ratio.0 / ratio.1) / pixel_aspect
    /// ```
    ///
    /// rounded UP to a multiple of 8 - a whole tile, and so also even, so
    /// the margin splits evenly either side of the 256-dot picture. At 224
    /// lines and square dots that gives 400 / 528 / 800 (from 398.2 /
    /// 522.7 / 796.4). A zero height or a non-positive or non-finite
    /// `pixel_aspect` is treated as `1.0` / refused (`0`) respectively, so
    /// a bad input can never produce a runaway width.
    #[must_use]
    pub fn output_width(self, height: usize, pixel_aspect: f32) -> usize {
        if height == 0 {
            return 0;
        }
        let par = if pixel_aspect.is_finite() && pixel_aspect > 0.0 {
            f64::from(pixel_aspect)
        } else {
            1.0
        };
        let (num, den) = self.ratio();
        let exact = height as f64 * f64::from(num) / f64::from(den) / par;
        // Subtract a hair before the ceiling so an exact multiple (e.g.
        // 288 lines at 16:9 = 512.0) is not bumped by float noise.
        let tiles = ((exact - 1e-6) / 8.0).ceil().max(1.0);
        tiles as usize * 8
    }
}

/// Where the widescreen margins' pixels come from, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarginSource {
    /// A decoded level image, indexed by the live camera.
    DecodedLevel,
    /// The stitched canvas of the scene being played.
    StitchedCanvas,
    /// What the PPU drew from VRAM - the fallback, and the whole story
    /// when nothing better is known.
    Vram,
}

/// What is known right now, for [`choose_margin_source`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarginFacts {
    /// A profile decoded this game's level and its image is available.
    pub level_decoded: bool,
    /// The live camera (the level position of the picture) was read.
    pub camera_known: bool,
    /// The stitcher placed the current frame on its canvas.
    pub canvas_has_frame_position: bool,
    /// The canvas snapshot belongs to the scene being shown now. False
    /// right after a scene change, until the canvas has re-stitched it.
    pub scene_fresh: bool,
}

/// Pick the margin source. Pure.
///
/// A decoded level wins when it is decoded and the camera is known (it is
/// complete; a stitched canvas only has what was visited). Otherwise the
/// stitched canvas, but only when it has this frame's position **and**
/// belongs to the current scene - a canvas from the previous room would
/// paint the wrong level into the margins. Otherwise VRAM.
#[must_use]
pub fn choose_margin_source(facts: MarginFacts) -> MarginSource {
    if facts.level_decoded && facts.camera_known {
        MarginSource::DecodedLevel
    } else if facts.canvas_has_frame_position && facts.scene_fresh {
        MarginSource::StitchedCanvas
    } else {
        MarginSource::Vram
    }
}

/// Compose a widescreen frame of `out_w` x `h` RGBA with its margins taken
/// from a world image rather than VRAM.
///
/// `native` is the console's own picture (`native_w` x `h` RGBA, possibly
/// already VRAM-widened) placed in the centre. The console's unwidened
/// 256-column picture sits at `pad_256 = (out_w - 256) / 2`; its left
/// edge is `frame_world_origin` in world space. Every output pixel
/// OUTSIDE those 256 columns comes from
/// `world(origin.x + (x - pad_256), origin.y + y)` when that is `Some`;
/// else from the native pixel if `x` falls inside the native picture; else
/// opaque black. Rows where `keep_native_row(y)` is true (HUD bands) never
/// take world pixels. The result is always `out_w * h * 4` bytes.
#[must_use]
pub fn fill_margins(
    native: &[u8],
    native_w: usize,
    h: usize,
    out_w: usize,
    frame_world_origin: (i64, i64),
    world: &dyn Fn(i64, i64) -> Option<[u8; 3]>,
    keep_native_row: &dyn Fn(usize) -> bool,
) -> Vec<u8> {
    let mut out = vec![0u8; out_w * h * 4];
    let pad = (out_w as i64 - native_w as i64).div_euclid(2);
    let pad_256 = (out_w as i64 - i64::from(NARROW_WIDTH)).div_euclid(2);
    let central = pad_256..pad_256 + i64::from(NARROW_WIDTH);
    // Bounded `for` ranges only (law 8): every pixel is visited once.
    for y in 0..h {
        let keep = keep_native_row(y);
        for x in 0..out_w {
            let xi = x as i64;
            let at = (y * out_w + x) * 4;
            let from_world = if keep || central.contains(&xi) {
                None
            } else {
                world(
                    frame_world_origin.0 + (xi - pad_256),
                    frame_world_origin.1 + y as i64,
                )
            };
            let rgb = from_world.or_else(|| {
                let nx = xi - pad;
                if nx < 0 || nx >= native_w as i64 {
                    return None;
                }
                let src = (y * native_w + nx as usize) * 4;
                native.get(src..src + 3).map(|p| [p[0], p[1], p[2]])
            });
            let [r, g, b] = rgb.unwrap_or([0, 0, 0]);
            out[at..at + 4].copy_from_slice(&[r, g, b, 0xFF]);
        }
    }
    out
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

    // ---- Ticket W30-03 ----

    #[test]
    fn aspect_widths_at_224_lines_square_dots() {
        assert_eq!(Aspect::Wide16x9.output_width(224, 1.0), 400);
        assert_eq!(Aspect::Wide21x9.output_width(224, 1.0), 528);
        assert_eq!(Aspect::Wide32x9.output_width(224, 1.0), 800);
    }

    #[test]
    fn tv_pixels_need_fewer_dots_and_bad_inputs_are_safe() {
        // 224 * 16/9 / (8/7) = 348.4 -> 352.
        assert_eq!(Aspect::Wide16x9.output_width(224, 8.0 / 7.0), 352);
        assert_eq!(Aspect::Wide16x9.output_width(0, 1.0), 0);
        assert_eq!(
            Aspect::Wide16x9.output_width(224, 0.0),
            400,
            "a zero pixel aspect falls back to square dots"
        );
        assert_eq!(Aspect::Wide16x9.output_width(224, f32::NAN), 400);
        // An exact multiple is not bumped: 288 * 16/9 = 512.
        assert_eq!(Aspect::Wide16x9.output_width(288, 1.0), 512);
        for a in Aspect::ALL {
            assert_eq!(a.output_width(224, 1.0) % 8, 0, "{a:?}");
        }
    }

    #[test]
    fn aspect_keys_round_trip_and_default_is_16_9() {
        assert_eq!(Aspect::default(), Aspect::Wide16x9);
        for a in Aspect::ALL {
            assert_eq!(Aspect::from_key(a.key()), Some(a));
        }
        assert_eq!(Aspect::from_key("4:3"), None);
    }

    fn facts(level: bool, cam: bool, pos: bool, fresh: bool) -> MarginFacts {
        MarginFacts {
            level_decoded: level,
            camera_known: cam,
            canvas_has_frame_position: pos,
            scene_fresh: fresh,
        }
    }

    #[test]
    fn margin_source_choice_covers_every_branch() {
        use MarginSource::*;
        // Decoded level wins when decoded AND the camera is known, even
        // if the canvas is also good.
        assert_eq!(
            choose_margin_source(facts(true, true, true, true)),
            DecodedLevel
        );
        assert_eq!(
            choose_margin_source(facts(true, true, false, false)),
            DecodedLevel
        );
        // Decoded but no camera: cannot index it - fall to the canvas.
        assert_eq!(
            choose_margin_source(facts(true, false, true, true)),
            StitchedCanvas
        );
        assert_eq!(choose_margin_source(facts(true, false, false, false)), Vram);
        // No decoded level: canvas only with position AND a fresh scene.
        assert_eq!(
            choose_margin_source(facts(false, true, true, true)),
            StitchedCanvas
        );
        assert_eq!(
            choose_margin_source(facts(false, false, true, true)),
            StitchedCanvas
        );
        assert_eq!(choose_margin_source(facts(false, false, false, true)), Vram);
        // A scene change: never the canvas until it has re-stitched.
        assert_eq!(choose_margin_source(facts(false, false, true, false)), Vram);
        assert_eq!(choose_margin_source(facts(false, true, true, false)), Vram);
        assert_eq!(
            choose_margin_source(facts(false, false, false, false)),
            Vram
        );
    }

    /// A 4-wide native frame of one colour, 2 rows.
    fn native_of(rgb: [u8; 3], w: usize, h: usize) -> Vec<u8> {
        (0..w * h)
            .flat_map(|_| [rgb[0], rgb[1], rgb[2], 0xFF])
            .collect()
    }

    fn px(out: &[u8], out_w: usize, x: usize, y: usize) -> [u8; 3] {
        let i = (y * out_w + x) * 4;
        [out[i], out[i + 1], out[i + 2]]
    }

    #[test]
    fn margins_come_from_the_world_and_the_centre_stays_native() {
        // out 300 wide, native 256: pad 22. World colour encodes the
        // world x so the mapping is checkable.
        let native = native_of([9, 9, 9], 256, 2);
        let world = |wx: i64, _wy: i64| Some([(wx & 0xFF) as u8, 1, 2]);
        let out = fill_margins(&native, 256, 2, 300, (1000, 0), &world, &|_| false);
        assert_eq!(out.len(), 300 * 2 * 4);
        // x = 0 is 22 columns left of the picture's left edge (world 1000).
        assert_eq!(px(&out, 300, 0, 0), [((1000 - 22) & 0xFF) as u8, 1, 2]);
        assert_eq!(px(&out, 300, 21, 0), [((1000 - 1) & 0xFF) as u8, 1, 2]);
        // The central 256 are the console's own picture.
        assert_eq!(px(&out, 300, 22, 0), [9, 9, 9]);
        assert_eq!(px(&out, 300, 277, 1), [9, 9, 9]);
        // Right margin starts at world 1000 + 256.
        assert_eq!(px(&out, 300, 278, 0), [((1000 + 256) & 0xFF) as u8, 1, 2]);
    }

    #[test]
    fn without_world_pixels_vram_shows_then_black() {
        // native 300 wide (VRAM-widened), out 400: pad 50, pad_256 72.
        let native = native_of([5, 6, 7], 300, 1);
        let none = |_: i64, _: i64| None;
        let out = fill_margins(&native, 300, 1, 400, (0, 0), &none, &|_| false);
        assert_eq!(px(&out, 400, 0, 0), [0, 0, 0], "beyond the VRAM width");
        assert_eq!(px(&out, 400, 49, 0), [0, 0, 0]);
        assert_eq!(px(&out, 400, 50, 0), [5, 6, 7], "within the VRAM width");
        assert_eq!(px(&out, 400, 349, 0), [5, 6, 7]);
        assert_eq!(px(&out, 400, 350, 0), [0, 0, 0]);
        assert_eq!(out[3], 0xFF, "opaque");
    }

    #[test]
    fn hud_rows_never_take_world_pixels() {
        let native = native_of([5, 6, 7], 256, 3);
        let world = |_: i64, _: i64| Some([200, 200, 200]);
        let out = fill_margins(&native, 256, 3, 300, (0, 0), &world, &|y| y == 1);
        assert_eq!(px(&out, 300, 0, 0), [200, 200, 200]);
        assert_eq!(
            px(&out, 300, 0, 1),
            [0, 0, 0],
            "HUD row: no native there, so black"
        );
        assert_eq!(px(&out, 300, 30, 1), [5, 6, 7], "HUD row keeps native");
        assert_eq!(px(&out, 300, 0, 2), [200, 200, 200]);
    }

    #[test]
    fn world_gaps_fall_back_to_native_and_origin_y_is_applied() {
        let native = native_of([5, 6, 7], 300, 2);
        // Only row world-y == 11 has data.
        let world = |_: i64, wy: i64| (wy == 11).then_some([1, 2, 3]);
        let out = fill_margins(&native, 300, 2, 400, (0, 10), &world, &|_| false);
        assert_eq!(
            px(&out, 400, 60, 0),
            [5, 6, 7],
            "y=0 -> world 10: gap -> native"
        );
        assert_eq!(px(&out, 400, 60, 1), [1, 2, 3], "y=1 -> world 11");
    }

    #[test]
    fn degenerate_sizes_do_not_panic() {
        let none = |_: i64, _: i64| None;
        assert!(fill_margins(&[], 0, 0, 0, (0, 0), &none, &|_| false).is_empty());
        // Short native buffer: missing pixels are black, no panic.
        let out = fill_margins(&[1, 2, 3, 255], 256, 1, 256, (0, 0), &none, &|_| false);
        assert_eq!(out.len(), 256 * 4);
        assert_eq!(px(&out, 256, 0, 0), [1, 2, 3]);
        assert_eq!(px(&out, 256, 1, 0), [0, 0, 0]);
    }
}
