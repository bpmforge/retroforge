//! The ROM library: scan configured folders, identify by normalized hash
//! (ticket W2-07; FR-FE-001, NFR-005, NFR-010).
//!
//! ## Identity is the normalized hash, never the filename
//!
//! `rf_cart::Cartridge::load` sniffs the console from content (never from
//! the extension — its own doc says so) and hashes the header-stripped
//! image. That normalized hash is what per-game settings key on
//! (`crate::game_settings`), what save states verify against
//! (`crate::save_state`), and what a profile matches — so the same cartridge
//! dumped twice, or renamed, or with a different header, is one game
//! everywhere in the app.
//!
//! **No network fallback, ever** (NFR-005, NON_GOALS #5): a ROM this build
//! cannot parse is still listed and still playable, shown as unrecognized.
//! Nothing here reaches for a database.
//!
//! ## Path containment (NFR-010 / D-006), and what "confined" means
//!
//! Every discovered path is `canonicalize`d and checked to still live under
//! the canonicalized root. A symlink pointing outside the root is refused
//! with a diagnostic naming it (`FAILURE_MODES.md` FM-15) rather than
//! silently followed or silently skipped — a scan that quietly ignores half
//! a library is indistinguishable from a scan that found nothing.
//!
//! Symlink loops are handled by remembering canonical directories already
//! visited: a loop resolves to a directory the walk has seen, so it stops
//! rather than recursing forever. That is why the visited set holds
//! canonical paths and not the paths as written.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Largest file the scanner will read. A ROM larger than this is not one
/// this app can run, and reading it would stall a scan on some unrelated
/// file that happens to live in a ROM folder.
const MAX_ROM_BYTES: u64 = 64 * 1024 * 1024;

/// How deep the walk goes. Deliberately finite: containment already stops
/// escapes and the visited set already stops loops, but a pathological tree
/// (a million nested empty directories) should still terminate promptly.
const MAX_DEPTH: usize = 16;

/// One entry in the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryEntry {
    /// Path as discovered (not canonicalized — this is what the user sees
    /// and what "open file location" should reveal).
    pub path: PathBuf,
    /// Display title: the file stem. No scraping, by design.
    pub title: String,
    /// What the scanner made of it.
    pub identity: EntryIdentity,
}

/// Whether a file could be identified, and as what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryIdentity {
    /// Parsed and hashed.
    Recognized {
        console: Console,
        /// Normalized (header-stripped) SHA-256, lowercase hex — the key
        /// everything else in the app uses.
        normalized_sha256: String,
    },
    /// A file that looks like a ROM by extension but this build could not
    /// parse. Still listed and still playable (FRONTEND_UI §3.1: "generic
    /// card"); the reason is kept so the UI can explain it.
    Unrecognized { reason: String },
}

/// The library screen's Grid/List toggle (ticket W15-05, `UX_WAVE_15.md`
/// §3), persisted in `Settings` (`crate::settings::LibrarySettings`).
///
/// `Grid` is the default since W21-05 (the design review's library
/// mockup). `List` stays the accessible fallback (§10) and a saved
/// choice of it is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum LibraryView {
    List,
    /// Ticket W21-05: the default since the design review's library
    /// mockup ("Home · grid view"); a saved `list` choice is kept.
    #[default]
    Grid,
}

/// Which console an entry is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Console {
    Nes,
    Snes,
}

impl Console {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Console::Nes => "NES",
            Console::Snes => "SNES",
        }
    }
}

/// Something the scan refused or could not do, kept so the UI can say what
/// happened instead of showing a shorter list than the user expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanIssue {
    /// A configured root does not exist or cannot be canonicalized.
    UnusableRoot { root: PathBuf, reason: String },
    /// NFR-010: a path resolved outside its root and was refused.
    EscapedRoot { path: PathBuf, resolved: PathBuf },
    /// A directory could not be read.
    UnreadableDir { path: PathBuf, reason: String },
    /// A file could not be read.
    UnreadableFile { path: PathBuf, reason: String },
    /// The file is larger than [`MAX_ROM_BYTES`].
    TooLarge { path: PathBuf, bytes: u64 },
}

/// One configured library folder, and optionally which console it holds.
///
/// ## Why the hint exists (ticket W14-01)
///
/// A real ROM collection is usually one folder per console, and the folder
/// above them holds every console the owner has. Without a hint, adding
/// that parent means the scan reads thousands of Game Boy and GBA
/// archives, finds no NES or SNES cartridge in any of them, and says
/// nothing about them — correct, but it read them all to find out. A root
/// that declares its console lets an entry that sniffs as the *other*
/// console be dropped without being listed.
///
/// **What it does NOT do, stated so nobody assumes otherwise:** it does
/// not avoid the read. A zip's console is only knowable by decompressing
/// and sniffing it, so the hint filters results, it does not save work.
/// Making a second scan cheap is `W14-02`'s cache, not this.
///
/// `None` means "whatever is in there", which is what every root
/// configured before this ticket deserializes to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum LibraryRoot {
    /// A bare path, the pre-W14-01 form. Kept as a variant rather than
    /// migrated so an existing `settings.toml` keeps working untouched:
    /// serde reads `library_folders = ["/roms"]` straight into this.
    Bare(PathBuf),
    /// A path that declares its console.
    Hinted {
        path: PathBuf,
        console: Option<Console>,
    },
}

impl LibraryRoot {
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            LibraryRoot::Bare(p) | LibraryRoot::Hinted { path: p, .. } => p,
        }
    }

    #[must_use]
    pub const fn console(&self) -> Option<Console> {
        match self {
            LibraryRoot::Bare(_) => None,
            LibraryRoot::Hinted { console, .. } => *console,
        }
    }
}

impl From<PathBuf> for LibraryRoot {
    fn from(path: PathBuf) -> Self {
        LibraryRoot::Bare(path)
    }
}

/// The result of one scan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Library {
    pub entries: Vec<LibraryEntry>,
    pub issues: Vec<ScanIssue>,
    /// Roots that were scanned successfully, canonicalized.
    pub scanned_roots: Vec<PathBuf>,
}

impl Library {
    /// Entries for a console.
    #[must_use]
    pub fn by_console(&self, console: Console) -> Vec<&LibraryEntry> {
        self.entries
            .iter()
            .filter(|e| matches!(&e.identity, EntryIdentity::Recognized { console: c, .. } if *c == console))
            .collect()
    }

    /// Look an entry up by normalized hash — the identity everything else
    /// in the app keys on.
    #[must_use]
    pub fn by_hash(&self, normalized_sha256: &str) -> Option<&LibraryEntry> {
        self.entries.iter().find(|e| {
            matches!(&e.identity, EntryIdentity::Recognized { normalized_sha256: h, .. }
                if h == normalized_sha256)
        })
    }
}

/// Cached facts about a game's play history (ticket W15-02), read from
/// `game_settings` once per scan/launch rather than once per row per
/// frame — UX_WAVE_15 §11 acceptance 1 requires this to be cheap enough
/// to do for every row, and re-reading a per-game file every frame for
/// every row would not be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecencyMeta {
    pub last_played_epoch_secs: Option<u64>,
    pub play_count: u32,
    pub favourite: bool,
}

/// The toolbar's Recently played / Favourites chips (§3): radio-like with
/// each other (selecting one clears the other) and combined with the
/// console filter by the caller — `Library::filter_and_sort` ANDs this in
/// alongside `console_filter`, never on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecencyFilter {
    #[default]
    All,
    RecentlyPlayed,
    Favourites,
}

/// The sort control's three explicit choices (§3, §11). There is
/// deliberately no `Default` variant here — "no sort chosen explicitly"
/// is `None` at the call site in [`filter_and_sort`], not a fourth
/// variant, so the "empty search defaults to recent-first" rule lives in
/// one place instead of being smeared across an enum default and a
/// separate check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    Title,
    LastPlayed,
    Console,
}

/// This entry's recency metadata, or the all-zero default for an entry
/// this build could not identify (an unrecognized ROM has no hash to key
/// play history on, so it is never "recently played" or a "favourite").
fn recency_of(entry: &LibraryEntry, meta: &BTreeMap<String, RecencyMeta>) -> RecencyMeta {
    match &entry.identity {
        EntryIdentity::Recognized {
            normalized_sha256, ..
        } => meta.get(normalized_sha256).copied().unwrap_or_default(),
        EntryIdentity::Unrecognized { .. } => RecencyMeta::default(),
    }
}

/// Sort key for [`SortMode::Console`]: NES before SNES before an entry
/// this build could not identify, so "no console" sorts last rather than
/// wherever an arbitrary string comparison happens to put it.
fn console_sort_key(entry: &LibraryEntry) -> u8 {
    match &entry.identity {
        EntryIdentity::Recognized {
            console: Console::Nes,
            ..
        } => 0,
        EntryIdentity::Recognized {
            console: Console::Snes,
            ..
        } => 1,
        EntryIdentity::Unrecognized { .. } => 2,
    }
}

/// The toolbar's whole filter+sort pipeline (UX_WAVE_15 §3, §11), factored
/// out as a pure function of its inputs so it is unit-testable over a
/// synthetic entry list with no `egui` harness — the app's `library_grid`
/// calls this with live state; the tests below call it with a hand-built
/// `Library` and `meta` map.
///
/// `sort` is `None` for "the user has not picked an explicit sort yet":
/// per §11's acceptance, that means recent-first (never-played entries
/// after, alphabetically) while the search box is empty, and Title order
/// — the order `Library::scan` already produces — once it is not, since a
/// search result reads better alphabetical than reshuffled by a history
/// the query has nothing to do with.
#[must_use]
pub fn filter_and_sort<'a>(
    entries: &'a [LibraryEntry],
    meta: &BTreeMap<String, RecencyMeta>,
    console_filter: Option<Console>,
    recency_filter: RecencyFilter,
    search: &str,
    sort: Option<SortMode>,
) -> Vec<&'a LibraryEntry> {
    let needle = search.trim().to_lowercase();
    let mut matches: Vec<&LibraryEntry> = entries
        .iter()
        .filter(|e| {
            let console_ok = match console_filter {
                None => true,
                Some(want) => matches!(
                    &e.identity,
                    EntryIdentity::Recognized { console, .. } if *console == want
                ),
            };
            let text_ok = needle.is_empty() || e.title.to_lowercase().contains(&needle);
            let recency_ok = match recency_filter {
                RecencyFilter::All => true,
                RecencyFilter::RecentlyPlayed => {
                    recency_of(e, meta).last_played_epoch_secs.is_some()
                }
                RecencyFilter::Favourites => recency_of(e, meta).favourite,
            };
            console_ok && text_ok && recency_ok
        })
        .collect();

    let effective_sort = sort.unwrap_or(if needle.is_empty() {
        SortMode::LastPlayed
    } else {
        SortMode::Title
    });

    match effective_sort {
        SortMode::Title => {
            matches.sort_by(|a, b| a.title.cmp(&b.title).then_with(|| a.path.cmp(&b.path)));
        }
        SortMode::Console => {
            matches.sort_by(|a, b| {
                console_sort_key(a)
                    .cmp(&console_sort_key(b))
                    .then_with(|| a.title.cmp(&b.title))
                    .then_with(|| a.path.cmp(&b.path))
            });
        }
        SortMode::LastPlayed => {
            matches.sort_by(|a, b| {
                let (ta, tb) = (
                    recency_of(a, meta).last_played_epoch_secs,
                    recency_of(b, meta).last_played_epoch_secs,
                );
                match (ta, tb) {
                    // Most recent first.
                    (Some(x), Some(y)) => y.cmp(&x),
                    // Never-played entries sort AFTER any played entry,
                    // per §11's "never-played titles after".
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
                .then_with(|| a.title.cmp(&b.title))
                .then_with(|| a.path.cmp(&b.path))
            });
        }
    }

    matches
}

/// What the library screen should show before anything else (FRONTEND_UI
/// §3.1's "Empty/first-run state", design review G-21).
///
/// A function of the scan rather than something the UI decides, so the rule
/// is testable without rendering: "no folders configured" and "folders
/// configured but nothing in them" are **different** states with different
/// messages, and conflating them is exactly the bug G-21 was raised about
/// (an empty grid that says nothing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirstRunState {
    /// No roots configured: show the "Add a ROM folder…" call to action.
    NoRootsConfigured,
    /// Roots configured, zero ROMs found: say so, naming the folders.
    NoRomsFound { roots: Vec<PathBuf> },
    /// Ordinary case.
    Populated { count: usize },
}

/// Classify a scan for the library screen.
#[must_use]
pub fn first_run_state(configured_roots: &[LibraryRoot], library: &Library) -> FirstRunState {
    if configured_roots.is_empty() {
        return FirstRunState::NoRootsConfigured;
    }
    if library.entries.is_empty() {
        return FirstRunState::NoRomsFound {
            // Paths, not roots: this is what the empty-library banner
            // shows the user, and a console hint is not part of that
            // sentence.
            roots: configured_roots
                .iter()
                .map(|r| r.path().to_path_buf())
                .collect(),
        };
    }
    FirstRunState::Populated {
        count: library.entries.len(),
    }
}

/// File extensions the scanner will open. Content decides what a file
/// actually is (`rf_cart::Cartridge::load` sniffs), but *opening* every
/// file in a folder that might hold thousands of unrelated ones is a waste;
/// the extension is a cheap pre-filter, never the identity.
/// `zip` is here because a real library is zipped: of 2546 NES and SNES
/// archives in the collection this ticket was measured against, every
/// single one is a `.zip`. The archive is opened through
/// [`crate::rom_open::resolve_rom_bytes`], the same untrusted-input path
/// the file-open dialog uses — capped on bytes actually READ rather than
/// on the size an archive declares, cumulative cap across entries, nested
/// archives refused, nothing written to disk.
const ROM_EXTENSIONS: [&str; 5] = ["nes", "sfc", "smc", "fig", "zip"];

/// Scan `roots`, returning everything found and everything refused.
///
/// Never fails as a whole: a bad root becomes a [`ScanIssue`] and the other
/// roots are still scanned, because one mistyped path should not cost the
/// user their whole library.
#[must_use]
pub fn scan(roots: &[LibraryRoot]) -> Library {
    scan_cached(roots, &mut crate::library_cache::LibraryCache::default()).0
}

/// Scan, reusing what `cache` already knows and recording what it learns
/// (ticket W14-02).
///
/// Returns the library and whether anything was learned, so a caller can
/// skip rewriting an unchanged cache file.
///
/// The cache is consulted per FILE, not per root, because that is the unit
/// whose cost it avoids: establishing identity means reading and hashing,
/// and since W14-01 decompressing too. Everything else the scan does —
/// walking, containment, symlink-loop detection — is cheap and still
/// happens every time, so a symlink that starts escaping its root is still
/// caught on the next scan rather than remembered as safe.
#[must_use]
pub fn scan_cached(
    roots: &[LibraryRoot],
    cache: &mut crate::library_cache::LibraryCache,
) -> (Library, bool) {
    let mut library = Library::default();
    let mut learned = false;
    let mut visited: HashSet<PathBuf> = HashSet::new();

    for root in roots {
        let canonical_root = match root.path().canonicalize() {
            Ok(path) => path,
            Err(e) => {
                library.issues.push(ScanIssue::UnusableRoot {
                    root: root.path().to_path_buf(),
                    reason: e.to_string(),
                });
                continue;
            }
        };
        library.scanned_roots.push(canonical_root.clone());
        scan_dir(
            &canonical_root,
            &canonical_root,
            0,
            root.console(),
            &mut visited,
            &mut library,
            cache,
            &mut learned,
        );
    }

    // Stable order so the grid does not reshuffle between scans; by title
    // then path, since two different dumps of one game share a title.
    library
        .entries
        .sort_by(|a, b| a.title.cmp(&b.title).then_with(|| a.path.cmp(&b.path)));

    // NOT DEDUPED BY HASH, deliberately (ticket W14-01). Two files holding
    // one cartridge — a bare `.sfc` beside its `.zip`, or one dump under
    // two folders — produce two entries that share a normalized hash. A
    // first draft folded them, and that was wrong twice over: it hides a
    // file the user actually has, and "keep the first" is a pick however
    // deterministically it is made, which is the thing `rom_open` already
    // refuses to do when an archive holds two ROMs. Whether the library
    // GRID should show one card per hash is a UI question with an owner;
    // the scan reports what is on disk.
    (library, learned)
}

#[allow(clippy::too_many_arguments)]
fn scan_dir(
    dir: &Path,
    root: &Path,
    depth: usize,
    hint: Option<Console>,
    visited: &mut HashSet<PathBuf>,
    library: &mut Library,
    cache: &mut crate::library_cache::LibraryCache,
    learned: &mut bool,
) {
    if depth > MAX_DEPTH {
        return;
    }
    // The loop guard: a symlink cycle resolves to a directory already
    // walked, so canonical identity is what must be remembered.
    let Ok(canonical_dir) = dir.canonicalize() else {
        return;
    };
    if !visited.insert(canonical_dir.clone()) {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            library.issues.push(ScanIssue::UnreadableDir {
                path: dir.to_path_buf(),
                reason: e.to_string(),
            });
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(resolved) = path.canonicalize() else {
            continue;
        };
        // NFR-010 / D-006: refuse anything that resolves outside the root,
        // naming it (FM-15) rather than skipping it silently.
        if !resolved.starts_with(root) {
            library.issues.push(ScanIssue::EscapedRoot {
                path: path.clone(),
                resolved,
            });
            continue;
        }
        if resolved.is_dir() {
            scan_dir(
                &path,
                root,
                depth + 1,
                hint,
                visited,
                library,
                cache,
                learned,
            );
        } else if is_rom_candidate(&path) {
            ingest_file(&path, hint, library, cache, learned);
        }
    }
}

/// A file's display title: its stem, which for a No-Intro set is the game
/// name and region. Never the identity — that is the normalized hash.
fn title_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("(unnamed)")
        .to_string()
}

fn is_rom_candidate(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|ext| ROM_EXTENSIONS.contains(&ext.as_str()))
}

/// Extensions that name a cartridge for a console this build does not
/// emulate.
///
/// ## Why a denylist and not the obvious allowlist (ticket W14-01)
///
/// `rom_open`'s module doc is explicit that an entry inside an archive is
/// chosen by CONTENT, never by its name — that is what lets an oddly named
/// entry work. This does not overturn that: an unknown extension is still
/// sniffed. It excludes only names that positively assert a console we do
/// not run.
///
/// **It exists because the sniff is not sound on foreign data.** Pointed
/// at a real Game Boy folder, `rf_cart::Cartridge::load` reported **130 of
/// 681** archives as cartridges: a SNES header is a checksum and a reset
/// vector at a fixed offset inside the image, with no leading magic, and
/// arbitrary ROM data hits that pattern often enough to matter. A library
/// root covering several consoles would otherwise fill with games that do
/// not exist. **The false-positive itself is `rf-cart`'s and is NOT fixed
/// here** — that crate is outside this ticket's write scope, and it has a
/// ticket of its own (W14-05).
///
/// **What this does NOT catch**, so nobody reads it as containment: it
/// keys on a name that DECLARES a console. A foreign ROM called `.bin`,
/// `.rom`, or nothing at all still reaches the sniffer and can still come
/// back a false cartridge, and every other caller of
/// `rf_cart::Cartridge::load` — the file-open dialog, `core_thread::spawn`,
/// profile matching — is unprotected by this entirely. That residual is
/// W14-05's, not this list's.
const FOREIGN_ROM_EXTENSIONS: [&str; 10] = [
    "gb", "gbc", "gba", "vb", "sms", "gg", "n64", "z64", "v64", "md",
];

/// Does this name assert a console this build does not emulate?
pub(crate) fn names_a_foreign_console(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|ext| FOREIGN_ROM_EXTENSIONS.contains(&ext.as_str()))
}

fn ingest_file(
    path: &Path,
    hint: Option<Console>,
    library: &mut Library,
    cache: &mut crate::library_cache::LibraryCache,
    learned: &mut bool,
) {
    // Ticket W14-02: if this exact file was read before, do not read it
    // again. The stamp is length + mtime; anything else is a miss.
    let stamp = crate::library_cache::stamp(path);
    if let Some((len, mtime)) = stamp {
        if let Some(outcome) = cache.get(path, len, mtime) {
            let identity = outcome.to_identity();
            push_entry(path, hint, identity, library);
            return;
        }
    }

    let bytes = match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_ROM_BYTES => {
            library.issues.push(ScanIssue::TooLarge {
                path: path.to_path_buf(),
                bytes: meta.len(),
            });
            return;
        }
        Ok(_) => match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                library.issues.push(ScanIssue::UnreadableFile {
                    path: path.to_path_buf(),
                    reason: e.to_string(),
                });
                return;
            }
        },
        Err(e) => {
            library.issues.push(ScanIssue::UnreadableFile {
                path: path.to_path_buf(),
                reason: e.to_string(),
            });
            return;
        }
    };

    // Ticket W14-01: a `.zip` becomes the cartridge inside it here, on the
    // SAME path the file-open dialog uses, so identity stays the
    // normalized hash of the ROM itself. A bare and a zipped copy of one
    // cartridge therefore collapse to ONE entry — which matters because
    // that hash is what per-game settings, save states and profile
    // matching all key on. A bare file is moved through untouched.
    let bytes = match crate::rom_open::resolve_rom_bytes(bytes) {
        Ok(rom) => rom,
        // An archive holding no cartridge at all is SKIPPED, not listed.
        // FRONTEND_UI 3.1's "unparseable is listed, not dropped" is about
        // a ROM this build cannot parse; a Game Boy archive is not that,
        // and the collection this was measured against holds 2507 of them
        // next to the NES and SNES folders. Listing them would bury the
        // library in entries for a console this emulator does not run.
        Err(crate::rom_open::RomOpenError::NoRomInArchive { .. }) => {
            // Remembered, not merely skipped (ticket W14-02): discovering
            // that an archive holds nothing costs the same decompression
            // as a success, and in a collection that also holds other
            // consoles' games this is the majority case.
            if let Some((len, mtime)) = stamp {
                cache.insert(
                    path.to_path_buf(),
                    len,
                    mtime,
                    crate::library_cache::CachedOutcome::NoCartridge,
                );
                *learned = true;
            }
            return;
        }
        // Everything else IS reported: a cartridge this build refused
        // (unsupported mapper or chip) is exactly the case W2-16 exists
        // for, and a corrupt archive is worth saying out loud.
        Err(e) => {
            let identity = EntryIdentity::Unrecognized {
                reason: e.to_string(),
            };
            if let Some((len, mtime)) = stamp {
                cache.insert(
                    path.to_path_buf(),
                    len,
                    mtime,
                    crate::library_cache::CachedOutcome::from_identity(Some(&identity)),
                );
                *learned = true;
            }
            push_entry(path, hint, Some(identity), library);
            return;
        }
    };

    let title = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("(unnamed)")
        .to_string();

    // An unparseable file is listed, not dropped: FRONTEND_UI §3.1 says
    // "unidentified ROMs still playable (shown with generic card)".
    let identity = match rf_cart::Cartridge::load(&bytes) {
        Ok(rf_cart::Cartridge::Nes { identity, .. }) => EntryIdentity::Recognized {
            console: Console::Nes,
            normalized_sha256: identity.normalized.sha256,
        },
        Ok(rf_cart::Cartridge::Snes { identity, .. }) => EntryIdentity::Recognized {
            console: Console::Snes,
            normalized_sha256: identity.normalized.sha256,
        },
        Err(e) => EntryIdentity::Unrecognized {
            reason: e.to_string(),
        },
    };

    if let Some((len, mtime)) = stamp {
        cache.insert(
            path.to_path_buf(),
            len,
            mtime,
            crate::library_cache::CachedOutcome::from_identity(Some(&identity)),
        );
        *learned = true;
    }

    push_entry(path, hint, Some(identity), library);
    let _ = title;
}

/// Add an entry for a file whose identity is already known, applying the
/// root's console hint.
///
/// `None` means the file holds no cartridge: it is remembered as such by
/// the cache but never listed (ticket W14-01's skip rule).
fn push_entry(
    path: &Path,
    hint: Option<Console>,
    identity: Option<EntryIdentity>,
    library: &mut Library,
) {
    let Some(identity) = identity else {
        return;
    };
    // Ticket W14-01: a root that declares its console drops the other
    // one. Applied HERE, after the sniff, because a zip's console is not
    // knowable before it is opened — the hint filters the result, it does
    // not save the read (see `LibraryRoot`).
    if let (Some(want), EntryIdentity::Recognized { console, .. }) = (hint, &identity) {
        if *console != want {
            return;
        }
    }

    library.entries.push(LibraryEntry {
        path: path.to_path_buf(),
        title: title_of(path),
        identity,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("rf-w2-07-{label}-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A minimal valid NROM image, so the scanner has something real to
    /// identify without needing a fetched ROM (NFR-006: this suite runs in
    /// CI, where `roms/` does not exist).
    fn nes_rom(seed: u8) -> Vec<u8> {
        let mut rom = vec![0u8; 16 + 0x4000 + 0x2000];
        rom[0..4].copy_from_slice(b"NES\x1a");
        rom[4] = 1;
        rom[5] = 1;
        rom[16] = seed;
        rom
    }

    /// A minimal LoROM image, same shape `rf-snes`'s own tests use.
    fn snes_rom(seed: u8) -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x0000] = 0x80;
        rom[0x0001] = 0xFE;
        for (i, b) in b"RF LIBRARY TEST      ".iter().enumerate() {
            rom[0x7FC0 + i] = *b;
        }
        rom[0x7FD5] = 0x20;
        rom[0x7FD6] = 0x00;
        rom[0x7FD7] = 0x08;
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        rom[0x0100] = seed;
        rom
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).expect("create archive");
        let mut w = zip::ZipWriter::new(file);
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, contents) in entries {
            w.start_file(*name, opts).expect("start entry");
            std::io::Write::write_all(&mut w, contents).expect("write entry");
        }
        w.finish().expect("finish archive");
    }

    /// Ticket W14-01 criterion 1, and the thing this change could silently
    /// break: identity must stay the hash of the CARTRIDGE, never of the
    /// archive holding it. Per-game settings, save states and profile
    /// matching all key on that hash, so a zipped copy that hashed
    /// differently would quietly become a different game.
    #[test]
    fn a_bare_and_a_zipped_copy_of_one_cartridge_are_one_entry() {
        let dir = temp_dir("bare-vs-zipped");
        std::fs::create_dir_all(&dir).unwrap();
        let rom = nes_rom(0x42);
        std::fs::write(dir.join("Game (USA).nes"), &rom).unwrap();
        write_zip(
            &dir.join("Game (USA).zip"),
            &[("Game (USA).nes", rom.as_slice())],
        );

        let library = scan(&[LibraryRoot::Bare(dir.clone())]);
        assert_eq!(
            library.entries.len(),
            2,
            "both files are on disk and both are reported, got {:?}",
            library.entries
        );

        let hashes: Vec<&str> = library
            .entries
            .iter()
            .map(|e| match &e.identity {
                EntryIdentity::Recognized {
                    console,
                    normalized_sha256,
                } => {
                    assert_eq!(*console, Console::Nes);
                    normalized_sha256.as_str()
                }
                EntryIdentity::Unrecognized { reason } => {
                    panic!("both copies must be recognized, got {reason}")
                }
            })
            .collect();
        assert_eq!(
            hashes[0], hashes[1],
            "the zipped copy must hash the CARTRIDGE, not the archive — that hash \
             is what per-game settings, save states and profile matching key on"
        );
        assert_eq!(hashes[0].len(), 64);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Criterion 3: an archive for a console this build does not run holds
    /// no NES or SNES cartridge, so it is skipped ENTIRELY — not listed as
    /// unrecognized. The collection this ticket was measured against keeps
    /// 2507 Game Boy, GBA, GBC and Virtual Boy archives beside the NES and
    /// SNES folders; listing them would bury the library.
    #[test]
    fn an_archive_holding_no_cartridge_is_skipped_not_listed() {
        let dir = temp_dir("foreign-console");
        std::fs::create_dir_all(&dir).unwrap();
        write_zip(
            &dir.join("Some Handheld Game (USA).zip"),
            &[(
                "Some Handheld Game (USA).gb",
                b"not a cartridge we run".as_slice(),
            )],
        );
        std::fs::write(dir.join("Real (USA).nes"), nes_rom(7)).unwrap();

        // ...while a BROKEN cartridge of a console this build does run is
        // still listed, because "we could not parse this ROM" and "this is
        // not our ROM" are different answers. A bare corrupt file was
        // always listed; before this the same file zipped vanished.
        write_zip(
            &dir.join("Broken Dump (USA).zip"),
            &[("Broken Dump (USA).sfc", b"junk header".as_slice())],
        );

        let library = scan(&[LibraryRoot::Bare(dir.clone())]);
        let titles: Vec<&str> = library.entries.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Broken Dump (USA)", "Real (USA)"],
            "the handheld archive is skipped; the broken cartridge is listed"
        );
        assert!(matches!(
            library.entries[0].identity,
            EntryIdentity::Unrecognized { .. }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Criterion 3, second half: a root that declares its console drops
    /// the other one.
    #[test]
    fn a_console_hint_drops_the_other_console() {
        let dir = temp_dir("console-hint");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("A Nes Game (USA).nes"), nes_rom(1)).unwrap();
        write_zip(
            &dir.join("A Snes Game (USA).zip"),
            &[("A Snes Game (USA).sfc", snes_rom(1).as_slice())],
        );

        let both = scan(&[LibraryRoot::Bare(dir.clone())]);
        assert_eq!(both.entries.len(), 2, "no hint means both, got {both:?}");

        let nes_only = scan(&[LibraryRoot::Hinted {
            path: dir.clone(),
            console: Some(Console::Nes),
        }]);
        assert_eq!(nes_only.entries.len(), 1);
        assert_eq!(nes_only.entries[0].title, "A Nes Game (USA)");

        let snes_only = scan(&[LibraryRoot::Hinted {
            path: dir.clone(),
            console: Some(Console::Snes),
        }]);
        assert_eq!(snes_only.entries.len(), 1);
        assert_eq!(snes_only.entries[0].title, "A Snes Game (USA)");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A root configured before W14-01 is a bare path string in
    /// `settings.toml`, and must keep working untouched.
    #[test]
    fn a_pre_w14_root_deserializes_as_an_unhinted_root() {
        let roots: Vec<LibraryRoot> = toml::from_str::<
            std::collections::BTreeMap<String, Vec<LibraryRoot>>,
        >("library_folders = [\"/roms/nes\", \"/mnt/nas\"]\n")
        .expect("the pre-ticket form must still parse")
        .remove("library_folders")
        .expect("key");
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].path(), Path::new("/roms/nes"));
        assert!(roots.iter().all(|r| r.console().is_none()));
    }

    /// Ticket W14-02: the second scan of an unchanged library reads no
    /// file at all.
    ///
    /// `learned` is the proof and it is exact, not a proxy: it is set only
    /// where a file is actually opened and identified, so `learned ==
    /// false` means every entry came from the cache. Timing would be the
    /// obvious thing to assert and would be the wrong thing — a fast
    /// machine can hide a re-read, and a loaded one can make a cached scan
    /// look slow.
    #[test]
    fn a_second_scan_of_an_unchanged_library_reads_nothing() {
        let dir = temp_dir("cache-hit");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("One (USA).nes"), nes_rom(1)).unwrap();
        write_zip(
            &dir.join("Two (USA).zip"),
            &[("Two (USA).sfc", snes_rom(2).as_slice())],
        );
        // An archive holding nothing this build runs: remembered too, or a
        // mixed collection pays to rediscover it on every launch.
        write_zip(
            &dir.join("Elsewhere (USA).zip"),
            &[("Elsewhere (USA).gb", b"not ours".as_slice())],
        );
        let roots = [LibraryRoot::Bare(dir.clone())];

        let mut cache = crate::library_cache::LibraryCache::default();
        let (first, learned) = scan_cached(&roots, &mut cache);
        assert!(learned, "the first scan must read the files");
        assert_eq!(first.entries.len(), 2);
        assert_eq!(
            cache.len(),
            3,
            "all three files are remembered, including the one that holds no cartridge"
        );

        let (second, learned_again) = scan_cached(&roots, &mut cache);
        assert!(
            !learned_again,
            "nothing changed on disk, so nothing should have been opened"
        );
        assert_eq!(
            second.entries, first.entries,
            "a cached scan must produce exactly the library a fresh one does"
        );

        // A new file is still picked up: the cache is per-file, so it
        // cannot mask a change.
        std::fs::write(dir.join("Three (USA).nes"), nes_rom(3)).unwrap();
        let (third, learned_third) = scan_cached(&roots, &mut cache);
        assert!(learned_third, "a new file must be read");
        assert_eq!(third.entries.len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Criterion 4: the only test that reads a REAL library takes its
    /// directory from the environment and skips cleanly when it is not
    /// set, exactly as the 65816 vector suite does. A hardcoded path under
    /// somebody's home directory is a test that passes on one machine.
    #[test]
    fn the_real_library_scans_when_one_is_configured() {
        let Ok(dir) = std::env::var("RF_ROM_LIBRARY") else {
            eprintln!("SKIP: set RF_ROM_LIBRARY to a ROM folder to run this");
            return;
        };
        let library = scan(&[LibraryRoot::Bare(PathBuf::from(dir))]);
        let recognized = library
            .entries
            .iter()
            .filter(|e| matches!(e.identity, EntryIdentity::Recognized { .. }))
            .count();
        let nes = library.by_console(Console::Nes).len();
        let snes = library.by_console(Console::Snes).len();
        eprintln!(
            "real library: {} entries, {recognized} recognized ({nes} NES, {snes} SNES), {} issues",
            library.entries.len(),
            library.issues.len()
        );
        assert!(
            recognized > 0,
            "a configured ROM library that yields no recognized cartridge is a \
             failure, not an empty folder — that is exactly the state this \
             ticket was filed to end"
        );
    }

    #[test]
    fn a_scan_identifies_roms_by_normalized_hash_and_console() {
        let root = temp_dir("identify");
        std::fs::write(root.join("Game One.nes"), nes_rom(1)).unwrap();
        std::fs::write(root.join("Game Two.nes"), nes_rom(2)).unwrap();

        let library = scan(&[LibraryRoot::Bare(root.clone())]);
        assert_eq!(library.entries.len(), 2);
        assert!(library.issues.is_empty(), "{:?}", library.issues);

        let titles: Vec<&str> = library.entries.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["Game One", "Game Two"], "sorted by title");

        let EntryIdentity::Recognized {
            console,
            normalized_sha256,
        } = &library.entries[0].identity
        else {
            panic!("expected a recognized NES rom");
        };
        assert_eq!(*console, Console::Nes);
        assert_eq!(normalized_sha256.len(), 64);
        assert_eq!(
            library.by_hash(normalized_sha256).map(|e| e.title.as_str()),
            Some("Game One"),
            "hash lookup is how the rest of the app finds a game"
        );

        // Two different dumps must not collide.
        let hashes: HashSet<String> = library
            .entries
            .iter()
            .filter_map(|e| match &e.identity {
                EntryIdentity::Recognized {
                    normalized_sha256, ..
                } => Some(normalized_sha256.clone()),
                EntryIdentity::Unrecognized { .. } => None,
            })
            .collect();
        assert_eq!(hashes.len(), 2);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// FRONTEND_UI §3.1: "unidentified ROMs still playable (shown with
    /// generic card)" — so an unparseable file must be LISTED, not dropped.
    #[test]
    fn an_unparseable_rom_is_listed_as_unrecognized_rather_than_dropped() {
        let root = temp_dir("unrecognized");
        std::fs::write(root.join("Broken.nes"), b"not a rom at all").unwrap();

        let library = scan(&[LibraryRoot::Bare(root.clone())]);
        assert_eq!(library.entries.len(), 1, "it must still appear");
        match &library.entries[0].identity {
            EntryIdentity::Unrecognized { reason } => {
                assert!(
                    !reason.is_empty(),
                    "the reason must be explainable to a user"
                );
            }
            other => panic!("expected Unrecognized, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_scan_recurses_into_subfolders_and_ignores_non_rom_files() {
        let root = temp_dir("recurse");
        std::fs::create_dir_all(root.join("nes/usa")).unwrap();
        std::fs::write(root.join("nes/usa/Deep.nes"), nes_rom(3)).unwrap();
        std::fs::write(root.join("readme.txt"), b"hello").unwrap();
        std::fs::write(root.join("cover.png"), b"\x89PNG").unwrap();

        let library = scan(&[LibraryRoot::Bare(root.clone())]);
        assert_eq!(library.entries.len(), 1);
        assert_eq!(library.entries[0].title, "Deep");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// NFR-010's symlink-loop safety. Without the canonical visited set
    /// this recurses until the stack dies — the test is here because "it
    /// terminates" is not something reading the code proves.
    #[test]
    #[cfg(unix)]
    fn a_symlink_loop_terminates_instead_of_recursing_forever() {
        let root = temp_dir("loop");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/Looped.nes"), nes_rom(4)).unwrap();
        // sub/back -> the root itself: walking it revisits the root.
        std::os::unix::fs::symlink(&root, root.join("sub/back")).unwrap();

        let library = scan(&[LibraryRoot::Bare(root.clone())]);
        assert_eq!(
            library.entries.len(),
            1,
            "the loop must not produce duplicates: {:?}",
            library.entries
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// NFR-010 / D-006 containment, and FM-15's "refused with a diagnostic
    /// naming the offending path": a symlink out of the root is neither
    /// followed nor silently skipped.
    #[test]
    #[cfg(unix)]
    fn a_symlink_escaping_the_root_is_refused_with_a_diagnostic() {
        let root = temp_dir("contain-root");
        let outside = temp_dir("contain-outside");
        std::fs::write(outside.join("Secret.nes"), nes_rom(5)).unwrap();
        std::fs::write(root.join("Inside.nes"), nes_rom(6)).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();

        let library = scan(&[LibraryRoot::Bare(root.clone())]);

        assert_eq!(library.entries.len(), 1, "only the contained ROM is listed");
        assert_eq!(library.entries[0].title, "Inside");
        let escapes: Vec<&ScanIssue> = library
            .issues
            .iter()
            .filter(|i| matches!(i, ScanIssue::EscapedRoot { .. }))
            .collect();
        assert_eq!(
            escapes.len(),
            1,
            "the escape must be reported: {:?}",
            library.issues
        );
        match escapes[0] {
            ScanIssue::EscapedRoot { path, resolved } => {
                assert!(path.ends_with("escape"), "the diagnostic names the symlink");
                assert!(resolved.starts_with(outside.canonicalize().unwrap()));
            }
            other => panic!("unexpected issue {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// A file symlinked out of the root is refused for the same reason a
    /// directory is — otherwise containment would only cover half the tree.
    #[test]
    #[cfg(unix)]
    fn a_symlinked_file_pointing_outside_the_root_is_refused_too() {
        let root = temp_dir("contain-file-root");
        let outside = temp_dir("contain-file-outside");
        let target = outside.join("Elsewhere.nes");
        std::fs::write(&target, nes_rom(7)).unwrap();
        std::os::unix::fs::symlink(&target, root.join("Link.nes")).unwrap();

        let library = scan(&[LibraryRoot::Bare(root.clone())]);
        assert!(library.entries.is_empty(), "{:?}", library.entries);
        assert!(library
            .issues
            .iter()
            .any(|i| matches!(i, ScanIssue::EscapedRoot { .. })));

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// One bad root must not cost the user the rest of the library.
    #[test]
    fn a_missing_root_is_reported_and_the_others_still_scan() {
        let good = temp_dir("multi-good");
        std::fs::write(good.join("Fine.nes"), nes_rom(8)).unwrap();
        let missing = good.join("does-not-exist");

        let library = scan(&[
            LibraryRoot::Bare(missing.clone()),
            LibraryRoot::Bare(good.clone()),
        ]);
        assert_eq!(library.entries.len(), 1);
        assert!(matches!(
            library.issues.as_slice(),
            [ScanIssue::UnusableRoot { root, .. }] if root == &missing
        ));
        let _ = std::fs::remove_dir_all(&good);
    }

    #[test]
    fn an_oversized_file_is_reported_rather_than_read() {
        let root = temp_dir("toobig");
        let path = root.join("Huge.nes");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_ROM_BYTES + 1).unwrap();
        drop(file);

        let library = scan(&[LibraryRoot::Bare(root.clone())]);
        assert!(library.entries.is_empty());
        assert!(matches!(
            library.issues.as_slice(),
            [ScanIssue::TooLarge { .. }]
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// G-21's whole point: "no folders configured" and "folders with
    /// nothing in them" are different states with different messages.
    #[test]
    fn the_first_run_states_are_distinguished_rather_than_both_being_an_empty_grid() {
        let empty = Library::default();
        assert_eq!(
            first_run_state(&[], &empty),
            FirstRunState::NoRootsConfigured
        );

        let root = PathBuf::from("/roms");
        assert_eq!(
            first_run_state(&[LibraryRoot::Bare(root.clone())], &empty),
            FirstRunState::NoRomsFound {
                roots: vec![root.clone()]
            },
            "a configured-but-empty folder must say so, naming it"
        );

        let mut populated = Library::default();
        populated.entries.push(LibraryEntry {
            path: PathBuf::from("/roms/a.nes"),
            title: "a".to_string(),
            identity: EntryIdentity::Unrecognized {
                reason: "test".to_string(),
            },
        });
        assert_eq!(
            first_run_state(&[LibraryRoot::Bare(root.clone())], &populated),
            FirstRunState::Populated { count: 1 }
        );
    }

    /// NFR-005: nothing in a scan may reach for the network. Enforced by
    /// construction (this module has no HTTP client and `rf-cart` has no
    /// network dependency), and stated here so the rule is visible next to
    /// the code it governs rather than only in the SRS.
    #[test]
    fn scanning_needs_no_network_by_construction() {
        let root = temp_dir("offline");
        std::fs::write(root.join("Offline.nes"), nes_rom(9)).unwrap();
        let library = scan(&[LibraryRoot::Bare(root.clone())]);
        assert_eq!(library.entries.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// Ticket W15-02, acceptance criterion 4: each filter chip's resulting
/// set and each sort order, as pure-function unit tests over a synthetic
/// entry list — no scan, no harness.
#[cfg(test)]
mod filter_and_sort_tests {
    use super::*;

    fn entry(title: &str, console: Console, hash: &str) -> LibraryEntry {
        LibraryEntry {
            path: PathBuf::from(format!("/roms/{title}.rom")),
            title: title.to_string(),
            identity: EntryIdentity::Recognized {
                console,
                normalized_sha256: hash.to_string(),
            },
        }
    }

    fn unrecognized(title: &str) -> LibraryEntry {
        LibraryEntry {
            path: PathBuf::from(format!("/roms/{title}.rom")),
            title: title.to_string(),
            identity: EntryIdentity::Unrecognized {
                reason: "test".to_string(),
            },
        }
    }

    fn titles<'a>(entries: &[&'a LibraryEntry]) -> Vec<&'a str> {
        entries.iter().map(|e| e.title.as_str()).collect()
    }

    /// A four-game synthetic library exercising every axis this pipeline
    /// has to combine: two consoles, one unrecognized entry, one
    /// favourite, two with play history at different times, one never
    /// played.
    fn sample() -> (Vec<LibraryEntry>, BTreeMap<String, RecencyMeta>) {
        let entries = vec![
            entry("Zelda-like", Console::Nes, "h-zelda"),
            entry("Alpha SNES Game", Console::Snes, "h-alpha"),
            entry("Beta NES Game", Console::Nes, "h-beta"),
            unrecognized("Mystery Cart"),
        ];
        let mut meta = BTreeMap::new();
        meta.insert(
            "h-zelda".to_string(),
            RecencyMeta {
                last_played_epoch_secs: Some(200),
                play_count: 3,
                favourite: true,
            },
        );
        meta.insert(
            "h-beta".to_string(),
            RecencyMeta {
                last_played_epoch_secs: Some(100),
                play_count: 1,
                favourite: false,
            },
        );
        // "h-alpha" and the unrecognized entry are deliberately absent —
        // never played, never favourited.
        (entries, meta)
    }

    #[test]
    fn console_filter_combines_with_the_console_hint() {
        let (entries, meta) = sample();
        let nes = filter_and_sort(
            &entries,
            &meta,
            Some(Console::Nes),
            RecencyFilter::All,
            "",
            Some(SortMode::Title),
        );
        assert_eq!(titles(&nes), vec!["Beta NES Game", "Zelda-like"]);
    }

    #[test]
    fn recently_played_chip_keeps_only_entries_with_a_last_played_time() {
        let (entries, meta) = sample();
        let recent = filter_and_sort(
            &entries,
            &meta,
            None,
            RecencyFilter::RecentlyPlayed,
            "",
            Some(SortMode::Title),
        );
        assert_eq!(titles(&recent), vec!["Beta NES Game", "Zelda-like"]);
    }

    #[test]
    fn favourites_chip_keeps_only_favourited_entries() {
        let (entries, meta) = sample();
        let favs = filter_and_sort(
            &entries,
            &meta,
            None,
            RecencyFilter::Favourites,
            "",
            Some(SortMode::Title),
        );
        assert_eq!(titles(&favs), vec!["Zelda-like"]);
    }

    /// The two new chips are radio-like with each other: this is enforced
    /// by construction (`RecencyFilter` is one field, not two booleans),
    /// so the test that matters is that each still combines with the
    /// console filter (AND), not with each other.
    #[test]
    fn recency_filter_combines_with_console_filter_rather_than_replacing_it() {
        let (entries, meta) = sample();
        let recent_snes = filter_and_sort(
            &entries,
            &meta,
            Some(Console::Snes),
            RecencyFilter::RecentlyPlayed,
            "",
            Some(SortMode::Title),
        );
        assert!(
            recent_snes.is_empty(),
            "Alpha SNES Game has never been played, so Recently played + SNES must be empty"
        );
    }

    #[test]
    fn search_text_still_applies_alongside_the_new_filters() {
        let (entries, meta) = sample();
        let hits = filter_and_sort(
            &entries,
            &meta,
            None,
            RecencyFilter::All,
            "beta",
            Some(SortMode::Title),
        );
        assert_eq!(titles(&hits), vec!["Beta NES Game"]);
    }

    #[test]
    fn sort_by_title_is_alphabetical_regardless_of_play_history() {
        let (entries, meta) = sample();
        let sorted = filter_and_sort(
            &entries,
            &meta,
            None,
            RecencyFilter::All,
            "",
            Some(SortMode::Title),
        );
        assert_eq!(
            titles(&sorted),
            vec![
                "Alpha SNES Game",
                "Beta NES Game",
                "Mystery Cart",
                "Zelda-like"
            ]
        );
    }

    #[test]
    fn sort_by_last_played_puts_most_recent_first_and_never_played_last() {
        let (entries, meta) = sample();
        let sorted = filter_and_sort(
            &entries,
            &meta,
            None,
            RecencyFilter::All,
            "",
            Some(SortMode::LastPlayed),
        );
        assert_eq!(
            titles(&sorted),
            vec![
                "Zelda-like",
                "Beta NES Game",
                "Alpha SNES Game",
                "Mystery Cart"
            ],
            "200 before 100 before never-played, alphabetical among the never-played"
        );
    }

    #[test]
    fn sort_by_console_groups_nes_then_snes_then_unidentified() {
        let (entries, meta) = sample();
        let sorted = filter_and_sort(
            &entries,
            &meta,
            None,
            RecencyFilter::All,
            "",
            Some(SortMode::Console),
        );
        assert_eq!(
            titles(&sorted),
            vec![
                "Beta NES Game",
                "Zelda-like",
                "Alpha SNES Game",
                "Mystery Cart"
            ],
            "NES titles (alphabetical) before SNES before the unidentified entry"
        );
    }

    /// §11's acceptance criterion: an empty search box with no sort chosen
    /// defaults to recent-first, never-played after.
    #[test]
    fn empty_search_with_no_explicit_sort_defaults_to_recent_first() {
        let (entries, meta) = sample();
        let default_order = filter_and_sort(&entries, &meta, None, RecencyFilter::All, "", None);
        let explicit_recent = filter_and_sort(
            &entries,
            &meta,
            None,
            RecencyFilter::All,
            "",
            Some(SortMode::LastPlayed),
        );
        assert_eq!(titles(&default_order), titles(&explicit_recent));
    }

    /// A non-empty search with no explicit sort falls back to Title order
    /// — a search result reshuffled by play history the query has nothing
    /// to do with would read as broken, not helpful.
    #[test]
    fn non_empty_search_with_no_explicit_sort_falls_back_to_title_order() {
        let (entries, meta) = sample();
        let default_order = filter_and_sort(&entries, &meta, None, RecencyFilter::All, "e", None);
        let explicit_title = filter_and_sort(
            &entries,
            &meta,
            None,
            RecencyFilter::All,
            "e",
            Some(SortMode::Title),
        );
        assert_eq!(titles(&default_order), titles(&explicit_title));
    }
}
