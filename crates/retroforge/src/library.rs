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

use std::collections::HashSet;
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

/// Which console an entry is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
pub fn first_run_state(configured_roots: &[PathBuf], library: &Library) -> FirstRunState {
    if configured_roots.is_empty() {
        return FirstRunState::NoRootsConfigured;
    }
    if library.entries.is_empty() {
        return FirstRunState::NoRomsFound {
            roots: configured_roots.to_vec(),
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
const ROM_EXTENSIONS: [&str; 4] = ["nes", "sfc", "smc", "fig"];

/// Scan `roots`, returning everything found and everything refused.
///
/// Never fails as a whole: a bad root becomes a [`ScanIssue`] and the other
/// roots are still scanned, because one mistyped path should not cost the
/// user their whole library.
#[must_use]
pub fn scan(roots: &[PathBuf]) -> Library {
    let mut library = Library::default();
    let mut visited: HashSet<PathBuf> = HashSet::new();

    for root in roots {
        let canonical_root = match root.canonicalize() {
            Ok(path) => path,
            Err(e) => {
                library.issues.push(ScanIssue::UnusableRoot {
                    root: root.clone(),
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
            &mut visited,
            &mut library,
        );
    }

    // Stable order so the grid does not reshuffle between scans; by title
    // then path, since two different dumps of one game share a title.
    library
        .entries
        .sort_by(|a, b| a.title.cmp(&b.title).then_with(|| a.path.cmp(&b.path)));
    library
}

fn scan_dir(
    dir: &Path,
    root: &Path,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
    library: &mut Library,
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
            scan_dir(&path, root, depth + 1, visited, library);
        } else if is_rom_candidate(&path) {
            ingest_file(&path, library);
        }
    }
}

fn is_rom_candidate(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|ext| ROM_EXTENSIONS.contains(&ext.as_str()))
}

fn ingest_file(path: &Path, library: &mut Library) {
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

    library.entries.push(LibraryEntry {
        path: path.to_path_buf(),
        title,
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

    #[test]
    fn a_scan_identifies_roms_by_normalized_hash_and_console() {
        let root = temp_dir("identify");
        std::fs::write(root.join("Game One.nes"), nes_rom(1)).unwrap();
        std::fs::write(root.join("Game Two.nes"), nes_rom(2)).unwrap();

        let library = scan(std::slice::from_ref(&root));
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

        let library = scan(std::slice::from_ref(&root));
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

        let library = scan(std::slice::from_ref(&root));
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

        let library = scan(std::slice::from_ref(&root));
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

        let library = scan(std::slice::from_ref(&root));

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

        let library = scan(std::slice::from_ref(&root));
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

        let library = scan(&[missing.clone(), good.clone()]);
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

        let library = scan(std::slice::from_ref(&root));
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
            first_run_state(std::slice::from_ref(&root), &empty),
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
            first_run_state(std::slice::from_ref(&root), &populated),
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
        let library = scan(std::slice::from_ref(&root));
        assert_eq!(library.entries.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
