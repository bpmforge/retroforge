//! ROM open dialog (ticket W1-06 acceptance criterion 3; zip support
//! added by W2-13).
//!
//! Split into a headless, testable half ([`load_rom_bytes`] — read a path
//! off disk and sniff/validate it via `rf-cart` before ever handing bytes
//! to a console core) and a window-owning half ([`pick_rom_file`], a thin
//! wrapper over `rfd::FileDialog` that cannot be exercised in CI — see
//! `docs/TECH_STACK.md` §2's `rfd` row).
//!
//! ## Zipped ROMs (ticket W2-13)
//!
//! [`resolve_rom_bytes`] is the pure, path-free core: hand it the bytes of
//! either a bare ROM or a `.zip` and it returns the ROM's bytes. Entries
//! are identified by **sniffing their content** through
//! [`rf_cart::Cartridge::load`], never by file extension — that is what
//! makes an arbitrarily-named entry work (the fetched `alter_ego.zip` is
//! the motivating real case) and it reuses the exact console-agnostic
//! validation a bare file already goes through.
//!
//! ### A zip is untrusted input
//!
//! Same posture `rf-state` already takes for its zstd body (see
//! `docs/design/SAVE_STATES.md` §2, "Robustness"): a zip carries
//! attacker-controlled metadata, so
//!
//! - the cap is on bytes **actually read** ([`MAX_ENTRY_BYTES`], enforced
//!   with `Read::take`), never on the central directory's `size()` field,
//!   which a malicious archive can simply lie about;
//! - cumulative bytes across all entries are capped too
//!   ([`MAX_TOTAL_BYTES`]) — one entry under the limit is not a bound when
//!   an archive may hold thousands of them;
//! - nested archives are **refused, never recursed** — no unbounded depth;
//! - **nothing is ever written to disk.** Every entry is read into memory
//!   and discarded unless it is the chosen ROM, which is why path
//!   traversal (`../..` entry names, absolute paths, symlink entries) is
//!   not a concern here. A future edit that adds real extraction must
//!   re-open that question rather than assume this note still holds.
use std::fmt;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use rf_cart::{CartError, Cartridge};

/// Local-file-header magic every non-empty zip starts with (`PK\x03\x04`).
/// An empty archive starts `PK\x05\x06`, which is deliberately NOT treated
/// as a zip here: it holds no ROM, so falling through to the ordinary
/// "not a recognizable ROM image" path gives a better message than
/// "archive contains no ROM". No real ROM format can collide with this —
/// iNES starts `NES\x1A`.
const ZIP_MAGIC: [u8; 4] = [b'P', b'K', 0x03, 0x04];

/// Per-entry read cap. Matches the 64 MiB ceiling `rf-state` already uses
/// for the same reason (a compressed stream declares no trustworthy
/// decompressed size). Far above any real cartridge: the largest NES image
/// is a few MiB, the largest SNES image ~12 MiB.
const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;

/// Cumulative read cap across every entry inspected in one archive — a
/// per-entry cap alone bounds nothing when an archive can hold thousands
/// of entries each just under it.
const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;

/// Everything that can go wrong turning a picked file into ROM bytes ready
/// to hand to `rf-nes`.
#[derive(Debug)]
pub enum RomOpenError {
    /// Couldn't read the file at all (permissions, doesn't exist, ...).
    Io(std::io::Error),
    /// `rf-cart` couldn't parse it as either an NES or SNES image.
    Cart(CartError),
    /// The file starts with zip magic but could not be read as an archive
    /// (truncated, corrupt, or an unsupported compression method).
    Zip(zip::result::ZipError),
    /// A readable archive holding no entry that sniffs as a ROM. Carries
    /// how many entries were inspected so the message can distinguish
    /// "empty archive" from "archive full of screenshots and a README".
    NoRomInArchive { entries_inspected: usize },
    /// The archive held exactly one entry that **is** a cartridge, but
    /// `rf-cart` rejected it — an unsupported mapper being the common
    /// case. Ticket W2-16: without this variant the specific, already
    /// generated diagnostic was discarded and the user saw the generic
    /// [`RomOpenError::NoRomInArchive`] instead, which reads as "your file
    /// is junk" when the real answer is "this emulator does not support
    /// mapper N yet". Reported by Brad against a real NES 2.0 mapper-7
    /// (AxROM) archive that loads in other emulators.
    ArchiveEntryRejected { name: String, source: CartError },
    /// A readable archive holding more than one ROM. Deliberately an
    /// error, never a silent pick: zip entry order is an artifact of how
    /// the archive was written, not a meaningful ranking, so choosing for
    /// the user would be choosing arbitrarily.
    MultipleRomsInArchive { names: Vec<String> },
}

impl fmt::Display for RomOpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RomOpenError::Io(e) => write!(f, "could not read ROM file: {e}"),
            RomOpenError::Cart(e) => write!(f, "not a recognizable ROM image: {e}"),
            RomOpenError::Zip(e) => write!(f, "could not read the zip archive: {e}"),
            RomOpenError::NoRomInArchive { entries_inspected } => write!(
                f,
                "the zip archive contains no recognizable ROM ({entries_inspected} \
                 entr{} inspected)",
                if *entries_inspected == 1 { "y" } else { "ies" }
            ),
            RomOpenError::ArchiveEntryRejected { name, source } => write!(
                f,
                "the zip archive's only ROM image, '{name}', could not be loaded: {source}"
            ),
            RomOpenError::MultipleRomsInArchive { names } => write!(
                f,
                "the zip archive contains {} ROMs ({}); extract the one you want and \
                 open it directly",
                names.len(),
                names.join(", ")
            ),
        }
    }
}

impl std::error::Error for RomOpenError {}

/// Read `path` and validate it holds a cartridge this build has a core
/// for (console-agnostic sniff via `rf_cart::Cartridge::load`) before
/// returning its raw bytes. Does not touch `rf-nes` or `rf-snes` directly
/// — `EmuStepper::open` dispatches on the sniffed console and each core
/// does its own validation (mapper/chip support) on top of this.
///
/// # Errors
/// See [`RomOpenError`].
pub fn load_rom_bytes(path: &Path) -> Result<Vec<u8>, RomOpenError> {
    let bytes = std::fs::read(path).map_err(RomOpenError::Io)?;
    resolve_rom_bytes(bytes)
}

/// The pure, path-free core of [`load_rom_bytes`] (module doc): turn the
/// bytes of either a bare ROM file or a `.zip` into the ROM's bytes.
///
/// Takes ownership so the common bare-ROM case moves its buffer straight
/// through with no copy.
///
/// # Errors
/// See [`RomOpenError`].
pub fn resolve_rom_bytes(bytes: Vec<u8>) -> Result<Vec<u8>, RomOpenError> {
    if bytes.starts_with(&ZIP_MAGIC) {
        return rom_from_zip(&bytes);
    }
    validate_rom(bytes)
}

/// Shared final check for both paths: a candidate is only a ROM this app
/// can open if `rf-cart` sniffs it as a cartridge of a console this build
/// has a core for.
fn validate_rom(bytes: Vec<u8>) -> Result<Vec<u8>, RomOpenError> {
    match Cartridge::load(&bytes) {
        // Ticket W11-12: BOTH consoles. This arm returned
        // `NotNesImage` from W1-06 until now, which was honest while
        // there was no SNES core to hand the bytes to — and became a
        // second, quieter gate the moment there was one. `spawn`
        // dispatches on cartridge type; this only has to stop deciding
        // for it.
        Ok(Cartridge::Nes { .. } | Cartridge::Snes { .. }) => Ok(bytes),
        Err(e) => Err(RomOpenError::Cart(e)),
    }
}

/// Find the single ROM inside a zip archive (module doc for the
/// untrusted-input posture). Every entry is read into memory under
/// [`MAX_ENTRY_BYTES`]/[`MAX_TOTAL_BYTES`] and sniffed through
/// [`Cartridge::load`]; nothing is written to disk.
fn rom_from_zip(bytes: &[u8]) -> Result<Vec<u8>, RomOpenError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(RomOpenError::Zip)?;
    let mut candidates: Vec<(String, Vec<u8>)> = Vec::new();
    // Entries that ARE cartridges but which rf-cart refused, kept so the
    // real reason can be reported instead of the generic no-ROM message
    // (ticket W2-16; widened past NES by W14-01 — see
    // `is_refused_cartridge`).
    let mut rejected: Vec<(String, CartError)> = Vec::new();
    let mut inspected = 0usize;
    let mut total_read = 0u64;

    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(RomOpenError::Zip)?;
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_string();
        // Refuse rather than recurse (module doc): a nested archive is the
        // unbounded-depth case, and nothing about a ROM needs it.
        if name.to_ascii_lowercase().ends_with(".zip") {
            continue;
        }
        // Ticket W14-01: an entry that positively declares a console this
        // build does not run is not offered to the sniffer at all. This is
        // a DENYLIST, so the module doc's "chosen by content, never by
        // extension" still holds for every unknown name — see
        // `library::names_a_foreign_console` for why it is needed at all
        // (the SNES sniff false-positives on foreign ROM data).
        if crate::library::names_a_foreign_console(&name) {
            continue;
        }
        inspected += 1;

        let budget = MAX_ENTRY_BYTES.min(MAX_TOTAL_BYTES.saturating_sub(total_read));
        if budget == 0 {
            break;
        }
        let mut buf = Vec::new();
        // `take` bounds what is actually READ — never trust `entry.size()`,
        // which the central directory can misreport (module doc).
        entry
            .take(budget)
            .read_to_end(&mut buf)
            .map_err(RomOpenError::Io)?;
        total_read = total_read.saturating_add(buf.len() as u64);

        match Cartridge::load(&buf) {
            // BOTH consoles (ticket W14-01). The SNES arm was empty from
            // W1-06 until now: an entry that sniffed as a SNES cartridge
            // was inspected, recognized, and then dropped, after which the
            // archive reported the generic "contains no recognizable ROM".
            // The BARE path (`validate_rom` below) has accepted both since
            // W11-12, which is what made this a bug rather than a
            // decision — and no test covered a zipped SNES ROM, which is
            // why it survived. Measured blast radius on a real No-Intro
            // set: 1119 of 1265 SNES archives unopenable.
            Ok(Cartridge::Nes { .. } | Cartridge::Snes { .. }) => candidates.push((name, buf)),
            Err(e) => {
                // Only worth reporting if the entry really was a cartridge;
                // otherwise every text file in the archive would produce a
                // confusing "could not be loaded" complaint.
                if is_refused_cartridge(&e, &buf, &name) {
                    rejected.push((name, e));
                }
            }
        }
    }

    match candidates.len() {
        1 => Ok(candidates.remove(0).1),
        // Ticket W2-16: prefer the specific reason over the generic one.
        // Exactly one rejected cartridge means we know precisely why this
        // archive did not load, and saying so beats "no recognizable ROM".
        0 if rejected.len() == 1 => {
            let (name, source) = rejected.remove(0);
            Err(RomOpenError::ArchiveEntryRejected { name, source })
        }
        0 => Err(RomOpenError::NoRomInArchive {
            entries_inspected: inspected,
        }),
        _ => Err(RomOpenError::MultipleRomsInArchive {
            names: candidates.into_iter().map(|(name, _)| name).collect(),
        }),
    }
}

/// Did `rf-cart` refuse something that genuinely IS a cartridge, as
/// opposed to failing on a README that happened to share an archive with
/// one?
///
/// Ticket W2-16 answered this for NES with an iNES-magic check, which is
/// kept: an iNES image that fails to load is always worth naming. It does
/// not generalise, because a SNES cartridge has no leading magic — its
/// header sits at `$7FC0` or `$FFC0` inside the image. W14-01 widens it
/// two ways:
///
/// - by ERROR KIND. [`CartError::UnsupportedMapper`] and
///   [`CartError::UnsupportedChip`] are only ever produced once a header
///   has been located and understood, so the file WAS a cartridge and this
///   build cannot run it — exactly W2-16's case, and the commonest reason
///   a real library refuses a SNES title (Super FX, DSP, S-DD1, SA-1).
/// - by ENTRY NAME. A `.sfc` that fails to parse is a broken dump and the
///   user should be told; a `.gb` that fails to parse is simply not for
///   this emulator. Without this, a corrupt BARE `.sfc` was listed as
///   unrecognized while the same file zipped vanished silently — the same
///   file, two answers. Measured on a real set: 18 SNES archives with
///   junk headers (mostly prototypes) fell into that gap.
///
/// Anything else stays silent, which is what a README looks like.
fn is_refused_cartridge(e: &CartError, buf: &[u8], entry_name: &str) -> bool {
    /// Extensions that assert "this is a cartridge image", matching the
    /// library scanner's own pre-filter.
    const ROM_EXTENSIONS: [&str; 4] = ["nes", "sfc", "smc", "fig"];

    let named_as_a_rom = Path::new(entry_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|ext| ROM_EXTENSIONS.contains(&ext.as_str()));

    matches!(
        e,
        CartError::UnsupportedMapper { .. } | CartError::UnsupportedChip { .. }
    ) || buf.starts_with(&rf_cart::nes::INES_MAGIC)
        || named_as_a_rom
}

/// Show a native "Open ROM" file dialog and return the picked path, or
/// `None` if the user cancelled. Cannot run headlessly (opens an OS
/// dialog) — see the manual checklist in this ticket's commit body for how
/// this was verified.
#[must_use]
pub fn pick_rom_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Open ROM")
        // `zip` belongs in the FIRST filter, not a separate one: it is the
        // default selection, so a zipped ROM must be pickable without the
        // user knowing to change the dropdown. Teaching the loader to read
        // an archive while leaving it unselectable in the picker would be
        // a feature nobody can reach (ticket W2-13).
        .add_filter("ROM", &["nes", "sfc", "smc", "fig", "zip"])
        .add_filter("All files", &["*"])
        .pick_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn synthetic_nrom_bytes() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&rf_cart::nes::INES_MAGIC);
        data.push(1);
        data.push(1);
        data.extend_from_slice(&[0u8; 10]);
        data.extend(vec![0u8; 16 * 1024]);
        data.extend(vec![0u8; 8 * 1024]);
        data
    }

    fn write_temp(name: &str, bytes: &[u8]) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "retroforge-rom-open-test-{name}-{}",
            std::process::id()
        ));
        let mut f = std::fs::File::create(&path).expect("create temp file");
        f.write_all(bytes).expect("write temp file");
        path
    }

    #[test]
    fn valid_nes_image_loads() {
        let path = write_temp("valid", &synthetic_nrom_bytes());
        let result = load_rom_bytes(&path);
        std::fs::remove_file(&path).ok();
        let bytes = result.expect("synthetic NROM image must load");
        assert_eq!(bytes.len(), 16 + 16 * 1024 + 8 * 1024);
    }

    #[test]
    fn garbage_bytes_are_rejected_not_panicking() {
        let path = write_temp("garbage", &[0u8; 4]);
        let result = load_rom_bytes(&path);
        std::fs::remove_file(&path).ok();
        assert!(matches!(result, Err(RomOpenError::Cart(_))));
    }

    #[test]
    fn missing_file_is_reported_as_io_error() {
        let mut path = std::env::temp_dir();
        path.push("retroforge-rom-open-test-does-not-exist.nes");
        let result = load_rom_bytes(&path);
        assert!(matches!(result, Err(RomOpenError::Io(_))));
    }

    // -----------------------------------------------------------------
    // Zipped ROMs (ticket W2-13). Archives are built IN MEMORY, never on
    // disk: this module's `write_temp` above keys its path on
    // `std::process::id()` alone, and libtest runs a binary's tests in
    // parallel threads that share a pid — a hazard this project has
    // already been bitten by. `resolve_rom_bytes` is path-free precisely
    // so these tests need no filesystem at all.
    // -----------------------------------------------------------------

    /// Build a zip in memory from `(name, contents)` pairs. `Stored`
    /// (uncompressed) keeps the fixtures byte-predictable; the deflate
    /// path is exercised by `deflated_entry_is_read`.
    fn zip_with(entries: &[(&str, &[u8])], method: zip::CompressionMethod) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut cursor);
            let opts: zip::write::FileOptions<'_, ()> =
                zip::write::FileOptions::default().compression_method(method);
            for (name, contents) in entries {
                w.start_file(*name, opts).expect("start zip entry");
                std::io::Write::write_all(&mut w, contents).expect("write zip entry");
            }
            w.finish().expect("finish zip");
        }
        cursor.into_inner()
    }

    #[test]
    fn zip_containing_one_rom_resolves_to_that_rom() {
        let rom = synthetic_nrom_bytes();
        // Deliberately NOT named `.nes`: entries are identified by
        // sniffing their bytes, never by extension (module doc), which is
        // what makes a real-world archive with an arbitrary inner name
        // work.
        let archive = zip_with(
            &[("some_game_v1.0 (PD).bin", &rom)],
            zip::CompressionMethod::Stored,
        );
        let out = resolve_rom_bytes(archive).expect("zip holding exactly one ROM must resolve");
        assert_eq!(out, rom, "the resolved bytes must be the inner ROM itself");
    }

    #[test]
    fn deflated_entry_is_read() {
        let rom = synthetic_nrom_bytes();
        let archive = zip_with(&[("game.nes", &rom)], zip::CompressionMethod::Deflated);
        let out = resolve_rom_bytes(archive).expect("a deflate-compressed entry must decompress");
        assert_eq!(out, rom);
    }

    #[test]
    fn zip_ignores_non_rom_entries_alongside_the_rom() {
        let rom = synthetic_nrom_bytes();
        let archive = zip_with(
            &[
                ("readme.txt", b"public domain, have fun".as_slice()),
                (
                    "cover.png",
                    b"\x89PNG\r\n\x1a\n not really a png".as_slice(),
                ),
                ("game.nes", &rom),
            ],
            zip::CompressionMethod::Stored,
        );
        let out = resolve_rom_bytes(archive).expect("junk alongside one ROM must still resolve");
        assert_eq!(out, rom);
    }

    /// Anti-vacuity: without this, an implementation that simply returned
    /// the raw archive bytes would pass every "a zip loads" test above.
    /// A minimal LoROM image: enough header for `rf_cart` to accept it.
    /// Same shape `rf-snes`'s own tests use.
    fn snes_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x0000] = 0x80;
        rom[0x0001] = 0xFE;
        for (i, b) in b"RF ZIP TEST          ".iter().enumerate() {
            rom[0x7FC0 + i] = *b;
        }
        rom[0x7FD5] = 0x20;
        rom[0x7FD6] = 0x00;
        rom[0x7FD7] = 0x08;
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        rom
    }

    /// Ticket W14-01, and this is the regression the ticket exists for.
    ///
    /// The `Ok(Cartridge::Snes { .. })` arm of `rom_from_zip` was EMPTY
    /// from W1-06 until now: a SNES entry was read, sniffed, recognized —
    /// and dropped, after which the archive reported the generic "contains
    /// no recognizable ROM". A bare `.sfc` opened fine the whole time,
    /// which is what made it a bug rather than a decision. Measured
    /// against a real No-Intro set, it cost 1119 of 1265 SNES archives.
    #[test]
    fn a_zipped_snes_cartridge_opens_the_same_as_a_bare_one() {
        let rom = snes_rom();
        let bare = resolve_rom_bytes(rom.clone()).expect("a bare SNES image opens");
        let archive = zip_with(
            &[("Some Game (USA).sfc", rom.as_slice())],
            zip::CompressionMethod::Deflated,
        );
        let zipped = resolve_rom_bytes(archive).expect("a zipped SNES image must open too");
        assert_eq!(
            bare, zipped,
            "the bytes handed to the core must be the cartridge, not the archive"
        );
    }

    /// The refusal reason survives for a SNES entry too. Before W14-01
    /// this could not arise (the entry was dropped before any error was
    /// considered); the NES half of it is W2-16's and is asserted below.
    #[test]
    fn a_zipped_cartridge_this_build_refuses_is_named_not_generic() {
        let mut rom = snes_rom();
        // Chipset byte $13 is Super FX, which `rf-cart` refuses by name —
        // the single most common reason a real SNES library refuses a
        // title, and an error kind only reachable once a header has been
        // located and understood.
        rom[0x7FD6] = 0x13;
        let archive = zip_with(
            &[("Star Whatever (USA).sfc", rom.as_slice())],
            zip::CompressionMethod::Stored,
        );
        match resolve_rom_bytes(archive) {
            Err(RomOpenError::ArchiveEntryRejected { name, source }) => {
                assert_eq!(name, "Star Whatever (USA).sfc");
                assert!(
                    source.to_string().contains("chip"),
                    "the specific reason must survive, got {source}"
                );
            }
            other => panic!("expected the named refusal, got {other:?}"),
        }
    }

    /// And a README next to nothing still stays quiet: `is_refused_cartridge`
    /// widened the report by ERROR KIND, not by reporting every unparsed
    /// file, which is what W2-16's iNES-magic guard was protecting against.
    #[test]
    fn a_readme_is_not_reported_as_a_refused_cartridge() {
        let archive = zip_with(
            &[("readme.txt", b"just notes".as_slice())],
            zip::CompressionMethod::Stored,
        );
        assert!(
            matches!(
                resolve_rom_bytes(archive),
                Err(RomOpenError::NoRomInArchive { .. })
            ),
            "a text file must not be named as a refused cartridge"
        );
    }

    #[test]
    fn zip_with_no_rom_is_refused() {
        let archive = zip_with(
            &[
                ("readme.txt", b"nothing to see".as_slice()),
                ("screenshot.png", b"also not a rom".as_slice()),
            ],
            zip::CompressionMethod::Stored,
        );
        match resolve_rom_bytes(archive) {
            Err(RomOpenError::NoRomInArchive { entries_inspected }) => {
                assert_eq!(
                    entries_inspected, 2,
                    "both entries must have been inspected"
                );
            }
            other => panic!("expected NoRomInArchive, got {other:?}"),
        }
    }

    /// Two ROMs must be an error naming both — never a silent pick, since
    /// zip entry order is an artifact of how the archive was written.
    #[test]
    fn zip_with_two_roms_is_refused_naming_them() {
        let rom = synthetic_nrom_bytes();
        let archive = zip_with(
            &[("disc_a.nes", &rom), ("disc_b.nes", &rom)],
            zip::CompressionMethod::Stored,
        );
        match resolve_rom_bytes(archive) {
            Err(RomOpenError::MultipleRomsInArchive { names }) => {
                assert_eq!(names.len(), 2);
                assert!(
                    names.iter().any(|n| n == "disc_a.nes")
                        && names.iter().any(|n| n == "disc_b.nes"),
                    "the diagnostic must name both candidates, got {names:?}"
                );
            }
            other => panic!("expected MultipleRomsInArchive, got {other:?}"),
        }
    }

    #[test]
    fn nested_zip_entry_is_not_recursed_into() {
        let rom = synthetic_nrom_bytes();
        let inner = zip_with(&[("game.nes", &rom)], zip::CompressionMethod::Stored);
        let outer = zip_with(&[("inner.zip", &inner)], zip::CompressionMethod::Stored);
        match resolve_rom_bytes(outer) {
            Err(RomOpenError::NoRomInArchive { entries_inspected }) => {
                assert_eq!(
                    entries_inspected, 0,
                    "a nested archive must be skipped outright, not inspected or recursed into"
                );
            }
            other => panic!("expected NoRomInArchive, got {other:?}"),
        }
    }

    /// A bare ROM must still take the original path untouched — the
    /// regression this feature could most easily cause.
    #[test]
    fn bare_rom_bytes_still_resolve_unchanged() {
        let rom = synthetic_nrom_bytes();
        let out = resolve_rom_bytes(rom.clone()).expect("a bare NROM image must still load");
        assert_eq!(out, rom);
    }

    /// Build a synthetic iNES image declaring `mapper`, used to reproduce
    /// the real-world case without any copyrighted ROM bytes (NFR-006).
    fn ines_declaring_mapper(mapper: u8) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&rf_cart::nes::INES_MAGIC);
        data.push(1); // 1x16KiB PRG
        data.push(1); // 1x8KiB CHR
        data.push((mapper & 0x0F) << 4); // flags6: mapper low nibble
        data.push(mapper & 0xF0); // flags7: mapper high nibble
        data.extend_from_slice(&[0u8; 8]);
        data.extend(vec![0u8; 16 * 1024]);
        data.extend(vec![0u8; 8 * 1024]);
        data
    }

    /// Ticket W2-16, the reported bug: an archive whose single entry IS a
    /// real NES image but carries an unsupported mapper must report THAT,
    /// not the generic "no recognizable ROM". Brad hit this with a NES 2.0
    /// mapper-7 (AxROM) archive that loads in other emulators, and the
    /// generic message reads as "your file is junk" when the true answer
    /// is "this emulator does not support mapper 7 yet".
    ///
    /// RETARGETED 7 -> 5 by ticket W2-17, which implemented AxROM. This is
    /// the THIRD test to expire this way (W2-02 retargeted one off mapper
    /// 1, W2-03 retired one off mapper 4), because "the currently
    /// unsupported mapper" is a premise that dies every time a mapper
    /// lands. Mapper 5 (MMC5) is the durable choice: EMULATION_CORES.md
    /// section 2.4 lists MMC2/4 and MMC5 as "explicitly deferred", so it
    /// is out of scope by design rather than merely not-yet-done. rf-cart
    /// still NAMES it ("MMC5 / ExROM"), which is what this test needs —
    /// the point is that a *named* diagnostic reaches the user.
    #[test]
    fn zip_whose_only_rom_has_an_unsupported_mapper_reports_the_real_reason() {
        let archive = zip_with(
            &[("Some Game (USA).nes", &ines_declaring_mapper(5))],
            zip::CompressionMethod::Stored,
        );
        match resolve_rom_bytes(archive) {
            Err(RomOpenError::ArchiveEntryRejected { name, source }) => {
                assert_eq!(name, "Some Game (USA).nes");
                let shown = source.to_string();
                assert!(
                    shown.contains('5'),
                    "the diagnostic must name the mapper number, got: {shown}"
                );
            }
            other => panic!("expected ArchiveEntryRejected, got {other:?}"),
        }
    }

    /// The generic path must survive: an archive with no NES image at all
    /// still reports NoRomInArchive. Without this, "always report the
    /// specific reason" could collapse the two cases into one and lose the
    /// distinction the fix exists to create.
    #[test]
    fn zip_with_no_nes_image_at_all_still_reports_the_generic_message() {
        let archive = zip_with(
            &[
                ("readme.txt", b"no rom here".as_slice()),
                ("art.png", b"still not a rom".as_slice()),
            ],
            zip::CompressionMethod::Stored,
        );
        assert!(
            matches!(
                resolve_rom_bytes(archive),
                Err(RomOpenError::NoRomInArchive {
                    entries_inspected: 2
                })
            ),
            "an archive with no NES image must keep the generic diagnostic"
        );
    }

    #[test]
    fn truncated_zip_is_reported_as_a_zip_error_not_a_panic() {
        let rom = synthetic_nrom_bytes();
        let mut archive = zip_with(&[("game.nes", &rom)], zip::CompressionMethod::Stored);
        archive.truncate(archive.len() / 2);
        assert!(matches!(
            resolve_rom_bytes(archive),
            Err(RomOpenError::Zip(_))
        ));
    }
}
