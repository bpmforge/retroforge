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
    /// Parsed fine, but as an SNES image — this ticket only wires an NES
    /// core (`rf-snes` doesn't exist yet), so report that plainly rather
    /// than attempting a load that would fail deeper in the stack with a
    /// less useful message.
    NotNesImage,
    /// The file starts with zip magic but could not be read as an archive
    /// (truncated, corrupt, or an unsupported compression method).
    Zip(zip::result::ZipError),
    /// A readable archive holding no entry that sniffs as a ROM. Carries
    /// how many entries were inspected so the message can distinguish
    /// "empty archive" from "archive full of screenshots and a README".
    NoRomInArchive { entries_inspected: usize },
    /// The archive held exactly one entry that **is** a NES image, but
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
            RomOpenError::NotNesImage => {
                write!(
                    f,
                    "this is an SNES image; only NES ROMs are supported so far"
                )
            }
            RomOpenError::Zip(e) => write!(f, "could not read the zip archive: {e}"),
            RomOpenError::NoRomInArchive { entries_inspected } => write!(
                f,
                "the zip archive contains no recognizable ROM ({entries_inspected} \
                 entr{} inspected)",
                if *entries_inspected == 1 { "y" } else { "ies" }
            ),
            RomOpenError::ArchiveEntryRejected { name, source } => write!(
                f,
                "the zip archive's only NES image, '{name}', could not be loaded: {source}"
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

/// Read `path` and validate it's a loadable NES image (console-agnostic
/// sniff via `rf_cart::Cartridge::load`, same entry point a future SNES
/// core would share) before returning its raw bytes. Does not touch
/// `rf-nes` directly — `EmuStepper::from_ines_bytes` does its own,
/// NES-specific validation (mapper support etc.) on top of this.
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
    validate_nes(bytes)
}

/// Shared final check for both paths: a candidate is only a ROM this app
/// can open if `rf-cart` sniffs it as NES.
fn validate_nes(bytes: Vec<u8>) -> Result<Vec<u8>, RomOpenError> {
    match Cartridge::load(&bytes) {
        Ok(Cartridge::Nes { .. }) => Ok(bytes),
        Ok(Cartridge::Snes { .. }) => Err(RomOpenError::NotNesImage),
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
    // Entries that ARE NES images but which rf-cart refused, kept so the
    // real reason can be reported instead of the generic no-ROM message
    // (ticket W2-16).
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
            Ok(Cartridge::Nes { .. }) => candidates.push((name, buf)),
            // Not a ROM at all (a README, a PNG): silently skipped, as
            // before — those are expected archive contents.
            Ok(Cartridge::Snes { .. }) => {}
            Err(e) => {
                // Only worth reporting if it really is a NES image;
                // otherwise every text file in the archive would produce
                // a confusing "could not be loaded" complaint.
                if buf.starts_with(&rf_cart::nes::INES_MAGIC) {
                    rejected.push((name, e));
                }
            }
        }
    }

    match candidates.len() {
        1 => Ok(candidates.remove(0).1),
        // Ticket W2-16: prefer the specific reason over the generic one.
        // Exactly one rejected NES image means we know precisely why this
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

/// Show a native "Open ROM" file dialog and return the picked path, or
/// `None` if the user cancelled. Cannot run headlessly (opens an OS
/// dialog) — see the manual checklist in this ticket's commit body for how
/// this was verified.
#[must_use]
pub fn pick_rom_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Open NES ROM")
        // `zip` belongs in the FIRST filter, not a separate one: it is the
        // default selection, so a zipped ROM must be pickable without the
        // user knowing to change the dropdown. Teaching the loader to read
        // an archive while leaving it unselectable in the picker would be
        // a feature nobody can reach (ticket W2-13).
        .add_filter("NES ROM", &["nes", "zip"])
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
    #[test]
    fn zip_whose_only_rom_has_an_unsupported_mapper_reports_the_real_reason() {
        // 7 = AxROM: rf-cart names it but does not support it.
        let archive = zip_with(
            &[("Some Game (USA).nes", &ines_declaring_mapper(7))],
            zip::CompressionMethod::Stored,
        );
        match resolve_rom_bytes(archive) {
            Err(RomOpenError::ArchiveEntryRejected { name, source }) => {
                assert_eq!(name, "Some Game (USA).nes");
                let shown = source.to_string();
                assert!(
                    shown.contains('7'),
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
