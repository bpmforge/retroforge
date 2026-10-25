//! Byte edits at PRG-ROM offsets of an iNES image (ticket W36-06): the
//! apply half of a recipe patch row. The recipe resolves rows to PRG offsets
//! (`rf_rebuild::resolve_patches`); this puts the new bytes in a **copy** of
//! the image. The caller's buffer is never written (law 5, GAME_PATCHING
//! "never write to the user's ROM"), and an edit that falls outside the PRG
//! ROM is refused rather than clipped.
//!
//! iNES layout (nesdev "INES"): a 16-byte header, a 512-byte trainer when
//! flags 6 bit 2 is set, then `header[4]` 16 KiB PRG-ROM units. NES 2.0
//! images whose PRG size needs the extended size nibble are refused (no
//! SMB-style cart needs it; guessing a size would put bytes in the wrong
//! place).

use std::fmt;

const HEADER: usize = 16;
const TRAINER: usize = 512;
const PRG_UNIT: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrgPatchError {
    /// Not an iNES image, or shorter than its header says.
    NotInes,
    /// A NES 2.0 image with an extended PRG size.
    ExtendedSize,
    /// An edit past the end of the PRG-ROM.
    OutOfPrg { offset: u32, prg_len: usize },
}

impl fmt::Display for PrgPatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInes => write!(f, "not an iNES image"),
            Self::ExtendedSize => write!(f, "NES 2.0 extended PRG size is not supported"),
            Self::OutOfPrg { offset, prg_len } => {
                write!(
                    f,
                    "edit at PRG offset {offset:#x} is past the {prg_len}-byte PRG-ROM"
                )
            }
        }
    }
}

impl std::error::Error for PrgPatchError {}

/// Where the PRG-ROM sits in the file: `(start, len)`.
///
/// # Errors
/// [`PrgPatchError`] for a non-iNES or truncated image.
pub fn prg_span(ines: &[u8]) -> Result<(usize, usize), PrgPatchError> {
    if ines.len() < HEADER || ines[..4] != *b"NES\x1a" {
        return Err(PrgPatchError::NotInes);
    }
    if ines[7] & 0x0C == 0x08 && ines[9] & 0x0F != 0 {
        return Err(PrgPatchError::ExtendedSize);
    }
    let start = HEADER + if ines[6] & 0x04 != 0 { TRAINER } else { 0 };
    let len = usize::from(ines[4]) * PRG_UNIT;
    if start + len > ines.len() {
        return Err(PrgPatchError::NotInes);
    }
    Ok((start, len))
}

/// A copy of `ines` with each `(prg_offset, value)` written.
///
/// # Errors
/// [`PrgPatchError`] if the image is not iNES or an offset is outside the
/// PRG-ROM; nothing is returned in that case.
pub fn apply_prg_bytes(ines: &[u8], edits: &[(u32, u8)]) -> Result<Vec<u8>, PrgPatchError> {
    let (start, len) = prg_span(ines)?;
    let mut out = ines.to_vec();
    for &(offset, value) in edits {
        let o = offset as usize;
        if o >= len {
            return Err(PrgPatchError::OutOfPrg {
                offset,
                prg_len: len,
            });
        }
        out[start + o] = value;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(trainer: bool) -> Vec<u8> {
        let mut v = vec![0u8; HEADER];
        v[..4].copy_from_slice(b"NES\x1a");
        v[4] = 1;
        v[5] = 1;
        if trainer {
            v[6] |= 0x04;
            v.extend(std::iter::repeat_n(0xEE, TRAINER));
        }
        v.extend((0..PRG_UNIT).map(|i| (i % 251) as u8));
        v.extend(std::iter::repeat_n(0xCC, 8192));
        v
    }

    #[test]
    fn edits_land_in_a_copy_at_the_prg_offset() {
        for trainer in [false, true] {
            let img = image(trainer);
            let before = img.clone();
            let out = apply_prg_bytes(&img, &[(0x10, 0xAB), (0x3FFF, 0xCD)]).unwrap();
            let (start, _) = prg_span(&img).unwrap();
            assert_eq!(out[start + 0x10], 0xAB);
            assert_eq!(out[start + 0x3FFF], 0xCD);
            assert_eq!(img, before, "the caller's buffer is never mutated");
            let changed = out.iter().zip(&img).filter(|(a, b)| a != b).count();
            assert!(changed <= 2);
            assert_eq!(out.len(), img.len());
        }
    }

    #[test]
    fn bad_images_and_offsets_are_refused() {
        let img = image(false);
        assert_eq!(
            apply_prg_bytes(&img, &[(0x4000, 1)]),
            Err(PrgPatchError::OutOfPrg {
                offset: 0x4000,
                prg_len: PRG_UNIT
            })
        );
        assert_eq!(
            apply_prg_bytes(&[1, 2, 3], &[]),
            Err(PrgPatchError::NotInes)
        );
        let mut short = image(false);
        short.truncate(100);
        assert_eq!(prg_span(&short), Err(PrgPatchError::NotInes));
        let mut nes2 = image(false);
        nes2[7] = 0x08;
        nes2[9] = 0x01;
        assert_eq!(prg_span(&nes2), Err(PrgPatchError::ExtendedSize));
    }
}
