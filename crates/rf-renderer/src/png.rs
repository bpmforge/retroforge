//! Minimal RGBA8 PNG encoder (ticket W3-04; `docs/design/RENDERER.md` §6
//! "Screenshot: … PNG", FR-FE-005).
//!
//! ## Why this exists instead of a dependency
//!
//! This workspace has no image codec, and `docs/TECH_STACK.md` has no row
//! for one. Adding `png`/`image` would be a TECH_STACK decision with a
//! licence review attached (NFR-011), which is a different ticket's work —
//! so rather than quietly widen the dependency graph to write a
//! screenshot, this encodes PNG directly.
//!
//! ## How it can be dependency-free: stored deflate blocks
//!
//! PNG's IDAT payload is a zlib stream, and zlib permits **stored**
//! (uncompressed) deflate blocks — `BTYPE = 00`, a length, its
//! complement, then the literal bytes (RFC 1951 §3.2.4). So a valid PNG
//! needs no compressor at all: only CRC-32 for the chunk framing (RFC
//! 1952 / PNG §5.5) and Adler-32 for the zlib trailer (RFC 1950 §2.2),
//! both a few lines each and implemented here from their specifications.
//!
//! **The honest trade:** these files are larger than a compressed PNG —
//! roughly the raw RGBA size, so a 256×240 frame is ~245 KB rather than
//! the ~10-40 KB a real encoder would manage. That is a fine price for a
//! screenshot a user takes deliberately, and a bad one for anything
//! per-frame, which is why `RENDERER.md` §6's Phase-8 video capture is
//! explicitly *not* built on this. If screenshots ever need to be small,
//! that is the moment to add a real codec **and** its TECH_STACK row.
//!
//! Every file this writes is a conformant PNG: the stored-block path is
//! part of the format, not a trick that happens to load.

/// CRC-32 (IEEE), as PNG §5.5 specifies for chunk checksums.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            // 0xEDB8_8320 is the reversed IEEE polynomial.
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Adler-32, as RFC 1950 §2.2 specifies for the zlib trailer.
fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in bytes {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    #[allow(clippy::cast_possible_truncation)]
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    let mut crc_input = Vec::with_capacity(4 + payload.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(payload);
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// Encode `rgba` (`width`x`height`, 8-bit RGBA) as a PNG byte stream.
///
/// # Panics
/// Panics if `rgba` is not exactly `width * height * 4` bytes, or if
/// either dimension is zero — PNG forbids a zero dimension, and a caller
/// asking for one has a bug rather than a degenerate frame.
#[must_use]
pub fn encode_rgba(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    assert!(width > 0 && height > 0, "png::encode_rgba: zero dimension");
    assert_eq!(
        rgba.len(),
        (width as usize) * (height as usize) * 4,
        "png::encode_rgba: buffer is not {width}x{height} RGBA"
    );

    // Raw scanlines, each prefixed with filter type 0 (None). Filtering
    // exists to help compression; with stored blocks there is nothing to
    // help, so None is both correct and the honest choice.
    let stride = (width as usize) * 4;
    let mut raw = Vec::with_capacity((height as usize) * (stride + 1));
    for y in 0..height as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * stride..(y + 1) * stride]);
    }

    // zlib stream: header, stored deflate blocks, Adler-32 of `raw`.
    let mut z = vec![0x78, 0x01]; // CM=8/CINFO=7, FCHECK making it %31==0
    let mut offset = 0usize;
    if raw.is_empty() {
        z.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
    }
    while offset < raw.len() {
        // A stored block's LEN field is 16 bits, so 65535 bytes max.
        let take = (raw.len() - offset).min(0xFFFF);
        let final_block = offset + take == raw.len();
        z.push(u8::from(final_block));
        #[allow(clippy::cast_possible_truncation)]
        let len = take as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(&raw[offset..offset + take]);
        offset += take;
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = Vec::with_capacity(z.len() + 128);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[
        8, // bit depth
        6, // colour type 6 = truecolour with alpha
        0, // compression method 0 = deflate
        0, // filter method 0
        0, // interlace method 0 = none
    ]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known-answer checks against the published test vectors, so the two
    /// checksums are verified against their specifications rather than
    /// against themselves.
    #[test]
    fn checksums_match_their_published_test_vectors() {
        // CRC-32 of "123456789" is 0xCBF43926 (the standard check value
        // quoted for CRC-32/ISO-HDLC).
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        // Adler-32 of "Wikipedia" is 0x11E60398 (RFC 1950's worked
        // example, as quoted in the algorithm's standard description).
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn produces_a_well_formed_png_header_and_trailer() {
        let png = encode_rgba(&[1, 2, 3, 4], 1, 1);
        assert_eq!(
            &png[..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
            "PNG signature"
        );
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..20], &1u32.to_be_bytes(), "width");
        assert_eq!(&png[20..24], &1u32.to_be_bytes(), "height");
        assert_eq!(png[24], 8, "bit depth");
        assert_eq!(png[25], 6, "colour type RGBA");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }

    /// Every chunk's declared length and CRC must check out when the file
    /// is walked the way a decoder walks it. This is the test that would
    /// catch a framing bug, which is the failure mode a hand-written
    /// encoder actually has.
    #[test]
    fn every_chunk_length_and_crc_validates_on_a_second_pass() {
        let (w, h) = (37u32, 11u32); // deliberately not a round size
        let rgba: Vec<u8> = (0..(w * h * 4)).map(|i| (i % 251) as u8).collect();
        let png = encode_rgba(&rgba, w, h);

        let mut pos = 8;
        let mut seen: Vec<String> = Vec::new();
        while pos < png.len() {
            let len = u32::from_be_bytes(png[pos..pos + 4].try_into().unwrap()) as usize;
            let kind = &png[pos + 4..pos + 8];
            let payload = &png[pos + 8..pos + 8 + len];
            let stored = u32::from_be_bytes(png[pos + 8 + len..pos + 12 + len].try_into().unwrap());
            let mut crc_input = Vec::from(kind);
            crc_input.extend_from_slice(payload);
            assert_eq!(
                crc32(&crc_input),
                stored,
                "chunk {} has a bad CRC",
                String::from_utf8_lossy(kind)
            );
            seen.push(String::from_utf8_lossy(kind).into_owned());
            pos += 12 + len;
        }
        assert_eq!(pos, png.len(), "chunks must exactly cover the file");
        assert_eq!(seen, vec!["IHDR", "IDAT", "IEND"]);
    }

    /// The zlib stream must be decodable by the stored-block rules, and
    /// must reproduce the exact scanlines that went in — including the
    /// per-row filter byte. Decoding it back here is what makes this an
    /// encoder test rather than a "it produced some bytes" test.
    #[test]
    fn the_zlib_stream_round_trips_back_to_the_original_scanlines() {
        // Big enough to need MORE THAN ONE stored block (>65535 bytes of
        // raw scanline data), which is the boundary a single-block
        // implementation silently gets wrong.
        let (w, h) = (64u32, 300u32);
        assert!((w as usize) * 4 * (h as usize) > 0xFFFF, "must span blocks");
        let rgba: Vec<u8> = (0..(w * h * 4)).map(|i| (i % 253) as u8).collect();
        let png = encode_rgba(&rgba, w, h);

        // Pull the IDAT payload back out.
        let mut pos = 8;
        let mut idat = Vec::new();
        while pos < png.len() {
            let len = u32::from_be_bytes(png[pos..pos + 4].try_into().unwrap()) as usize;
            if &png[pos + 4..pos + 8] == b"IDAT" {
                idat.extend_from_slice(&png[pos + 8..pos + 8 + len]);
            }
            pos += 12 + len;
        }

        // Walk the stored blocks.
        let mut raw = Vec::new();
        let mut p = 2; // skip the 2-byte zlib header
        loop {
            let header = idat[p];
            let final_block = header & 1 == 1;
            assert_eq!(header >> 1, 0, "must be a STORED block (BTYPE 00)");
            let len = u16::from_le_bytes(idat[p + 1..p + 3].try_into().unwrap());
            let nlen = u16::from_le_bytes(idat[p + 3..p + 5].try_into().unwrap());
            assert_eq!(nlen, !len, "NLEN must be LEN's complement");
            raw.extend_from_slice(&idat[p + 5..p + 5 + len as usize]);
            p += 5 + len as usize;
            if final_block {
                break;
            }
        }
        assert_eq!(
            adler32(&raw),
            u32::from_be_bytes(idat[p..p + 4].try_into().unwrap()),
            "zlib trailer must be Adler-32 of the raw data"
        );

        // And the raw data must be filter-0 rows of the original pixels.
        let stride = (w as usize) * 4;
        assert_eq!(raw.len(), (h as usize) * (stride + 1));
        for y in 0..h as usize {
            let row = &raw[y * (stride + 1)..(y + 1) * (stride + 1)];
            assert_eq!(row[0], 0, "row {y} filter byte must be None");
            assert_eq!(&row[1..], &rgba[y * stride..(y + 1) * stride], "row {y}");
        }
    }
}
