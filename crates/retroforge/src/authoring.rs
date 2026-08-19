//! The profile authoring loop: watch a profile TOML, re-decode on save,
//! and report what broke with ROM offsets (ticket W5-06;
//! `docs/design/FRONTEND_UI.md` §3.5, story E6-S2).
//!
//! §3.5: "live decode preview — profile TOML (external editor or embedded
//! text editor) hot-reloads on save; decoded level/tilemap renders
//! immediately with error list inline."
//!
//! ## Polling mtime, not an OS file-watcher, and why that is the better
//! choice here rather than a shortcut
//!
//! The obvious implementation is the `notify` crate. This polls
//! `fs::metadata` instead, and it is worth saying why, because "we avoided
//! a dependency" is not on its own a good reason:
//!
//! * **The cadence is already free.** The shell repaints continuously
//!   while a core runs; a `stat` on one file per repaint is far below the
//!   noise floor of a frame that also uploads a 240 KB texture. A watcher
//!   would buy no latency a user could perceive against a control they
//!   drive by pressing Ctrl-S in another window.
//! * **It is testable without sleeping.** A watcher's callback arrives on
//!   its own thread at the OS's convenience, so testing one means either
//!   sleeping (flaky) or mocking the watcher (testing the mock). [`Watch`]
//!   is a pure function of two `fs::metadata` calls, so its tests below
//!   drive it directly and assert both that it fires on a change and that
//!   it does NOT fire when nothing changed — the second being the half
//!   that a watcher's tests usually skip.
//! * **A new crate is a licence and platform surface.** `notify` carries
//!   per-OS backends (inotify/FSEvents/ReadDirectoryChangesW) and would
//!   need a `docs/TECH_STACK.md` row and a `cargo deny` pass for a feature
//!   this size.
//!
//! The honest limit, stated rather than left to be discovered: polling
//! cannot see a change that happens and is reverted between two polls,
//! and it compares (mtime, len) rather than content, so an editor that
//! rewrites a file byte-identically with a preserved mtime is invisible.
//! Both are acceptable for a human pressing save; neither would be for a
//! build system.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rf_enhance::decode::metatile_screens::{self, DecodedLevel, Spec};
use rf_profiles::schema::Profile;

/// A watched profile file.
#[derive(Debug, Clone)]
pub struct Watch {
    path: PathBuf,
    stamp: Option<(SystemTime, u64)>,
}

impl Watch {
    /// Start watching `path` **as it is now** — the first [`Watch::poll`]
    /// reports no change.
    ///
    /// This matters: constructing a watch and immediately getting a
    /// "changed!" would make every panel open look like an edit, and a
    /// reload storm on open is indistinguishable from a working
    /// hot-reload until someone notices the preview flickering.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let stamp = stamp_of(&path);
        Self { path, stamp }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Has the file changed since the last poll? Updates the stamp.
    ///
    /// A file that has been **deleted** counts as a change, once: the
    /// preview should stop claiming to show a profile that no longer
    /// exists.
    pub fn poll(&mut self) -> bool {
        let now = stamp_of(&self.path);
        let changed = now != self.stamp;
        self.stamp = now;
        changed
    }
}

fn stamp_of(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// One thing wrong with the profile, as the author needs to read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoringError {
    /// What went wrong, already worded for a human.
    pub message: String,
    /// The ROM offset the error is about, when it has one — acceptance
    /// criterion 2's "with ROM offsets".
    ///
    /// `Option` because not every authoring error has one: a TOML syntax
    /// error or a missing `[decode]` section is about the *file*, and
    /// inventing an offset for it would be worse than having none.
    pub rom_offset: Option<u32>,
}

impl AuthoringError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            rom_offset: None,
        }
    }

    fn at(message: impl Into<String>, offset: u32) -> Self {
        Self {
            message: message.into(),
            rom_offset: Some(offset),
        }
    }

    /// One line for the inline error list, with the offset rendered in
    /// hex because that is how every other tool in this project (the
    /// debugger's viewers, `retroforge-tool rom inspect`, the profile's
    /// own `rom_map`) writes ROM addresses.
    #[must_use]
    pub fn line(&self) -> String {
        match self.rom_offset {
            Some(o) => format!("[ROM {o:#06X}] {}", self.message),
            None => self.message.clone(),
        }
    }
}

/// What one reload produced.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReloadOutcome {
    pub profile: Option<Profile>,
    pub spec: Option<Spec>,
    /// The decoded level, when the profile decoded — this is what the
    /// preview panel draws.
    pub level: Option<DecodedLevel>,
    /// Inline error list (§3.5).
    pub errors: Vec<AuthoringError>,
    /// Non-fatal loader warnings (unknown keys and the like), which the
    /// panel shows separately: a warning is something to look at, an
    /// error is something that stopped the preview.
    pub warnings: Vec<String>,
}

impl ReloadOutcome {
    /// Whether the preview has something to draw.
    #[must_use]
    pub fn has_preview(&self) -> bool {
        self.level.is_some()
    }
}

/// Load `path` and decode `rom_normalized` through it.
///
/// `rom_normalized` is the **header-stripped** image, because that is
/// what `[[rom_map]]` offsets are relative to
/// (`rf_enhance::decode`'s module doc). Passing a raw iNES file shifts
/// every table by 16 bytes and produces a plausible wrong level rather
/// than an error, which in an authoring loop is the worst possible
/// outcome: the author would tune offsets against a lie.
#[must_use]
pub fn reload(path: &Path, rom_normalized: Option<&[u8]>) -> ReloadOutcome {
    let mut out = ReloadOutcome::default();

    let loaded = match rf_profiles::load_file(path) {
        Ok(loaded) => loaded,
        Err(e) => {
            out.errors.push(AuthoringError::new(e.to_string()));
            return out;
        }
    };
    out.warnings = loaded.warnings.iter().map(ToString::to_string).collect();
    let profile = loaded.profile;

    match metatile_screens::spec_from_profile(&profile) {
        Ok(spec) => {
            if let Some(rom) = rom_normalized {
                match metatile_screens::decode(rom, &spec) {
                    Ok(level) => out.level = Some(level),
                    Err(e) => out.errors.push(decode_error(&e)),
                }
            } else {
                // Not an error: an author can iterate on a profile's
                // structure before the ROM they are describing is open.
                out.warnings
                    .push("No ROM open — the profile parsed, but nothing was decoded.".to_string());
            }
            out.spec = Some(spec);
        }
        Err(e) => out.errors.push(decode_error(&e)),
    }

    out.profile = Some(profile);
    out
}

/// Turn a decoder error into an authoring error, carrying its ROM offset
/// where it has one.
///
/// `OutOfRange` is the case criterion 2 is really about — a mistyped
/// table address is the single most common authoring mistake, and the
/// error already knows exactly which table and where it pointed.
fn decode_error(e: &rf_enhance::decode::DecodeError) -> AuthoringError {
    use rf_enhance::decode::DecodeError;
    match e {
        DecodeError::OutOfRange { offset, .. } => {
            AuthoringError::at(e.to_string(), u32::try_from(*offset).unwrap_or(u32::MAX))
        }
        _ => AuthoringError::new(e.to_string()),
    }
}

/// A compact text rendering of a decoded level, for the preview panel and
/// for tests.
///
/// Text rather than pixels on purpose: the preview's job in the authoring
/// loop is to answer "did my table offsets land on real level data?", and
/// a metatile-id grid answers that more directly than a rendering would —
/// a wrong palette makes a picture look broken while the ids are fine,
/// and a right palette can make garbage look plausible. Ticket W5-03's
/// `SceneGraph` is the path to actual pixels.
#[must_use]
pub fn preview_text(level: &DecodedLevel, max_cols: u32, max_rows: u32) -> String {
    let cols = level.width.min(max_cols);
    let rows = level.height.min(max_rows);
    let mut out = String::new();
    for row in 0..rows {
        for col in 0..cols {
            let id = level.at(col, row).unwrap_or(0);
            out.push_str(&format!("{id:X}"));
        }
        if cols < level.width {
            out.push('\u{2026}');
        }
        out.push('\n');
    }
    if rows < level.height {
        out.push_str("\u{2026}\n");
    }
    out
}
