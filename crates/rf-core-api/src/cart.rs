//! Cartridge-image boundary type.
//!
//! Layering note (ARCHITECTURE §3/§6, enforced by `scripts/validate-arch.sh`):
//! `rf-core-api` may not depend on any `rf-*` crate, including `rf-cart`,
//! which owns cartridge header parsing / mapper identification / ROM
//! hashing. The ARCHITECTURE §5 sketch names `Cartridge` as the argument to
//! `EmulatorCore::load`, but that concrete, richer type lives in `rf-cart`
//! and cannot be named here without an upward dependency that the gate
//! forbids.
//!
//! Resolution: `EmulatorCore::load` takes [`CartImage`], a minimal,
//! dependency-free descriptor of raw bytes. Callers that hold a parsed
//! `rf_cart::Cartridge` (harness, frontend — both permitted to depend on
//! `rf-cart` directly, see the `Cores` layer diagram) extract the raw ROM
//! and any existing battery-backed save RAM to build a `CartImage` before
//! calling `load`. Richer identity data (mapper id, header fields, hash)
//! stays in `rf-cart` and is applied by the concrete core's own
//! `rf-cart`-dependent code *inside* its `load` implementation — `rf-nes`
//! and `rf-snes` are permitted to depend on `rf-cart` directly (ARCHITECTURE
//! §3 dependency rule), so nothing is lost, only re-parsed at the boundary
//! rather than passed pre-parsed through the trait.
use crate::error::CoreError;

/// Minimal, dependency-free cartridge image handed to [`crate::EmulatorCore::load`].
///
/// See the module doc for why this is not `rf_cart::Cartridge`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CartImage<'a> {
    /// Raw ROM bytes, exactly as read from the image file. Header
    /// stripping/mapper parsing is the concrete core's job (via `rf-cart`),
    /// not this crate's.
    pub rom: &'a [u8],
    /// Existing battery-backed save RAM to preload, if continuing a save.
    /// `None` means "cold" SRAM (core-defined initial fill, typically zero
    /// or 0xFF per hardware convention).
    pub sram: Option<&'a [u8]>,
}

impl<'a> CartImage<'a> {
    /// Build an image from ROM bytes only, no existing save RAM.
    #[must_use]
    pub const fn from_rom(rom: &'a [u8]) -> Self {
        CartImage { rom, sram: None }
    }

    /// Reject an image that is structurally unusable by any core (empty
    /// ROM). Concrete cores still run their own mapper/header validation on
    /// top of this; this is just the boundary-level sanity check shared by
    /// all of them.
    ///
    /// # Errors
    /// Returns [`CoreError::InvalidImage`] if `rom` is empty.
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.rom.is_empty() {
            return Err(CoreError::InvalidImage("cartridge ROM is empty".into()));
        }
        Ok(())
    }
}
