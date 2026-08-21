//! Translation and accessibility overlays (ticket W8-12; profile `[text]`,
//! `docs/design/GAME_PROFILES.md` §2).
//!
//! ## The honesty problem here is sharper than usual
//!
//! Every other enhancement changes how the game *looks*. This one changes
//! what the game **says** — and a player who cannot tell a translation
//! from the original has been told something the game never told them.
//! Criterion 3 makes that mechanical: *"text an overlay replaces is
//! identifiable in the ledger, so a player can always tell what was
//! changed"*.
//!
//! So [`LedgerEntry`] carries the **original string verbatim**, not a hash
//! and not a region id. A ledger that said "region `hud_score` was
//! replaced" would satisfy a careless reading of the criterion and fail
//! the player it exists for: they still could not read what the game
//! actually said. The profile therefore records `original` as text
//! (`rf_profiles::schema::TextEntry::original`), and the ledger quotes it
//! back.
//!
//! ## Off by default, and structurally absent from Accuracy Mode
//!
//! Law 6. [`Overlays::default`] is disabled, and a profile **cannot**
//! enable it — `[text]` has no `enabled` field, exactly as `[widescreen]`
//! and `[mods]` have none. The profile describes what a translation would
//! be; the user decides whether to have one.
//!
//! [`Overlays::resolve`] returns [`Resolution::Unchanged`] for everything
//! when disabled, so a caller never has to remember to check a flag first
//! — forgetting that is precisely how an enhancement leaks into Accuracy
//! Mode, and it is the mistake `widescreen::decide_all` is shaped to
//! prevent too.
//!
//! ## Translation and accessibility are separate choices
//!
//! A player may want plain-language rewording without a translation, or a
//! translation without it. They are two [`Mode`] values rather than one
//! "text help" switch, and a test asserts enabling one does not enable
//! the other.

use std::collections::BTreeMap;

use rf_profiles::schema::{Profile, Text, TextEntry};

use crate::experiments::HonestyLabel;

/// Trust-ladder name and honesty-label id for text overlays.
pub const TEXT_OVERLAY: &str = "text_overlay";

/// The claim a surface must show whenever an overlay acted.
///
/// `invents: true`, and that is not a close call: a translated line is
/// text the machine never produced. Stitching reveals real pixels from
/// earlier; this substitutes words.
pub const TEXT_OVERLAY_LABEL: HonestyLabel = HonestyLabel {
    feature: TEXT_OVERLAY,
    claim: "Text on screen has been replaced. The game's own words are in the ledger.",
    invents: true,
};

/// Which kind of replacement the user asked for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Mode {
    /// No overlay. The default, and what Accuracy Mode always uses.
    #[default]
    Off,
    /// Replace with the profile's translation for this language tag.
    Translate(String),
    /// Replace with the profile's accessibility rewording.
    Accessible,
}

/// What happened to one region's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// The game's own text is shown.
    Unchanged,
    /// Replaced. The original is in the ledger, and in this value.
    Replaced { original: String, shown: String },
}

impl Resolution {
    /// `true` when the player is looking at something the game did not
    /// say.
    #[must_use]
    pub fn is_replaced(&self) -> bool {
        matches!(self, Resolution::Replaced { .. })
    }
}

/// One ledger row: an overlay that acted.
///
/// **`original` is the game's text verbatim** — see the module doc. This
/// is criterion 3's whole content, and it is why the profile schema
/// records the original as a string rather than only as a hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    /// Which `[[text.region]]` acted, so the row says *where*.
    pub region: String,
    /// What the game said.
    pub original: String,
    /// What the player saw instead.
    pub shown: String,
    /// Which mode produced it, so a translation and an accessibility
    /// rewording are distinguishable after the fact.
    pub mode: Mode,
}

/// Why a `[text]` table could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayError {
    /// An entry names a region the profile never declared. Refused rather
    /// than ignored: a typo'd region id would otherwise silently disable
    /// one line of a translation, which the author would discover only by
    /// reading every line in-game.
    UnknownRegion { region: String, entry: String },
    /// Two entries claim the same (region, original) pair, so which one
    /// wins would depend on file order.
    DuplicateEntry { region: String, original: String },
}

impl std::fmt::Display for OverlayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OverlayError::UnknownRegion { region, entry } => write!(
                f,
                "text entry {entry:?} names region {region:?}, which no [[text.region]] declares"
            ),
            OverlayError::DuplicateEntry { region, original } => write!(
                f,
                "two entries in region {region} both replace {original:?}"
            ),
        }
    }
}

impl std::error::Error for OverlayError {}

/// The overlay engine.
#[derive(Debug, Clone, Default)]
pub struct Overlays {
    /// **Off by default (law 6).** A profile cannot set this; only a user
    /// can.
    mode: Mode,
    /// `(region, original)` → entry.
    entries: BTreeMap<(String, String), TextEntry>,
    regions: BTreeMap<String, ()>,
    ledger: Vec<LedgerEntry>,
}

impl Overlays {
    /// Read a profile's `[text]` table.
    ///
    /// The result is **disabled**: a profile declares what a translation
    /// would be and never switches it on. Same rule as `[widescreen]` and
    /// `[mods]`.
    ///
    /// # Errors
    /// [`OverlayError`] for an entry naming an undeclared region, or two
    /// entries competing for one string.
    pub fn from_profile(profile: &Profile) -> Result<Self, OverlayError> {
        let mut out = Self::default();
        let Some(text) = profile.text.as_ref() else {
            return Ok(out);
        };
        out.load(text)?;
        Ok(out)
    }

    fn load(&mut self, text: &Text) -> Result<(), OverlayError> {
        for r in &text.regions {
            self.regions.insert(r.id.clone(), ());
        }
        for e in &text.entries {
            if !self.regions.contains_key(&e.region) {
                return Err(OverlayError::UnknownRegion {
                    region: e.region.clone(),
                    entry: e.original.clone(),
                });
            }
            let key = (e.region.clone(), e.original.clone());
            if self.entries.contains_key(&key) {
                return Err(OverlayError::DuplicateEntry {
                    region: e.region.clone(),
                    original: e.original.clone(),
                });
            }
            self.entries.insert(key, e.clone());
        }
        Ok(())
    }

    /// Turn an overlay on or off. **The user's call, never the profile's.**
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// The current mode.
    #[must_use]
    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    /// Every replacement that has happened, in order.
    #[must_use]
    pub fn ledger(&self) -> &[LedgerEntry] {
        &self.ledger
    }

    /// The claim a surface must display while any overlay is acting.
    ///
    /// `None` when nothing has been replaced, so a UI cannot badge a
    /// screen that is showing the game's own words.
    #[must_use]
    pub fn label(&self) -> Option<HonestyLabel> {
        (!self.ledger.is_empty()).then_some(TEXT_OVERLAY_LABEL)
    }

    /// Decide what to show for `original` in `region`, ledgering any
    /// replacement.
    ///
    /// Returns [`Resolution::Unchanged`] whenever the mode is
    /// [`Mode::Off`], **without consulting the table at all** — so a
    /// caller cannot forget to check first, and Accuracy Mode is absent
    /// by construction rather than by discipline.
    pub fn resolve(&mut self, region: &str, original: &str) -> Resolution {
        if self.mode == Mode::Off {
            return Resolution::Unchanged;
        }
        let Some(entry) = self
            .entries
            .get(&(region.to_string(), original.to_string()))
        else {
            return Resolution::Unchanged;
        };
        let shown = match &self.mode {
            Mode::Off => return Resolution::Unchanged,
            Mode::Translate(lang) => entry.translations.get(lang).cloned(),
            Mode::Accessible => entry.accessible.clone(),
        };
        // A profile that has no string for THIS mode leaves the game's own
        // text alone. Falling back to another language would show the
        // player words from a language they did not ask for and ledger it
        // as though they had.
        let Some(shown) = shown else {
            return Resolution::Unchanged;
        };
        self.ledger.push(LedgerEntry {
            region: region.to_string(),
            original: original.to_string(),
            shown: shown.clone(),
            mode: self.mode.clone(),
        });
        Resolution::Replaced {
            original: original.to_string(),
            shown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_profiles::schema::{TextEntry, TextRegion};

    fn table() -> Text {
        let mut translations = BTreeMap::new();
        translations.insert("fr".to_string(), "PARTIE TERMINEE".to_string());
        Text {
            regions: vec![TextRegion {
                id: "banner".into(),
                x: 0,
                y: 0,
                width: 256,
                height: 16,
            }],
            entries: vec![TextEntry {
                region: "banner".into(),
                original: "GAME OVER".into(),
                translations,
                accessible: Some("The game has ended.".into()),
            }],
        }
    }

    fn loaded() -> Overlays {
        let mut o = Overlays::default();
        o.load(&table()).unwrap();
        o
    }

    /// **Criterion 2, and law 6.**
    #[test]
    fn a_freshly_loaded_overlay_set_is_off_and_replaces_nothing() {
        let mut o = loaded();
        assert_eq!(*o.mode(), Mode::Off);
        assert_eq!(o.resolve("banner", "GAME OVER"), Resolution::Unchanged);
        assert!(
            o.ledger().is_empty(),
            "an overlay that is off must not ledger - and must not act"
        );
        assert!(
            o.label().is_none(),
            "nothing was replaced, so nothing to badge"
        );
    }

    /// **Criterion 3**, and the reason `original` is a string.
    #[test]
    fn the_ledger_quotes_the_games_own_words_verbatim() {
        let mut o = loaded();
        o.set_mode(Mode::Translate("fr".into()));
        let r = o.resolve("banner", "GAME OVER");
        assert_eq!(
            r,
            Resolution::Replaced {
                original: "GAME OVER".into(),
                shown: "PARTIE TERMINEE".into()
            }
        );
        let row = &o.ledger()[0];
        assert_eq!(row.original, "GAME OVER");
        assert_eq!(row.shown, "PARTIE TERMINEE");
        assert_eq!(row.region, "banner");
        // A ledger naming only the region would pass a careless reading of
        // criterion 3 and still leave the player unable to read what the
        // game said.
        assert!(
            row.original.contains("GAME OVER"),
            "the ledger must carry the original TEXT, not just its location"
        );
    }

    #[test]
    fn accessibility_and_translation_are_separate_choices() {
        let mut o = loaded();
        o.set_mode(Mode::Accessible);
        assert_eq!(
            o.resolve("banner", "GAME OVER"),
            Resolution::Replaced {
                original: "GAME OVER".into(),
                shown: "The game has ended.".into()
            }
        );
        // The French string exists, but Accessible mode must not reach it.
        assert_eq!(o.ledger()[0].shown, "The game has ended.");
        assert_eq!(o.ledger()[0].mode, Mode::Accessible);
    }

    #[test]
    fn a_missing_string_for_this_mode_leaves_the_game_alone() {
        // Falling back to another language would show words the player
        // never asked for and ledger them as if they had.
        let mut o = loaded();
        o.set_mode(Mode::Translate("de".into()));
        assert_eq!(o.resolve("banner", "GAME OVER"), Resolution::Unchanged);
        assert!(o.ledger().is_empty());
    }

    #[test]
    fn text_with_no_entry_is_untouched() {
        let mut o = loaded();
        o.set_mode(Mode::Translate("fr".into()));
        assert_eq!(o.resolve("banner", "1UP"), Resolution::Unchanged);
        assert!(o.ledger().is_empty());
    }

    #[test]
    fn the_label_appears_only_once_something_was_replaced() {
        let mut o = loaded();
        o.set_mode(Mode::Translate("fr".into()));
        assert!(o.label().is_none());
        o.resolve("banner", "GAME OVER");
        let label = o.label().expect("a replacement must carry its claim");
        assert!(label.invents, "replaced text is not what the machine drew");
        assert_eq!(label.feature, TEXT_OVERLAY);
    }

    #[test]
    fn an_entry_naming_an_undeclared_region_is_refused() {
        // A typo'd region id would otherwise silently disable one line,
        // discoverable only by reading every line in-game.
        let mut t = table();
        t.entries[0].region = "bannner".into();
        let mut o = Overlays::default();
        assert_eq!(
            o.load(&t).unwrap_err(),
            OverlayError::UnknownRegion {
                region: "bannner".into(),
                entry: "GAME OVER".into()
            }
        );
    }

    #[test]
    fn two_entries_for_one_string_are_refused() {
        // Otherwise which one wins depends on file order.
        let mut t = table();
        let dup = t.entries[0].clone();
        t.entries.push(dup);
        let mut o = Overlays::default();
        assert_eq!(
            o.load(&t).unwrap_err(),
            OverlayError::DuplicateEntry {
                region: "banner".into(),
                original: "GAME OVER".into()
            }
        );
    }

    /// Parse a real profile, the way `experiments.rs` does — which also
    /// proves the `[text]` table actually reaches `Profile` through
    /// serde. A struct literal would test this module while leaving the
    /// schema wiring unproven, and an unwired table is exactly the
    /// failure this ticket's scope amendment existed to prevent
    /// (`Profile` has no `deny_unknown_fields`, so a table nothing
    /// deserialises is silently ignored).
    fn profile_with_text() -> Profile {
        let toml = "\
[meta]
profile_version = \"0.1\"
title = \"T\"
console = \"nes\"
region = \"ntsc\"

[[text.region]]
id = \"banner\"
x = 0
y = 0
width = 256
height = 16

[[text.entry]]
region = \"banner\"
original = \"GAME OVER\"
accessible = \"The game has ended.\"
translations = { fr = \"PARTIE TERMINEE\" }
";
        rf_profiles::load_str(toml).expect("valid profile").profile
    }

    #[test]
    fn a_text_table_actually_reaches_the_profile_through_serde() {
        // The anti-vacuity check for the schema half of this ticket.
        let p = profile_with_text();
        let t = p.text.as_ref().expect("[text] must deserialise");
        assert_eq!(t.regions.len(), 1);
        assert_eq!(t.entries[0].original, "GAME OVER");
        assert_eq!(
            t.entries[0].translations.get("fr").unwrap(),
            "PARTIE TERMINEE"
        );
    }

    #[test]
    fn a_profile_declaring_text_still_loads_disabled() {
        // The law-6 structural point: a profile describes, the user
        // decides. There is no [text].enabled to set.
        let mut o = Overlays::from_profile(&profile_with_text()).unwrap();
        assert_eq!(*o.mode(), Mode::Off);
        assert_eq!(o.resolve("banner", "GAME OVER"), Resolution::Unchanged);
    }

    #[test]
    fn a_profile_declared_overlay_acts_once_the_user_turns_it_on() {
        // Criterion 1 end to end: profile TOML -> schema -> engine ->
        // ledger. Anti-vacuity for the test above, which would pass on an
        // engine that could never act at all.
        let mut o = Overlays::from_profile(&profile_with_text()).unwrap();
        o.set_mode(Mode::Translate("fr".into()));
        assert!(o.resolve("banner", "GAME OVER").is_replaced());
        assert_eq!(o.ledger()[0].original, "GAME OVER");
    }

    #[test]
    fn a_profile_without_a_text_table_is_not_an_error() {
        let toml = "[meta]\nprofile_version = \"0.1\"\ntitle = \"T\"\n\
                    console = \"nes\"\nregion = \"ntsc\"\n";
        let p = rf_profiles::load_str(toml).expect("valid profile").profile;
        let o = Overlays::from_profile(&p).unwrap();
        assert_eq!(*o.mode(), Mode::Off);
        assert!(o.ledger().is_empty());
    }

    #[test]
    fn turning_an_overlay_back_off_stops_it_acting() {
        // FR-ENH-010: every enhancement independently toggleable at
        // runtime. The ledger keeps what already happened — a ledger that
        // erased itself on toggle would lose exactly the record criterion
        // 3 asks for.
        let mut o = loaded();
        o.set_mode(Mode::Translate("fr".into()));
        o.resolve("banner", "GAME OVER");
        o.set_mode(Mode::Off);
        assert_eq!(o.resolve("banner", "GAME OVER"), Resolution::Unchanged);
        assert_eq!(
            o.ledger().len(),
            1,
            "history is not erased by switching off"
        );
    }
}
