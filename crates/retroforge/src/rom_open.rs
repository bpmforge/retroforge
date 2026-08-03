//! ROM open dialog (ticket W1-06 acceptance criterion 3).
//!
//! Split into a headless, testable half ([`load_rom_bytes`] — read a path
//! off disk and sniff/validate it via `rf-cart` before ever handing bytes
//! to a console core) and a window-owning half ([`pick_rom_file`], a thin
//! wrapper over `rfd::FileDialog` that cannot be exercised in CI — see
//! `docs/TECH_STACK.md` §2's `rfd` row).
use std::fmt;
use std::path::{Path, PathBuf};

use rf_cart::{CartError, Cartridge};

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
    match Cartridge::load(&bytes) {
        Ok(Cartridge::Nes { .. }) => Ok(bytes),
        Ok(Cartridge::Snes { .. }) => Err(RomOpenError::NotNesImage),
        Err(e) => Err(RomOpenError::Cart(e)),
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
        .add_filter("NES ROM", &["nes"])
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
}
