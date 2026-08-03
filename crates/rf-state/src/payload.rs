//! bincode 2 helpers for chunk *payload* contents (SAVE_STATES.md §2:
//! "Payload encoding: bincode 2 (`Encode`/`Decode` derives — NOT the 1.x
//! serde API)").
//!
//! These helpers operate on the bytes *inside* a [`crate::Chunk`]; the
//! envelope around them (tag/version/len, header) is hand-rolled and
//! never touches bincode — see the module doc comments on `header.rs` and
//! `chunk.rs`.
//!
//! `bincode = "2"` (resolves 2.0.1). `bincode 3.0.0` on crates.io is a
//! placeholder whose entire `lib.rs` is `compile_error!(...)` — never pin
//! `"3"` (TECH_STACK.md §2).

use bincode::{Decode, Encode};

use crate::error::ContainerError;

/// Safety cap on a single decoded payload, independent of and in addition
/// to the container-level decompressed-body cap in `container.rs` — this
/// bounds `decode_payload` even if called directly on caller-supplied
/// bytes that did not come through [`crate::Container::decode`].
const MAX_PAYLOAD_DECODE_BYTES: usize = 64 * 1024 * 1024;

/// Encode `value` as a chunk payload.
pub fn encode_payload<T: Encode>(value: &T) -> Result<Vec<u8>, ContainerError> {
    bincode::encode_to_vec(value, bincode::config::standard())
        .map_err(|e| ContainerError::Encode(e.to_string()))
}

/// Decode a chunk payload as `T`, bounded by [`MAX_PAYLOAD_DECODE_BYTES`]
/// so a corrupt/hostile payload cannot drive an unbounded allocation.
pub fn decode_payload<T: Decode<()>>(bytes: &[u8]) -> Result<T, ContainerError> {
    let cfg = bincode::config::standard().with_limit::<MAX_PAYLOAD_DECODE_BYTES>();
    let (value, _len) = bincode::decode_from_slice(bytes, cfg)
        .map_err(|e| ContainerError::Decode(e.to_string()))?;
    Ok(value)
}

/// The `RPLY` chunk payload rf-state itself owns (SAVE_STATES.md §2:
/// "input-log cursor for replay-attached states"). Minimal placeholder
/// carrying just the replay frame cursor; the `.rfreplay` format itself
/// (SAVE_STATES §3) is out of scope for this ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct ReplayCursor {
    /// Frame index into the attached `.rfreplay` input log.
    pub frame: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_cursor_round_trips_through_bincode() {
        let cursor = ReplayCursor { frame: 12345 };
        let bytes = encode_payload(&cursor).unwrap();
        let back: ReplayCursor = decode_payload(&bytes).unwrap();
        assert_eq!(cursor, back);
    }

    #[test]
    fn decode_payload_rejects_garbage_without_panicking() {
        let err = decode_payload::<ReplayCursor>(&[0xFF; 3]).unwrap_err();
        assert!(matches!(err, ContainerError::Decode(_)));
    }
}
