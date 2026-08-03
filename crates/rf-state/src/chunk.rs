//! The TLV chunk list that makes up the (zstd-compressed) `.rfstate` body.
//!
//! # Byte layout (little-endian), repeated back-to-back until input ends
//!
//! ```text
//! offset  size  field
//! 0       4     tag       [u8; 4], e.g. b"CPU_"
//! 4       2     version   u16
//! 6       4     len       u32, payload length in bytes
//! 10      len   payload   opaque bytes — owned and interpreted only by
//!                             the chunk's owning crate (SAVE_STATES §2's
//!                             ownership table); this crate never looks
//!                             inside a payload it does not itself own
//!                             (`RPLY`, see `payload.rs`)
//! ```
//!
//! `payload` bytes are conventionally a bincode 2 `Encode`/`Decode`
//! struct belonging to the owning crate — this module does not assume or
//! require that; it treats every payload as an opaque `Vec<u8>`.

use crate::cursor::Cursor;
use crate::error::ContainerError;

/// One TLV entry: a tagged, versioned, opaque payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// The 4-byte chunk tag (SAVE_STATES.md §2 registry).
    pub tag: [u8; 4],
    /// The payload's format version, as assigned by its owning crate.
    pub version: u16,
    /// Opaque payload bytes.
    pub payload: Vec<u8>,
}

pub(crate) fn encode_chunks(chunks: &[Chunk], out: &mut Vec<u8>) -> Result<(), ContainerError> {
    for c in chunks {
        out.extend_from_slice(&c.tag);
        out.extend_from_slice(&c.version.to_le_bytes());
        let len = u32::try_from(c.payload.len()).map_err(|_| ContainerError::PayloadTooLarge {
            tag: c.tag,
            len: c.payload.len(),
        })?;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&c.payload);
    }
    Ok(())
}

/// Parse the decompressed chunk body into a list of chunks.
///
/// Each chunk's declared `len` is checked against the bytes actually
/// remaining *before* it is used to slice/allocate the payload — this is
/// the guard against a corrupt or hostile `len` field driving an
/// out-of-bounds read or an oversized allocation. Since the body itself
/// is already capped (see `container.rs`'s decompression guard), the
/// worst case here is bounded by that cap, not by an attacker-chosen
/// `u32`.
pub(crate) fn decode_chunks(body: &[u8]) -> Result<Vec<Chunk>, ContainerError> {
    let mut cur = Cursor::new(body);
    let mut chunks = Vec::new();
    while cur.remaining() > 0 {
        let tag = cur.take_array4("chunk tag")?;
        let version = cur.take_u16_le("chunk version")?;
        let len = cur.take_u32_le("chunk len")? as usize;
        if len > cur.remaining() {
            return Err(ContainerError::ChunkTruncated {
                tag,
                needed: len,
                got: cur.remaining(),
            });
        }
        let payload = cur.take(len, "chunk payload")?.to_vec();
        chunks.push(Chunk {
            tag,
            version,
            payload,
        });
    }
    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_chunks() -> Vec<Chunk> {
        vec![
            Chunk {
                tag: *b"CPU_",
                version: 1,
                payload: vec![1, 2, 3],
            },
            Chunk {
                tag: *b"WRAM",
                version: 1,
                payload: vec![0; 2048],
            },
            Chunk {
                tag: *b"INPT",
                version: 1,
                payload: vec![],
            },
        ]
    }

    #[test]
    fn round_trips() {
        let chunks = sample_chunks();
        let mut bytes = Vec::new();
        encode_chunks(&chunks, &mut bytes).unwrap();
        let back = decode_chunks(&bytes).unwrap();
        assert_eq!(chunks, back);
    }

    #[test]
    fn empty_body_decodes_to_no_chunks() {
        assert_eq!(decode_chunks(&[]).unwrap(), Vec::<Chunk>::new());
    }

    #[test]
    fn oversized_declared_len_errors_without_huge_allocation() {
        // Hand-built: a single chunk header claiming a payload of
        // u32::MAX bytes, with only 3 bytes actually following it. If this
        // allocated `len` bytes before checking, it would try to grab ~4
        // GiB; instead it must error immediately.
        let mut body = Vec::new();
        body.extend_from_slice(b"CPU_");
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&u32::MAX.to_le_bytes());
        body.extend_from_slice(&[9, 9, 9]);
        let err = decode_chunks(&body).unwrap_err();
        assert_eq!(
            err,
            ContainerError::ChunkTruncated {
                tag: *b"CPU_",
                needed: u32::MAX as usize,
                got: 3,
            }
        );
    }

    #[test]
    fn duplicate_tags_do_not_panic_and_both_are_retained() {
        let mut body = Vec::new();
        encode_chunks(
            &[
                Chunk {
                    tag: *b"INPT",
                    version: 1,
                    payload: vec![1],
                },
                Chunk {
                    tag: *b"INPT",
                    version: 1,
                    payload: vec![2],
                },
            ],
            &mut body,
        )
        .unwrap();
        let chunks = decode_chunks(&body).unwrap();
        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|c| c.tag == *b"INPT"));
    }

    #[test]
    fn truncating_anywhere_inside_the_last_chunk_errors_without_panicking() {
        // A prefix that lands exactly on a chunk boundary is a valid
        // (shorter) chunk list, not an error — so this sweep only covers
        // byte offsets strictly inside the final chunk's own span, where
        // truncation must always be rejected.
        let mut leading = Vec::new();
        encode_chunks(&sample_chunks()[..2], &mut leading).unwrap();
        let last = Chunk {
            tag: *b"INPT",
            version: 1,
            payload: vec![7, 7, 7, 7, 7],
        };
        let mut full = leading.clone();
        encode_chunks(std::slice::from_ref(&last), &mut full).unwrap();

        for len in (leading.len() + 1)..full.len() {
            assert!(
                decode_chunks(&full[..len]).is_err(),
                "prefix len {len} (inside the last chunk) should error"
            );
        }
        // The full, untruncated stream must still parse.
        assert!(decode_chunks(&full).is_ok());
    }
}
