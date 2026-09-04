//! The in-GUI profile editor: create a profile from nothing, edit it,
//! validate it against the real loader, and save only what the loader
//! accepts (ticket W9-02; `docs/design/FRONTEND_UI.md` §3.5).
//!
//! §3.5 gives the author workspace an "embedded text editor" and a
//! "profile inspector (identity match status, capability checklist,
//! validation output)". [`crate::authoring`] already built the *external*
//! half of that loop — watch a file, re-decode on save. This module is the
//! other half: the file can now be brought into existence and changed
//! without leaving the window.
//!
//! ## The text buffer is the single source of truth
//!
//! The tempting shape is a typed `Profile`-like struct behind the form
//! fields, serialized to TOML on save. It is the wrong shape here, for a
//! reason worth stating because it is not obvious:
//!
//! * A second in-memory model means two writers for one file, and the
//!   moment the author types into the text pane the two disagree. Which
//!   one wins on save is then a coin-flip the author cannot see.
//! * An emitter built from `schema.rs`'s Rust field names would emit
//!   `ty = "u16"`. The wire name is `type` — `MemoryMapEntry` carries a
//!   serde rename. `ty` is not a *hard* error: it deserializes as a
//!   missing field, and before that `shape::unknown_keys` reports it as a
//!   warning. An editor whose output warns is precisely what criterion 2
//!   forbids, and it would fail quietly.
//!
//! So: one pinned [`SKELETON`] template using wire names, checked by a
//! test that runs the real loader and asserts zero warnings, and every
//! form control is a *patch onto the buffer* rather than a parallel model.
//!
//! ## Validation is the loader itself, never a copy of it
//!
//! [`Draft::check`] calls `rf_profiles::loader::load_str`. It does not
//! re-derive "is this valid" — a reimplementation is a second source of
//! truth that drifts, and the drift shows up as the editor blessing a
//! file the loader then refuses. [`Draft::save_to`] runs the same check
//! and writes nothing on failure, which is the actual content of
//! criterion 3: the error is *removed*, not moved later.
//!
//! **The loader is fail-fast**, so a failing check yields exactly one
//! diagnostic, never a list — it returns at the first problem, so a file
//! with three faults is fixed in three passes. Warnings are the opposite:
//! they are collected exhaustively, but only on the success path, so an
//! invalid draft shows no warnings at all. Both are honest limits of the
//! loader rather than of this module, and the UI is worded for them.

use std::path::{Path, PathBuf};

use rf_profiles::loader::{self, LoadOutcome};
pub use rf_profiles::schema::Console;

/// The template a brand-new profile starts from.
///
/// Deliberately a literal rather than generated: it is the one place wire
/// names are written down on the writing side, and `skeleton_loads_clean`
/// pins it against the real loader. `{TITLE}` / `{CONSOLE}` / `{REGION}` /
/// `{AUTHOR}` / `{SOURCE}` are substituted by [`NewProfileForm::to_toml`].
///
/// It carries no `[[identity]]` and no `[[memory_map]]` rows on purpose.
/// Both are optional to the loader, and both have a required-field rule
/// that an empty placeholder row would trip immediately (`[[identity]]`
/// needs a hash family; every map row needs a `source`). A new profile
/// that fails validation on its first frame teaches the author that the
/// validator is noise.
pub const SKELETON: &str = "\
# Written by the RetroForge profile editor (schema v0,
# docs/design/GAME_PROFILES.md §2).
#
# Every [[memory_map]] and [[rom_map]] row needs a `source` citation
# (FR-PROF-003): where the address came from, in documentation you are
# allowed to read. The loader refuses a row without one.

[meta]
profile_version = \"0.1\"
title = {TITLE}
console = {CONSOLE}
region = {REGION}
authors = [{AUTHOR}]
sources = [{SOURCE}]

[capabilities]
full_level = false
hud_separation = false
entity_overlay = false
widescreen = \"none\"
fast_load = false
smooth_camera = false
";

/// The "new profile" form: the handful of fields that have no sensible
/// default because only the author knows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProfileForm {
    pub title: String,
    pub console: Console,
    pub region: String,
    pub author: String,
    pub source: String,
}

impl Default for NewProfileForm {
    fn default() -> Self {
        Self {
            title: String::new(),
            console: Console::Nes,
            region: "ntsc".to_string(),
            author: String::new(),
            source: String::new(),
        }
    }
}

impl NewProfileForm {
    /// Render the form onto [`SKELETON`].
    #[must_use]
    pub fn to_toml(&self) -> String {
        SKELETON
            .replace("{TITLE}", &basic_string(&self.title))
            .replace("{CONSOLE}", &basic_string(console_wire(self.console)))
            .replace("{REGION}", &basic_string(&self.region))
            .replace("{AUTHOR}", &basic_string(&self.author))
            .replace("{SOURCE}", &basic_string(&self.source))
    }
}

/// The wire spelling of a [`Console`], which is the lowercase one —
/// `schema.rs` puts `#[serde(rename_all = "lowercase")]` on the enum, and
/// writing `"Nes"` here would produce a file the loader rejects.
#[must_use]
pub fn console_wire(console: Console) -> &'static str {
    match console {
        Console::Nes => "nes",
        Console::Snes => "snes",
    }
}

/// Quote and escape a value as a TOML basic string.
///
/// A title with a quote or a backslash in it — `"Zelda \ II"`, a Windows
/// path in a `source` citation — would otherwise emit a file that does not
/// parse. The editor would then reject the author's own typing with a TOML
/// syntax error they did not write, which reads as a bug in the editor
/// because it is one.
#[must_use]
pub fn basic_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7F => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The result of validating a draft: the loader's own verdict, nothing
/// added and nothing interpreted.
#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    /// The loader accepted it. `warnings` are its unknown-key warnings —
    /// empty for a profile the editor wrote and did not have keys typed
    /// into by hand.
    Accepted { warnings: Vec<String> },
    /// The loader refused it. One diagnostic, because the loader is
    /// fail-fast; it is `ProfileError`'s own `Display`, verbatim, so what
    /// the editor shows and what the CLI would print are the same string.
    Refused { diagnostic: String },
}

impl Check {
    #[must_use]
    pub fn is_accepted(&self) -> bool {
        matches!(self, Check::Accepted { .. })
    }

    /// The refusal diagnostic, if this is one.
    #[must_use]
    pub fn diagnostic(&self) -> Option<&str> {
        match self {
            Check::Refused { diagnostic } => Some(diagnostic),
            Check::Accepted { .. } => None,
        }
    }
}

/// A save the editor declined to perform, carrying the reason.
///
/// Distinct from an I/O error on purpose: nothing was written and nothing
/// is half-written, so there is no file state to recover. The caller shows
/// `diagnostic` where the author is already looking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveRefused {
    pub diagnostic: String,
}

/// An open editing buffer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    text: String,
    path: Option<PathBuf>,
    saved_text: Option<String>,
}

impl Draft {
    /// A new, unsaved profile built from the form.
    #[must_use]
    pub fn from_form(form: &NewProfileForm) -> Self {
        Self {
            text: form.to_toml(),
            path: None,
            saved_text: None,
        }
    }

    /// A new, unsaved profile from TOML text somebody else produced.
    ///
    /// Ticket W13-02f uses it for the annotation → skeleton export: the
    /// skeleton has never been a file, so it has no path, and it is dirty
    /// from the first frame because nothing on disk matches it yet. That
    /// is [`Draft::from_form`]'s state exactly, with the text coming from
    /// [`rf_debugger::profile_export::export_skeleton`] instead of a form.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self {
            text: text.to_string(),
            path: None,
            saved_text: None,
        }
    }

    /// Open an existing profile for editing.
    ///
    /// Reads the file's *bytes as text* rather than loading and
    /// re-rendering it: a profile carries comments — provenance, in this
    /// project's own profiles, at length — and a load/emit round trip
    /// would silently delete every one of them.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let text = std::fs::read_to_string(&path)?;
        Ok(Self {
            saved_text: Some(text.clone()),
            text,
            path: Some(path),
        })
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The buffer, mutably — this is what an `egui::TextEdit::multiline`
    /// binds to.
    pub fn text_mut(&mut self) -> &mut String {
        &mut self.text
    }

    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// True when the buffer differs from what is on disk (or has never
    /// been written).
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.saved_text.as_deref() != Some(self.text.as_str())
    }

    /// Validate through the real loader.
    #[must_use]
    pub fn check(&self) -> Check {
        match loader::load_str(&self.text) {
            Ok(LoadOutcome { warnings, .. }) => Check::Accepted { warnings },
            Err(e) => Check::Refused {
                diagnostic: e.to_string(),
            },
        }
    }

    /// Append a `[[memory_map]]` row.
    ///
    /// Appending is safe regardless of what the buffer already contains:
    /// TOML lets an array-of-tables element appear anywhere after the
    /// tables it is not nested in, so a row added below a `[decode]`
    /// section still belongs to `memory_map`. Inserting it *next to* the
    /// other rows would mean finding the end of a section in text, which
    /// is a parser this module deliberately does not own.
    pub fn append_memory_map_row(
        &mut self,
        addr: u32,
        len: u32,
        ty: &str,
        label: &str,
        source: &str,
    ) {
        self.append_row(
            "memory_map",
            &format!("addr = 0x{addr:04X}"),
            len,
            ty,
            label,
            source,
        );
    }

    /// Append a `[[rom_map]]` row. Keyed by `offset`, not `addr` — a ROM
    /// map indexes the file, a memory map indexes the address space.
    pub fn append_rom_map_row(
        &mut self,
        offset: u32,
        len: u32,
        ty: &str,
        label: &str,
        source: &str,
    ) {
        self.append_row(
            "rom_map",
            &format!("offset = 0x{offset:05X}"),
            len,
            ty,
            label,
            source,
        );
    }

    fn append_row(
        &mut self,
        table: &str,
        first: &str,
        len: u32,
        ty: &str,
        label: &str,
        source: &str,
    ) {
        if !self.text.ends_with('\n') {
            self.text.push('\n');
        }
        self.text.push_str(&format!(
            "\n[[{table}]]\n{first}\nlen = {len}\ntype = {}\nlabel = {}\nsource = {}\n",
            basic_string(ty),
            basic_string(label),
            basic_string(source),
        ));
    }

    /// Append an `[[identity]]` row.
    ///
    /// `sha256` is required by this helper although the schema treats
    /// every hash family as optional, because the loader refuses an entry
    /// that names none: a helper that could emit an empty row would exist
    /// only to produce invalid files.
    pub fn append_identity(&mut self, sha256: &str, revision: Option<&str>) {
        if !self.text.ends_with('\n') {
            self.text.push('\n');
        }
        self.text.push_str(&format!(
            "\n[[identity]]\nsha256 = {}\n",
            basic_string(sha256)
        ));
        if let Some(rev) = revision {
            self.text
                .push_str(&format!("revision = {}\n", basic_string(rev)));
        }
    }

    /// Save to `path`, **refusing to write anything the loader would
    /// refuse to read**.
    ///
    /// The check happens before the file is touched, so a refused save
    /// leaves an existing file byte-identical and does not bring a new one
    /// into existence. That ordering is the criterion, not an
    /// optimisation.
    pub fn save_to(&mut self, path: impl AsRef<Path>) -> Result<Vec<String>, SaveRefused> {
        let warnings = match self.check() {
            Check::Accepted { warnings } => warnings,
            Check::Refused { diagnostic } => return Err(SaveRefused { diagnostic }),
        };
        let path = path.as_ref();
        std::fs::write(path, &self.text).map_err(|e| SaveRefused {
            diagnostic: format!("{}: {e}", path.display()),
        })?;
        self.path = Some(path.to_path_buf());
        self.saved_text = Some(self.text.clone());
        Ok(warnings)
    }

    /// Save back over the file this draft was opened from.
    pub fn save(&mut self) -> Result<Vec<String>, SaveRefused> {
        let Some(path) = self.path.clone() else {
            return Err(SaveRefused {
                diagnostic: "this profile has never been saved — choose a path first".to_string(),
            });
        };
        self.save_to(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rf_profile_editor_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn form() -> NewProfileForm {
        NewProfileForm {
            title: "Test Game".to_string(),
            console: Console::Nes,
            region: "ntsc".to_string(),
            author: "W9-02".to_string(),
            source: "docs/design/GAME_PROFILES.md §2".to_string(),
        }
    }

    /// Criterion 2, and the pin on [`SKELETON`]'s wire names. Zero
    /// warnings is the assertion that matters: a template that spelled a
    /// key wrong would still *load*, and only the warning list would say
    /// so.
    #[test]
    fn skeleton_loads_clean() {
        let draft = Draft::from_form(&form());
        match draft.check() {
            Check::Accepted { warnings } => {
                assert!(warnings.is_empty(), "editor output warns: {warnings:?}");
            }
            Check::Refused { diagnostic } => panic!("editor output does not load: {diagnostic}"),
        }
    }

    /// Criterion 1 end to end: created from the form, edited through the
    /// row helpers, saved, and read back by `rf-profiles` from the file —
    /// with no warnings and with the edits present.
    #[test]
    fn created_edited_and_saved_round_trips_through_the_loader() {
        let dir = scratch("roundtrip");
        let path = dir.join("profile.toml");

        let mut draft = Draft::from_form(&form());
        draft.append_identity(&"a".repeat(64), Some("rev1"));
        draft.append_memory_map_row(
            0x6029,
            2,
            "u16",
            "player_x",
            "docs/design/GAME_PROFILES.md §2 (worked example)",
        );
        draft.append_rom_map_row(0x10010, 64, "tiles", "level_tiles", "the same");
        let warnings = draft.save_to(&path).expect("a valid draft saves");
        assert!(warnings.is_empty(), "saved profile warns: {warnings:?}");

        let outcome = loader::load_file(&path).expect("rf-profiles loads what the editor wrote");
        assert!(
            outcome.warnings.is_empty(),
            "loaded from disk with warnings: {:?}",
            outcome.warnings
        );
        let p = outcome.profile;
        assert_eq!(p.meta.title, "Test Game");
        assert_eq!(p.meta.console, Console::Nes);
        assert_eq!(p.identity.len(), 1);
        assert_eq!(p.identity[0].revision.as_deref(), Some("rev1"));
        assert_eq!(p.memory_map.len(), 1);
        assert_eq!(p.memory_map[0].addr, 0x6029);
        assert_eq!(p.memory_map[0].label, "player_x");
        assert_eq!(p.rom_map.len(), 1);
        assert_eq!(p.rom_map[0].offset, 0x10010);

        assert!(!draft.is_dirty(), "a just-saved draft is clean");
        *draft.text_mut() += "\n";
        assert!(draft.is_dirty(), "an edit after saving is dirty");
    }

    /// Criterion 3, the half that diagnostic-equality cannot test: a
    /// refused save must not bring the file into existence.
    #[test]
    fn a_refused_save_writes_no_file() {
        let dir = scratch("nofile");
        let path = dir.join("profile.toml");
        let mut draft = Draft::from_form(&form());
        // A map row with no `source` — FR-PROF-003's rule, and the most
        // likely mistake an author actually makes.
        *draft.text_mut() +=
            "\n[[memory_map]]\naddr = 0x0300\nlen = 1\ntype = \"u8\"\nlabel = \"lives\"\n";

        let refused = draft
            .save_to(&path)
            .expect_err("an invalid draft is refused");
        assert!(!path.exists(), "a refused save must not create the file");

        // The diagnostic is `MapEntryMissingSource`'s, and it must name
        // the table, the row index AND the label — hardcoded here rather
        // than compared against another `load_str` call, which would be
        // an assertion that cannot fail.
        let d = &refused.diagnostic;
        assert!(d.contains("memory_map"), "diagnostic omits the table: {d}");
        assert!(d.contains("lives"), "diagnostic omits the label: {d}");
        assert!(d.contains("source"), "diagnostic omits the rule: {d}");
    }

    /// The other half: a refused save over an *existing* file leaves it
    /// byte-identical. Truncate-then-write would have destroyed a working
    /// profile in exchange for a diagnostic.
    #[test]
    fn a_refused_save_leaves_an_existing_file_untouched() {
        let dir = scratch("untouched");
        let path = dir.join("profile.toml");
        let mut draft = Draft::from_form(&form());
        draft.save_to(&path).expect("the good version saves");
        let good = std::fs::read(&path).unwrap();

        *draft.text_mut() += "\nthis is not toml at all\n";
        let refused = draft.save().expect_err("broken text is refused");
        assert!(
            refused.diagnostic.to_lowercase().contains("parse")
                || refused.diagnostic.contains("TOML")
                || refused.diagnostic.contains("expected"),
            "a syntax error should read as one: {}",
            refused.diagnostic
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            good,
            "a refused save rewrote the file it refused to save"
        );
    }

    /// The editor's diagnostic *is* the loader's, so what the author sees
    /// while editing is what the CLI prints later.
    #[test]
    fn the_diagnostic_is_the_loaders_own_display() {
        let mut draft = Draft::from_form(&form());
        *draft.text_mut() = "[meta]\ntitle = \"no version\"\n".to_string();
        let expected = loader::load_str(draft.text()).unwrap_err().to_string();
        assert_eq!(draft.check().diagnostic(), Some(expected.as_str()));
        assert!(
            expected.contains("meta.profile_version"),
            "and it names the field: {expected}"
        );
    }

    /// Quoting, because an unescaped title is a file that does not parse
    /// and an error message the author cannot act on.
    #[test]
    fn awkward_field_text_still_produces_a_loadable_profile() {
        let mut f = form();
        f.title = "The \"Quoted\" Game \\ Deluxe".to_string();
        f.source = "C:\\docs\\map.txt\tline 3".to_string();
        let draft = Draft::from_form(&f);
        let outcome = match draft.check() {
            Check::Accepted { warnings } => {
                assert!(warnings.is_empty());
                loader::load_str(draft.text()).unwrap()
            }
            Check::Refused { diagnostic } => panic!("escaping is wrong: {diagnostic}"),
        };
        assert_eq!(outcome.profile.meta.title, "The \"Quoted\" Game \\ Deluxe");
        assert_eq!(outcome.profile.meta.sources[0], "C:\\docs\\map.txt\tline 3");
    }

    /// The console goes out in its wire spelling. `"Nes"` would be
    /// refused, and the enum's `Debug` is exactly that.
    #[test]
    fn console_is_written_lowercase() {
        let mut f = form();
        f.console = Console::Snes;
        let draft = Draft::from_form(&f);
        assert!(draft.text().contains("console = \"snes\""));
        assert_eq!(
            loader::load_str(draft.text()).unwrap().profile.meta.console,
            Console::Snes
        );
    }

    /// An unsaved draft has nowhere to save to, and says so rather than
    /// writing somewhere surprising.
    #[test]
    fn saving_a_draft_with_no_path_is_refused() {
        let mut draft = Draft::from_form(&form());
        let e = draft.save().expect_err("no path, no save");
        assert!(e.diagnostic.contains("never been saved"));
    }

    /// Opening preserves comments — this project's own profiles carry
    /// their provenance in them, and a load/emit round trip would eat it.
    #[test]
    fn opening_and_saving_preserves_comments() {
        let dir = scratch("comments");
        let path = dir.join("profile.toml");
        let mut draft = Draft::from_form(&form());
        draft.save_to(&path).unwrap();

        let mut reopened = Draft::open(&path).expect("opens");
        assert!(
            reopened
                .text()
                .contains("# Written by the RetroForge profile editor"),
            "the header comment did not survive the open"
        );
        assert!(!reopened.is_dirty(), "a freshly opened draft is not dirty");
        reopened.save().expect("re-saves");
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("FR-PROF-003"));
    }
}
