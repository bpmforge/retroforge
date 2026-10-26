//! Snes9x 1.43 `.smv` (binary, version 1), SNES.
//!
//! Little-endian header, as decoded from a real file and cross-checked
//! against the sizes (data length = 2 bytes x enabled pads x frames):
//!
//! | offset | size | field |
//! |---|---|---|
//! | 0x00 | 4 | `SMV\x1A` |
//! | 0x04 | 4 | version, 1 |
//! | 0x08 | 4 | movie id (recording time) |
//! | 0x0C | 4 | re-record count |
//! | 0x10 | 4 | frame count |
//! | 0x14 | 1 | pad mask, bit n = pad n+1 present |
//! | 0x15 | 1 | options: bit 0 start from power-on, bit 1 PAL |
//! | 0x16 | 1 | sync options (not used by a replay here) |
//! | 0x18 | 4 | offset of the embedded savestate/SRAM block |
//! | 0x1C | 4 | offset of the pad data |
//!
//! The header is **not** a fixed length (an author block and ROM info follow
//! the fixed part), so the pad data is found through the offset at `0x1C`,
//! never by assuming where the header ends. Each frame holds one 16-bit word
//! per present pad, and that word is already the `$4218/$4219` layout (B at
//! bit 15, R at bit 4; bits 0-3 are always zero), so no translation is done.
//!
//! The file may hold one more record than the header's frame count (the one
//! being recorded when the author stopped): extra trailing data is ignored,
//! missing data is [`MovieError::Truncated`].

use super::{malformed, Console, Format, Movie, MovieError, MovieFrame, MAX_FRAMES};

const MAGIC: &[u8; 4] = b"SMV\x1a";
const FIXED_HEADER: usize = 0x20;

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Parse a `.smv` file.
///
/// # Errors
/// [`MovieError::Unsupported`] for another version, a savestate start or
/// PAL; [`MovieError::Truncated`] when the pad data is shorter than the
/// header's frame count; [`MovieError::Malformed`] otherwise.
pub fn parse(bytes: &[u8]) -> Result<Movie, MovieError> {
    if bytes.len() < FIXED_HEADER {
        return Err(MovieError::Truncated);
    }
    if &bytes[..4] != MAGIC {
        return Err(malformed(0, "not an SMV file"));
    }
    let version = u32_at(bytes, 4).ok_or(MovieError::Truncated)?;
    if version != 1 {
        return Err(MovieError::Unsupported(format!("SMV version {version}")));
    }
    let rerecords = u32_at(bytes, 0x0C).ok_or(MovieError::Truncated)?;
    let frames = u32_at(bytes, 0x10).ok_or(MovieError::Truncated)? as usize;
    let mask = bytes[0x14] & 0x1F;
    let options = bytes[0x15];
    if options & 1 == 0 {
        return Err(MovieError::Unsupported("starts from a savestate".into()));
    }
    if options & 2 != 0 {
        return Err(MovieError::Unsupported("PAL".into()));
    }
    let data_at = u32_at(bytes, 0x1C).ok_or(MovieError::Truncated)? as usize;
    if frames == 0 {
        return Err(MovieError::Empty);
    }
    if frames > MAX_FRAMES {
        return Err(MovieError::TooLarge);
    }
    if mask == 0 {
        return Err(malformed(0x14, "no pad is present"));
    }
    if data_at < FIXED_HEADER {
        return Err(malformed(0x1C, "pad data starts inside the header"));
    }
    let data = bytes.get(data_at..).ok_or(MovieError::Truncated)?;

    // Pads present, in pad order: only pads 1 and 2 are replayed.
    let present: Vec<usize> = (0..5).filter(|i| mask & (1 << i) != 0).collect();
    let stride = present.len() * 2;
    // Bounded by the bytes that exist, not by the header's claim.
    if data.len() / stride < frames {
        return Err(MovieError::Truncated);
    }
    let out: Vec<MovieFrame> = data
        .chunks_exact(stride)
        .take(frames)
        .map(|rec| {
            let mut f = MovieFrame::default();
            for (k, &pad) in present.iter().enumerate() {
                if let Some(slot) = f.pads.get_mut(pad) {
                    *slot = u16::from_le_bytes([rec[2 * k], rec[2 * k + 1]]);
                }
            }
            f
        })
        .collect();
    Ok(Movie {
        format: Format::Smv,
        console: Console::Snes,
        frames: out,
        rerecords: Some(u64::from(rerecords)),
        rom_hint: None,
        dropped_subframes: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny valid file: pad 1 only, `words` as the frames, `extra`
    /// trailing bytes, 8 bytes of author block between header and data.
    fn sample(words: &[u16], frames: u32, extra: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; FIXED_HEADER + 8];
        b[..4].copy_from_slice(MAGIC);
        b[4] = 1;
        b[0x0C] = 9; // re-records
        b[0x10..0x14].copy_from_slice(&frames.to_le_bytes());
        b[0x14] = 0b1;
        b[0x15] = 0b1;
        b[0x1C..0x20].copy_from_slice(&((FIXED_HEADER + 8) as u32).to_le_bytes());
        for w in words {
            b.extend(w.to_le_bytes());
        }
        b.extend(extra);
        b
    }

    #[test]
    fn words_are_the_hardware_layout_untouched() {
        let m = parse(&sample(&[0, 0x8000, 0x4100, 0x0090], 4, &[])).expect("parses");
        assert_eq!(m.frames.len(), 4);
        assert_eq!(m.rerecords, Some(9));
        assert_eq!(m.frames[1].pads[0], 0x8000);
        assert_eq!(m.frames[2].pads[0], 0x4100);
        assert_eq!(m.frames[3].pads[0], 0x0090);
        // B is bit 15 and Right bit 8 in rf-input's table: the same word.
        assert_eq!(rf_input::SnesButton::B.bit(), 15);
        assert_eq!(rf_input::SnesButton::Right.bit(), 8);
    }

    #[test]
    fn a_trailing_record_is_tolerated_and_two_pads_are_read() {
        let m = parse(&sample(&[1 << 15, 2 << 8], 1, &[0xAA, 0xBB])).expect("parses");
        assert_eq!(m.frames.len(), 1);
        // Two pads present: mask 0b11, 4 bytes per frame.
        let mut b = sample(&[0x8000, 0x0100], 1, &[]);
        b[0x14] = 0b11;
        let m = parse(&b).expect("parses");
        assert_eq!(m.frames[0].pads, [0x8000, 0x0100]);
    }

    #[test]
    fn refuses_what_it_cannot_replay() {
        let mut b = sample(&[0], 1, &[]);
        b[0x15] = 0;
        assert!(matches!(parse(&b), Err(MovieError::Unsupported(_))));
        b[0x15] = 3;
        assert!(matches!(parse(&b), Err(MovieError::Unsupported(_))));
        let mut b = sample(&[0], 1, &[]);
        b[4] = 4;
        assert!(matches!(parse(&b), Err(MovieError::Unsupported(_))));
        let mut b = sample(&[0], 1, &[]);
        b[0x14] = 0;
        assert!(parse(&b).is_err());
        let mut b = sample(&[0], 1, &[]);
        b[0] = b'X';
        assert!(parse(&b).is_err());
    }

    #[test]
    fn a_header_that_lies_about_its_length_is_an_error_not_an_allocation() {
        // Claims u32::MAX frames over 2 bytes of data.
        assert!(parse(&sample(&[0], u32::MAX, &[])).is_err());
        // Claims 3 frames, has 2.
        assert_eq!(parse(&sample(&[0, 0], 3, &[])), Err(MovieError::Truncated));
        // Data offset past the end, and inside the header.
        let mut b = sample(&[0], 1, &[]);
        b[0x1C..0x20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse(&b), Err(MovieError::Truncated));
        b[0x1C..0x20].copy_from_slice(&4u32.to_le_bytes());
        assert!(parse(&b).is_err());
        assert_eq!(parse(&sample(&[], 0, &[])), Err(MovieError::Empty));
    }

    #[test]
    fn truncation_at_every_byte_and_garbage_never_panic() {
        let good = sample(&[0, 0x8000, 0x4100], 3, &[1]);
        for cut in 0..good.len() {
            let r = parse(&good[..cut]);
            if cut < good.len() - 1 {
                assert!(r.is_err(), "cut {cut}");
            }
        }
        for seed in 0u32..200 {
            let junk: Vec<u8> = (0..64)
                .map(|i| (seed.wrapping_mul(2_654_435_761).wrapping_add(i * 97) >> 8) as u8)
                .collect();
            let _ = parse(&junk);
            let mut b = good.clone();
            for (i, v) in junk.iter().enumerate().take(0x20) {
                b[i % 0x20] ^= *v;
            }
            let _ = parse(&b);
        }
    }
}
