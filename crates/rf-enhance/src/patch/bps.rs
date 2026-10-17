//! BPS (Near, romhacking.net documents/746): `"BPS1"`, varint source size,
//! target size and metadata size, the metadata, then actions until the
//! target is full, then CRC32s of the source, the target and the patch
//! itself (all three verified, so a wrong ROM is refused up front).
//!
//! An action is a varint whose low 2 bits pick SourceRead / TargetRead /
//! SourceCopy / TargetCopy and whose remaining bits are `length - 1`.
//! The copies take a second varint: a sign bit and a magnitude, relative
//! to that copy kind's own running offset.
use super::{capped_size, crc32, read_le32, PatchError, Reader};

fn signed(v: u64) -> i64 {
    let mag = (v >> 1) as i64;
    if v & 1 != 0 {
        -mag
    } else {
        mag
    }
}

/// Apply a BPS patch.
///
/// # Errors
/// [`PatchError`] for any malformed, truncated or mismatched input.
pub fn apply_bps(source: &[u8], patch: &[u8]) -> Result<Vec<u8>, PatchError> {
    if !patch.starts_with(b"BPS1") {
        return Err(PatchError::BadMagic);
    }
    if patch.len() < 4 + 3 + 12 {
        return Err(PatchError::Truncated);
    }
    let body_end = patch.len() - 12;
    let footer = &patch[body_end..];
    if crc32(&patch[..patch.len() - 4]) != read_le32(&footer[8..]) {
        return Err(PatchError::CorruptPatch);
    }
    let mut r = Reader::new(&patch[..body_end], 4);
    let source_size = r.varint()?;
    let target_size = capped_size(r.varint()?)?;
    let meta = usize::try_from(r.varint()?).map_err(|_| PatchError::Truncated)?;
    r.take(meta)?;
    let src_crc = crc32(source);
    if source.len() as u64 != source_size || src_crc != read_le32(footer) {
        return Err(PatchError::WrongSource(format!(
            "expected {source_size} bytes with CRC32 {:08x}, got {} bytes with CRC32 {src_crc:08x}",
            read_le32(footer),
            source.len()
        )));
    }
    let mut out: Vec<u8> = Vec::with_capacity(target_size);
    let (mut src_rel, mut tgt_rel): (i64, i64) = (0, 0);
    // Each iteration reads an action varint (>= 1 byte), and `out` grows by
    // >= 1 (length is at least 1), bounded by `target_size`.
    while out.len() < target_size {
        let action = r.varint()?;
        let len =
            usize::try_from((action >> 2) + 1).map_err(|_| PatchError::OutOfRange("length"))?;
        if len > target_size - out.len() {
            return Err(PatchError::OutOfRange("action past target size"));
        }
        match action & 3 {
            0 => {
                let at = out.len();
                let s = source
                    .get(at..at + len)
                    .ok_or(PatchError::OutOfRange("source read"))?;
                out.extend_from_slice(s);
            }
            1 => out.extend_from_slice(r.take(len)?),
            2 => {
                src_rel = src_rel
                    .checked_add(signed(r.varint()?))
                    .ok_or(PatchError::OutOfRange("source copy"))?;
                let start =
                    usize::try_from(src_rel).map_err(|_| PatchError::OutOfRange("source copy"))?;
                let s = source
                    .get(start..start.saturating_add(len))
                    .ok_or(PatchError::OutOfRange("source copy"))?;
                out.extend_from_slice(s);
                src_rel += len as i64;
            }
            _ => {
                tgt_rel = tgt_rel
                    .checked_add(signed(r.varint()?))
                    .ok_or(PatchError::OutOfRange("target copy"))?;
                let start =
                    usize::try_from(tgt_rel).map_err(|_| PatchError::OutOfRange("target copy"))?;
                // Overlap is the point (run-length): copy byte by byte, and
                // each byte read must already exist.
                for k in 0..len {
                    let b = *out
                        .get(start + k)
                        .ok_or(PatchError::OutOfRange("target copy"))?;
                    out.push(b);
                }
                tgt_rel += len as i64;
            }
        }
    }
    if r.remaining() != 0 {
        return Err(PatchError::TrailingData);
    }
    if crc32(&out) != read_le32(&footer[4..]) {
        return Err(PatchError::BadTarget);
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::patch::testutil::{enc, noise};

    pub fn finish(mut p: Vec<u8>, src: &[u8], tgt: &[u8]) -> Vec<u8> {
        p.extend_from_slice(&crc32(src).to_le_bytes());
        p.extend_from_slice(&crc32(tgt).to_le_bytes());
        let c = crc32(&p);
        p.extend_from_slice(&c.to_le_bytes());
        p
    }

    fn header(src: &[u8], tgt_len: usize) -> Vec<u8> {
        let mut p = b"BPS1".to_vec();
        p.extend(enc(src.len() as u64));
        p.extend(enc(tgt_len as u64));
        p.extend(enc(0));
        p
    }

    fn zig(d: i64) -> Vec<u8> {
        enc(((d.unsigned_abs()) << 1) | u64::from(d < 0))
    }

    /// A patch exercising all four actions; returns (source, patch, target).
    pub fn sample() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let src = noise(11, 1024);
        let mut tgt = Vec::new();
        let mut p;
        // SourceRead 16 bytes at position 0.
        tgt.extend_from_slice(&src[0..16]);
        // TargetRead "HELLO".
        tgt.extend_from_slice(b"HELLO");
        // SourceCopy 20 bytes from source offset 100.
        tgt.extend_from_slice(&src[100..120]);
        // TargetCopy 8 bytes from target offset 16 ("HELLO" + 3), then an
        // overlapping run: 6 bytes from offset len-1.
        let t0: Vec<u8> = tgt[16..24].to_vec();
        tgt.extend_from_slice(&t0);
        let last = tgt.len() - 1;
        for k in 0..6 {
            let b = tgt[last + k];
            tgt.push(b);
        }
        let mut body = Vec::new();
        body.extend(enc((16 - 1) << 2));
        body.extend(enc(((5 - 1) << 2) | 1));
        body.extend_from_slice(b"HELLO");
        body.extend(enc(((20 - 1) << 2) | 2));
        body.extend(zig(100));
        body.extend(enc(((8 - 1) << 2) | 3));
        body.extend(zig(16));
        body.extend(enc(((6 - 1) << 2) | 3));
        body.extend(zig(last as i64 - (16 + 8)));
        p = header(&src, tgt.len());
        p.extend(body);
        (src.clone(), finish(p, &src, &tgt), tgt)
    }

    #[test]
    fn all_four_actions_round_trip() {
        let (src, p, tgt) = sample();
        assert_eq!(apply_bps(&src, &p).unwrap(), tgt);
    }

    #[test]
    fn wrong_source_and_corrupt_patch_are_distinct_errors() {
        let (src, p, _) = sample();
        let other = noise(12, src.len());
        assert!(matches!(
            apply_bps(&other, &p),
            Err(PatchError::WrongSource(_))
        ));
        assert!(matches!(
            apply_bps(&src[..100], &p),
            Err(PatchError::WrongSource(_))
        ));
        let mut bad = p.clone();
        bad[20] ^= 1;
        assert_eq!(apply_bps(&src, &bad), Err(PatchError::CorruptPatch));
    }

    #[test]
    fn bad_target_crc_and_trailing_data() {
        let (src, p, tgt) = sample();
        let mut q = p[..p.len() - 12].to_vec();
        let mut wrong = tgt.clone();
        wrong[0] ^= 1;
        let q2 = finish(q.clone(), &src, &wrong);
        assert_eq!(apply_bps(&src, &q2), Err(PatchError::BadTarget));
        q.push(0x80);
        let q3 = finish(q, &src, &tgt);
        assert_eq!(apply_bps(&src, &q3), Err(PatchError::TrailingData));
    }

    #[test]
    fn out_of_range_actions_error() {
        let src = noise(13, 64);
        // Declared target 10 but a SourceRead of 1000.
        let mut p = header(&src, 10);
        p.extend(enc((1000 - 1) << 2));
        assert!(apply_bps(&src, &finish(p, &src, &[])).is_err());
        // TargetCopy from an offset that does not exist yet.
        let mut p = header(&src, 4);
        p.extend(enc((4 - 1) << 2 | 3));
        p.extend(zig(5));
        assert!(apply_bps(&src, &finish(p, &src, &[])).is_err());
        // Negative SourceCopy offset.
        let mut p = header(&src, 4);
        p.extend(enc((4 - 1) << 2 | 2));
        p.extend(zig(-3));
        assert!(apply_bps(&src, &finish(p, &src, &[])).is_err());
        // Declared size over the cap.
        let mut p = b"BPS1".to_vec();
        p.extend(enc(src.len() as u64));
        p.extend(enc(1 << 40));
        p.extend(enc(0));
        assert!(apply_bps(&src, &finish(p, &src, &[])).is_err());
        // Oversized varint.
        let mut p = b"BPS1".to_vec();
        p.extend_from_slice(&[0x7f; 12]);
        assert!(apply_bps(&src, &finish(p, &src, &[])).is_err());
    }

    #[test]
    fn every_truncation_point_is_handled() {
        let (src, p, _) = sample();
        for cut in 0..p.len() {
            assert!(apply_bps(&src, &p[..cut]).is_err(), "cut {cut}");
        }
    }
}
