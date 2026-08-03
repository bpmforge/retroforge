//! The `.rfstate` header — the uncompressed prefix of every container, so
//! the magic/version/ROM-hash checks in [`Header::read`] never require
//! spending work decompressing a body first (CONTRACTS.md §3, SAVE_STATES
//! §2).
//!
//! # Byte layout (little-endian throughout, all offsets in bytes)
//!
//! ```text
//! offset  size  field
//! 0       4     magic               = b"RFST"
//! 4       2     container_version   u16, this format's version (= 1)
//! 6       1     console             u8  (0 = NES, 1 = SNES; other values
//!                                        pass through uninterpreted —
//!                                        rf-state does not depend on
//!                                        rf-cart and does not validate
//!                                        this byte)
//! 7       2     flags               u16, MUST be 0 in v1 (no bits
//!                                        defined; nonzero is refused,
//!                                        not ignored)
//! 9       32    rom_sha256          [u8; 32], normalized-ROM SHA-256
//! 41      2     emu_version_len     u16, byte length of the field below
//!                                        (capped at MAX_EMU_VERSION_LEN)
//! 43      N     emu_version         UTF-8 semver string, N =
//!                                        emu_version_len
//! 43+N    8     timestamp           u64, Unix seconds — METADATA ONLY,
//!                                        see the module-level note below
//! 51+N    ...   chunk body          zstd-compressed TLV chunk list
//!                                        (chunk.rs), runs to EOF
//! ```
//!
//! `emu_version_len`/`timestamp` widths are this crate's own choice (the
//! design doc specifies the field set and order, not their encodings);
//! everything else is SAVE_STATES.md §2 verbatim.
//!
//! # `timestamp` and `emu_version` are metadata only
//!
//! FR-STATE-002 requires save → load → run N frames to be hash-identical
//! to uninterrupted execution, and NFR-001 makes determinism a release
//! gate. If a wall-clock value ever leaked into a hash used for that
//! comparison, roundtrip determinism would break non-reproducibly — the
//! worst bug class in this project. [`crate::Container::state_hash`]
//! therefore hashes chunk payloads only and never reads `self.header` at
//! all. `timestamp` is caller-injected (never `SystemTime::now()` inside
//! this crate) so golden fixtures stay byte-reproducible.

use crate::cursor::Cursor;
use crate::error::ContainerError;

pub(crate) const MAGIC: [u8; 4] = *b"RFST";
pub(crate) const CONTAINER_VERSION: u16 = 1;
pub(crate) const MAX_EMU_VERSION_LEN: usize = 128;

/// The parsed `.rfstate` header. See the module doc comment for the exact
/// wire layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Container format version this file was written with. Always
    /// [`CONTAINER_VERSION`] after a successful [`Header::read`] — older
    /// or newer values are rejected before any other field is trusted.
    pub container_version: u16,
    /// Console byte (0 = NES, 1 = SNES by convention; not validated here).
    pub console: u8,
    /// Reserved for future use; must be 0 in v1.
    pub flags: u16,
    /// Normalized ROM SHA-256 this state was captured against.
    pub rom_sha256: [u8; 32],
    /// Emulator semver string that wrote this state. Metadata only.
    pub emu_version: String,
    /// Unix-seconds capture time. Metadata only — never hashed.
    pub timestamp: u64,
}

impl Header {
    pub(crate) fn write(&self, out: &mut Vec<u8>) -> Result<(), ContainerError> {
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&CONTAINER_VERSION.to_le_bytes());
        out.push(self.console);
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.rom_sha256);

        let ev_bytes = self.emu_version.as_bytes();
        if ev_bytes.len() > MAX_EMU_VERSION_LEN {
            return Err(ContainerError::OversizedLength {
                context: "emu_version",
                declared: ev_bytes.len(),
                max: MAX_EMU_VERSION_LEN,
            });
        }
        let ev_len =
            u16::try_from(ev_bytes.len()).map_err(|_| ContainerError::OversizedLength {
                context: "emu_version",
                declared: ev_bytes.len(),
                max: MAX_EMU_VERSION_LEN,
            })?;
        out.extend_from_slice(&ev_len.to_le_bytes());
        out.extend_from_slice(ev_bytes);

        out.extend_from_slice(&self.timestamp.to_le_bytes());
        Ok(())
    }

    pub(crate) fn read(cur: &mut Cursor<'_>) -> Result<Header, ContainerError> {
        let magic = cur.take_array4("magic")?;
        if magic != MAGIC {
            return Err(ContainerError::BadMagic { found: magic });
        }

        let container_version = cur.take_u16_le("container_version")?;
        if container_version != CONTAINER_VERSION {
            return Err(ContainerError::UnsupportedContainerVersion {
                found: container_version,
                supported: CONTAINER_VERSION,
            });
        }

        let console = cur.take_u8("console")?;

        let flags = cur.take_u16_le("flags")?;
        if flags != 0 {
            return Err(ContainerError::NonZeroFlags { found: flags });
        }

        let rom_sha256 = cur.take_array32("rom_sha256")?;

        let ev_len = cur.take_u16_le("emu_version_len")? as usize;
        if ev_len > MAX_EMU_VERSION_LEN {
            return Err(ContainerError::OversizedLength {
                context: "emu_version",
                declared: ev_len,
                max: MAX_EMU_VERSION_LEN,
            });
        }
        let ev_bytes = cur.take(ev_len, "emu_version")?;
        let emu_version =
            String::from_utf8(ev_bytes.to_vec()).map_err(|_| ContainerError::InvalidUtf8 {
                context: "emu_version",
            })?;

        let timestamp = cur.take_u64_le("timestamp")?;

        Ok(Header {
            container_version,
            console,
            flags,
            rom_sha256,
            emu_version,
            timestamp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Header {
        Header {
            container_version: CONTAINER_VERSION,
            console: 0,
            flags: 0,
            rom_sha256: [0xAB; 32],
            emu_version: "0.1.0".to_string(),
            timestamp: 1_700_000_000,
        }
    }

    #[test]
    fn round_trips() {
        let h = sample();
        let mut bytes = Vec::new();
        h.write(&mut bytes).unwrap();
        let mut cur = Cursor::new(&bytes);
        let back = Header::read(&mut cur).unwrap();
        assert_eq!(h, back);
        assert!(cur.into_rest().is_empty());
    }

    #[test]
    fn rejects_bad_magic() {
        let h = sample();
        let mut bytes = Vec::new();
        h.write(&mut bytes).unwrap();
        bytes[0] = b'X';
        let mut cur = Cursor::new(&bytes);
        assert_eq!(
            Header::read(&mut cur).unwrap_err(),
            ContainerError::BadMagic { found: *b"XFST" }
        );
    }

    #[test]
    fn rejects_newer_container_version() {
        let h = sample();
        let mut bytes = Vec::new();
        h.write(&mut bytes).unwrap();
        bytes[4..6].copy_from_slice(&(CONTAINER_VERSION + 1).to_le_bytes());
        let mut cur = Cursor::new(&bytes);
        assert_eq!(
            Header::read(&mut cur).unwrap_err(),
            ContainerError::UnsupportedContainerVersion {
                found: CONTAINER_VERSION + 1,
                supported: CONTAINER_VERSION
            }
        );
    }

    #[test]
    fn rejects_nonzero_flags() {
        let h = sample();
        let mut bytes = Vec::new();
        h.write(&mut bytes).unwrap();
        bytes[7..9].copy_from_slice(&1u16.to_le_bytes());
        let mut cur = Cursor::new(&bytes);
        assert_eq!(
            Header::read(&mut cur).unwrap_err(),
            ContainerError::NonZeroFlags { found: 1 }
        );
    }

    #[test]
    fn rejects_oversized_emu_version_declared_length() {
        let h = sample();
        let mut bytes = Vec::new();
        h.write(&mut bytes).unwrap();
        // Overwrite emu_version_len (offset 41) with something past the cap.
        bytes[41..43].copy_from_slice(&(MAX_EMU_VERSION_LEN as u16 + 1).to_le_bytes());
        let mut cur = Cursor::new(&bytes);
        let err = Header::read(&mut cur).unwrap_err();
        assert!(matches!(err, ContainerError::OversizedLength { .. }));
    }

    #[test]
    fn every_truncation_prefix_errors_without_panicking() {
        let h = sample();
        let mut bytes = Vec::new();
        h.write(&mut bytes).unwrap();
        for len in 0..bytes.len() {
            let mut cur = Cursor::new(&bytes[..len]);
            assert!(
                Header::read(&mut cur).is_err(),
                "prefix len {len} should error"
            );
        }
    }
}
