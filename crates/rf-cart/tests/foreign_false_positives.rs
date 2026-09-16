//! A ratchet on how often `Cartridge::load` says yes to something that is
//! not a cartridge (ticket W14-05).
//!
//! # Why this test exists at all
//!
//! A SNES header carries **no magic number**. iNES starts `NES\x1A`, so the
//! NES half of the loader can simply look; the SNES half has to decide
//! whether a checksum, a reset vector and some text at `$7FC0` or `$FFC0`
//! mean anything. Until 2026-09-15 it accepted a single accidental match,
//! and pointed at a real 681-archive Game Boy folder it called **130 of
//! them** SNES cartridges — 19%. That is a soundness bug in the identity
//! function that per-game settings, save states, profile matching and the
//! library grid all key on: a false `Ok` mints an identity for a game that
//! does not exist.
//!
//! # Why it is a ratchet and not an assertion of zero
//!
//! **Zero is not reachable, and claiming it would be the dishonest version
//! of this test.** The floor is set by real cartridges, not by the
//! heuristic: prototypes and beta dumps ship with blanked headers whose
//! only remaining evidence is a reset vector and a map-mode nibble — two
//! bits of signal that arbitrary data can also supply. Any rule strict
//! enough to reject every foreign ROM also rejects those, and they are real
//! games somebody owns. So the bar is the measured rate, and this test
//! fails if it gets worse.
//!
//! # Running it
//!
//! Set `RF_FOREIGN_ROM_LIBRARY` to one or more directories of archives for
//! consoles this project does **not** emulate, colon-separated the way
//! `PATH` is:
//!
//! ```text
//! RF_FOREIGN_ROM_LIBRARY=~/Games/Roms/gb:~/Games/Roms/gba cargo test -p rf-cart
//! ```
//!
//! Each directory is read directly and its subdirectories are NOT walked,
//! so a parent folder that also holds NES and SNES games cannot be pointed
//! at by accident — those would count as "false" positives while being
//! exactly right, and a test that can be misconfigured into failing
//! teaches people to ignore it.
//!
//! It skips cleanly when the variable is unset, the same way the 65816
//! vector suite does, because no path under anybody's home directory
//! belongs in this repository and neither do ROM bytes (law 5).

use std::io::Read as _;
use std::path::{Path, PathBuf};

use rf_cart::Cartridge;

/// Highest share of foreign archives that may be mistaken for cartridges.
///
/// Measured 2026-09-15 over 2495 Game Boy, Game Boy Color, GBA and Virtual
/// Boy archives: **11**, or 0.44%. The pre-ticket rule scored 130 on the
/// Game Boy folder alone. One percent leaves room for a differently-shaped
/// collection without leaving room for the bug coming back.
const MAX_FALSE_POSITIVE_RATE: f64 = 0.01;

/// Cap on bytes read from any one archive entry, mirroring the shell's
/// untrusted-archive posture: never trust a declared size.
const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;

#[test]
fn foreign_roms_are_rarely_mistaken_for_cartridges() {
    let Ok(root) = std::env::var("RF_FOREIGN_ROM_LIBRARY") else {
        eprintln!("SKIP: set RF_FOREIGN_ROM_LIBRARY to a folder of non-NES/SNES ROMs");
        return;
    };

    let mut archives = 0u32;
    let mut false_positives = 0u32;
    let mut examples: Vec<String> = Vec::new();

    for entry in root.split(':').flat_map(|dir| archives_in(Path::new(dir))) {
        let Ok(raw) = std::fs::read(&entry) else {
            continue;
        };
        archives += 1;
        for candidate in rom_entries(&raw) {
            if Cartridge::load(&candidate).is_ok() {
                false_positives += 1;
                if examples.len() < 5 {
                    examples.push(
                        entry
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
                break;
            }
        }
    }

    assert!(
        archives > 0,
        "RF_FOREIGN_ROM_LIBRARY names no archives — a test that scanned nothing \
         must not report success"
    );
    let rate = f64::from(false_positives) / f64::from(archives);
    eprintln!(
        "foreign archives: {archives}, mistaken for cartridges: {false_positives} ({rate:.4})"
    );
    for example in &examples {
        eprintln!("   e.g. {example}");
    }
    assert!(
        rate <= MAX_FALSE_POSITIVE_RATE,
        "{false_positives} of {archives} foreign archives were accepted as cartridges \
         ({rate:.4}), above the {MAX_FALSE_POSITIVE_RATE} ratchet. The SNES header \
         heuristic has loosened — see crates/rf-cart/src/snes.rs, score_candidate."
    );
}

/// Every `.zip` directly inside `dir`. One level, never recursive — see
/// the module doc for why descending would make this test misconfigurable.
fn archives_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("zip"))
        .collect()
}

/// The bytes of every file in an archive, capped, so each can be offered to
/// the sniffer. Nested archives are skipped rather than recursed.
fn rom_entries(raw: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(raw)) else {
        return out;
    };
    for index in 0..archive.len() {
        let Ok(mut file) = archive.by_index(index) else {
            break;
        };
        if !file.is_file() || file.name().to_ascii_lowercase().ends_with(".zip") {
            continue;
        }
        let mut buf = Vec::new();
        if (&mut file)
            .take(MAX_ENTRY_BYTES)
            .read_to_end(&mut buf)
            .is_ok()
        {
            out.push(buf);
        }
    }
    out
}
