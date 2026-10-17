//! Soft-patching (ticket W32-01, `docs/design/GAME_PATCHING.md` level 1):
//! apply IPS / BPS / UPS patch files to a ROM image **in memory**. The ROM
//! file on disk is never written (law 5: no ROM bytes anywhere; the user
//! supplies the patch), and a patched session is never "the cartridge"
//! (law 6) -- callers must report it as such.
//!
//! Clean-room: only the published format descriptions were used (BPS:
//! Near's "BPS File Format Specification", romhacking.net documents/746;
//! IPS and UPS: the published format descriptions). No patcher or emulator
//! source was read.
//!
//! ## Every walk proves progress (law 8)
//!
//! Patch files are untrusted input. Every loop here either iterates a
//! range whose length was validated against the declared output size
//! (itself capped at [`MAX_OUTPUT`]) or consumes at least one input byte
//! per iteration through [`Reader`], whose reads fail rather than stall at
//! the end of input. Nothing loops on a value read from the patch.
use std::fmt;

pub mod bps;
pub mod bso;
pub mod discover;
pub mod ips;
pub mod ups;

/// Largest image any patch may produce (also the cap used for zips in
/// `rom_open`). Real SNES images are at most ~12 MiB.
pub const MAX_OUTPUT: usize = 64 * 1024 * 1024;

/// The SNES copier header size (ROM files with `len % 1024 == 512`).
pub const COPIER_HEADER: usize = 512;

/// Why a patch could not be applied. Never a panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchError {
    /// The patch file does not start with the format's magic.
    BadMagic,
    /// The patch ended in the middle of a field or record.
    Truncated,
    /// A variable-length integer was too large for 64 bits.
    BadVarint,
    /// A size, offset or run reaches past the declared (or capped) output.
    OutOfRange(&'static str),
    /// The patch was written for a different ROM (size or CRC32 mismatch).
    WrongSource(String),
    /// The patch file itself is damaged (its own CRC32 is wrong).
    CorruptPatch,
    /// The produced image does not match the patch's target CRC32.
    BadTarget,
    /// Bytes left over after the patch's last action.
    TrailingData,
    /// Not a patch format this build knows.
    UnknownFormat,
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => write!(f, "not a valid patch file (bad header)"),
            Self::Truncated => write!(f, "the patch file is cut short"),
            Self::BadVarint => write!(f, "the patch file has an oversized number"),
            Self::OutOfRange(what) => write!(f, "the patch reaches outside the image ({what})"),
            Self::WrongSource(why) => write!(f, "this patch is for a different ROM ({why})"),
            Self::CorruptPatch => write!(f, "the patch file is damaged (checksum mismatch)"),
            Self::BadTarget => write!(f, "the patched image failed its checksum"),
            Self::TrailingData => write!(f, "unexpected extra data at the end of the patch"),
            Self::UnknownFormat => write!(f, "not a recognised patch format"),
        }
    }
}

impl std::error::Error for PatchError {}

/// A patch format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PatchKind {
    Bps,
    Ips,
    Ups,
}

impl PatchKind {
    /// Lower-case extension (without any stack number).
    #[must_use]
    pub const fn ext(self) -> &'static str {
        match self {
            Self::Bps => "bps",
            Self::Ips => "ips",
            Self::Ups => "ups",
        }
    }

    /// Sniff by magic bytes.
    #[must_use]
    pub fn sniff(patch: &[u8]) -> Option<Self> {
        if patch.starts_with(b"BPS1") {
            Some(Self::Bps)
        } else if patch.starts_with(b"PATCH") {
            Some(Self::Ips)
        } else if patch.starts_with(b"UPS1") {
            Some(Self::Ups)
        } else {
            None
        }
    }

    /// Apply a patch of this kind.
    ///
    /// # Errors
    /// See [`PatchError`].
    pub fn apply(self, source: &[u8], patch: &[u8]) -> Result<Vec<u8>, PatchError> {
        match self {
            Self::Bps => bps::apply_bps(source, patch),
            Self::Ips => ips::apply_ips(source, patch),
            Self::Ups => ups::apply_ups(source, patch),
        }
    }
}

/// Which form of the ROM a patch matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderUsed {
    /// The ROM has no copier header; patched as is.
    NoHeader,
    /// The ROM has a 512-byte copier header and the patch fit the
    /// headerless image (output is headerless).
    Headerless,
    /// The ROM has a header and the patch fit the headered image.
    Headered,
}

impl HeaderUsed {
    /// Plain-words note for the status line.
    #[must_use]
    pub const fn note(self) -> &'static str {
        match self {
            Self::NoHeader => "",
            Self::Headerless => "applied to the image without its copier header",
            Self::Headered => "applied to the image including its copier header",
        }
    }
}

/// Does `rom` look like it carries a 512-byte SNES copier header?
#[must_use]
pub const fn has_copier_header(rom: &[u8]) -> bool {
    rom.len() % 1024 == COPIER_HEADER
}

/// Apply `patch` (format sniffed from its magic) to `rom`, trying the
/// headerless image first when the ROM has a copier header, then the
/// headered one. Reports which worked.
///
/// # Errors
/// The error of the first attempt when every attempt fails (for a headered
/// ROM both are tried; the message names the headerless failure first).
pub fn apply_patch(rom: &[u8], patch: &[u8]) -> Result<(Vec<u8>, HeaderUsed), PatchError> {
    let kind = PatchKind::sniff(patch).ok_or(PatchError::UnknownFormat)?;
    if has_copier_header(rom) {
        match kind.apply(&rom[COPIER_HEADER..], patch) {
            Ok(out) => Ok((out, HeaderUsed::Headerless)),
            Err(first) => match kind.apply(rom, patch) {
                Ok(out) => Ok((out, HeaderUsed::Headered)),
                Err(_) => Err(first),
            },
        }
    } else {
        kind.apply(rom, patch)
            .map(|out| (out, HeaderUsed::NoHeader))
    }
}

/// Bounds-checked sequential reader over a patch. Every read either
/// advances `pos` or fails, so a loop that reads at least once per
/// iteration terminates (law 8).
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pub(crate) pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub(crate) fn byte(&mut self) -> Result<u8, PatchError> {
        let b = *self.data.get(self.pos).ok_or(PatchError::Truncated)?;
        self.pos += 1;
        Ok(b)
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], PatchError> {
        let end = self.pos.checked_add(n).ok_or(PatchError::Truncated)?;
        let s = self.data.get(self.pos..end).ok_or(PatchError::Truncated)?;
        self.pos = end;
        Ok(s)
    }

    /// The BPS/UPS variable-length number: 7 data bits per byte, high bit
    /// marks the last byte, and each continuation adds one (so every value
    /// has exactly one encoding). Consumes a byte per iteration.
    pub(crate) fn varint(&mut self) -> Result<u64, PatchError> {
        let mut data: u64 = 0;
        let mut shift: u64 = 1;
        loop {
            let x = self.byte()?;
            let part = u64::from(x & 0x7f)
                .checked_mul(shift)
                .ok_or(PatchError::BadVarint)?;
            data = data.checked_add(part).ok_or(PatchError::BadVarint)?;
            if x & 0x80 != 0 {
                return Ok(data);
            }
            shift = shift.checked_mul(128).ok_or(PatchError::BadVarint)?;
            data = data.checked_add(shift).ok_or(PatchError::BadVarint)?;
        }
    }
}

pub(crate) fn read_le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

pub(crate) fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

/// Declared output size -> `usize`, refusing anything over [`MAX_OUTPUT`].
pub(crate) fn capped_size(n: u64) -> Result<usize, PatchError> {
    usize::try_from(n)
        .ok()
        .filter(|&n| n <= MAX_OUTPUT)
        .ok_or(PatchError::OutOfRange("declared size"))
}

#[cfg(test)]
pub(crate) mod testutil {
    /// Deterministic pseudo-random bytes (xorshift), never ROM bytes.
    pub fn noise(seed: u64, len: usize) -> Vec<u8> {
        let mut s = seed | 1;
        (0..len)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 24) as u8
            })
            .collect()
    }

    /// BPS/UPS varint encoder.
    pub fn enc(mut n: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let x = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                out.push(0x80 | x);
                return out;
            }
            out.push(x);
            n -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testutil::noise;

    #[test]
    fn dispatch_and_header_handling() {
        let (src, p, tgt) = bps::tests::sample();
        assert_eq!(PatchKind::sniff(&p), Some(PatchKind::Bps));
        // Headerless ROM: straight through.
        assert_eq!(
            apply_patch(&src, &p).unwrap(),
            (tgt.clone(), HeaderUsed::NoHeader)
        );
        // The same ROM with a 512-byte copier header: headerless tried first.
        let mut headered = noise(99, COPIER_HEADER);
        headered.extend_from_slice(&src);
        assert!(has_copier_header(&headered));
        let (out, used) = apply_patch(&headered, &p).unwrap();
        assert_eq!((out, used), (tgt, HeaderUsed::Headerless));
        assert!(!HeaderUsed::Headerless.note().is_empty());
    }

    #[test]
    fn patch_made_for_the_headered_dump_falls_back() {
        // Source = header + body, size % 1024 == 512; the patch's source
        // is that whole file, so the headerless attempt fails on CRC.
        let mut src = noise(41, COPIER_HEADER);
        src.extend(noise(42, 2048));
        let mut tgt = src.clone();
        tgt[700] ^= 0xFF;
        let mut p = b"BPS1".to_vec();
        p.extend(testutil::enc(src.len() as u64));
        p.extend(testutil::enc(src.len() as u64));
        p.extend(testutil::enc(0));
        // SourceRead 700, TargetRead 1, SourceRead rest.
        p.extend(testutil::enc((700 - 1) << 2));
        p.extend(testutil::enc(1));
        p.push(tgt[700]);
        let rest = src.len() - 701;
        p.extend(testutil::enc(((rest as u64) - 1) << 2));
        let p = bps::tests::finish(p, &src, &tgt);
        let (out, used) = apply_patch(&src, &p).unwrap();
        assert_eq!(used, HeaderUsed::Headered);
        assert_eq!(out, tgt);
        // A genuinely wrong ROM still errors, with the WrongSource message.
        let other = noise(43, src.len());
        assert!(matches!(
            apply_patch(&other, &p),
            Err(PatchError::WrongSource(_))
        ));
        assert_eq!(apply_patch(&src, b"nope"), Err(PatchError::UnknownFormat));
    }

    #[test]
    fn varint_round_trips_and_limits() {
        for n in [
            0u64,
            1,
            127,
            128,
            129,
            16511,
            1 << 20,
            1 << 40,
            u64::MAX / 4,
        ] {
            let e = testutil::enc(n);
            assert_eq!(Reader::new(&e, 0).varint(), Ok(n), "{n}");
        }
        assert_eq!(
            Reader::new(&[0x7f; 20], 0).varint(),
            Err(PatchError::BadVarint)
        );
        assert_eq!(Reader::new(&[0x01], 0).varint(), Err(PatchError::Truncated));
    }
}
