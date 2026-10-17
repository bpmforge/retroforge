//! IPS: `"PATCH"`, then records of (3-byte big-endian offset, 2-byte size,
//! size bytes), where size 0 means RLE (2-byte count, 1 fill byte); ends
//! with `"EOF"` and optionally a 3-byte truncate length. No checksums, so
//! a wrong ROM cannot be detected here.
use super::{PatchError, Reader, MAX_OUTPUT};

const EOF_MARK: [u8; 3] = *b"EOF";

fn be(b: &[u8]) -> usize {
    b.iter().fold(0usize, |a, &x| (a << 8) | usize::from(x))
}

fn grow(out: &mut Vec<u8>, end: usize) -> Result<(), PatchError> {
    if end > MAX_OUTPUT {
        return Err(PatchError::OutOfRange("record past 64 MiB"));
    }
    if out.len() < end {
        out.resize(end, 0);
    }
    Ok(())
}

/// Apply an IPS patch.
///
/// # Errors
/// [`PatchError`] for any malformed or truncated input.
pub fn apply_ips(source: &[u8], patch: &[u8]) -> Result<Vec<u8>, PatchError> {
    if !patch.starts_with(b"PATCH") {
        return Err(PatchError::BadMagic);
    }
    let mut r = Reader::new(patch, 5);
    let mut out = source.to_vec();
    // Each iteration reads at least the 3 offset bytes, so it consumes
    // input on every path and the first read past the end fails.
    loop {
        let off = r.take(3)?;
        if off == EOF_MARK {
            break;
        }
        let offset = be(off);
        let size = be(r.take(2)?);
        if size == 0 {
            let count = be(r.take(2)?);
            let fill = r.byte()?;
            grow(&mut out, offset + count)?;
            out[offset..offset + count].fill(fill);
        } else {
            let data = r.take(size)?;
            grow(&mut out, offset + size)?;
            out[offset..offset + size].copy_from_slice(data);
        }
    }
    match r.remaining() {
        0 => {}
        3 => {
            let len = be(r.take(3)?);
            if len > MAX_OUTPUT {
                return Err(PatchError::OutOfRange("truncate length"));
            }
            out.resize(len, 0);
        }
        _ => return Err(PatchError::TrailingData),
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::testutil::noise;

    fn rec(off: usize, data: &[u8]) -> Vec<u8> {
        let mut v = vec![(off >> 16) as u8, (off >> 8) as u8, off as u8];
        v.extend_from_slice(&(data.len() as u16).to_be_bytes());
        v.extend_from_slice(data);
        v
    }

    fn rle(off: usize, count: u16, fill: u8) -> Vec<u8> {
        let mut v = vec![(off >> 16) as u8, (off >> 8) as u8, off as u8, 0, 0];
        v.extend_from_slice(&count.to_be_bytes());
        v.push(fill);
        v
    }

    #[test]
    fn records_rle_growth_and_truncate() {
        let src = noise(1, 1000);
        let mut p = b"PATCH".to_vec();
        p.extend(rec(10, b"hello"));
        p.extend(rle(100, 50, 0xAA));
        p.extend(rec(1100, b"tail")); // grows past the source
        p.extend_from_slice(b"EOF");
        let out = apply_ips(&src, &p).unwrap();
        assert_eq!(out.len(), 1104);
        assert_eq!(&out[10..15], b"hello");
        assert!(out[100..150].iter().all(|&b| b == 0xAA));
        assert_eq!(&out[1000..1100], &[0u8; 100][..]);
        assert_eq!(&out[1100..], b"tail");
        assert_eq!(&out[..10], &src[..10]);

        p.extend_from_slice(&[0, 1, 0]); // truncate to 256
        assert_eq!(apply_ips(&src, &p).unwrap().len(), 256);
    }

    #[test]
    fn malformed_inputs_error() {
        let src = noise(2, 64);
        assert_eq!(apply_ips(&src, b"PATC"), Err(PatchError::BadMagic));
        assert_eq!(apply_ips(&src, b"PATCH"), Err(PatchError::Truncated));
        let mut p = b"PATCH".to_vec();
        p.extend_from_slice(b"EOF\x01");
        assert_eq!(apply_ips(&src, &p), Err(PatchError::TrailingData));
        // Record whose data runs off the end of the patch.
        let mut p = b"PATCH".to_vec();
        p.extend_from_slice(&[0, 0, 5, 0, 200, 1, 2]);
        assert_eq!(apply_ips(&src, &p), Err(PatchError::Truncated));
    }

    #[test]
    fn every_truncation_point_is_handled() {
        let src = noise(3, 300);
        let mut p = b"PATCH".to_vec();
        p.extend(rec(5, b"abcdef"));
        p.extend(rle(50, 20, 7));
        p.extend_from_slice(b"EOF");
        for cut in 0..p.len() {
            assert!(apply_ips(&src, &p[..cut]).is_err(), "cut {cut}");
        }
        assert!(apply_ips(&src, &p).is_ok());
    }
}
