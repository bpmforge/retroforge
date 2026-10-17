//! UPS: `"UPS1"`, varint source size, varint target size, then blocks of
//! (varint relative skip, XOR bytes up to and including a 0 terminator),
//! then three little-endian CRC32s (source, target, patch). The XOR makes
//! a UPS patch symmetric: it also turns the target back into the source,
//! so either direction is accepted when its CRC matches.
use super::{capped_size, crc32, read_le32, PatchError, Reader};

/// Apply a UPS patch.
///
/// # Errors
/// [`PatchError`] for any malformed, truncated or mismatched input.
pub fn apply_ups(source: &[u8], patch: &[u8]) -> Result<Vec<u8>, PatchError> {
    if !patch.starts_with(b"UPS1") {
        return Err(PatchError::BadMagic);
    }
    if patch.len() < 4 + 2 + 12 {
        return Err(PatchError::Truncated);
    }
    let body_end = patch.len() - 12;
    let footer = &patch[body_end..];
    if crc32(&patch[..patch.len() - 4]) != read_le32(&footer[8..]) {
        return Err(PatchError::CorruptPatch);
    }
    let (crc_a, crc_b) = (read_le32(footer), read_le32(&footer[4..]));
    let mut r = Reader::new(&patch[..body_end], 4);
    let (size_a, size_b) = (r.varint()?, r.varint()?);
    let src_crc = crc32(source);
    // Forward: source is `a`. Reverse: source is `b`.
    let (target_size, want_target) = if src_crc == crc_a && source.len() as u64 == size_a {
        (size_b, crc_b)
    } else if src_crc == crc_b && source.len() as u64 == size_b {
        (size_a, crc_a)
    } else {
        return Err(PatchError::WrongSource(format!(
            "expected {size_a} bytes with CRC32 {crc_a:08x}, got {} bytes with CRC32 {src_crc:08x}",
            source.len()
        )));
    };
    let target_size = capped_size(target_size)?;
    // Work on max(source, target) bytes: XOR runs may reach past the
    // shorter image (they cancel to zero there), and the result is cut to
    // the target size afterwards.
    let mut out = source.to_vec();
    out.resize(target_size.max(source.len()), 0);
    let mut ptr: usize = 0;
    // Each iteration reads a varint (>= 1 byte) so input is consumed.
    while r.remaining() > 0 {
        let skip = usize::try_from(r.varint()?).map_err(|_| PatchError::OutOfRange("skip"))?;
        ptr = ptr
            .checked_add(skip)
            .ok_or(PatchError::OutOfRange("skip"))?;
        loop {
            let x = r.byte()?;
            // The terminating 0 occupies an output position too.
            if let Some(b) = out.get_mut(ptr) {
                *b ^= x;
            } else if x != 0 {
                return Err(PatchError::OutOfRange("xor past target"));
            }
            ptr += 1;
            if x == 0 {
                break;
            }
        }
    }
    out.truncate(target_size);
    if crc32(&out) != want_target {
        return Err(PatchError::BadTarget);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::testutil::{enc, noise};

    pub fn make_ups(src: &[u8], tgt: &[u8]) -> Vec<u8> {
        let mut p = b"UPS1".to_vec();
        p.extend(enc(src.len() as u64));
        p.extend(enc(tgt.len() as u64));
        let n = src.len().max(tgt.len());
        let mut last = 0usize;
        let mut i = 0;
        while i < n {
            let a = src.get(i).copied().unwrap_or(0);
            let b = tgt.get(i).copied().unwrap_or(0);
            if a == b {
                i += 1;
                continue;
            }
            p.extend(enc((i - last) as u64));
            while i < n {
                let a = src.get(i).copied().unwrap_or(0);
                let b = tgt.get(i).copied().unwrap_or(0);
                if a == b {
                    break;
                }
                p.push(a ^ b);
                i += 1;
            }
            p.push(0);
            i += 1; // the terminator's position
            last = i;
        }
        p.extend_from_slice(&crc32(src).to_le_bytes());
        p.extend_from_slice(&crc32(tgt).to_le_bytes());
        let c = crc32(&p);
        p.extend_from_slice(&c.to_le_bytes());
        p
    }

    #[test]
    fn round_trip_grow_shrink_and_reverse() {
        let src = noise(5, 500);
        let mut tgt = src.clone();
        tgt[3] ^= 0xFF;
        tgt[4] ^= 0x01;
        tgt[300] = tgt[300].wrapping_add(1);
        tgt.extend_from_slice(b"extra");
        let p = make_ups(&src, &tgt);
        assert_eq!(apply_ups(&src, &p).unwrap(), tgt);
        assert_eq!(apply_ups(&tgt, &p).unwrap(), src); // reverse
        let short: Vec<u8> = src[..100].to_vec();
        let p = make_ups(&src, &short);
        assert_eq!(apply_ups(&src, &p).unwrap(), short);
    }

    #[test]
    fn wrong_source_and_corruption_are_clear_errors() {
        let src = noise(6, 200);
        let mut tgt = src.clone();
        tgt[9] ^= 3;
        let p = make_ups(&src, &tgt);
        let other = noise(9, 200);
        let e = apply_ups(&other, &p);
        assert!(matches!(e, Err(PatchError::WrongSource(_))), "{e:?}");
        let mut bad = p.clone();
        bad[7] ^= 0x40;
        assert_eq!(apply_ups(&src, &bad), Err(PatchError::CorruptPatch));
    }

    #[test]
    fn every_truncation_point_is_handled() {
        let src = noise(8, 400);
        let mut tgt = src.clone();
        for i in (0..400).step_by(37) {
            tgt[i] ^= 0x5A;
        }
        let p = make_ups(&src, &tgt);
        for cut in 0..p.len() {
            assert!(apply_ups(&src, &p[..cut]).is_err());
        }
    }
}
